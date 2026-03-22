//! Recursion depth guards for preventing stack overflow
//!
//! This module provides guards that protect against stack overflow from
//! deeply nested structures (AST trees, expressions, etc.).
//!
//! # Guards
//!
//! - [`RecursionGuard`]: Depth-based guard for AST traversal
//! - [`ExprFuelCounter`]: Fuel-based guard for expression evaluation
//!
//! # Usage
//!
//! ```no_run
//! use sqry_core::config::RecursionLimits;
//! use sqry_core::query::security::RecursionGuard;
//!
//! # struct Node;
//! # impl Node { fn children(&self) -> Vec<&Node> { vec![] } }
//! # type RecursionError = Box<dyn std::error::Error>;
//! fn walk_tree(node: &Node, guard: &mut RecursionGuard) -> Result<(), RecursionError> {
//!     guard.enter()?;
//!     // Process node...
//!     for child in node.children() {
//!         walk_tree(child, guard)?;
//!     }
//!     guard.exit();
//!     Ok(())
//! }
//! ```

use anyhow::{Result, bail};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Recursion depth guard for AST traversal and file operations
///
/// Tracks the current recursion depth and enforces a maximum limit to
/// prevent stack overflow from pathological inputs like deeply nested
/// function definitions.
///
/// # Thread Safety
///
/// `RecursionGuard` is NOT thread-safe and should not be shared between threads.
/// Each thread should have its own guard instance.
///
/// # Example
///
/// ```
/// use sqry_core::query::security::RecursionGuard;
///
/// fn process_node(node: &str, guard: &mut RecursionGuard) -> Result<(), Box<dyn std::error::Error>> {
///     guard.enter()?;
///     // Process the node...
///     guard.exit();
///     Ok(())
/// }
///
/// let mut guard = RecursionGuard::new(100)?;
/// process_node("example", &mut guard)?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug)]
pub struct RecursionGuard {
    max_depth: usize,
    current_depth: usize,
    max_depth_reached: usize,
}

impl RecursionGuard {
    /// Create a new recursion guard with the specified maximum depth
    ///
    /// # Errors
    ///
    /// Returns an error if `max_depth` is 0.
    pub fn new(max_depth: usize) -> Result<Self> {
        if max_depth == 0 {
            bail!("RecursionGuard max_depth cannot be 0");
        }

        Ok(Self {
            max_depth,
            current_depth: 0,
            max_depth_reached: 0,
        })
    }

    /// Enter a new recursion level
    ///
    /// This must be called at the beginning of each recursive function.
    /// Must be paired with a corresponding [`exit`](Self::exit) call.
    ///
    /// # Errors
    ///
    /// Returns [`RecursionError::DepthLimitExceeded`] if entering would exceed the max depth.
    pub fn enter(&mut self) -> Result<(), RecursionError> {
        self.current_depth += 1;

        // Track maximum depth reached for telemetry
        if self.current_depth > self.max_depth_reached {
            self.max_depth_reached = self.current_depth;
        }

        if self.current_depth > self.max_depth {
            return Err(RecursionError::DepthLimitExceeded {
                current: self.current_depth,
                limit: self.max_depth,
            });
        }

        Ok(())
    }

    /// Exit the current recursion level
    ///
    /// This must be called when leaving a recursive function, typically
    /// in a `defer`-like pattern or before returning.
    pub fn exit(&mut self) {
        if self.current_depth > 0 {
            self.current_depth -= 1;
        }
    }

    /// Get the current recursion depth
    #[must_use]
    pub fn current_depth(&self) -> usize {
        self.current_depth
    }

    /// Get the maximum depth reached during execution
    ///
    /// Useful for telemetry and understanding actual depth requirements.
    #[must_use]
    pub fn max_depth_reached(&self) -> usize {
        self.max_depth_reached
    }

    /// Get the configured maximum depth limit
    #[must_use]
    pub fn max_depth(&self) -> usize {
        self.max_depth
    }
}

