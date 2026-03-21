/// Generates unique Swift file paths for each test to avoid collisions
pub fn unique_swift_path(prefix: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("/test/swift/{prefix}_{id}.swift")
}
