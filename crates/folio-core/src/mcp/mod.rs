//! The MCP server.
//!
//! The MCP surface is the product's API; the UI is one client among two. Every
//! tool here is a thin name-and-schema over an [`crate::api::Folio::dispatch`]
//! operation, which is what stops the agent-facing surface from ever drifting
//! away from what the app itself does.
//!
//! The server is transport-agnostic about *where* the core lives. Behind
//! [`Backend`] there are two shapes:
//!
//! * **bridged** — a Folio app is running; tool calls travel over the local
//!   IPC socket so the app stays the single watcher and the live review surface;
//! * **headless** — no app is running; the core is embedded. Reads work,
//!   direct writes work, and proposals queue durably in the store for review
//!   the next time the app opens.

use crate::api::{Caller, Folio};
use crate::error::Result;
use serde_json::{json, Map, Value};
use std::sync::{Arc, Mutex};

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    InitializeResult, ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerInfo,
    Tool, ToolAnnotations,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{ErrorData as McpError, ServerHandler};

/// Where the tools' work actually happens.
pub trait Backend: Send + Sync + 'static {
    fn call(&self, caller: &Caller, op: &str, params: &Value) -> Result<Value>;
    /// `bridged` or `headless`, for the status bar and for logging.
    fn mode(&self) -> &'static str;
}

/// The core, embedded in this process.
pub struct Embedded {
    pub folio: Arc<Folio>,
}

impl Embedded {
    pub fn new(folio: Arc<Folio>) -> Embedded {
        Embedded { folio }
    }
}

impl Backend for Embedded {
    fn call(&self, caller: &Caller, op: &str, params: &Value) -> Result<Value> {
        self.folio.dispatch(caller, op, params)
    }
    fn mode(&self) -> &'static str {
        "headless"
    }
}

// ---------------------------------------------------------------------------
// Tool definitions
// ---------------------------------------------------------------------------

struct ToolDef {
    name: &'static str,
    /// The dispatch operation this tool calls.
    op: &'static str,
    description: &'static str,
    read_only: bool,
    schema: fn() -> Value,
}

