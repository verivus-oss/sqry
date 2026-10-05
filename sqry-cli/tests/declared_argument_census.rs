//! T1 and T2 (surface parity, unit W4, design W4-D1): the declared-argument
//! census as a derived test, not a maintained list.
//!
//! T1 (`every_underscore_parameter_is_not_a_declared_argument`) builds the
//! unified graph over `sqry-cli/src` with the Rust plugin only (in memory,
//! nothing persisted), derives the set of declared argument ids at runtime by
//! walking `<Cli as clap::CommandFactory>::command()` and every subcommand
//! under it, takes every `Function` and `Method` node the graph holds with
//! its file and byte span, parses the parameter list out of that span, and
//! requires that no parameter of any function in the crate is named `_<id>`
//! for an id in that set: a value clap produced that a function binds under
//! a leading underscore is a flag nothing reads. The design (W4-D1) asked for
//! `NodeKind::Parameter` nodes; on `9062a02ef` the Rust plugin emits those
//! only for macro metavariables (6 over the crate, none underscore-prefixed),
//! and a function node's `signature` holds only its return type, so the
//! parameter names come from the source bytes at the span the graph
//! recorded, read from a tree-sitter parse of that span rather than from its
//! text (design W4-D12). The instrument is checked before the assertion: the graph must
//! register exactly the `.rs` files under `sqry-cli/src`, its function count
//! and the number of parsed parameter lists must be above zero, and the
//! parser must see at least one underscore-prefixed parameter somewhere in
//! the crate, so a walker that skipped files or a parser that stopped seeing
//! parameters fails the test instead of passing it vacuously.
//!
//! T2 (`every_cli_field_is_read_on_some_path`) derives the top-level ids the
//! same way, maps an id that `#[arg(id = "...")]` renamed back to its field
//! (read from the parse of the `Cli` struct, design W4-D19), and, for each
//! field, requires a read of that field in `sqry-cli/src` outside
//! `args/mod.rs`, or a `self.<field>` read inside a `Cli` method in
//! `args/mod.rs` that is called from outside the module. The graph records no
//! field-access edges for struct fields (design W4-D1 and Q5), so the reads
//! and the calls come from a tree-sitter parse of every source file (design
//! W4-D12): a read is a `field_expression` whose field child is a
//! `field_identifier` with the field's name and which is not the callee of a
//! call, or the same `receiver . name` token sequence inside a macro's token
//! tree. A comment or a string literal is its own node and never either, so
//! neither satisfies the requirement. Round 1 searched raw text, and a bare
//! comment naming a field passed the census over a field nothing read.
//!
//! Only what a non-test build compiles counts (design W4-D19, over the model
//! of `sqry_core::test_support::rust_liveness`, design W4-D18). Round 2
//! counted every read in every file under `src`, so a field read only inside
//! a `#[cfg(test)]` function, module, file or statement, a `#[test]`
//! function, or a test macro's body passed the census over a field no
//! shipped binary reads. Now a file that is not `Live` in the module tree
//! cargo's roots define contributes no read and no call; a read or a call
//! whose node an attribute removes is dropped; a read in a macro token tree
//! counts only when the invocation is live, carries no attribute-shaped
//! tokens and names no crate `macro_rules!` whose transcriber carries them;
//! and a `self` read counts through a `Cli` method only when that method is
//! live.
//!
//! The parse and the model are checked before the assertion: the roots cargo
//! reported are among the parsed files; every `.rs` file under `sqry-cli/src`
//! parsed with no error at its root; the files classify as `Live`,
//! `TestOnly` or `Unreachable` with at least one `TestOnly` and every root
//! `Live`; nothing is unmodelled; the grammar-derived attribute containers
//! equal the model's declared sets; the crate holds `field_expression`
//! nodes; `args/mod.rs` holds `self` field reads; reads are dropped as not
//! live (the crate's test modules read `Cli` fields, so a model that drops
//! nothing is broken); and the rename map is a bijection between the
//! top-level ids and the `Cli` fields that are arguments.
//!
//! Both tests print every figure they assert. Record:
//! `docs/development/surface-parity/06_TEST_EXECUTION-surface-parity.md`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use clap::CommandFactory;
use sqry_cli::args::Cli;
use sqry_core::graph::unified::NodeKind;
use sqry_core::graph::unified::build::{BuildConfig, build_unified_graph};
use sqry_core::plugin::PluginManager;
use sqry_core::test_support::rust_liveness::{
    self as liveness, BuildCfg, CrateLiveness, FileLiveness, RustSource,
};
use sqry_lang_rust::RustPlugin;

/// Clap's deep subcommand tree needs more than the default test-thread
/// stack (see `large_stack_test!` in the crate); run the body on 64 MB.
fn on_large_stack<T: Send + 'static>(body: impl FnOnce() -> T + Send + 'static) -> T {
    let joined = std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(body)
        .expect("spawn census thread")
        .join();
    match joined {
        Ok(value) => value,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

fn crate_src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// Every `.rs` file under `sqry-cli/src`, sorted, as the walker's expected
/// registration set.
fn rust_sources_under(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "rs"))
        .map(|entry| entry.path().to_path_buf())
        .collect();
    out.sort();
    out
}

/// Every argument id clap knows for `sqry`, recursively over the whole
/// subcommand tree. Ids are the clap derive's field names.
fn declared_argument_ids() -> BTreeSet<String> {
    fn walk(cmd: &clap::Command, out: &mut BTreeSet<String>) {
        for arg in cmd.get_arguments() {
            out.insert(arg.get_id().to_string());
        }
        for sub in cmd.get_subcommands() {
            walk(sub, out);
        }
    }
    let mut out = BTreeSet::new();
    walk(&Cli::command(), &mut out);
    out
}

/// The two ids clap generates on every command that are not `Cli` fields.
const CLAP_GENERATED_IDS: [&str; 2] = ["help", "version"];

/// The top-level (non-subcommand) argument ids of `Cli`, minus clap's own
/// `help` and `version`, which are not struct fields.
fn top_level_cli_ids() -> BTreeSet<String> {
    Cli::command()
        .get_arguments()
        .map(|arg| arg.get_id().to_string())
        .filter(|id| !CLAP_GENERATED_IDS.contains(&id.as_str()))
        .collect()
}

/// The text between (`start_line`, `start_column`) and (`end_line`,
/// `end_column`) of `text`, lines 1-based and columns 0-based as the Rust
/// plugin records them; `None` when the span falls outside the file.
fn line_span(
    text: &str,
    start_line: u32,
    start_column: u32,
    end_line: u32,
    end_column: u32,
) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let first = start_line.checked_sub(1)? as usize;
    let last = end_line.checked_sub(1)? as usize;
    if first > last || last >= lines.len() {
        return None;
    }
    let mut out = String::new();
    for (index, line) in lines[first..=last].iter().enumerate() {
        let from = if index == 0 { start_column as usize } else { 0 };
        let to = if first + index == last {
            (end_column as usize).min(line.chars().count())
        } else {
            line.chars().count()
        };
        if from > to {
            return None;
        }
        out.extend(line.chars().skip(from).take(to - from));
        out.push('\n');
    }
    Some(out)
}

/// The binding names of the parameters of the first function item in
/// `span` (the source the graph recorded for a `Function` or `Method` node),
/// from a tree-sitter parse of that span: one name per `parameter` whose
/// pattern is a plain identifier (`mut` is a separate specifier node, so
/// `mut x: T` yields `x`). Tuple, struct and reference patterns and `self`
/// receivers are skipped: none of them can carry a clap value under a name.
/// A function written inside a macro invocation (the crate's
/// `large_stack_test! { .. }` tests) is found by parsing the invocation's
/// token tree as items. `None` when the span holds no function item (a
/// call-site stub, or a string literal that merely contains `fn`).
///
/// Round 1 scanned the span's text for the first `fn ` and split its
/// parenthesised list on commas, so a comment holding a parenthesis or a
/// `name: Type` shape could hide a parameter or invent one, and a string
/// literal holding `fn` was taken for a function. A comment is its own node
/// in the parse and is never a `parameter`, and a string literal is never a
/// function item (design W4-D12).
fn parameter_names_in_span(parser: &mut tree_sitter::Parser, span: &str) -> Option<Vec<String>> {
    parameter_names_at_depth(parser, span, 0)
}

