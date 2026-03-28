// Test fixture: Unsafe functions and unsafe blocks
// Tests: dangerous fn, use_raw_ptr fn with unsafe blocks

unsafe fn dangerous(ptr: *mut i32) {
    if !ptr.is_null() {
        *ptr += 10;
    }
}

fn use_raw_ptr(value: &mut i32) {
    let ptr = value as *mut i32;
    unsafe {
        dangerous(ptr);
    }
}

unsafe fn read_uninitialized() -> i32 {
    let x: i32;
    // This is unsafe and demonstrates unsafe code patterns
    std::ptr::read(&x as *const i32)
}

fn safe_wrapper() {
    let mut num = 42;
    use_raw_ptr(&mut num);
    println!("Modified value: {}", num);
}

fn main() {
    safe_wrapper();

    let mut value = 100;
    unsafe {
        dangerous(&mut value as *mut i32);
        println!("Unsafe modification: {}", value);
    }
}
