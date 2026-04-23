//! Edit operators applied to fixture snapshots by the §E equivalence
//! harness.
//!
//! Each [`EditOp`] represents a small, localised change a user might make
//! to a file tree: adding or removing a function, importing a module,
//! creating or deleting a file, injecting invalid syntax, etc. The
//! operators are applied to a tempdir copy of a fixture before building
//! the graph, so the harness can verify that an incremental rebuild
//! against a mutated workspace is semantically equivalent to a full
//! rebuild of the same mutated workspace.
//!
//! Operators are **always safe to apply**. When preconditions are not
//! met (target file does not exist, symbol to rename not found, etc.),
//! [`EditOp::apply`] returns [`ApplyOutcome::NoOp`] rather than panicking.
//! This keeps the proptest shrinker honest: a shrunk-to-minimal case is
//! the simplest failing case, not one that panicked before the graph
//! build ran.
//!
//! # Per-fixture validity
//!
//! Not every operator makes sense for every fixture. The
//! [`FixtureSpec::supported_ops`] table declares which operator kinds a
//! fixture can legitimately run — for example, `AddExternBlock` is
//! meaningful in `multi_lang_ffi` but not `ts_http_routes`. The proptest
//! generator uses these tables to sample only meaningful operators.

#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};

use proptest::prelude::*;

/// Result of applying an edit operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyOutcome {
    /// The edit mutated the filesystem.
    Applied,
    /// The edit was legal but its preconditions were not met (e.g. target
    /// file missing, symbol not found). The filesystem is unchanged.
    NoOp,
}

/// The twelve edit operators the §E harness supports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditOp {
    /// Append a new function definition (language-appropriate syntax) to
    /// an existing source file.
    AddFunction {
        /// Relative path (from the fixture root) of the file to modify.
        rel_path: String,
        /// Name of the new function.
        name: String,
    },

    /// Remove the first function definition matching `name` from a file,
    /// by deleting the enclosing source block. Falls back to [`NoOp`] if
    /// the function cannot be located textually.
    RemoveFunction {
        /// Relative path of the file.
        rel_path: String,
        /// Name of the function to remove.
        name: String,
    },

    /// Rename the first textual occurrence of `old` to `new` in a file.
    RenameSymbol {
        /// Relative path of the file.
        rel_path: String,
        /// Original identifier.
        old: String,
        /// Replacement identifier.
        new: String,
    },

    /// Insert an import statement at the top of a file.
    AddImport {
        /// Relative path of the file.
        rel_path: String,
        /// Full import statement (including terminator / newline).
        statement: String,
    },

    /// Remove the first line containing `needle` from a file.
    RemoveImport {
        /// Relative path of the file.
        rel_path: String,
        /// Substring that identifies the import line to remove.
        needle: String,
    },

    /// Append a Rust `extern "C"` block declaring a new FFI symbol.
    AddExternBlock {
        /// Relative path of the `.rs` file to modify.
        rel_path: String,
        /// Name of the declared FFI function.
        symbol: String,
    },

    /// Append an HTTP route handler to a TypeScript / Python server file.
    AddHttpRoute {
        /// Relative path of the server file.
        rel_path: String,
        /// HTTP method (`"GET"`, `"POST"`, etc.).
        method: String,
        /// Route pattern (e.g. `/api/status`).
        path: String,
        /// Handler function name.
        handler: String,
    },

    /// Create a new file at `rel_path` with the given content.
    AddFile {
        /// Relative path to create.
        rel_path: String,
        /// Content to write to the new file.
        content: String,
    },

    /// Remove a file from the fixture.
    RemoveFile {
        /// Relative path to delete.
        rel_path: String,
    },

    /// Rename a file from `old_rel_path` to `new_rel_path`. The new path
    /// must be inside the fixture root; its extension should match so the
    /// file stays plugin-routable.
    RenameFile {
        /// Original relative path.
        old_rel_path: String,
        /// New relative path.
        new_rel_path: String,
    },

    /// Append whitespace to a file. Should not change graph semantics —
    /// included to exercise "non-semantic edits still round-trip" paths.
    WhitespaceEdit {
        /// Relative path of the file.
        rel_path: String,
    },

    /// Append a deliberately invalid syntax snippet to a file. Both the
    /// baseline and the incremental build must handle this identically
    /// (typically: the plugin parses as much as it can and skips the
    /// trailing garbage).
    InvalidSyntaxEdit {
        /// Relative path of the file.
        rel_path: String,
    },
}

/// Variant tag used for fixture/operator validity declarations and for
/// proptest sampling. One-to-one with [`EditOp`] variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditOpKind {
    AddFunction,
    RemoveFunction,
    RenameSymbol,
    AddImport,
    RemoveImport,
    AddExternBlock,
    AddHttpRoute,
    AddFile,
    RemoveFile,
    RenameFile,
    WhitespaceEdit,
    InvalidSyntaxEdit,
}

