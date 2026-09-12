//! `folio-core` — everything Folio knows how to do.
//!
//! This is a plain Rust library. The Tauri app and the `folio mcp` stdio
//! bridge are thin shells over it, and both reach it through the same
//! [`api::dispatch`] entry point. That is the single most important structural
//! decision in the product: the MCP surface cannot drift from the UI, because
//! there is only one implementation underneath both.

pub mod artifact;
pub mod comment;
pub mod config;
pub mod corpus;
pub mod diff;
pub mod error;
pub mod event;
pub mod instrumentation;
pub mod proposal;
pub mod search;
pub mod store;
pub mod util;
pub mod version;
pub mod watch;

pub mod api;

#[cfg(feature = "mcp")]
pub mod mcp;

pub use api::Folio;
pub use config::Config;
pub use error::{Error, Result};

/// The product version, surfaced in About and in the MCP server info.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
