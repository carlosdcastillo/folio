//! One error type for the whole core.
//!
//! Every shell (Tauri commands, the local IPC socket, the MCP bridge) turns
//! these into wire errors, so each variant carries a stable machine-readable
//! `code` alongside its human message. `Stale` additionally carries a JSON
//! payload: the spec requires a stale `task_set_status` to fail *with the
//! current list attached* so the agent can re-read without a second round trip.

use serde_json::Value;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Io(#[from] std::io::Error),

    #[error("store: {0}")]
    Db(#[from] rusqlite::Error),

    #[error("json: {0}")]
    Json(#[from] serde_json::Error),

    #[error("not found: {0}")]
    NotFound(String),

    /// Path sandboxing. Anything that resolves outside every registered root
    /// — `..` traversal, an absolute path elsewhere, a symlink that escapes.
    #[error("path is outside every registered root: {0}")]
    OutsideRoot(String),

    #[error("write policy for {path} is `{policy}`; this write must go through a proposal")]
    PolicyDenied { path: String, policy: String },

    #[error("conflict: {0}")]
    Conflict(String),

    #[error("invalid: {0}")]
    Invalid(String),

    #[error("too large: {0}")]
    TooLarge(String),

    #[error("queue full: {0}")]
    QueueFull(String),

    #[error("rate limited: {0}")]
    RateLimited(String),

    /// The caller acted on a snapshot that is no longer current. `data` holds
    /// whatever the caller needs to retry (for task ids: the current list).
    #[error("{message}")]
    Stale { message: String, data: Box<Value> },

    #[error("unknown operation: {0}")]
    UnknownOp(String),

    #[error("{0}")]
    Other(String),
}

impl Error {
    pub fn other(msg: impl Into<String>) -> Self {
        Error::Other(msg.into())
    }

    pub fn invalid(msg: impl Into<String>) -> Self {
        Error::Invalid(msg.into())
    }

    pub fn not_found(msg: impl Into<String>) -> Self {
        Error::NotFound(msg.into())
    }

    pub fn stale(msg: impl Into<String>, data: Value) -> Self {
        Error::Stale { message: msg.into(), data: Box::new(data) }
    }

    /// Stable identifier for the wire. Clients branch on this, never on prose.
    pub fn code(&self) -> &'static str {
        match self {
            Error::Io(_) => "io",
            Error::Db(_) => "store",
            Error::Json(_) => "json",
            Error::NotFound(_) => "not_found",
            Error::OutsideRoot(_) => "outside_root",
            Error::PolicyDenied { .. } => "policy_denied",
            Error::Conflict(_) => "conflict",
            Error::Invalid(_) => "invalid",
            Error::TooLarge(_) => "too_large",
            Error::QueueFull(_) => "queue_full",
            Error::RateLimited(_) => "rate_limited",
            Error::Stale { .. } => "stale",
            Error::UnknownOp(_) => "unknown_op",
            Error::Other(_) => "error",
        }
    }

    /// Structured detail a client can act on without re-parsing the message.
    pub fn data(&self) -> Option<&Value> {
        match self {
            Error::Stale { data, .. } => Some(data),
            _ => None,
        }
    }

    pub fn to_wire(&self) -> Value {
        let mut obj = serde_json::json!({
            "code": self.code(),
            "message": self.to_string(),
        });
        if let Some(data) = self.data() {
            obj["data"] = data.clone();
        }
        obj
    }
}

impl From<serde_yaml_ng::Error> for Error {
    fn from(e: serde_yaml_ng::Error) -> Self {
        Error::Invalid(format!("frontmatter: {e}"))
    }
}
