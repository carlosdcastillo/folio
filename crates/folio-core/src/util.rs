//! Small shared helpers: time, hex, ids, path normalisation.

use crate::error::Result;
use sha2::{Digest, Sha256};
use std::path::{Component, Path, PathBuf};

/// Milliseconds since the Unix epoch. The store keeps every timestamp as an
/// integer and formats only at the edges, so ordering never depends on a
/// string comparison.
pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// RFC 3339 in UTC, the format every record in the spec's appendices uses.
pub fn ms_to_rfc3339(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .unwrap_or_else(chrono::Utc::now)
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

pub fn now_rfc3339() -> String {
    ms_to_rfc3339(now_ms())
}

pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(DIGITS[(b >> 4) as usize] as char);
        out.push(DIGITS[(b & 0x0f) as usize] as char);
    }
    out
}

/// Content address. Blobs are keyed by this and nothing else, which is what
/// makes "save twice with no change" free and makes two watchers harmless.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex(&hasher.finalize())
}

/// A short random suffix. Ids read like the spec's records (`prop_8f3a`), so
/// they stay quotable in a review note; uniqueness is enforced by the store,
/// which widens the suffix if a short one is already taken.
pub fn random_suffix(nybbles: usize) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(nybbles);
    let mut bits: u64 = 0;
    let mut have = 0usize;
    for _ in 0..nybbles {
        if have == 0 {
            bits = rand::random::<u64>();
            have = 16;
        }
        out.push(DIGITS[(bits & 0x0f) as usize] as char);
        bits >>= 4;
        have -= 1;
    }
    out
}

/// Normalise a path lexically: no `.`, no `..`. Deliberately lexical — it must
/// work for paths that do not exist yet (a `propose_edit` that creates a file).
pub fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Canonicalise for storage: absolute, lexically normalised, the Windows
/// verbatim prefix stripped, separators normalised to `/` so a path is one
/// string everywhere — in the store, on the wire, and in the UI. The native
/// separator is restored only when we touch the filesystem, via [`to_fs_path`].
pub fn canonical_key(path: &Path) -> String {
    let resolved = std::fs::canonicalize(path).unwrap_or_else(|_| {
        // The file may not exist yet: canonicalise the deepest existing
        // ancestor so symlinked parents are still resolved, then re-attach
        // the remainder.
        let normalized = lexical_normalize(path);
        let mut prefix = normalized.as_path();
        let mut tail: Vec<std::ffi::OsString> = Vec::new();
        loop {
            if let Ok(real) = std::fs::canonicalize(prefix) {
                let mut out = real;
                for part in tail.iter().rev() {
                    out.push(part);
                }
                return out;
            }
            match (prefix.file_name(), prefix.parent()) {
                (Some(name), Some(parent)) => {
                    tail.push(name.to_os_string());
                    prefix = parent;
                }
                _ => return normalized,
            }
        }
    });
    let mut s = resolved.to_string_lossy().to_string();
    // Strip the Windows verbatim prefix that `canonicalize` prepends; it is
    // correct but it is not what a user or an agent ever types.
    if let Some(stripped) = s.strip_prefix(r"\\?\") {
        s = stripped.to_string();
    }
    s.replace('\\', "/")
}

/// The inverse of [`canonical_key`] for filesystem calls.
pub fn to_fs_path(key: &str) -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(key.replace('/', "\\"))
    } else {
        PathBuf::from(key)
    }
}

/// Windows paths are case-insensitive; the store must not treat
/// `C:/Users/...` and `c:/users/...` as two different documents.
pub fn path_eq(a: &str, b: &str) -> bool {
    if cfg!(windows) {
        a.eq_ignore_ascii_case(b)
    } else {
        a == b
    }
}

/// Is `path` inside `root` (or equal to it)? Both must already be canonical keys.
pub fn is_under(root: &str, path: &str) -> bool {
    if path_eq(root, path) {
        return true;
    }
    let root_slash = if root.ends_with('/') { root.to_string() } else { format!("{root}/") };
    if path.len() <= root_slash.len() {
        return false;
    }
    path_eq(&path[..root_slash.len()], &root_slash)
}

/// `~/foo` for display, matching the paths the spec's records show.
pub fn display_path(key: &str) -> String {
    if let Some(home) = dirs::home_dir() {
        let home_key = canonical_key(&home);
        if is_under(&home_key, key) && key.len() > home_key.len() {
            let rest = &key[home_key.len()..];
            return format!("~{rest}");
        }
    }
    key.to_string()
}

/// Expand a leading `~` so users and agents can pass the paths they actually
/// think in.
pub fn expand_tilde(input: &str) -> PathBuf {
    let rest = input.strip_prefix("~/").or_else(|| input.strip_prefix("~\\"));
    if let Some(rest) = rest {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    if input == "~" {
        if let Some(home) = dirs::home_dir() {
            return home;
        }
    }
    PathBuf::from(input)
}

/// Write a file so a crash can never leave a half-written document on disk:
/// write a sibling temp file, fsync, then rename over the target.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut tmp_name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "folio".to_string());
    tmp_name.push_str(&format!(".folio-tmp-{}", random_suffix(8)));
    let tmp = path.with_file_name(tmp_name);
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    // Windows rename fails if the destination exists, so remove first. The
    // temp file is deliberately a sibling: same volume means the rename is a
    // metadata operation, not a copy.
    if path.exists() {
        let _ = std::fs::remove_file(path);
    }
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e.into())
        }
    }
}

/// Detect the newline convention of an existing document so edits preserve it.
pub fn dominant_newline(text: &str) -> &'static str {
    let crlf = text.matches("\r\n").count();
    let lf = text.matches('\n').count().saturating_sub(crlf);
    if crlf > lf {
        "\r\n"
    } else {
        "\n"
    }
}

/// Split into lines, keeping the information needed to round-trip exactly:
/// the trailing-newline flag is returned separately.
pub fn split_lines(text: &str) -> (Vec<String>, bool) {
    let normalized = text.replace("\r\n", "\n");
    if normalized.is_empty() {
        return (Vec::new(), false);
    }
    let ends_with_newline = normalized.ends_with('\n');
    let body = if ends_with_newline {
        &normalized[..normalized.len() - 1]
    } else {
        &normalized[..]
    };
    (body.split('\n').map(|s| s.to_string()).collect(), ends_with_newline)
}

pub fn join_lines(lines: &[String], trailing_newline: bool, newline: &str) -> String {
    let mut out = lines.join(newline);
    if trailing_newline {
        out.push_str(newline);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexical_normalize_eats_traversal() {
        assert_eq!(lexical_normalize(Path::new("a/b/../c")), PathBuf::from("a/c"));
        assert_eq!(lexical_normalize(Path::new("./a/./b")), PathBuf::from("a/b"));
    }

    #[test]
    fn is_under_requires_a_component_boundary() {
        assert!(is_under("C:/roots/skills", "C:/roots/skills/a/SKILL.md"));
        assert!(is_under("C:/roots/skills", "C:/roots/skills"));
        // The classic prefix bug: `skills-backup` is not inside `skills`.
        assert!(!is_under("C:/roots/skills", "C:/roots/skills-backup/x.md"));
    }

    #[test]
    fn lines_round_trip() {
        for text in ["a\nb\n", "a\nb", "", "\n"] {
            let (lines, trailing) = split_lines(text);
            assert_eq!(join_lines(&lines, trailing, "\n"), text.replace("\r\n", "\n"));
        }
    }

    #[test]
    fn random_suffix_has_the_requested_width() {
        assert_eq!(random_suffix(4).len(), 4);
        assert_eq!(random_suffix(20).len(), 20);
    }
}
