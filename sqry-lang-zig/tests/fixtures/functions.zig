// Test functions with various modifiers (AC-ZIG-1)

const std = @import("std");

// Public function
pub fn add(a: i32, b: i32) i32 {
    return a + b;
}

// Private function (no pub)
fn multiply(a: i32, b: i32) i32 {
    return a * b;
}

// Generic function with comptime parameters
pub fn max(comptime T: type, a: T, b: T) T {
    return if (a > b) a else b;
}

// Extern function (C interop)
extern fn external_c_function(x: i32) i32;

// Function with error union return type
pub fn divide(a: i32, b: i32) !f32 {
    if (b == 0) return error.DivisionByZero;
    return @as(f32, @floatFromInt(a)) / @as(f32, @floatFromInt(b));
}

// Function with multiple comptime parameters
pub fn Pair(comptime First: type, comptime Second: type) type {
    return struct {
        first: First,
        second: Second,
    };
}
