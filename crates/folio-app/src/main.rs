//! One binary, two personalities.
//!
//! `folio` launches the desktop app. `folio mcp` runs the stdio MCP bridge
//! that MCP clients spawn per session. Both are shells over `folio-core`.

// The GUI must not open a console window behind it on Windows. `folio mcp`
// still works from a terminal: it inherits the parent's stdio.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod bridge;
mod gui;
mod ipc;

const HELP: &str = "\
Folio — your agents' markdown, under your control.

USAGE:
    folio                    Launch the desktop app
    folio mcp                Run the stdio MCP server (what MCP clients spawn)

OPTIONS:
    --store <path>           Use a different store directory
                             (default: %LOCALAPPDATA%\\Folio, or $FOLIO_STORE)
    --author <name>          Default author recorded for MCP writes
    -V, --version            Print the version
    -h, --help               Print this help

MCP CLIENT CONFIGURATION:
    {\"mcpServers\": {\"folio\": {\"command\": \"folio\", \"args\": [\"mcp\"]}}}
";

fn main() {
    let mut args = std::env::args().skip(1).peekable();
    let mut command: Option<String> = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--store" => match args.next() {
                Some(path) => std::env::set_var("FOLIO_STORE", path),
                None => {
                    eprintln!("folio: --store needs a path");
                    std::process::exit(2);
                }
            },
            "--author" => match args.next() {
                Some(author) => std::env::set_var("FOLIO_AUTHOR", author),
                None => {
                    eprintln!("folio: --author needs a name");
                    std::process::exit(2);
                }
            },
            "-h" | "--help" => {
                println!("{HELP}");
                return;
            }
            "-V" | "--version" => {
                println!("folio {}", folio_core::VERSION);
                return;
            }
            other if command.is_none() && !other.starts_with('-') => {
                command = Some(other.to_string());
            }
            other => {
                eprintln!("folio: unknown argument `{other}`\n\n{HELP}");
                std::process::exit(2);
            }
        }
    }

    let code = match command.as_deref() {
        Some("mcp") => bridge::run(),
        None => gui::run(),
        Some(other) => {
            eprintln!("folio: unknown command `{other}`\n\n{HELP}");
            2
        }
    };
    std::process::exit(code);
}
