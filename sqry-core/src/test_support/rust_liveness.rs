//! What a build of a Rust crate compiles, a non-test build or a test build,
//! decided over tree-sitter parses of its sources (surface parity W4 round 3,
//! design W4-D18).
//!
//! Two derived tests read code facts out of parsed source: the declared
//! argument census in `sqry-cli` (a `Cli` field is read) and the environment
//! lock discipline test in `sqry-daemon` (an environment call is test code).
//! Both used to count every node of every file under `src`, so a read that
//! only a `#[cfg(test)]` item, module, file or statement holds counted as a
//! read, and test code outside a `#[cfg(test)]` item counted as production.
//! This module answers one question for both: does this parse node exist in
//! the build a [`BuildCfg`] describes, a non-test build of this crate
//! ([`BuildCfg::new`], [`BuildCfg::from_cargo_metadata`]) or a test build of
//! it ([`BuildCfg::for_test_build`])?
//!
//! The model has five parts.
//!
//! 1. The build configuration, [`BuildCfg`]: the crate roots (the source
//!    path of every library or binary target) and the package's feature
//!    table, from `cargo metadata`. A cfg predicate evaluates to a [`Tri`]
//!    by [`evaluate_cfg`]: `test`, `doc`, `doctest` and `miri` are false; a
//!    feature is true when the `default` feature enables it, unknown when the
//!    package declares it but `default` does not enable it, and false when
//!    the package does not declare it (cargo can never enable it); `all`,
//!    `any` and `not` are three-valued; every other predicate is unknown. A
//!    node is removed only when its existence is false: unknown means "exists
//!    in some non-test build" and is kept. [`BuildCfg::for_test_build`] is
//!    the same package as a test build sees it: there `test` is true, and
//!    every other predicate evaluates as above. Every function below answers
//!    for the build its [`BuildCfg`] describes.
//! 2. Attribute existence, [`attribute_existence`]: `cfg(P)` is P;
//!    `cfg_attr(P, a1, .., an)` is `not(P) or (e(a1) and .. and e(an))`; an
//!    attribute whose path is `test` or `bench`, or ends in `::test`, is false
//!    in a non-test build and true in a test build; any other attribute is
//!    true.
//! 3. Attachment, derived from the grammar: [`attribute_containers_in_grammar`]
//!    reads `tree_sitter_rust::NODE_TYPES` and returns every node kind whose
//!    children may be an attribute item. [`LEADING_ATTRIBUTE_NODES`] and
//!    [`SIBLING_ATTRIBUTE_CONTAINERS`] say how an attribute child applies in
//!    each, and their union must equal the derived set, so a grammar upgrade
//!    that adds a position fails its callers with the new kind named.
//!    [`node_is_live`] walks from a node to the root and answers false when an
//!    attribute applying to any node on that chain has existence false.
//! 4. The module tree, [`crate_liveness`]: every file under the source
//!    directory is [`FileLiveness::Live`], [`FileLiveness::TestOnly`] or
//!    [`FileLiveness::Unreachable`], walking `mod name;` declarations from the
//!    roots with rustc's file rules. What the walk does not model is returned
//!    in [`CrateLiveness::unmodelled`], one entry per site, never guessed.
//! 5. Macros, [`macro_token_is_live`]: a node in a macro invocation's token
//!    tree exists only when the invocation is live, the token tree carries no
//!    attribute-shaped token sequence ([`token_tree_has_attribute_tokens`]),
//!    and the invoked name is not a `macro_rules!` whose transcriber carries
//!    one ([`attribute_bearing_macro_rules`]). A node inside a
//!    `macro_rules!` definition never exists: a transcriber is a template
//!    until it is invoked.
//!
//! What a `true` answer does not show, stated so nobody reads more into it: a
//! macro from another crate whose expansion is test-only is not modelled; a
//! cfg predicate other than the ones part 1 names is unknown and kept (a node
//! under `cfg(target_os = "haiku")` is live, because it exists in a non-test
//! build for that target); a `cfg_attr` nested inside another `cfg_attr`'s
//! attribute list does not gate the nodes in its own arguments; and an inner
//! attribute is read wherever the grammar places it, not only where rustc
//! accepts one.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use tree_sitter::{Node, Tree};

/// Three-valued truth for a cfg predicate or an attribute's existence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tri {
    /// Holds in every non-test build.
    True,
    /// Holds in no non-test build.
    False,
    /// Holds in some non-test builds (a target, a non-default feature).
    Unknown,
}

impl std::ops::Not for Tri {
    type Output = Self;

    fn not(self) -> Self {
        match self {
            Self::True => Self::False,
            Self::False => Self::True,
            Self::Unknown => Self::Unknown,
        }
    }
}

impl Tri {
    /// Three-valued conjunction: false when either side is false.
    #[must_use]
    pub fn and(self, other: Self) -> Self {
        match (self, other) {
            (Self::False, _) | (_, Self::False) => Self::False,
            (Self::True, Self::True) => Self::True,
            _ => Self::Unknown,
        }
    }

    /// Three-valued disjunction: true when either side is true.
    #[must_use]
    pub fn or(self, other: Self) -> Self {
        match (self, other) {
            (Self::True, _) | (_, Self::True) => Self::True,
            (Self::False, Self::False) => Self::False,
            _ => Self::Unknown,
        }
    }

    /// The conjunction of every item; `all()` of nothing is true.
    pub fn all(items: impl IntoIterator<Item = Self>) -> Self {
        items.into_iter().fold(Self::True, Self::and)
    }

    /// The disjunction of every item; `any()` of nothing is false.
    pub fn any(items: impl IntoIterator<Item = Self>) -> Self {
        items.into_iter().fold(Self::False, Self::or)
    }
}

/// The target kinds whose source path is a crate root of a non-test build.
/// `cargo metadata` names a library target by its crate type, so every
/// library crate type is listed beside `lib`.
pub const ROOT_TARGET_KINDS: [&str; 7] = [
    "lib",
    "rlib",
    "dylib",
    "cdylib",
    "staticlib",
    "proc-macro",
    "bin",
];

/// The build configuration a non-test build of one package sees: its crate
/// roots and its feature table. [`BuildCfg::for_test_build`] gives the same
/// package as a test build (`cargo test`) sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildCfg {
    roots: Vec<PathBuf>,
    features: BTreeMap<String, Vec<String>>,
    default_enabled: BTreeSet<String>,
    /// The build is a test build: `cfg(test)` is true and a test harness
    /// attribute keeps its item.
    test: bool,
}

/// `cargo metadata` could not produce a [`BuildCfg`]. Every variant names
/// the command that was run.
#[derive(Debug, thiserror::Error)]
pub enum BuildCfgError {
    /// The cargo process could not be started.
    #[error("`{command}` could not be started: {source}")]
    Spawn {
        /// The command line.
        command: String,
        /// The spawn error.
        source: std::io::Error,
    },
    /// Cargo exited unsuccessfully.
    #[error("`{command}` exited with {status}: {stderr}")]
    Failed {
        /// The command line.
        command: String,
        /// The exit status.
        status: String,
        /// Cargo's standard error.
        stderr: String,
    },
    /// Cargo's output is not JSON.
    #[error("`{command}` printed output that is not JSON: {source}")]
    Json {
        /// The command line.
        command: String,
        /// The parse error.
        source: serde_json::Error,
    },
    /// The document does not describe the package the manifest names.
    #[error("`{command}`: {reason}")]
    Document {
        /// The command line.
        command: String,
        /// What the document lacks.
        reason: String,
    },
}

impl BuildCfg {
    /// A configuration from explicit roots and a feature table (the unit
    /// tests build one without cargo). The set of features `default`
    /// enables is computed here.
    #[must_use]
    pub fn new(roots: Vec<PathBuf>, features: BTreeMap<String, Vec<String>>) -> Self {
        let mut roots: Vec<PathBuf> = roots.iter().map(|root| normalize(root)).collect();
        roots.sort();
        roots.dedup();
        let default_enabled = default_closure(&features);
        Self {
            roots,
            features,
            default_enabled,
            test: false,
        }
    }

    /// The same package as a test build sees it (the build `cargo test`
    /// makes of a library or binary target): `cfg(test)` is true, and an
    /// attribute whose path is `test` or `bench`, or ends in `::test`, keeps
    /// its item. Every other predicate evaluates as in the non-test build, so
    /// a node is live in both builds only when no attribute on its chain
    /// removes it in either.
    #[must_use]
    pub fn for_test_build(&self) -> Self {
        Self {
            test: true,
            ..self.clone()
        }
    }

    /// Whether this configuration is a test build ([`BuildCfg::for_test_build`]).
    #[must_use]
    pub fn is_test_build(&self) -> bool {
        self.test
    }

