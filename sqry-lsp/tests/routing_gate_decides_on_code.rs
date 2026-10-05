//! U16 (surface parity W4 round 3, design W4-D21): the static routing gate
//! decides on code, not on comments or string literals.
//!
//! `scripts/ci/check_routing_gates.sh` used to accept a construction when any
//! gate token occurred anywhere in the raw text of the preceding window, so a
//! `"logical_workspace"` string or a `LogicalWorkspace` comment above an
//! ungated `GraphStorage::new` made it pass, and its annotation was honoured
//! inside a string and with an empty reason. It now lexes every scanned file
//! first and searches a code view, a line-comment table and a string table.
//!
//! This test drives the repository's own script, run by path from this
//! crate's manifest directory, never a copy committed beside the test:
//!
//! - the repository leg runs it in place and requires exit 0 with
//!   `(0, 0, 0)` violations, and under `ROUTING_GATE_TRACE=1` one decision
//!   for each family B construction in `sqry-lsp/src`, the constructions
//!   derived independently of the gate by this file's own Rust lexer;
//! - each case leg copies the script into a temporary root holding
//!   `scripts/ci/`, `sqry-lsp/src/` and `sqry-vscode/src/`, plants one file,
//!   runs it with the trace on, and asserts the exit status, the three counts
//!   and the trace decision for the planted file.
//!
//! Instrument first: `bash` and `python3` must resolve on `PATH`; a missing
//! tool panics naming it, and nothing is skipped.
//!
//! Record: `docs/development/surface-parity/06_TEST_EXECUTION-surface-parity.md`.
#![cfg(unix)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

const SCRIPT: &str = "scripts/ci/check_routing_gates.sh";
const RUST_CASE: &str = "sqry-lsp/src/w4_case.rs";
const TS_CASE: &str = "sqry-vscode/src/w4_case.ts";

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("sqry-lsp lives one level under the workspace root")
        .to_path_buf()
}

/// The tool's path on `PATH`, or a panic naming it: the gate cannot run
/// without it, and a skipped run would report nothing.
fn require_on_path(tool: &str) -> PathBuf {
    let search = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&search)
        .map(|directory| directory.join(tool))
        .find(|candidate| candidate.is_file())
        .unwrap_or_else(|| {
            panic!("instrument: `{tool}` is not on PATH; the routing gate needs it to run")
        })
}

/// One trace line: family, repository-relative path, line, decision.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Trace {
    family: String,
    path: String,
    line: usize,
    decision: String,
}

struct GateRun {
    status: Option<i32>,
    stdout: String,
    stderr: String,
}

impl GateRun {
    /// `(enumeration, navigation, rust)` from the summary line.
    fn counts(&self) -> Option<(usize, usize, usize)> {
        let summary = self
            .stdout
            .lines()
            .find(|line| line.starts_with("static-routing gate: scanned "))?;
        let numbers: Vec<usize> = summary
            .split(|c: char| !c.is_ascii_digit())
            .filter(|part| !part.is_empty())
            .filter_map(|part| part.parse().ok())
            .collect();
        match numbers.as_slice() {
            [enumeration, navigation, rust] => Some((*enumeration, *navigation, *rust)),
            _ => None,
        }
    }

    fn traces(&self) -> Vec<Trace> {
        self.stdout
            .lines()
            .filter_map(|line| {
                let fields: Vec<&str> = line.split('\t').collect();
                match fields.as_slice() {
                    ["routing-gate-trace", family, path, number, decision] => Some(Trace {
                        family: (*family).to_string(),
                        path: (*path).to_string(),
                        line: number.parse().ok()?,
                        decision: (*decision).to_string(),
                    }),
                    _ => None,
                }
            })
            .collect()
    }
}