/// How deep [`parameter_names_in_span`] follows nested macro bodies.
const MACRO_BODY_DEPTH: usize = 4;

fn parameter_names_at_depth(
    parser: &mut tree_sitter::Parser,
    span: &str,
    depth: usize,
) -> Option<Vec<String>> {
    let tree = parser.parse(span, None)?;
    let source = span.as_bytes();
    let mut stack = vec![tree.root_node()];
    let mut macro_bodies: Vec<(usize, usize)> = Vec::new();
    let mut function = None;
    while let Some(node) = stack.pop() {
        if matches!(node.kind(), "function_item" | "function_signature_item") {
            function = Some(node);
            break;
        }
        if node.kind() == "token_tree"
            && node
                .parent()
                .is_some_and(|parent| parent.kind() == "macro_invocation")
            && node.child_count() >= 2
        {
            let last = u32::try_from(node.child_count() - 1).ok();
            if let (Some(open), Some(close)) = (node.child(0), last.and_then(|i| node.child(i))) {
                macro_bodies.push((open.end_byte(), close.start_byte()));
            }
        }
        let mut cursor = node.walk();
        let children: Vec<tree_sitter::Node<'_>> = node.children(&mut cursor).collect();
        stack.extend(children.into_iter().rev());
    }
    let Some(function) = function else {
        if depth >= MACRO_BODY_DEPTH {
            return None;
        }
        return macro_bodies.into_iter().find_map(|(start, end)| {
            let body = span.get(start..end)?.to_string();
            parameter_names_at_depth(parser, &body, depth + 1)
        });
    };
    let parameters = function.child_by_field_name("parameters")?;
    let mut cursor = parameters.walk();
    let names = parameters
        .named_children(&mut cursor)
        .filter(|child| child.kind() == "parameter")
        .filter_map(|parameter| parameter.child_by_field_name("pattern"))
        .filter(|pattern| pattern.kind() == "identifier")
        .map(|pattern| node_text(pattern, source).to_string())
        .collect();
    Some(names)
}

#[test]
fn every_underscore_parameter_is_not_a_declared_argument() {
    on_large_stack(|| {
        let src = crate_src_dir();
        let expected_files = rust_sources_under(&src);
        assert!(
            !expected_files.is_empty(),
            "instrument: no .rs files under {}",
            src.display()
        );

        let ids = declared_argument_ids();
        println!("declared argument ids (clap tree): {}", ids.len());
        assert!(
            ids.len() > 100,
            "instrument: the clap tree walked {} ids",
            ids.len()
        );

        let mut plugins = PluginManager::new();
        plugins.register_builtin(Box::new(RustPlugin::default()));
        let graph = build_unified_graph(&src, &plugins, &BuildConfig::default())
            .expect("build the unified graph over sqry-cli/src with the Rust plugin");
        let snapshot = graph.snapshot();

        // Instrument check 1: the graph registered exactly the crate's files.
        let absolute = |path: &Path| -> PathBuf {
            if path.is_absolute() {
                path.to_path_buf()
            } else {
                src.join(path)
            }
        };
        let mut registered: Vec<PathBuf> = snapshot
            .files()
            .iter()
            .map(|(_, path)| absolute(path.as_ref()))
            .collect();
        registered.sort();
        println!(
            "files registered: {} (expected {})",
            registered.len(),
            expected_files.len()
        );
        assert_eq!(
            registered, expected_files,
            "instrument: the graph must register exactly the .rs files under sqry-cli/src"
        );

        // Every Function and Method node, its span read from the source and
        // its parameters read from a parse of that span.
        let mut parser = rust_parser();
        let strings = snapshot.strings();
        let mut sources: BTreeMap<PathBuf, String> = BTreeMap::new();
        let mut function_count = 0usize;
        let mut parsed_lists = 0usize;
        let mut underscore_parameters = 0usize;
        let mut offending: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (_id, entry) in snapshot.nodes().iter() {
            if !matches!(entry.kind, NodeKind::Function | NodeKind::Method) {
                continue;
            }
            function_count += 1;
            let Some(path) = snapshot.files().resolve(entry.file) else {
                continue;
            };
            let path = absolute(path.as_ref());
            let text = sources
                .entry(path.clone())
                .or_insert_with(|| std::fs::read_to_string(&path).expect("read source"));
            // The Rust plugin records line and column spans (1-based lines,
            // 0-based columns; `start_byte`/`end_byte` stay 0), so the span
            // is sliced by line. Call-site stubs share the `Function` kind and
            // hold no function item, so they parse to `None` and are skipped.
            let Some(span) = line_span(
                text,
                entry.start_line,
                entry.start_column,
                entry.end_line,
                entry.end_column,
            ) else {
                continue;
            };
            let Some(names) = parameter_names_in_span(&mut parser, &span) else {
                continue;
            };
            parsed_lists += 1;
            let owner = entry
                .qualified_name
                .and_then(|q| strings.resolve(q))
                .map(|q| q.to_string())
                .unwrap_or_default();
            for name in names {
                let Some(stripped) = name.strip_prefix('_') else {
                    continue;
                };
                if stripped.is_empty() {
                    continue;
                }
                underscore_parameters += 1;
                if ids.contains(stripped) {
                    offending.entry(name.clone()).or_default().push(format!(
                        "{} line {} ({owner})",
                        path.strip_prefix(&src).unwrap_or(&path).display(),
                        entry.start_line
                    ));
                }
            }
        }
        println!("function and method nodes over sqry-cli/src: {function_count}");
        println!("parameter lists parsed from their spans: {parsed_lists}");
        println!("underscore-prefixed parameters seen: {underscore_parameters}");
        assert!(
            function_count > 0,
            "instrument: the Rust plugin emitted no Function or Method nodes"
        );
        assert!(
            parsed_lists > 0,
            "instrument: no function span yielded a parameter list"
        );
        assert!(
            underscore_parameters > 0,
            "instrument: the parser saw no underscore-prefixed parameter anywhere in the crate"
        );

        println!(
            "underscore parameters named after a declared argument id: {}",
            offending.len()
        );
        for (name, sites) in &offending {
            for site in sites {
                println!("  {name} at {site}");
            }
        }
        assert!(
            offending.is_empty(),
            "every value clap produces must be read on its path; these parameters bind a \
             declared argument under a leading underscore: {offending:#?}"
        );
    });
}

/// A Rust parser over `tree_sitter_rust::LANGUAGE`, the grammar the Rust
/// plugin uses.
fn rust_parser() -> tree_sitter::Parser {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .expect("the Rust grammar loads");
    parser
}

/// The build configuration of `sqry-cli` as `cargo metadata` reports it: the
/// library and binary roots and the feature table (design W4-D18 part 1).
/// Read once per test binary.
fn sqry_cli_build_cfg() -> &'static BuildCfg {
    static CFG: OnceLock<BuildCfg> = OnceLock::new();
    CFG.get_or_init(|| {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        BuildCfg::from_cargo_metadata(Path::new(env!("CARGO")), &manifest)
            .unwrap_or_else(|error| panic!("instrument: the build configuration: {error}"))
    })
}

/// One source file and its parse.
struct ParsedSource {
    path: PathBuf,
    text: String,
    tree: tree_sitter::Tree,
}

/// One read of a named field, found in the parse.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FieldRead {
    /// The field's name.
    field: String,
    /// The receiver is `self`.
    receiver_is_self: bool,
    /// The name of the innermost `function_item` holding the read, found by
    /// walking the parent chain.
    owner: Option<String>,
    /// The owning `function_item` exists in a non-test build.
    owner_live: bool,
    /// Found as `receiver . name` tokens inside a macro's token tree rather
    /// than as a `field_expression` node.
    in_token_tree: bool,
    /// The read exists in a non-test build: its file is `Live` and the model
    /// keeps its node (design W4-D19).
    live: bool,
}

