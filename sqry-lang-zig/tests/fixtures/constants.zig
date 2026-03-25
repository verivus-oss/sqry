// Test constants and variables (AC-ZIG-3)

const std = @import("std");

// Public constant
pub const MAX_SIZE: usize = 1024;

// Private constant
const BUFFER_SIZE: usize = 512;

// Comptime constant
pub const comptime DEFAULT_ALIGNMENT = 16;

// Variable
var global_counter: u32 = 0;

// Public variable
pub var initialized: bool = false;

// Thread-local variable
threadlocal var thread_id: u32 = 0;

// Constant with complex initialization
const PI: f64 = 3.14159265358979;

// Constant array
const PRIMES = [_]u32{ 2, 3, 5, 7, 11, 13 };

// Type alias
pub const ByteArray = []const u8;
