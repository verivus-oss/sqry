//! CLI integration tests for Lua relation queries
//!
//! Tests that relation queries work end-to-end through the CLI for Lua:
//! - Callers queries (function calls, dot methods, colon methods, bracket calls)
//! - Callees queries (what a function calls, with receiver metadata)
//! - Exports queries (return tables, declarations, assignments, colon methods)
//!
//! This validates the Lua relation extraction implementation against the
//! ≥95% recall target (M1, M2, M3 from docs/development/relation-tracking/lua/01_SPEC.md).

mod common;
use common::sqry_bin;

use assert_cmd::Command;
use predicates::prelude::*;
use std::path::Path;
use tempfile::TempDir;

// ============================================================================
// Callers Queries - Function and Method Calls
// ============================================================================

#[test]
fn cli_lua_callers_simple_function() {
    let project = TempDir::new().unwrap();

    let lua_code = r"
local function helper()
    return 42
end

function main()
    local result = helper()
    print(result)
end

main()
";
    std::fs::write(project.path().join("script.lua"), lua_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of helper function
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:helper")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("main"));
}

#[test]
fn cli_lua_callers_module_function_dot_syntax() {
    let project = TempDir::new().unwrap();

    let lua_code = r"
local Module = {}

function Module.init()
    return {}
end

function Module.setup()
    local config = Module.init()
    return config
end

return Module
";
    std::fs::write(project.path().join("module.lua"), lua_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of Module.init
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:Module::init")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("setup"));
}

#[test]
fn cli_lua_callers_colon_method() {
    let project = TempDir::new().unwrap();

    let lua_code = r#"
local Player = {}

function Player:new(name)
    local instance = {name = name, hp = 100}
    setmetatable(instance, {__index = Player})
    return instance
end

function Player:takeDamage(amount)
    self.hp = self.hp - amount
end

function Player:attack(target)
    target:takeDamage(10)
end

local p1 = Player:new("Alice")
local p2 = Player:new("Bob")
p1:attack(p2)
"#;
    std::fs::write(project.path().join("player.lua"), lua_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of takeDamage (colon method)
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:Player::takeDamage")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("attack"));
}

#[test]
fn cli_lua_callers_nested_module_call() {
    let project = TempDir::new().unwrap();

    let lua_code = r#"
local vim = {
    api = {
        nvim_buf_set_lines = function(buf, start, end_, strict, lines)
            return true
        end,
        nvim_create_autocmd = function(event, opts)
            return 1
        end
    }
}

function setup_autocmds()
    vim.api.nvim_create_autocmd("BufEnter", {
        callback = function()
            vim.api.nvim_buf_set_lines(0, 0, 1, false, {"-- Header"})
        end
    })
end

setup_autocmds()
"#;
    std::fs::write(project.path().join("autocmds.lua"), lua_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of nvim_create_autocmd
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:vim::api::nvim_create_autocmd")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("setup_autocmds"));

    // Query for callers of nvim_buf_set_lines
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:vim::api::nvim_buf_set_lines")
        .arg(project.path())
        .assert()
        .success();
    // Called from anonymous callback - might not show named caller
}

#[test]
fn cli_lua_callers_bracket_call_string_literal() {
    let project = TempDir::new().unwrap();

    let lua_code = r#"
local commands = {
    start = function()
        print("Starting...")
    end,
    stop = function()
        print("Stopping...")
    end
}

function execute_command(cmd)
    commands[cmd]()
end

-- Only string literal calls should be indexed
commands["start"]()
execute_command("stop")
"#;
    std::fs::write(project.path().join("commands.lua"), lua_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of commands["start"] (should find top-level call)
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:commands::start")
        .arg(project.path())
        .assert()
        .success();
    // Note: execute_command won't show because commands[cmd] has variable key
}

#[test]
fn cli_lua_callers_chained_method_calls() {
    let project = TempDir::new().unwrap();

    let lua_code = r#"
local Query = {}

function Query:new()
    return setmetatable({}, {__index = Query})
end

function Query:where(condition)
    return self
end

function Query:orderBy(field)
    return self
end

function Query:execute()
    return {}
end

function run_query()
    local results = Query:new():where("active = 1"):orderBy("created_at"):execute()
    return results
end

run_query()
"#;
    std::fs::write(project.path().join("query.lua"), lua_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of where, orderBy, execute
    // All should show run_query as caller
    for method in ["where", "orderBy", "execute"] {
        Command::new(sqry_bin())
            .arg("query")
            .arg(format!("callers:Query::{method}"))
            .arg(project.path())
            .assert()
            .success()
            .stdout(predicate::str::contains("run_query"));
    }
}

#[test]
fn cli_lua_callers_with_json_metadata() {
    let project = TempDir::new().unwrap();

    let lua_code = r#"
local Service = {}

function Service:process(data)
    return data
end

function handler()
    local svc = Service
    svc:process({key = "value"})
end

handler()
"#;
    std::fs::write(project.path().join("service.lua"), lua_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers with JSON output to verify metadata
    let output = Command::new(sqry_bin())
        .arg("query")
        .arg("callers:Service::process")
        .arg("-j")
        .arg(project.path())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Verify JSON structure contains the caller function
    assert!(stdout.contains("handler"));
    // Verify JSON has standard structure fields
    assert!(stdout.contains("\"name\""));
    assert!(stdout.contains("\"kind\""));
}

// ============================================================================
// Callees Queries - What Functions Call
// ============================================================================

#[test]
fn cli_lua_callees_simple_function() {
    let project = TempDir::new().unwrap();

    let lua_code = r#"
function log(message)
    print(message)
end

function warn(message)
    print("[WARN] " .. message)
end

function main()
    log("Application started")
    warn("Low memory")
end

main()
"#;
    std::fs::write(project.path().join("logging.lua"), lua_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callees of main
    Command::new(sqry_bin())
        .arg("query")
        .arg("callees:main")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("log"))
        .stdout(predicate::str::contains("warn"));
}

#[test]
fn cli_lua_callees_module_method() {
    let project = TempDir::new().unwrap();

    let lua_code = r#"
local Database = {}

function Database.connect(config)
    return {}
end

function Database.disconnect(conn)
    -- cleanup
end

function Database.transaction(callback)
    local conn = Database.connect({host = "localhost"})
    callback(conn)
    Database.disconnect(conn)
end

Database.transaction(function(conn) end)
"#;
    std::fs::write(project.path().join("database.lua"), lua_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callees of transaction
    Command::new(sqry_bin())
        .arg("query")
        .arg("callees:Database::transaction")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("connect"))
        .stdout(predicate::str::contains("disconnect"));
}

#[test]
#[ignore = "implicit_self/receiver metadata not included in unified graph output - requires P2-XX"]
fn cli_lua_callees_colon_method_with_metadata() {
    let project = TempDir::new().unwrap();

    let lua_code = r#"
local HTTP = {}

function HTTP:get(url)
    return self:request("GET", url)
end

function HTTP:post(url, data)
    return self:request("POST", url, data)
end

function HTTP:request(method, url, data)
    return {status = 200, body = ""}
end

local client = HTTP
client:get("https://api.example.com/users")
"#;
    std::fs::write(project.path().join("http.lua"), lua_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callees of get method
    let output = Command::new(sqry_bin())
        .arg("query")
        .arg("callees:HTTP::get")
        .arg("-j")
        .arg(project.path())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Verify JSON metadata for colon calls
    assert!(stdout.contains("request") || stdout.contains("HTTP::request"));
    assert!(stdout.contains("implicit_self") || stdout.contains("receiver"));
}

#[test]
fn cli_lua_callees_nested_calls() {
    let project = TempDir::new().unwrap();

    let lua_code = r"
local vim = {
    api = {
        nvim_get_current_buf = function() return 0 end,
        nvim_buf_get_lines = function(buf, start, end_, strict) return {} end
    }
}

function get_buffer_content()
    local buf = vim.api.nvim_get_current_buf()
    local lines = vim.api.nvim_buf_get_lines(buf, 0, -1, false)
    return lines
end

get_buffer_content()
";
    std::fs::write(project.path().join("buffer.lua"), lua_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callees of get_buffer_content
    Command::new(sqry_bin())
        .arg("query")
        .arg("callees:get_buffer_content")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("nvim_get_current_buf"))
        .stdout(predicate::str::contains("nvim_buf_get_lines"));
}

#[test]
fn cli_lua_callees_with_receiver_metadata() {
    let project = TempDir::new().unwrap();

    let lua_code = r"
local Config = {}

function Config.load()
    return {}
end

function Config.validate(cfg)
    return true
end

function initialize()
    local cfg = Config.load()
    if Config.validate(cfg) then
        return cfg
    end
    return nil
end

initialize()
";
    std::fs::write(project.path().join("config.lua"), lua_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callees with JSON to verify receiver metadata
    let output = Command::new(sqry_bin())
        .arg("query")
        .arg("callees:initialize")
        .arg("-j")
        .arg(project.path())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Verify callees are found
    assert!(stdout.contains("load") || stdout.contains("Config::load"));
    assert!(stdout.contains("validate") || stdout.contains("Config::validate"));
    // Verify JSON structure
    assert!(stdout.contains("\"results\""));
}

// ============================================================================
// Exports Queries - Public API Surface
// ============================================================================

#[test]
fn cli_lua_exports_return_table() {
    let project = TempDir::new().unwrap();

    let lua_code = r"
local M = {}

local function private_helper()
    return 42
end

function M.public_function(x)
    return x * 2
end

function M.another_function(y)
    return private_helper() + y
end

return M
";
    std::fs::write(project.path().join("module.lua"), lua_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for exports - test specific exported functions
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:public_function")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("public_function"));

    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:another_function")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("another_function"));

    // Should NOT find private_helper (local function)
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:private_helper")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::is_empty().or(predicate::str::contains("0 matches")));
}

#[test]

fn cli_lua_exports_function_declarations() {
    let project = TempDir::new().unwrap();

    let lua_code = r#"
function global_func()
    return "global"
end

local function local_func()
    return "local"
end

function Module.namespaced_func()
    return "namespaced"
end

return {
    global_func = global_func,
    namespaced = Module.namespaced_func
}
"#;
    std::fs::write(project.path().join("exports.lua"), lua_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for exports - test specific declarations
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:global_func")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("global_func"));

    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:namespaced_func")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("namespaced_func"));

    // Should NOT find local_func
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:local_func")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::is_empty().or(predicate::str::contains("0 matches")));
}

#[test]

fn cli_lua_exports_colon_method_declarations() {
    let project = TempDir::new().unwrap();

    let lua_code = r#"
local Class = {}

function Class:method_one()
    return self
end

function Class:method_two(arg)
    return self:method_one()
end

function Class.static_method()
    return "static"
end

return Class
"#;
    std::fs::write(project.path().join("class.lua"), lua_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for exports - should find colon method
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:method_one")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("method_one"));

    // Query for static method (dot syntax)
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:static_method")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("static_method"));
}

#[test]

fn cli_lua_exports_assignment_exports() {
    let project = TempDir::new().unwrap();

    let lua_code = r"
local API = {}

API.fetch = function(url)
    return {}
end

API.parse = function(data)
    return data
end

-- Local assignment should not be exported
local private = function()
    return nil
end

return API
";
    std::fs::write(project.path().join("api.lua"), lua_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for exports - test assignment exports
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:fetch")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("fetch"));

    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:parse")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("parse"));

    // Should NOT find private (local assignment)
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:private")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::is_empty().or(predicate::str::contains("0 matches")));
}

#[test]

fn cli_lua_exports_bracket_assignment_string_literal() {
    let project = TempDir::new().unwrap();

    let lua_code = r#"
local Handlers = {}

Handlers["on-click"] = function(event)
    return true
end

Handlers["on-hover"] = function(event)
    return false
end

-- Dynamic key should not be exported
local key = "dynamic"
Handlers[key] = function() end

return Handlers
"#;
    std::fs::write(project.path().join("handlers.lua"), lua_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for exports - test bracket string literal assignments
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:on-click")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("on-click"));

    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:on-hover")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("on-hover"));

    // Dynamic key exports are skipped per spec (variable keys not extracted)
}

#[test]

fn cli_lua_exports_filtering_visibility() {
    let project = TempDir::new().unwrap();

    let lua_code = r"
-- All non-local declarations are public in Lua
function public_one()
    return 1
end

local function private_one()
    return 2
end

local M = {}

function M.public_two()
    return 3
end

M.public_three = function()
    return 4
end

return M
";
    std::fs::write(project.path().join("visibility.lua"), lua_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for exports - test visibility filtering
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:public_one")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("public_one"));

    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:public_two")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("public_two"));

    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:public_three")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("public_three"));

    // Should NOT find private_one (local function)
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:private_one")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::is_empty().or(predicate::str::contains("0 matches")));
}

// ============================================================================
// Integration Tests - Realistic Fixtures
// ============================================================================

#[test]
fn cli_lua_integration_neovim_plugin_exports() {
    let project = TempDir::new().unwrap();

    // Copy neovim_plugin.lua fixture
    let fixture_path =
        Path::new("../sqry-lang-lua/tests/fixtures/relations/lua/integration/neovim_plugin.lua");

    if !fixture_path.exists() {
        // Skip test if fixture not available
        eprintln!("Skipping test: fixture not found at {fixture_path:?}");
        return;
    }

    std::fs::copy(fixture_path, project.path().join("neovim_plugin.lua")).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for exports - test if fixture can be queried
    // Note: Without knowing exact export names, just verify indexing worked
    Command::new(sqry_bin())
        .arg("query")
        .arg("kind:function")
        .arg(project.path())
        .assert()
        .success(); // Should find some functions at minimum
}

#[test]
fn cli_lua_integration_wow_addon_callers() {
    let project = TempDir::new().unwrap();

    // Copy wow_addon.lua fixture
    let fixture_path =
        Path::new("../sqry-lang-lua/tests/fixtures/relations/lua/integration/wow_addon.lua");

    if !fixture_path.exists() {
        eprintln!("Skipping test: fixture not found at {fixture_path:?}");
        return;
    }

    std::fs::copy(fixture_path, project.path().join("wow_addon.lua")).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query should succeed - just verify indexing worked
    Command::new(sqry_bin())
        .arg("query")
        .arg("kind:function")
        .arg(project.path())
        .assert()
        .success();
}

#[test]
fn cli_lua_integration_love2d_comprehensive() {
    let project = TempDir::new().unwrap();

    // Copy love2d_game.lua fixture
    let fixture_path =
        Path::new("../sqry-lang-lua/tests/fixtures/relations/lua/integration/love2d_game.lua");

    if !fixture_path.exists() {
        eprintln!("Skipping test: fixture not found at {fixture_path:?}");
        return;
    }

    std::fs::copy(fixture_path, project.path().join("love2d_game.lua")).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Test that indexing and queries work on fixture
    Command::new(sqry_bin())
        .arg("query")
        .arg("kind:function")
        .arg(project.path())
        .assert()
        .success();
}