/// What one parse holds that the census reads.
#[derive(Debug, Default)]
struct ParseFacts {
    /// Every `field_expression` node, including callees and tuple indices.
    field_expression_nodes: usize,
    /// Every read of a named field that is not the callee of a call, live or
    /// not.
    reads: Vec<FieldRead>,
    /// The file component the parse was given (design W4-D28 part 2): the
    /// module tree's answer for the source this parse walked.
    file_live: bool,
    /// Reads found at a `field_expression` that a non-test build does not
    /// compile (design W4-D28 part 3).
    reads_dropped_field: usize,
    /// Reads found inside a macro's token tree that a non-test build does not
    /// compile (design W4-D28 part 3).
    reads_dropped_token: usize,
    /// The name of every function or method called in a non-test build (the
    /// last path segment, or the method name of a `receiver.method(..)`
    /// call).
    called_names: BTreeSet<String>,
    /// Calls found, live or not.
    calls_found: usize,
    /// Calls found that a non-test build does not compile.
    calls_dropped: usize,
}

impl ParseFacts {
    /// Reads found that a non-test build does not compile, over both read
    /// paths. The two paths are counted apart so each one's node component
    /// can be asserted on its own (design W4-D28 part 3), and this sum keeps
    /// the crate-wide figure and its assertion independent of both halves
    /// (part 5).
    fn reads_dropped(&self) -> usize {
        self.reads_dropped_field + self.reads_dropped_token
    }
}

/// What the liveness of one file's nodes is decided against.
struct Liveness<'a> {
    cfg: &'a BuildCfg,
    /// The file is `Live` in the crate's module tree.
    file_live: bool,
    /// The crate's `macro_rules!` names whose transcriber carries an
    /// attribute-shaped token sequence.
    attribute_macros: &'a BTreeSet<String>,
}

fn node_text<'a>(node: tree_sitter::Node<'_>, source: &'a [u8]) -> &'a str {
    node.utf8_text(source)
        .expect("tree-sitter spans are UTF-8 boundaries")
}

/// The innermost enclosing `function_item`, by the parent chain.
fn owning_function<'t>(node: tree_sitter::Node<'t>) -> Option<tree_sitter::Node<'t>> {
    let mut current = node.parent();
    while let Some(parent) = current {
        if parent.kind() == "function_item" {
            return Some(parent);
        }
        current = parent.parent();
    }
    None
}

/// The owner's name and whether the owner exists in a non-test build.
fn owner_facts(
    node: tree_sitter::Node<'_>,
    source: &[u8],
    context: &Liveness<'_>,
) -> (Option<String>, bool) {
    match owning_function(node) {
        Some(function) => (
            function
                .child_by_field_name("name")
                .map(|name| node_text(name, source).to_string()),
            context.file_live && liveness::node_is_live(function, source, context.cfg),
        ),
        None => (None, false),
    }
}

/// The called name a call expression's `function` child denotes.
fn callee_name(function: tree_sitter::Node<'_>, source: &[u8]) -> Option<String> {
    match function.kind() {
        "identifier" => Some(node_text(function, source).to_string()),
        "field_expression" => function
            .child_by_field_name("field")
            .map(|field| node_text(field, source).to_string()),
        "scoped_identifier" => function
            .child_by_field_name("name")
            .map(|name| node_text(name, source).to_string()),
        "generic_function" => function
            .child_by_field_name("function")
            .and_then(|inner| callee_name(inner, source)),
        _ => None,
    }
}

/// The facts of one token tree's direct children: `receiver . name` is a
/// read of `name` unless a parenthesised token tree follows (a method
/// call); `name (..)` is a call of `name`. Comments and string literals are
/// their own nodes inside a token tree, so neither can form either shape.
/// Each read and call is live only as design W4-D18 part 5 says: its file is
/// live, and `liveness::macro_token_is_live` keeps it.
fn token_tree_facts(
    tree_node: tree_sitter::Node<'_>,
    source: &[u8],
    context: &Liveness<'_>,
    facts: &mut ParseFacts,
) {
    let mut cursor = tree_node.walk();
    let children: Vec<tree_sitter::Node<'_>> = tree_node.children(&mut cursor).collect();
    let opens_with_paren = |node: tree_sitter::Node<'_>| {
        node.kind() == "token_tree"
            && node
                .child(0)
                .is_some_and(|first| node_text(first, source) == "(")
    };
    let is_live = |node: tree_sitter::Node<'_>| {
        context.file_live
            && liveness::macro_token_is_live(node, source, context.cfg, context.attribute_macros)
    };
    for (index, child) in children.iter().enumerate() {
        if child.kind() != "identifier" {
            continue;
        }
        let next_is_call = children
            .get(index + 1)
            .is_some_and(|next| opens_with_paren(*next));
        if next_is_call {
            facts.calls_found += 1;
            if is_live(*child) {
                facts
                    .called_names
                    .insert(node_text(*child, source).to_string());
            } else {
                facts.calls_dropped += 1;
            }
            continue;
        }
        let Some(dot) = index.checked_sub(1).and_then(|i| children.get(i)) else {
            continue;
        };
        let dot_text = node_text(*dot, source);
        if dot.is_named() || !dot_text.ends_with('.') || dot_text.ends_with("..") {
            continue;
        }
        let Some(receiver) = index.checked_sub(2).and_then(|i| children.get(i)) else {
            continue;
        };
        let live = is_live(*child);
        if !live {
            facts.reads_dropped_token += 1;
        }
        let (owner, owner_live) = owner_facts(*child, source, context);
        facts.reads.push(FieldRead {
            field: node_text(*child, source).to_string(),
            receiver_is_self: receiver.kind() == "self",
            owner,
            owner_live,
            in_token_tree: true,
            live,
        });
    }
}

/// Walk one parse and collect the census facts, deciding each read and each
/// call's liveness against `context` (design W4-D19).
fn parse_facts(tree: &tree_sitter::Tree, source: &[u8], context: &Liveness<'_>) -> ParseFacts {
    let mut facts = ParseFacts {
        file_live: context.file_live,
        ..ParseFacts::default()
    };
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        match node.kind() {
            "field_expression" => {
                facts.field_expression_nodes += 1;
                let is_callee = node.parent().is_some_and(|parent| {
                    parent.kind() == "call_expression"
                        && parent
                            .child_by_field_name("function")
                            .is_some_and(|function| function.id() == node.id())
                });
                if let Some(field) = node.child_by_field_name("field")
                    && field.kind() == "field_identifier"
                    && !is_callee
                {
                    let live =
                        context.file_live && liveness::node_is_live(node, source, context.cfg);
                    if !live {
                        facts.reads_dropped_field += 1;
                    }
                    let (owner, owner_live) = owner_facts(node, source, context);
                    facts.reads.push(FieldRead {
                        field: node_text(field, source).to_string(),
                        receiver_is_self: node
                            .child_by_field_name("value")
                            .is_some_and(|value| value.kind() == "self"),
                        owner,
                        owner_live,
                        in_token_tree: false,
                        live,
                    });
                }
            }
            "call_expression" => {
                if let Some(name) = node
                    .child_by_field_name("function")
                    .and_then(|function| callee_name(function, source))
                {
                    facts.calls_found += 1;
                    if context.file_live && liveness::node_is_live(node, source, context.cfg) {
                        facts.called_names.insert(name);
                    } else {
                        facts.calls_dropped += 1;
                    }
                }
            }
            "token_tree" => token_tree_facts(node, source, context, &mut facts),
            _ => {}
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
    facts
}

fn parse_source(parser: &mut tree_sitter::Parser, path: PathBuf, text: String) -> ParsedSource {
    let tree = parser
        .parse(&text, None)
        .unwrap_or_else(|| panic!("tree-sitter returned no tree for {}", path.display()));
    ParsedSource { path, text, tree }
}

/// The union of `liveness::attribute_bearing_macro_rules` over `sources`.
fn attribute_macros_of(sources: &[ParsedSource]) -> BTreeSet<String> {
    sources
        .iter()
        .flat_map(|source| {
            liveness::attribute_bearing_macro_rules(&source.tree, source.text.as_bytes())
        })
        .collect()
}

/// The module-tree classification of `sources` from `roots` (design W4-D18
/// part 4).
fn crate_liveness_of(roots: &[PathBuf], sources: &[ParsedSource], cfg: &BuildCfg) -> CrateLiveness {
    let views: Vec<RustSource<'_>> = sources
        .iter()
        .map(|source| RustSource {
            path: &source.path,
            source: source.text.as_bytes(),
            tree: &source.tree,
        })
        .collect();
    liveness::crate_liveness(roots, &views, cfg)
}

/// Does `text`, parsed as a live crate root of `sqry-cli` under its default
/// features, read the field `id` in a non-test build? A read is a
/// `field_expression` whose field child is a `field_identifier` named `id`
/// and which is not the callee of a call, or the same shape as tokens in a
/// macro's token tree, and it counts only when the census would count it
/// (design W4-D19): the fragment's own root attributes, the attributes on
/// its chain, and the fragment's own `macro_rules!` decide. An identifier
/// that appears only in a comment, a string literal, a method call or as a
/// plain identifier is not a read.
fn reads_field(text: &str, id: &str) -> bool {
    let root = PathBuf::from("fragment").join("lib.rs");
    let parsed = vec![parse_source(
        &mut rust_parser(),
        root.clone(),
        text.to_string(),
    )];
    let cfg = sqry_cli_build_cfg();
    let crate_liveness = crate_liveness_of(std::slice::from_ref(&root), &parsed, cfg);
    let attribute_macros = attribute_macros_of(&parsed);
    let context = Liveness {
        cfg,
        file_live: crate_liveness.is_live(&root),
        attribute_macros: &attribute_macros,
    };
    let facts = parse_facts(&parsed[0].tree, parsed[0].text.as_bytes(), &context);
    live_fields_read([&facts]).contains(id)
}

/// The names of the fields read in a non-test build, over `facts`: the one
/// read set both the census and `reads_field` take their answer from, so the
/// fragment tests exercise the predicate the census applies (design W4-D19).
fn live_fields_read<'a>(facts: impl IntoIterator<Item = &'a ParseFacts>) -> BTreeSet<&'a str> {
    facts
        .into_iter()
        .flat_map(|facts| {
            facts
                .reads
                .iter()
                .filter(|read| read.live)
                .map(|read| read.field.as_str())
        })
        .collect()
}

