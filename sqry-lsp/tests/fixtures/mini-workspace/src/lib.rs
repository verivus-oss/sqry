//! Fixture Rust module for LSP integration tests.
//! Contains nested symbols, doc comments, and emoji identifiers.

mod extra;

mod internal {
    /// Inner state used by `process_data` for hover tests.
    pub struct InnerState {
        pub value: i32,
    }

    impl InnerState {
        pub fn new(value: i32) -> Self {
            Self { value }
        }
    }

    pub fn helper(value: i32) -> String {
        format!("value={value}")
    }
}

use internal::{helper, InnerState};

/// Processes input and returns a formatted string.
/// Used for hover + definition tests at line ~22.
pub async fn process_data(input: i32) -> String {
    let state = InnerState::new(input);
    let details = helper(state.value);
    format!("processed:{details}")
}

/// Primary entry point used for call hierarchy tests.
pub fn orchestrate(value: i32) -> String {
    let first = summarize(value);
    let second = summarize(value + 1);
    format!("{first}:{second}")
}

/// Secondary caller to ensure multiple incoming edges.
pub fn summarize(value: i32) -> String {
    format_state(value / 2)
}

/// Additional public caller to exercise pagination.
pub fn alternate(value: i32) -> String {
    summarize(value + 10)
}

/// Cross-file caller exercising relations into `extra.rs`.
pub fn use_extra_helper(value: i32) -> String {
    extra::helper(value)
}

pub fn multi_call_line(value: i32) -> String {
    format_state(value) + &format_state(value + 1)
}

fn format_state(value: i32) -> String {
    let state = InnerState::new(value);
    helper(state.value)
}

pub fn recursive(value: i32) -> i32 {
    if value <= 0 {
        0
    } else {
        recursive(value - 1)
    }
}

/// Function with emoji identifier to validate UTF-16 handling.
pub fn emoji_fn() -> &'static str {
    let 🚀 = "rocket"; // reference for textDocument/hover emoji position
    🚀
}

pub fn rocket_launcher() -> &'static str {
    helper_🚀()
}

fn helper_🚀() -> &'static str {
    "emoji-call"
}

fn unused_helper() {
    // Placeholder to ensure document symbol hierarchy includes a private item.
}

pub fn lonely_function() {
    // Intentionally unused; verifies empty incoming call hierarchies.
    let _ = InnerState::new(10);
    let _ = rocket_launcher();
}
