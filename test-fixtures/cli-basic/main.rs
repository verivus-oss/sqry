//! Minimal test fixture for CLI basic tests
//! Contains simple functions and structures for basic queries

/// A simple utility function
pub fn calculate_sum(a: i32, b: i32) -> i32 {
    a + b
}

/// Another utility function
pub fn multiply(x: i32, y: i32) -> i32 {
    x * y
}

/// A simple struct
pub struct Calculator {
    value: i32,
}

impl Calculator {
    /// Create a new calculator
    pub fn new(initial: i32) -> Self {
        Calculator { value: initial }
    }

    /// Add to the current value
    pub fn add(&mut self, n: i32) {
        self.value = calculate_sum(self.value, n);
    }

    /// Get the current value
    pub fn get_value(&self) -> i32 {
        self.value
    }
}

fn main() {
    let result = calculate_sum(5, 3);
    println!("Sum: {}", result);

    let mut calc = Calculator::new(10);
    calc.add(5);
}
