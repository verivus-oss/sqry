//! CLI integration tests for Elixir relation queries
//!
//! Tests that relation queries work end-to-end through the CLI for Elixir:
//! - Callers queries (function calls, module-qualified calls, pipe operators)
//! - Callees queries (what a function calls)
//! - Exports queries (modules, public functions, macros, protocols)
//!
//! This validates the Elixir relation extraction implementation.

mod common;
use common::sqry_bin;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

// ============================================================================
// Callers Queries - Function Calls
// ============================================================================

#[test]
fn cli_elixir_callers_simple_function() {
    let project = TempDir::new().unwrap();

    let elixir_code = r#"
defmodule Demo do
  def helper do
    :ok
  end

  def main do
    helper()
    :done
  end
end
"#;
    std::fs::write(project.path().join("demo.ex"), elixir_code).unwrap();

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
fn cli_elixir_callers_module_qualified() {
    let project = TempDir::new().unwrap();

    let elixir_code = r#"
defmodule Utils do
  def log(message) do
    IO.puts(message)
  end
end

defmodule App do
  def process do
    Utils.log("processing")
  end
end
"#;
    std::fs::write(project.path().join("app.ex"), elixir_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of log
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:log")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("process"));
}

#[test]
fn cli_elixir_callers_pipe_operator() {
    let project = TempDir::new().unwrap();

    let elixir_code = r#"
defmodule Pipeline do
  def transform(data) do
    data |> String.upcase()
  end

  def validate(data) do
    String.length(data) > 0
  end

  def process(input) do
    input
    |> transform()
    |> validate()
  end
end
"#;
    std::fs::write(project.path().join("pipeline.ex"), elixir_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of transform
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:transform")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("process"));
}

#[test]
fn cli_elixir_callers_erlang_ffi() {
    let project = TempDir::new().unwrap();

    let elixir_code = r#"
defmodule ErlangFFI do
  def get_timestamp do
    :erlang.system_time(:second)
  end
end
"#;
    std::fs::write(project.path().join("ffi.ex"), elixir_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of erlang functions
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:system_time")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("get_timestamp"));
}

// ============================================================================
// Callees Queries - What Functions Call
// ============================================================================

#[test]
fn cli_elixir_callees_function() {
    let project = TempDir::new().unwrap();

    let elixir_code = r#"
defmodule Worker do
  def log(message), do: IO.puts(message)

  def validate(data), do: data != nil

  def process(input) do
    log("starting")
    result = validate(input)
    log("done")
    result
  end
end
"#;
    std::fs::write(project.path().join("worker.ex"), elixir_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for what process calls
    Command::new(sqry_bin())
        .arg("query")
        .arg("callees:process")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("log").and(predicate::str::contains("validate")));
}

// ============================================================================
// Integration Tests - Complex Scenarios
// ============================================================================

#[test]
fn cli_elixir_multi_file_calls() {
    let project = TempDir::new().unwrap();

    let utils_code = r#"
defmodule Utils do
  def log(message) do
    IO.puts("[LOG] #{message}")
  end

  def validate(input) do
    is_binary(input) && String.length(input) > 0
  end
end
"#;
    std::fs::write(project.path().join("utils.ex"), utils_code).unwrap();

    let main_code = r#"
defmodule Main do
  def process(data) do
    Utils.log("processing")
    Utils.validate(data)
  end
end
"#;
    std::fs::write(project.path().join("main.ex"), main_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of log across files
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:log")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("process"));
}

#[test]
fn cli_elixir_nested_module_calls() {
    let project = TempDir::new().unwrap();

    let elixir_code = r#"
defmodule Parent do
  defmodule Child do
    def helper, do: :ok
  end

  def use_child do
    Child.helper()
  end
end
"#;
    std::fs::write(project.path().join("nested.ex"), elixir_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of helper
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:helper")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("use_child"));
}

#[test]
fn cli_elixir_multiple_callers() {
    let project = TempDir::new().unwrap();

    let elixir_code = r#"
defmodule Logger do
  def log(message), do: IO.puts(message)

  def process do
    log("processing")
  end

  def main do
    log("starting")
    process()
  end
end
"#;
    std::fs::write(project.path().join("logger.ex"), elixir_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of log - should find both process and main
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:log")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("process").and(predicate::str::contains("main")));
}

// ============================================================================
// Exports Queries
// ============================================================================

#[test]
fn cli_elixir_exports_public_function() {
    let project = TempDir::new().unwrap();

    let elixir_code = r#"
defmodule Visibility do
  def public_fun do
    :ok
  end

  defp private_fun do
    :secret
  end
end
"#;
    std::fs::write(project.path().join("visibility.ex"), elixir_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:public_fun")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("public_fun"));
}

#[test]
fn cli_elixir_exports_hide_private_functions() {
    let project = TempDir::new().unwrap();

    let elixir_code = r#"
defmodule Secret do
  def public_fun, do: :ok
  defp private_fun, do: :secret
end
"#;
    std::fs::write(project.path().join("secret.ex"), elixir_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:private_fun")
        .arg(project.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("No matches found"));
}

#[test]
fn cli_elixir_exports_public_macro() {
    let project = TempDir::new().unwrap();

    let elixir_code = r#"
defmodule MacroModule do
  defmacro public_macro do
    quote do: :ok
  end

  defmacrop private_macro do
    quote do: :secret
  end
end
"#;
    std::fs::write(project.path().join("macros.ex"), elixir_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:public_macro")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("public_macro"));
}

#[test]
fn cli_elixir_exports_hide_private_macros() {
    let project = TempDir::new().unwrap();

    let elixir_code = r#"
defmodule MacroModule do
  defmacro public_macro, do: quote(do: :ok)
  defmacrop private_macro, do: quote(do: :secret)
end
"#;
    std::fs::write(project.path().join("macros.ex"), elixir_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:private_macro")
        .arg(project.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("No matches found"));
}

#[test]
fn cli_elixir_exports_mixed_functions_and_macros() {
    let project = TempDir::new().unwrap();

    let elixir_code = r#"
defmodule MixedModule do
  def public_fun, do: :ok
  defp private_fun, do: :secret
  defmacro public_macro, do: quote(do: :ok)
  defmacrop private_macro, do: quote(do: :secret)
end
"#;
    std::fs::write(project.path().join("mixed.ex"), elixir_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Should find public function
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:public_fun")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("public_fun"));

    // Should find public macro
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:public_macro")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("public_macro"));

    // Should NOT find private function
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:private_fun")
        .arg(project.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("No matches found"));

    // Should NOT find private macro
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:private_macro")
        .arg(project.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("No matches found"));
}
