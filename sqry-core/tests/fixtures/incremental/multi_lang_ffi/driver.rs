//! Thin driver that exercises the FFI surface from the Rust side.
//!
//! Split out from `caller.rs` so the fixture has a second Rust file that
//! can be mutated independently of the extern block.

use crate::caller;

pub fn run_arithmetic(a: i32, b: i32) -> (i32, i32) {
    let sum = caller::call_add(a, b);
    let product = caller::call_multiply(a, b);
    (sum, product)
}
