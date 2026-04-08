//! CLI integration tests for Ruby relation queries
//!
//! Tests that relation queries work end-to-end through the CLI for Ruby:
//! - Callers queries (method calls, blocks)
//! - Callees queries (what a method calls)
//! - Exports queries (classes, modules, methods)
//! - Imports queries (require statements)

mod common;
use common::sqry_bin;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

// ============================================================================
// Exports Queries - Classes, Modules, Methods
// ============================================================================

#[test]
fn cli_ruby_exports_classes_and_methods() {
    let project = TempDir::new().unwrap();

    let ruby_code = r#"
def greet(name)
  "Hello, #{name}!"
end

private def private_helper
  42
end

class User
  attr_reader :name, :age

  def initialize(name, age)
    @name = name
    @age = age
  end

  def get_name
    @name
  end

  private

  def validate
    !@name.empty?
  end
end

API_VERSION = "1.0.0"
"#;
    std::fs::write(project.path().join("module.rb"), ruby_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for exported function
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:greet")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("module.rb"));

    // Query for exported class
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:User")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("module.rb"));

    // Private function should NOT be exported
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:private_helper")
        .arg(project.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("No matches found"));
}

#[test]
fn cli_ruby_exports_modules() {
    let project = TempDir::new().unwrap();

    let ruby_code = r#"
module Utils
  def self.format(str)
    str.upcase
  end

  class Formatter
    def format(input)
      input.downcase
    end
  end
end

module API
  class Service
    def execute
      puts "Executing"
    end
  end
end
"#;
    std::fs::write(project.path().join("modules.rb"), ruby_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for module
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:Utils")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("modules.rb"));

    // Query for class in module
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:Formatter")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("modules.rb"));
}

// ============================================================================
// Callers Queries - Method Calls
// ============================================================================

#[test]
fn cli_ruby_callers_method_calls() {
    let project = TempDir::new().unwrap();

    let ruby_code = r#"
def validate(input)
  !input.nil? && !input.empty?
end

def process(data)
  if validate(data)
    data.strip
  else
    nil
  end
end

process("test")
"#;
    std::fs::write(project.path().join("processor.rb"), ruby_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of validate
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:validate")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("process"));
}

#[test]
fn cli_ruby_callers_blocks() {
    let project = TempDir::new().unwrap();

    let ruby_code = r"
def transform(x)
  x * 2
end

def process_data
  data = [1, 2, 3]
  data.map { |x| transform(x) }
end

process_data
";
    std::fs::write(project.path().join("blocks.rb"), ruby_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of transform (called in block)
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:transform")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("process_data"));
}

#[test]
fn cli_ruby_callers_supports_qualified_filters_and_output() {
    let project = TempDir::new().unwrap();

    let ruby_code = r#"
module Admin
  module Users
    class Controller
      def show
        render_view()
      end
    end
  end
end

module Dashboard
  class Controller
    def index
      Admin::Users::Controller.new.show
    end
  end
end

def render_view
  puts "rendering"
end

Dashboard::Controller.new.index
"#;
    std::fs::write(project.path().join("app.rb"), ruby_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Qualified display flag should surface the namespace-qualified caller
    Command::new(sqry_bin())
        .arg("query")
        .arg("--qualified-names")
        .arg("callers:render_view")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Admin::Users::Controller#show"));

    // JSON output includes caller_identity payload even without flag
    Command::new(sqry_bin())
        .arg("query")
        .arg("--json")
        .arg("callers:\"Admin::Users::Controller#show\"")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("\"caller_identity\""))
        .stdout(predicate::str::contains("Dashboard::Controller#index"));
}

// ============================================================================
// Callees Queries - What Methods Call
// ============================================================================

#[test]
fn cli_ruby_callees_method() {
    let project = TempDir::new().unwrap();

    let ruby_code = r#"
def log(message)
  puts message
end

def warn(message)
  puts "WARNING: #{message}"
end

def handle_error(error)
  log("Error occurred")
  warn(error.message)
end

handle_error(StandardError.new("Test"))
"#;
    std::fs::write(project.path().join("logger.rb"), ruby_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callees of handle_error
    Command::new(sqry_bin())
        .arg("query")
        .arg("callees:handle_error")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("log"))
        .stdout(predicate::str::contains("warn"));
}

#[test]
fn cli_ruby_callees_supports_qualified_output() {
    let project = TempDir::new().unwrap();

    let ruby_code = r#"
module Database
  class Connection
    def self.execute(sql)
      puts sql
    end

    def self.query(sql)
      puts sql
    end
  end
end

def process_user_data
  Database::Connection.execute("SELECT * FROM users")
  Database::Connection.query("SELECT COUNT(*) FROM users")
end

def bootstrap_database
  Database::Connection.execute("CREATE TABLE users")
end

process_user_data
bootstrap_database
"#;
    std::fs::write(project.path().join("app.rb"), ruby_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // First verify basic callees query works (returns simple names)
    Command::new(sqry_bin())
        .arg("query")
        .arg("callees:process_user_data")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("execute"))
        .stdout(predicate::str::contains("query"));

    // Qualified display flag should surface the namespace-qualified callee
    Command::new(sqry_bin())
        .arg("query")
        .arg("--qualified-names")
        .arg("callees:process_user_data")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Database::Connection"));

    // JSON output includes callee_identity payload
    Command::new(sqry_bin())
        .arg("query")
        .arg("--json")
        .arg("callees:bootstrap_database")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("\"callee_identity\""));
}

// ============================================================================
// Imports Queries - Require Statements
// ============================================================================

#[test]
fn cli_ruby_imports() {
    let project = TempDir::new().unwrap();

    let ruby_code = r"
require 'json'
require 'fileutils'
require_relative 'user'

def read_config
  file = File.read('config.json')
  JSON.parse(file)
end

read_config
";
    std::fs::write(project.path().join("config.rb"), ruby_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for requires
    Command::new(sqry_bin())
        .arg("query")
        .arg("imports:json")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("config.rb"));

    Command::new(sqry_bin())
        .arg("query")
        .arg("imports:fileutils")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("config.rb"));
}

// ============================================================================
// Negative Tests
// ============================================================================

#[test]
fn cli_ruby_private_methods() {
    let project = TempDir::new().unwrap();

    let ruby_code = r"
class Service
  def execute
    validate
  end

  private

  def validate
    # private method
  end
end
";
    std::fs::write(project.path().join("service.rb"), ruby_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of private method
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:validate")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("execute"));
}

#[test]
fn cli_ruby_callers_no_results() {
    let project = TempDir::new().unwrap();

    let ruby_code = r#"
def unused_function
  42
end

puts "Hello"
"#;
    std::fs::write(project.path().join("unused.rb"), ruby_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of unused function
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:unused_function")
        .arg(project.path())
        .assert()
        .success();
    // No specific assertion - just verify it doesn't crash
}