/// Source file descriptor inside a fixture.
#[derive(Debug, Clone, Copy)]
pub struct FixtureFile {
    /// Path relative to the fixture root.
    pub rel_path: &'static str,
    /// Known function identifiers defined in this file (used by
    /// `RemoveFunction` / `RenameSymbol`).
    pub known_symbols: &'static [&'static str],
    /// Known import-line substrings (used by `RemoveImport`).
    pub known_import_needles: &'static [&'static str],
}

/// Per-fixture validity declaration: which operators make sense, what
/// files exist, and what identifiers are safe to use.
#[derive(Debug, Clone, Copy)]
pub struct FixtureSpec {
    /// Fixture directory name under `sqry-core/tests/fixtures/incremental/`.
    pub name: &'static str,
    /// File descriptors for every file in the fixture.
    pub files: &'static [FixtureFile],
    /// Operator kinds allowed for this fixture.
    pub supported_ops: &'static [EditOpKind],
    /// Directory (relative to fixture root) and extension used when
    /// generating `AddFile` operators. Matches the fixture's dominant
    /// language so new files stay plugin-routable.
    pub add_file_stub: AddFileStub,
    /// Relative path accepted by `AddExternBlock` (empty if unsupported).
    pub extern_block_target: Option<&'static str>,
    /// Relative path accepted by `AddHttpRoute` (empty if unsupported).
    pub http_route_target: Option<&'static str>,
}

/// Template for `AddFile` generation — controls which directory new files
/// are written into and what syntax they must obey.
#[derive(Debug, Clone, Copy)]
pub struct AddFileStub {
    /// Directory (relative to fixture root) to place generated files in.
    pub dir: &'static str,
    /// File extension (no leading dot).
    pub extension: &'static str,
    /// Starter content — must parse cleanly.
    pub content_template: &'static str,
}