    /// Runs `<cargo> metadata --no-deps --offline --format-version 1
    /// --manifest-path <manifest_path>` and reads the package whose manifest
    /// is `manifest_path`. A test passes `env!("CARGO")`.
    ///
    /// # Errors
    ///
    /// [`BuildCfgError`], naming the command, when cargo cannot be started,
    /// fails, prints something other than JSON, or describes no package at
    /// that manifest path.
    pub fn from_cargo_metadata(cargo: &Path, manifest_path: &Path) -> Result<Self, BuildCfgError> {
        let arguments = [
            "metadata",
            "--no-deps",
            "--offline",
            "--format-version",
            "1",
            "--manifest-path",
        ];
        let command = format!(
            "{} {} {}",
            cargo.display(),
            arguments.join(" "),
            manifest_path.display()
        );
        let output = Command::new(cargo)
            .args(arguments)
            .arg(manifest_path)
            .output()
            .map_err(|source| BuildCfgError::Spawn {
                command: command.clone(),
                source,
            })?;
        if !output.status.success() {
            return Err(BuildCfgError::Failed {
                command,
                status: output.status.to_string(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }
        let document: serde_json::Value =
            serde_json::from_slice(&output.stdout).map_err(|source| BuildCfgError::Json {
                command: command.clone(),
                source,
            })?;
        Self::from_metadata_document(&document, manifest_path)
            .map_err(|reason| BuildCfgError::Document { command, reason })
    }

    /// Reads the roots and the feature table of the package whose manifest
    /// is `manifest_path` out of a parsed `cargo metadata --format-version 1`
    /// document. Paths are compared canonicalised when both exist on disk and
    /// lexically normalised otherwise.
    ///
    /// # Errors
    ///
    /// A sentence naming what the document lacks: no `packages` array, no
    /// package at that manifest path, or a package with no root target.
    pub fn from_metadata_document(
        document: &serde_json::Value,
        manifest_path: &Path,
    ) -> Result<Self, String> {
        let wanted = comparable(manifest_path);
        let packages = document
            .get("packages")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| "the document has no packages array".to_string())?;
        let package = packages
            .iter()
            .find(|package| {
                package
                    .get("manifest_path")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|path| comparable(Path::new(path)) == wanted)
            })
            .ok_or_else(|| {
                format!(
                    "no package among {} has the manifest {}",
                    packages.len(),
                    manifest_path.display()
                )
            })?;
        let mut roots = Vec::new();
        for target in package
            .get("targets")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
        {
            let is_root = target
                .get("kind")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(serde_json::Value::as_str)
                .any(|kind| ROOT_TARGET_KINDS.contains(&kind));
            if let (true, Some(path)) = (
                is_root,
                target.get("src_path").and_then(serde_json::Value::as_str),
            ) {
                roots.push(PathBuf::from(path));
            }
        }
        if roots.is_empty() {
            return Err(format!(
                "the package at {} has no library or binary target",
                manifest_path.display()
            ));
        }
        let mut features = BTreeMap::new();
        if let Some(table) = package
            .get("features")
            .and_then(serde_json::Value::as_object)
        {
            for (name, enables) in table {
                let enables = enables
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_string)
                    .collect();
                features.insert(name.clone(), enables);
            }
        }
        Ok(Self::new(roots, features))
    }

    /// The crate roots, lexically normalised, sorted and de-duplicated.
    #[must_use]
    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// The package's feature table.
    #[must_use]
    pub fn features(&self) -> &BTreeMap<String, Vec<String>> {
        &self.features
    }

    /// The package's own features the `default` feature enables, `default`
    /// itself included when it is declared.
    #[must_use]
    pub fn default_enabled_features(&self) -> &BTreeSet<String> {
        &self.default_enabled
    }

    /// `cfg(feature = "<name>")` in a build with the default features: true
    /// when `default` enables it, unknown when it is declared, false when it
    /// is not declared.
    #[must_use]
    pub fn feature(&self, name: &str) -> Tri {
        if self.default_enabled.contains(name) {
            Tri::True
        } else if self.features.contains_key(name) {
            Tri::Unknown
        } else {
            Tri::False
        }
    }
}

/// The package's own features reachable from `default`: an entry naming a
/// feature enables it; `dep:x` enables none; `x/y` enables the feature `x`
/// when the package declares one; `x?/y` enables none.
fn default_closure(features: &BTreeMap<String, Vec<String>>) -> BTreeSet<String> {
    let mut enabled = BTreeSet::new();
    let mut queue = VecDeque::from(["default".to_string()]);
    while let Some(name) = queue.pop_front() {
        let Some(entries) = features.get(&name) else {
            continue;
        };
        if !enabled.insert(name) {
            continue;
        }
        for entry in entries {
            if entry.starts_with("dep:") {
                continue;
            }
            let own = match entry.split_once('/') {
                Some((dependency, _)) if dependency.ends_with('?') => continue,
                Some((dependency, _)) => dependency,
                None => entry.as_str(),
            };
            if features.contains_key(own) {
                queue.push_back(own.to_string());
            }
        }
    }
    enabled
}

/// A path with `.` removed and `..` folded, without touching the disk.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// A manifest path in the form two paths are compared in.
fn comparable(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| normalize(path))
}

/// A cfg predicate as the parse holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CfgPredicate {
    /// A bare option such as `test` or `unix`.
    Name(String),
    /// A key and a string value such as `feature = "x"`.
    KeyValue {
        /// The key, `feature` or `target_os` for instance.
        key: String,
        /// The string literal's content.
        value: String,
    },
    /// `all(..)`.
    All(Vec<CfgPredicate>),
    /// `any(..)`.
    Any(Vec<CfgPredicate>),
    /// `not(..)`.
    Not(Box<CfgPredicate>),
    /// Tokens that are not a predicate (a build rejects them).
    Unparsed(String),
}

impl CfgPredicate {
    /// The predicate `text` would be inside `#[cfg(..)]`, read through the
    /// same parse the model reads attributes with. `None` when the text does
    /// not parse as one attribute argument.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let source = format!("#[cfg({text})]\nfn cfg_predicate_probe() {{}}\n");
        let tree = rust_tree(&source)?;
        let root = tree.root_node();
        let attribute_item = named_children(root)
            .into_iter()
            .find(|child| child.kind() == "attribute_item")?;
        let arguments = attribute_node(attribute_item)?.child_by_field_name("arguments")?;
        let tokens = group_tokens(arguments, source.as_bytes());
        let parts = split_top_level_commas(&tokens);
        match parts.as_slice() {
            [single] => Some(parse_predicate(single)),
            _ => None,
        }
    }
}

/// Evaluates a cfg predicate for a non-test build with the default features
/// (design W4-D18 part 1), or for a test build of the same package when `cfg`
/// is one ([`BuildCfg::for_test_build`]): there `test` is true.
#[must_use]
pub fn evaluate_cfg(predicate: &CfgPredicate, cfg: &BuildCfg) -> Tri {
    match predicate {
        CfgPredicate::Name(name) => match name.as_str() {
            "test" if cfg.is_test_build() => Tri::True,
            "test" | "doc" | "doctest" | "miri" => Tri::False,
            _ => Tri::Unknown,
        },
        CfgPredicate::KeyValue { key, value } => {
            if key == "feature" {
                cfg.feature(value)
            } else {
                Tri::Unknown
            }
        }
        CfgPredicate::All(members) => Tri::all(members.iter().map(|m| evaluate_cfg(m, cfg))),
        CfgPredicate::Any(members) => Tri::any(members.iter().map(|m| evaluate_cfg(m, cfg))),
        CfgPredicate::Not(inner) => !evaluate_cfg(inner, cfg),
        CfgPredicate::Unparsed(_) => Tri::Unknown,
    }
}

/// One token of an attribute argument list, as the parse delimits it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Ident(String),
    Punct(String),
    Str(String),
    Group(Vec<Token>),
    Other(String),
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ident(text) | Self::Punct(text) | Self::Other(text) => write!(f, "{text}"),
            Self::Str(text) => write!(f, "{text:?}"),
            Self::Group(tokens) => {
                write!(f, "(")?;
                for (index, token) in tokens.iter().enumerate() {
                    if index > 0 {
                        write!(f, " ")?;
                    }
                    write!(f, "{token}")?;
                }
                write!(f, ")")
            }
        }
    }
}

fn text<'s>(node: Node<'_>, source: &'s [u8]) -> &'s str {
    node.utf8_text(source).unwrap_or("")
}

fn compact(node: Node<'_>, source: &[u8]) -> String {
    text(node, source)
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect()
}

