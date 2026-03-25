// Test fixture: Macro invocations
// Tests: println!, vec!, assert_eq!

fn test_print_macros() {
    println!("Simple message");
    println!("Formatted: {}", 42);
    println!("Multiple: {} and {}", "first", "second");

    eprintln!("Error message");
}

fn test_collection_macros() -> Vec<i32> {
    let v1 = vec![1, 2, 3];
    let v2 = vec![4; 3]; // [4, 4, 4]

    println!("v1: {:?}", v1);
    println!("v2: {:?}", v2);

    v1
}

fn test_assertion_macros(a: i32, b: i32) {
    assert_eq!(a + b, 3);
    assert_ne!(a, b);
    assert!(a > 0);

    debug_assert!(b > 0);
}

macro_rules! custom_macro {
    ($x:expr) => {
        println!("Custom macro: {}", $x)
    };
}

fn test_custom_macro() {
    custom_macro!(42);
    custom_macro!("Hello");
}

fn main() {
    test_print_macros();
    let numbers = test_collection_macros();
    test_assertion_macros(1, 2);
    test_custom_macro();

    let map = std::collections::HashMap::from([
        ("key1", 1),
        ("key2", 2),
    ]);
    println!("Map: {:?}", map);
}
