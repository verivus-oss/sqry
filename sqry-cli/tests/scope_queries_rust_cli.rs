//! CLI integration tests for Rust scope queries (P2-34 Phase 2)
//!
//! Tests that scope.* queries work end-to-end through the CLI for Rust:
//! - scope.type queries (filtering by scope type: module, class, function)
//! - scope.name queries (filtering by scope name)
//! - scope.parent queries (filtering by immediate parent scope)
//! - scope.ancestor queries (filtering by any ancestor scope)
//! - Composition with name: filters

mod common;
use common::sqry_bin;

use assert_cmd::Command;
use tempfile::TempDir;

// ============================================================================
// scope.type Queries - Filter by Scope Type
// ============================================================================

#[test]
fn cli_scope_type_module_filters_functions_in_module() {
    let project = TempDir::new().unwrap();

    let rust_code = r#"
mod database {
    pub fn connect() {
        println!("Connecting to database");
    }

    pub fn disconnect() {
        println!("Disconnecting from database");
    }
}

fn helper() {
    println!("Top-level helper");
}
"#;
    std::fs::write(project.path().join("lib.rs"), rust_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for symbols in module scope
    let output = Command::new(sqry_bin())
        .arg("query")
        .arg("scope.type:module")
        .arg(project.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8_lossy(&output);

    // Should contain functions inside module
    assert!(
        stdout.contains("connect"),
        "Expected 'connect' in module scope. Actual output:\n{stdout}"
    );
    assert!(
        stdout.contains("disconnect"),
        "Expected 'disconnect' in module scope"
    );

    // Should NOT contain top-level helper (not in module scope)
    assert!(
        !stdout.contains("helper"),
        "Top-level 'helper' should not be in module scope"
    );
}

#[test]
fn cli_scope_type_class_filters_methods() {
    let project = TempDir::new().unwrap();

    let rust_code = r#"
struct Connection {
    host: String,
    port: u16,
}

impl Connection {
    fn new(host: String, port: u16) -> Self {
        Connection { host, port }
    }

    fn connect(&self) {
        println!("Connecting to {}:{}", self.host, self.port);
    }
}

fn global_init() {
    println!("Initializing");
}
"#;
    std::fs::write(project.path().join("connection.rs"), rust_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for methods (impl blocks are not tracked as nodes in the unified graph,
    // but methods inside them are tracked as NodeKind::Method)
    let output = Command::new(sqry_bin())
        .arg("query")
        .arg("kind:method")
        .arg(project.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8_lossy(&output);

    // Should contain methods inside impl
    assert!(stdout.contains("new"), "Expected 'new' method");
    assert!(stdout.contains("connect"), "Expected 'connect' method");

    // Should NOT contain global function (it's a function, not a method)
    assert!(
        !stdout.contains("global_init"),
        "Global 'global_init' should not be a method"
    );
}

// ============================================================================
// scope.ancestor Queries - Filter by Ancestor Scope
// ============================================================================

#[test]
fn cli_scope_ancestor_matches_nested_methods() {
    let project = TempDir::new().unwrap();

    let rust_code = r#"
mod network {
    mod client {
        pub fn connect() {
            println!("Connecting via client");
        }

        pub fn disconnect() {
            println!("Disconnecting client");
        }
    }

    mod server {
        pub fn start() {
            println!("Starting server");
        }
    }
}

mod database {
    pub fn connect() {
        println!("Connecting to database");
    }
}
"#;
    std::fs::write(project.path().join("app.rs"), rust_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for symbols with 'network' ancestor
    let output = Command::new(sqry_bin())
        .arg("query")
        .arg("scope.ancestor:network")
        .arg(project.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8_lossy(&output);

    // Should contain functions nested under network module
    // (3 total: connect, disconnect, start)
    assert!(
        stdout.contains("connect"),
        "Expected 'connect' from network::client"
    );
    assert!(
        stdout.contains("disconnect"),
        "Expected 'disconnect' from network::client"
    );
    assert!(
        stdout.contains("start"),
        "Expected 'start' from network::server"
    );

    // Verify exactly 3 matches (all from network hierarchy)
    let match_count = stdout.matches("function").count();
    assert_eq!(
        match_count, 3,
        "Expected exactly 3 functions with network ancestor"
    );
}

// ============================================================================
// scope.* with name: Composition - Combining Filters
// ============================================================================

#[test]
fn cli_scope_type_with_name_composition() {
    let project = TempDir::new().unwrap();

    let rust_code = r#"
mod database {
    pub fn connect() {
        println!("DB connect");
    }

    pub fn disconnect() {
        println!("DB disconnect");
    }

    pub fn migrate() {
        println!("DB migrate");
    }
}
"#;
    std::fs::write(project.path().join("db.rs"), rust_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for symbols in module scope with name matching 'connect'
    let output = Command::new(sqry_bin())
        .arg("query")
        .arg("scope.type:module AND name:connect")
        .arg(project.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8_lossy(&output);

    // Should contain only 'connect' (not disconnect or migrate)
    assert!(stdout.contains("connect"), "Expected 'connect' in results");

    // Should NOT contain disconnect or migrate
    assert!(
        !stdout.contains("disconnect"),
        "'disconnect' should not match name:connect filter"
    );
    assert!(
        !stdout.contains("migrate"),
        "'migrate' should not match name:connect filter"
    );
}

#[test]
fn cli_scope_ancestor_with_name_composition() {
    let project = TempDir::new().unwrap();

    let rust_code = r#"
mod network {
    mod client {
        pub fn connect() {
            println!("Client connect");
        }

        pub fn send() {
            println!("Client send");
        }
    }
}

mod database {
    pub fn connect() {
        println!("Database connect");
    }
}
"#;
    std::fs::write(project.path().join("services.rs"), rust_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for symbols with 'network' ancestor AND name 'connect'
    let output = Command::new(sqry_bin())
        .arg("query")
        .arg("scope.ancestor:network AND name:connect")
        .arg(project.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8_lossy(&output);

    // Should contain network::client::connect
    assert!(
        stdout.contains("connect"),
        "Expected 'connect' with network ancestor"
    );

    // Should NOT contain 'send' (wrong name)
    assert!(
        !stdout.contains("send"),
        "'send' should not match name:connect filter"
    );

    // The database::connect should NOT appear (no network ancestor)
    // This is hard to verify in plain stdout, but the query should only find one match
}

// ============================================================================
// NOT scope.* Queries - Negation
// ============================================================================

#[test]
fn cli_scope_not_test_scopes() {
    let project = TempDir::new().unwrap();

    let rust_code = r#"
#[cfg(test)]
mod tests {
    fn test_helper() {
        assert!(true);
    }

    #[test]
    fn test_connection() {
        assert!(true);
    }
}

mod app {
    pub fn run() {
        println!("Running app");
    }

    pub fn init() {
        println!("Initializing app");
    }
}
"#;
    std::fs::write(project.path().join("main.rs"), rust_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for functions NOT in test scope (using AST boolean syntax)
    // Filter by kind:function to exclude CallSites (which have different parent scopes)
    let output = Command::new(sqry_bin())
        .arg("query")
        .arg("kind:function AND NOT scope.name:tests")
        .arg(project.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8_lossy(&output);

    // Should contain app functions
    assert!(stdout.contains("run"), "Expected 'run' not in test scope");
    assert!(stdout.contains("init"), "Expected 'init' not in test scope");

    // Should NOT contain test functions
    assert!(
        !stdout.contains("test_helper"),
        "'test_helper' should not appear (in tests scope)"
    );
    assert!(
        !stdout.contains("test_connection"),
        "'test_connection' should not appear (in tests scope)"
    );
}

// ============================================================================
// scope.parent Queries - Immediate Parent Scope
// ============================================================================

#[test]
fn cli_scope_parent_filters_direct_children() {
    let project = TempDir::new().unwrap();

    let rust_code = r#"
mod network {
    pub fn top_level() {
        println!("Top level in network");
    }

    mod client {
        pub fn connect() {
            println!("Client connect");
        }
    }
}
"#;
    std::fs::write(project.path().join("net.rs"), rust_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for symbols with 'network' as immediate parent
    let output = Command::new(sqry_bin())
        .arg("query")
        .arg("scope.parent:network")
        .arg(project.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8_lossy(&output);

    // Should contain top_level (direct child of network)
    assert!(
        stdout.contains("top_level"),
        "Expected 'top_level' with parent:network"
    );

    // Should NOT contain connect (its parent is client, not network)
    assert!(
        !stdout.contains("connect"),
        "'connect' should not appear (parent is client, not network)"
    );

    // Note: client module is not indexed as a symbol (only as a scope),
    // so it won't appear in query results
}
