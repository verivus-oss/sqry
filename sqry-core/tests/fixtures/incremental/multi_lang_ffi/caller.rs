//! Rust caller for the C helpers defined in `native/*.c`.
//!
//! Exercises the Pass 5 FFI linker by declaring externs that match the
//! symbols defined in `native_math.c` and `native_util.c`.

unsafe extern "C" {
    fn native_add(a: i32, b: i32) -> i32;
    fn native_multiply(a: i32, b: i32) -> i32;
    fn native_format_status(code: i32) -> *const u8;
}

pub fn call_add(a: i32, b: i32) -> i32 {
    unsafe { native_add(a, b) }
}

pub fn call_multiply(a: i32, b: i32) -> i32 {
    unsafe { native_multiply(a, b) }
}

pub fn call_format_status(code: i32) -> *const u8 {
    unsafe { native_format_status(code) }
}
