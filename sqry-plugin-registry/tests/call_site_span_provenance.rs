//! Issue #748: span provenance gates the body plane.
//!
//! A node minted from a **call site** holds the caller's extent. That extent is
//! a genuine, non-degenerate range, so the span-validity gate that guards
//! `body_hash` and the shape descriptor admits it, and the stub is fingerprinted
//! as if it owned the caller's body. Every stub minted from one call site then
//! hashes identically and they are reported as duplicate bodies of each other.
//!
//! Three contracts are under test here, all end to end through the real
//! `build_unified_graph` pipeline with the full plugin roster:
//!
//! 1. **Stubs are excluded.** A node minted by `ensure_callee` (or by the
//!    explicit `add_call_site_node` sink the FFI/WASM extractors use) reaches
//!    the committed graph with `body_hash: None` and no shape descriptor.
//!
//! 2. **Declarations are not.** A symbol called ABOVE its own definition is the
//!    common case: the call site mints the node, the declaration reaches it
//!    afterwards, and the node must end up with a real extent and a real body
//!    hash. `no_definition_loses_the_body_plane` states this as a corpus-wide
//!    invariant rather than a count, so it survives fixture churn.
//!
//! 3. **The two halves stay in lock-step.** No node carries a shape descriptor
//!    without a body hash, so `structural_similar` and `find_duplicates` never
//!    disagree about which nodes own a body.
//!
//! On why the mechanism is span provenance and not `is_definition`, see
//! `docs/development/call-site-span-provenance/02_DESIGN-call-site-span-provenance.md`
//! section "Mechanism choice". The short version: `is_definition` answers
//! "is this symbol declared in the workspace", which is a fact about identity,
//! while the body plane needs "is the extent recorded on this node a body",
//! which is a fact about the extent. `body_carrying_non_definitions_are_only_ffi_prototypes`
//! pins the population where the two answers differ.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use sqry_core::graph::unified::build::body_hash::{
    has_valid_body_span, node_kind_supports_body_hash,
};
use sqry_core::graph::unified::build::{BuildConfig, build_unified_graph};
use sqry_core::graph::unified::node::NodeKind;
use sqry_plugin_registry::create_plugin_manager_all;
use tempfile::TempDir;

/// One observed node in a committed graph.
#[derive(Debug, Clone)]
struct ObservedNode {
    name: String,
    kind: NodeKind,
    file: String,
    start_line: u32,
    has_body_hash: bool,
    /// The body hash itself, so a test can group by it the way `sqry-lsp` does.
    body_hash_hex: Option<String>,
    has_shape_descriptor: bool,
    has_valid_span: bool,
    is_definition: bool,
}

/// Kinds observed here: every kind the body plane can admit.
///
/// Deliberately not just Function/Method, which is all `duplicates --type body`
/// walks. `sqry-lsp`'s `collect_duplicate_body_groups` groups ANY node carrying
/// a body hash with no kind filter, and the FFI, WebAssembly, export-list and
/// type-reference extractors mint `Module`, `Class` and `Interface` stubs. A
/// narrower filter here would let those stubs pass unseen, which is exactly how
/// an earlier version of this file managed to assert nothing at all.
fn is_observed_kind(kind: NodeKind) -> bool {
    node_kind_supports_body_hash(kind)
}

/// Build a workspace rooted at `root` and return every live observed node in
/// the committed graph with the two body-plane facts alongside `is_definition`.
fn observe_root(root: &Path) -> Vec<ObservedNode> {
    let plugins = create_plugin_manager_all();
    let graph = build_unified_graph(root, &plugins, &BuildConfig::default())
        .expect("fixture workspace builds");
    let snapshot = graph.snapshot();
    let strings = snapshot.strings();
    let files = snapshot.files();
    let descriptors = snapshot.macro_metadata().shape_descriptors();
    let prefix = root.display().to_string();

    let mut out = Vec::new();
    for (node_id, entry) in snapshot.nodes().iter() {
        if !is_observed_kind(entry.kind) {
            continue;
        }
        if entry.is_unified_loser() {
            continue;
        }
        let name = entry
            .qualified_name
            .and_then(|id| strings.resolve(id))
            .or_else(|| strings.resolve(entry.name))
            .unwrap_or_default()
            .to_string();
        let file = files
            .resolve(entry.file)
            .map(|p| p.display().to_string().replace(&prefix, ""))
            .unwrap_or_default();
        out.push(ObservedNode {
            name,
            kind: entry.kind,
            file,
            start_line: entry.start_line,
            has_body_hash: entry.body_hash.is_some(),
            body_hash_hex: entry.body_hash.map(|h| format!("{h}")),
            has_shape_descriptor: descriptors.contains_key(&node_id),
            has_valid_span: has_valid_body_span(entry),
            is_definition: entry.is_definition,
        });
    }
    out
}

/// Write `fixtures` into a fresh temp workspace and observe it.
fn observe(fixtures: &[(&str, &str)]) -> Vec<ObservedNode> {
    let dir = TempDir::new().expect("tempdir");
    for (name, src) in fixtures {
        let path = dir.path().join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        std::fs::write(&path, src).expect("write fixture");
    }
    observe_root(dir.path())
}

fn find<'a>(nodes: &'a [ObservedNode], name: &str) -> &'a ObservedNode {
    let matches: Vec<&ObservedNode> = nodes.iter().filter(|n| n.name == name).collect();
    assert_eq!(
        matches.len(),
        1,
        "expected exactly one node named {name}, found {matches:#?}"
    );
    matches[0]
}

/// Source-tree `test-fixtures` directory. `build_unified_graph` is the pure
/// in-memory builder and writes nothing, so reading it here is safe.
fn fixtures_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root is the parent of sqry-plugin-registry")
        .join("test-fixtures")
}

/// The issue's headline reproduction, in miniature. `errors.Join(a, b, c)`
/// promotes `a`, `b` and `c` to Function stubs pinned to the call site and
/// mints an `errors::Join` stub at the same site. All four held one extent, so
/// all four hashed identically and were reported as duplicate bodies.
#[test]
fn go_call_site_stubs_carry_no_body_hash_and_no_shape_descriptor() {
    let src = "package errorwrapping\n\
               \n\
               import \"errors\"\n\
               \n\
               func bundle(a, b, c error) error {\n\
               \treturn errors.Join(a, b, c)\n\
               }\n";
    let nodes = observe(&[("go.mod", "module errorwrapping\n"), ("join.go", src)]);

    for stub in [
        "errors::Join",
        "errorwrapping::a",
        "errorwrapping::b",
        "errorwrapping::c",
    ] {
        let node = find(&nodes, stub);
        assert!(
            node.has_valid_span,
            "the stub still carries a usable location for get_references: only the \
             body plane declines it (got {node:?})"
        );
        assert!(
            !node.has_body_hash,
            "{stub} holds the call site's extent, not a body it owns, so it must \
             carry no body hash (got {node:?})"
        );
        assert!(
            !node.has_shape_descriptor,
            "{stub} must carry no shape descriptor either: the two halves of the \
             body plane stay in lock-step (got {node:?})"
        );
    }

    let bundle = find(&nodes, "errorwrapping::bundle");
    assert!(
        bundle.has_body_hash && bundle.has_shape_descriptor,
        "the enclosing declaration owns its body and keeps it (got {bundle:?})"
    );
}

