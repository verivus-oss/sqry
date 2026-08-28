mod common;
use common::sqry_bin;

use assert_cmd::Command;
use std::fs;
use std::path::Path;
use tempfile::TempDir;

fn sqry_cmd() -> Command {
    let path = sqry_bin();
    let mut cmd = Command::new(path);
    cmd.env("NO_COLOR", "1");
    cmd
}

fn write_file(dir: &Path, name: &str, content: &str) {
    fs::create_dir_all(dir).unwrap();
    fs::write(dir.join(name), content).unwrap();
}

/// End-to-end smoke test: ensure `lang:` filters are accepted for all
/// built-in languages registered by the CLI in workspace mode.
///
/// This test does NOT assert specific symbols for every language; instead it
/// verifies that:
/// - each language has at least one indexed file in the workspace
/// - `sqry workspace query . "lang:<id>"` succeeds without parse/validation errors
///
/// This guards against regressions where the `lang` field validation or
/// workspace query path rejects certain language IDs.
#[test]
#[allow(clippy::too_many_lines)] // Tests all 37 language plugins end-to-end
fn workspace_lang_filter_smoke_for_all_languages() {
    let workspace_dir = TempDir::new().unwrap();
    let workspace_path = workspace_dir.path();

    // Initialize workspace once
    sqry_cmd()
        .args(["workspace", "init", workspace_path.to_str().unwrap()])
        .assert()
        .success();

    // Each entry: (language id, directory name, filename, file contents)
    let languages: &[(&str, &str, &str, &str)] = &[
        // General-purpose languages
        (
            "c",
            "c-service",
            "main.c",
            "int add(int a, int b) { return a + b; }\n",
        ),
        (
            "cpp",
            "cpp-service",
            "main.cpp",
            "int add(int a, int b) { return a + b; }\n",
        ),
        (
            "csharp",
            "csharp-service",
            "Program.cs",
            "class Program { static void Main() {} }\n",
        ),
        ("css", "css-service", "styles.css", "body { color: red; }\n"),
        (
            "dart",
            "dart-service",
            "main.dart",
            "int add(int a, int b) => a + b;\n",
        ),
        (
            "elixir",
            "elixir-service",
            "app.ex",
            "defmodule App do\n  def run, do: :ok\nend\n",
        ),
        (
            "go",
            "go-service",
            "main.go",
            "package main\n\nfunc main() {}\n",
        ),
        (
            "groovy",
            "groovy-service",
            "App.groovy",
            "class App { static void main(String[] args) { println 'hi' } }\n",
        ),
        (
            "haskell",
            "haskell-service",
            "Main.hs",
            "module Main where\n\nmain :: IO ()\nmain = pure ()\n",
        ),
        (
            "html",
            "html-service",
            "index.html",
            "<!doctype html><html><body><h1>Hi</h1></body></html>\n",
        ),
        (
            "java",
            "java-service",
            "App.java",
            "public class App { public static void main(String[] args) {} }\n",
        ),
        (
            "javascript",
            "javascript-service",
            "app.js",
            "function main() { console.log('hi'); }\nmain();\n",
        ),
        (
            "kotlin",
            "kotlin-service",
            "App.kt",
            "fun main() { println(\"hi\") }\n",
        ),
        (
            "lua",
            "lua-service",
            "app.lua",
            "local function main() end\nmain()\n",
        ),
        (
            "perl",
            "perl-service",
            "app.pl",
            "sub main { return 0; }\nmain();\n",
        ),
        (
            "php",
            "php-service",
            "app.php",
            "<?php function main() {} main();\n",
        ),
        (
            "python",
            "python-service",
            "app.py",
            "def main():\n    return 0\n\nmain()\n",
        ),
        (
            "r",
            "r-service",
            "app.r",
            "main <- function() { 1 }\nmain()\n",
        ),
        (
            "ruby",
            "ruby-service",
            "app.rb",
            "def main\n  puts 'hi'\nend\n\nmain\n",
        ),
        (
            "rust",
            "rust-service",
            "main.rs",
            "fn main() { println!(\"hi\"); }\n",
        ),
        (
            "scala",
            "scala-service",
            "App.scala",
            "object App { def main(args: Array[String]): Unit = () }\n",
        ),
        (
            "shell",
            "shell-service",
            "app.sh",
            "#!/usr/bin/env bash\nmain() { echo hi; }\nmain\n",
        ),
        (
            "sql",
            "sql-service",
            "schema.sql",
            "CREATE TABLE users(id INT);\n",
        ),
        (
            "svelte",
            "svelte-service",
            "App.svelte",
            "<script>let x = 1;</script><h1>{x}</h1>\n",
        ),
        (
            "swift",
            "swift-service",
            "main.swift",
            "func main() { print(\"hi\") }\nmain()\n",
        ),
        (
            "typescript",
            "typescript-service",
            "app.ts",
            "function main(): void { console.log('hi'); }\nmain();\n",
        ),
        (
            "vue",
            "vue-service",
            "App.vue",
            "<template><div>Hi</div></template><script>export default {}</script>\n",
        ),
        (
            "zig",
            "zig-service",
            "main.zig",
            "pub fn main() !void { }\n",
        ),
        // Domain-specific plugins
        (
            "plsql",
            "plsql-service",
            "package.pks",
            "CREATE OR REPLACE PACKAGE demo AS END demo;\n",
        ),
        (
            "apex",
            "apex-service",
            "Demo.cls",
            "public class Demo { public static void run() {} }\n",
        ),
        (
            "abap",
            "abap-service",
            "demo.abap",
            "REPORT z_demo.\nWRITE 'hi'.\n",
        ),
        (
            // `servicenow`, the Language id, NOT `servicenow-xanadu-js`, which
            // is a PLUGIN id. Both ServiceNow plugins (`servicenow-xanadu-js`
            // and `servicenow-xml`) produce `Language::ServiceNow`, so a
            // plugin-id filter would promise a distinction the graph cannot
            // make. This entry read `servicenow-xanadu-js` until issue #714
            // made `lang:` validate its value: before that, any string was
            // "accepted" here because it silently matched nothing.
            "servicenow",
            "servicenow-service",
            "script.snjs",
            "class App {}\nvar x = function() { return 1; };\n",
        ),
    ];

    // Index each language-specific repo and add it to the workspace
    for (lang_id, dir_name, file_name, contents) in languages {
        let repo_dir = workspace_path.join(dir_name);
        write_file(&repo_dir, file_name, contents);

        // Index repository
        sqry_cmd()
            .args(["index", repo_dir.to_str().unwrap()])
            .assert()
            .success();

        // Add to workspace
        sqry_cmd()
            .args([
                "workspace",
                "add",
                workspace_path.to_str().unwrap(),
                repo_dir.to_str().unwrap(),
            ])
            .assert()
            .success();

        // Sanity check: lang:<id> query must be accepted and not fail validation
        let query = format!("lang:{lang_id}");
        sqry_cmd()
            .args([
                "workspace",
                "query",
                workspace_path.to_str().unwrap(),
                &query,
            ])
            .assert()
            .success();
    }
}
