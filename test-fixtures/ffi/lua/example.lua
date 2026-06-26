#!/usr/bin/env luajit
--[[
LuaJIT FFI Example
Demonstrates all FFI patterns detected by sqry:
1. require("ffi")
2. ffi.cdef for C declarations
3. ffi.C.* for C library calls
4. ffi.load() for shared libraries
5. Aliasing patterns
]]

local ffi = require("ffi")

-- Declare C functions
ffi.cdef[[
    int printf(const char *fmt, ...);
    void *malloc(size_t size);
    void free(void *ptr);
    double cos(double x);
]]

-- Test 1: Direct C library calls
print("=== Test 1: Direct C library calls ===")
ffi.C.printf("Hello from LuaJIT FFI!\n")
ffi.C.printf("Value: %d\n", 42)

-- Test 2: C library alias
print("\n=== Test 2: C library alias ===")
local C = ffi.C
C.printf("Using alias C\n")
C.printf("Another call via alias\n")

-- Test 3: Load external library (math library)
print("\n=== Test 3: Load external library ===")
local m = ffi.load("m")  -- libm.so (math library)
local result = m.cos(0)
C.printf("cos(0) = %f\n", result)

-- Test 4: Memory management
print("\n=== Test 4: Memory management ===")
local ptr = C.malloc(1024)
C.printf("Allocated 1024 bytes at %p\n", ptr)
C.free(ptr)
C.printf("Memory freed\n")

-- Test 5: Multiple library loads
print("\n=== Test 5: Multiple library loads ===")
local m2 = ffi.load("m")
local result2 = m2.cos(1.5708)  -- cos(π/2) ≈ 0
C.printf("cos(π/2) = %f\n", result2)

-- Test 6: FFI in function
print("\n=== Test 6: FFI in function ===")
function test_ffi_in_function()
    C.printf("FFI call from inside a function\n")
    local x = m.cos(3.14159)
    C.printf("cos(π) = %f\n", x)
end

test_ffi_in_function()

-- Test 7: Nested functions with FFI
print("\n=== Test 7: Nested functions with FFI ===")
function outer_function()
    C.printf("Outer function\n")

    function inner_function()
        C.printf("Inner function\n")
    end

    inner_function()
end

outer_function()

-- Summary
print("\n=== All FFI Tests Completed Successfully! ===")
C.printf("Total FFI call patterns detected: 12+\n")
C.printf("Test file complete.\n")
