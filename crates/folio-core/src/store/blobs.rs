//! Content-addressed blob store: `blobs/ab/cd/<sha256>`.
//!
//! Every version of every tracked file lands here exactly once. Two documents
//! with identical content share one blob, and re-saving a file with no change
//! writes nothing at all — which is what lets the version store sit under a
//! whole corpus without growing without bound.

use crate::error::{Error, Result};
use crate::util::{atomic_write, sha256_hex};
use std::path::{Path, PathBuf};

pub struct BlobStore {
    root: PathBuf,
}

impl BlobStore {
    pub fn new(root: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&root)?;
        Ok(BlobStore { root })
    }

    fn path_for(&self, hash: &str) -> PathBuf {
        // Two levels of fan-out keeps any single directory well under the
        // point where Windows directory enumeration slows down.
        self.root.join(&hash[0..2]).join(&hash[2..4]).join(hash)
    }

    pub fn contains(&self, hash: &str) -> bool {
        hash.len() >= 4 && self.path_for(hash).exists()
    }

    /// Store `bytes`, returning its content address. Writing a blob that is
    /// already present is a no-op, so this is safe to call unconditionally.
    pub fn put(&self, bytes: &[u8]) -> Result<String> {
        let hash = sha256_hex(bytes);
        let path = self.path_for(&hash);
        if path.exists() {
            return Ok(hash);
        }
        // The blob is written *before* the index row that references it, so a
        // hard kill can leave an unreferenced blob (harmless, collectable) but
        // never an index row pointing at content that does not exist.
        atomic_write(&path, bytes)?;
        Ok(hash)
    }

    pub fn get(&self, hash: &str) -> Result<Vec<u8>> {
        let path = self.path_for(hash);
        std::fs::read(&path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                Error::NotFound(format!("blob {hash}"))
            } else {
                Error::Io(e)
            }
        })
    }

    /// Text accessor for the many callers that only ever hold markdown.
    /// Invalid UTF-8 is replaced rather than refused: a tracked asset should
    /// never make the timeline unreadable.
    pub fn get_text(&self, hash: &str) -> Result<String> {
        Ok(String::from_utf8_lossy(&self.get(hash)?).into_owned())
    }

    /// Total bytes on disk, for the store-size readout in the status bar.
    pub fn total_size(&self) -> u64 {
        walkdir::WalkDir::new(&self.root)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .filter_map(|e| e.metadata().ok())
            .map(|m| m.len())
            .sum()
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_content_stores_once() {
        let dir = tempfile::tempdir().unwrap();
        let blobs = BlobStore::new(dir.path().join("blobs")).unwrap();
        let a = blobs.put(b"# Skill\n").unwrap();
        let b = blobs.put(b"# Skill\n").unwrap();
        assert_eq!(a, b);
        assert_eq!(blobs.get(&a).unwrap(), b"# Skill\n");
        assert!(blobs.contains(&a));
    }

    #[test]
    fn missing_blob_is_not_found_not_io() {
        let dir = tempfile::tempdir().unwrap();
        let blobs = BlobStore::new(dir.path().join("blobs")).unwrap();
        let err = blobs.get(&"0".repeat(64)).unwrap_err();
        assert_eq!(err.code(), "not_found");
    }
}