fn run_gate(bash: &Path, script: &Path) -> GateRun {
    let output = Command::new(bash)
        .arg(script)
        .env("ROUTING_GATE_TRACE", "1")
        .output()
        .unwrap_or_else(|error| panic!("run {}: {error}", script.display()));
    GateRun {
        status: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

/// An independent code view of Rust source, written for this test rather
/// than shared with the gate: comments (nested block comments included) and
/// the contents of string, raw string and character literals become spaces,
/// newlines kept. A `'` followed by `\`, or by one character and a `'`,
/// opens a character literal; any other `'` is a lifetime or a label.
fn rust_code_view(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let starts = |at: usize, pattern: &str| {
        pattern
            .chars()
            .enumerate()
            .all(|(offset, expected)| chars.get(at + offset) == Some(&expected))
    };
    let blank = |c: char| if c == '\n' { '\n' } else { ' ' };
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < len {
        let c = chars[i];
        if starts(i, "//") {
            while i < len && chars[i] != '\n' {
                out.push(' ');
                i += 1;
            }
            continue;
        }
        if starts(i, "/*") {
            let mut depth = 0usize;
            loop {
                assert!(i < len, "an unterminated block comment in the repository");
                if starts(i, "/*") {
                    depth += 1;
                    out.push_str("  ");
                    i += 2;
                } else if starts(i, "*/") {
                    depth -= 1;
                    out.push_str("  ");
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    out.push(blank(chars[i]));
                    i += 1;
                }
            }
            continue;
        }
        let boundary = i == 0 || !ident(chars[i - 1]);
        if boundary && matches!(c, 'r' | 'b' | 'c') {
            let r_at = if matches!(c, 'b' | 'c') && chars.get(i + 1) == Some(&'r') {
                i + 1
            } else {
                i
            };
            if chars[r_at] == 'r' {
                let mut hashes_end = r_at + 1;
                while chars.get(hashes_end) == Some(&'#') {
                    hashes_end += 1;
                }
                if chars.get(hashes_end) == Some(&'"') {
                    let hashes = hashes_end - r_at - 1;
                    let close: String = std::iter::once('"')
                        .chain(std::iter::repeat_n('#', hashes))
                        .collect();
                    for &kept in &chars[i..=hashes_end] {
                        out.push(kept);
                    }
                    let mut j = hashes_end + 1;
                    while !starts(j, &close) {
                        assert!(j < len, "an unterminated raw string in the repository");
                        out.push(blank(chars[j]));
                        j += 1;
                    }
                    out.push_str(&close);
                    i = j + close.chars().count();
                    continue;
                }
            }
        }
        if c == '"' {
            out.push('"');
            let mut j = i + 1;
            while j < len && chars[j] != '"' {
                if chars[j] == '\\' {
                    out.push(' ');
                    j += 1;
                }
                if j < len {
                    out.push(blank(chars[j]));
                }
                j += 1;
            }
            assert!(j < len, "an unterminated string in the repository");
            out.push('"');
            i = j + 1;
            continue;
        }
        if c == '\'' {
            let close = if chars.get(i + 1) == Some(&'\\') {
                (i + 3..len).find(|&j| chars[j] == '\'')
            } else if chars.get(i + 2) == Some(&'\'') && chars.get(i + 1) != Some(&'\n') {
                Some(i + 2)
            } else {
                None
            };
            if let Some(close) = close {
                out.push('\'');
                for &inner in &chars[i + 1..close] {
                    out.push(blank(inner));
                }
                out.push('\'');
                i = close + 1;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

/// The 1-based lines of `text` whose code holds `GraphStorage::new`, any
/// whitespace, then `(`.
fn construction_lines(text: &str) -> Vec<usize> {
    let needle = "GraphStorage::new";
    rust_code_view(text)
        .lines()
        .enumerate()
        .filter(|(_, line)| {
            line.match_indices(needle)
                .any(|(at, _)| line[at + needle.len()..].trim_start().starts_with('('))
        })
        .map(|(index, _)| index + 1)
        .collect()
}

fn rust_files_under(directory: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(directory).expect("read a source directory") {
        let entry = entry.expect("a directory entry");
        let kind = entry.file_type().expect("a file type");
        let path = entry.path();
        if kind.is_dir() {
            rust_files_under(&path, out);
        } else if kind.is_file() && path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn the_repository_gate_passes_and_decides_every_construction() {
    let bash = require_on_path("bash");
    let python = require_on_path("python3");
    println!("bash: {}, python3: {}", bash.display(), python.display());
    let root = repository_root();
    let run = run_gate(&bash, &root.join(SCRIPT));
    println!("stdout:\n{}\nstderr:\n{}", run.stdout, run.stderr);
    assert_eq!(run.status, Some(0), "the gate must pass on the repository");
    assert_eq!(
        run.counts(),
        Some((0, 0, 0)),
        "(enumeration, navigation, rust) violations on the repository"
    );

    let mut files = Vec::new();
    rust_files_under(&root.join("sqry-lsp").join("src"), &mut files);
    files.sort();
    let mut expected: BTreeSet<(String, usize)> = BTreeSet::new();
    for file in &files {
        let text = std::fs::read_to_string(file).expect("read a source file");
        let relative = file
            .strip_prefix(&root)
            .expect("under the root")
            .display()
            .to_string();
        for line in construction_lines(&text) {
            expected.insert((relative.clone(), line));
        }
    }
    let traces = run.traces();
    let decided: BTreeSet<(String, usize)> = traces
        .iter()
        .filter(|trace| trace.family == "B")
        .map(|trace| (trace.path.clone(), trace.line))
        .collect();
    let family_b = traces.iter().filter(|trace| trace.family == "B").count();
    println!(
        "sqry-lsp/src files: {}, constructions in code (this test's lexer): {}, family B trace lines: {family_b}",
        files.len(),
        expected.len()
    );
    for trace in traces.iter().filter(|trace| trace.family == "B") {
        println!("  {} {} {}", trace.path, trace.line, trace.decision);
    }
    assert!(
        !files.is_empty(),
        "instrument: no .rs file under sqry-lsp/src"
    );
    assert!(
        !expected.is_empty(),
        "instrument: this test's lexer found no construction in sqry-lsp/src"
    );
    assert_eq!(
        family_b,
        decided.len(),
        "one trace line per construction line"
    );
    assert_eq!(
        decided, expected,
        "the gate must decide exactly the constructions in code"
    );
    let undecided: Vec<&Trace> = traces
        .iter()
        .filter(|trace| trace.family == "B")
        .filter(|trace| {
            !(trace.decision.starts_with("token ")
                || trace.decision == "annotation"
                || trace.decision == "allowlist")
        })
        .collect();
    assert!(
        undecided.is_empty(),
        "every construction on the repository is gated: {undecided:?}"
    );
}

#[derive(Debug, Clone, Copy)]
enum Expect {
    /// Exactly one trace line for the planted file, with this decision.
    Decision(&'static str),
    /// No trace line for the planted file.
    NoLine,
    /// The lexer refuses the file: exit 2, stderr names it, no summary.
    Unlexable,
}

struct Case {
    name: &'static str,
    path: &'static str,
    content: &'static str,
    status: i32,
    counts: Option<(usize, usize, usize)>,
    expect: Expect,
}

const fn rust_case(
    name: &'static str,
    content: &'static str,
    status: i32,
    counts: Option<(usize, usize, usize)>,
    expect: Expect,
) -> Case {
    Case {
        name,
        path: RUST_CASE,
        content,
        status,
        counts,
        expect,
    }
}

const fn ts_case(
    name: &'static str,
    content: &'static str,
    status: i32,
    counts: Option<(usize, usize, usize)>,
    expect: Expect,
) -> Case {
    Case {
        name,
        path: TS_CASE,
        content,
        status,
        counts,
        expect,
    }
}

const PASS: Option<(usize, usize, usize)> = Some((0, 0, 0));
const RUST_FAIL: Option<(usize, usize, usize)> = Some((0, 0, 1));
const ENUM_FAIL: Option<(usize, usize, usize)> = Some((1, 0, 0));
const NAV_FAIL: Option<(usize, usize, usize)> = Some((0, 1, 0));

const CASES: [Case; 26] = [
    rust_case(
        "B-none",
        "fn w4_case(p: &Path) {\n    let _storage = GraphStorage::new(p);\n}\n",
        1,
        RUST_FAIL,
        Expect::Decision("violation"),
    ),
    rust_case(
        "B-code",
        "fn w4_case(p: &Path, session: &S) {\n    let w = session.classify_path(p);\n    let _storage = GraphStorage::new(p);\n}\n",
        0,
        PASS,
        Expect::Decision("token classify_path"),
    ),
    rust_case(
        "B-string",
        "fn w4_case(p: &Path) {\n    let _s = \"logical_workspace\";\n    let _storage = GraphStorage::new(p);\n}\n",
        1,
        RUST_FAIL,
        Expect::Decision("violation"),
    ),
    rust_case(
        "B-raw-string",
        "fn w4_case(p: &Path) {\n    let _s = r#\"LogicalWorkspace::classify\"#;\n    let _storage = GraphStorage::new(p);\n}\n",
        1,
        RUST_FAIL,
        Expect::Decision("violation"),
    ),
    rust_case(
        "B-line-comment",
        "fn w4_case(p: &Path) {\n    // LogicalWorkspace is consulted upstream\n    let _storage = GraphStorage::new(p);\n}\n",
        1,
        RUST_FAIL,
        Expect::Decision("violation"),
    ),
    rust_case(
        "B-doc-comment",
        "/// Calls classify_path first.\nfn w4_case(p: &Path) {\n    let _storage = GraphStorage::new(p);\n}\n",
        1,
        RUST_FAIL,
        Expect::Decision("violation"),
    ),
    rust_case(
        "B-block-comment",
        "fn w4_case(p: &Path) {\n    /* outer /* classify_for_serve */ still a comment */\n    let _storage = GraphStorage::new(p);\n}\n",
        1,
        RUST_FAIL,
        Expect::Decision("violation"),
    ),
    rust_case(
        "B-char-quote",
        "fn w4_case(p: &Path, session: &S) {\n    let q = '\"'; let w = session.classify_path(p);\n    let _storage = GraphStorage::new(p);\n}\n",
        0,
        PASS,
        Expect::Decision("token classify_path"),
    ),
    rust_case(
        "B-lifetime",
        "fn f<'a>(p: &'a Path, session: &S) { let w = session.classify_path(p); GraphStorage::new(p); }\n",
        0,
        PASS,
        Expect::Decision("token classify_path"),
    ),
    rust_case(
        "B-annotation",
        "fn w4_case(p: &Path) {\n    // routing-gate-allow: W4 round 3 accepted case\n    let _storage = GraphStorage::new(p);\n}\n",
        0,
        PASS,
        Expect::Decision("annotation"),
    ),
    rust_case(
        "B-annotation-in-string",
        "fn w4_case(p: &Path) {\n    let _s = \"// routing-gate-allow: in a string\";\n    let _storage = GraphStorage::new(p);\n}\n",
        1,
        RUST_FAIL,
        Expect::Decision("violation"),
    ),
    rust_case(
        "B-annotation-empty",
        "fn w4_case(p: &Path) {\n    // routing-gate-allow:\n    let _storage = GraphStorage::new(p);\n}\n",
        1,
        RUST_FAIL,
        Expect::Decision("violation"),
    ),
    rust_case(
        "B-comment-occurrence",
        "fn w4_case() {\n    // GraphStorage::new(p) used to be here\n}\n",
        0,
        PASS,
        Expect::NoLine,
    ),
    rust_case(
        "B-unterminated",
        "fn w4_case() {}\n/* never closed\n",
        2,
        None,
        Expect::Unlexable,
    ),
    ts_case(
        "A-none",
        "import * as vscode from 'vscode';\n\nexport function w4Case(): void {\n  for (const x of vscode.workspace.workspaceFolders) {}\n}\n",
        1,
        ENUM_FAIL,
        Expect::Decision("violation"),
    ),
    ts_case(
        "A-code",
        "import * as vscode from 'vscode';\n\nexport function w4Case(): void {\n  const f = nonExcludedFolders();\n  for (const x of vscode.workspace.workspaceFolders) {}\n}\n",
        0,
        PASS,
        Expect::Decision("token nonExcludedFolders"),
    ),
    ts_case(
        "A-string",
        "import * as vscode from 'vscode';\n\nexport function w4Case(): void {\n  const s = \"classifyLogicalWorkspace\";\n  for (const x of vscode.workspace.workspaceFolders) {}\n}\n",
        1,
        ENUM_FAIL,
        Expect::Decision("violation"),
    ),
    ts_case(
        "A-comment",
        "import * as vscode from 'vscode';\n\nexport function w4Case(): void {\n  // isFolderExcluded\n  for (const x of vscode.workspace.workspaceFolders) {}\n}\n",
        1,
        ENUM_FAIL,
        Expect::Decision("violation"),
    ),
    ts_case(
        "A-template-text",
        "import * as vscode from 'vscode';\n\nexport function w4Case(): void {\n  const s = `classifyPath`;\n  for (const x of vscode.workspace.workspaceFolders) {}\n}\n",
        1,
        ENUM_FAIL,
        Expect::Decision("violation"),
    ),
    ts_case(
        "A-template-substitution",
        "import * as vscode from 'vscode';\n\nexport function w4Case(x: string): void {\n  const s = `${classifyPath(x)}`;\n  for (const y of vscode.workspace.workspaceFolders) {}\n}\n",
        0,
        PASS,
        Expect::Decision("token classifyPath"),
    ),
    ts_case(
        "A-regex-quote",
        "import * as vscode from 'vscode';\n\nexport function w4Case(): void {\n  const r = /\"/g; classifyLogicalWorkspace();\n  for (const x of vscode.workspace.workspaceFolders) {}\n}\n",
        0,
        PASS,
        Expect::Decision("token classifyLogicalWorkspace"),
    ),
    ts_case(
        "C-double",
        "import * as vscode from 'vscode';\n\nexport function w4Case(uri: vscode.Uri): void {\n  void vscode.commands.executeCommand(\"vscode.open\", uri);\n}\n",
        1,
        NAV_FAIL,
        Expect::Decision("violation"),
    ),
    ts_case(
        "C-single",
        "import * as vscode from 'vscode';\n\nexport function w4Case(uri: vscode.Uri): void {\n  void vscode.commands.executeCommand('vscode.open', uri);\n}\n",
        1,
        NAV_FAIL,
        Expect::Decision("violation"),
    ),
    ts_case(
        "C-comment",
        "import * as vscode from 'vscode';\n\nexport function w4Case(): void {\n  // opening it with `vscode.open` directly is not allowed here\n}\n",
        0,
        PASS,
        Expect::NoLine,
    ),
    ts_case(
        "C-annotation",
        "import * as vscode from 'vscode';\n\nexport function w4Case(uri: vscode.Uri): void {\n  // routing-gate-allow: not result-derived\n  void vscode.commands.executeCommand(\"vscode.open\", uri);\n}\n",
        0,
        PASS,
        Expect::Decision("annotation"),
    ),
    ts_case(
        "C-annotation-in-string",
        "import * as vscode from 'vscode';\n\nexport function w4Case(uri: vscode.Uri): void {\n  const note = \"routing-gate-allow: in a string\";\n  void vscode.commands.executeCommand(\"vscode.open\", uri);\n}\n",
        1,
        NAV_FAIL,
        Expect::Decision("violation"),
    ),
];

/// A temporary root holding the repository's script and one planted file.
fn planted_root(case: &Case) -> tempfile::TempDir {
    let root = tempfile::tempdir().expect("a temporary root");
    let script = root.path().join(SCRIPT);
    std::fs::create_dir_all(script.parent().expect("scripts/ci")).expect("mkdir scripts/ci");
    std::fs::copy(repository_root().join(SCRIPT), &script).expect("copy the repository's script");
    std::fs::create_dir_all(root.path().join("sqry-lsp").join("src")).expect("mkdir sqry-lsp/src");
    std::fs::create_dir_all(root.path().join("sqry-vscode").join("src"))
        .expect("mkdir sqry-vscode/src");
    std::fs::write(root.path().join(case.path), case.content).expect("plant the case file");
    root
}

#[test]
fn a_comment_or_a_string_is_not_consultation_and_code_is() {
    let bash = require_on_path("bash");
    let python = require_on_path("python3");
    println!("bash: {}, python3: {}", bash.display(), python.display());
    let mut failed: Vec<String> = Vec::new();
    for case in &CASES {
        let root = planted_root(case);
        let run = run_gate(&bash, &root.path().join(SCRIPT));
        let counts = run.counts();
        let planted: Vec<Trace> = run
            .traces()
            .into_iter()
            .filter(|trace| trace.path == case.path)
            .collect();
        let decisions: Vec<&str> = planted
            .iter()
            .map(|trace| trace.decision.as_str())
            .collect();
        println!(
            "{}: status {:?} (expected {}), counts {counts:?} (expected {:?}), trace {decisions:?} (expected {:?})",
            case.name, run.status, case.status, case.counts, case.expect
        );
        let mut problems = Vec::new();
        if run.status != Some(case.status) {
            problems.push(format!("status {:?}, expected {}", run.status, case.status));
        }
        if counts != case.counts {
            problems.push(format!("counts {counts:?}, expected {:?}", case.counts));
        }
        match case.expect {
            Expect::Decision(expected) => {
                if decisions != [expected] {
                    problems.push(format!("trace {decisions:?}, expected [{expected:?}]"));
                }
            }
            Expect::NoLine => {
                if !decisions.is_empty() {
                    problems.push(format!("trace {decisions:?}, expected no line"));
                }
            }
            Expect::Unlexable => {
                if !run
                    .stderr
                    .contains(&format!("error: cannot lex {}", case.path))
                {
                    problems.push(format!(
                        "stderr does not name the file as unlexable: {}",
                        run.stderr.trim()
                    ));
                }
            }
        }
        if !problems.is_empty() {
            failed.push(format!(
                "{}: {}\nstdout:\n{}\nstderr:\n{}",
                case.name,
                problems.join("; "),
                run.stdout,
                run.stderr
            ));
        }
    }
    println!("cases {}, failed {}", CASES.len(), failed.len());
    assert!(
        failed.is_empty(),
        "{} case(s) failed:\n{}",
        failed.len(),
        failed.join("\n\n")
    );
}
