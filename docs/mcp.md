# The MCP surface

The MCP surface is the product's API; the UI is one client among two. Every
tool is a thin name-and-schema over an operation in `folio-core`'s dispatch
table, which is what stops the agent-facing surface from ever drifting away
from what the app itself does.

## Connecting

```json
{
  "mcpServers": {
    "folio": { "command": "folio", "args": ["mcp"] }
  }
}
```

Set `FOLIO_AUTHOR` (or pass `--author`) to the model's name so versions are
attributed to the model rather than to the client. Individual tool calls can
also carry an `author` argument, which wins.

Multiple clients can be connected at once; the app's status bar lists them.

## What an agent should know

Three things, which the server also states in its `instructions`:

1. **Your edits are proposals, not writes.** `propose_edit` queues a changeset
   for human review by default. It has not landed until the human accepts it.
   Task lists are the exception and apply immediately, because a list
   maintained overnight must not block on review.
2. **Rejections carry reasons.** Call `list_proposals` with `status: rejected`
   and `mine_only: true` before re-proposing; the reviewer's note says what to
   change.
3. **Open comments are your to-do list.** `list_comments` with `status: open`
   is an inbox of passages the human has flagged. Reply in the thread, then
   send a `propose_edit` with `addressing` set to the comment id. Accepting it
   resolves the thread.

## Tools

### Corpus

| Tool | Params | Returns |
|---|---|---|
| `list_roots` | — | Roots with kind, policy, doc counts |
| `add_root` | `path`, `policy?`, `label?` | The root, and how many files were indexed |
| `list_docs` | `root?`, `type?` | Docs with type, version count, pending proposals, open comments |
| `read_doc` | `path` | Content, parsed frontmatter, current version id, type, effective policy |
| `search_docs` | `query`, `regex?`, `glob?`, `case_sensitive?`, `max_results?` | Matches with line numbers and snippets |

### Versioning

| Tool | Params | Returns |
|---|---|---|
| `list_versions` | `path`, `limit?` | Timeline: id, source, author, message, timestamp, size delta |
| `read_version` | `path`, `version_id` | Historical content |
| `diff_versions` | `path`, `from?`, `to?` | Prose-aware diff (the same engine the UI uses) plus a unified rendering |
| `checkpoint` | `path`, `message`, `author?` | New snapshot id |

`version_id` accepts a snapshot id, `latest`, or `~N` for N versions back.

### Writing

| Tool | Params | Returns |
|---|---|---|
| `propose_edit` | `path`, `content` \| `patch`, `message?`, `addressing?`, `author?` | `{outcome: "proposed", proposal}` or `{outcome: "applied", snapshot}` |
| `task_list` | `doc?`, `owner?`, `tag?`, `open_only?`, `query?` | Tasks with line-anchored ids and the version they came from |
| `task_add` | `doc`, `text`, `owner?`, `tag?`, `author?` | Applied or proposed, plus the new task id |
| `task_set_status` | `doc`, `task_id`, `status`, `version?`, `text?`, `author?` | Applied or proposed |

`propose_edit` with `content` replaces the whole file; with `patch` it applies a
unified diff to the base you read. Both record the base snapshot id, so
conflicts are detectable at review time.

### Intelligence and feedback

| Tool | Params | Returns |
|---|---|---|
| `validate_doc` | `path` | Type-aware findings |
| `render_prompt` | `path`, `variables` | Rendered text, declared variables, used slots |
| `list_proposals` | `status?`, `path?`, `mine_only?`, `limit?` | Proposals with status and rejection notes |
| `list_comments` | `path?`, `status?`, `mine_only?`, `limit?` | Threads with anchor excerpt, replies, addressing proposal |
| `reply_comment` | `comment_id`, `body`, `author?` | The reply |

## What is deliberately not a tool

`accept_proposal`, `reject_proposal`, `rebase_proposal`, `create_comment`,
`resolve_comment`, `delete_comment`, `save_doc`, `restore_version`, and every
configuration operation.

Those exist in the dispatch table and the UI calls them, but they are judgement,
and judgement stays with the human. The core enforces this independently of the
tool list: a call arriving with an agent identity is refused with
`policy_denied`, so the boundary does not depend on the bridge being the only
door.

## Errors an agent should handle

Tool failures come back as tool-level errors — `isError: true` with a
structured body — so the message reaches the model rather than being rendered
opaquely as a protocol fault.

| `code` | Meaning | What to do |
|---|---|---|
| `stale` | The document moved on since the version you read | The current list is in `data`; retry against it without a second read |
| `conflict` | A patch will not apply, or a rebase cannot replay | Re-read and propose against current content |
| `outside_root` | The path is not in the corpus | Use `list_roots` / `list_docs` |
| `policy_denied` | Human-only operation | Do not retry |
| `queue_full` | 500 proposals already pending | Stop proposing; ask for a review |
| `rate_limited` | Too many comment replies from this client | Back off |
| `invalid` | Bad arguments, or a no-op edit | Fix the call |

### The stale protocol, concretely

```jsonc
// 1. read
task_list { "doc": "~/TODO.md", "open_only": true }
// → { "tasks": [ { "id": "L42", "text": "Ship 1.0", "version": "snap_c117", … } ] }

// 2. write, carrying the version you read
task_set_status { "doc": "~/TODO.md", "task_id": "L42",
                  "status": "done", "version": "snap_c117", "text": "Ship 1.0" }

// → if the file moved on:
// { "code": "stale", "message": "…re-read the list before writing",
//   "data": { "version": "snap_c221", "tasks": [ … the current list … ] } }
```

Task ids are line-anchored and valid only for the snapshot they were read from.
Passing `version` back is what turns a silent mis-tick into a loud, recoverable
failure. Passing `text` as well checks the same thing a second way.

## Comment addressing

```jsonc
list_comments { "status": "open" }
// → { "comments": [ { "id": "cm_3f21", "excerpt": "The threshold: if the table…",
//                     "body": "4+ is too aggressive; make it 4 rows AND 4 columns." } ] }

reply_comment { "comment_id": "cm_3f21",
                "body": "Tightened to 4 rows and 4 columns. A proposal addresses this." }

propose_edit { "path": "…/SKILL.md", "content": "…", "addressing": "cm_3f21",
               "message": "Tighten the proactive-table threshold" }
```

Accepting that proposal resolves the thread automatically. Rejecting leaves the
comment open and files the rejection note into the thread as a reply, so the
feedback lands where you are already looking.
