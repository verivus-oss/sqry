extern "C" {
    fn c_helper(value: i32) -> i32;
}

pub fn call_native(input: i32) -> i32 {
    unsafe { c_helper(input) }
}