/// The five fixtures shipped by Task 4 Gate 0a. Kept in a single array so
/// the proptest generator can sample uniformly and the harness can
/// enumerate fixtures for the CI gate.
pub const FIXTURES: &[FixtureSpec] = &[
    // -------------------- rust_small --------------------
    FixtureSpec {
        name: "rust_small",
        files: &[
            FixtureFile {
                rel_path: "lib.rs",
                known_symbols: &[],
                known_import_needles: &["pub use math::add"],
            },
            FixtureFile {
                rel_path: "math.rs",
                known_symbols: &["add", "multiply", "square"],
                known_import_needles: &[],
            },
            FixtureFile {
                rel_path: "shapes.rs",
                known_symbols: &["new", "distance_squared_from_origin"],
                known_import_needles: &["use crate::math::square"],
            },
            FixtureFile {
                rel_path: "util.rs",
                known_symbols: &["format_point", "summarize"],
                known_import_needles: &["use crate::shapes::Point"],
            },
            FixtureFile {
                rel_path: "main.rs",
                known_symbols: &["main"],
                known_import_needles: &[
                    "use rust_small::math::add",
                    "use rust_small::shapes::Point",
                ],
            },
        ],
        supported_ops: &[
            EditOpKind::AddFunction,
            EditOpKind::RemoveFunction,
            EditOpKind::RenameSymbol,
            EditOpKind::AddImport,
            EditOpKind::RemoveImport,
            EditOpKind::AddExternBlock,
            EditOpKind::AddFile,
            EditOpKind::RemoveFile,
            EditOpKind::RenameFile,
            EditOpKind::WhitespaceEdit,
            EditOpKind::InvalidSyntaxEdit,
        ],
        add_file_stub: AddFileStub {
            dir: "",
            extension: "rs",
            content_template: "pub fn __harness_stub() -> i32 { 0 }\n",
        },
        extern_block_target: Some("main.rs"),
        http_route_target: None,
    },
    // -------------------- multi_lang_ffi --------------------
    FixtureSpec {
        name: "multi_lang_ffi",
        files: &[
            FixtureFile {
                rel_path: "caller.rs",
                known_symbols: &["call_add", "call_multiply", "call_format_status"],
                known_import_needles: &[],
            },
            FixtureFile {
                rel_path: "driver.rs",
                known_symbols: &["run_arithmetic"],
                known_import_needles: &["use crate::caller"],
            },
            FixtureFile {
                rel_path: "native_math.c",
                known_symbols: &["native_add", "native_multiply"],
                known_import_needles: &[],
            },
            FixtureFile {
                rel_path: "native_util.c",
                known_symbols: &["native_format_status"],
                known_import_needles: &[],
            },
            FixtureFile {
                rel_path: "native_support.c",
                known_symbols: &["native_abs", "native_clamp"],
                known_import_needles: &[],
            },
        ],
        supported_ops: &[
            EditOpKind::AddFunction,
            EditOpKind::RemoveFunction,
            EditOpKind::RenameSymbol,
            EditOpKind::AddImport,
            EditOpKind::AddExternBlock,
            EditOpKind::AddFile,
            EditOpKind::RemoveFile,
            EditOpKind::RenameFile,
            EditOpKind::WhitespaceEdit,
            EditOpKind::InvalidSyntaxEdit,
        ],
        add_file_stub: AddFileStub {
            dir: "",
            extension: "rs",
            content_template: "pub fn __harness_stub() -> i32 { 0 }\n",
        },
        extern_block_target: Some("driver.rs"),
        http_route_target: None,
    },
    // -------------------- ts_http_routes --------------------
    FixtureSpec {
        name: "ts_http_routes",
        files: &[
            FixtureFile {
                rel_path: "client.ts",
                known_symbols: &["fetchUsers", "createUser", "deleteUser", "sendWebhook"],
                known_import_needles: &["import type { User, WebhookPayload }"],
            },
            FixtureFile {
                rel_path: "server.ts",
                known_symbols: &[],
                known_import_needles: &["import express", "import type { User }"],
            },
            FixtureFile {
                rel_path: "webhook.ts",
                known_symbols: &[],
                known_import_needles: &["import express", "import type { WebhookPayload }"],
            },
            FixtureFile {
                rel_path: "types.ts",
                known_symbols: &[],
                known_import_needles: &[],
            },
            FixtureFile {
                rel_path: "helpers.ts",
                known_symbols: &["formatUser", "sortUsers"],
                known_import_needles: &["import type { User }"],
            },
        ],
        supported_ops: &[
            EditOpKind::AddFunction,
            EditOpKind::RemoveFunction,
            EditOpKind::RenameSymbol,
            EditOpKind::AddImport,
            EditOpKind::RemoveImport,
            EditOpKind::AddHttpRoute,
            EditOpKind::AddFile,
            EditOpKind::RemoveFile,
            EditOpKind::RenameFile,
            EditOpKind::WhitespaceEdit,
            EditOpKind::InvalidSyntaxEdit,
        ],
        add_file_stub: AddFileStub {
            dir: "",
            extension: "ts",
            content_template: "export function __harness_stub(): number { return 0; }\n",
        },
        extern_block_target: None,
        http_route_target: Some("server.ts"),
    },
    // -------------------- java_enterprise --------------------
    FixtureSpec {
        name: "java_enterprise",
        files: &[
            FixtureFile {
                rel_path: "User.java",
                known_symbols: &["getId", "getName", "getEmail"],
                known_import_needles: &[],
            },
            FixtureFile {
                rel_path: "UserRepository.java",
                known_symbols: &["findAll", "findById", "save", "deleteById"],
                known_import_needles: &["import java.util.List", "import java.util.Optional"],
            },
            FixtureFile {
                rel_path: "InMemoryUserRepository.java",
                known_symbols: &["findAll", "findById", "save", "deleteById"],
                known_import_needles: &["import java.util.ArrayList", "import java.util.HashMap"],
            },
            FixtureFile {
                rel_path: "UserService.java",
                known_symbols: &["listUsers", "getUser", "createUser", "removeUser"],
                known_import_needles: &["import java.util.List", "import java.util.Optional"],
            },
            FixtureFile {
                rel_path: "Application.java",
                known_symbols: &["main"],
                known_import_needles: &[],
            },
        ],
        supported_ops: &[
            EditOpKind::AddFunction,
            EditOpKind::RemoveFunction,
            EditOpKind::RenameSymbol,
            EditOpKind::AddImport,
            EditOpKind::RemoveImport,
            EditOpKind::AddFile,
            EditOpKind::RemoveFile,
            EditOpKind::RenameFile,
            EditOpKind::WhitespaceEdit,
            EditOpKind::InvalidSyntaxEdit,
        ],
        add_file_stub: AddFileStub {
            dir: "",
            extension: "java",
            content_template: "package com.example.enterprise;\n\npublic class HarnessStub {\n    public int zero() { return 0; }\n}\n",
        },
        extern_block_target: None,
        http_route_target: None,
    },
    // -------------------- monorepo_mixed --------------------
    FixtureSpec {
        name: "monorepo_mixed",
        files: &[
            FixtureFile {
                rel_path: "backend/server.py",
                known_symbols: &["list_items", "add_item", "remove_item"],
                known_import_needles: &["from flask", "from backend.store"],
            },
            FixtureFile {
                rel_path: "backend/store.py",
                known_symbols: &["all", "add", "remove"],
                known_import_needles: &["from typing"],
            },
            FixtureFile {
                rel_path: "frontend/client.js",
                known_symbols: &["listItems", "addItem", "removeItem"],
                known_import_needles: &[],
            },
            FixtureFile {
                rel_path: "frontend/ui.js",
                known_symbols: &["refreshList", "submit"],
                known_import_needles: &["require(\"./client\")"],
            },
            FixtureFile {
                rel_path: "shared/types.ts",
                known_symbols: &[],
                known_import_needles: &[],
            },
        ],
        supported_ops: &[
            EditOpKind::AddFunction,
            EditOpKind::RemoveFunction,
            EditOpKind::RenameSymbol,
            EditOpKind::AddImport,
            EditOpKind::RemoveImport,
            EditOpKind::AddHttpRoute,
            EditOpKind::AddFile,
            EditOpKind::RemoveFile,
            EditOpKind::RenameFile,
            EditOpKind::WhitespaceEdit,
            EditOpKind::InvalidSyntaxEdit,
        ],
        add_file_stub: AddFileStub {
            dir: "shared",
            extension: "ts",
            content_template: "export function harnessStub(): number { return 0; }\n",
        },
        extern_block_target: None,
        http_route_target: Some("backend/server.py"),
    },
];

