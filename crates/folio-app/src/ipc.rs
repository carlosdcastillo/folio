//! The local IPC socket that joins the two personalities.
//!
//! When the GUI is running it is the single watcher and the live review
//! surface, so the MCP bridge forwards tool calls to it rather than opening a
//! second core over the same store. The socket binds 127.0.0.1 only and is
//! authenticated with the token file in the store directory; the port and
//! token are published in `ipc.json` beside it.
//!
//! Write policy is enforced in `folio-core`, not here, so nothing that reaches
//! this socket can bypass it.

use folio_core::api::{Caller, Folio};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Endpoint {
    pub port: u16,
    pub token: String,
    pub pid: u32,
    pub version: String,
    /// Which store the running app has open. A bridge told nothing follows
    /// this; a bridge told a specific store checks it against this and refuses
    /// to bridge across a mismatch rather than silently writing somewhere else.
    #[serde(default)]
    pub store: String,
}

#[derive(Debug, Deserialize)]
struct Request {
    token: String,
    op: String,
    #[serde(default)]
    params: Value,
    #[serde(default)]
    caller: Option<Caller>,
}

/// Where a running app announces itself.
///
/// Deliberately *not* inside the app's own store: a client configured with a
/// bare `folio mcp` has no idea which store the app was launched with, and a
/// store-relative rendezvous means it can never find an app started with
/// `--store`. The two would then quietly work on different stores, which looks
/// exactly like "my tool calls do nothing".
fn rendezvous_path() -> PathBuf {
    folio_core::store::standard_store_dir().join("ipc.json")
}

/// Compare two store directories the way the filesystem would.
fn same_dir(a: &std::path::Path, b: &std::path::Path) -> bool {
    let key = |p: &std::path::Path| folio_core::util::canonical_key(p);
    folio_core::util::path_eq(&key(a), &key(b))
}

/// Start accepting connections. The returned guard removes `ipc.json` on drop,
/// so a bridge started after the app quits falls back to headless instead of
/// dialling a dead port.
pub struct Server {
    endpoint_file: PathBuf,
    pub port: u16,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.endpoint_file);
    }
}

pub fn serve(folio: Arc<Folio>) -> std::io::Result<Server> {
    // Port 0: the OS picks a free one, and we publish it.
    let listener = TcpListener::bind(SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)))?;
    let port = listener.local_addr()?.port();
    let token = folio
        .store
        .ipc_token()
        .map_err(|e| std::io::Error::other(e.to_string()))?;

    let endpoint_file = rendezvous_path();
    if let Some(parent) = endpoint_file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(
        &endpoint_file,
        serde_json::to_vec_pretty(&Endpoint {
            port,
            token: token.clone(),
            pid: std::process::id(),
            version: folio_core::VERSION.to_string(),
            store: folio.store.dir().to_string_lossy().to_string(),
        })?,
    )?;

    std::thread::Builder::new()
        .name("folio-ipc".into())
        .spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let folio = Arc::clone(&folio);
                let token = token.clone();
                std::thread::spawn(move || {
                    if let Err(e) = handle(stream, folio, &token) {
                        eprintln!("folio: ipc connection ended: {e}");
                    }
                });
            }
        })?;

    Ok(Server { endpoint_file, port })
}

fn handle(stream: TcpStream, folio: Arc<Folio>, token: &str) -> std::io::Result<()> {
    stream.set_nodelay(true)?;
    let mut writer = stream.try_clone()?;
    let reader = BufReader::new(stream);

    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Request>(&line) {
            Err(e) => json!({ "ok": false, "error": { "code": "invalid", "message": e.to_string() } }),
            Ok(request) => {
                // Constant-time-ish comparison is overkill for a loopback
                // socket with a 192-bit token, but rejecting on length first
                // costs nothing.
                if request.token.len() != token.len() || request.token != token {
                    json!({ "ok": false, "error": { "code": "unauthorized", "message": "bad token" } })
                } else {
                    let caller = request.caller.unwrap_or_else(|| {
                        Caller::agent("external", "folio-bridge")
                    });
                    match folio.dispatch(&caller, &request.op, &request.params) {
                        Ok(result) => json!({ "ok": true, "result": result }),
                        Err(e) => json!({ "ok": false, "error": e.to_wire() }),
                    }
                }
            }
        };
        writeln!(writer, "{response}")?;
        writer.flush()?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Client side (the bridge)
// ---------------------------------------------------------------------------

pub struct Client {
    stream: std::sync::Mutex<(BufReader<TcpStream>, TcpStream)>,
    token: String,
}

impl Client {
    /// Connect to a running app, or fail so the caller can fall back to an
    /// embedded core. Returns the app's store alongside the client, because
    /// that — not the bridge's default — is the store the session is on.
    pub fn connect() -> Result<(Client, PathBuf), String> {
        let raw = std::fs::read(rendezvous_path())
            .map_err(|e| format!("no running Folio app ({e})"))?;
        let endpoint: Endpoint =
            serde_json::from_slice(&raw).map_err(|e| format!("unreadable ipc.json: {e}"))?;

        // Told a specific store, the bridge must use that one. Bridging to an
        // app on a different store would put the work somewhere the caller did
        // not ask for, and the caller would never see it.
        let app_store = PathBuf::from(&endpoint.store);
        if let Some(requested) = folio_core::store::store_dir_override() {
            if !same_dir(&requested, &app_store) {
                return Err(format!(
                    "a Folio app is running but on a different store ({}); \
                     FOLIO_STORE asks for {}",
                    app_store.display(),
                    requested.display()
                ));
            }
        }

        let address = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, endpoint.port));
        let stream = TcpStream::connect_timeout(&address, Duration::from_millis(500))
            .map_err(|e| format!("cannot reach the Folio app on port {}: {e}", endpoint.port))?;
        stream.set_nodelay(true).ok();

        // A stale `ipc.json` can point at a port something else now owns. That
        // something else may accept the connection and then never answer, so
        // the handshake read is bounded before anything else happens.
        stream
            .set_read_timeout(Some(Duration::from_millis(1500)))
            .map_err(|e| e.to_string())?;

        let client = Client {
            stream: std::sync::Mutex::new((BufReader::new(stream.try_clone().map_err(|e| e.to_string())?), stream)),
            token: endpoint.token,
        };

        // Prove it is really Folio before trusting the connection.
        match client.call_raw("ping", &json!({}), None) {
            Ok(value) if value.get("ok").and_then(Value::as_bool) == Some(true) => {
                // Real calls may legitimately take a while — indexing a large
                // root, diffing a large document — so the bound is relaxed once
                // we know who is on the other end.
                client.set_read_timeout(Duration::from_secs(120));
                Ok((client, app_store))
            }
            Ok(other) => Err(format!("unexpected reply from the app: {other}")),
            Err(e) => Err(e),
        }
    }

    fn set_read_timeout(&self, timeout: Duration) {
        if let Ok(guard) = self.stream.lock() {
            let _ = guard.1.set_read_timeout(Some(timeout));
        }
    }

    pub fn call_raw(&self, op: &str, params: &Value, caller: Option<&Caller>) -> Result<Value, String> {
        let request = json!({
            "token": self.token,
            "op": op,
            "params": params,
            "caller": caller,
        });

        let mut guard = self.stream.lock().map_err(|_| "ipc lock poisoned".to_string())?;
        let (reader, writer) = &mut *guard;
        writeln!(writer, "{request}").map_err(|e| e.to_string())?;
        writer.flush().map_err(|e| e.to_string())?;

        let mut line = String::new();
        let read = reader.read_line(&mut line).map_err(|e| e.to_string())?;
        if read == 0 {
            return Err("the Folio app closed the connection".into());
        }
        serde_json::from_str::<Value>(&line).map_err(|e| e.to_string())
    }
}