/// The FFI / WebAssembly extractors mint their target through
/// `add_call_site_node` rather than `ensure_callee`, and the node they mint is
/// a `Module`, which is body-hashable. Same contract: the extent is the
/// `require(...)` call, not a declaration of the native module.
#[test]
fn javascript_native_addon_stub_carries_no_body_hash() {
    let src = "const addon = require('./build/Release/addon.node');\n\
               function run(x) {\n\
               \tif (x > 0) {\n\
               \t\treturn addon.compute(x);\n\
               \t}\n\
               \treturn 0;\n\
               }\n\
               module.exports = { run };\n";
    let nodes = observe(&[("index.js", src)]);

    let run = find(&nodes, "run");
    assert!(
        run.has_body_hash,
        "the real function keeps its body (got {run:?})"
    );

    let stubs: Vec<&ObservedNode> = nodes
        .iter()
        .filter(|n| n.name.starts_with("native::"))
        .collect();
    assert!(
        !stubs.is_empty(),
        "the native-addon extractor must have minted a stub, or this test proves \
         nothing; observed {nodes:#?}"
    );
    for stub in stubs {
        assert!(
            stub.has_valid_span,
            "the stub keeps a usable location (got {stub:?})"
        );
        assert!(
            !stub.has_body_hash,
            "a native-addon stub holds the require() call's extent (got {stub:?})"
        );
    }
}

/// One per-language case for [`every_language_keeps_its_stubs_out_of_the_body_plane`].
struct StubCase {
    /// What the case is, for the failure message.
    what: &'static str,
    /// Files to write into the temp workspace.
    files: &'static [(&'static str, &'static str)],
    /// Every observed node whose name starts with this must be stub-shaped.
    /// At least one must exist, or the case proves nothing.
    stub_prefix: &'static str,
    /// Require an EXACT name match rather than a prefix match.
    ///
    /// Needed when the stub's name is also a prefix of a real declaration's:
    /// `OtherCrateType` is a prefix of `OtherCrateType::greet`, the method
    /// declared inside the `impl` block, which genuinely owns its body.
    exact_name: bool,
}