/// Fetch a fixture spec by name. Panics when `name` is unknown so proptest
/// cannot silently drop cases against a misspelled fixture.
#[must_use]
pub fn fixture_spec(name: &str) -> &'static FixtureSpec {
    FIXTURES
        .iter()
        .find(|spec| spec.name == name)
        .unwrap_or_else(|| panic!("unknown fixture: {name}"))
}

/// Copy the source fixture into `dest`. Recursively walks the fixture
/// directory. Used at the start of every proptest case to materialize an
/// isolated workspace.
pub fn copy_fixture(fixture_name: &str, source_root: &Path, dest: &Path) -> std::io::Result<()> {
    let source = source_root.join(fixture_name);
    copy_dir_recursive(&source, dest)
}

fn copy_dir_recursive(source: &Path, dest: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dest)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let dest_path = dest.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_recursive(&entry.path(), &dest_path)?;
        } else if file_type.is_file() {
            fs::copy(entry.path(), dest_path)?;
        }
    }
    Ok(())
}

impl EditOp {
    /// Return the tag of this operator — useful for statistics and
    /// proptest rejection messages.
    #[must_use]
    pub fn kind(&self) -> EditOpKind {
        match self {
            Self::AddFunction { .. } => EditOpKind::AddFunction,
            Self::RemoveFunction { .. } => EditOpKind::RemoveFunction,
            Self::RenameSymbol { .. } => EditOpKind::RenameSymbol,
            Self::AddImport { .. } => EditOpKind::AddImport,
            Self::RemoveImport { .. } => EditOpKind::RemoveImport,
            Self::AddExternBlock { .. } => EditOpKind::AddExternBlock,
            Self::AddHttpRoute { .. } => EditOpKind::AddHttpRoute,
            Self::AddFile { .. } => EditOpKind::AddFile,
            Self::RemoveFile { .. } => EditOpKind::RemoveFile,
            Self::RenameFile { .. } => EditOpKind::RenameFile,
            Self::WhitespaceEdit { .. } => EditOpKind::WhitespaceEdit,
            Self::InvalidSyntaxEdit { .. } => EditOpKind::InvalidSyntaxEdit,
        }
    }

    /// Apply this operator to a workspace rooted at `workspace`. Returns
    /// [`ApplyOutcome::NoOp`] if preconditions aren't satisfied (target
    /// missing, symbol absent, etc.).
    ///
    /// # Errors
    ///
    /// Returns `io::Error` only for unexpected filesystem failures (disk
    /// full, permission denied). Logical preconditions always resolve
    /// through `NoOp` instead.
    pub fn apply(&self, workspace: &Path) -> std::io::Result<ApplyOutcome> {
        match self {
            Self::AddFunction { rel_path, name } => apply_add_function(workspace, rel_path, name),
            Self::RemoveFunction { rel_path, name } => {
                apply_remove_function(workspace, rel_path, name)
            }
            Self::RenameSymbol { rel_path, old, new } => {
                apply_rename_symbol(workspace, rel_path, old, new)
            }
            Self::AddImport {
                rel_path,
                statement,
            } => apply_add_import(workspace, rel_path, statement),
            Self::RemoveImport { rel_path, needle } => {
                apply_remove_import(workspace, rel_path, needle)
            }
            Self::AddExternBlock { rel_path, symbol } => {
                apply_add_extern_block(workspace, rel_path, symbol)
            }
            Self::AddHttpRoute {
                rel_path,
                method,
                path,
                handler,
            } => apply_add_http_route(workspace, rel_path, method, path, handler),
            Self::AddFile { rel_path, content } => apply_add_file(workspace, rel_path, content),
            Self::RemoveFile { rel_path } => apply_remove_file(workspace, rel_path),
            Self::RenameFile {
                old_rel_path,
                new_rel_path,
            } => apply_rename_file(workspace, old_rel_path, new_rel_path),
            Self::WhitespaceEdit { rel_path } => apply_whitespace(workspace, rel_path),
            Self::InvalidSyntaxEdit { rel_path } => apply_invalid_syntax(workspace, rel_path),
        }
    }
}

// ================================================================
// Apply helpers
// ================================================================

fn safe_join(workspace: &Path, rel: &str) -> Option<PathBuf> {
    // Prevent `..` traversal or absolute paths from escaping the
    // workspace. Defensive — proptest generators produce static strings,
    // but the apply API is public and could be misused.
    let candidate = workspace.join(rel);
    let canonical_workspace = workspace.canonicalize().ok()?;
    let canonical_candidate = candidate
        .canonicalize()
        .ok()
        .or_else(|| candidate.parent()?.canonicalize().ok());
    let canonical_candidate = canonical_candidate?;
    if canonical_candidate.starts_with(&canonical_workspace) {
        Some(candidate)
    } else {
        None
    }
}

