//! Integration tests for config commands
//!
//! Tests the unified graph config partition CLI commands:
//! - sqry config init
//! - sqry config show
//! - sqry config set/get
//! - sqry config validate
//! - sqry config alias set/list/remove

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use tempfile::TempDir;

/// Helper to create a sqry command
#[allow(deprecated)]
fn sqry_cmd() -> Command {
    Command::cargo_bin("sqry").unwrap()
}

#[test]
fn test_config_init_creates_file() {
    let temp = TempDir::new().unwrap();
    let config_path = temp.path().join(".sqry/graph/config/config.json");

    // Config should not exist initially
    assert!(!config_path.exists());

    // Initialize config
    sqry_cmd()
        .arg("config")
        .arg("init")
        .arg("--path")
        .arg(temp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Config initialized"));

    // Config file should now exist
    assert!(config_path.exists());

    // Verify it's valid JSON
    let content = fs::read_to_string(&config_path).unwrap();
    let json: serde_json::Value = serde_json::from_str(&content).unwrap();

    // Check schema version
    assert_eq!(json["schema_version"], 1);
}

#[test]
fn test_config_init_force_overwrites() {
    let temp = TempDir::new().unwrap();
    let _config_path = temp.path().join(".sqry/graph/config/config.json");

    // Initialize config first time
    sqry_cmd()
        .arg("config")
        .arg("init")
        .arg("--path")
        .arg(temp.path())
        .assert()
        .success();

    // Try to init again without force - should fail
    sqry_cmd()
        .arg("config")
        .arg("init")
        .arg("--path")
        .arg(temp.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("already initialized"));

    // Init with --force should succeed
    sqry_cmd()
        .arg("config")
        .arg("init")
        .arg("--path")
        .arg(temp.path())
        .arg("--force")
        .assert()
        .success()
        .stdout(predicate::str::contains("Config initialized"));
}

#[test]
fn test_config_show_displays_config() {
    let temp = TempDir::new().unwrap();

    // Initialize config
    sqry_cmd()
        .arg("config")
        .arg("init")
        .arg("--path")
        .arg(temp.path())
        .assert()
        .success();

    // Show config
    sqry_cmd()
        .arg("config")
        .arg("show")
        .arg("--path")
        .arg(temp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Schema version"))
        .stdout(predicate::str::contains("Limits"))
        .stdout(predicate::str::contains("max_results"));
}

#[test]
fn test_config_show_json_output() {
    let temp = TempDir::new().unwrap();

    // Initialize config
    sqry_cmd()
        .arg("config")
        .arg("init")
        .arg("--path")
        .arg(temp.path())
        .assert()
        .success();

    // Show config as JSON
    let output = sqry_cmd()
        .arg("config")
        .arg("show")
        .arg("--path")
        .arg(temp.path())
        .arg("--json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    // Parse as JSON
    let json: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["schema_version"], 1);
    assert!(json["config"]["limits"].is_object());
}

#[test]
fn test_config_show_specific_key() {
    let temp = TempDir::new().unwrap();

    // Initialize config
    sqry_cmd()
        .arg("config")
        .arg("init")
        .arg("--path")
        .arg(temp.path())
        .assert()
        .success();

    // Show specific key
    sqry_cmd()
        .arg("config")
        .arg("show")
        .arg("--path")
        .arg(temp.path())
        .arg("--key")
        .arg("limits.max_results")
        .assert()
        .success()
        .stdout(predicate::str::contains("5000"));
}

#[test]
fn test_config_get_retrieves_value() {
    let temp = TempDir::new().unwrap();

    // Initialize config
    sqry_cmd()
        .arg("config")
        .arg("init")
        .arg("--path")
        .arg(temp.path())
        .assert()
        .success();

    // Get a config value
    sqry_cmd()
        .arg("config")
        .arg("get")
        .arg("--path")
        .arg(temp.path())
        .arg("limits.max_results")
        .assert()
        .success()
        .stdout("5000\n");
}

#[test]
fn test_config_set_updates_value() {
    let temp = TempDir::new().unwrap();

    // Initialize config
    sqry_cmd()
        .arg("config")
        .arg("init")
        .arg("--path")
        .arg(temp.path())
        .assert()
        .success();

    // Set a new value (with --yes to skip confirmation)
    sqry_cmd()
        .arg("config")
        .arg("set")
        .arg("--path")
        .arg(temp.path())
        .arg("limits.max_results")
        .arg("10000")
        .arg("--yes")
        .assert()
        .success()
        .stdout(predicate::str::contains("Config updated"));

    // Verify the new value
    sqry_cmd()
        .arg("config")
        .arg("get")
        .arg("--path")
        .arg(temp.path())
        .arg("limits.max_results")
        .assert()
        .success()
        .stdout("10000\n");
}

#[test]
fn test_config_set_validates_value() {
    let temp = TempDir::new().unwrap();

    // Initialize config
    sqry_cmd()
        .arg("config")
        .arg("init")
        .arg("--path")
        .arg(temp.path())
        .assert()
        .success();

    // Try to set invalid value for page_size (0 is not allowed)
    sqry_cmd()
        .arg("config")
        .arg("set")
        .arg("--path")
        .arg(temp.path())
        .arg("output.page_size")
        .arg("0")
        .arg("--yes")
        .assert()
        .failure();
}

#[test]
fn test_config_validate_checks_config() {
    let temp = TempDir::new().unwrap();

    // Initialize config
    sqry_cmd()
        .arg("config")
        .arg("init")
        .arg("--path")
        .arg(temp.path())
        .assert()
        .success();

    // Validate should pass
    sqry_cmd()
        .arg("config")
        .arg("validate")
        .arg("--path")
        .arg(temp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Config is valid"));
}

#[test]
fn test_config_alias_set_creates_alias() {
    let temp = TempDir::new().unwrap();

    // Initialize config
    sqry_cmd()
        .arg("config")
        .arg("init")
        .arg("--path")
        .arg(temp.path())
        .assert()
        .success();

    // Create an alias
    sqry_cmd()
        .arg("config")
        .arg("alias")
        .arg("set")
        .arg("--path")
        .arg(temp.path())
        .arg("my-funcs")
        .arg("kind:function")
        .assert()
        .success()
        .stdout(predicate::str::contains("Alias 'my-funcs' created"));
}

#[test]
fn test_config_alias_set_with_description() {
    let temp = TempDir::new().unwrap();

    // Initialize config
    sqry_cmd()
        .arg("config")
        .arg("init")
        .arg("--path")
        .arg(temp.path())
        .assert()
        .success();

    // Create an alias with description
    sqry_cmd()
        .arg("config")
        .arg("alias")
        .arg("set")
        .arg("--path")
        .arg(temp.path())
        .arg("my-funcs")
        .arg("kind:function")
        .arg("--description")
        .arg("Find all functions")
        .assert()
        .success()
        .stdout(predicate::str::contains("Alias 'my-funcs' created"))
        .stdout(predicate::str::contains("Find all functions"));
}

#[test]
fn test_config_alias_list_shows_aliases() {
    let temp = TempDir::new().unwrap();

    // Initialize config
    sqry_cmd()
        .arg("config")
        .arg("init")
        .arg("--path")
        .arg(temp.path())
        .assert()
        .success();

    // Initially no aliases
    sqry_cmd()
        .arg("config")
        .arg("alias")
        .arg("list")
        .arg("--path")
        .arg(temp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("No aliases defined"));

    // Create an alias
    sqry_cmd()
        .arg("config")
        .arg("alias")
        .arg("set")
        .arg("--path")
        .arg(temp.path())
        .arg("my-funcs")
        .arg("kind:function")
        .assert()
        .success();

    // List should now show the alias
    sqry_cmd()
        .arg("config")
        .arg("alias")
        .arg("list")
        .arg("--path")
        .arg(temp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("my-funcs"))
        .stdout(predicate::str::contains("kind:function"));
}

#[test]
fn test_config_alias_list_json_output() {
    let temp = TempDir::new().unwrap();

    // Initialize config
    sqry_cmd()
        .arg("config")
        .arg("init")
        .arg("--path")
        .arg(temp.path())
        .assert()
        .success();

    // Create an alias
    sqry_cmd()
        .arg("config")
        .arg("alias")
        .arg("set")
        .arg("--path")
        .arg(temp.path())
        .arg("my-funcs")
        .arg("kind:function")
        .assert()
        .success();

    // List as JSON
    let output = sqry_cmd()
        .arg("config")
        .arg("alias")
        .arg("list")
        .arg("--path")
        .arg(temp.path())
        .arg("--json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    // Parse as JSON
    let json: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert!(json["my-funcs"].is_object());
    assert_eq!(json["my-funcs"]["query"], "kind:function");
}

#[test]
fn test_config_alias_remove_deletes_alias() {
    let temp = TempDir::new().unwrap();

    // Initialize config
    sqry_cmd()
        .arg("config")
        .arg("init")
        .arg("--path")
        .arg(temp.path())
        .assert()
        .success();

    // Create an alias
    sqry_cmd()
        .arg("config")
        .arg("alias")
        .arg("set")
        .arg("--path")
        .arg(temp.path())
        .arg("my-funcs")
        .arg("kind:function")
        .assert()
        .success();

    // Remove the alias
    sqry_cmd()
        .arg("config")
        .arg("alias")
        .arg("remove")
        .arg("--path")
        .arg(temp.path())
        .arg("my-funcs")
        .assert()
        .success()
        .stdout(predicate::str::contains("Alias 'my-funcs' removed"));

    // List should show no aliases
    sqry_cmd()
        .arg("config")
        .arg("alias")
        .arg("list")
        .arg("--path")
        .arg(temp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("No aliases defined"));
}

#[test]
fn test_config_alias_remove_nonexistent_fails() {
    let temp = TempDir::new().unwrap();

    // Initialize config
    sqry_cmd()
        .arg("config")
        .arg("init")
        .arg("--path")
        .arg(temp.path())
        .assert()
        .success();

    // Try to remove non-existent alias
    sqry_cmd()
        .arg("config")
        .arg("alias")
        .arg("remove")
        .arg("--path")
        .arg(temp.path())
        .arg("nonexistent")
        .assert()
        .failure()
        .stderr(predicate::str::contains("not found"));
}

#[test]
fn test_config_alias_update_existing() {
    let temp = TempDir::new().unwrap();

    // Initialize config
    sqry_cmd()
        .arg("config")
        .arg("init")
        .arg("--path")
        .arg(temp.path())
        .assert()
        .success();

    // Create an alias
    sqry_cmd()
        .arg("config")
        .arg("alias")
        .arg("set")
        .arg("--path")
        .arg(temp.path())
        .arg("my-funcs")
        .arg("kind:function")
        .assert()
        .success()
        .stdout(predicate::str::contains("created"));

    // Update the same alias
    sqry_cmd()
        .arg("config")
        .arg("alias")
        .arg("set")
        .arg("--path")
        .arg(temp.path())
        .arg("my-funcs")
        .arg("kind:class")
        .assert()
        .success()
        .stdout(predicate::str::contains("updated"));

    // Verify the updated query
    let output = sqry_cmd()
        .arg("config")
        .arg("alias")
        .arg("list")
        .arg("--path")
        .arg(temp.path())
        .arg("--json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["my-funcs"]["query"], "kind:class");
}

#[test]
fn test_config_commands_without_init_fail() {
    let temp = TempDir::new().unwrap();

    // Show without init should fail
    sqry_cmd()
        .arg("config")
        .arg("show")
        .arg("--path")
        .arg(temp.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("not initialized"));

    // Get without init should fail
    sqry_cmd()
        .arg("config")
        .arg("get")
        .arg("--path")
        .arg(temp.path())
        .arg("limits.max_results")
        .assert()
        .failure()
        .stderr(predicate::str::contains("not initialized"));

    // Set without init should fail
    sqry_cmd()
        .arg("config")
        .arg("set")
        .arg("--path")
        .arg(temp.path())
        .arg("limits.max_results")
        .arg("10000")
        .arg("--yes")
        .assert()
        .failure()
        .stderr(predicate::str::contains("not initialized"));
}
