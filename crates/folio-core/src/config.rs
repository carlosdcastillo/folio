//! Tunables. Every default here is a number the spec names.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Snapshots of identical content inside this window collapse into one.
    /// This is the double-watcher guard: the GUI and a headless bridge both
    /// watching a root cannot produce duplicate history entries.
    pub coalesce_ms: i64,

    /// Runaway-agent guards.
    pub max_pending_proposals: i64,
    pub max_blob_bytes: u64,

    /// Per-client comment replies allowed per hour.
    pub reply_rate_limit: i64,
    pub reply_rate_window_ms: i64,

    /// Lines of context stored on each side of a comment anchor.
    pub anchor_context_lines: usize,

    /// Watcher debounce. Long enough to collapse an editor's write-truncate-
    /// write dance, short enough to stay under the 1 s latency budget.
    pub watch_debounce_ms: u64,

    /// A client is "connected" if it has been seen this recently.
    pub client_active_window_ms: i64,

    /// Files larger than this are indexed but never read into the editor.
    pub max_editor_bytes: u64,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            coalesce_ms: 2_000,
            max_pending_proposals: 500,
            max_blob_bytes: 10 * 1024 * 1024,
            reply_rate_limit: 60,
            reply_rate_window_ms: 60 * 60 * 1000,
            anchor_context_lines: 2,
            watch_debounce_ms: 300,
            client_active_window_ms: 5 * 60 * 1000,
            max_editor_bytes: 4 * 1024 * 1024,
        }
    }
}