fn read_if_exists(path: &Path) -> std::io::Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(content) => Ok(Some(content)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

fn extension_for(path: &Path) -> &str {
    path.extension().and_then(|s| s.to_str()).unwrap_or("")
}

fn apply_add_function(
    workspace: &Path,
    rel_path: &str,
    name: &str,
) -> std::io::Result<ApplyOutcome> {
    let Some(target) = safe_join(workspace, rel_path) else {
        return Ok(ApplyOutcome::NoOp);
    };
    let Some(mut content) = read_if_exists(&target)? else {
        return Ok(ApplyOutcome::NoOp);
    };
    if !content.ends_with('\n') {
        content.push('\n');
    }
    let snippet = match extension_for(&target) {
        "rs" => format!("\npub fn {name}() -> i32 {{ 0 }}\n"),
        "c" => format!("\nint {name}(void) {{ return 0; }}\n"),
        "ts" | "js" => format!("\nexport function {name}() {{ return 0; }}\n"),
        "py" => format!("\n\ndef {name}():\n    return 0\n"),
        "java" => format!("\n    public int {name}() {{ return 0; }}\n"),
        _ => return Ok(ApplyOutcome::NoOp),
    };
    content.push_str(&snippet);
    fs::write(&target, content)?;
    Ok(ApplyOutcome::Applied)
}

fn apply_remove_function(
    workspace: &Path,
    rel_path: &str,
    name: &str,
) -> std::io::Result<ApplyOutcome> {
    let Some(target) = safe_join(workspace, rel_path) else {
        return Ok(ApplyOutcome::NoOp);
    };
    let Some(content) = read_if_exists(&target)? else {
        return Ok(ApplyOutcome::NoOp);
    };
    // Naive pattern: find a line that declares the symbol, delete to the
    // matching closing `}` or end-of-block. Conservative — for fixtures
    // we only need to remove simple function bodies.
    let ext = extension_for(&target);
    let stripped = match ext {
        "rs" | "c" | "ts" | "js" | "java" => remove_braced_block(&content, name),
        "py" => remove_python_def(&content, name),
        _ => None,
    };
    match stripped {
        Some(updated) if updated != content => {
            fs::write(&target, updated)?;
            Ok(ApplyOutcome::Applied)
        }
        _ => Ok(ApplyOutcome::NoOp),
    }
}

fn remove_braced_block(content: &str, name: &str) -> Option<String> {
    // Find "fn name"/"int name"/"function name" etc. We look for any
    // occurrence of the name followed by `(` and then a matching `{`
    // block. Minimal — just enough to remove the simple fixture helpers.
    let needle = format!(" {name}(");
    let start = content.find(&needle)?;
    // Walk back to the start of the line.
    let line_start = content[..start].rfind('\n').map_or(0, |i| i + 1);
    // Find the opening brace after the parens.
    let brace = content[start..].find('{')?;
    let brace_abs = start + brace;
    let mut depth = 0_i32;
    let bytes = content.as_bytes();
    let mut end_exclusive = bytes.len();
    for (i, &b) in bytes.iter().enumerate().skip(brace_abs) {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    end_exclusive = i + 1;
                    break;
                }
            }
            _ => {}
        }
    }
    // Trim a trailing newline if one exists to avoid leaving blank lines.
    let after = if end_exclusive < bytes.len() && bytes[end_exclusive] == b'\n' {
        end_exclusive + 1
    } else {
        end_exclusive
    };
    let mut out = String::with_capacity(content.len());
    out.push_str(&content[..line_start]);
    out.push_str(&content[after..]);
    Some(out)
}

fn remove_python_def(content: &str, name: &str) -> Option<String> {
    let needle = format!("def {name}(");
    let start = content.find(&needle)?;
    let line_start = content[..start].rfind('\n').map_or(0, |i| i + 1);
    // Consume lines until a line that is not indented (or end-of-file).
    let rest = &content[line_start..];
    let mut lines = rest.split_inclusive('\n');
    let mut consumed = 0_usize;
    // First line is the def.
    if let Some(first) = lines.next() {
        consumed += first.len();
    } else {
        return None;
    }
    for line in lines {
        let trimmed_left = line.trim_start();
        if trimmed_left.is_empty() || line.starts_with(char::is_whitespace) {
            consumed += line.len();
        } else {
            break;
        }
    }
    let end_exclusive = line_start + consumed;
    let mut out = String::with_capacity(content.len());
    out.push_str(&content[..line_start]);
    out.push_str(&content[end_exclusive..]);
    Some(out)
}

fn apply_rename_symbol(
    workspace: &Path,
    rel_path: &str,
    old: &str,
    new: &str,
) -> std::io::Result<ApplyOutcome> {
    if old == new || old.is_empty() || new.is_empty() {
        return Ok(ApplyOutcome::NoOp);
    }
    let Some(target) = safe_join(workspace, rel_path) else {
        return Ok(ApplyOutcome::NoOp);
    };
    let Some(content) = read_if_exists(&target)? else {
        return Ok(ApplyOutcome::NoOp);
    };
    let Some(position) = content.find(old) else {
        return Ok(ApplyOutcome::NoOp);
    };
    let mut updated = String::with_capacity(content.len() - old.len() + new.len());
    updated.push_str(&content[..position]);
    updated.push_str(new);
    updated.push_str(&content[position + old.len()..]);
    fs::write(&target, updated)?;
    Ok(ApplyOutcome::Applied)
}