/// Expression fuel counter for limiting expression evaluation complexity
///
/// Uses a fuel-based approach where each operation consumes fuel.
/// This prevents both deep recursion (many nested calls) and wide
/// recursion (many sibling calls).
///
/// # Thread Safety
///
/// `ExprFuelCounter` uses atomic operations and is safe to share between threads.
///
/// # Example
///
/// ```
/// use sqry_core::query::security::ExprFuelCounter;
///
/// fn evaluate_expr(expr: &str, fuel: &ExprFuelCounter) -> Result<(), Box<dyn std::error::Error>> {
///     fuel.consume(1)?;
///     // Evaluate expression...
///     Ok(())
/// }
///
/// let fuel = ExprFuelCounter::new(1000)?;
/// evaluate_expr("a AND b", &fuel)?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug)]
pub struct ExprFuelCounter {
    fuel: AtomicUsize,
    initial_fuel: usize,
}

impl ExprFuelCounter {
    /// Create a new fuel counter with the specified initial fuel
    ///
    /// # Errors
    ///
    /// Returns an error if `initial_fuel` is 0.
    pub fn new(initial_fuel: usize) -> Result<Self> {
        if initial_fuel == 0 {
            bail!("ExprFuelCounter initial_fuel cannot be 0");
        }

        Ok(Self {
            fuel: AtomicUsize::new(initial_fuel),
            initial_fuel,
        })
    }

    /// Consume the specified amount of fuel
    ///
    /// # Errors
    ///
    /// Returns [`RecursionError::FuelExhausted`] if there is not enough fuel remaining.
    ///
    /// # Implementation Note
    ///
    /// Uses `fetch_update` for atomic check-then-subtract to prevent underflow
    /// (per `FINAL_CORRECTIONS.md` `ExprFuelCounter` bug fix).
    pub fn consume(&self, amount: usize) -> Result<(), RecursionError> {
        let result = self
            .fuel
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
                if current >= amount {
                    Some(current - amount)
                } else {
                    None
                }
            });

        match result {
            Ok(_previous) => Ok(()),
            Err(current) => Err(RecursionError::FuelExhausted {
                remaining: current,
                requested: amount,
            }),
        }
    }

    /// Get the current fuel remaining
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.fuel.load(Ordering::SeqCst)
    }

    /// Get the initial fuel amount
    #[must_use]
    pub fn initial_fuel(&self) -> usize {
        self.initial_fuel
    }

    /// Get the amount of fuel consumed so far
    #[must_use]
    pub fn consumed(&self) -> usize {
        self.initial_fuel.saturating_sub(self.remaining())
    }

    /// Check if there is enough fuel remaining
    #[must_use]
    pub fn has_fuel(&self, amount: usize) -> bool {
        self.remaining() >= amount
    }

    /// Reset the fuel counter to its initial value
    ///
    /// Useful for reusing a fuel counter across multiple operations.
    pub fn reset(&self) {
        self.fuel.store(self.initial_fuel, Ordering::SeqCst);
    }
}

/// Errors from recursion guards
#[derive(Debug, thiserror::Error)]
pub enum RecursionError {
    /// Recursion depth limit exceeded
    #[error("Recursion depth limit exceeded: depth {current} > limit {limit}")]
    DepthLimitExceeded {
        /// Current recursion depth
        current: usize,
        /// Maximum allowed depth
        limit: usize,
    },