fn object(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

fn str_prop(description: &str) -> Value {
    json!({ "type": "string", "description": description })
}

fn bool_prop(description: &str) -> Value {
    json!({ "type": "boolean", "description": description })
}

const PATH_DOC: &str = "Path to the document. Absolute, `~/`-relative, or relative to a registered root. Anything that resolves outside every root is rejected.";
const AUTHOR_DOC: &str = "The model making this change, recorded as the version's author (e.g. `claude-sonnet-4.6`). Defaults to the MCP client name.";

/// The full tool surface. Everything the UI does, an agent can do — except
/// decide: accepting, rejecting, and resolving stay with the human.
fn tools() -> Vec<ToolDef> {
    vec![
        // -- corpus navigation ------------------------------------------------
        ToolDef {
            name: "list_roots",
            op: "list_roots",
            description: "List the registered roots that make up the corpus, with each root's kind, write policy, and document count.",
            read_only: true,
            schema: || object(json!({}), &[]),
        },
        ToolDef {
            name: "add_root",
            op: "add_root",
            description: "Register a directory or a single file with Folio. Everything inside is indexed and versioned from then on.",
            read_only: false,
            schema: || {
                object(
                    json!({
                        "path": str_prop("Directory or file to register."),
                        "policy": {
                            "type": "string",
                            "enum": ["auto", "propose", "direct"],
                            "description": "Write policy. `auto` (the default) proposes edits to docs, skills and prompts, and applies task-list edits directly.",
                        },
                        "label": str_prop("Display name for the sidebar. Defaults to the folder name."),
                    }),
                    &["path"],
                )
            },
        },
        ToolDef {
            name: "list_docs",
            op: "list_docs",
            description: "List documents in the corpus with their artifact type, version count, pending-proposal count, and open-comment count.",
            read_only: true,
            schema: || {
                object(
                    json!({
                        "root": str_prop("Restrict to one root, by id, label, or path."),
                        "type": {
                            "type": "string",
                            "enum": ["skill", "prompt", "task_list", "doc", "asset"],
                            "description": "Restrict to one artifact type.",
                        },
                    }),
                    &[],
                )
            },
        },
        ToolDef {
            name: "read_doc",
            op: "read_doc",
            description: "Read a document: its content, parsed frontmatter, artifact type, current version id, and effective write policy.",
            read_only: true,
            schema: || object(json!({ "path": str_prop(PATH_DOC) }), &["path"]),
        },
        ToolDef {
            name: "search_docs",
            op: "search_docs",
            description: "Search the corpus. Ripgrep semantics: literal by default, regex on request, with an optional glob filter. Returns line numbers and snippets.",
            read_only: true,
            schema: || {
                object(
                    json!({
                        "query": str_prop("Text or regular expression to search for."),
                        "regex": bool_prop("Treat `query` as a regular expression."),
                        "glob": str_prop("Only search paths matching this glob, e.g. `**/SKILL.md`."),
                        "case_sensitive": bool_prop("Match case exactly. Defaults to false."),
                        "max_results": { "type": "integer", "description": "Cap on returned matches (default 200)." },
                    }),
                    &["query"],
                )
            },
        },
        // -- versioning -------------------------------------------------------
        ToolDef {
            name: "list_versions",
            op: "list_versions",
            description: "The version timeline for a document: id, source, author, message, timestamp, and size delta, newest first.",
            read_only: true,
            schema: || {
                object(
                    json!({
                        "path": str_prop(PATH_DOC),
                        "limit": { "type": "integer", "description": "Maximum versions to return (default 200)." },
                    }),
                    &["path"],
                )
            },
        },
        ToolDef {
            name: "read_version",
            op: "read_version",
            description: "Read the content of a historical version.",
            read_only: true,
            schema: || {
                object(
                    json!({
                        "path": str_prop(PATH_DOC),
                        "version_id": str_prop("A version id from list_versions, or `latest`, or `~1` for one version back."),
                    }),
                    &["path", "version_id"],
                )
            },
        },
        ToolDef {
            name: "diff_versions",
            op: "diff_versions",
            description: "Diff two versions of a document with the same prose-aware engine the UI uses: paragraph-grouped hunks with word-level highlights, plus a unified-diff rendering.",
            read_only: true,
            schema: || {
                object(
                    json!({
                        "path": str_prop(PATH_DOC),
                        "from": str_prop("Older version id, or `~1`. Defaults to the previous version."),
                        "to": str_prop("Newer version id, or `latest`. Defaults to the current version."),
                    }),
                    &["path"],
                )
            },
        },
        ToolDef {
            name: "checkpoint",
            op: "checkpoint",
            description: "Mark a milestone in a document's timeline with a message. Records a version even when the content has not changed.",
            read_only: false,
            schema: || {
                object(
                    json!({
                        "path": str_prop(PATH_DOC),
                        "message": str_prop("What this checkpoint marks, e.g. `before restructuring commands`."),
                        "author": str_prop(AUTHOR_DOC),
                    }),
                    &["path", "message"],
                )
            },
        },
        // -- writing ----------------------------------------------------------
        ToolDef {
            name: "propose_edit",
            op: "propose_edit",
            description: "Propose a narrowly scoped change against the exact document version you read. Existing documents require `base_version`; stale edits are rejected. Prefer `patch` so unchanged content is not regenerated. Under the default policy the human reviews each hunk before anything reaches disk.",
            read_only: false,
            schema: || {
                object(
                    json!({
                        "path": str_prop(PATH_DOC),
                        "base_version": str_prop("Required for an existing document. Pass the exact `version` returned by read_doc; the proposal fails if that version is no longer current."),
                        "content": str_prop("The complete new content. Prefer this only when creating a new file; use `patch` for existing documents."),
                        "patch": str_prop("A focused unified diff against `base_version`. Use instead of `content`, never with it."),
                        "intent": str_prop("A concise statement of the requested outcome and why this particular change satisfies it. The reviewer sees this above the diff."),
                        "message": str_prop("Deprecated alias for `intent`."),
                        "addressing": str_prop("The id of a comment this change answers, e.g. `cm_3f21`. Accepting the proposal resolves that thread automatically."),
                        "author": str_prop(AUTHOR_DOC),
                    }),
                    &["path", "intent"],
                )
            },
        },
        ToolDef {
            name: "task_list",
            op: "task_list",
            description: "List tasks across the corpus or in one document. Ids are line-anchored (`L42`) and valid only for the `version` they were read from; pass that version back to task_set_status.",
            read_only: true,
            schema: || {
                object(
                    json!({
                        "doc": str_prop("Restrict to one document. Omit to aggregate every task list in the corpus."),
                        "owner": str_prop("Only tasks assigned with this `@owner`."),
                        "tag": str_prop("Only tasks carrying this `#tag`."),
                        "open_only": bool_prop("Skip completed tasks."),
                        "query": str_prop("Only tasks whose text contains this."),
                    }),
                    &[],
                )
            },
        },
        ToolDef {
            name: "task_add",
            op: "task_add",
            description: "Add a task to a task list. Applied immediately under the default policy for task lists; queued as a proposal if the path's policy says so.",
            read_only: false,
            schema: || {
                object(
                    json!({
                        "doc": str_prop("The task list to add to."),
                        "text": str_prop("The task text, without the `@owner` and `#tag` tokens."),
                        "owner": str_prop("Assign to this owner, written as `@owner`."),
                        "tag": {
                            "type": ["string", "array"],
                            "items": { "type": "string" },
                            "description": "One or more tags, written as `#tag`.",
                        },
                        "author": str_prop(AUTHOR_DOC),
                    }),
                    &["doc", "text"],
                )
            },
        },
        ToolDef {
            name: "task_set_status",
            op: "task_set_status",
            description: "Tick or untick a task. Read the list first: if the document has moved on since the version you read, this fails and returns the current list so you can retry without a second read.",
            read_only: false,
            schema: || {
                object(
                    json!({
                        "doc": str_prop("The task list holding the task."),
                        "task_id": str_prop("A line-anchored id from task_list, e.g. `L42`."),
                        "status": {
                            "type": "string",
                            "enum": ["done", "open"],
                            "description": "The new state.",
                        },
                        "version": str_prop("The `version` task_list reported. Strongly recommended: it is what makes a stale write fail loudly instead of ticking the wrong line."),
                        "text": str_prop("The task text you expect at this id, as a second safety check."),
                        "author": str_prop(AUTHOR_DOC),
                    }),
                    &["doc", "task_id", "status"],
                )
            },
        },
        // -- artifact intelligence and feedback --------------------------------
        ToolDef {
            name: "validate_doc",
            op: "validate_doc",
            description: "Validate a document against the rules for its type: skill frontmatter, progressive-disclosure structure, reference integrity, the command inventory cross-check, and any lint rules the skill declares about itself; prompt slots against declared variables.",
            read_only: true,
            schema: || object(json!({ "path": str_prop(PATH_DOC) }), &["path"]),
        },
        ToolDef {
            name: "render_prompt",
            op: "render_prompt",
            description: "Fill a prompt template's `{{slots}}` and return the text. Folio renders prompts; it never sends them to a model.",
            read_only: true,
            schema: || {
                object(
                    json!({
                        "path": str_prop(PATH_DOC),
                        "variables": {
                            "type": "object",
                            "description": "Values for the template's slots, keyed by variable name.",
                            "additionalProperties": true,
                        },
                    }),
                    &["path"],
                )
            },
        },
        ToolDef {
            name: "list_proposals",
            op: "list_proposals",
            description: "List proposals with their status. This is the feedback loop: a rejected proposal carries the reviewer's note saying why, so read your own rejections before proposing again.",
            read_only: true,
            schema: || {
                object(
                    json!({
                        "status": {
                            "type": "string",
                            "enum": ["pending", "accepted", "rejected", "superseded", "conflict"],
                            "description": "Restrict to one status.",
                        },
                        "path": str_prop("Restrict to proposals for one document."),
                        "mine_only": bool_prop("Only proposals from this MCP client."),
                        "limit": { "type": "integer", "description": "Maximum proposals to return." },
                    }),
                    &[],
                )
            },
        },
        ToolDef {
            name: "list_comments",
            op: "list_comments",
            description: "List anchored comment threads. Open threads are work items addressed to you: read one, reply to it, then propose an edit with `addressing` set to its id. Accepting that proposal resolves the thread.",
            read_only: true,
            schema: || {
                object(
                    json!({
                        "path": str_prop("Restrict to one document."),
                        "status": {
                            "type": "string",
                            "enum": ["open", "resolved", "outdated", "orphaned"],
                            "description": "`open` is your inbox. `outdated` means the anchored text has since changed.",
                        },
                        "mine_only": bool_prop("Only threads you have taken part in."),
                        "limit": { "type": "integer", "description": "Maximum threads to return." },
                    }),
                    &[],
                )
            },
        },
        ToolDef {
            name: "reply_comment",
            op: "reply_comment",
            description: "Reply in a comment thread. Use it to say what you are going to do before you propose it; the reply lands where the reviewer is already looking.",
            read_only: false,
            schema: || {
                object(
                    json!({
                        "comment_id": str_prop("The thread to reply in, e.g. `cm_3f21`."),
                        "body": str_prop("The reply text."),
                        "author": str_prop(AUTHOR_DOC),
                    }),
                    &["comment_id", "body"],
                )
            },
        },
    ]
}

const INSTRUCTIONS: &str = "\
Folio is the system of record for this developer's agent-adjacent markdown: docs, prompts, \
task lists, and skills. Files on disk stay plain markdown; Folio keeps the history, the \
review queue, and the comment threads beside them.

Three things to know before you write:

1. Your edits are proposals, not writes. `propose_edit` queues a changeset for human review \
by default. It has not landed until the human accepts it. Task lists are the exception and \
apply immediately, because a list maintained overnight must not block on review.

2. Rejections carry reasons. Call `list_proposals` with `status: rejected` and `mine_only: true` \
before re-proposing; the reviewer's note says what to change.

3. Open comments are your to-do list. `list_comments` with `status: open` is an inbox of \
passages the human has flagged. Reply in the thread, then send a `propose_edit` with \
`addressing` set to the comment id. Accepting it resolves the thread.

Read before you write. For `propose_edit`, pass the exact `version` from `read_doc` as \
`base_version`, prefer a focused patch, and preserve constraints outside the requested scope. \
Task ids are line-anchored and only valid for the version they came from, so pass `version` \
back to `task_set_status`.";

// ---------------------------------------------------------------------------
// The server
// ---------------------------------------------------------------------------

pub struct FolioServer {
    backend: Arc<dyn Backend>,
    /// Learned from the MCP `initialize` handshake.
    client: Mutex<String>,
    /// Default author when a tool call does not name one.
    default_author: Mutex<Option<String>>,
}

impl FolioServer {
    pub fn new(backend: Arc<dyn Backend>) -> FolioServer {
        FolioServer {
            backend,
            client: Mutex::new("mcp-client".to_string()),
            default_author: Mutex::new(std::env::var("FOLIO_AUTHOR").ok().filter(|a| !a.is_empty())),
        }
    }

    fn client_name(&self) -> String {
        self.client.lock().map(|c| c.clone()).unwrap_or_else(|e| e.into_inner().clone())
    }

    /// Learn who is connected from the handshake the request carries, and note
    /// it in the store so the status bar can list the connected clients.
    fn note_client(&self, context: &RequestContext<RoleServer>) {
        let Some(info) = context.client_info() else { return };
        let name = info.name;
        if name.is_empty() {
            return;
        }
        let changed = match self.client.lock() {
            Ok(mut current) => {
                let changed = *current != name;
                *current = name.clone();
                changed
            }
            Err(_) => true,
        };
        // The heartbeat is what makes a client "active", so refresh it on
        // every call, not only when the name changes.
        let _ = changed;
        let _ = self.backend.call(
            &Caller::agent(name.clone(), name.clone()),
            "touch_client",
            &json!({ "name": name, "mode": self.backend.mode() }),
        );
    }

    fn caller(&self, arguments: &Map<String, Value>) -> Caller {
        let client = self.client_name();
        let author = arguments
            .get("author")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| {
                self.default_author
                    .lock()
                    .ok()
                    .and_then(|a| a.clone())
            })
            .unwrap_or_else(|| client.clone());
        Caller::agent(author, client)
    }
}

