//! Opt-in, local-only product instrumentation.
//!
//! Events intentionally carry no properties. Stable event names and a random
//! per-launch session id are enough to reveal which paths through the product
//! are common without collecting document paths, content, or user input.

use crate::error::{Error, Result};
use crate::store::Store;
use crate::util::now_ms;
use serde::Serialize;

const ENABLED_META_KEY: &str = "instrumentation.enabled";

#[derive(Debug, Serialize)]
pub struct UsageEvent {
    pub session: String,
    pub event: String,
    pub app_version: String,
    pub at: i64,
}

pub fn enabled(store: &Store) -> Result<bool> {
    Ok(store.get_meta(ENABLED_META_KEY)?.as_deref() == Some("true"))
}

pub fn set_enabled(store: &Store, value: bool) -> Result<()> {
    store.set_meta(ENABLED_META_KEY, if value { "true" } else { "false" })
}

pub fn record(store: &Store, session: &str, event: &str) -> Result<bool> {
    if !enabled(store)? {
        return Ok(false);
    }
    validate_token("session", session, 64)?;
    validate_token("event", event, 80)?;

    store.conn().execute(
        "INSERT INTO usage_events(session, event, app_version, at) VALUES (?1, ?2, ?3, ?4)",
        (session, event, crate::VERSION, now_ms()),
    )?;
    Ok(true)
}

pub fn list(store: &Store) -> Result<Vec<UsageEvent>> {
    let conn = store.conn();
    let mut statement =
        conn.prepare("SELECT session, event, app_version, at FROM usage_events ORDER BY at, id")?;
    let rows = statement.query_map([], |row| {
        Ok(UsageEvent {
            session: row.get(0)?,
            event: row.get(1)?,
            app_version: row.get(2)?,
            at: row.get(3)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

pub fn count(store: &Store) -> Result<i64> {
    Ok(store
        .conn()
        .query_row("SELECT COUNT(*) FROM usage_events", [], |row| row.get(0))?)
}

pub fn clear(store: &Store) -> Result<usize> {
    Ok(store.conn().execute("DELETE FROM usage_events", [])?)
}

fn validate_token(label: &str, value: &str, max_len: usize) -> Result<()> {
    let valid = !value.is_empty()
        && value.len() <= max_len
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
    if valid {
        Ok(())
    } else {
        Err(Error::invalid(format!(
            "`{label}` must be 1-{max_len} letters, numbers, dots, dashes, or underscores"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_nothing_until_enabled_and_can_be_cleared() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();

        assert!(!record(&store, "launch-1", "view.today").unwrap());
        assert_eq!(count(&store).unwrap(), 0);

        set_enabled(&store, true).unwrap();
        assert!(record(&store, "launch-1", "view.today").unwrap());
        assert!(record(&store, "launch-1", "action.save").unwrap());
        let events = list(&store).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event, "view.today");
        assert_eq!(events[1].event, "action.save");
        assert_eq!(events[0].session, "launch-1");

        assert_eq!(clear(&store).unwrap(), 2);
        assert_eq!(count(&store).unwrap(), 0);
    }

    #[test]
    fn rejects_context_disguised_as_an_event_name() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        set_enabled(&store, true).unwrap();

        let error = record(&store, "launch-1", "open./home/carlos/private.md").unwrap_err();
        assert_eq!(error.code(), "invalid");
        assert_eq!(count(&store).unwrap(), 0);
    }
}
