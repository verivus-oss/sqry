// Rust FFI bindings to C math library
extern "C" {
    fn calculate_sum(a: i32, b: i32) -> i32;
    fn calculate_product(a: i32, b: i32) -> i32;
}

fn main() {
    unsafe {
        let sum = calculate_sum(3, 4);
        let product = calculate_product(3, 4);
        println!("Sum: {}, Product: {}", sum, product);
    }
}
