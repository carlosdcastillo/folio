//! The file watcher.
//!
//! Every registered root is watched. A change anyone makes — you in another
//! editor, an agent through a shell, a script — becomes a snapshot within a
//! second. Content-hash dedup in the version store means this is safe to run
//! in the GUI and in a headless bridge at the same time: two watchers seeing
//! the same write cannot produce two history entries.

use crate::api::Folio;
use crate::corpus::{self, Resolved};
use crate::error::Result;
use crate::event::Event;
use crate::util::canonical_key;
use crate::version::{self, Source};
use notify::RecursiveMode;
use notify_debouncer_full::{new_debouncer, DebounceEventResult, Debouncer, RecommendedCache};
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::{Arc, Weak};
use std::time::Duration;

pub struct Watcher {
    // Dropping the debouncer stops its thread.
    _debouncer: Debouncer<notify::RecommendedWatcher, RecommendedCache>,
    watching: usize,
}

impl Watcher {
    pub fn start(folio: Arc<Folio>) -> Result<Watcher> {
        // A weak handle, deliberately: `Folio` owns the watcher, and an `Arc`
        // here would make the pair immortal.
        let weak: Weak<Folio> = Arc::downgrade(&folio);

        let debouncer = new_debouncer(
            Duration::from_millis(folio.config.watch_debounce_ms),
            None,
            move |result: DebounceEventResult| {
                let Some(folio) = weak.upgrade() else { return };
                match result {
                    Ok(events) => {
                        let mut touched: BTreeSet<String> = BTreeSet::new();
                        for event in events {
                            for path in &event.paths {
                                let name = path
                                    .file_name()
                                    .map(|n| n.to_string_lossy().to_string())
                                    .unwrap_or_default();
                                // Our own atomic-write temp files are not news.
                                if name.contains(".folio-tmp-") {
                                    continue;
                                }
                                if path.is_dir() {
                                    continue;
                                }
                                touched.insert(canonical_key(path));
                            }
                        }
                        for key in touched {
                            handle_change(&folio, &key);
                        }
                    }
                    Err(errors) => {
                        let message = errors
                            .iter()
                            .map(|e| e.to_string())
                            .collect::<Vec<_>>()
                            .join("; ");
                        folio.bus.emit(Event::WatcherStatus {
                            watching: folio.is_watching(),
                            healthy: false,
                            message: Some(message),
                        });
                    }
                }
            },
        )
        .map_err(|e| crate::error::Error::other(format!("watcher: {e}")))?;

        let mut watcher = Watcher {
            _debouncer: debouncer,
            watching: 0,
        };
        for root in folio.roots()? {
            // A single unreadable root must not stop the others being watched.
            match watcher.watch_root(&root) {
                Ok(()) => {}
                Err(e) => folio.bus.emit(Event::WatcherStatus {
                    watching: watcher.watching,
                    healthy: false,
                    message: Some(format!("cannot watch {}: {e}", root.display)),
                }),
            }
        }

        Ok(watcher)
    }

    pub fn watching(&self) -> usize {
        self.watching
    }

    pub fn watch_root(&mut self, root: &corpus::Root) -> Result<()> {
        let path = crate::util::to_fs_path(&root.path);
        let mode = match root.kind {
            corpus::RootKind::Dir => RecursiveMode::Recursive,
            corpus::RootKind::File => RecursiveMode::NonRecursive,
        };
        self._debouncer
            .watch(&path, mode)
            .map_err(|e| crate::error::Error::other(format!("watcher: {e}")))?;
        self.watching += 1;
        Ok(())
    }

    pub fn unwatch_root(&mut self, path: &Path) -> Result<()> {
        self._debouncer
            .unwatch(path)
            .map_err(|e| crate::error::Error::other(format!("watcher: {e}")))?;
        self.watching = self.watching.saturating_sub(1);
        Ok(())
    }
}

fn handle_change(folio: &Arc<Folio>, key: &str) {
    let Ok(roots) = folio.roots() else { return };
    // Sandboxing applies to the watcher too: an event for a path that is not
    // in the corpus is dropped, not recorded.
    let Ok(resolved) = corpus::resolve_within(&roots, key) else { return };

    // Recursive roots are Markdown corpora. Ignore unrelated files without
    // reading or hashing them; a non-Markdown file root remains explicitly
    // trackable as an asset.
    if resolved.root.kind == corpus::RootKind::Dir
        && !corpus::is_markdown_path(&resolved.path)
    {
        return;
    }

    if !resolved.fs_path.exists() {
        folio.bus.emit(Event::DocRemoved {
            display: resolved.display(),
            path: resolved.path.clone(),
        });
        return;
    }

    record(folio, &resolved);
}

fn record(folio: &Arc<Folio>, resolved: &Resolved) {
    match version::record_from_disk(
        &folio.store,
        &folio.config,
        resolved,
        Source::External,
        None,
        None,
        None,
    ) {
        Ok(outcome) => {
            // Only a genuinely new version is news. A write Folio itself just
            // made hashes to the version already recorded and lands here as
            // `created: false`.
            if outcome.created {
                folio.bus.emit(Event::SnapshotCreated { snapshot: Box::new(outcome.snapshot) });
            }
        }
        Err(crate::error::Error::TooLarge(_)) => {}
        Err(e) => folio.bus.emit(Event::WatcherStatus {
            watching: folio.is_watching(),
            healthy: false,
            message: Some(format!("{}: {e}", resolved.display())),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::Caller;
    use serde_json::json;
    use std::thread;
    use std::time::Instant;

    #[test]
    fn a_root_added_while_watching_tracks_new_top_level_files() {
        let dir = tempfile::tempdir().unwrap();
        let corpus = dir.path().join("corpus");
        std::fs::create_dir(&corpus).unwrap();

        let folio = Folio::open(&dir.path().join("store")).unwrap();
        folio.start_watching().unwrap();
        assert_eq!(folio.is_watching(), 0);

        folio
            .dispatch(
                &Caller::human(),
                "add_root",
                &json!({ "path": corpus.to_string_lossy() }),
            )
            .unwrap();
        assert_eq!(folio.is_watching(), 1);

        std::fs::write(corpus.join("new.md"), "# New\n").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let docs = folio
                .dispatch(&Caller::human(), "list_docs", &json!({}))
                .unwrap();
            if docs["docs"].as_array().unwrap().len() == 1 {
                assert_eq!(docs["docs"][0]["relative"], "new.md");
                break;
            }
            assert!(
                Instant::now() < deadline,
                "new root-level file was not indexed"
            );
            thread::sleep(Duration::from_millis(20));
        }
    }
}