/// Does the census count `id` as read through a live `Cli` method, over two
/// planted fragments: `args_fragment` standing in for `args/mod.rs` and
/// `outside_fragment` for a source outside it? Each fragment is parsed as a
/// live crate root of `sqry-cli` under its default features, exactly as
/// [`reads_field`] does, and the answer is the rule
/// `every_cli_field_is_read_on_some_path` applies (design W4-D19): a live
/// `self.<id>` read owned by a live method of `args_fragment`, whose name is
/// among the live called names of `outside_fragment`.
fn reads_through_a_method(args_fragment: &str, outside_fragment: &str, id: &str) -> bool {
    let args_root = PathBuf::from("fragment").join("args").join("mod.rs");
    let outside_root = PathBuf::from("fragment").join("outside.rs");
    let mut parser = rust_parser();
    let parsed = vec![
        parse_source(&mut parser, args_root.clone(), args_fragment.to_string()),
        parse_source(
            &mut parser,
            outside_root.clone(),
            outside_fragment.to_string(),
        ),
    ];
    let cfg = sqry_cli_build_cfg();
    let roots = vec![args_root.clone(), outside_root.clone()];
    let crate_liveness = crate_liveness_of(&roots, &parsed, cfg);
    // Both fragments are declared roots, so both must be live: a case that
    // held because its file was not live would prove nothing about the rule.
    assert!(
        crate_liveness.is_live(&args_root) && crate_liveness.is_live(&outside_root),
        "instrument: both planted fragments are live crate roots"
    );
    let attribute_macros = attribute_macros_of(&parsed);
    let facts: Vec<ParseFacts> = parsed
        .iter()
        .map(|source| {
            let context = Liveness {
                cfg,
                file_live: crate_liveness.is_live(&source.path),
                attribute_macros: &attribute_macros,
            };
            parse_facts(&source.tree, source.text.as_bytes(), &context)
        })
        .collect();
    let methods = cli_methods_reading(&facts[0], id);
    let called_outside: BTreeSet<&str> = facts[1].called_names.iter().map(String::as_str).collect();
    methods
        .iter()
        .any(|method| called_outside.contains(method.as_str()))
}

/// One field of the `Cli` struct, from the parse of `args/mod.rs`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CliField {
    /// The field's name.
    name: String,
    /// The id an `#[arg(id = "...")]` or `#[clap(id = "...")]` gives it.
    renamed_id: Option<String>,
    /// Not a `#[command(subcommand)]` or `#[command(flatten)]` field, so clap
    /// reports it as an argument id.
    is_argument: bool,
}

/// The inner children of a token tree (its delimiters dropped, comments
/// dropped).
fn token_tree_inner(token_tree: tree_sitter::Node<'_>) -> Vec<tree_sitter::Node<'_>> {
    let mut cursor = token_tree.walk();
    let children: Vec<tree_sitter::Node<'_>> = token_tree.children(&mut cursor).collect();
    let inner = match children.len() {
        0..=2 => &[][..],
        len => &children[1..len - 1],
    };
    inner
        .iter()
        .copied()
        .filter(|child| !matches!(child.kind(), "line_comment" | "block_comment"))
        .collect()
}

/// The path and argument token tree of an `attribute_item`.
fn attribute_parts<'t>(
    attribute_item: tree_sitter::Node<'t>,
    source: &[u8],
) -> Option<(String, Option<tree_sitter::Node<'t>>)> {
    let mut cursor = attribute_item.walk();
    let attribute = attribute_item
        .named_children(&mut cursor)
        .find(|child| child.kind() == "attribute")?;
    let mut cursor = attribute.walk();
    let path = attribute.named_children(&mut cursor).next()?;
    let compact: String = node_text(path, source)
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    Some((compact, attribute.child_by_field_name("arguments")))
}

/// The fields of the top-level `Cli` struct, from the parse: each field's
/// name, the id a clap `id = "..."` argument gives it, and whether clap
/// reports it as an argument. A rename is an attribute item on the field
/// whose path is `arg` or `clap` and whose argument token tree holds, as
/// direct children in order and at the start of an argument, an identifier
/// `id`, an `=` token and a string literal. A string literal or a comment is
/// one node, so no text inside either can form a rename.
fn cli_struct_fields(args_mod: &str) -> Vec<CliField> {
    let parsed = parse_source(
        &mut rust_parser(),
        PathBuf::from("args/mod.rs"),
        args_mod.to_string(),
    );
    let source = parsed.text.as_bytes();
    let root = parsed.tree.root_node();
    let mut cursor = root.walk();
    let structs: Vec<tree_sitter::Node<'_>> = root
        .named_children(&mut cursor)
        .filter(|node| {
            node.kind() == "struct_item"
                && node
                    .child_by_field_name("name")
                    .is_some_and(|name| node_text(name, source) == "Cli")
        })
        .collect();
    assert_eq!(
        structs.len(),
        1,
        "instrument: exactly one top-level struct named Cli in args/mod.rs"
    );
    let body = structs[0]
        .child_by_field_name("body")
        .expect("instrument: the Cli struct has a field list");
    let mut cursor = body.walk();
    let members: Vec<tree_sitter::Node<'_>> = body.children(&mut cursor).collect();
    let mut out = Vec::new();
    for (index, member) in members.iter().enumerate() {
        if member.kind() != "field_declaration" {
            continue;
        }
        let Some(name) = member.child_by_field_name("name") else {
            continue;
        };
        let mut renamed_id = None;
        let mut is_argument = true;
        for attribute_item in members[..index]
            .iter()
            .rev()
            .take_while(|node| {
                matches!(
                    node.kind(),
                    "attribute_item" | "line_comment" | "block_comment"
                )
            })
            .filter(|node| node.kind() == "attribute_item")
        {
            let Some((path, Some(arguments))) = attribute_parts(*attribute_item, source) else {
                continue;
            };
            let inner = token_tree_inner(arguments);
            if path == "command"
                && let [only] = inner.as_slice()
                && only.kind() == "identifier"
                && matches!(node_text(*only, source), "subcommand" | "flatten")
            {
                is_argument = false;
            }
            if !matches!(path.as_str(), "arg" | "clap") {
                continue;
            }
            for (position, window) in inner.windows(3).enumerate() {
                let starts_an_argument = position == 0
                    || (!inner[position - 1].is_named()
                        && node_text(inner[position - 1], source) == ",");
                if starts_an_argument
                    && window[0].kind() == "identifier"
                    && node_text(window[0], source) == "id"
                    && !window[1].is_named()
                    && node_text(window[1], source) == "="
                    && window[2].kind() == "string_literal"
                {
                    let mut cursor = window[2].walk();
                    let content: String = window[2]
                        .named_children(&mut cursor)
                        .filter(|part| matches!(part.kind(), "string_content" | "escape_sequence"))
                        .map(|part| node_text(part, source))
                        .collect();
                    renamed_id = Some(content);
                }
            }
        }
        out.push(CliField {
            name: node_text(name, source).to_string(),
            renamed_id,
            is_argument,
        });
    }
    out
}

