# Architecture

## One core, two shells

```
┌────────────────────────────────────────────────────────────────┐
│                        Folio App (Tauri 2)                     │
│                                                                │
│  ┌──────────────────┐        ┌──────────────────────────────┐  │
│  │   Web frontend   │        │          folio-core          │  │
│  │  (WebView2, no   │  Tauri │   (Rust library crate)       │  │
│  │   framework)     │ events │                              │  │
│  │                  │ + cmds │   roots · watcher · store    │  │
│  │                  │───────▶│   snapshots · proposals      │  │
│  └──────────────────┘        │   diff engine · linters      │  │
│                              └──────────┬───────────────────┘  │
│                              ┌──────────▼───────────────────┐  │
│                              │   Local IPC (127.0.0.1 TCP   │  │
│                              │   + token)                   │  │
│                              └──────────▲───────────────────┘  │
└─────────────────────────────────────────┼──────────────────────┘
                    ┌─────────────────────┴────────────────────┐
                    │  `folio mcp` (stdio MCP bridge, same     │
                    │  binary, second personality)             │
                    └─────────────────────┬────────────────────┘
                                          │ stdio (MCP)
                    ┌─────────────────────▼────────────────────┐
                    │  Any MCP client: Claude Code, Codex, …   │
                    └──────────────────────────────────────────┘
```

**`folio-core` is a plain Rust library.** All logic lives there, frontend-
agnostic and unit-testable. The Tauri app and the MCP bridge are thin shells.
This is the single most important structural decision in the product: it keeps
the MCP surface honest, because it cannot drift from the UI when both call the
same core.

**One binary, two personalities.** `folio` launches the GUI. `folio mcp` runs
the stdio MCP bridge that MCP clients spawn per session.

**The bridge prefers a running app.** If the GUI is up, tool calls forward over
local IPC; the app stays the single watcher and the live review surface. If the
GUI is down, the bridge embeds `folio-core` headless: reads work, direct writes
work, and proposals queue in the store for review the next time the app opens.
Proposals are durable; the review surface does not need to be online when they
are created. If the app goes away mid-session the bridge switches to an embedded
core against the same store, so an agent halfway through a task does not fail
because a window was closed.

**The rendezvous is at a fixed path, not inside the store.** A client
configured with a bare `folio mcp` has no idea which store the app was launched
with. If the app announced itself inside its own store directory, a bridge
could never find an app started with `--store`, and the two would work on
different stores — which presents as "my tool calls do nothing". So the app
always writes `ipc.json` to the standard store location and records which store
it actually has open. A bridge told nothing follows that; a bridge told a store
explicitly refuses to bridge across a mismatch and says why.

**Snapshot dedup makes multi-watcher harmless.** Snapshots are keyed by
`(path, content hash)` with a coalescing window, so a headless bridge and the
GUI both watching the same root cannot produce duplicate history entries.

## Crate map

```
crates/folio-core/src/
├── api.rs          the dispatch table — the API, for every shell
├── store/          SQLite index + content-addressed blob store
│   ├── blobs.rs    blobs/ab/cd/<sha256>
│   └── schema.rs   the whole schema, applied idempotently
├── corpus/         roots, artifact typing, path sandboxing
├── watch.rs        notify + debouncer, coalescing
├── version/        snapshots, timeline, restore, patch-series export
├── proposal/       changesets, policies, accept/reject, conflicts, rebase
├── comment/        anchored threads, re-anchoring, replies, resolution
├── diff/           three-level prose diff; unified patch parse and apply
├── artifact/       skills, prompts, tasks, docs (parse + lint)
├── search.rs       ripgrep-semantics corpus search
├── event.rs        the bus the shells subscribe to
└── mcp/            the rmcp server: tool schemas over dispatch

crates/folio-app/src/
├── main.rs         argv routing: GUI, or `mcp`
├── gui.rs          Tauri commands, event forwarding, window
├── ipc.rs          the loopback socket, both ends
└── bridge.rs       `folio mcp`: bridged, or headless

ui/                 vanilla JS frontend, embedded at compile time
```

## The dispatch table

Every operation the product has is a name in one `match`:

```rust
folio.dispatch(&caller, "propose_edit", &json!({
    "path": …,
    "base_version": …,
    "patch": …,
    "intent": …,
}))
```

Three callers reach it:

| Caller | Path | Identity |
|---|---|---|
| The UI | Tauri command `folio_call` | `Caller::human()` |
| An agent, app running | MCP tool → IPC socket → dispatch | `Caller::agent(model, client)` |
| An agent, app closed | MCP tool → embedded core → dispatch | `Caller::agent(model, client)` |

`Caller::is_human` gates the operations that constitute judgement — accepting,
rejecting, resolving, restoring, saving, configuring. Those are also absent
from the MCP tool list, so the check is defence in depth rather than the only
line.

## Data model

