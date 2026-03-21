//! Test fixture for Rust call patterns
//!
//! Covers three callee types that the Rust plugin should handle:
//! 1. Simple identifier: foo()
//! 2. Field expression: obj.method()
//! 3. Scoped identifier: std::io::stdout()

// Simple identifier calls
fn outer_function() {
    helper_function();
    another_helper();
}

fn helper_function() {
    println!("Helper");
}

fn another_helper() {
    println!("Another helper");
}

// Field expression (method) calls
struct MyStruct {
    value: i32,
}

impl MyStruct {
    fn new(value: i32) -> Self {
        MyStruct { value }
    }

    fn method_one(&self) {
        self.method_two();
    }

    fn method_two(&self) {
        println!("Value: {}", self.value);
    }

    fn method_with_calls(&self) {
        self.method_one();
        self.method_two();
        helper_function();
    }
}

// Scoped identifier calls
fn function_with_scoped_calls() {
    std::io::stdout();
    std::process::exit(0);
}

// Mixed patterns
fn complex_function() {
    // Simple identifier
    helper_function();

    // Field expression
    let obj = MyStruct::new(42);
    obj.method_one();

    // Scoped identifier
    std::io::stdout();
}

// Nested function calls
fn outer() {
    fn inner() {
        helper_function();
    }
    inner();
}

fn main() {
    outer_function();
    let s = MyStruct::new(10);
    s.method_with_calls();
}
