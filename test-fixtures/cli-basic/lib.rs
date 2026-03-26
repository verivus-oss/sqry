//! Minimal test fixture library module

/// Helper module for utilities
pub mod utils {
    /// Subtract two numbers
    pub fn subtract(a: i32, b: i32) -> i32 {
        a - b
    }

    /// Divide two numbers
    pub fn divide(a: i32, b: i32) -> i32 {
        if b == 0 {
            0
        } else {
            a / b
        }
    }
}

/// A simple constant
pub const PI: f64 = 3.14159;

/// A simple trait
pub trait Processor {
    fn process(&self);
}

/// An implementation
pub struct DefaultProcessor;

impl Processor for DefaultProcessor {
    fn process(&self) {
        println!("Processing...");
    }
}
