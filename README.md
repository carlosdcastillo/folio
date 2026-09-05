# Folio

**Your agents' markdown, under your control.**

Folio is a local, single-user markdown workshop for the files your agents read
and write: documentation, prompts, task lists, and skills. It is not a
note-taking app, not a generic markdown editor, and not a chat client. It is the
system of record for agent-adjacent markdown.

Four capabilities define it:

1. **Versioning without git.** Every change to every registered file is
   snapshotted automatically — whether you made it in the editor, an agent made
   it through MCP, or any external tool made it on disk. Files stay plain,
   portable markdown; the history lives in Folio's private store.
2. **Proposals.** Agents that edit through MCP land their changes as pending
   changesets by default. You review a prose-aware diff and accept or reject
   hunk by hunk. Direct writes are an opt-in policy, not the default.
3. **Artifact intelligence.** Folio knows what a skill, a prompt, and a task
   list *are*. It validates skill structure and reference integrity, renders
   prompt templates with variable slots, and aggregates task lists into a
   morning review view.
4. **Anchored comments.** Highlight any region and leave a note; connected
   agents see open comments as first-class work items, reply to them, and
   address them with proposals. Accepting an addressing proposal closes the
   loop. Comments live in Folio's store, never inside the markdown files.

---

## Building and running

Requires a Rust toolchain (1.85+) and, for the editor bundle, Node 18+.

```bash
# The CodeMirror bundle. Only needed once, or after editing ui/cm-entry.js.
cd ui && npm install && npm run build && cd ..

# The app, and the MCP bridge — the same binary.
cargo build --release -p folio-app

./target/release/folio            # the desktop app
./target/release/folio mcp        # the stdio MCP server
./target/release/folio --help
```

> The frontend is embedded into the binary at compile time. After editing
> anything under `ui/`, rebuild `folio-app` before you will see the change.

### Options

| Flag | Meaning |
|---|---|
| `--store <path>` | Use a different store directory (default `%LOCALAPPDATA%\Folio`, or `$FOLIO_STORE`) |
| `--author <name>` | Default author recorded for MCP writes, e.g. the model's name |

---

## Connecting an agent

Point any MCP client at the same binary:

```json
{
  "mcpServers": {
    "folio": { "command": "folio", "args": ["mcp"] }
  }
}
```

The bridge prefers a running app: tool calls travel over an authenticated
loopback socket so the GUI stays the single watcher and the live review
surface, and a proposal appears on screen the moment the tool call returns.
With no app running it embeds the core headless — reads work, direct writes
work, and proposals queue durably in the store for review the next time you
open it.

### The tool surface

Eighteen tools. Everything the UI does, an agent can do — except *decide*.

| Group | Tools |
|---|---|
| Corpus | `list_roots`, `add_root`, `list_docs`, `read_doc`, `search_docs` |
| Versioning | `list_versions`, `read_version`, `diff_versions`, `checkpoint` |
| Writing | `propose_edit`, `task_list`, `task_add`, `task_set_status` |
| Intelligence and feedback | `validate_doc`, `render_prompt`, `list_proposals`, `list_comments`, `reply_comment` |

Accepting, rejecting, resolving, restoring and saving are deliberately *not*
tools. Judgement stays with the human, and that boundary is enforced in the
core, not in the bridge — see [docs/mcp.md](docs/mcp.md).

---

## How it fits together

```
folio-core  ── the whole product: store, watcher, corpus, versions,
                proposals, comments, diff, artifact intelligence, MCP
     ▲
     ├── folio-app (GUI)     Tauri 2 shell + local IPC socket
     └── folio mcp (bridge)  rmcp stdio server → IPC, or embedded core
```

`folio-core` is a plain Rust library and both shells reach it through one
function, `Folio::dispatch(caller, op, params)`. That is the single most
important structural decision in the codebase: the MCP surface cannot drift
from the UI, because there is only one implementation underneath both.

See [docs/architecture.md](docs/architecture.md).

---

## The store

Windows default `%LOCALAPPDATA%\Folio`:

```
store.db     SQLite (WAL): roots, snapshots, proposals, comments
blobs/       content-addressed: blobs/ab/cd/<sha256>
token        secret for the local IPC socket
```

A running app also writes `ipc.json` — `{port, token, pid, store}` — into the
*standard* location above, even when `--store` points somewhere else. That is
how a client configured with a bare `folio mcp` finds the app whatever store it
was launched with. If you give the bridge its own `FOLIO_STORE` and it does not
match the running app's, it says so and works headless on the store you asked
for rather than quietly using a different one.

Losing the store costs history, never documents. Your markdown is on disk,
unchanged, and Folio never writes a `.git` directory into your folders.

**Do not put the store in a cloud-synced folder.** SQLite and file sync corrupt
each other. Folio detects the well-known sync roots and warns loudly; move it
with `--store` and back up by exporting instead.

---

## Development

```bash
cargo test -p folio-core                     # 96 unit + 20 end-to-end tests
python tools/mcp_smoke.py                    # the real stdio MCP protocol
python tools/seed_demo.py                    # a demo corpus with history
python tools/watcher_check.py                # watcher latency, against a running app
python tools/bridge_failover.py              # the bridge survives the app closing mid-session
```

The end-to-end tests are written directly against the specification's success
criteria: an agent's edit arriving as a reviewable proposal, hunk-level accept
applying exactly the accepted hunks, a comment surviving its paragraph moving,
a stale task write failing with the current list attached.

More in [docs/development.md](docs/development.md).

---

## License

MIT — see [LICENSE](LICENSE).

Vendored browser libraries and Rust dependencies are listed in
[THIRD-PARTY.md](THIRD-PARTY.md); all are permissively licensed.
