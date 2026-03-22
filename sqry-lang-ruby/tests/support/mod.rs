use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

/// Generate a unique in-memory Ruby file path for tests.
pub fn unique_rb_path(prefix: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    PathBuf::from(format!("{prefix}_{id}.rb"))
}
