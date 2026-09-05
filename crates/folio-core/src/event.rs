//! The core's event bus.
//!
//! Tauri events, not polling: the Rust core emits these straight to the
//! frontend, so a proposal arriving from an agent is a toast within a
//! heartbeat of the tool call rather than on the next poll tick.

use crate::comment::Comment;
use crate::proposal::Proposal;
use crate::version::Snapshot;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Event {
    /// A new version of a document was recorded, from any of the six triggers.
    SnapshotCreated { snapshot: Box<Snapshot> },
    /// An agent queued a change for review.
    ProposalArrived { proposal: Box<Proposal> },
    /// A proposal was accepted, rejected, or rebased.
    ProposalDecided { proposal: Box<Proposal> },
    /// A thread was created, replied to, or resolved.
    CommentActivity { kind: CommentActivityKind, comment: Box<Comment> },
    /// The file watcher's health, for the status-bar dot.
    WatcherStatus { watching: usize, healthy: bool, message: Option<String> },
    /// The set of connected MCP clients changed.
    ClientsChanged { clients: Vec<ClientInfo> },
    /// A document disappeared from disk.
    DocRemoved { path: String, display: String },
    /// Roots were added or removed; the sidebar should reload.
    CorpusChanged,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CommentActivityKind {
    Created,
    Replied,
    Resolved,
    Reopened,
    Deleted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientInfo {
    pub name: String,
    pub mode: String,
    pub last_seen: i64,
}

type Listener = Box<dyn Fn(&Event) + Send + Sync + 'static>;

/// A fan-out point with no ordering guarantees beyond "in registration order".
/// Listeners must not block: the Tauri listener only forwards to the webview.
#[derive(Clone, Default)]
pub struct Bus {
    listeners: Arc<Mutex<Vec<Listener>>>,
}

impl std::fmt::Debug for Bus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let n = self.listeners.lock().map(|l| l.len()).unwrap_or(0);
        f.debug_struct("Bus").field("listeners", &n).finish()
    }
}

impl Bus {
    pub fn new() -> Bus {
        Bus::default()
    }

    pub fn subscribe(&self, listener: impl Fn(&Event) + Send + Sync + 'static) {
        if let Ok(mut listeners) = self.listeners.lock() {
            listeners.push(Box::new(listener));
        }
    }

    pub fn emit(&self, event: Event) {
        let Ok(listeners) = self.listeners.lock() else { return };
        for listener in listeners.iter() {
            listener(&event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn listeners_receive_events() {
        let bus = Bus::new();
        let count = Arc::new(AtomicUsize::new(0));
        let seen = count.clone();
        bus.subscribe(move |_| {
            seen.fetch_add(1, Ordering::SeqCst);
        });
        bus.emit(Event::CorpusChanged);
        bus.emit(Event::WatcherStatus { watching: 2, healthy: true, message: None });
        assert_eq!(count.load(Ordering::SeqCst), 2);
    }
}
