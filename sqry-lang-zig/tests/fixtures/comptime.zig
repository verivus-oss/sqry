// Test comptime handling (AC-ZIG-6)

const std = @import("std");

// Comptime function
pub fn factorial(comptime n: u32) u32 {
    if (n <= 1) return 1;
    return n * factorial(n - 1);
}

// Comptime block within function
pub fn buildStruct(comptime fields: u32) type {
    return struct {
        comptime {
            @setEvalBranchQuota(2000);
        }

        data: [fields]u32,

        pub fn init() @This() {
            return .{ .data = [_]u32{0} ** fields };
        }
    };
}

// Comptime variable
pub const COMPUTED_VALUE = comptime blk: {
    var sum: u32 = 0;
    var i: u32 = 0;
    while (i < 10) : (i += 1) {
        sum += i;
    }
    break :blk sum;
};

// Generic with comptime logic
pub fn Array(comptime T: type, comptime size: usize) type {
    comptime {
        if (size == 0) {
            @compileError("Array size must be greater than 0");
        }
    }

    return struct {
        data: [size]T,

        pub fn len(self: @This()) usize {
            return self.data.len;
        }
    };
}

// Inline function with comptime
pub inline fn square(comptime x: anytype) @TypeOf(x) {
    return x * x;
}