/// The per-language gate.
///
/// `ensure_callee` is not the only path that hands a node a foreign extent.
/// Plugins mint FFI, syscall, WebAssembly, native-module, export-list and
/// type-reference stubs directly, and the first sweep for this fix matched a
/// literal expression shape (`Some(span_from_node(call_node))`), so it missed
/// every site that spelled the node variable differently. This table names one
/// case per surviving family so a future miss fails here rather than shipping.
///
/// Each case asserts BOTH directions: the stub set is non-empty (the extractor
/// really ran) and no member of it carries a body hash or a shape descriptor.
///
/// An earlier version of this test excused Swift and Kotlin from the table on
/// the grounds that neither extractor fires on a minimal fixture. Both do, and
/// the excuse was wrong on the facts: Kotlin's only guard is
/// `if context.is_external`, and Swift's `build_graph` locates and indexes the
/// bridging header itself, from the `.swift` file's own directory, before it
/// walks. Both are in the table now. The lesson is in the review record: a
/// justification for leaving a converted site outside the regression gate has
/// to be checked like any other claim, because a wrong one is how a revert
/// lands green.
///
/// The specialty-plugin cases (Apex, ABAP, Puppet, ServiceNow) are gated on the
/// `specialty-plugins` feature, matching `shape_coverage_workspace`.
#[test]
fn every_language_keeps_its_stubs_out_of_the_body_plane() {
    #[allow(unused_mut)]
    let mut cases: Vec<StubCase> = vec![
        StubCase {
            what: "python __all__ export entry",
            files: &[(
                "pkg.py",
                "__all__ = [\"shared_symbol_name\"]\n\n\ndef other():\n    return 1\n",
            )],
            stub_prefix: "shared_symbol_name",
            exact_name: false,
        },
        StubCase {
            what: "python native extension import",
            files: &[(
                "app.py",
                "import numpy\n\n\ndef use(x):\n    return numpy.array(x)\n",
            )],
            stub_prefix: "native::numpy",
            exact_name: false,
        },
        StubCase {
            what: "sql call inside a routine body",
            files: &[(
                "proc.sql",
                "CREATE FUNCTION outer_fn(score INT) RETURNS INT AS $$\n\
                 BEGIN\n\
                 \x20 RETURN shared_helper(score, 42);\n\
                 END;\n\
                 $$ LANGUAGE plpgsql;\n",
            )],
            stub_prefix: "shared_helper",
            exact_name: false,
        },
        StubCase {
            what: "php FFI cdef",
            files: &[(
                "ffi.php",
                "<?php\nfunction boot() {\n    $ffi = FFI::cdef(\"int puts(const char *s);\", \"libc.so.6\");\n    return $ffi;\n}\n",
            )],
            stub_prefix: "native::",
            exact_name: false,
        },
        StubCase {
            what: "java JNA Native.load",
            files: &[(
                "One.java",
                "package probe;\n\nimport com.sun.jna.Native;\n\npublic class One {\n    public static Object load() {\n        return Native.load(\"c\", CLib.class);\n    }\n}\n",
            )],
            stub_prefix: "native::",
            exact_name: false,
        },
        StubCase {
            what: "ruby FFI attach_function",
            files: &[(
                "ffi.rb",
                "module NativeBind\n  extend FFI::Library\n  FFI.attach_function :puts, [:string], :int\nend\n",
            )],
            stub_prefix: "ffi::",
            exact_name: false,
        },
        StubCase {
            what: "javascript WebAssembly constructor",
            files: &[(
                "wasm.js",
                "function boot(bytes) {\n    const mod = new WebAssembly.Module(bytes);\n    return mod;\n}\n",
            )],
            stub_prefix: "wasm::",
            exact_name: false,
        },
        StubCase {
            what: "typescript WebAssembly constructor",
            files: &[(
                "wasm.ts",
                "function boot(bytes: BufferSource) {\n    const mod = new WebAssembly.Module(bytes);\n    return mod;\n}\n",
            )],
            stub_prefix: "wasm::",
            exact_name: false,
        },
        StubCase {
            what: "go type assertion shadow",
            files: &[
                ("go.mod", "module probe\n"),
                (
                    "assert.go",
                    "package probe\n\nimport \"errors\"\n\ntype myErr struct{}\n\nfunc (myErr) Error() string { return \"x\" }\n\nfunc check(err error) bool {\n\t_, ok := err.(myErr)\n\t_ = errors.New\n\treturn ok\n}\n",
                ),
            ],
            stub_prefix: "<type:",
            exact_name: false,
        },
        StubCase {
            what: "kotlin JNI external fun",
            files: &[(
                "a.kt",
                "package probe\n\nexternal fun processOne(x: Int): Int\n",
            )],
            stub_prefix: "<ffi:",
            exact_name: false,
        },
        StubCase {
            what: "swift bridged C call",
            files: &[
                ("App-Bridging-Header.h", "int probe_c_helper(int x);\n"),
                (
                    "a.swift",
                    "func alpha() -> Int32 {\n    return probe_c_helper(1)\n}\n",
                ),
            ],
            stub_prefix: "C::probe_c_helper",
            exact_name: false,
        },
        StubCase {
            what: "elixir erlang NIF load",
            files: &[(
                "nif.ex",
                "defmodule Probe do\n  def load do\n    :erlang.load_nif('./probe_nif', 0)\n  end\nend\n",
            )],
            stub_prefix: "ffi::erlang::load_nif",
            exact_name: false,
        },
        StubCase {
            what: "typescript re-export of another module",
            files: &[
                ("shared.ts", "export const value = 1;\n"),
                ("barrel.ts", "export * from \"./shared\";\n"),
            ],
            stub_prefix: "./shared",
            exact_name: false,
        },
        StubCase {
            what: "python base class in an inheritance clause",
            files: &[(
                "child.py",
                "class Child(AbstractLongBaseName):\n    def run(self):\n        return 1\n",
            )],
            stub_prefix: "AbstractLongBaseName",
            exact_name: false,
        },
        StubCase {
            what: "php trait use",
            files: &[(
                "user.php",
                "<?php\nclass Consumer {\n    use SomeSharedTraitName;\n    public function go() { return 1; }\n}\n",
            )],
            stub_prefix: "SomeSharedTraitName",
            exact_name: false,
        },
        StubCase {
            what: "c bodyless struct specifier in a parameter list",
            files: &[(
                "handler.c",
                "int handler_one(struct probe_payload *p) {\n    return p ? 1 : 0;\n}\n",
            )],
            stub_prefix: "probe_payload",
            exact_name: false,
        },
        StubCase {
            what: "cpp bodyless struct specifier in a parameter list",
            files: &[(
                "handler.cpp",
                "int handlerOne(struct ProbePayload *p) {\n    return p ? 1 : 0;\n}\n",
            )],
            stub_prefix: "ProbePayload",
            exact_name: false,
        },
        StubCase {
            what: "rust INHERENT impl block naming a type declared elsewhere",
            files: &[(
                "inherent.rs",
                "impl OtherInherentType {\n    pub fn greet(&self) -> u32 {\n        7\n    }\n}\n",
            )],
            stub_prefix: "OtherInherentType",
            // Same reason as the trait-impl case below.
            exact_name: true,
        },
        StubCase {
            what: "rust TRAIT impl block naming a type declared elsewhere",
            files: &[(
                "lib.rs",
                "pub trait Greeter {\n    fn greet(&self) -> u32;\n}\n\nimpl Greeter for OtherCrateType {\n    fn greet(&self) -> u32 {\n        7\n    }\n}\n",
            )],
            stub_prefix: "OtherCrateType",
            // `OtherCrateType::greet` is the method declared inside the impl
            // block; it owns its body and must not be swept in.
            exact_name: true,
        },
    ];

    #[cfg(feature = "specialty-plugins")]
    cases.extend([
        StubCase {
            what: "servicenow new GlideRecord",
            files: &[(
                "script.snjs",
                "function run() {\n    var gr = new GlideRecord('incident');\n    gr.query();\n    return gr;\n}\n",
            )],
            stub_prefix: "GlideRecord:",
            exact_name: false,
        },
        StubCase {
            what: "servicenow new GlideAjax script include",
            files: &[(
                "ajax.snjs",
                "function run() {\n    var ga = new GlideAjax('MyHelper');\n    ga.getXML();\n    return ga;\n}\n",
            )],
            stub_prefix: "ScriptInclude:",
            exact_name: false,
        },
        StubCase {
            what: "apex method invocation",
            files: &[(
                "Caller.cls",
                "public class Caller {\n    public void run() {\n        Helper.sharedRoutine(1);\n    }\n}\n",
            )],
            stub_prefix: "Helper::sharedRoutine",
            exact_name: false,
        },
        StubCase {
            what: "abap SUBMIT statement",
            files: &[(
                "prog.abap",
                "REPORT zcaller.\n\nSTART-OF-SELECTION.\n  SUBMIT zother_program AND RETURN.\n",
            )],
            stub_prefix: "zother_program",
            exact_name: false,
        },
        StubCase {
            what: "puppet include of a class declared elsewhere",
            files: &[(
                "manifests/site.pp",
                "class site {\n  include other::thing\n}\n",
            )],
            stub_prefix: "manifests/other/thing.pp",
            exact_name: false,
        },
        StubCase {
            what: "servicenow gs.* platform API call",
            files: &[(
                "script.js",
                "function run() {\n    gs.addInfoMessage('hello');\n    return 1;\n}\n",
            )],
            stub_prefix: "gs::",
            exact_name: false,
        },
    ]);

    let mut failures: Vec<String> = Vec::new();
    for case in &cases {
        let nodes = observe(case.files);
        let stubs: Vec<&ObservedNode> = nodes
            .iter()
            .filter(|n| {
                if case.exact_name {
                    n.name == case.stub_prefix
                } else {
                    n.name.starts_with(case.stub_prefix)
                }
            })
            .collect();
        if stubs.is_empty() {
            failures.push(format!(
                "{}: no node named {}* was minted, so this case proves nothing. \
                 Observed: {:?}",
                case.what,
                case.stub_prefix,
                nodes.iter().map(|n| &n.name).collect::<Vec<_>>()
            ));
            continue;
        }
        for stub in stubs {
            if stub.has_body_hash || stub.has_shape_descriptor {
                failures.push(format!(
                    "{}: {} still enters the body plane (body_hash={}, descriptor={}) \
                     at {}:{}",
                    case.what,
                    stub.name,
                    stub.has_body_hash,
                    stub.has_shape_descriptor,
                    stub.file,
                    stub.start_line
                ));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "call-site stubs reached the body plane:\n  {}",
        failures.join("\n  ")
    );
}

/// A symbol called ABOVE its own definition is the common case. The call site
/// mints the node first; the declaration must still hand it a real extent and a
/// real body hash. This is the failure mode a naive "first mint wins" guard
/// would introduce.
///
/// Python is used deliberately: its plugin walks calls and declarations in one
/// pass, so the call really does reach `callee` before the `def` does. Several
/// other plugins extract all declarations first, which would make the ordering
/// this test is named for unreachable, and the test vacuous.
#[test]
fn a_function_called_before_it_is_declared_still_gets_a_body_hash() {
    let src = "def caller(x):\n\
               \x20   if x > 0:\n\
               \x20       return callee(x)\n\
               \x20   return 0\n\
               \n\
               \n\
               def callee(x):\n\
               \x20   total = 0\n\
               \x20   for i in range(x):\n\
               \x20       total += i\n\
               \x20   return total\n";
    let nodes = observe(&[("order.py", src)]);

    let callee = find(&nodes, "callee");
    assert!(
        callee.has_body_hash && callee.has_shape_descriptor,
        "a function called above its definition must still own its body (got {callee:?})"
    );
    let caller = find(&nodes, "caller");
    assert!(
        caller.has_body_hash && caller.has_shape_descriptor,
        "the caller owns its body too (got {caller:?})"
    );
}

/// The regression gate, stated as an invariant so fixture churn cannot silently
/// weaken it: across the whole committed fixture corpus, in every language, a
/// node that the build marked as a real declaration and gave a valid body
/// extent keeps its body hash.
///
/// This is the check that would have failed had the body plane been gated on
/// something the fix must not disturb. The floor keeps it from passing
/// vacuously if the corpus ever stops building.
#[test]
fn no_definition_loses_the_body_plane() {
    let root = fixtures_root();
    assert!(root.is_dir(), "missing {}", root.display());
    let nodes = observe_root(&root);

    let definitions: Vec<&ObservedNode> = nodes
        .iter()
        .filter(|n| {
            matches!(n.kind, NodeKind::Function | NodeKind::Method)
                && n.is_definition
                && n.has_valid_span
        })
        .collect();
    let missing: Vec<&&ObservedNode> = definitions.iter().filter(|n| !n.has_body_hash).collect();
    assert!(
        missing.is_empty(),
        "every declaration with a valid body extent must keep its body hash; \
         {} lost it: {missing:#?}",
        missing.len()
    );
    assert!(
        definitions.len() >= 300,
        "non-vacuity floor: expected the corpus to contribute at least 300 \
         body-carrying declarations, saw {}",
        definitions.len()
    );
}

/// `body_hash` and the shape descriptor are two halves of one plane and must
/// agree about which nodes own a body. A descriptor without a body hash would
/// mean `structural_similar` still fingerprints something `find_duplicates`
/// has already disowned.
///
/// The converse is not an invariant: a body hash without a descriptor is the
/// pre-existing R-plugin span quirk recorded in
/// `sqry-plugin-registry/tests/shape_coverage_workspace.rs`, where the recorded
/// extent matches no single tree-sitter node, so it is not asserted here.
#[test]
fn no_shape_descriptor_without_a_body_hash() {
    let nodes = observe_root(&fixtures_root());
    let stranded: Vec<&ObservedNode> = nodes
        .iter()
        .filter(|n| n.has_shape_descriptor && !n.has_body_hash)
        .collect();
    assert!(
        stranded.is_empty(),
        "shape descriptors must not outlive their body hash: {stranded:#?}"
    );
    // Non-vacuity floor. "No descriptor lacks a body hash" is trivially true if
    // no descriptor exists at all, which is a different regression than the one
    // this test names and which the mutation matrix does not induce.
    let with_descriptor = nodes.iter().filter(|n| n.has_shape_descriptor).count();
    assert!(
        with_descriptor >= 100,
        "non-vacuity floor: expected the corpus to carry at least 100 shape \
         descriptors, saw {with_descriptor}"
    );
}

/// Pins the population where `is_definition` and span provenance disagree, and
/// with it the reason the fix keys off the call path rather than the flag.
///
/// These four nodes are real `extern` prototype declarations. They own the
/// extent they sit at, so span provenance keeps them in the body plane, but
/// their plugins mint them through a bare `add_*` helper without opting into
/// `is_definition`, so an `is_definition` gate would drop them. Any growth in
/// this set means a new minting path is handing declaration extents to nodes
/// that never opt in, which is worth seeing.
#[test]
fn body_carrying_non_definitions_are_only_ffi_prototypes() {
    let nodes = observe_root(&fixtures_root());
    let observed: BTreeSet<String> = nodes
        .iter()
        .filter(|n| {
            matches!(n.kind, NodeKind::Function | NodeKind::Method)
                && n.has_body_hash
                && !n.is_definition
        })
        .map(|n| format!("{} {}:{}", n.name, n.file, n.start_line))
        .collect();

    let expected: BTreeSet<String> = [
        "extern::C::calculate_product /cross-language/ffi/bindings.rs:4",
        "extern::C::calculate_sum /cross-language/ffi/bindings.rs:3",
        "extern::C::compute /shape/systems/sample.c:5",
        "extern::C::emit /shape/systems/sample.c:6",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();

    assert_eq!(
        observed, expected,
        "the body-carrying non-definition set changed; every member must be a \
         real declaration whose plugin does not set is_definition"
    );
}

/// Every body-hashable node on the committed fixture corpus that carries a body
/// hash without `is_definition`, pinned exactly. Populated by measurement; see
/// `body_carrying_non_definitions_across_every_kind_are_pinned`.
const EXPECTED_BODY_CARRYING_NON_DEFINITIONS: &[&str] = &[
    // Real `extern` prototype declarations. They own the extent they sit at, so
    // span provenance keeps them; an `is_definition` gate would drop them.
    "Function extern::C::calculate_product /cross-language/ffi/bindings.rs:4",
    "Function extern::C::calculate_sum /cross-language/ffi/bindings.rs:3",
    "Function extern::C::compute /shape/systems/sample.c:5",
    "Function extern::C::emit /shape/systems/sample.c:6",
    // Real declarations whose plugin never opts into `is_definition`: CSS rules,
    // a shell file module, two TypeScript classes.
    "Module css::rule::::card:hover@16:0 /shape/data/sample.css:17",
    "Module css::rule::::card@10:0 /shape/data/sample.css:11",
    "Module css::rule::::card@21:2 /shape/data/sample.css:22",
    "Module css::rule:::root@5:0 /shape/data/sample.css:6",
    "Module script::module /shape/dynamic/script.sh:1",
    "Class Archive /cross-language/typescript/fields.ts:11",
    "Class Ledger /cross-language/typescript/fields.ts:1",
    // Pre-existing and NOT caused by this change: these five anonymous
    // TypeScript class nodes all share one body hash across three files, so they
    // are a false duplicate group in the Class plane that `sqry-lsp`'s
    // kind-filter-free duplicate surface can see. Pinned here so it cannot grow
    // unnoticed; fixing it is a separate change against the TypeScript plugin's
    // anonymous-class span recording.
    "Class <anon:class@11> /cross-language/typescript/fields.ts:11",
    "Class <anon:class@14> /cross-language/typescript/realistic.ts:14",
    "Class <anon:class@1> /cross-language/typescript/fields.ts:1",
    "Class <anon:class@2> /cross-language/typescript/realistic.ts:2",
    "Class <anon:class@5> /e2e-scenarios/multi-lang/src/utils.ts:5",
];

/// The loss round 4 reproduced, pinned: a genuine duplicate must survive a
/// later reference to the type.
///
/// Two namespaces hold byte-identical `struct ProbePayload { int value; };`
/// bodies. Each is followed, INSIDE its namespace, by a prototype naming the
/// type, which qualifies to the same name and so lands on the same node. Under
/// the latest-ending rule the prototype takes over the recorded location, and
/// the earlier model, which asked whether the RECORDED extent was a body,
/// dropped both hashes and lost a real duplicate group. The body extent is now
/// recorded independently, so the declaration keeps its own bytes.
#[test]
fn a_later_reference_does_not_cost_a_type_its_duplicate_body() {
    let one = concat!(
        "namespace one {\n",
        "struct ProbePayload { int value; };\n",
        "void useOne(struct ProbePayload *p);\n",
        "}\n",
    );
    let two = concat!(
        "namespace two {\n",
        "struct ProbePayload { int value; };\n",
        "void useTwo(struct ProbePayload *p);\n",
        "}\n",
    );
    let nodes = observe(&[("one.cpp", one), ("two.cpp", two)]);

    let a = find(&nodes, "one::ProbePayload");
    let b = find(&nodes, "two::ProbePayload");
    assert_eq!(
        a.start_line, 3,
        "the prototype on line 3 does take over the recorded location, which is \
         the pre-existing span policy and is left alone (got {a:?})"
    );
    assert!(
        a.has_body_hash && b.has_body_hash,
        "but each struct keeps the body it owns (got {a:?} and {b:?})"
    );
    assert_eq!(
        a.body_hash_hex, b.body_hash_hex,
        "and the two identical bodies still hash alike, so the duplicate group \
         survives (got {a:?} and {b:?})"
    );
}

/// C++ elaborated type references: a bodyless tagged specifier at a REFERENCE
/// site is neither a declaration nor a body.
///
/// `void render(enum Color c, int slot);` names an enum declared elsewhere, and
/// `struct Payload *slot;` inside a class body names a struct declared
/// elsewhere. Neither is a declaration, and the second is not a nested type at
/// all: `Holder::Payload` is a name nothing in the source has.
#[test]
fn cpp_elaborated_type_references_do_not_declare_or_hash() {
    // Two files that share nothing but a prototype naming an enum NEITHER of
    // them declares. Before the fix both minted an `is_definition` Enum at the
    // prototype's extent and the two hashed alike: a false duplicate group.
    let one = "void renderOne(enum ProbeHue c, int slot) { (void)c; (void)slot; }\n";
    let two = "void renderTwo(enum ProbeHue c, int slot) { (void)c; (void)slot; }\n";
    let nodes = observe(&[("one.cpp", one), ("two.cpp", two)]);
    // Enum is not in `CALL_COMPATIBLE_KINDS`, so the two per-file nodes are not
    // unified. Both must be clean.
    let hues: Vec<&ObservedNode> = nodes.iter().filter(|n| n.name == "ProbeHue").collect();
    assert_eq!(hues.len(), 2, "one per file (got {hues:#?})");
    assert!(
        hues.iter().all(|n| !n.has_body_hash),
        "a type named only in a parameter list owns no body (got {hues:#?})"
    );
    assert!(
        hues.iter().all(|n| !n.is_definition),
        "and naming a type is not declaring it (got {hues:#?})"
    );

    // A member declared with an elaborated type reference is not a nested type.
    let member_src = concat!(
        "struct ProbePayload { int a; int b; };\n",
        "class Holder {\n",
        "public:\n",
        "    struct ProbePayload *slot;\n",
        "    int one() { return 1; }\n",
        "};\n",
    );
    let nodes = observe(&[("holder.cpp", member_src)]);
    let fabricated: Vec<&ObservedNode> = nodes
        .iter()
        .filter(|n| n.name == "Holder::ProbePayload")
        .collect();
    assert!(
        fabricated.is_empty(),
        "a member declared with an elaborated type reference is not a nested \
         type; `Holder::ProbePayload` names nothing in the source (got \
         {fabricated:#?})"
    );
    // The member reference resolves to the real struct, which keeps its own
    // line-1 body.
    let payload = find(&nodes, "ProbePayload");
    assert!(
        payload.has_body_hash,
        "the referenced struct keeps the body it declared on line 1 (got \
         {payload:?})"
    );
}

/// A class member wrapped in a preprocessor conditional is still a class
/// member.
///
/// `walk_class_body` iterated the body's DIRECT children, so anything inside a
/// `#ifdef` never reached the member arms and fell through to the generic file
/// walker with the class stack still pushed. That re-fabricated the
/// `Holder::Payload` name the nested-reference fix exists to prevent, through a
/// completely different code path.
#[test]
fn a_guarded_class_member_is_still_walked_as_a_member() {
    let src = concat!(
        "struct ProbeExternal { int a; int b; };\n",
        "class ProbeHolder {\n",
        "public:\n",
        "#ifdef PROBE_FEATURE\n",
        "    struct ProbeExternal *slot;\n",
        "#endif\n",
        "    int go() { return 1; }\n",
        "};\n",
    );
    let nodes = observe(&[("holder.cpp", src)]);

    let fabricated: Vec<&ObservedNode> = nodes
        .iter()
        .filter(|n| n.name == "ProbeHolder::ProbeExternal")
        .collect();
    assert!(
        fabricated.is_empty(),
        "a guarded member's elaborated type reference is not a nested type \
         either; `ProbeHolder::ProbeExternal` names nothing in the source (got \
         {fabricated:#?})"
    );

    // And the reference resolves to the real struct, which keeps the body it
    // declared on line 1.
    let external = find(&nodes, "ProbeExternal");
    assert!(
        external.is_definition && external.has_body_hash,
        "the referenced struct keeps its own declaration and body (got \
         {external:?})"
    );
}

/// An elaborated reference resolves innermost-scope-first, the way C++ name
/// lookup does.
///
/// `class Outer { class Inner; class Inner *slot; };` declares `Outer::Inner`
/// and then names it. Qualifying the reference with the namespace stack alone
/// minted a SECOND, namespace-level `Inner`, so the declaration and its use
/// were two unrelated nodes.
#[test]
fn a_member_naming_its_own_class_nested_type_resolves_to_it() {
    let src = concat!(
        "class ProbeOuter {\n",
        "public:\n",
        "    class ProbeInner;\n",
        "    class ProbeInner *slot;\n",
        "    int go() { return 1; }\n",
        "};\n",
    );
    let nodes = observe(&[("nested.cpp", src)]);

    let orphans: Vec<&ObservedNode> = nodes.iter().filter(|n| n.name == "ProbeInner").collect();
    assert!(
        orphans.is_empty(),
        "the member reference must resolve to the nested type the same class \
         declared, not mint a namespace-level twin (got {orphans:#?})"
    );

    let nested = find(&nodes, "ProbeOuter::ProbeInner");
    assert!(
        nested.is_definition,
        "the nested forward declaration declares the symbol (got {nested:?})"
    );
    assert!(
        !nested.has_body_hash,
        "and it still has no body to fingerprint (got {nested:?})"
    );
}

/// A forward declaration is a DECLARATION, not a reference.
///
/// `struct Config;` and `class Widget;` name a symbol without giving it a
/// body. Round 4 caught a two-way `has a body or it is a reference` split
/// sending them to the reference sink, which cleared `is_definition`, a bit
/// `find_unused`, the items filter and centrality all read. The committed C
/// fixture corpus contains exactly this shape.
///
/// The other half of the contract is that they stay OUT of the body plane:
/// hashing a declaration line would group every forward declaration whose text
/// happens to match.
#[test]
fn forward_declarations_stay_definitions_and_stay_out_of_the_body_plane() {
    struct ForwardCase {
        file: &'static str,
        src: &'static str,
        declared: &'static str,
        /// A name in the same file that is a REFERENCE, to prove the fixture
        /// distinguishes the two rather than marking everything a definition.
        referenced: &'static str,
    }

    let cases = &[
        ForwardCase {
            file: "fwd.c",
            src: concat!(
                "struct ProbeConfig;\n",
                "union ProbeBag;\n",
                "enum ProbeState;\n",
                "void takes(struct ProbeOther *o);\n",
            ),
            declared: "ProbeConfig",
            referenced: "ProbeOther",
        },
        // The two reference shapes whose PARENT is on the forward-declaration
        // allow-list and which are told apart only by the `declarator` check:
        // a file-scope variable (parent `declaration`) and a struct field
        // (parent `field_declaration`). A parameter reference cannot cover
        // this, because `parameter_declaration` is not on the list at all.
        ForwardCase {
            file: "declarator.c",
            src: concat!(
                "struct ProbeVarFwd;\n",
                "static struct ProbeVarRef *global_slot;\n",
            ),
            declared: "ProbeVarFwd",
            referenced: "ProbeVarRef",
        },
        ForwardCase {
            file: "field.c",
            src: concat!(
                "struct ProbeWrap {\n",
                "    struct ProbeFieldFwd;\n",
                "    struct ProbeFieldRef *inner;\n",
                "};\n",
            ),
            declared: "ProbeFieldFwd",
            referenced: "ProbeFieldRef",
        },
        // A header with an include guard, which is what essentially every real
        // header is. tree-sitter parses the guard as a `preproc_ifdef` holding
        // the declarations directly, so a forward declaration's parent is NOT
        // `translation_unit`. Leaving preprocessor conditionals off the
        // allow-list made this whole case unreachable in practice, and the
        // committed fixture at
        // `sqry-lang-c/tests/fixtures/c/exports/forward_declarations.h` is
        // exactly this shape.
        ForwardCase {
            file: "guarded.h",
            src: concat!(
                "#ifndef PROBE_GUARD_H\n",
                "#define PROBE_GUARD_H\n",
                "struct ProbeGuardedFwd;\n",
                "void guarded_takes(struct ProbeGuardedRef *o);\n",
                "#endif\n",
            ),
            declared: "ProbeGuardedFwd",
            referenced: "ProbeGuardedRef",
        },
        ForwardCase {
            file: "guarded.hpp",
            src: concat!(
                "#ifndef PROBE_GUARD_HPP\n",
                "#define PROBE_GUARD_HPP\n",
                "#ifdef PROBE_FEATURE\n",
                "class ProbeGuardedCppFwd;\n",
                "#endif\n",
                "void guarded_takes(class ProbeGuardedCppRef *o);\n",
                "#endif\n",
            ),
            declared: "ProbeGuardedCppFwd",
            referenced: "ProbeGuardedCppRef",
        },
        // Every arm of a conditional is a separate grammar node, so each needs
        // its own case: asserting one arm leaves the others minted but
        // unchecked, which is what the allow-list mutation sweep caught.
        //
        // `#else`.
        ForwardCase {
            file: "branch_else.h",
            src: concat!(
                "#ifdef PROBE_A\n",
                "struct ProbeUnused;\n",
                "#else\n",
                "struct ProbeElseFwd;\n",
                "#endif\n",
                "void else_takes(struct ProbeElseRef *o);\n",
            ),
            declared: "ProbeElseFwd",
            referenced: "ProbeElseRef",
        },
        // `#elifdef`, which also covers `#elifndef`: both parse to the same
        // node kind.
        ForwardCase {
            file: "branch_elifdef.h",
            src: concat!(
                "#ifdef PROBE_A\n",
                "struct ProbeUnused;\n",
                "#elifdef PROBE_B\n",
                "struct ProbeElifdefFwd;\n",
                "#endif\n",
                "void elifdef_takes(struct ProbeElifdefRef *o);\n",
            ),
            declared: "ProbeElifdefFwd",
            referenced: "ProbeElifdefRef",
        },
        // `#if` and `#elif`, the expression forms, which are distinct node
        // kinds from the `#ifdef` family.
        ForwardCase {
            file: "branch_if.h",
            src: concat!(
                "#if defined(PROBE_A)\n",
                "struct ProbeIfFwd;\n",
                "#endif\n",
                "void if_takes(struct ProbeIfRef *o);\n",
            ),
            declared: "ProbeIfFwd",
            referenced: "ProbeIfRef",
        },
        ForwardCase {
            file: "branch_elif.h",
            src: concat!(
                "#if defined(PROBE_A)\n",
                "struct ProbeUnused;\n",
                "#elif defined(PROBE_B)\n",
                "struct ProbeElifFwd;\n",
                "#endif\n",
                "void elif_takes(struct ProbeElifRef *o);\n",
            ),
            declared: "ProbeElifFwd",
            referenced: "ProbeElifRef",
        },
        // One case per remaining allow-list entry. Before these, five of the
        // nine entries had no fixture at all, so deleting any of those arms was
        // a mutation that survived the whole suite.
        //
        // `declaration_list`: a namespace body.
        ForwardCase {
            file: "ns.cpp",
            src: concat!(
                "namespace probe_ns {\n",
                "class ProbeNsFwd;\n",
                "void ns_takes(class ProbeNsRef *o);\n",
                "}\n",
            ),
            declared: "probe_ns::ProbeNsFwd",
            referenced: "probe_ns::ProbeNsRef",
        },
        // `declaration_list` again, this time an `extern "C"` block.
        ForwardCase {
            file: "linkage.cpp",
            src: concat!(
                "extern \"C\" {\n",
                "struct ProbeLinkFwd;\n",
                "void link_takes(struct ProbeLinkRef *o);\n",
                "}\n",
            ),
            declared: "ProbeLinkFwd",
            referenced: "ProbeLinkRef",
        },
        // `compound_statement`: a block-scope forward declaration.
        ForwardCase {
            file: "local.c",
            src: concat!(
                "void probe_local(void) {\n",
                "    struct ProbeLocalFwd;\n",
                "    struct ProbeLocalRef *p = 0;\n",
                "    (void)p;\n",
                "}\n",
            ),
            declared: "ProbeLocalFwd",
            referenced: "ProbeLocalRef",
        },
        // `template_declaration`, C++ only.
        ForwardCase {
            file: "template.cpp",
            src: concat!(
                "template <typename T> class ProbeTmplFwd;\n",
                "void tmpl_takes(class ProbeTmplRef *o);\n",
            ),
            declared: "ProbeTmplFwd",
            referenced: "ProbeTmplRef",
        },
        // `declaration`, reachable only once a MISSING declarator stops
        // counting as a real one. An attribute prefix makes tree-sitter parse
        // the forward as a `declaration` with `declarator: (MISSING
        // identifier)`, which is the shape that used to send every prefixed
        // forward to the reference sink.
        ForwardCase {
            file: "attr.c",
            src: concat!(
                "__attribute__((unused)) struct ProbeAttrFwd;\n",
                "void attr_takes(struct ProbeAttrRef *o);\n",
            ),
            declared: "ProbeAttrFwd",
            referenced: "ProbeAttrRef",
        },
        ForwardCase {
            file: "attr.cpp",
            src: concat!(
                "[[maybe_unused]] class ProbeAttrCppFwd;\n",
                "void attr_cpp_takes(class ProbeAttrCppRef *o);\n",
            ),
            declared: "ProbeAttrCppFwd",
            referenced: "ProbeAttrCppRef",
        },
        ForwardCase {
            file: "fwd.cpp",
            src: concat!(
                "class ProbeWidget;\n",
                "enum ProbeShade : int;\n",
                "void takes(class ProbeOtherCpp *o);\n",
            ),
            declared: "ProbeWidget",
            referenced: "ProbeOtherCpp",
        },
        ForwardCase {
            file: "nested.cpp",
            src: concat!(
                "class ProbeHolder {\n",
                "public:\n",
                "    class ProbeInner;\n",
                "    struct ProbeElsewhere *slot;\n",
                "    int one() { return 1; }\n",
                "};\n",
            ),
            // A nested forward declaration IS a nested type: `Holder::Inner` is
            // a real name. A member's elaborated type reference is not.
            declared: "ProbeHolder::ProbeInner",
            referenced: "ProbeElsewhere",
        },
    ];

    for case in cases {
        let nodes = observe(&[(case.file, case.src)]);
        let declared = find(&nodes, case.declared);
        assert!(
            declared.is_definition,
            "{}: a forward declaration declares the symbol (got {declared:?})",
            case.file
        );
        assert!(
            !declared.has_body_hash && !declared.has_shape_descriptor,
            "{}: but it has no body to fingerprint (got {declared:?})",
            case.file
        );

        let referenced = find(&nodes, case.referenced);
        assert!(
            !referenced.is_definition,
            "{}: naming a type in a declarator is not declaring it, or this \
             fixture is not telling the two apart (got {referenced:?})",
            case.file
        );
        assert!(
            !referenced.has_body_hash,
            "{}: and a reference owns no body either (got {referenced:?})",
            case.file
        );
    }
}

/// The one site in the #748 sweep that changes a recorded LOCATION.
///
/// The TypeScript route-handler reference moved from `ensure_function` to
/// `ensure_callee`. `ensure_callee` returns an exact-kind cache hit as-is,
/// where `ensure_function` fell through to the cached-node update and applied
/// the route-argument span. So a handler declared in the same file now keeps
/// its DECLARATION's location instead of being pulled to the registration.
///
/// Pinned because the branch claims everywhere else that only provenance
/// changed, and this is the exception.
#[test]
fn a_declared_route_handler_keeps_its_declaration_location() {
    let src = concat!(
        "import express from \"express\";\n",
        "const app = express();\n",
        "\n",
        "function listUsers(req: any, res: any) {\n",
        "    if (req) {\n",
        "        res.json([]);\n",
        "    }\n",
        "    return 1;\n",
        "}\n",
        "\n",
        "app.get(\"/api/users\", listUsers);\n",
    );
    let nodes = observe(&[("server.ts", src)]);

    let handler = find(&nodes, "listUsers");
    assert_eq!(
        handler.start_line, 4,
        "the handler keeps its own declaration line, not line 11 where the route \
         registration names it (got {handler:?})"
    );
    assert!(
        handler.has_body_hash && handler.has_shape_descriptor,
        "and it keeps its body, because its extent is still its own (got {handler:?})"
    );
}

/// Every C and C++ `Struct` / `Class` / `Enum` node on the committed fixture
/// corpus that is NOT a definition, pinned exactly. Populated by measurement;
/// see `c_and_cpp_type_nodes_that_are_not_definitions_are_pinned`.
///
/// All 56 are types the `linux-driver-subset` fixture NAMES but never
/// declares: kernel structs like `inode`, `super_block` and `file_operations`
/// that the real headers define elsewhere. On the base commit each reference
/// site minted a node, called `mark_definition` on it, and hashed it over the
/// reference's bytes. Every entry here is a reference correctly refusing to
/// claim it is a declaration.
///
/// The path has no line number on purpose: a reference's recorded location
/// follows the latest-ending mention, which moves whenever the fixture is
/// edited anywhere below. The pin is about WHICH types are references, not
/// where they are reported.
const EXPECTED_C_FAMILY_NON_DEFINITIONS: &[&str] = &[
    "Struct address_space /c-icall-precision/linux-driver-subset/file.c",
    "Struct address_space /c-icall-precision/linux-driver-subset/verity.c",
    "Struct address_space_operations /c-icall-precision/linux-driver-subset/verity.c",
    "Struct buffer_head /c-icall-precision/linux-driver-subset/dir.c",
    "Struct buffer_head /c-icall-precision/linux-driver-subset/symlink.c",
    "Struct dax_device /c-icall-precision/linux-driver-subset/file.c",
    "Struct delayed_call /c-icall-precision/linux-driver-subset/symlink.c",
    "Struct dentry /c-icall-precision/linux-driver-subset/symlink.c",
    "Struct dir_context /c-icall-precision/linux-driver-subset/dir.c",
    "Struct dir_private_info /c-icall-precision/linux-driver-subset/dir.c",
    "Struct ext4_dir_entry_2 /c-icall-precision/linux-driver-subset/dir.c",
    "Struct ext4_ext_path /c-icall-precision/linux-driver-subset/verity.c",
    "Struct ext4_extent /c-icall-precision/linux-driver-subset/verity.c",
    "Struct ext4_fsmap /c-icall-precision/linux-driver-subset/fsmap.c",
    "Struct ext4_fsmap_head /c-icall-precision/linux-driver-subset/fsmap.c",
    "Struct ext4_group_desc /c-icall-precision/linux-driver-subset/fsmap.c",
    "Struct ext4_iloc /c-icall-precision/linux-driver-subset/verity.c",
    "Struct ext4_map_blocks /c-icall-precision/linux-driver-subset/dir.c",
    "Struct ext4_map_blocks /c-icall-precision/linux-driver-subset/file.c",
    "Struct ext4_sb_info /c-icall-precision/linux-driver-subset/file.c",
    "Struct ext4_sb_info /c-icall-precision/linux-driver-subset/fsmap.c",
    "Struct file /c-icall-precision/linux-driver-subset/dir.c",
    "Struct file /c-icall-precision/linux-driver-subset/file.c",
    "Struct file /c-icall-precision/linux-driver-subset/verity.c",
    "Struct file_operations /c-icall-precision/linux-driver-subset/dir.c",
    "Struct file_operations /c-icall-precision/linux-driver-subset/file.c",
    "Struct folio /c-icall-precision/linux-driver-subset/verity.c",
    "Struct fscrypt_str /c-icall-precision/linux-driver-subset/dir.c",
    "Struct fsmap /c-icall-precision/linux-driver-subset/fsmap.c",
    "Struct fsverity_operations /c-icall-precision/linux-driver-subset/verity.c",
    "Struct inode /c-icall-precision/linux-driver-subset/dir.c",
    "Struct inode /c-icall-precision/linux-driver-subset/file.c",
    "Struct inode /c-icall-precision/linux-driver-subset/symlink.c",
    "Struct inode /c-icall-precision/linux-driver-subset/verity.c",
    "Struct inode_operations /c-icall-precision/linux-driver-subset/file.c",
    "Struct inode_operations /c-icall-precision/linux-driver-subset/symlink.c",
    "Struct iomap_dio_ops /c-icall-precision/linux-driver-subset/file.c",
    "Struct iomap_ops /c-icall-precision/linux-driver-subset/file.c",
    "Struct iov_iter /c-icall-precision/linux-driver-subset/file.c",
    "Struct kiocb /c-icall-precision/linux-driver-subset/file.c",
    "Struct kstat /c-icall-precision/linux-driver-subset/symlink.c",
    "Struct list_head /c-icall-precision/linux-driver-subset/fsmap.c",
    "Struct mnt_idmap /c-icall-precision/linux-driver-subset/symlink.c",
    "Struct page /c-icall-precision/linux-driver-subset/verity.c",
    "Struct path /c-icall-precision/linux-driver-subset/file.c",
    "Struct path /c-icall-precision/linux-driver-subset/symlink.c",
    "Struct pipe_inode_info /c-icall-precision/linux-driver-subset/file.c",
    "Struct rb_node /c-icall-precision/linux-driver-subset/dir.c",
    "Struct rb_root /c-icall-precision/linux-driver-subset/dir.c",
    "Struct super_block /c-icall-precision/linux-driver-subset/dir.c",
    "Struct super_block /c-icall-precision/linux-driver-subset/file.c",
    "Struct super_block /c-icall-precision/linux-driver-subset/fsmap.c",
    "Struct vfsmount /c-icall-precision/linux-driver-subset/file.c",
    "Struct vm_area_struct /c-icall-precision/linux-driver-subset/file.c",
    "Struct vm_fault /c-icall-precision/linux-driver-subset/file.c",
    "Struct vm_operations_struct /c-icall-precision/linux-driver-subset/file.c",
];

/// The committed C fixture round 4 named, asserted end to end.
///
/// `sqry-lang-c/tests/fixtures/c/exports/forward_declarations.h` holds a bare
/// `struct Config;` inside the file's `#ifndef FORWARD_DECLARATIONS_H` include
/// guard, and two references to the same type below it (a parameter and a
/// `typedef`). It is the shape the third `SpanOrigin` variant exists for, and
/// checking a synthetic fixture of the same nominal kind was not enough: the
/// guard put the declaration under a `preproc_ifdef` parent that the
/// allow-list did not cover, and the node lost `is_definition` anyway.
///
/// Pinned against the real file so that cannot recur.
#[test]
fn the_committed_c_forward_declaration_fixture_keeps_its_definition_bit() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root is the parent of sqry-plugin-registry")
        .join("sqry-lang-c/tests/fixtures/c/exports");
    assert!(
        root.join("forward_declarations.h").is_file(),
        "the fixture this test is named for must exist at {}",
        root.display()
    );

    let nodes = observe_root(&root);
    let config = find(&nodes, "Config");
    assert!(
        config.is_definition,
        "`struct Config;` is a forward declaration, so the symbol IS declared \
         here (got {config:?})"
    );
    assert!(
        !config.has_body_hash && !config.has_shape_descriptor,
        "but it has no body to fingerprint (got {config:?})"
    );

    // Non-vacuity: the same file declares types WITH bodies, and those must
    // still carry a hash, or an implementation that stripped every type's body
    // would pass the assertions above.
    let point = find(&nodes, "Point");
    assert!(
        point.is_definition && point.has_body_hash,
        "a bodied struct in the same file keeps both (got {point:?})"
    );
}

/// Every C and C++ type node on the committed corpus that is NOT a definition,
/// pinned exactly.
///
/// The other three pins all start from a population that the defect removes
/// the node from: two filter `has_body_hash && !is_definition`, and a forward
/// declaration correctly has no body hash; the third filters `is_definition`,
/// so a node that wrongly lost the bit is simply absent. None of them can see
/// a type that should be a definition and is not.
///
/// This one keys on the bit itself, over the kinds the C and C++ classifiers
/// mint. A flip in either direction is a diff here.
#[test]
fn c_and_cpp_type_nodes_that_are_not_definitions_are_pinned() {
    let nodes = observe_root(&fixtures_root());
    let observed: BTreeSet<String> = nodes
        .iter()
        .filter(|n| {
            matches!(n.kind, NodeKind::Struct | NodeKind::Class | NodeKind::Enum)
                && (n.file.ends_with(".c")
                    || n.file.ends_with(".h")
                    || n.file.ends_with(".cpp")
                    || n.file.ends_with(".hpp"))
                && !n.is_definition
        })
        .map(|n| format!("{:?} {} {}", n.kind, n.name, n.file))
        .collect();

    let expected: BTreeSet<String> = EXPECTED_C_FAMILY_NON_DEFINITIONS
        .iter()
        .map(|s| (*s).to_string())
        .collect();

    let added: Vec<&String> = observed.difference(&expected).collect();
    let removed: Vec<&String> = expected.difference(&observed).collect();
    assert!(
        added.is_empty() && removed.is_empty(),
        "the C/C++ non-definition type set changed.\n  added:   {added:#?}\n  \
         removed: {removed:#?}\nEvery member must be a REFERENCE to a type \
         declared elsewhere. A member disappearing means a reference started \
         claiming to be a declaration; a member appearing means a real \
         declaration lost its definition bit."
    );

    // Non-vacuity: the set must not be empty, and the corpus must also contain
    // C/C++ type nodes that ARE definitions, or this pin would pass against a
    // build that minted no type nodes at all.
    assert!(
        !observed.is_empty(),
        "the corpus is expected to contain C/C++ type references"
    );
    assert!(
        nodes.iter().any(|n| {
            matches!(n.kind, NodeKind::Struct | NodeKind::Class | NodeKind::Enum)
                && (n.file.ends_with(".c") || n.file.ends_with(".cpp"))
                && n.is_definition
        }),
        "and to contain C/C++ type declarations"
    );
}

/// Every group the kind-filter-free duplicate surface reports on the committed
/// fixture corpus, pinned exactly. Populated by measurement; see
/// `the_lsp_duplicate_surface_reports_only_real_duplicate_bodies`.
const EXPECTED_LSP_DUPLICATE_GROUPS: &[&str] = &[
    // GENUINE. Two byte-identical `App.java` files, one under the Gradle
    // fixture project and one under the Maven one.
    "Class com::example::App /jvm-classpath/gradle-single-module/src/main/java/App.java:5 | \
Class com::example::App /jvm-classpath/maven-single-module/src/main/java/App.java:5",
    // GENUINE. The same arrow function written in JavaScript and TypeScript.
    "Function anon:arrow:b214272d /shape/reference/sample.js:15 | \
Method classify::<anon:arrow@15> /shape/reference/sample.ts:15",
    // GENUINE. Byte-identical C functions across the address-taken fixtures.
    "Function my_func /c-icall-precision/address-taken-patterns/argument_pass.c:15 | \
Function my_func /c-icall-precision/address-taken-patterns/unary_amp.c:15",
    "Function my_func /c-icall-precision/address-taken-patterns/init_declarator.c:13 | \
Function my_func /c-icall-precision/address-taken-patterns/return_function.c:14 | \
Function my_func /c-icall-precision/address-taken-patterns/subscript_assign.c:18",
    "Function my_read /c-icall-precision/address-taken-patterns/designated_init.c:21 | \
Function my_read /c-icall-precision/address-taken-patterns/field_assign.c:20 | \
Function my_read /c-icall-precision/address-taken-patterns/positional_init.c:20",
    // GENUINE, and the one group this change RECOVERS. Both files declare a
    // byte-identical `struct ops { int (*read)(int); int (*write)(int); };` on
    // lines 16-19, and both then name the type again in a
    // `static const struct ops g_ops = ...` below it. That reference wins the
    // recorded location, which is why the group is reported at lines 25 and 24
    // rather than at 16 (a pre-existing span-modelling question, unchanged
    // here). Under the model this change replaced, losing the location cost
    // each struct its body hash and the duplicate went unreported.
    "Struct ops /c-icall-precision/address-taken-patterns/designated_init.c:25 | \
Struct ops /c-icall-precision/address-taken-patterns/positional_init.c:24",
    // NOT genuine, and NOT caused by this change: five anonymous TypeScript
    // class nodes share one body hash across three files. A pre-existing defect
    // in the TypeScript plugin's anonymous-class span recording, visible on
    // this surface, out of scope here, and pinned so it cannot grow.
    "Class <anon:class@11> /cross-language/typescript/fields.ts:11 | \
Class <anon:class@14> /cross-language/typescript/realistic.ts:14 | \
Class <anon:class@1> /cross-language/typescript/fields.ts:1 | \
Class <anon:class@2> /cross-language/typescript/realistic.ts:2 | \
Class <anon:class@5> /e2e-scenarios/multi-lang/src/utils.ts:5",
];

/// The gate that measures the surface the design doc actually names.
///
/// Both equality pins above filter on `!is_definition`, and that filter is
/// itself a hiding place: a plugin can mint a node at a REFERENCE site and then
/// call `mark_definition` on it, and every such leak is invisible to them. C and
/// C++ did exactly that for bodyless tagged type specifiers, and it produced 24
/// false duplicate groups on this corpus that every other gate passed over.
///
/// So this one asks the question `sqry-lsp/src/handlers/index.rs` asks: group
/// every node that carries a body hash by that hash, with no kind filter and no
/// `is_definition` filter, and pin the result. It is the strictest statement of
/// "the body plane reports only real duplicate bodies" that this corpus can
/// make.
#[test]
fn the_lsp_duplicate_surface_reports_only_real_duplicate_bodies() {
    let nodes = observe_root(&fixtures_root());

    let mut by_hash: std::collections::BTreeMap<String, Vec<&ObservedNode>> =
        std::collections::BTreeMap::new();
    for node in &nodes {
        if let Some(hash) = &node.body_hash_hex {
            by_hash.entry(hash.clone()).or_default().push(node);
        }
    }
    let groups: Vec<Vec<&ObservedNode>> = by_hash
        .into_values()
        .filter(|members| members.len() > 1)
        .collect();

    let rendered: BTreeSet<String> = groups
        .iter()
        .map(|members| {
            let mut names: Vec<String> = members
                .iter()
                .map(|n| format!("{:?} {} {}:{}", n.kind, n.name, n.file, n.start_line))
                .collect();
            names.sort();
            names.join(" | ")
        })
        .collect();

    let expected: BTreeSet<String> = EXPECTED_LSP_DUPLICATE_GROUPS
        .iter()
        .map(|s| (*s).to_string())
        .collect();

    assert_eq!(
        rendered,
        expected,
        "the kind-filter-free duplicate surface changed.\n  added:   {:#?}\n  removed: {:#?}",
        rendered.difference(&expected).collect::<Vec<_>>(),
        expected.difference(&rendered).collect::<Vec<_>>()
    );
}

/// The same equality pin, widened to EVERY body-hashable kind.
///
/// The Function/Method filter on the test above is precisely what let two
/// misses land green through round 2: a `Module` stub for a TypeScript
/// re-export and a `Class` stub for `new GlideAjax(...)` are invisible to it,
/// and to `duplicates --type body`, but perfectly visible to `sqry-lsp`'s
/// `collect_duplicate_body_groups`, which groups any node carrying a body hash
/// with no kind filter at all.
///
/// So this pins the whole set exactly. A new entry means a minting path is
/// handing a body hash to a node that did not opt into `is_definition`, which
/// is either a real declaration the plugin never flagged (fine, add it here
/// with a note) or a stub that escaped the sweep (not fine).
///
/// The `<anon:class@N>` entries are worth knowing about: all five share one
/// body hash across three files, which is a pre-existing false-duplicate group
/// in the Class plane, unrelated to this change and visible to the LSP surface.
/// Pinned here so it cannot quietly grow.
#[test]
fn body_carrying_non_definitions_across_every_kind_are_pinned() {
    let nodes = observe_root(&fixtures_root());
    let observed: BTreeSet<String> = nodes
        .iter()
        .filter(|n| n.has_body_hash && !n.is_definition)
        .map(|n| format!("{:?} {} {}:{}", n.kind, n.name, n.file, n.start_line))
        .collect();

    let expected: BTreeSet<String> = EXPECTED_BODY_CARRYING_NON_DEFINITIONS
        .iter()
        .map(|s| (*s).to_string())
        .collect();

    assert_eq!(
        observed,
        expected,
        "the body-carrying non-definition set changed across the body-hashable \
         kinds.\n  added:   {:?}\n  removed: {:?}",
        observed.difference(&expected).collect::<Vec<_>>(),
        expected.difference(&observed).collect::<Vec<_>>()
    );
}
