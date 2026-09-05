//! `folio mcp` — the second personality.
//!
//! MCP clients spawn this as a stdio server. It prefers a running app: the
//! GUI is then the single watcher and the live review surface, and a proposal
//! shows up on screen the moment the tool call returns. With no app running it
//! embeds the core instead — reads work, direct writes work, and proposals
//! queue durably in the store for review the next time the app opens.
//!
//! Nothing here may write to stdout except MCP protocol frames. Diagnostics go
//! to stderr.

use folio_core::api::Folio;
use folio_core::mcp::{Backend, Embedded};
use std::sync::Arc;

pub fn run() -> i32 {
    let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("folio mcp: cannot start a runtime: {e}");
            return 1;
        }
    };

    runtime.block_on(async {
        let backend: Arc<dyn Backend> = match crate::ipc::Client::connect() {
            Ok((client, app_store)) => {
                eprintln!(
                    "folio mcp: bridged to the running Folio app (store: {})",
                    app_store.display()
                );
                // Follow the app's store, so that if it later goes away this
                // session continues against the same data rather than jumping
                // to a different, probably empty, default store.
                std::env::set_var("FOLIO_STORE", &app_store);
                Arc::new(crate::ipc::BridgedBackend::new(client))
            }
            Err(reason) => {
                eprintln!(
                    "folio mcp: running headless ({reason}); store: {}",
                    folio_core::store::default_store_dir().display()
                );
                match Folio::open_default() {
                    Ok(folio) => {
                        // A headless bridge indexes but does not watch: the app
                        // owns watching, and a bridge that outlives its client
                        // should not keep a watcher alive.
                        if let Err(e) = folio.index_all() {
                            eprintln!("folio mcp: initial index incomplete: {e}");
                        }
                        Arc::new(Embedded::new(folio))
                    }
                    Err(e) => {
                        eprintln!("folio mcp: cannot open the store: {e}");
                        return 1;
                    }
                }
            }
        };

        match folio_core::mcp::serve_stdio(backend).await {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("folio mcp: {e}");
                1
            }
        }
    })
}