fn apply_add_import(
    workspace: &Path,
    rel_path: &str,
    statement: &str,
) -> std::io::Result<ApplyOutcome> {
    let Some(target) = safe_join(workspace, rel_path) else {
        return Ok(ApplyOutcome::NoOp);
    };
    let Some(content) = read_if_exists(&target)? else {
        return Ok(ApplyOutcome::NoOp);
    };
    let mut line = statement.trim_end().to_string();
    line.push('\n');
    let updated = format!("{line}{content}");
    fs::write(&target, updated)?;
    Ok(ApplyOutcome::Applied)
}

fn apply_remove_import(
    workspace: &Path,
    rel_path: &str,
    needle: &str,
) -> std::io::Result<ApplyOutcome> {
    if needle.is_empty() {
        return Ok(ApplyOutcome::NoOp);
    }
    let Some(target) = safe_join(workspace, rel_path) else {
        return Ok(ApplyOutcome::NoOp);
    };
    let Some(content) = read_if_exists(&target)? else {
        return Ok(ApplyOutcome::NoOp);
    };
    let Some(idx) = content.find(needle) else {
        return Ok(ApplyOutcome::NoOp);
    };
    let line_start = content[..idx].rfind('\n').map_or(0, |i| i + 1);
    let line_end = content[idx..]
        .find('\n')
        .map_or(content.len(), |j| idx + j + 1);
    let mut out = String::with_capacity(content.len());
    out.push_str(&content[..line_start]);
    out.push_str(&content[line_end..]);
    fs::write(&target, out)?;
    Ok(ApplyOutcome::Applied)
}

fn apply_add_extern_block(
    workspace: &Path,
    rel_path: &str,
    symbol: &str,
) -> std::io::Result<ApplyOutcome> {
    let Some(target) = safe_join(workspace, rel_path) else {
        return Ok(ApplyOutcome::NoOp);
    };
    if extension_for(&target) != "rs" {
        return Ok(ApplyOutcome::NoOp);
    }
    let Some(mut content) = read_if_exists(&target)? else {
        return Ok(ApplyOutcome::NoOp);
    };
    if !content.ends_with('\n') {
        content.push('\n');
    }
    content.push_str(&format!(
        "\nunsafe extern \"C\" {{\n    fn {symbol}(value: i32) -> i32;\n}}\n"
    ));
    fs::write(&target, content)?;
    Ok(ApplyOutcome::Applied)
}

fn apply_add_http_route(
    workspace: &Path,
    rel_path: &str,
    method: &str,
    path: &str,
    handler: &str,
) -> std::io::Result<ApplyOutcome> {
    let Some(target) = safe_join(workspace, rel_path) else {
        return Ok(ApplyOutcome::NoOp);
    };
    let Some(mut content) = read_if_exists(&target)? else {
        return Ok(ApplyOutcome::NoOp);
    };
    if !content.ends_with('\n') {
        content.push('\n');
    }
    let snippet = match extension_for(&target) {
        "ts" | "js" => format!(
            "\napp.{method}(\"{path}\", (_req, res) => {{ res.json({{ handler: \"{handler}\" }}); }});\n",
            method = method.to_lowercase(),
        ),
        "py" => format!(
            "\n@app.route(\"{path}\", methods=[\"{method}\"])\ndef {handler}():\n    return {{\"handler\": \"{handler}\"}}\n",
        ),
        _ => return Ok(ApplyOutcome::NoOp),
    };
    content.push_str(&snippet);
    fs::write(&target, content)?;
    Ok(ApplyOutcome::Applied)
}

fn apply_add_file(
    workspace: &Path,
    rel_path: &str,
    content: &str,
) -> std::io::Result<ApplyOutcome> {
    let Some(target) = safe_join(workspace, rel_path) else {
        return Ok(ApplyOutcome::NoOp);
    };
    if target.exists() {
        return Ok(ApplyOutcome::NoOp);
    }
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&target, content)?;
    Ok(ApplyOutcome::Applied)
}

fn apply_remove_file(workspace: &Path, rel_path: &str) -> std::io::Result<ApplyOutcome> {
    let Some(target) = safe_join(workspace, rel_path) else {
        return Ok(ApplyOutcome::NoOp);
    };
    if !target.exists() {
        return Ok(ApplyOutcome::NoOp);
    }
    fs::remove_file(&target)?;
    Ok(ApplyOutcome::Applied)
}

fn apply_rename_file(
    workspace: &Path,
    old_rel: &str,
    new_rel: &str,
) -> std::io::Result<ApplyOutcome> {
    if old_rel == new_rel {
        return Ok(ApplyOutcome::NoOp);
    }
    let Some(old_path) = safe_join(workspace, old_rel) else {
        return Ok(ApplyOutcome::NoOp);
    };
    let Some(new_path) = safe_join(workspace, new_rel) else {
        return Ok(ApplyOutcome::NoOp);
    };
    if !old_path.exists() || new_path.exists() {
        return Ok(ApplyOutcome::NoOp);
    }
    if let Some(parent) = new_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::rename(&old_path, &new_path)?;
    Ok(ApplyOutcome::Applied)
}

