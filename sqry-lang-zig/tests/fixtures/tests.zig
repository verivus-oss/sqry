// Test blocks (AC-ZIG-5)

const std = @import("std");
const testing = std.testing;

test "basic addition" {
    try testing.expect(2 + 2 == 4);
}

test "multiplication works" {
    const result = 3 * 4;
    try testing.expectEqual(@as(i32, 12), result);
}

test "array operations" {
    const arr = [_]i32{ 1, 2, 3, 4, 5 };
    try testing.expect(arr.len == 5);
    try testing.expect(arr[0] == 1);
}

test "string comparison" {
    const str1 = "hello";
    const str2 = "hello";
    try testing.expectEqualStrings(str1, str2);
}

test "error handling" {
    const value = try getPositiveNumber(42);
    try testing.expect(value == 42);
}

fn getPositiveNumber(x: i32) !i32 {
    if (x < 0) return error.Negative;
    return x;
}

// Anonymous test (no name)
test {
    try testing.expect(true);
}
