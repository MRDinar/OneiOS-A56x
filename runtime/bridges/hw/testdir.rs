// SPDX-License-Identifier: MIT
//! A folder for one test, removed when it ends (the C tests' mkdtemp and `rm -rf`), and the lock the tests that
//! change the process's environment take.

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, PoisonError};

pub(crate) struct TempDir(PathBuf);

impl TempDir {
    pub(crate) fn new(name: &str) -> TempDir {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("oneios-{name}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("a test folder");
        TempDir(path)
    }

    /// `rel`'s path as a string.
    pub(crate) fn at(&self, rel: &str) -> String {
        self.0.join(rel).to_string_lossy().into_owned()
    }

    /// Writes `rel` (its folders made), returns its path.
    pub(crate) fn put(&self, rel: &str, bytes: impl AsRef<[u8]>) -> String {
        let path = self.0.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("a test folder");
        }
        std::fs::write(&path, bytes).expect("a test file");
        path.to_string_lossy().into_owned()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Held by a test that sets or reads environment variables the code under test reads.
pub(crate) fn env_lock() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}