/// Clap ids the `Cli` struct renames with `#[arg(id = "...")]`, mapped to the
/// field they rename (the field is what the code reads; the id is what clap
/// reports), read from the parse (design W4-D19; round 2 read raw lines, so
/// an `id = "..."` line inside a multi-line string literal or a block
/// comment was taken for a rename of the next field).
fn renamed_ids(args_mod: &str) -> BTreeMap<String, String> {
    cli_struct_fields(args_mod)
        .into_iter()
        .filter_map(|field| field.renamed_id.map(|id| (id, field.name)))
        .collect()
}

/// The names of the live functions in `args/mod.rs` whose bodies read
/// `self.<id>` in a non-test build, from the parse: every live read whose
/// receiver is `self`, owned by the innermost enclosing `function_item`
/// found by the parent chain, when that function is live too.
fn cli_methods_reading(args_facts: &ParseFacts, id: &str) -> BTreeSet<String> {
    args_facts
        .reads
        .iter()
        .filter(|read| read.live && read.owner_live && read.receiver_is_self && read.field == id)
        .filter_map(|read| read.owner.clone())
        .collect()
}

fn check_cases(label: &str, cases: &[(&str, &str, bool)]) {
    let mut failed: Vec<String> = Vec::new();
    for (case, fragment, expected) in cases {
        let actual = reads_field(fragment, "w4_probe");
        println!("{label}: {case}: reads_field == {actual} (expected {expected})");
        if actual != *expected {
            failed.push(format!(
                "{case}: reads_field returned {actual}, expected {expected}, for:\n{fragment}"
            ));
        }
    }
    println!("{label}: cases {}, failed {}", cases.len(), failed.len());
    assert!(
        failed.is_empty(),
        "{label}: {} case(s) failed:\n{}",
        failed.len(),
        failed.join("\n")
    );
}

/// U3 (surface parity W4 round 2, design W4-D12): the read predicate
/// decides from the parse. An identifier that appears only in a line
/// comment, a block comment, a string literal, a method call or as a plain
/// identifier is not a field read; a real field expression, in code or in a
/// macro's arguments, is.
#[test]
fn a_comment_does_not_count_as_a_field_read() {
    let cases: [(&str, &str, bool); 7] = [
        (
            "line comment",
            "fn f(cli: &Cli) {\n    let _ = cli; // .w4_probe\n}\n",
            false,
        ),
        (
            "string literal",
            "fn f() {\n    let _s = \".w4_probe\";\n}\n",
            false,
        ),
        (
            "block comment",
            "fn f() {\n    /* cli.w4_probe */\n}\n",
            false,
        ),
        (
            "plain identifier",
            "fn f(w4_probe: bool) -> bool {\n    w4_probe\n}\n",
            false,
        ),
        (
            "method call",
            "fn f(cli: &Cli) -> bool {\n    cli.w4_probe()\n}\n",
            false,
        ),
        (
            "field expression",
            "fn f(cli: &Cli) -> bool {\n    cli.w4_probe\n}\n",
            true,
        ),
        (
            "field read inside a macro",
            "fn f(cli: &Cli) {\n    println!(\"{}\", cli.w4_probe);\n}\n",
            true,
        ),
    ];
    check_cases("U3", &cases);
}

/// U11 (surface parity W4 round 3, design W4-D19): a read counts only when a
/// non-test build compiles it. Every "not counted" fragment reads
/// `cli.w4_probe` only in code a non-test build removes: a `#[cfg(test)]`
/// item, module or statement, a test harness attribute, a cfg that is false
/// without `test`, an undeclared feature, a removed match arm or struct
/// field, a file-level `#![cfg(test)]`, a test macro's token tree, a local
/// `macro_rules!` whose expansion is test-only, or a transcriber never
/// invoked. Every "counted" fragment is a control (invariant I18): the read
/// exists in some non-test build.
#[test]
fn a_read_no_non_test_build_compiles_does_not_count() {
    const READ_FN: &str = "fn f(cli: &Cli) -> bool {\n    cli.w4_probe\n}\n";
    let gated = |attribute: &str| format!("{attribute}\n{READ_FN}");
    let not_counted: Vec<(&str, String)> = vec![
        ("1 item: #[cfg(test)] function", gated("#[cfg(test)]")),
        (
            "2 inline module: #[cfg(test)] mod tests",
            "#[cfg(test)]\nmod tests {\n    fn f(cli: &Cli) -> bool {\n        cli.w4_probe\n    }\n}\n"
                .to_string(),
        ),
        (
            "3 statement: #[cfg(test)] let",
            "fn f(cli: &Cli) {\n    #[cfg(test)]\n    let _ = cli.w4_probe;\n}\n".to_string(),
        ),
        (
            "4 #[test] function",
            "#[test]\nfn f() {\n    let cli = Cli::default();\n    let _ = cli.w4_probe;\n}\n"
                .to_string(),
        ),
        (
            "5 #[tokio::test] async function",
            "#[tokio::test]\nasync fn f() {\n    let cli = Cli::default();\n    let _ = cli.w4_probe;\n}\n"
                .to_string(),
        ),
        ("6 #[cfg(all(test, unix))]", gated("#[cfg(all(test, unix))]")),
        ("7 #[cfg(not(not(test)))]", gated("#[cfg(not(not(test)))]")),
        (
            "8 #[cfg_attr(not(test), cfg(test))]",
            gated("#[cfg_attr(not(test), cfg(test))]"),
        ),
        (
            "9 #[cfg(feature = \"w4_undeclared_feature\")]",
            gated("#[cfg(feature = \"w4_undeclared_feature\")]"),
        ),
        (
            "10 match arm: #[cfg(test)] Some(_) => cli.w4_probe",
            "fn f(cli: &Cli, x: Option<u8>) -> bool {\n    match x {\n        #[cfg(test)]\n        Some(_) => cli.w4_probe,\n        _ => false,\n    }\n}\n"
                .to_string(),
        ),
        (
            "11 struct literal field: #[cfg(test)] a: cli.w4_probe",
            "fn f(cli: &Cli) -> Probe {\n    Probe {\n        #[cfg(test)]\n        a: cli.w4_probe,\n    }\n}\n"
                .to_string(),
        ),
        (
            "12 #![cfg(test)] at the fragment's root",
            format!("#![cfg(test)]\n{READ_FN}"),
        ),
        (
            "13 large_stack_test! { #[test] fn t() .. }",
            "large_stack_test! {\n    #[test]\n    fn t() {\n        let cli = Cli::default();\n        let _ = cli.w4_probe;\n    }\n}\n"
                .to_string(),
        ),
        (
            "14 a local macro_rules! whose transcriber wraps its input in a #[cfg(test)] function",
            "macro_rules! only_in_tests {\n    ($($body:tt)*) => {\n        #[cfg(test)]\n        fn only_in_tests_body(cli: &Cli) {\n            $($body)*\n        }\n    };\n}\nonly_in_tests! { let _ = cli.w4_probe; }\n"
                .to_string(),
        ),
        (
            "15 a macro_rules! body never invoked",
            "macro_rules! never_invoked {\n    () => {\n        let _ = cli.w4_probe;\n    };\n}\n"
                .to_string(),
        ),
    ];
    let counted: Vec<(&str, String)> =
        vec![
        ("1 plain function", READ_FN.to_string()),
        ("2 #[cfg(not(test))]", gated("#[cfg(not(test))]")),
        ("3 #[cfg(any(test, unix))]", gated("#[cfg(any(test, unix))]")),
        ("4 #[cfg(unix)]", gated("#[cfg(unix)]")),
        (
            "5 #[cfg(feature = \"jvm-classpath\")] (a default feature of sqry-cli)",
            gated("#[cfg(feature = \"jvm-classpath\")]"),
        ),
        (
            "6 println! in a plain function",
            "fn f(cli: &Cli) {\n    println!(\"{}\", cli.w4_probe);\n}\n".to_string(),
        ),
        ("7 #[allow(dead_code)]", gated("#[allow(dead_code)]")),
        (
            "8 a #[cfg(test)] item before the reading function",
            format!("#[cfg(test)]\nfn other() {{}}\n{READ_FN}"),
        ),
        (
            "9 a #[cfg(test)] statement before a plain read in the same block",
            "fn f(cli: &Cli) {\n    #[cfg(test)]\n    let _x = 1;\n    let _ = cli.w4_probe;\n}\n"
                .to_string(),
        ),
    ];
    assert_eq!(not_counted.len(), 15, "the plan's not-counted list");
    assert_eq!(counted.len(), 9, "the plan's counted list");
    let mut cases: Vec<(&str, &str, bool)> = Vec::new();
    for (label, fragment) in &not_counted {
        cases.push((label, fragment.as_str(), false));
    }
    for (label, fragment) in &counted {
        cases.push((label, fragment.as_str(), true));
    }
    check_cases("U11", &cases);
}