fn apply_whitespace(workspace: &Path, rel_path: &str) -> std::io::Result<ApplyOutcome> {
    let Some(target) = safe_join(workspace, rel_path) else {
        return Ok(ApplyOutcome::NoOp);
    };
    let Some(mut content) = read_if_exists(&target)? else {
        return Ok(ApplyOutcome::NoOp);
    };
    // Trailing blank line — never changes parser behaviour.
    content.push('\n');
    fs::write(&target, content)?;
    Ok(ApplyOutcome::Applied)
}

fn apply_invalid_syntax(workspace: &Path, rel_path: &str) -> std::io::Result<ApplyOutcome> {
    let Some(target) = safe_join(workspace, rel_path) else {
        return Ok(ApplyOutcome::NoOp);
    };
    let Some(mut content) = read_if_exists(&target)? else {
        return Ok(ApplyOutcome::NoOp);
    };
    // Syntax garbage that every plugin we ship will reject. Avoids
    // accidentally being meaningful in some language by mixing
    // non-identifier characters together.
    content.push_str("\n@@@ <<< >>> !!!{{ }}\n");
    fs::write(&target, content)?;
    Ok(ApplyOutcome::Applied)
}

// ================================================================
// Proptest strategies
// ================================================================

/// Build a proptest strategy that samples a single [`EditOp`] valid for
/// the given fixture. The strategy restricts operator kinds to the
/// fixture's `supported_ops` list and draws parameters from the fixture's
/// known file and symbol sets.
pub fn any_edit_op(spec: &'static FixtureSpec) -> BoxedStrategy<EditOp> {
    let mut branches: Vec<BoxedStrategy<EditOp>> = Vec::new();

    for &kind in spec.supported_ops {
        if let Some(strategy) = strategy_for(kind, spec) {
            branches.push(strategy);
        }
    }

    assert!(
        !branches.is_empty(),
        "fixture {} has no supported operators with strategies",
        spec.name
    );

    proptest::strategy::Union::new(branches).boxed()
}

/// Build a strategy sampling a sequence of 1..=`max_len` edit operators,
/// all valid for the given fixture.
pub fn any_edit_sequence(spec: &'static FixtureSpec, max_len: usize) -> BoxedStrategy<Vec<EditOp>> {
    proptest::collection::vec(any_edit_op(spec), 1..=max_len).boxed()
}

/// Build a strategy sampling one of the five shipped fixtures.
pub fn any_fixture() -> BoxedStrategy<&'static FixtureSpec> {
    let indices = 0_usize..FIXTURES.len();
    indices.prop_map(|i| &FIXTURES[i]).boxed()
}

