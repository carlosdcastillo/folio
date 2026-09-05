//! The Tauri shell.
//!
//! Deliberately thin. It owns three things: the window, one command that
//! forwards to the core's dispatch table, and a subscription that pushes core
//! events straight to the frontend. Tauri events, not polling — the core emits
//! `snapshot-created`, `proposal-arrived`, `comment-activity` and
//! `watcher-status` as they happen.

use folio_core::api::{Caller, Folio};
use folio_core::event::Event;
use serde_json::{json, Value};
use std::sync::Arc;
use tauri::{Emitter, Manager};

pub struct AppState {
    pub folio: Arc<Folio>,
    /// Held for its `Drop`, which removes `ipc.json` when the app exits.
    _ipc: Option<crate::ipc::Server>,
}

/// The one command. Everything the UI does goes through the same dispatch
/// table the MCP tools use, so the two surfaces cannot diverge.
#[tauri::command]
async fn folio_call(
    state: tauri::State<'_, AppState>,
    op: String,
    params: Option<Value>,
) -> Result<Value, Value> {
    let params = params.unwrap_or_else(|| json!({}));
    state
        .folio
        .dispatch(&Caller::human(), &op, &params)
        .map_err(|e| e.to_wire())
}

/// Startup facts the frontend needs before its first paint.
#[tauri::command]
fn folio_boot(state: tauri::State<'_, AppState>) -> Value {
    json!({
        "version": folio_core::VERSION,
        "store_dir": state.folio.store.dir().to_string_lossy(),
        "platform": std::env::consts::OS,
        "cloud_sync_warning": state.folio.store.cloud_sync_warning(),
    })
}

pub fn run() -> i32 {
    let folio = match Folio::open_default() {
        Ok(folio) => folio,
        Err(e) => {
            eprintln!("Folio: cannot open the store: {e}");
            return 1;
        }
    };

    // The socket is what lets `folio mcp` prefer this process.
    let ipc = match crate::ipc::serve(Arc::clone(&folio)) {
        Ok(server) => {
            eprintln!("Folio: MCP bridge endpoint on 127.0.0.1:{}", server.port);
            Some(server)
        }
        Err(e) => {
            eprintln!("Folio: local IPC unavailable, MCP clients will run headless: {e}");
            None
        }
    };

    let state = AppState { folio: Arc::clone(&folio), _ipc: ipc };

    let result = tauri::Builder::default()
        .manage(state)
        .invoke_handler(tauri::generate_handler![folio_call, folio_boot])
        .setup(move |app| {
            let handle = app.handle().clone();

            // One channel, one payload shape: the frontend switches on the
            // event's own `type` field rather than on a Tauri event name.
            folio.bus.subscribe(move |event: &Event| {
                if let Ok(payload) = serde_json::to_value(event) {
                    let _ = handle.emit("folio://event", payload);
                }
            });

            // Start watching before the initial index so a file created while
            // a large root is being walked cannot fall between the two.
            // Both operations stay off the main thread: a 10k-file root must
            // not hold up the first paint.
            let background = Arc::clone(&folio);
            std::thread::spawn(move || {
                if let Err(e) = background.start_watching() {
                    eprintln!("Folio: watcher failed to start: {e}");
                }
                if let Err(e) = background.index_all() {
                    eprintln!("Folio: initial index incomplete: {e}");
                }
                background.bus.emit(Event::CorpusChanged);
            });

            // Theme before first paint. The frontend sets `data-theme` from an
            // inline script so its own content never flashes; this handles the
            // frame before that, by painting the window itself in the theme the
            // user left it in rather than in a default.
            if let Some(window) = app.get_webview_window("main") {
                let light = folio
                    .store
                    .get_meta("ui.theme")
                    .ok()
                    .flatten()
                    .as_deref()
                    == Some("light");
                if light {
                    let _ = window.set_background_color(Some(tauri::window::Color(255, 255, 255, 255)));
                }
            }
            Ok(())
        })
        .run(tauri::generate_context!());

    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("Folio: {e}");
            1
        }
    }
}