/// U19 (surface parity W4 round 4, design W4-D26): the method half of the
/// census's rule counts only what a non-test build compiles. A `Cli` method
/// whose own item, whose `self` read, or whose only call site lives in code a
/// non-test build removes does not make the field read, and the call that
/// reaches the method may stand in a macro's token tree as well as in an
/// expression. The first case is the control, the method half of invariant
/// I18: without it the rule could be "count nothing". The second and the
/// fifth are boundary coverage that the read's own liveness decides, and the
/// record says so instead of counting them as reds; the third and the fourth
/// are the pins for the call's liveness in `parse_facts` and in
/// `token_tree_facts`.
#[test]
fn a_cli_method_read_no_non_test_build_compiles_does_not_count() {
    const LIVE_METHOD: &str =
        "impl Cli {\n    pub fn w4_reader(&self) -> bool {\n        self.w4_probe\n    }\n}\n";
    const LIVE_CALL: &str = "pub fn outside(cli: &Cli) -> bool {\n    cli.w4_reader()\n}\n";
    let cases: [(&str, &str, &str, bool); 5] = [
        (
            "1 a live method with a live outside call (control)",
            LIVE_METHOD,
            LIVE_CALL,
            true,
        ),
        (
            "2 a #[cfg(test)] method with a live outside call",
            "#[cfg(test)]\nimpl Cli {\n    pub fn w4_reader(&self) -> bool {\n        self.w4_probe\n    }\n}\n",
            LIVE_CALL,
            false,
        ),
        (
            "3 a live method called only from a #[cfg(test)] function",
            LIVE_METHOD,
            "#[cfg(test)]\nfn outside(cli: &Cli) -> bool {\n    cli.w4_reader()\n}\n",
            false,
        ),
        (
            "4 a live method called only from a macro token tree in a #[cfg(test)] function",
            LIVE_METHOD,
            "#[cfg(test)]\nfn outside(cli: &Cli) {\n    println!(\"{}\", cli.w4_reader());\n}\n",
            false,
        ),
        (
            "5 a live method whose self read is a #[cfg(test)] statement",
            "impl Cli {\n    pub fn w4_reader(&self) -> bool {\n        #[cfg(test)]\n        let _ = self.w4_probe;\n        false\n    }\n}\n",
            LIVE_CALL,
            false,
        ),
    ];
    assert_eq!(cases.len(), 5, "the plan's case list");
    let mut failed: Vec<String> = Vec::new();
    for (case, args_fragment, outside_fragment, expected) in &cases {
        let actual = reads_through_a_method(args_fragment, outside_fragment, "w4_probe");
        println!("U19: {case}: reads_through_a_method == {actual} (expected {expected})");
        if actual != *expected {
            failed.push(format!(
                "{case}: reads_through_a_method returned {actual}, expected {expected}, for:\n\
                 {args_fragment}---\n{outside_fragment}"
            ));
        }
    }
    println!("U19: cases {}, failed {}", cases.len(), failed.len());
    assert!(
        failed.is_empty(),
        "U19: {} case(s) failed:\n{}",
        failed.len(),
        failed.join("\n")
    );
}

/// U14 (surface parity W4 round 3, design W4-D19): a rename comes from the
/// attribute's parsed tokens. An `id = "..."` line inside a raw string
/// literal or a block comment is one token of that literal or comment and
/// renames nothing; a real `id = "..."` argument, on one line or on its own
/// line, and the one the real `args/mod.rs` carries, still do. Round 2 read
/// raw lines and took only an `id = "` at the start of a trimmed line, so it
/// also missed a rename written inside a one-line `#[arg(..)]`.
#[test]
fn a_renamed_id_comes_from_the_attribute_not_from_text() {
    let real = std::fs::read_to_string(crate_src_dir().join("args").join("mod.rs"))
        .expect("read args/mod.rs");
    let owned = |pairs: &[(&str, &str)]| -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(id, field)| ((*id).to_string(), (*field).to_string()))
            .collect()
    };
    let cases: [(&str, &str, BTreeMap<String, String>); 5] = [
        (
            "an attribute argument on its own line (control)",
            "pub struct Cli {\n    #[arg(\n        long,\n        id = \"global_y\",\n    )]\n    pub y: bool,\n}\n",
            owned(&[("global_y", "y")]),
        ),
        (
            "an attribute argument inside a one-line #[arg(..)]",
            "pub struct Cli {\n    #[arg(long, id = \"global_x\")]\n    pub x: bool,\n}\n",
            owned(&[("global_x", "x")]),
        ),
        (
            "an id line inside a raw string help",
            "pub struct Cli {\n    #[arg(long, help = r#\"first line\nid = \"w4_decoy\"\nlast line\"#)]\n    pub json: bool,\n}\n",
            owned(&[]),
        ),
        (
            "an id line inside a block comment",
            "pub struct Cli {\n    /*\n    id = \"w4_block\"\n    */\n    pub verbose: bool,\n}\n",
            owned(&[]),
        ),
        (
            "an id inside a line comment (control)",
            "pub struct Cli {\n    // id = \"w4_comment\"\n    pub quiet: bool,\n}\n",
            owned(&[]),
        ),
    ];
    let mut failed: Vec<String> = Vec::new();
    for (label, fragment, expected) in &cases {
        let actual = renamed_ids(fragment);
        println!("U14: {label}: {actual:?} (expected {expected:?})");
        if &actual != expected {
            failed.push(format!(
                "{label}: renamed_ids returned {actual:?}, expected {expected:?}, for:\n{fragment}"
            ));
        }
    }
    let from_real = renamed_ids(&real);
    let real_workspace = from_real.get("global_workspace_path").cloned();
    println!(
        "U14: the real args/mod.rs (control): global_workspace_path => {real_workspace:?} (expected Some(\"workspace\"))"
    );
    if real_workspace.as_deref() != Some("workspace") {
        failed.push(format!(
            "the real args/mod.rs: global_workspace_path => {real_workspace:?}, expected workspace"
        ));
    }
    println!("U14: cases {}, failed {}", cases.len() + 1, failed.len());
    assert!(
        failed.is_empty(),
        "{} case(s) failed:\n{}",
        failed.len(),
        failed.join("\n")
    );
}

