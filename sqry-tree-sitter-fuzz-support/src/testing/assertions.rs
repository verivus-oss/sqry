//! Error assertion helpers for malformed input tests.
//!
//! Provides utilities to assert that operations on malformed input
//! gracefully return errors instead of panicking.

use std::fmt::Debug;

/// Asserts that a Result is an Err variant, without caring about the specific error.
///
/// # Parameters
/// - `result`: Result to check
///
/// # Panics
/// Panics if the result is Ok.
///
/// # Examples
/// ```
/// use sqry_tree_sitter_fuzz_support::testing::assertions::assert_is_error;
///
/// let result: Result<(), &str> = Err("parse failed");
/// assert_is_error(result); // Passes
///
/// // This would panic:
/// // let ok_result: Result<i32, &str> = Ok(42);
/// // assert_is_error(ok_result);
/// ```
pub fn assert_is_error<T: Debug, E>(result: Result<T, E>) {
    if let Ok(value) = result {
        panic!("Expected Err, but got Ok({value:?}). Test should have failed on malformed input.");
    }
    // If it's Err, test passes
}

/// Asserts that a Result is Ok, used to verify valid inputs still work.
///
/// # Parameters
/// - `result`: Result to check
///
/// # Panics
/// Panics if the result is Err.
pub fn assert_is_ok<T, E: Debug>(result: Result<T, E>) {
    if let Err(err) = result {
        panic!("Expected Ok, but got Err({err:?})");
    }
}

/// Asserts that executing a closure does not panic.
///
/// # Parameters
/// - `f`: Closure to execute
///
/// # Returns
/// The value returned by the closure.
///
/// # Panics
/// Panics if the closure panics.
pub fn assert_no_panic<F, T>(f: F) -> T
where
    F: FnOnce() -> T,
{
    f()
}

/// Trait for results that can be checked for graceful failure.
pub trait GracefulFailure {
    /// Returns true if this represents a graceful error (not a panic).
    fn is_graceful_failure(&self) -> bool;
}

impl<T, E> GracefulFailure for Result<T, E> {
    fn is_graceful_failure(&self) -> bool {
        self.is_err()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_assert_is_error_with_err() {
        let result: Result<(), &str> = Err("error");
        assert_is_error(result); // Should not panic
    }

    #[test]
    #[should_panic(expected = "Expected Err, but got Ok")]
    fn test_assert_is_error_with_ok() {
        let result: Result<i32, &str> = Ok(42);
        assert_is_error(result); // Should panic
    }

    #[test]
    fn test_assert_is_ok_with_ok() {
        let result: Result<i32, &str> = Ok(42);
        assert_is_ok(result); // Should not panic
    }

    #[test]
    #[should_panic(expected = "Expected Ok, but got Err")]
    fn test_assert_is_ok_with_err() {
        let result: Result<(), &str> = Err("error");
        assert_is_ok(result); // Should panic
    }

    #[test]
    fn test_assert_no_panic() {
        let value = assert_no_panic(|| 42);
        assert_eq!(value, 42);
    }

    #[test]
    #[should_panic(expected = "Intentional panic")]
    fn test_assert_no_panic_with_panic() {
        assert_no_panic(|| panic!("Intentional panic"));
    }

    #[test]
    fn test_graceful_failure_trait() {
        let err_result: Result<(), &str> = Err("error");
        assert!(err_result.is_graceful_failure());

        let ok_result: Result<i32, &str> = Ok(42);
        assert!(!ok_result.is_graceful_failure());
    }
}
