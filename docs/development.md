# Development

## Layout

```
crates/folio-core/   the product; a plain library, no shell dependencies
crates/folio-app/    the Tauri shell and the MCP bridge (binary: `folio`)
ui/                  vanilla JS frontend, embedded into the binary at build time
tools/               dev scripts: demo seeding, MCP smoke test, screenshots
docs/                this
```

## Build

```bash
cd ui && npm install && npm run build && cd ..   # CodeMirror bundle, once
cargo build -p folio-app                          # debug
cargo build --release -p folio-app                # release
```

**The frontend is embedded at compile time.** `tauri.conf.json` points
`frontendDist` at `../../ui`, and `tauri::generate_context!` bakes those files
into the binary. After editing anything under `ui/`, rebuild `folio-app` or you
will keep looking at the previous build. On Windows, kill a running `folio.exe`
first — the linker cannot overwrite a running executable.

Only `ui/cm-entry.js` needs `npm run build`; the rest of `ui/` is plain files.

The editor bundle includes fenced-code grammars for Rust, JavaScript, Python,
shell, JSON, YAML, HTML, CSS, and SQL. The focused set increased the minified
bundle from 544,917 bytes to 724,604 bytes (+179,687 bytes); using the full
`@codemirror/language-data` catalogue instead produced 1,600,846 bytes
(+1,055,929 bytes). Keep the focused list unless the build is changed to emit
separate runtime chunks.

## Test

```bash
cargo test -p folio-core          # 90 unit tests + 19 end-to-end
```

`crates/folio-core/tests/end_to_end.rs` is written directly against the
specification's success criteria and drives the real dispatch table with real
files in a temp directory. If you change behaviour, that is the file that
should argue with you.

```bash
python tools/mcp_smoke.py         # the real stdio MCP protocol, against the binary
```

This one spawns `folio mcp` and speaks JSON-RPC to it: the handshake, the tool
schemas, the round trip, the stale-write protocol, and path sandboxing. It
catches the class of bug unit tests cannot — a schema that does not serialise, a
tool wired to the wrong operation, a protocol framing mistake.

## Run it against something real

```bash
python tools/seed_demo.py
FOLIO_STORE="$PWD/.demo/store" ./target/debug/folio
```

`seed_demo.py` builds the corpus the specification describes — a skill tree,
specs, a prompt library, an agent-maintained task list — and then drives
`folio mcp` as two different agents to leave behind pending proposals, an open
comment thread with an agent reply, and overnight history. It seeds through the
real agent path, so what you see is what an agent would actually have produced.

```bash
python tools/watcher_check.py     # with the app running: disk change → snapshot latency
python tools/bridge_failover.py   # the bridge keeps working when the app closes mid-session
python tools/live_probe.py        # which store is a bridge actually attached to?
```

`live_probe.py` exists because of a bug worth remembering: the app and an MCP
client can end up on *different stores*, and the symptom is silently doing
nothing. It makes one mutation over MCP and reports which store it landed in,
and whether the bridge attached to a running app or fell back to headless. Pass
`--no-store-env` to launch the bridge the way a client configured with a bare
`folio mcp` really does.

## Looking at the app

```bash
powershell -File tools/screenshot.ps1 -Process folio -Out shot.png
powershell -File tools/sendkeys.ps1  -Process folio -Keys "^r"
```

The screenshot script uses `PrintWindow` with `PW_RENDERFULLCONTENT`, so it
works when the window is occluded or the session has no interactive desktop —
the usual case when this runs from an agent. It marks a frame `[BLANK]` when
nothing rendered, which is the failure you actually care about. It also sets
process DPI awareness: without that, every coordinate and pixel is virtualised
and the numbers quietly stop meaning anything.

`sendkeys.ps1` needs a real interactive desktop. Where that is not available,
the app's remembered state is the way in: `ui.view`, `ui.lastDoc`, `ui.drawer`
and `ui.theme` live in the store's `meta` table, and the app restores them at
launch.

```bash
python - <<'PY'
import sqlite3
c = sqlite3.connect('.demo/store/store.db')
c.execute("INSERT INTO meta(key,value) VALUES('ui.view','review') "
          "ON CONFLICT(key) DO UPDATE SET value=excluded.value")
c.commit()
PY
```

## Other tools

```bash
powershell -File tools/coldstart.ps1   # launch-to-window latency, sampled
python tools/make_icons.py             # regenerate the app icons from code
```

## Packaging

`tauri.conf.json` builds the native formats for the current platform (app/DMG
on macOS and DEB/RPM/AppImage on Linux). Windows overrides this to a per-user
NSIS installer. The Tauri CLI is not a workspace dependency; install it when
you need a bundle:

```bash
cargo install tauri-cli --version "^2"
cargo tauri build            # from crates/folio-app
```

## Conventions worth keeping

* **Everything goes through `dispatch`.** If the UI needs an operation the MCP
  surface should not have, add it to `dispatch` and leave it out of the tool
  list — do not add a second path into the core.
* **Never a native dialog.** `alert` / `confirm` / `prompt` show the page origin
  and centre on the screen rather than the window. `UI.alert`, `UI.confirm`,
  `UI.prompt`, `UI.note` and `UI.select` are the replacements.
* **Guard `hljs.getLanguage()` before `hljs.highlight()`.** An unknown language
  tag in one fenced block must never be able to blank a whole preview.
* **Toasts for events, dialogs for decisions.** A proposal arriving is a toast;
  accepting one is a dialog.
* **Restoring a preference must not re-persist it.** `setTheme` and `showView`
  take `{persist: false}` / `{remember: false}` for exactly this reason — the
  version that writes on restore silently overwrites the value it is restoring,
  and the bug looks like "the setting does not stick".
