-- Test fixture for Lua GraphBuilder
-- Tests various function call patterns

-- Global function
function global_func()
    local_func()
end

-- Local function
local function local_func()
    -- Empty
end

-- Module pattern
local MyModule = {}

function MyModule.method1()
    MyModule.method2()
end

function MyModule:method2()
    self:method3()
end

function MyModule:method3()
    -- Empty
end

-- Closures
local function outer()
    local inner = function()
        outer()
    end
    inner()
end

-- Requires
local config = require("config")

-- Nested module access
local function process_data()
    MyModule.method1()
    MyModule:method2()
end

-- Bracket-based module method
function MyModule["string-key"]()
    return "ok"
end

-- Numeric command table
local commands = {}
commands[1] = function()
    return "cmd"
end

local function run_command()
    commands[1]()
end

-- Environment-driven name injection
_ENV["init"] = function()
    return true
end

local function boot()
    init()
end

return MyModule
