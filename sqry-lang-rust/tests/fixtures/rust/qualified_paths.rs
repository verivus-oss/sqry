// Test fixture: Qualified path calls
// Tests: std::mem::drop, std::process::exit, std::thread::sleep

use std::time::Duration;

fn test_mem_operations() {
    let s = String::from("Hello");
    std::mem::drop(s);
    // s is now dropped, can't use it

    let x = Box::new(42);
    let size = std::mem::size_of_val(&x);
    println!("Size: {}", size);
}

fn test_string_operations() {
    let s1 = String::from("hello");
    let s2 = String::from("world");

    let concatenated = std::format!("{} {}", s1, s2);
    println!("{}", concatenated);
}

fn conditional_exit(should_exit: bool) {
    if should_exit {
        // std::process::exit(1);  // Commented out to allow test to complete
        println!("Would exit here");
    }
}

fn test_thread_operations() {
    let duration = std::time::Duration::from_millis(10);
    std::thread::sleep(duration);

    let handle = std::thread::spawn(|| {
        println!("In spawned thread");
        42
    });

    let result = handle.join().unwrap();
    println!("Thread result: {}", result);
}

fn test_io_operations() {
    use std::io::Write;
    let mut buffer = std::vec::Vec::new();
    let _ = std::io::Write::write_all(&mut buffer, b"test");
    println!("Buffer len: {}", buffer.len());
}

fn main() {
    test_mem_operations();
    test_string_operations();
    conditional_exit(false);
    test_thread_operations();
    test_io_operations();
}
