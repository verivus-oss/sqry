//! Regression guard for the complete natural-language surface removal.
//!
//! The `sqry-nl` crate, the CLI `sqry ask` command, the MCP `sqry_ask`
//! tool, the LSP `sqry/ask` request, and the whole ONNX classifier and
//! embedding-model surface were removed (see
//! `docs/reviews/sqry-nl-removal/2026-06-14/`). These tests fail if any
//! of those symbols reappear in live product source or in the workspace
//! lockfile, so an accidental reintroduction is caught by
//! `cargo test --workspace --locked`.

use std::fs;
use std::path::{Path, PathBuf};

/// Workspace root, derived from this crate's manifest dir
/// (`<root>/tests/integration`).
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace root is two levels above tests/integration")
        .to_path_buf()
}

/// Symbols that only existed to support the removed NL/ONNX surface.
/// Each is specific enough to avoid matching unrelated identifiers
/// (we deliberately avoid bare `ask`, `nl`, or `onnx`).
const FORBIDDEN_SYMBOLS: &[&str] = &[
    "sqry_ask",
    "SqryAsk",
    "run_ask",
    "Command::Ask",
    "execute_sqry_ask",
    "dispatch_sqry_ask",
    "handle_sqry_ask",
    "validate_sqry_ask_args",
    "sqry_nl",
    "OnnxRuntimeMissing",
    "ONNX_RUNTIME_MISSING",
    "onnx_runtime_install_hint",
    "get_or_init_translator",
    "nl_translator",
    "SQRY_MCP_ENABLE_SQRY_ASK",
];

/// Every workspace crate source tree (`<root>/sqry*/src`) must stay free of the
/// removed surface, not just the four product binaries. Enumerated dynamically so
/// a newly added crate is covered without editing a hand-maintained list.
fn product_src_dirs() -> Vec<PathBuf> {
    let root = workspace_root();
    let mut dirs = Vec::new();
    let Ok(entries) = fs::read_dir(&root) else {
        return dirs;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && entry.file_name().to_string_lossy().starts_with("sqry") {
            let src = path.join("src");
            if src.is_dir() {
                dirs.push(src);
            }
        }
    }
    dirs
}

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn sqry_nl_crate_directory_is_absent() {
    let crate_dir = workspace_root().join("sqry-nl");
    assert!(
        !crate_dir.exists(),
        "the sqry-nl crate was removed but {} still exists",
        crate_dir.display()
    );
}

#[test]
fn product_source_has_no_removed_nl_symbols() {
    let dirs = product_src_dirs();
    let mut files = Vec::new();
    for dir in &dirs {
        collect_rs_files(dir, &mut files);
    }
    assert!(
        !files.is_empty(),
        "expected to scan crate source files under {dirs:?}"
    );

    let mut violations = Vec::new();
    for file in &files {
        let Ok(contents) = fs::read_to_string(file) else {
            continue;
        };
        for (lineno, line) in contents.lines().enumerate() {
            for symbol in FORBIDDEN_SYMBOLS {
                if line.contains(symbol) {
                    violations.push(format!(
                        "{}:{}: contains removed symbol `{symbol}`",
                        file.display(),
                        lineno + 1
                    ));
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "removed natural-language / ONNX symbols reappeared in product source:\n{}",
        violations.join("\n")
    );
}

#[test]
fn workspace_lockfile_has_no_classifier_stack() {
    let lock = workspace_root().join("Cargo.lock");
    let contents = fs::read_to_string(&lock).expect("Cargo.lock is readable");
    for pkg in ["sqry-nl", "ort", "ort-sys", "tokenizers"] {
        let needle = format!("name = \"{pkg}\"");
        assert!(
            !contents.contains(&needle),
            "Cargo.lock still records the removed package `{pkg}`"
        );
    }
}

/// Manifests must not reference the removed crate, the ONNX classifier stack, or
/// the deleted `ask_ort_missing` NL test files. This closes the gap that let an
/// orphaned `sqry-lsp/Cargo.toml` test-helper self-dep (justified solely by the
/// now-deleted `tests/ask_ort_missing.rs`) survive the first removal pass.
#[test]
fn manifests_have_no_removed_nl_references() {
    const FORBIDDEN_MANIFEST_TERMS: &[&str] =
        &["sqry-nl", "ort-sys", "tokenizers", "ask_ort_missing"];
    let root = workspace_root();
    let mut manifests = vec![root.join("Cargo.toml")];
    if let Ok(entries) = fs::read_dir(&root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() && entry.file_name().to_string_lossy().starts_with("sqry") {
                let manifest = path.join("Cargo.toml");
                if manifest.is_file() {
                    manifests.push(manifest);
                }
            }
        }
    }

    let mut violations = Vec::new();
    for manifest in &manifests {
        let Ok(contents) = fs::read_to_string(manifest) else {
            continue;
        };
        for (lineno, line) in contents.lines().enumerate() {
            for term in FORBIDDEN_MANIFEST_TERMS {
                if line.contains(term) {
                    violations.push(format!(
                        "{}:{}: contains removed reference `{term}`",
                        manifest.display(),
                        lineno + 1
                    ));
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "removed natural-language / ONNX references reappeared in manifests:\n{}",
        violations.join("\n")
    );
}
