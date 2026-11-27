//! Fixtures of the unit tests.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// A new directory under the temporary directory of the system. The drop
/// removes the directory and its content.
pub(crate) struct TempDir(PathBuf);

impl TempDir {
    pub(crate) fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let mut bytes = [0_u8; 8];
        getrandom::fill(&mut bytes).unwrap();
        let name = format!(
            "telemetry-web-server-{}-{}-{:016x}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
            u64::from_le_bytes(bytes)
        );
        let path = std::env::temp_dir().join(name);
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }

    /// Writes `bytes` to the relative `path` and creates its parent
    /// directories.
    pub(crate) fn write(&self, path: &str, bytes: &[u8]) {
        let path = self.0.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        // A failed clean-up leaves a directory in the temporary directory;
        // it does not change a test result.
        std::fs::remove_dir_all(&self.0).ok();
    }
}
