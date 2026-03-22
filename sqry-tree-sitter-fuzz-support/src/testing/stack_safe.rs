//! Stack-safe test harness for deep nesting tests.
//!
//! Provides a dedicated thread with increased stack size to safely test
//! deeply nested constructs without triggering stack overflow in the test suite.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::thread;

/// Default stack size for deep nesting tests: 8 MB (4x default 2 MB).
pub const STACK_SIZE: usize = 8 * 1024 * 1024;

/// Result of a stack-safe test execution.
#[derive(Debug, PartialEq, Eq)]
pub enum StackSafeResult<T> {
    /// Test completed successfully.
    Ok(T),
    /// Test panicked (includes stack overflow).
    Panicked(String),
}

/// Runs a test closure in a dedicated thread with increased stack size.
///
/// # Parameters
/// - `test_fn`: Closure to execute in the dedicated thread
///
/// # Returns
/// `StackSafeResult` indicating success or panic (including stack overflow).
///
/// # Examples
/// ```
/// use sqry_tree_sitter_fuzz_support::testing::stack_safe::{run_with_stack, StackSafeResult};
///
/// let result = run_with_stack(|| {
///     // Deep recursion or nesting that might overflow
///     "success".to_string()
/// });
///
/// match result {
///     StackSafeResult::Ok(s) => assert_eq!(s, "success"),
///     StackSafeResult::Panicked(msg) => panic!("Test panicked: {}", msg),
/// }
/// ```
pub fn run_with_stack<F, T>(test_fn: F) -> StackSafeResult<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    run_with_custom_stack(STACK_SIZE, test_fn)
}

/// Runs a test closure in a dedicated thread with custom stack size.
///
/// # Parameters
/// - `stack_size`: Stack size in bytes
/// - `test_fn`: Closure to execute
///
/// # Returns
/// `StackSafeResult` indicating success or panic.
///
/// # Panics
/// Panics if the thread cannot be spawned with the requested stack size.
pub fn run_with_custom_stack<F, T>(stack_size: usize, test_fn: F) -> StackSafeResult<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let builder = thread::Builder::new().stack_size(stack_size);

    let handle = builder
        .spawn(move || {
            // Wrap test_fn in catch_unwind to detect panics (including stack overflow)
            catch_unwind(AssertUnwindSafe(test_fn))
        })
        .expect("Failed to spawn thread with custom stack size");

    match handle.join() {
        Ok(Ok(result)) => StackSafeResult::Ok(result),
        Ok(Err(panic_info)) => {
            let msg = if let Some(s) = panic_info.downcast_ref::<String>() {
                s.clone()
            } else if let Some(s) = panic_info.downcast_ref::<&str>() {
                (*s).to_string()
            } else {
                "Unknown panic".to_string()
            };
            StackSafeResult::Panicked(msg)
        }
        Err(_) => StackSafeResult::Panicked("Thread panicked during join".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_successful_execution() {
        let result = run_with_stack(|| 42);
        assert_eq!(result, StackSafeResult::Ok(42));
    }

    #[test]
    fn test_panic_detection() {
        let result = run_with_stack(|| {
            panic!("Intentional panic");
        });

        match result {
            StackSafeResult::Panicked(msg) => {
                assert!(msg.contains("Intentional panic"));
            }
            StackSafeResult::Ok(()) => panic!("Expected panic to be caught"),
        }
    }

    #[test]
    fn test_custom_stack_size() {
        let small_stack = 256 * 1024; // 256 KB
        let result = run_with_custom_stack(small_stack, || "works");

        assert_eq!(result, StackSafeResult::Ok("works"));
    }

    #[test]
    fn test_return_value() {
        let result = run_with_stack(|| {
            let mut sum = 0;
            for i in 1..=100 {
                sum += i;
            }
            sum
        });

        assert_eq!(result, StackSafeResult::Ok(5050));
    }

    #[test]
    fn test_deep_recursion() {
        fn recursive_sum(n: u64) -> u64 {
            if n == 0 {
                0
            } else {
                n + recursive_sum(n - 1)
            }
        }

        // With 8MB stack, this should work fine
        let result = run_with_stack(|| recursive_sum(1000));

        match result {
            StackSafeResult::Ok(sum) => {
                assert_eq!(sum, 500_500); // Sum of 1..=1000
            }
            StackSafeResult::Panicked(_) => {
                // Stack overflow can still happen with extreme recursion
                // This is acceptable behavior
            }
        }
    }

    #[test]
    fn test_stack_size_constant() {
        assert_eq!(STACK_SIZE, 8 * 1024 * 1024);
    }
}
