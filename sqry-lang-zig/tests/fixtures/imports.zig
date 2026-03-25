// Test imports and usingnamespace (AC-ZIG-4)

const std = @import("std");
const builtin = @import("builtin");
const types = @import("types.zig");

// Using namespace pattern
pub usingnamespace @import("constants.zig");

// Conditional imports based on target
const os_specific = if (builtin.os.tag == .linux)
    @import("linux.zig")
else if (builtin.os.tag == .windows)
    @import("windows.zig")
else
    @import("generic.zig");

// Import with alias usage
pub fn processData(data: []const u8) !void {
    var allocator = std.heap.page_allocator;
    const result = try allocator.alloc(u8, data.len);
    defer allocator.free(result);
}