/// U3b (surface parity W4 round 2, design W4-D12, the parameter half of the
/// class R2-T4 names): parameter names come from the parse of the span. A
/// comment inside the parameter list neither hides a real parameter nor
/// invents one, and a `fn` inside a doc comment above the item is not the
/// item.
#[test]
fn a_comment_neither_hides_nor_invents_a_parameter() {
    let cases: [(&str, &str, Vec<&str>); 5] = [
        (
            "plain parameters",
            "fn f(_json: bool, mut depth: usize, (a, b): (u8, u8), &c: &u8) {}\n",
            vec!["_json", "depth"],
        ),
        (
            "comment with an open parenthesis before a parameter",
            "fn f(\n    // see g(\n    _json: bool,\n) {}\n",
            vec!["_json"],
        ),
        (
            "comment shaped like a parameter",
            "fn f(depth: usize /* , _json: bool */) {}\n",
            vec!["depth"],
        ),
        (
            "doc comment naming another fn",
            "/// Calls fn g(_other: u8) first.\nfn f(_json: bool) {}\n",
            vec!["_json"],
        ),
        (
            "function inside a macro body",
            "large_stack_test! {\n    #[test]\n    fn f(_json: bool) {}\n}\n",
            vec!["_json"],
        ),
    ];
    let mut parser = rust_parser();
    let mut failed: Vec<String> = Vec::new();
    // A string literal that holds `fn` is not a function item at all.
    let literal = "fs::write(&path, \"fn g(_json: bool) {}\").unwrap()\n";
    let from_literal = parameter_names_in_span(&mut parser, literal);
    println!("string literal holding fn: {from_literal:?} (expected None)");
    if from_literal.is_some() {
        failed.push(format!(
            "string literal holding fn: got {from_literal:?}, expected None, for:\n{literal}"
        ));
    }
    for (label, span, expected) in &cases {
        let actual = parameter_names_in_span(&mut parser, span);
        let expected: Vec<String> = expected.iter().map(|name| (*name).to_string()).collect();
        println!("{label}: {actual:?} (expected {expected:?})");
        if actual.as_ref() != Some(&expected) {
            failed.push(format!(
                "{label}: got {actual:?}, expected {expected:?}, for:\n{span}"
            ));
        }
    }
    println!("cases: {}, failed: {}", cases.len() + 1, failed.len());
    assert!(
        failed.is_empty(),
        "{} case(s) failed:\n{}",
        failed.len(),
        failed.join("\n")
    );
}