fn strategy_for(kind: EditOpKind, spec: &'static FixtureSpec) -> Option<BoxedStrategy<EditOp>> {
    match kind {
        EditOpKind::AddFunction => {
            let files: Vec<&'static str> = spec.files.iter().map(|f| f.rel_path).collect();
            if files.is_empty() {
                return None;
            }
            Some(
                (0_usize..files.len(), 0_u32..32)
                    .prop_map(move |(i, n)| EditOp::AddFunction {
                        rel_path: files[i].to_string(),
                        name: format!("harness_new_fn_{n}"),
                    })
                    .boxed(),
            )
        }
        EditOpKind::RemoveFunction => {
            let targets: Vec<(&'static str, &'static str)> = spec
                .files
                .iter()
                .flat_map(|f| f.known_symbols.iter().map(move |s| (f.rel_path, *s)))
                .collect();
            if targets.is_empty() {
                return None;
            }
            Some(
                (0_usize..targets.len())
                    .prop_map(move |i| {
                        let (rel, name) = targets[i];
                        EditOp::RemoveFunction {
                            rel_path: rel.to_string(),
                            name: name.to_string(),
                        }
                    })
                    .boxed(),
            )
        }
        EditOpKind::RenameSymbol => {
            let targets: Vec<(&'static str, &'static str)> = spec
                .files
                .iter()
                .flat_map(|f| f.known_symbols.iter().map(move |s| (f.rel_path, *s)))
                .collect();
            if targets.is_empty() {
                return None;
            }
            Some(
                (0_usize..targets.len(), 0_u32..32)
                    .prop_map(move |(i, n)| {
                        let (rel, name) = targets[i];
                        EditOp::RenameSymbol {
                            rel_path: rel.to_string(),
                            old: name.to_string(),
                            new: format!("{name}_renamed_{n}"),
                        }
                    })
                    .boxed(),
            )
        }
        EditOpKind::AddImport => {
            let files: Vec<&'static str> = spec.files.iter().map(|f| f.rel_path).collect();
            if files.is_empty() {
                return None;
            }
            Some(
                (0_usize..files.len())
                    .prop_map(move |i| {
                        let rel = files[i];
                        let statement = match extension_of(rel) {
                            "rs" => "#[allow(unused_imports)]\nuse std::collections::HashMap;\n"
                                .to_string(),
                            "ts" | "js" => {
                                "import { __harness_nop } from \"./__harness_missing\";\n"
                                    .to_string()
                            }
                            "py" => "import sys  # harness probe\n".to_string(),
                            "java" => "import java.util.Date;\n".to_string(),
                            _ => "// harness probe\n".to_string(),
                        };
                        EditOp::AddImport {
                            rel_path: rel.to_string(),
                            statement,
                        }
                    })
                    .boxed(),
            )
        }
        EditOpKind::RemoveImport => {
            let targets: Vec<(&'static str, &'static str)> = spec
                .files
                .iter()
                .flat_map(|f| f.known_import_needles.iter().map(move |n| (f.rel_path, *n)))
                .collect();
            if targets.is_empty() {
                return None;
            }
            Some(
                (0_usize..targets.len())
                    .prop_map(move |i| {
                        let (rel, needle) = targets[i];
                        EditOp::RemoveImport {
                            rel_path: rel.to_string(),
                            needle: needle.to_string(),
                        }
                    })
                    .boxed(),
            )
        }
        EditOpKind::AddExternBlock => {
            let rel = spec.extern_block_target?;
            Some(
                (0_u32..32)
                    .prop_map(move |n| EditOp::AddExternBlock {
                        rel_path: rel.to_string(),
                        symbol: format!("harness_extern_sym_{n}"),
                    })
                    .boxed(),
            )
        }
        EditOpKind::AddHttpRoute => {
            let rel = spec.http_route_target?;
            Some(
                (
                    prop::sample::select(["GET", "POST", "DELETE"].to_vec()),
                    0_u32..32,
                )
                    .prop_map(move |(method, n)| EditOp::AddHttpRoute {
                        rel_path: rel.to_string(),
                        method: method.to_string(),
                        path: format!("/api/harness/{n}"),
                        handler: format!("harness_route_handler_{n}"),
                    })
                    .boxed(),
            )
        }
        EditOpKind::AddFile => {
            let stub = spec.add_file_stub;
            Some(
                (0_u32..1_000_000)
                    .prop_map(move |n| {
                        let name = format!("harness_added_{n}.{}", stub.extension);
                        let rel = if stub.dir.is_empty() {
                            name.clone()
                        } else {
                            format!("{}/{}", stub.dir, name)
                        };
                        EditOp::AddFile {
                            rel_path: rel,
                            content: stub.content_template.to_string(),
                        }
                    })
                    .boxed(),
            )
        }
        EditOpKind::RemoveFile => {
            let files: Vec<&'static str> = spec.files.iter().map(|f| f.rel_path).collect();
            if files.is_empty() {
                return None;
            }
            Some(
                (0_usize..files.len())
                    .prop_map(move |i| EditOp::RemoveFile {
                        rel_path: files[i].to_string(),
                    })
                    .boxed(),
            )
        }
        EditOpKind::RenameFile => {
            let files: Vec<&'static str> = spec.files.iter().map(|f| f.rel_path).collect();
            if files.is_empty() {
                return None;
            }
            Some(
                (0_usize..files.len(), 0_u32..32)
                    .prop_map(move |(i, n)| {
                        let rel = files[i];
                        let new_rel = renamed_sibling(rel, n);
                        EditOp::RenameFile {
                            old_rel_path: rel.to_string(),
                            new_rel_path: new_rel,
                        }
                    })
                    .boxed(),
            )
        }
        EditOpKind::WhitespaceEdit => {
            let files: Vec<&'static str> = spec.files.iter().map(|f| f.rel_path).collect();
            if files.is_empty() {
                return None;
            }
            Some(
                (0_usize..files.len())
                    .prop_map(move |i| EditOp::WhitespaceEdit {
                        rel_path: files[i].to_string(),
                    })
                    .boxed(),
            )
        }
        EditOpKind::InvalidSyntaxEdit => {
            let files: Vec<&'static str> = spec.files.iter().map(|f| f.rel_path).collect();
            if files.is_empty() {
                return None;
            }
            Some(
                (0_usize..files.len())
                    .prop_map(move |i| EditOp::InvalidSyntaxEdit {
                        rel_path: files[i].to_string(),
                    })
                    .boxed(),
            )
        }
    }
}

fn extension_of(rel_path: &str) -> &str {
    rel_path.rsplit_once('.').map_or("", |(_, ext)| ext)
}

fn renamed_sibling(rel_path: &str, salt: u32) -> String {
    // Preserve directory and extension, replace stem with a harness-
    // unique one so the file stays routable to the same plugin.
    let (dir, file) = rel_path.rsplit_once('/').unwrap_or(("", rel_path));
    let (stem, ext) = file.rsplit_once('.').unwrap_or((file, ""));
    let new_file = if ext.is_empty() {
        format!("{stem}_renamed_{salt}")
    } else {
        format!("{stem}_renamed_{salt}.{ext}")
    };
    if dir.is_empty() {
        new_file
    } else {
        format!("{dir}/{new_file}")
    }
}