/// The bridge's view of a running app.
///
/// If the app goes away mid-session — you closed the window while an agent was
/// working — the bridge falls back to an embedded core rather than failing the
/// agent's call. Both sides share one store, so the fallback is not a different
/// Folio; it is the same one, without a window.
pub struct BridgedBackend {
    client: std::sync::Mutex<Option<Client>>,
    fallback: std::sync::Mutex<Option<Arc<Folio>>>,
}

impl BridgedBackend {
    pub fn new(client: Client) -> BridgedBackend {
        BridgedBackend {
            client: std::sync::Mutex::new(Some(client)),
            fallback: std::sync::Mutex::new(None),
        }
    }

    /// Send over the socket, or `Err` describing why the socket is unusable.
    fn try_socket(&self, caller: &Caller, op: &str, params: &Value) -> std::result::Result<Value, String> {
        let guard = self.client.lock().map_err(|_| "ipc lock poisoned".to_string())?;
        let client = guard.as_ref().ok_or_else(|| "the app is gone".to_string())?;
        client.call_raw(op, params, Some(caller))
    }

    fn go_headless(&self, reason: &str) -> folio_core::Result<Arc<Folio>> {
        if let Ok(mut client) = self.client.lock() {
            if client.take().is_some() {
                eprintln!("folio mcp: {reason}; continuing headless against the same store");
            }
        }
        let mut fallback = self
            .fallback
            .lock()
            .map_err(|_| folio_core::Error::other("fallback lock poisoned"))?;
        if fallback.is_none() {
            *fallback = Some(Folio::open_default()?);
        }
        Ok(Arc::clone(fallback.as_ref().expect("just populated")))
    }
}

impl folio_core::mcp::Backend for BridgedBackend {
    fn call(&self, caller: &Caller, op: &str, params: &Value) -> folio_core::Result<Value> {
        let response = match self.try_socket(caller, op, params) {
            Ok(response) => response,
            Err(reason) => return self.go_headless(&reason)?.dispatch(caller, op, params),
        };

        if response.get("ok").and_then(Value::as_bool) == Some(true) {
            return Ok(response.get("result").cloned().unwrap_or(Value::Null));
        }
        let error = response.get("error").cloned().unwrap_or(Value::Null);
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("the Folio app rejected the call")
            .to_string();

        // Rebuild the error kind so the agent sees the same code it would get
        // from an embedded core. The two paths must be indistinguishable.
        Err(match error.get("code").and_then(Value::as_str) {
            Some("not_found") => folio_core::Error::NotFound(message),
            Some("outside_root") => folio_core::Error::OutsideRoot(message),
            Some("conflict") => folio_core::Error::Conflict(message),
            Some("invalid") => folio_core::Error::Invalid(message),
            Some("too_large") => folio_core::Error::TooLarge(message),
            Some("queue_full") => folio_core::Error::QueueFull(message),
            Some("rate_limited") => folio_core::Error::RateLimited(message),
            Some("policy_denied") => folio_core::Error::PolicyDenied {
                path: message,
                policy: "propose".into(),
            },
            Some("stale") => folio_core::Error::stale(
                message,
                error.get("data").cloned().unwrap_or(Value::Null),
            ),
            _ => folio_core::Error::Other(message),
        })
    }

    fn mode(&self) -> &'static str {
        // Reported at the handshake, before any fallback can have happened.
        "bridged"
    }
}
