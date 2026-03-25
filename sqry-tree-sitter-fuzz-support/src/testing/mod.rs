//! Testing utilities for malformed input tests.
//!
//! Provides infrastructure for safe and reliable malformed input testing:
//! - Stack-safe test harness for deep nesting
//! - Error assertion helpers for graceful failure verification

pub mod assertions;
pub mod stack_safe;

pub use assertions::{assert_is_error, assert_is_ok, assert_no_panic, GracefulFailure};
pub use stack_safe::{run_with_custom_stack, run_with_stack, StackSafeResult, STACK_SIZE};
