// Test container types (AC-ZIG-2)

const std = @import("std");

// Struct
pub const Point = struct {
    x: f32,
    y: f32,

    pub fn distance(self: Point) f32 {
        return @sqrt(self.x * self.x + self.y * self.y);
    }
};

// Enum
const Status = enum {
    ok,
    error,
    pending,
};

// Tagged union
pub const Value = union(enum) {
    int: i32,
    float: f64,
    boolean: bool,
};

// Error set
pub const FileError = error{
    AccessDenied,
    OutOfMemory,
    FileNotFound,
};

// Opaque type
pub const OpaqueHandle = opaque {};

// Generic struct
pub fn List(comptime T: type) type {
    return struct {
        items: []T,
        len: usize,
    };
}

// Packed struct
const PackedData = packed struct {
    flag1: bool,
    flag2: bool,
    value: u6,
};