fn children(node: Node<'_>) -> Vec<Node<'_>> {
    let mut cursor = node.walk();
    node.children(&mut cursor).collect()
}

fn named_children(node: Node<'_>) -> Vec<Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

fn is_comment(node: Node<'_>) -> bool {
    matches!(node.kind(), "line_comment" | "block_comment")
}

fn rust_tree(source: &str) -> Option<Tree> {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .ok()?;
    parser.parse(source, None)
}

/// The content of a string or raw string literal node.
fn string_content(literal: Node<'_>, source: &[u8]) -> String {
    named_children(literal)
        .into_iter()
        .filter(|child| matches!(child.kind(), "string_content" | "escape_sequence"))
        .map(|child| text(child, source))
        .collect()
}

/// The tokens between a token tree's delimiters, comments dropped.
fn group_tokens(token_tree: Node<'_>, source: &[u8]) -> Vec<Token> {
    let all = children(token_tree);
    let inner = match all.len() {
        0..=2 => &[][..],
        len => &all[1..len - 1],
    };
    let mut out = Vec::new();
    for child in inner {
        match child.kind() {
            "line_comment" | "block_comment" => {}
            "identifier" => out.push(Token::Ident(text(*child, source).to_string())),
            "string_literal" | "raw_string_literal" => {
                out.push(Token::Str(string_content(*child, source)));
            }
            "token_tree" => out.push(Token::Group(group_tokens(*child, source))),
            _ if !child.is_named() => out.push(Token::Punct(text(*child, source).to_string())),
            _ => out.push(Token::Other(text(*child, source).to_string())),
        }
    }
    out
}

/// `tokens` split at every top-level comma; a trailing empty segment (a
/// trailing comma) is dropped.
fn split_top_level_commas(tokens: &[Token]) -> Vec<&[Token]> {
    if tokens.is_empty() {
        return Vec::new();
    }
    let mut parts = Vec::new();
    let mut start = 0;
    for (index, token) in tokens.iter().enumerate() {
        if matches!(token, Token::Punct(p) if p == ",") {
            parts.push(&tokens[start..index]);
            start = index + 1;
        }
    }
    if start < tokens.len() {
        parts.push(&tokens[start..]);
    }
    parts
}

fn render(tokens: &[Token]) -> String {
    tokens
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(" ")
}

fn parse_predicate(tokens: &[Token]) -> CfgPredicate {
    match tokens {
        [Token::Ident(name)] => CfgPredicate::Name(name.clone()),
        [Token::Ident(key), Token::Punct(eq), Token::Str(value)] if eq == "=" => {
            CfgPredicate::KeyValue {
                key: key.clone(),
                value: value.clone(),
            }
        }
        [Token::Ident(operator), Token::Group(inner)] => {
            let members = split_top_level_commas(inner);
            match operator.as_str() {
                "all" => CfgPredicate::All(members.into_iter().map(parse_predicate).collect()),
                "any" => CfgPredicate::Any(members.into_iter().map(parse_predicate).collect()),
                "not" => match members.as_slice() {
                    [single] => CfgPredicate::Not(Box::new(parse_predicate(single))),
                    _ => CfgPredicate::Unparsed(render(tokens)),
                },
                _ => CfgPredicate::Unparsed(render(tokens)),
            }
        }
        _ => CfgPredicate::Unparsed(render(tokens)),
    }
}

/// The `attribute` node inside an `attribute_item` or
/// `inner_attribute_item`.
fn attribute_node(item: Node<'_>) -> Option<Node<'_>> {
    named_children(item)
        .into_iter()
        .find(|child| child.kind() == "attribute")
}

/// The compacted path of an attribute item (`cfg`, `tokio::test`).
fn attribute_path(item: Node<'_>, source: &[u8]) -> Option<String> {
    let attribute = attribute_node(item)?;
    named_children(attribute)
        .into_iter()
        .next()
        .map(|path| compact(path, source))
}

fn is_test_harness_path(path: &str) -> bool {
    path == "test" || path == "bench" || path.ends_with("::test")
}

/// The existence of an attribute given its path and its argument tokens.
fn existence_of(path: &str, arguments: Option<&[Token]>, cfg: &BuildCfg) -> Tri {
    match path {
        "cfg" => match arguments.map(split_top_level_commas).as_deref() {
            Some([single]) => evaluate_cfg(&parse_predicate(single), cfg),
            _ => Tri::Unknown,
        },
        "cfg_attr" => {
            let Some(parts) = arguments.map(split_top_level_commas) else {
                return Tri::Unknown;
            };
            let Some((predicate, attributes)) = parts.split_first() else {
                return Tri::Unknown;
            };
            let predicate = evaluate_cfg(&parse_predicate(predicate), cfg);
            let applied = Tri::all(
                attributes
                    .iter()
                    .map(|attribute| token_attribute_existence(attribute, cfg)),
            );
            (!predicate).or(applied)
        }
        _ if is_test_harness_path(path) && !cfg.is_test_build() => Tri::False,
        _ => Tri::True,
    }
}

/// Splits an attribute written as tokens (inside `cfg_attr`) into its path
/// and its argument tokens.
fn token_attribute_parts(tokens: &[Token]) -> (String, Option<&[Token]>) {
    let mut path = String::new();
    for token in tokens {
        match token {
            Token::Ident(name) => path.push_str(name),
            Token::Punct(p) if p == "::" => path.push_str("::"),
            Token::Group(inner) => return (path, Some(inner.as_slice())),
            _ => return (path, None),
        }
    }
    (path, None)
}

fn token_attribute_existence(tokens: &[Token], cfg: &BuildCfg) -> Tri {
    let (path, arguments) = token_attribute_parts(tokens);
    existence_of(&path, arguments, cfg)
}

/// The existence of one `attribute_item` or `inner_attribute_item` in a
/// non-test build (design W4-D18 part 2).
#[must_use]
pub fn attribute_existence(attribute_item: Node<'_>, source: &[u8], cfg: &BuildCfg) -> Tri {
    let Some(attribute) = attribute_node(attribute_item) else {
        return Tri::True;
    };
    let Some(path) = attribute_path(attribute_item, source) else {
        return Tri::True;
    };
    let arguments = attribute
        .child_by_field_name("arguments")
        .map(|tree| group_tokens(tree, source));
    existence_of(&path, arguments.as_deref(), cfg)
}

/// Node kinds whose own attribute children apply to the node itself.
pub const LEADING_ATTRIBUTE_NODES: [&str; 3] = [
    "field_initializer",
    "match_arm",
    "shorthand_field_initializer",
];

/// Node kinds whose attribute children apply to the next sibling that is
/// neither an attribute nor a comment (in `ordered_field_declaration_list`,
/// nor the field's visibility modifier, which the grammar places between the
/// attribute and the field's type).
pub const SIBLING_ATTRIBUTE_CONTAINERS: [&str; 11] = [
    "arguments",
    "array_expression",
    "block",
    "declaration_list",
    "enum_variant_list",
    "field_declaration_list",
    "ordered_field_declaration_list",
    "parameters",
    "source_file",
    "tuple_expression",
    "type_parameters",
];

/// The union of [`LEADING_ATTRIBUTE_NODES`] and
/// [`SIBLING_ATTRIBUTE_CONTAINERS`], the set every caller compares with
/// [`attribute_containers_in_grammar`].
#[must_use]
pub fn declared_attribute_containers() -> BTreeSet<String> {
    LEADING_ATTRIBUTE_NODES
        .iter()
        .chain(SIBLING_ATTRIBUTE_CONTAINERS.iter())
        .map(|kind| (*kind).to_string())
        .collect()
}

/// Every node kind of `tree_sitter_rust::NODE_TYPES` whose children or
/// fields may be an `attribute_item` or an `inner_attribute_item`, directly
/// or through a supertype. Derived at run time, so a grammar upgrade that
/// adds an attribute position changes this set.
///
/// # Errors
///
/// The JSON error when `NODE_TYPES` does not parse.
pub fn attribute_containers_in_grammar() -> Result<BTreeSet<String>, serde_json::Error> {
    let node_types: serde_json::Value = serde_json::from_str(tree_sitter_rust::NODE_TYPES)?;
    let entries = node_types.as_array().map(Vec::as_slice).unwrap_or_default();
    let mut subtypes: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for entry in entries {
        if let (Some(kind), Some(list)) = (
            entry.get("type").and_then(serde_json::Value::as_str),
            entry.get("subtypes").and_then(serde_json::Value::as_array),
        ) {
            subtypes.insert(
                kind,
                list.iter()
                    .filter_map(|s| s.get("type").and_then(serde_json::Value::as_str))
                    .collect(),
            );
        }
    }
    let attribute_kinds = ["attribute_item", "inner_attribute_item"];
    let reaches_attribute = |kind: &str| -> bool {
        let mut stack = vec![kind];
        let mut seen = BTreeSet::new();
        while let Some(current) = stack.pop() {
            if attribute_kinds.contains(&current) {
                return true;
            }
            if seen.insert(current)
                && let Some(list) = subtypes.get(current)
            {
                stack.extend(list.iter().copied());
            }
        }
        false
    };
    let mut out = BTreeSet::new();
    for entry in entries {
        let Some(kind) = entry.get("type").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let mut child_lists = Vec::new();
        if let Some(children) = entry.get("children") {
            child_lists.push(children);
        }
        if let Some(fields) = entry.get("fields").and_then(serde_json::Value::as_object) {
            child_lists.extend(fields.values());
        }
        let holds_attribute = child_lists
            .iter()
            .filter_map(|list| list.get("types").and_then(serde_json::Value::as_array))
            .flatten()
            .filter_map(|t| t.get("type").and_then(serde_json::Value::as_str))
            .any(reaches_attribute);
        if holds_attribute {
            out.insert(kind.to_string());
        }
    }
    Ok(out)
}

/// The attribute items that apply to `node` itself: its own leading
/// attribute children (for a leading node), the attributes immediately
/// before it (in a sibling container), and its own inner attribute children.
fn applying_attributes(node: Node<'_>) -> Vec<Node<'_>> {
    let mut out = Vec::new();
    let kind = node.kind();
    if LEADING_ATTRIBUTE_NODES.contains(&kind) {
        out.extend(
            children(node)
                .into_iter()
                .filter(|child| matches!(child.kind(), "attribute_item" | "inner_attribute_item")),
        );
    } else {
        out.extend(
            children(node)
                .into_iter()
                .filter(|child| child.kind() == "inner_attribute_item"),
        );
    }
    if let Some(parent) = node.parent()
        && SIBLING_ATTRIBUTE_CONTAINERS.contains(&parent.kind())
        && !matches!(kind, "attribute_item" | "inner_attribute_item")
        && !is_comment(node)
    {
        let ordered = parent.kind() == "ordered_field_declaration_list";
        let mut previous = node.prev_sibling();
        while let Some(sibling) = previous {
            match sibling.kind() {
                "attribute_item" => out.push(sibling),
                "line_comment" | "block_comment" => {}
                "visibility_modifier" if ordered => {}
                _ => break,
            }
            previous = sibling.prev_sibling();
        }
    }
    out
}

/// The node an attribute item applies to, or `None` for an attribute the
/// grammar left with nothing after it.
fn attribute_target(attribute: Node<'_>) -> Option<Node<'_>> {
    let parent = attribute.parent()?;
    if attribute.kind() == "inner_attribute_item"
        || LEADING_ATTRIBUTE_NODES.contains(&parent.kind())
    {
        return Some(parent);
    }
    if !SIBLING_ATTRIBUTE_CONTAINERS.contains(&parent.kind()) {
        return None;
    }
    let ordered = parent.kind() == "ordered_field_declaration_list";
    let mut next = attribute.next_sibling();
    while let Some(sibling) = next {
        match sibling.kind() {
            "attribute_item" | "line_comment" | "block_comment" => {}
            "visibility_modifier" if ordered => {}
            _ => return sibling.is_named().then_some(sibling),
        }
        next = sibling.next_sibling();
    }
    None
}

/// When `arguments` is the argument token tree of a `cfg_attr` attribute and
/// `child` sits after its first top-level comma (in the attribute list, not
/// in the predicate), the predicate's value; otherwise true.
fn cfg_attr_gate(arguments: Node<'_>, child: Node<'_>, source: &[u8], cfg: &BuildCfg) -> Tri {
    let Some(attribute) = arguments.parent() else {
        return Tri::True;
    };
    let is_cfg_attr_arguments = attribute.kind() == "attribute"
        && attribute
            .child_by_field_name("arguments")
            .is_some_and(|node| node.id() == arguments.id())
        && named_children(attribute)
            .first()
            .is_some_and(|path| compact(*path, source) == "cfg_attr");
    if !is_cfg_attr_arguments {
        return Tri::True;
    }
    let mut after_comma = false;
    for sibling in children(arguments) {
        if sibling.id() == child.id() {
            break;
        }
        if !sibling.is_named() && text(sibling, source) == "," {
            after_comma = true;
        }
    }
    if !after_comma {
        return Tri::True;
    }
    let tokens = group_tokens(arguments, source);
    match split_top_level_commas(&tokens).first() {
        Some(predicate) => evaluate_cfg(&parse_predicate(predicate), cfg),
        None => Tri::Unknown,
    }
}

/// Whether `node` exists in a non-test build of the file it was parsed from
/// (design W4-D18 part 3): false when an attribute applying to any node on
/// the chain from `node` to the root has existence false. A node inside an
/// attribute continues the chain at the node the attribute applies to. The
/// file's own place in the module tree is [`crate_liveness`]'s question, not
/// this one's.
#[must_use]
pub fn node_is_live(node: Node<'_>, source: &[u8], cfg: &BuildCfg) -> bool {
    let mut previous: Option<Node<'_>> = None;
    let mut current = Some(node);
    while let Some(here) = current {
        if here.kind() == "token_tree"
            && let Some(child) = previous
            && cfg_attr_gate(here, child, source, cfg) == Tri::False
        {
            return false;
        }
        if applying_attributes(here)
            .into_iter()
            .any(|attribute| attribute_existence(attribute, source, cfg) == Tri::False)
        {
            return false;
        }
        previous = Some(here);
        current = match here.kind() {
            "attribute_item" | "inner_attribute_item" => match attribute_target(here) {
                Some(target) => Some(target),
                None => return false,
            },
            _ => here.parent(),
        };
    }
    true
}

/// Whether a node inside a macro token tree exists in a non-test build
/// (design W4-D18 part 5). Inside a `macro_rules!` definition: never. Inside
/// a macro invocation's token tree: when the invocation is live, the token
/// tree carries no attribute-shaped token sequence, and the invoked name is
/// not in `attribute_bearing` (the crate's [`attribute_bearing_macro_rules`]).
/// Anywhere else (an attribute's arguments, ordinary code): [`node_is_live`].
#[must_use]
pub fn macro_token_is_live(
    node: Node<'_>,
    source: &[u8],
    cfg: &BuildCfg,
    attribute_bearing: &BTreeSet<String>,
) -> bool {
    let mut inner = node;
    while let Some(parent) = inner.parent() {
        match parent.kind() {
            "macro_definition" | "macro_rule" => return false,
            "token_tree"
            | "token_repetition"
            | "token_tree_pattern"
            | "token_repetition_pattern"
            | "token_binding_pattern" => inner = parent,
            "macro_invocation" => {
                if !node_is_live(parent, source, cfg)
                    || token_tree_has_attribute_tokens(inner, source)
                {
                    return false;
                }
                return macro_invocation_name(parent, source)
                    .is_none_or(|name| !attribute_bearing.contains(&name));
            }
            _ => break,
        }
    }
    node_is_live(node, source, cfg)
}

/// The last path segment of a macro invocation's name.
fn macro_invocation_name(invocation: Node<'_>, source: &[u8]) -> Option<String> {
    let name = invocation.child_by_field_name("macro")?;
    let compacted = compact(name, source);
    Some(
        compacted
            .rsplit("::")
            .next()
            .unwrap_or(compacted.as_str())
            .to_string(),
    )
}

/// Whether a token tree (or a transcriber's token repetition) holds, at any
/// depth, an attribute-shaped token sequence: a `#` token, optionally
/// followed by `!`, followed by a bracketed token tree.
#[must_use]
pub fn token_tree_has_attribute_tokens(token_tree: Node<'_>, source: &[u8]) -> bool {
    let all: Vec<Node<'_>> = children(token_tree)
        .into_iter()
        .filter(|child| !is_comment(*child))
        .collect();
    for (index, child) in all.iter().enumerate() {
        match child.kind() {
            "token_tree" | "token_repetition" => {
                if token_tree_has_attribute_tokens(*child, source) {
                    return true;
                }
            }
            "#" if !child.is_named() => {
                let mut next = index + 1;
                if all
                    .get(next)
                    .is_some_and(|node| !node.is_named() && node.kind() == "!")
                {
                    next += 1;
                }
                if let Some(bracketed) = all.get(next)
                    && bracketed.kind() == "token_tree"
                    && bracketed
                        .child(0)
                        .is_some_and(|open| text(open, source) == "[")
                {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

/// The names of the `macro_rules!` definitions in one parse whose
/// transcriber (the right-hand side of any rule) carries an attribute-shaped
/// token sequence. A caller unions this over every file of the crate.
#[must_use]
pub fn attribute_bearing_macro_rules(tree: &Tree, source: &[u8]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.kind() == "macro_definition" {
            let bearing = named_children(node)
                .into_iter()
                .filter(|child| child.kind() == "macro_rule")
                .filter_map(|rule| rule.child_by_field_name("right"))
                .any(|right| token_tree_has_attribute_tokens(right, source));
            if bearing && let Some(name) = node.child_by_field_name("name") {
                out.insert(text(name, source).to_string());
            }
        }
        stack.extend(children(node));
    }
    out
}

/// Where a file sits in the module tree of a non-test build.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FileLiveness {
    /// Reached from a root through live declarations from live files.
    Live,
    /// Reached, but only through a declaration that is not live, only from a
    /// file that is not live, or carrying an inner attribute at its root whose
    /// existence is false.
    TestOnly,
    /// Reached by no declaration from any root: no build compiles it.
    Unreachable,
}

/// One parsed source file handed to [`crate_liveness`].
#[derive(Debug, Clone, Copy)]
pub struct RustSource<'a> {
    /// The file's path.
    pub path: &'a Path,
    /// The file's bytes, as parsed.
    pub source: &'a [u8],
    /// The parse.
    pub tree: &'a Tree,
}

/// The classification of every file handed to [`crate_liveness`], and what
/// the walk did not model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrateLiveness {
    /// Every file, by its lexically normalised path.
    pub files: BTreeMap<PathBuf, FileLiveness>,
    /// One entry per site the walk does not model, naming the file and the
    /// site: a `#[path]` inside an inline module or on one, a `cfg_attr`
    /// carrying `path` or `cfg` on a module declaration, a declaration with no
    /// candidate file or with two, a declaration outside a module body, an
    /// `include!` invocation, or a root that is not among the files. Sites are
    /// reported for every file a walk reaches (live or test-only); a file no
    /// walk reaches is compiled by no build and reports none.
    pub unmodelled: Vec<String>,
}

impl CrateLiveness {
    /// The classification of `path`, when it was among the files.
    #[must_use]
    pub fn get(&self, path: &Path) -> Option<FileLiveness> {
        self.files.get(&normalize(path)).copied()
    }

    /// How many files carry `class`.
    #[must_use]
    pub fn count(&self, class: FileLiveness) -> usize {
        self.files.values().filter(|value| **value == class).count()
    }

    /// Whether `path` is [`FileLiveness::Live`].
    #[must_use]
    pub fn is_live(&self, path: &Path) -> bool {
        self.get(path) == Some(FileLiveness::Live)
    }
}

/// One `mod name;` declaration as the walk resolves it.
struct Declaration<'t> {
    node: Node<'t>,
    name: String,
    inline_path: Vec<String>,
    /// The enclosing inline `mod x { .. }` items, innermost first.
    enclosing_modules: Vec<Node<'t>>,
}

/// Every `mod name;` item of a parse, or the reason one is not modelled.
fn declarations<'t>(tree: &'t Tree, source: &[u8]) -> Vec<Result<Declaration<'t>, String>> {
    let mut out = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        stack.extend(children(node));
        if node.kind() != "mod_item" || node.child_by_field_name("body").is_some() {
            continue;
        }
        let name = node
            .child_by_field_name("name")
            .map(|name| text(name, source).trim_start_matches("r#").to_string())
            .unwrap_or_default();
        let mut inline_path = Vec::new();
        let mut enclosing_modules = Vec::new();
        let mut parent = node.parent();
        let mut outside = None;
        while let Some(container) = parent {
            let module_owner = container.parent().filter(|owner| {
                container.kind() == "declaration_list" && owner.kind() == "mod_item"
            });
            match (container.kind(), module_owner) {
                ("source_file", _) => break,
                (_, Some(owner)) => {
                    inline_path.push(
                        owner
                            .child_by_field_name("name")
                            .map(|n| text(n, source).trim_start_matches("r#").to_string())
                            .unwrap_or_default(),
                    );
                    enclosing_modules.push(owner);
                    parent = owner.parent();
                }
                (other, None) => {
                    outside = Some(other.to_string());
                    break;
                }
            }
        }
        if let Some(kind) = outside {
            out.push(Err(format!(
                "a module declaration `mod {name};` inside a {kind}, not a module body"
            )));
            continue;
        }
        inline_path.reverse();
        out.push(Ok(Declaration {
            node,
            name,
            inline_path,
            enclosing_modules,
        }));
    }
    out
}

/// The string value of a `#[path = ".."]` attribute item.
fn path_attribute_value(attribute_item: Node<'_>, source: &[u8]) -> Option<String> {
    if attribute_path(attribute_item, source)?.as_str() != "path" {
        return None;
    }
    let value = attribute_node(attribute_item)?.child_by_field_name("value")?;
    matches!(value.kind(), "string_literal" | "raw_string_literal")
        .then(|| string_content(value, source))
}

/// Whether a `cfg_attr` attribute item carries `path` or `cfg` in its
/// attribute list.
fn cfg_attr_carries_path_or_cfg(attribute_item: Node<'_>, source: &[u8]) -> bool {
    if attribute_path(attribute_item, source).as_deref() != Some("cfg_attr") {
        return false;
    }
    let Some(arguments) =
        attribute_node(attribute_item).and_then(|a| a.child_by_field_name("arguments"))
    else {
        return false;
    };
    let tokens = group_tokens(arguments, source);
    split_top_level_commas(&tokens)
        .iter()
        .skip(1)
        .any(|attribute| matches!(token_attribute_parts(attribute).0.as_str(), "path" | "cfg"))
}

/// Every `include!` invocation of a parse.
fn include_invocations(tree: &Tree, source: &[u8]) -> usize {
    let mut count = 0;
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.kind() == "macro_invocation"
            && node.child_by_field_name("macro").is_some_and(|name| {
                let path = compact(name, source);
                path == "include" || path.ends_with("::include")
            })
        {
            count += 1;
        }
        stack.extend(children(node));
    }
    count
}

/// Resolves one declaration of `file` to the index of the file it names.
fn resolve_declaration(
    declaration: &Declaration<'_>,
    file: &Path,
    is_root: bool,
    source: &[u8],
    index: &BTreeMap<PathBuf, usize>,
) -> Result<usize, String> {
    let shown = format!("`mod {};`", declaration.name);
    let attributes = applying_attributes(declaration.node);
    if attributes
        .iter()
        .any(|attribute| cfg_attr_carries_path_or_cfg(*attribute, source))
    {
        return Err(format!(
            "a cfg_attr carrying path or cfg on the declaration {shown}"
        ));
    }
    for module in &declaration.enclosing_modules {
        if applying_attributes(*module)
            .iter()
            .any(|attribute| path_attribute_value(*attribute, source).is_some())
        {
            return Err(format!(
                "a #[path] on an inline module enclosing the declaration {shown}"
            ));
        }
    }
    let directory = file.parent().unwrap_or_else(|| Path::new(""));
    let explicit = attributes
        .iter()
        .find_map(|attribute| path_attribute_value(*attribute, source));
    if let Some(explicit) = explicit {
        if !declaration.inline_path.is_empty() {
            return Err(format!(
                "a #[path] inside an inline module on the declaration {shown}"
            ));
        }
        let target = normalize(&directory.join(explicit));
        return index.get(&target).copied().ok_or_else(|| {
            format!(
                "the #[path] of the declaration {shown} names {}, which is not among the files",
                target.display()
            )
        });
    }
    let is_mod_rs = file.file_name().is_some_and(|name| name == "mod.rs");
    let mut base = if is_root || is_mod_rs {
        directory.to_path_buf()
    } else {
        let stem = file.file_stem().map(PathBuf::from).unwrap_or_default();
        directory.join(stem)
    };
    for segment in &declaration.inline_path {
        base.push(segment);
    }
    let flat = normalize(&base.join(format!("{}.rs", declaration.name)));
    let nested = normalize(&base.join(&declaration.name).join("mod.rs"));
    match (index.get(&flat), index.get(&nested)) {
        (Some(found), None) | (None, Some(found)) => Ok(*found),
        (Some(_), Some(_)) => Err(format!(
            "the declaration {shown} has two candidate files, {} and {}",
            flat.display(),
            nested.display()
        )),
        (None, None) => Err(format!(
            "the declaration {shown} has no candidate file among the files ({} or {})",
            flat.display(),
            nested.display()
        )),
    }
}

/// Classifies every file of a crate (design W4-D18 part 4), walking from
/// `roots` through every out-of-line module declaration with rustc's file
/// rules. The base directory of a declaring file is its own directory when
/// it is a root or is named `mod.rs`, and the directory named after its stem
/// otherwise; enclosing inline module names are appended; `name.rs` and
/// `name/mod.rs` are the candidates and exactly one must be among `files`;
/// `#[path = "p"]` outside every inline module resolves against the
/// declaring file's directory. Candidates are looked up among `files`, never
/// on disk.
#[must_use]
pub fn crate_liveness(
    roots: &[PathBuf],
    files: &[RustSource<'_>],
    cfg: &BuildCfg,
) -> CrateLiveness {
    let paths: Vec<PathBuf> = files.iter().map(|file| normalize(file.path)).collect();
    let index: BTreeMap<PathBuf, usize> = paths
        .iter()
        .enumerate()
        .map(|(position, path)| (path.clone(), position))
        .collect();
    let root_paths: BTreeSet<PathBuf> = roots.iter().map(|root| normalize(root)).collect();
    let mut unmodelled = Vec::new();
    let mut root_indices = Vec::new();
    for root in &root_paths {
        match index.get(root) {
            Some(found) => root_indices.push(*found),
            None => unmodelled.push(format!(
                "{}: a crate root that is not among the files",
                root.display()
            )),
        }
    }

    // Per file: (target file, declaration live in its own file), and the
    // sites of that file the walk does not model.
    let mut edges: Vec<Vec<(usize, bool)>> = vec![Vec::new(); files.len()];
    let mut sites: Vec<Vec<String>> = vec![Vec::new(); files.len()];
    for (position, file) in files.iter().enumerate() {
        let path = &paths[position];
        for _ in 0..include_invocations(file.tree, file.source) {
            sites[position].push(format!("{}: an include! invocation", path.display()));
        }
        let is_root = root_paths.contains(path);
        for declaration in declarations(file.tree, file.source) {
            let resolved = declaration.and_then(|declaration| {
                resolve_declaration(&declaration, path, is_root, file.source, &index)
                    .map(|target| (target, node_is_live(declaration.node, file.source, cfg)))
            });
            match resolved {
                Ok(edge) => edges[position].push(edge),
                Err(reason) => sites[position].push(format!("{}: {reason}", path.display())),
            }
        }
    }

    let root_is_live = |position: usize| {
        node_is_live(
            files[position].tree.root_node(),
            files[position].source,
            cfg,
        )
    };
    let mut live = vec![false; files.len()];
    let mut queue: VecDeque<usize> = VecDeque::new();
    for &root in &root_indices {
        if !live[root] && root_is_live(root) {
            live[root] = true;
            queue.push_back(root);
        }
    }
    while let Some(position) = queue.pop_front() {
        for &(target, declaration_live) in &edges[position] {
            if declaration_live && !live[target] && root_is_live(target) {
                live[target] = true;
                queue.push_back(target);
            }
        }
    }

    let mut reached = vec![false; files.len()];
    let mut queue: VecDeque<usize> = root_indices.iter().copied().collect();
    for &root in &root_indices {
        reached[root] = true;
    }
    while let Some(position) = queue.pop_front() {
        for &(target, _) in &edges[position] {
            if !reached[target] {
                reached[target] = true;
                queue.push_back(target);
            }
        }
    }

    // A file no walk reaches is compiled by no build, so what it holds is not
    // a site of any build: only reached files report unmodelled sites.
    let mut reached_sites: Vec<String> = sites
        .into_iter()
        .enumerate()
        .filter(|(position, _)| reached[*position])
        .flat_map(|(_, entries)| entries)
        .collect();
    reached_sites.sort();
    unmodelled.extend(reached_sites);

    let files = paths
        .into_iter()
        .enumerate()
        .map(|(position, path)| {
            let class = if live[position] {
                FileLiveness::Live
            } else if reached[position] {
                FileLiveness::TestOnly
            } else {
                FileLiveness::Unreachable
            };
            (path, class)
        })
        .collect();
    CrateLiveness { files, unmodelled }
}

#[cfg(test)]
mod tests {
    use super::{
        BuildCfg, CfgPredicate, FileLiveness, LEADING_ATTRIBUTE_NODES, RustSource,
        SIBLING_ATTRIBUTE_CONTAINERS, Tri, attribute_bearing_macro_rules,
        attribute_containers_in_grammar, attribute_existence, crate_liveness,
        declared_attribute_containers, evaluate_cfg, macro_token_is_live, node_is_live,
        token_tree_has_attribute_tokens,
    };
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::{Path, PathBuf};
    use tree_sitter::{Node, Tree};

    fn parse(source: &str) -> Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .expect("the Rust grammar loads");
        let tree = parser.parse(source, None).expect("a parse");
        assert!(
            !tree.root_node().has_error(),
            "the fragment must parse cleanly:\n{source}"
        );
        tree
    }

    /// The first named leaf node whose text is `marker`, in document order.
    fn find<'t>(tree: &'t Tree, source: &str, marker: &str) -> Node<'t> {
        let mut stack = vec![tree.root_node()];
        let mut found = Vec::new();
        while let Some(node) = stack.pop() {
            if node.is_named()
                && node.named_child_count() == 0
                && node.utf8_text(source.as_bytes()).ok() == Some(marker)
            {
                found.push(node);
            }
            let mut cursor = node.walk();
            stack.extend(node.children(&mut cursor));
        }
        found.sort_by_key(Node::start_byte);
        *found
            .first()
            .unwrap_or_else(|| panic!("no node `{marker}` in:\n{source}"))
    }

    /// The first node of `kind`, in document order.
    fn first_of_kind<'t>(tree: &'t Tree, kind: &str) -> Option<Node<'t>> {
        let mut stack = vec![tree.root_node()];
        let mut found = Vec::new();
        while let Some(node) = stack.pop() {
            if node.kind() == kind {
                found.push(node);
            }
            let mut cursor = node.walk();
            stack.extend(node.children(&mut cursor));
        }
        found.sort_by_key(Node::start_byte);
        found.first().copied()
    }

    /// A declared table: `default` enables `a`, which enables `b`, and
    /// through `x/y` the feature `x`; `c` is declared and not enabled; `z` is
    /// named only as `z?/w` and `dep:dz`, so it is declared and not enabled.
    fn table_cfg() -> BuildCfg {
        let features: BTreeMap<String, Vec<String>> = [
            ("default", vec!["a"]),
            ("a", vec!["b", "dep:da", "x/y", "z?/w"]),
            ("b", vec![]),
            ("c", vec![]),
            ("x", vec![]),
            ("z", vec!["dep:dz"]),
        ]
        .into_iter()
        .map(|(name, enables)| {
            (
                name.to_string(),
                enables.into_iter().map(str::to_string).collect(),
            )
        })
        .collect();
        BuildCfg::new(vec![PathBuf::from("lib.rs")], features)
    }

    fn report(label: &str, cases: usize, failures: &[String]) {
        for failure in failures {
            println!("FAILED {failure}");
        }
        println!("{label}: cases {cases}, failures {}", failures.len());
        assert!(
            failures.is_empty(),
            "{label}: {} of {cases} case(s) failed:\n{}",
            failures.len(),
            failures.join("\n")
        );
    }

    #[test]
    fn node_cfg_predicates_evaluate_three_valued() {
        let cfg = table_cfg();
        let cases: [(&str, Tri); 26] = [
            ("test", Tri::False),
            ("doc", Tri::False),
            ("doctest", Tri::False),
            ("miri", Tri::False),
            ("unix", Tri::Unknown),
            ("debug_assertions", Tri::Unknown),
            ("target_os = \"linux\"", Tri::Unknown),
            ("feature = \"default\"", Tri::True),
            ("feature = \"a\"", Tri::True),
            ("feature = \"b\"", Tri::True),
            ("feature = \"x\"", Tri::True),
            ("feature = \"c\"", Tri::Unknown),
            ("feature = \"z\"", Tri::Unknown),
            ("feature = \"undeclared\"", Tri::False),
            ("all()", Tri::True),
            ("any()", Tri::False),
            ("all(test, unix)", Tri::False),
            ("all(unix, feature = \"a\")", Tri::Unknown),
            ("all(feature = \"a\", feature = \"b\")", Tri::True),
            ("any(test, unix)", Tri::Unknown),
            ("any(test, feature = \"undeclared\")", Tri::False),
            ("any(unix, feature = \"a\")", Tri::True),
            ("not(test)", Tri::True),
            ("not(unix)", Tri::Unknown),
            ("not(not(test))", Tri::False),
            (
                "all(not(test), any(unix, not(feature = \"undeclared\")))",
                Tri::True,
            ),
        ];
        let mut failures = Vec::new();
        for (text, expected) in cases {
            let actual = CfgPredicate::parse(text).map(|p| evaluate_cfg(&p, &cfg));
            println!("cfg({text}) => {actual:?} (expected {expected:?})");
            if actual != Some(expected) {
                failures.push(format!("cfg({text}): {actual:?}, expected {expected:?}"));
            }
        }
        report("cfg predicates", cases.len(), &failures);
    }

    #[test]
    fn node_the_default_closure_is_the_features_default_enables() {
        let cfg = table_cfg();
        let expected: BTreeSet<String> = ["default", "a", "b", "x"]
            .into_iter()
            .map(str::to_string)
            .collect();
        println!("default enables: {:?}", cfg.default_enabled_features());
        assert_eq!(cfg.default_enabled_features(), &expected);
        let empty = BuildCfg::new(Vec::new(), BTreeMap::new());
        assert!(empty.default_enabled_features().is_empty());
        assert_eq!(empty.feature("anything"), Tri::False);
    }

    #[test]
    fn node_attribute_existence_follows_cfg_cfg_attr_and_harness_paths() {
        let cfg = table_cfg();
        let cases: [(&str, Tri); 12] = [
            ("#[cfg(test)]", Tri::False),
            ("#[cfg(not(test))]", Tri::True),
            ("#[cfg(unix)]", Tri::Unknown),
            ("#[cfg_attr(not(test), cfg(test))]", Tri::False),
            ("#[cfg_attr(unix, cfg(test))]", Tri::Unknown),
            ("#[cfg_attr(test, cfg(test))]", Tri::True),
            ("#[cfg_attr(test, allow(dead_code))]", Tri::True),
            ("#[test]", Tri::False),
            ("#[tokio::test]", Tri::False),
            ("#[tokio::test(flavor = \"multi_thread\")]", Tri::False),
            ("#[bench]", Tri::False),
            ("#[allow(dead_code)]", Tri::True),
        ];
        let mut failures = Vec::new();
        for (attribute, expected) in cases {
            let source = format!("{attribute}\nfn probe() {{}}\n");
            let tree = parse(&source);
            let item = first_of_kind(&tree, "attribute_item").expect("an attribute item");
            let actual = attribute_existence(item, source.as_bytes(), &cfg);
            println!("{attribute} => {actual:?} (expected {expected:?})");
            if actual != expected {
                failures.push(format!("{attribute}: {actual:?}, expected {expected:?}"));
            }
        }
        report("attribute existence", cases.len(), &failures);
    }

    /// One fragment per container kind: `#[ATTR]` applies to the element
    /// holding `dead_marker`, and `live_marker` is in the next element.
    const ATTACHMENT_CASES: [(&str, &str); 14] = [
        (
            "arguments",
            "fn f() { g(#[ATTR] dead_marker, live_marker); }",
        ),
        (
            "array_expression",
            "fn f() { let _ = [#[ATTR] dead_marker, live_marker]; }",
        ),
        (
            "block",
            "fn f() { #[ATTR] let _ = dead_marker; let _ = live_marker; }",
        ),
        (
            "declaration_list",
            "mod m { #[ATTR] fn dead_marker() {} fn live_marker() {} }",
        ),
        (
            "enum_variant_list",
            "enum E { #[ATTR] dead_marker, live_marker }",
        ),
        (
            "field_declaration_list",
            "struct S { #[ATTR] dead_marker: u8, live_marker: u8 }",
        ),
        (
            "field_initializer",
            "fn f() { S { #[ATTR] a: dead_marker, b: live_marker }; }",
        ),
        (
            "match_arm",
            "fn f() { match x { #[ATTR] 1 => dead_marker, _ => live_marker } }",
        ),
        (
            "ordered_field_declaration_list",
            "struct T(#[ATTR] pub dead_marker, live_marker);",
        ),
        (
            "parameters",
            "fn f(#[ATTR] dead_marker: u8, live_marker: u8) {}",
        ),
        (
            "shorthand_field_initializer",
            "fn f() { S { #[ATTR] dead_marker, live_marker }; }",
        ),
        (
            "source_file",
            "#[ATTR] fn dead_marker() {} fn live_marker() {}",
        ),
        (
            "tuple_expression",
            "fn f() { let _ = (#[ATTR] dead_marker, live_marker); }",
        ),
        (
            "type_parameters",
            "fn f<#[ATTR] dead_marker, live_marker>() {}",
        ),
    ];

    #[test]
    fn node_attachment_covers_every_container_the_grammar_allows() {
        let cfg = table_cfg();
        let grammar = attribute_containers_in_grammar().expect("NODE_TYPES parses");
        let covered: BTreeSet<String> = ATTACHMENT_CASES
            .iter()
            .map(|(kind, _)| (*kind).to_string())
            .collect();
        println!("container kinds covered: {}", covered.len());
        assert_eq!(
            covered, grammar,
            "one attachment case per grammar-derived container kind"
        );
        let mut failures = Vec::new();
        let mut checks = 0;
        for (kind, template) in ATTACHMENT_CASES {
            for (attribute, dead_expected) in [("cfg(test)", false), ("cfg(not(test))", true)] {
                let source = template.replace("ATTR", attribute);
                let tree = parse(&source);
                let item = first_of_kind(&tree, "attribute_item").expect("an attribute item");
                let holder = item.parent().map(|p| p.kind()).unwrap_or_default();
                checks += 1;
                if holder != kind {
                    failures.push(format!(
                        "{kind}: the attribute sits in a {holder}, not a {kind}: {source}"
                    ));
                }
                let dead =
                    node_is_live(find(&tree, &source, "dead_marker"), source.as_bytes(), &cfg);
                let live =
                    node_is_live(find(&tree, &source, "live_marker"), source.as_bytes(), &cfg);
                println!(
                    "{kind} #[{attribute}]: element live {dead} (expected {dead_expected}), next live {live} (expected true)"
                );
                checks += 2;
                if dead != dead_expected {
                    failures.push(format!(
                        "{kind} #[{attribute}]: element live {dead}, expected {dead_expected}: {source}"
                    ));
                }
                if !live {
                    failures.push(format!(
                        "{kind} #[{attribute}]: the next element is not live: {source}"
                    ));
                }
            }
        }
        report("attachment", checks, &failures);
    }

    #[test]
    fn node_inner_attributes_apply_to_their_parent() {
        let cfg = table_cfg();
        let cases: [(&str, &str); 4] = [
            ("source_file", "#![cfg(test)]\nfn dead_marker() {}\n"),
            (
                "declaration_list",
                "mod m { #![cfg(test)] fn dead_marker() {} }\nfn live_marker() {}\n",
            ),
            (
                "block",
                "fn f() { #![cfg(test)] let _ = dead_marker; }\nfn g() { let _ = live_marker; }\n",
            ),
            (
                "match_arm",
                "fn f() { match x { #![cfg(test)] 1 => dead_marker, _ => live_marker } }\n",
            ),
        ];
        let mut failures = Vec::new();
        let mut checks = 0;
        for (kind, source) in cases {
            let tree = parse(source);
            let item = first_of_kind(&tree, "inner_attribute_item").expect("an inner attribute");
            let holder = item.parent().map(|p| p.kind()).unwrap_or_default();
            checks += 2;
            if holder != kind {
                failures.push(format!("{kind}: the inner attribute sits in a {holder}"));
            }
            let dead = node_is_live(find(&tree, source, "dead_marker"), source.as_bytes(), &cfg);
            println!("{kind}: element live {dead} (expected false)");
            if dead {
                failures.push(format!("{kind}: the element under #![cfg(test)] is live"));
            }
            if source.contains("live_marker") {
                checks += 1;
                let live =
                    node_is_live(find(&tree, source, "live_marker"), source.as_bytes(), &cfg);
                println!("{kind}: outside element live {live} (expected true)");
                if !live {
                    failures.push(format!("{kind}: the element outside is not live"));
                }
            }
        }
        report("inner attributes", checks, &failures);
    }

    #[test]
    fn node_grammar_containers_equal_the_declared_sets() {
        let grammar = attribute_containers_in_grammar().expect("NODE_TYPES parses");
        let declared = declared_attribute_containers();
        let leading: BTreeSet<&str> = LEADING_ATTRIBUTE_NODES.into_iter().collect();
        let sibling: BTreeSet<&str> = SIBLING_ATTRIBUTE_CONTAINERS.into_iter().collect();
        println!(
            "grammar-derived attribute containers: {} {grammar:?}",
            grammar.len()
        );
        println!(
            "declared: leading {}, sibling {}, union {}",
            leading.len(),
            sibling.len(),
            declared.len()
        );
        assert!(
            leading.is_disjoint(&sibling),
            "a kind is either leading or sibling"
        );
        assert_eq!(leading.len() + sibling.len(), declared.len());
        assert_eq!(
            grammar, declared,
            "the declared sets must equal the grammar"
        );
        assert_eq!(
            grammar.len(),
            14,
            "tree-sitter-rust 0.24.2 has 14 containers"
        );
    }

    #[test]
    fn node_attribute_arguments_follow_their_target_and_cfg_attr_predicate() {
        let cfg = table_cfg();
        let cases: [(&str, &str, bool); 4] = [
            (
                "an attribute on a live item",
                "#[doc = probe_marker.x]\nfn f() {}\n",
                true,
            ),
            (
                "an attribute on a removed item",
                "#[cfg(test)]\n#[doc = probe_marker.x]\nfn f() {}\n",
                false,
            ),
            (
                "cfg_attr(test, ..) attribute list",
                "#[cfg_attr(test, doc = probe_marker.x)]\nfn f() {}\n",
                false,
            ),
            (
                "cfg_attr(not(test), ..) attribute list",
                "#[cfg_attr(not(test), doc(probe_marker))]\nfn f() {}\n",
                true,
            ),
        ];
        let mut failures = Vec::new();
        for (label, source, expected) in cases {
            let tree = parse(source);
            let actual = node_is_live(find(&tree, source, "probe_marker"), source.as_bytes(), &cfg);
            println!("{label}: live {actual} (expected {expected})");
            if actual != expected {
                failures.push(format!("{label}: live {actual}, expected {expected}"));
            }
        }
        report("attribute arguments", cases.len(), &failures);
    }

    #[test]
    fn node_macro_token_trees_and_transcribers() {
        let cfg = table_cfg();
        let source = "\
macro_rules! only_in_tests {\n    ($($body:tt)*) => { #[cfg(test)] fn only() { $($body)* } };\n}\n\
macro_rules! passes_attributes {\n    ($(#[$attr:meta])* fn $name:ident() $body:block) => { $(#[$attr])* fn $name() $body };\n}\n\
macro_rules! plain {\n    ($e:expr) => { println!(\"{}\", $e) };\n}\n\
macro_rules! template {\n    () => { let _ = cli.in_template; };\n}\n\
fn f(cli: &Cli) {\n    println!(\"{}\", cli.in_println);\n    large_stack_test! { #[test] fn t() { let _ = cli.in_harness; } }\n    only_in_tests! { let _ = cli.in_only_in_tests; }\n    plain!(cli.in_plain);\n    inner! { #![cfg(test)] cli.in_inner_attr }\n}\n\
#[cfg(test)]\nfn g(cli: &Cli) {\n    println!(\"{}\", cli.in_removed_fn);\n}\n";
        let tree = parse(source);
        let bytes = source.as_bytes();
        let bearing = attribute_bearing_macro_rules(&tree, bytes);
        let expected_bearing: BTreeSet<String> = ["only_in_tests", "passes_attributes"]
            .into_iter()
            .map(str::to_string)
            .collect();
        println!("attribute-bearing macro_rules: {bearing:?}");
        let mut failures = Vec::new();
        if bearing != expected_bearing {
            failures.push(format!(
                "attribute-bearing macro_rules {bearing:?}, expected {expected_bearing:?}"
            ));
        }
        let cases: [(&str, bool); 8] = [
            ("in_println", true),
            ("in_plain", true),
            ("in_harness", false),
            ("in_only_in_tests", false),
            ("in_template", false),
            ("in_inner_attr", false),
            ("in_removed_fn", false),
            ("in_println_absent_from_bearing", true),
        ];
        let mut checks = 1;
        for (marker, expected) in cases {
            let (marker, set) = if marker == "in_println_absent_from_bearing" {
                // the same read judged with no attribute-bearing names at all
                ("in_println", BTreeSet::new())
            } else {
                (marker, bearing.clone())
            };
            checks += 1;
            let actual = macro_token_is_live(find(&tree, source, marker), bytes, &cfg, &set);
            println!("{marker}: live {actual} (expected {expected})");
            if actual != expected {
                failures.push(format!("{marker}: live {actual}, expected {expected}"));
            }
        }
        // The token-tree predicate on its own.
        let harness = first_of_kind(&tree, "macro_invocation")
            .and_then(|invocation| invocation.child(2))
            .expect("the println invocation's token tree");
        checks += 1;
        if token_tree_has_attribute_tokens(harness, bytes) {
            failures.push("the println! token tree carries no attribute tokens".to_string());
        }
        report("macro token trees", checks, &failures);
    }

    #[test]
    fn crate_roots_and_features_come_from_the_metadata_document() {
        let document = serde_json::json!({
            "packages": [
                {
                    "name": "other",
                    "manifest_path": "/w4/other/Cargo.toml",
                    "targets": [{"kind": ["lib"], "src_path": "/w4/other/src/lib.rs"}],
                    "features": {}
                },
                {
                    "name": "probe",
                    "manifest_path": "/w4/probe/Cargo.toml",
                    "targets": [
                        {"kind": ["lib"], "src_path": "/w4/probe/src/lib.rs"},
                        {"kind": ["bin"], "src_path": "/w4/probe/src/main.rs"},
                        {"kind": ["test"], "src_path": "/w4/probe/tests/t.rs"},
                        {"kind": ["custom-build"], "src_path": "/w4/probe/build.rs"}
                    ],
                    "features": {"default": ["on"], "on": [], "off": []}
                }
            ]
        });
        let cfg = BuildCfg::from_metadata_document(&document, Path::new("/w4/probe/Cargo.toml"))
            .expect("the probe package");
        println!("roots {:?}, features {:?}", cfg.roots(), cfg.features());
        assert_eq!(
            cfg.roots(),
            &[
                PathBuf::from("/w4/probe/src/lib.rs"),
                PathBuf::from("/w4/probe/src/main.rs")
            ]
        );
        assert_eq!(cfg.feature("on"), Tri::True);
        assert_eq!(cfg.feature("off"), Tri::Unknown);
        assert_eq!(cfg.feature("none"), Tri::False);
        let missing =
            BuildCfg::from_metadata_document(&document, Path::new("/w4/absent/Cargo.toml"));
        println!("absent manifest: {missing:?}");
        assert!(missing.is_err_and(|reason| reason.contains("/w4/absent/Cargo.toml")));
    }

    struct Planted {
        _dir: tempfile::TempDir,
        root: PathBuf,
        files: Vec<(PathBuf, String, Tree)>,
    }

    impl Planted {
        fn new(files: &[(&str, &str)]) -> Self {
            let dir = tempfile::tempdir().expect("a temporary directory");
            let root = dir.path().to_path_buf();
            let mut parsed = Vec::new();
            for (relative, text) in files {
                let path = root.join(relative);
                std::fs::create_dir_all(path.parent().expect("a parent")).expect("mkdir");
                std::fs::write(&path, text).expect("write");
                let read = std::fs::read_to_string(&path).expect("read back");
                let tree = parse(&read);
                parsed.push((path, read, tree));
            }
            Self {
                _dir: dir,
                root,
                files: parsed,
            }
        }

        fn sources(&self) -> Vec<RustSource<'_>> {
            self.files
                .iter()
                .map(|(path, text, tree)| RustSource {
                    path,
                    source: text.as_bytes(),
                    tree,
                })
                .collect()
        }
    }

    const CRATE_FILES: [(&str, &str); 19] = [
        (
            "lib.rs",
            "mod live;\n#[cfg(test)]\nmod t;\n#[cfg(test)]\n#[path = \"x/y.rs\"]\nmod z;\nmod gated;\nmod nested;\nmod outer {\n    mod leaf2;\n}\nmod holder {\n    #[path = \"p.rs\"]\n    mod q;\n}\n#[cfg(test)]\nmod tests {\n    mod helper;\n}\nmod missing;\nmod dup;\nmod included;\n",
        ),
        ("main.rs", "mod live;\n"),
        ("live.rs", "mod inner;\n"),
        ("live/inner.rs", "pub fn inner() {}\n"),
        ("t.rs", "mod deeper;\n"),
        ("t/deeper.rs", "pub fn deeper() {}\n"),
        ("x/y.rs", "pub fn y() {}\n"),
        ("gated.rs", "#![cfg(test)]\npub fn gated() {}\n"),
        ("nested/mod.rs", "mod leaf;\n"),
        ("nested/leaf.rs", "pub fn leaf() {}\n"),
        ("outer/leaf2.rs", "pub fn leaf2() {}\n"),
        ("holder/p.rs", "pub fn p() {}\n"),
        ("tests/helper.rs", "pub fn helper() {}\n"),
        ("orphan.rs", "pub fn orphan() {}\n"),
        ("dup.rs", "pub fn dup() {}\n"),
        ("dup/mod.rs", "pub fn dup() {}\n"),
        ("q.rs", "pub fn q() {}\n"),
        ("included.rs", "include!(\"elsewhere.rs\");\n"),
        (
            "unreached_include.rs",
            "include!(\"elsewhere.rs\");\nmod not_followed;\n",
        ),
    ];

    fn crate_expected(with_main: bool) -> BTreeMap<&'static str, FileLiveness> {
        use FileLiveness::{Live, TestOnly, Unreachable};
        [
            ("lib.rs", Live),
            ("main.rs", if with_main { Live } else { Unreachable }),
            ("live.rs", Live),
            ("live/inner.rs", Live),
            ("t.rs", TestOnly),
            ("t/deeper.rs", TestOnly),
            ("x/y.rs", TestOnly),
            ("gated.rs", TestOnly),
            ("nested/mod.rs", Live),
            ("nested/leaf.rs", Live),
            ("outer/leaf2.rs", Live),
            ("holder/p.rs", Unreachable),
            ("tests/helper.rs", TestOnly),
            ("orphan.rs", Unreachable),
            ("dup.rs", Unreachable),
            ("dup/mod.rs", Unreachable),
            ("q.rs", Unreachable),
            ("included.rs", Live),
            ("unreached_include.rs", Unreachable),
        ]
        .into_iter()
        .collect()
    }

    /// Runs the planted crate with `roots` and returns (checks, failures).
    fn check_crate(label: &str, roots: &[&str]) -> (usize, Vec<String>) {
        let planted = Planted::new(&CRATE_FILES);
        let roots: Vec<PathBuf> = roots.iter().map(|r| planted.root.join(r)).collect();
        let cfg = BuildCfg::new(roots.clone(), BTreeMap::new());
        let sources = planted.sources();
        let liveness = crate_liveness(&roots, &sources, &cfg);
        let expected = crate_expected(roots.len() == 2);
        let mut failures = Vec::new();
        let mut checks = 0;
        for (relative, class) in &expected {
            checks += 1;
            let actual = liveness.get(&planted.root.join(relative));
            println!("{label}: {relative} => {actual:?} (expected {class:?})");
            if actual != Some(*class) {
                failures.push(format!(
                    "{label}: {relative} {actual:?}, expected {class:?}"
                ));
            }
        }
        let counted = liveness.count(FileLiveness::Live)
            + liveness.count(FileLiveness::TestOnly)
            + liveness.count(FileLiveness::Unreachable);
        println!(
            "{label}: live {}, test-only {}, unreachable {}, files {}",
            liveness.count(FileLiveness::Live),
            liveness.count(FileLiveness::TestOnly),
            liveness.count(FileLiveness::Unreachable),
            CRATE_FILES.len()
        );
        checks += 1;
        if counted != CRATE_FILES.len() || liveness.files.len() != expected.len() {
            failures.push(format!(
                "{label}: {counted} classified over {} files, {} expected",
                liveness.files.len(),
                expected.len()
            ));
        }
        for entry in &liveness.unmodelled {
            println!("{label}: unmodelled {entry}");
        }
        let needles = [
            "`mod missing;` has no candidate file",
            "`mod dup;` has two candidate files",
            "a #[path] inside an inline module on the declaration `mod q;`",
            "included.rs: an include! invocation",
        ];
        for needle in needles {
            checks += 1;
            let hits = liveness
                .unmodelled
                .iter()
                .filter(|entry| entry.contains(needle))
                .count();
            if hits != 1 {
                failures.push(format!("{label}: unmodelled `{needle}` found {hits} times"));
            }
        }
        checks += 1;
        if liveness.unmodelled.len() != needles.len() {
            failures.push(format!(
                "{label}: {} unmodelled entries, expected {}",
                liveness.unmodelled.len(),
                needles.len()
            ));
        }
        (checks, failures)
    }

    #[test]
    fn crate_module_tree_classifies_every_file() {
        let (checks, failures) = check_crate("lib root", &["lib.rs"]);
        assert_eq!(
            checks,
            CRATE_FILES.len() + 6,
            "every planted file and six instrument checks"
        );
        report("module tree", checks, &failures);
    }

    #[test]
    fn crate_a_second_root_that_declares_the_same_module_changes_nothing_else() {
        let (checks, failures) = check_crate("lib and main roots", &["lib.rs", "main.rs"]);
        assert_eq!(
            checks,
            CRATE_FILES.len() + 6,
            "every planted file and six instrument checks"
        );
        report("two roots", checks, &failures);
    }

    /// The test-build mode (`BuildCfg::for_test_build`), from both sides: in a
    /// test build `cfg(test)` is true and a test harness attribute keeps its
    /// item, every other predicate is unchanged, and the non-test answers stay
    /// what they were.
    #[test]
    fn node_a_test_build_keeps_test_code_and_removes_non_test_code() {
        let build = table_cfg();
        let test_build = build.for_test_build();
        let mut failures = Vec::new();
        let mut cases = 0usize;
        if build.is_test_build() || !test_build.is_test_build() {
            failures.push("is_test_build does not tell the two builds apart".to_string());
        }
        cases += 1;
        let predicates: [(&str, Tri, Tri); 9] = [
            ("test", Tri::False, Tri::True),
            ("not(test)", Tri::True, Tri::False),
            ("doc", Tri::False, Tri::False),
            ("miri", Tri::False, Tri::False),
            ("unix", Tri::Unknown, Tri::Unknown),
            ("feature = \"a\"", Tri::True, Tri::True),
            ("feature = \"undeclared\"", Tri::False, Tri::False),
            ("all(test, unix)", Tri::False, Tri::Unknown),
            ("any(test, feature = \"undeclared\")", Tri::False, Tri::True),
        ];
        for (text, non_test, test) in predicates {
            cases += 1;
            let predicate = CfgPredicate::parse(text).expect("a predicate");
            let actual = (
                evaluate_cfg(&predicate, &build),
                evaluate_cfg(&predicate, &test_build),
            );
            println!(
                "cfg({text}) => {actual:?} (expected {:?})",
                (non_test, test)
            );
            if actual != (non_test, test) {
                failures.push(format!(
                    "cfg({text}): {actual:?}, expected {:?}",
                    (non_test, test)
                ));
            }
        }
        let attributes: [(&str, Tri, Tri); 6] = [
            ("#[test]", Tri::False, Tri::True),
            ("#[tokio::test]", Tri::False, Tri::True),
            ("#[cfg(test)]", Tri::False, Tri::True),
            ("#[cfg(not(test))]", Tri::True, Tri::False),
            ("#[cfg_attr(test, test)]", Tri::True, Tri::True),
            ("#[cfg_attr(not(test), cfg(test))]", Tri::False, Tri::True),
        ];
        for (attribute, non_test, test) in attributes {
            cases += 1;
            let source = format!("{attribute}\nfn probe() {{}}\n");
            let tree = parse(&source);
            let item = first_of_kind(&tree, "attribute_item").expect("an attribute item");
            let actual = (
                attribute_existence(item, source.as_bytes(), &build),
                attribute_existence(item, source.as_bytes(), &test_build),
            );
            println!(
                "{attribute} => {actual:?} (expected {:?})",
                (non_test, test)
            );
            if actual != (non_test, test) {
                failures.push(format!(
                    "{attribute}: {actual:?}, expected {:?}",
                    (non_test, test)
                ));
            }
        }
        // A statement no test build compiles inside a test, and a test that a
        // non-test build compiles as a plain function.
        let source = "#[test]\nfn t() {\n    #[cfg(not(test))]\n    let _ = dead_marker;\n    let _ = test_marker;\n}\n#[cfg_attr(test, test)]\nfn u() {\n    let _ = both_marker;\n}\n";
        let tree = parse(source);
        let nodes: [(&str, bool, bool); 3] = [
            ("dead_marker", false, false),
            ("test_marker", false, true),
            ("both_marker", true, true),
        ];
        for (marker, non_test, test) in nodes {
            cases += 1;
            let node = find(&tree, source, marker);
            let actual = (
                node_is_live(node, source.as_bytes(), &build),
                node_is_live(node, source.as_bytes(), &test_build),
            );
            println!("{marker} => {actual:?} (expected {:?})", (non_test, test));
            if actual != (non_test, test) {
                failures.push(format!(
                    "{marker}: {actual:?}, expected {:?}",
                    (non_test, test)
                ));
            }
        }
        // A file a `#[cfg(test)] mod` declares is live in a test build only,
        // and one a `#[cfg(not(test))] mod` declares in a non-test build only.
        let planted = Planted::new(&[
            (
                "lib.rs",
                "#[cfg(test)]\nmod only_in_tests;\n#[cfg(not(test))]\nmod never_in_tests;\n",
            ),
            ("only_in_tests.rs", "pub fn t() {}\n"),
            ("never_in_tests.rs", "pub fn n() {}\n"),
        ]);
        let roots = vec![planted.root.join("lib.rs")];
        let planted_build = BuildCfg::new(roots.clone(), BTreeMap::new());
        let planted_test_build = planted_build.for_test_build();
        let sources = planted.sources();
        let in_build = crate_liveness(&roots, &sources, &planted_build);
        let in_test_build = crate_liveness(&roots, &sources, &planted_test_build);
        let files: [(&str, FileLiveness, FileLiveness); 3] = [
            ("lib.rs", FileLiveness::Live, FileLiveness::Live),
            (
                "only_in_tests.rs",
                FileLiveness::TestOnly,
                FileLiveness::Live,
            ),
            (
                "never_in_tests.rs",
                FileLiveness::Live,
                FileLiveness::TestOnly,
            ),
        ];
        for (file, non_test, test) in files {
            cases += 1;
            let path = planted.root.join(file);
            let actual = (in_build.get(&path), in_test_build.get(&path));
            println!("{file} => {actual:?} (expected {:?})", (non_test, test));
            if actual != (Some(non_test), Some(test)) {
                failures.push(format!(
                    "{file}: {actual:?}, expected {:?}",
                    (non_test, test)
                ));
            }
        }
        assert_eq!(cases, 22, "every declared case ran");
        report("test build", cases, &failures);
    }

    #[test]
    fn crate_a_root_absent_from_the_files_is_unmodelled() {
        let planted = Planted::new(&[("lib.rs", "pub fn f() {}\n")]);
        let roots = vec![planted.root.join("lib.rs"), planted.root.join("main.rs")];
        let cfg = BuildCfg::new(roots.clone(), BTreeMap::new());
        let liveness = crate_liveness(&roots, &planted.sources(), &cfg);
        println!("unmodelled: {:?}", liveness.unmodelled);
        assert_eq!(liveness.unmodelled.len(), 1);
        assert!(liveness.unmodelled[0].contains("a crate root that is not among the files"));
        assert_eq!(
            liveness.get(&planted.root.join("lib.rs")),
            Some(FileLiveness::Live)
        );
    }
}
