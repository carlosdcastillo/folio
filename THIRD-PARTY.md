# Third-party components

Folio is MIT licensed (see [LICENSE](LICENSE)). It vendors a small number of
browser libraries under `ui/lib/`, and links a set of Rust crates. All are
permissively licensed and compatible with MIT; none are copyleft.

## Vendored in this repository (`ui/lib/`)

These ship as files in the tree, so their notices travel with the source.

| Component | License | Used for |
|---|---|---|
| [marked](https://github.com/markedjs/marked) | MIT | Markdown rendering in the preview pane |
| [DOMPurify](https://github.com/cure53/DOMPurify) | Apache-2.0 **or** MPL-2.0 | Sanitising rendered HTML before it reaches the DOM |
| [highlight.js](https://github.com/highlightjs/highlight.js) | BSD-3-Clause | Syntax highlighting in fenced code blocks |
| [KaTeX](https://github.com/KaTeX/KaTeX) (incl. fonts) | MIT | Math rendering |
| [CodeMirror 6](https://codemirror.net/) / [Lezer](https://lezer.codemirror.net/) | MIT | The editor. Bundled into `ui/lib/codemirror.bundle.js` by `ui/cm-entry.js` |

`ui/lib/codemirror.bundle.js` is generated — see `ui/package.json` for the
exact packages and `npm run build` for how it is produced.

## Rust dependencies

Resolved from `Cargo.lock`; all MIT, Apache-2.0, or dual MIT/Apache-2.0. The
significant ones:

| Crate | Purpose |
|---|---|
| `tauri` | The desktop shell (WebView2 on Windows) |
| `rmcp` | The Model Context Protocol server |
| `rusqlite` (bundled SQLite) | The store. SQLite itself is public domain |
| `similar` | The diff engine underneath the three-level prose diff |
| `comrak` | GFM parsing for artifact typing and linting |
| `notify` / `notify-debouncer-full` | Filesystem watching |
| `serde` / `serde_json` / `serde_yaml_ng` | Serialisation, and frontmatter |
| `globset`, `regex`, `walkdir` | Path matching and corpus search |
| `sha2` | Content addressing for the blob store |
| `chrono`, `dirs`, `rand`, `thiserror` | Time, platform paths, ids, errors |

To regenerate a complete, exact list:

```bash
cargo install cargo-license
cargo license --avoid-build-deps
```