    /// Expression fuel exhausted
    #[error(
        "Expression evaluation fuel exhausted: requested {requested}, only {remaining} remaining"
    )]
    FuelExhausted {
        /// Amount of fuel remaining
        remaining: usize,
        /// Amount of fuel requested
        requested: usize,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    // RecursionGuard tests
    #[test]
    fn test_guard_new() {
        let guard = RecursionGuard::new(100).unwrap();
        assert_eq!(guard.current_depth(), 0);
        assert_eq!(guard.max_depth(), 100);
        assert_eq!(guard.max_depth_reached(), 0);
    }

    #[test]
    fn test_guard_new_zero_fails() {
        let result = RecursionGuard::new(0);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("cannot be 0"));
    }

    #[test]
    fn test_guard_enter_exit() {
        let mut guard = RecursionGuard::new(10).unwrap();

        guard.enter().unwrap();
        assert_eq!(guard.current_depth(), 1);
        assert_eq!(guard.max_depth_reached(), 1);

        guard.enter().unwrap();
        assert_eq!(guard.current_depth(), 2);
        assert_eq!(guard.max_depth_reached(), 2);

        guard.exit();
        assert_eq!(guard.current_depth(), 1);
        assert_eq!(guard.max_depth_reached(), 2); // Max reached stays at 2

        guard.exit();
        assert_eq!(guard.current_depth(), 0);
    }

    #[test]
    fn test_guard_depth_limit_enforced() {
        let mut guard = RecursionGuard::new(3).unwrap();

        guard.enter().unwrap(); // depth 1
        guard.enter().unwrap(); // depth 2
        guard.enter().unwrap(); // depth 3

        let err = guard.enter().unwrap_err(); // depth 4 - should fail
        assert!(matches!(
            err,
            RecursionError::DepthLimitExceeded {
                current: 4,
                limit: 3
            }
        ));
    }

    #[test]
    fn test_guard_exit_at_zero_is_safe() {
        let mut guard = RecursionGuard::new(10).unwrap();
        guard.exit(); // Should not panic or underflow
        assert_eq!(guard.current_depth(), 0);
    }

    #[test]
    fn test_guard_max_depth_tracking() {
        let mut guard = RecursionGuard::new(100).unwrap();

        // Go to depth 5
        for _ in 0..5 {
            guard.enter().unwrap();
        }
        assert_eq!(guard.max_depth_reached(), 5);

        // Come back to depth 2
        for _ in 0..3 {
            guard.exit();
        }
        assert_eq!(guard.current_depth(), 2);
        assert_eq!(guard.max_depth_reached(), 5); // Max stays at 5

        // Go to depth 3
        guard.enter().unwrap();
        assert_eq!(guard.max_depth_reached(), 5); // Still 5, not 3
    }

    // ExprFuelCounter tests
    #[test]
    fn test_fuel_new() {
        let fuel = ExprFuelCounter::new(1000).unwrap();
        assert_eq!(fuel.remaining(), 1000);
        assert_eq!(fuel.initial_fuel(), 1000);
        assert_eq!(fuel.consumed(), 0);
    }

    #[test]
    fn test_fuel_new_zero_fails() {
        let result = ExprFuelCounter::new(0);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("cannot be 0"));
    }

    #[test]
    fn test_fuel_consume() {
        let fuel = ExprFuelCounter::new(100).unwrap();

        fuel.consume(30).unwrap();
        assert_eq!(fuel.remaining(), 70);
        assert_eq!(fuel.consumed(), 30);

        fuel.consume(40).unwrap();
        assert_eq!(fuel.remaining(), 30);
        assert_eq!(fuel.consumed(), 70);
    }

    #[test]
    fn test_fuel_exhaustion() {
        let fuel = ExprFuelCounter::new(50).unwrap();

        fuel.consume(30).unwrap();
        assert_eq!(fuel.remaining(), 20);

        let err = fuel.consume(30).unwrap_err();
        assert!(matches!(
            err,
            RecursionError::FuelExhausted {
                remaining: 20,
                requested: 30
            }
        ));

        // Fuel should remain unchanged after failed consume
        assert_eq!(fuel.remaining(), 20);
    }

    #[test]
    fn test_fuel_exact_exhaustion() {
        let fuel = ExprFuelCounter::new(100).unwrap();

        fuel.consume(100).unwrap();
        assert_eq!(fuel.remaining(), 0);

        let err = fuel.consume(1).unwrap_err();
        assert!(matches!(
            err,
            RecursionError::FuelExhausted {
                remaining: 0,
                requested: 1
            }
        ));
    }

    #[test]
    fn test_fuel_has_fuel() {
        let fuel = ExprFuelCounter::new(100).unwrap();

        assert!(fuel.has_fuel(50));
        assert!(fuel.has_fuel(100));
        assert!(!fuel.has_fuel(101));

        fuel.consume(60).unwrap();
        assert!(fuel.has_fuel(40));
        assert!(!fuel.has_fuel(41));
    }

    #[test]
    fn test_fuel_reset() {
        let fuel = ExprFuelCounter::new(100).unwrap();

        fuel.consume(80).unwrap();
        assert_eq!(fuel.remaining(), 20);

        fuel.reset();
        assert_eq!(fuel.remaining(), 100);
        assert_eq!(fuel.consumed(), 0);
    }

    #[test]
    fn test_fuel_no_underflow_on_exhaustion() {
        // This test verifies the fix from FINAL_CORRECTIONS.md
        let fuel = ExprFuelCounter::new(5).unwrap();

        // Try to consume more than available
        let err = fuel.consume(10).unwrap_err();
        assert!(matches!(
            err,
            RecursionError::FuelExhausted {
                remaining: 5,
                requested: 10
            }
        ));

        // Fuel should still be 5, not underflowed
        assert_eq!(fuel.remaining(), 5);
    }

    #[test]
    fn test_fuel_multiple_small_consumes() {
        let fuel = ExprFuelCounter::new(100).unwrap();

        for _ in 0..10 {
            fuel.consume(10).unwrap();
        }

        assert_eq!(fuel.remaining(), 0);
        assert_eq!(fuel.consumed(), 100);
    }

    // Integration tests
    #[test]
    fn test_recursive_function_with_guard() {
        fn recursive_countdown(
            n: usize,
            guard: &mut RecursionGuard,
        ) -> Result<usize, RecursionError> {
            guard.enter()?;
            let result = if n == 0 {
                Ok(0)
            } else {
                recursive_countdown(n - 1, guard)
            };
            guard.exit();
            result
        }

        let mut guard = RecursionGuard::new(100).unwrap();
        let result = recursive_countdown(50, &mut guard);
        assert!(result.is_ok());
        assert_eq!(guard.current_depth(), 0); // Should be back to 0
        assert_eq!(guard.max_depth_reached(), 51); // 50 + initial call
    }

    #[test]
    fn test_recursive_function_exceeds_limit() {
        fn recursive_countdown(
            n: usize,
            guard: &mut RecursionGuard,
        ) -> Result<usize, RecursionError> {
            guard.enter()?;
            let result = if n == 0 {
                Ok(0)
            } else {
                recursive_countdown(n - 1, guard)
            };
            guard.exit();
            result
        }

        let mut guard = RecursionGuard::new(10).unwrap();
        let result = recursive_countdown(20, &mut guard);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            RecursionError::DepthLimitExceeded { .. }
        ));
    }

    #[test]
    fn test_expression_evaluation_with_fuel() {
        fn evaluate_tree(nodes: usize, fuel: &ExprFuelCounter) -> Result<(), RecursionError> {
            for _ in 0..nodes {
                fuel.consume(1)?;
            }
            Ok(())
        }

        let fuel = ExprFuelCounter::new(100).unwrap();
        let result = evaluate_tree(50, &fuel);
        assert!(result.is_ok());
        assert_eq!(fuel.remaining(), 50);
    }

    #[test]
    fn test_expression_evaluation_exhausts_fuel() {
        fn evaluate_tree(nodes: usize, fuel: &ExprFuelCounter) -> Result<(), RecursionError> {
            for _ in 0..nodes {
                fuel.consume(1)?;
            }
            Ok(())
        }

        let fuel = ExprFuelCounter::new(50).unwrap();
        let result = evaluate_tree(100, &fuel);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            RecursionError::FuelExhausted { .. }
        ));
    }
}