impl ServerHandler for FolioServer {
    fn get_info(&self) -> ServerInfo {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(
                Implementation::new("folio", crate::VERSION)
                    .with_title("Folio")
                    .with_description("Versioned, reviewable markdown for the files your agents read and write."),
            )
            .with_instructions(INSTRUCTIONS)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> std::result::Result<ListToolsResult, McpError> {
        let tools = tools()
            .into_iter()
            .map(|def| {
                let schema = (def.schema)();
                let schema_object = schema
                    .as_object()
                    .cloned()
                    .unwrap_or_default();
                Tool::new(def.name, def.description, std::sync::Arc::new(schema_object)).annotate(
                    ToolAnnotations::new()
                        .read_only(def.read_only)
                        // Nothing here destroys data: every write is
                        // snapshotted, and every snapshot is restorable.
                        .destructive(false)
                        .open_world(false),
                )
            })
            .collect();
        Ok(ListToolsResult::with_all_items(tools))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> std::result::Result<CallToolResponse, McpError> {
        self.note_client(&context);
        let arguments = request.arguments.clone().unwrap_or_default();
        let Some(def) = tools().into_iter().find(|t| t.name == request.name) else {
            return Err(McpError::invalid_params(
                format!("unknown tool `{}`", request.name),
                None,
            ));
        };

        let caller = self.caller(&arguments);
        let params = Value::Object(arguments);

        match self.backend.call(&caller, def.op, &params) {
            Ok(value) => {
                let text = serde_json::to_string_pretty(&value)
                    .unwrap_or_else(|_| value.to_string());
                let mut result = CallToolResult::success(vec![ContentBlock::text(text)]);
                result.structured_content = Some(value);
                Ok(result.into())
            }
            // A tool that ran and did not work is a tool-level error: the
            // message reaches the agent, which is the whole point of the
            // rejection-note and stale-list designs.
            Err(e) => {
                let payload = e.to_wire();
                let text = serde_json::to_string_pretty(&payload).unwrap_or_else(|_| e.to_string());
                let mut result = CallToolResult::error(vec![ContentBlock::text(text)]);
                result.structured_content = Some(payload);
                Ok(result.into())
            }
        }
    }
}

/// Run the stdio MCP server until the client disconnects.
#[cfg(feature = "mcp")]
pub async fn serve_stdio(backend: Arc<dyn Backend>) -> std::result::Result<(), Box<dyn std::error::Error>> {
    use rmcp::transport::io::stdio;
    use rmcp::ServiceExt;

    let service = FolioServer::new(backend).serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tool_surface_matches_the_specification() {
        let names: Vec<&str> = tools().iter().map(|t| t.name).collect();
        for expected in [
            "list_roots", "list_docs", "read_doc", "search_docs",
            "list_versions", "read_version", "diff_versions", "checkpoint",
            "propose_edit", "task_list", "task_add", "task_set_status",
            "validate_doc", "render_prompt", "list_proposals", "list_comments", "reply_comment",
            "add_root",
        ] {
            assert!(names.contains(&expected), "missing tool {expected}");
        }
        assert_eq!(names.len(), 18, "tool surface changed: {names:?}");
    }

    #[test]
    fn every_tool_has_a_valid_object_schema() {
        for def in tools() {
            let schema = (def.schema)();
            assert_eq!(schema["type"], "object", "{} schema is not an object", def.name);
            assert!(schema.get("properties").is_some(), "{} has no properties", def.name);
            let required = schema["required"].as_array().expect("required must be an array");
            for key in required {
                let key = key.as_str().unwrap();
                assert!(
                    schema["properties"].get(key).is_some(),
                    "{} requires `{key}` but does not declare it",
                    def.name
                );
            }
        }
    }

    #[test]
    fn proposals_expose_the_version_bound_review_contract() {
        let def = tools()
            .into_iter()
            .find(|def| def.name == "propose_edit")
            .unwrap();
        let schema = (def.schema)();
        assert!(schema["properties"].get("base_version").is_some());
        assert!(schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value.as_str() == Some("intent")));
    }

    #[test]
    fn no_tool_lets_an_agent_decide_a_proposal_or_resolve_a_thread() {
        // The human keeps judgement. These operations exist in dispatch but
        // are deliberately absent from the agent-facing surface.
        let names: Vec<&str> = tools().iter().map(|t| t.op).collect();
        for forbidden in [
            "accept_proposal", "reject_proposal", "rebase_proposal",
            "create_comment", "resolve_comment", "delete_comment",
            "save_doc", "restore_version",
        ] {
            assert!(!names.contains(&forbidden), "{forbidden} must not be an MCP tool");
        }
    }
}