#[test]
fn every_cli_field_is_read_on_some_path() {
    on_large_stack(|| {
        let src = crate_src_dir();
        let args_mod_path = src.join("args").join("mod.rs");
        let args_mod = std::fs::read_to_string(&args_mod_path).expect("read args/mod.rs");
        let expected_files = rust_sources_under(&src);
        let cfg = sqry_cli_build_cfg();

        // Instrument 0: the roots cargo reported are among the files.
        println!("crate roots (cargo metadata): {}", cfg.roots().len());
        for root in cfg.roots() {
            println!("  {}", root.display());
        }
        assert!(
            !cfg.roots().is_empty(),
            "instrument: cargo reported no root"
        );
        let missing_roots: Vec<&PathBuf> = cfg
            .roots()
            .iter()
            .filter(|root| !expected_files.contains(root))
            .collect();
        assert!(
            missing_roots.is_empty(),
            "instrument: every root is a parsed file: {missing_roots:?}"
        );

        // Parse every source file once.
        let mut parser = rust_parser();
        let parsed: Vec<ParsedSource> = expected_files
            .iter()
            .map(|path| {
                let text = std::fs::read_to_string(path).expect("read source");
                parse_source(&mut parser, path.clone(), text)
            })
            .collect();

        // Instrument 1: every `.rs` file under sqry-cli/src was parsed.
        println!(
            "files parsed: {} (.rs files under sqry-cli/src: {})",
            parsed.len(),
            expected_files.len()
        );
        assert!(
            !expected_files.is_empty(),
            "instrument: no .rs files under {}",
            src.display()
        );
        assert_eq!(
            parsed.len(),
            expected_files.len(),
            "instrument: every .rs file under sqry-cli/src must be parsed"
        );

        // Instrument 2: no parsed tree has an error at its root.
        let root_errors: Vec<String> = parsed
            .iter()
            .filter(|source| source.tree.root_node().has_error())
            .map(|source| source.path.display().to_string())
            .collect();
        println!(
            "parsed trees with an error at the root: {}",
            root_errors.len()
        );
        for path in &root_errors {
            println!("  {path}");
        }
        assert!(
            root_errors.is_empty(),
            "instrument: every parse must be clean: {root_errors:?}"
        );

        // Instrument 3: the model's attachment sets are the grammar's.
        let grammar = liveness::attribute_containers_in_grammar().expect("NODE_TYPES parses");
        let declared = liveness::declared_attribute_containers();
        println!(
            "attribute containers: grammar {}, declared {}",
            grammar.len(),
            declared.len()
        );
        assert_eq!(
            grammar, declared,
            "instrument: the grammar's attribute containers must equal the model's declared sets"
        );

        // Instrument 4: the module tree classifies every file.
        let crate_liveness = crate_liveness_of(cfg.roots(), &parsed, cfg);
        let live = crate_liveness.count(FileLiveness::Live);
        let test_only = crate_liveness.count(FileLiveness::TestOnly);
        let unreachable = crate_liveness.count(FileLiveness::Unreachable);
        println!(
            "files: live {live}, test-only {test_only}, unreachable {unreachable} (parsed {})",
            parsed.len()
        );
        for (path, class) in &crate_liveness.files {
            if *class != FileLiveness::Live {
                println!(
                    "  {class:?}: {}",
                    path.strip_prefix(&src).unwrap_or(path).display()
                );
            }
        }
        println!("unmodelled sites: {}", crate_liveness.unmodelled.len());
        for site in &crate_liveness.unmodelled {
            println!("  {site}");
        }
        assert_eq!(
            live + test_only + unreachable,
            parsed.len(),
            "instrument: every parsed file is classified once"
        );
        assert!(live > 0, "instrument: no file is live");
        for root in cfg.roots() {
            assert!(
                crate_liveness.is_live(root),
                "instrument: the root {} must be live",
                root.display()
            );
        }
        assert!(
            test_only > 0,
            "instrument: no file is test-only (sqry-cli/src/commands/rules/tests.rs is)"
        );
        assert!(
            crate_liveness.unmodelled.is_empty(),
            "instrument: nothing may be unmodelled: {:?}",
            crate_liveness.unmodelled
        );

        let attribute_macros = attribute_macros_of(&parsed);
        println!("attribute-bearing macro_rules: {attribute_macros:?}");
        let facts: Vec<(&ParsedSource, ParseFacts)> = parsed
            .iter()
            .map(|source| {
                let context = Liveness {
                    cfg,
                    file_live: crate_liveness.is_live(&source.path),
                    attribute_macros: &attribute_macros,
                };
                (
                    source,
                    parse_facts(&source.tree, source.text.as_bytes(), &context),
                )
            })
            .collect();

        // Instrument 5: the crate holds field expressions at all.
        let field_expression_nodes: usize = facts
            .iter()
            .map(|(_, facts)| facts.field_expression_nodes)
            .sum();
        println!("field_expression nodes over the crate: {field_expression_nodes}");
        assert!(
            field_expression_nodes > 0,
            "instrument: the parse found no field_expression node in the crate"
        );

        // Instrument 6: the model drops reads, and every read is either kept
        // or dropped.
        let reads_found: usize = facts.iter().map(|(_, facts)| facts.reads.len()).sum();
        let reads_live: usize = facts
            .iter()
            .map(|(_, facts)| facts.reads.iter().filter(|read| read.live).count())
            .sum();
        let reads_dropped: usize = facts.iter().map(|(_, facts)| facts.reads_dropped()).sum();
        let calls_found: usize = facts.iter().map(|(_, facts)| facts.calls_found).sum();
        let calls_dropped: usize = facts.iter().map(|(_, facts)| facts.calls_dropped).sum();
        println!(
            "field reads over the crate: found {reads_found}, counted as live {reads_live}, dropped as not live {reads_dropped}"
        );
        println!("calls over the crate: found {calls_found}, dropped as not live {calls_dropped}");
        assert_eq!(
            reads_live + reads_dropped,
            reads_found,
            "instrument: every read found is counted or dropped once"
        );
        assert!(
            reads_dropped > 0,
            "instrument: the crate's test modules read Cli fields, so a model that drops no read is broken"
        );

        // Instrument 6b (design W4-D28 part 2): the file component of every
        // parse is the module tree's answer for that source. The claim is the
        // wiring, which is what this decision site does, and its two
        // falsifiers are the two directions of the same binding: forced true
        // reports one mismatch, forced false reports one per Live source.
        //
        // What the round 4 instrument claimed here and round 5 no longer
        // claims: that the file component is what keeps a source no non-test
        // build compiles out of the census's answer. Both components of the
        // read's liveness conjunction reject every read and every call the one
        // such source holds, so no single-site plant can falsify that claim,
        // and this unit does not assert what nothing can falsify.
        let mut file_live_mismatches: Vec<&Path> = Vec::new();
        let mut told_not_live = 0usize;
        for (source, source_facts) in &facts {
            if source_facts.file_live != crate_liveness.is_live(&source.path) {
                file_live_mismatches.push(source.path.as_path());
            }
            if !source_facts.file_live {
                told_not_live += 1;
            }
        }
        println!(
            "sources whose parse used a file liveness other than the module tree's: {}",
            file_live_mismatches.len()
        );
        for path in &file_live_mismatches {
            println!("  {}", path.strip_prefix(&src).unwrap_or(path).display());
        }
        println!("sources the parse was told are not Live: {told_not_live}");
        assert!(
            file_live_mismatches.is_empty(),
            "every parsed source's file liveness must be the module tree's answer for that source: {file_live_mismatches:?}"
        );
        assert_eq!(
            told_not_live,
            test_only + unreachable,
            "every source the module tree does not call Live is parsed as not Live, and no other source is"
        );
        // `told_not_live > 0` is printed above and deliberately not asserted.
        // Instrument 4 already asserts `test_only > 0`, and the equality just
        // above ties this figure to `test_only + unreachable`, so no plant can
        // leave this figure at zero and reach an assertion of it: the equality
        // fires first. The vacuity guard lives in instrument 4, and this unit
        // does not assert what nothing can falsify.

        // Instrument 6c (design W4-D28 part 3): in a source the module tree
        // calls `Live` the file component is true, so a read dropped there is
        // the node component's own verdict. One figure per read path, one
        // falsifier each, and the crate-wide `reads_dropped` figure above
        // stays independent of both: a plant that empties one path leaves the
        // other path and the source that is not Live dropping.
        let mut live_reads_dropped_field = 0usize;
        let mut live_reads_dropped_token = 0usize;
        for (source, source_facts) in &facts {
            if crate_liveness.is_live(&source.path) {
                live_reads_dropped_field += source_facts.reads_dropped_field;
                live_reads_dropped_token += source_facts.reads_dropped_token;
            }
        }
        println!(
            "field-expression reads in Live sources the model drops: {live_reads_dropped_field}"
        );
        println!("token-tree reads in Live sources the model drops: {live_reads_dropped_token}");
        assert!(
            live_reads_dropped_field > 0,
            "a field-expression read dropped inside a Live source is the node component's own verdict; with none, nothing tells that component from a constant true"
        );
        assert!(
            live_reads_dropped_token > 0,
            "a token-tree read dropped inside a Live source is the node component's own verdict; with none, nothing tells that component from a constant true"
        );

        // Instrument 7: args/mod.rs holds `self` field expressions.
        let args_facts = &facts
            .iter()
            .find(|(source, _)| source.path == args_mod_path)
            .expect("instrument: args/mod.rs is among the parsed sources")
            .1;
        let self_field_expressions = args_facts
            .reads
            .iter()
            .filter(|read| read.receiver_is_self && !read.in_token_tree)
            .count();
        println!("self field_expression reads inside args/mod.rs: {self_field_expressions}");
        assert!(
            self_field_expressions > 0,
            "instrument: the parse found no self field_expression read in args/mod.rs"
        );

        let outside: Vec<&ParseFacts> = facts
            .iter()
            .filter(|(source, _)| source.path != args_mod_path)
            .map(|(_, facts)| facts)
            .collect();
        println!("sources outside args/mod.rs: {}", outside.len());
        assert!(
            !outside.is_empty(),
            "instrument: no sources outside args/mod.rs"
        );
        assert_eq!(
            outside.len() + 1,
            parsed.len(),
            "args/mod.rs is excluded exactly once"
        );
        let fields_read_outside = live_fields_read(outside.iter().copied());
        let token_tree_reads_outside = outside
            .iter()
            .flat_map(|facts| facts.reads.iter())
            .filter(|read| read.live && read.in_token_tree)
            .count();
        let called_outside: BTreeSet<&str> = outside
            .iter()
            .flat_map(|facts| facts.called_names.iter().map(String::as_str))
            .collect();
        println!(
            "distinct field names read outside args/mod.rs (live): {}",
            fields_read_outside.len()
        );
        println!(
            "live field reads found inside macro token trees outside args/mod.rs: {token_tree_reads_outside}"
        );
        println!(
            "distinct called names outside args/mod.rs (live): {}",
            called_outside.len()
        );

        let ids = top_level_cli_ids();
        println!(
            "top-level Cli ids (clap, minus help/version): {}",
            ids.len()
        );
        assert!(
            ids.len() > 30,
            "instrument: the top-level walk found {} ids",
            ids.len()
        );

        // Instrument 8: the rename map is a bijection between the top-level
        // ids and the Cli fields that are arguments.
        let fields = cli_struct_fields(&args_mod);
        let argument_fields: BTreeSet<String> = fields
            .iter()
            .filter(|field| field.is_argument)
            .map(|field| field.name.clone())
            .collect();
        let renamed = renamed_ids(&args_mod);
        println!(
            "Cli fields: {}, of which arguments: {}; ids renamed by #[arg(id = ...)]: {}",
            fields.len(),
            argument_fields.len(),
            renamed.len()
        );
        let field_of = |id: &String| renamed.get(id).cloned().unwrap_or_else(|| id.clone());
        let ids_without_field: Vec<&String> = ids
            .iter()
            .filter(|id| !argument_fields.contains(&field_of(id)))
            .collect();
        let mapped_fields: BTreeSet<String> = ids.iter().map(field_of).collect();
        let fields_without_id: Vec<&String> = argument_fields
            .iter()
            .filter(|field| !mapped_fields.contains(*field))
            .collect();
        println!(
            "rename map: ids with no argument field {}, argument fields with no id {}, distinct fields mapped {}",
            ids_without_field.len(),
            fields_without_id.len(),
            mapped_fields.len()
        );
        assert!(
            ids_without_field.is_empty(),
            "instrument: every id maps to a Cli argument field: {ids_without_field:?}"
        );
        assert!(
            fields_without_id.is_empty(),
            "instrument: every Cli argument field is an id: {fields_without_id:?}"
        );
        assert_eq!(
            mapped_fields.len(),
            ids.len(),
            "instrument: no two ids map to one field"
        );

        let mut checked = 0usize;
        let mut read_directly = 0usize;
        let mut read_through_a_method = 0usize;
        let mut unread: BTreeSet<String> = BTreeSet::new();
        for id in &ids {
            checked += 1;
            let field = field_of(id);
            if fields_read_outside.contains(field.as_str()) {
                read_directly += 1;
                continue;
            }
            // A `self.<field>` read inside a live `Cli` method counts when
            // that method is called from live code outside the module.
            let methods = cli_methods_reading(args_facts, &field);
            if methods
                .iter()
                .any(|method| called_outside.contains(method.as_str()))
            {
                read_through_a_method += 1;
                continue;
            }
            unread.insert(id.clone());
        }
        println!("ids checked: {checked}");
        println!("ids read outside args/mod.rs: {read_directly}");
        println!("ids read through a Cli method called from outside: {read_through_a_method}");
        assert_eq!(checked, ids.len(), "every walked id was checked");
        assert_eq!(
            read_directly + read_through_a_method + unread.len(),
            checked,
            "every checked id is classified exactly once"
        );
        println!("top-level ids nothing reads: {}", unread.len());
        for id in &unread {
            println!("  {id}");
        }
        assert!(
            unread.is_empty(),
            "every top-level Cli field must be read, in code a non-test build compiles, outside \
             args/mod.rs or through a live Cli method with a live outside caller; unread: {unread:?}"
        );
    });
}
