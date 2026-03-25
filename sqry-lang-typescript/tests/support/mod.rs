use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

/// Generate a unique in-memory file path for tests to prevent key collisions.
pub fn unique_ts_path(prefix: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    PathBuf::from(format!("{prefix}_{id}.ts"))
}
