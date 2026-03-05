// Test fixture: External crate declarations with aliases
// Tests: serde, tokio (using extern crate syntax)

// Note: Modern Rust (2018+) doesn't require extern crate for most cases,
// but this demonstrates the syntax for crates that need it or legacy code

extern crate alloc;

use alloc::string::String as AllocString;
use alloc::vec::Vec as AllocVec;

fn use_alloc_types() {
    let s: AllocString = AllocString::from("Hello");
    let mut v: AllocVec<i32> = AllocVec::new();
    v.push(1);
    v.push(2);

    println!("String: {}, Vec len: {}", s, v.len());
}

// Demonstrating re-exports
mod external {
    pub use std::collections::HashMap as ExtMap;
    pub use std::sync::Arc as ExtArc;
}

fn use_reexports() {
    let map = external::ExtMap::new();
    let arc = external::ExtArc::new(42);
    println!("Created external types");
}

fn main() {
    use_alloc_types();
    use_reexports();
}