```sql
roots         (id, path, kind, policy, label, added_at)
path_policies (id, root_id, pattern, policy)          -- per-glob overrides
snapshots     (id, root_id, path, blob_hash, size, mtime,
               source, author, client, message, artifact_type,
               created_at, coalesce_window)
proposals     (id, root_id, path, base_snapshot_id, base_blob_hash,
               proposed_blob_hash, author, client, message, addressing,
               status, created_at, decided_at, decision_note,
               result_snapshot_id)
comments      (id, path, anchor_hash, anchor_text, context_before,
               context_after, author, body, created_at, resolved_at,
               resolved_by, addressed_by_proposal)
replies       (id, comment_id, author, body, created_at)
clients       (name, mode, first_seen, last_seen)
rate_events   (client, kind, at)                       -- reply rate limiting
```

Blobs are content-addressed by SHA-256 and deduplicated: saving a file twice
with no change creates no new version, and identical content across files
shares storage.

### Deduplication, precisely

Two mechanisms, deliberately different:

* **In code.** If the newest snapshot for a path already has this content hash,
  nothing is written. That makes re-saving free and makes the watcher's echo of
  Folio's own write a no-op.
* **In the schema.** A partial unique index on
  `(path_key, blob_hash, coalesce_window) WHERE source = 'external'` stops two
  *watchers* recording the same external write twice.

The index is scoped to `external` on purpose. Passive observation is the only
source that can fire twice for one write; every other source is somebody
deciding something, and a decision that happens to restore two-second-old
content is still a version.

## Write policy

| Policy | Meaning |
|---|---|
| `auto` | Decide by artifact type: task lists direct, everything else proposed |
| `propose` | Every MCP write becomes a proposal |
| `direct` | MCP writes apply immediately, always snapshotted |

Resolution order: an explicit per-path glob wins, then the root's setting, and
`auto` finally resolves by type. The asymmetry is deliberate: an agent
maintaining `TODO.md` overnight must not block on review, and task lists are
append-mostly, low-risk and self-evident in a diff. Skills and docs are the
opposite — high blast radius, and exactly where you want a gate.

Human edits in the Folio editor never become proposals. The gate is for agents.

## The diff engine

Three levels, all in `folio-core`:

1. **Line diff** — `similar` over the two blobs.
2. **Paragraph grouping** — raw change ranges are expanded to enclosing block
   boundaries (headings, list items, fences and paragraphs are units), then
   merged, so a rewritten paragraph reads as one hunk instead of interleaved
   line noise.
3. **Word-level highlights** — runs of deletes are paired with the runs of
   inserts that follow them, and changed word ranges are emitted as token
   offsets the frontend renders as spans.

Expansion is *anchored*: the same number of unchanged lines is pulled in on
both sides. That invariant is what makes hunk-level accept exact — the regions
between hunks are identical in both texts by construction, so applying a subset
is a splice, not a re-merge.

Rebasing a conflicting proposal uses a real three-way merge over *raw* change
ranges, not display hunks: paragraph expansion exists to make a diff readable,
and using it there would invent conflicts between edits that never overlapped.

Move detection is deferred: v1 reports a moved paragraph as delete plus add.

## Anchored comments

A comment is anchored by the selected text plus two lines of context on each
side, hashed. The anchor is re-resolved against current content every time it
is read:

* the text still exists verbatim → the comment pins to it wherever it moved, so
  a rewritten paragraph above it does not orphan it;
* the text changed → **outdated**, shown in the drawer, never silently dropped;
* the file is gone → **orphaned**, retained.

When the anchored text appears more than once, the stored context breaks the
tie. Matching is verbatim on purpose: a fuzzy matcher would keep more threads
alive across agent rewrites at the cost of occasionally pinning to the wrong
spot, which is a worse failure than an honest "outdated".

## Events

The core emits; the shells forward. No polling.

```
snapshot-created · proposal-arrived · proposal-decided
comment-activity · watcher-status · clients-changed
doc-removed · corpus-changed
```

The Tauri shell subscribes once and re-emits everything on a single channel;
the frontend switches on the event's own `type` field.

## Security

* **Path sandboxing.** Every path resolves against the registered roots.
  `canonicalize` runs *before* the containment test, so a symlink that escapes a
  root fails it, as do `..` traversal and absolute paths elsewhere.
* **The IPC socket** binds 127.0.0.1 only and is authenticated with a token
  file in the store directory. `ipc.json` is removed when the app exits, the
  bridge bounds its handshake read, and it pings before trusting a connection,
  so a stale file pointing at a port something else now owns cannot redirect or
  hang it.
* **Write policy is enforced in `folio-core`**, not in the bridge, so no client
  can bypass it.
* **Runaway-agent guards.** Pending-proposal cap (500), blob size limit (10 MB),
  and a per-client comment-reply rate limit (60/hour).
