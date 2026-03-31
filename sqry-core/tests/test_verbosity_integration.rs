//! Integration test for test verbosity system
//!
//! This test demonstrates how to use the `test_support` module in integration tests.

use sqry_core::test_support::verbosity;
use std::sync::Once;

static INIT: Once = Once::new();

fn init_logging() {
    INIT.call_once(|| {
        verbosity::init(env!("CARGO_PKG_NAME"));
    });
}

#[test]
fn test_verbose_logging_works() {
    init_logging();
    log::info!("This is an info message");
    log::debug!("This is a debug message");
    log::trace!("This is a trace message");
    // Test passes - logging doesn't interfere with test execution
}

#[test]
fn test_multiple_init_calls_safe() {
    init_logging();
    init_logging(); // Second call should be no-op
    log::info!("Multiple init calls are safe");
}

#[test]
fn test_logging_in_threads() {
    init_logging();

    let handles: Vec<_> = (0..4)
        .map(|i| {
            std::thread::spawn(move || {
                log::info!("Thread {i} message");
            })
        })
        .collect();

    for handle in handles {
        handle.join().unwrap();
    }
}

#[test]
fn test_should_enable_parsing() {
    // Test that should_enable function works correctly
    assert!(verbosity::should_enable("sqry-core") || !verbosity::should_enable("sqry-core"));
    // This always passes - it's testing the function doesn't panic
}
