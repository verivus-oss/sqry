// Test fixture: FFI extern "C" and extern "system" blocks
// Tests: malloc, free, printf, GetCurrentProcessId

use std::ffi::CString;

#[cfg(unix)]
extern "C" {
    fn malloc(size: usize) -> *mut u8;
    fn free(ptr: *mut u8);
    fn printf(format: *const i8, ...) -> i32;
    fn getpid() -> i32;
}

#[cfg(windows)]
extern "system" {
    fn GetCurrentProcessId() -> u32;
    fn Sleep(milliseconds: u32);
}

fn use_libc() {
    #[cfg(unix)]
    unsafe {
        let ptr = malloc(100);
        if !ptr.is_null() {
            let msg = CString::new("Hello from FFI\n").unwrap();
            printf(msg.as_ptr());
            free(ptr);
        }
        let pid = getpid();
        println!("Process ID: {}", pid);
    }

    #[cfg(windows)]
    unsafe {
        let pid = GetCurrentProcessId();
        println!("Process ID: {}", pid);
        Sleep(0);
    }
}

extern "C" fn callback_function(value: i32) -> i32 {
    value * 2
}

fn main() {
    use_libc();

    let result = callback_function(21);
    println!("Callback result: {}", result);
}
