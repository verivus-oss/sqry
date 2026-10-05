//! U7 (surface parity W4 round 2, design W4-D14), U15 (round 3, design
//! W4-D20), U21 to U23 (round 4, design W4-D27) and the round 7 repairs,
//! decision D-i7-envlock-1 the last of them: the static half of the check that test
//! code in `sqry-daemon/src` holds the crate-wide `TEST_ENV_LOCK` wherever it
//! touches the process environment.
//!
//! The environment is process-global and `cargo test` runs a crate's unit
//! tests as threads of one binary, so a test that reads or writes a variable
//! without the lock can observe a value another test planted under it.
//! Round 1's `load_config_with_explicit_path_does_not_set_env_var` did that
//! and failed intermittently on a correct tree. Two module-local mutexes did
//! the same thing in a quieter way: a lock nobody else takes serialises
//! nothing against the tests that take the crate's lock.
//!
//! # Two checks
//!
//! - **This gate** reads a tree-sitter parse of every `.rs` file under
//!   `sqry-daemon/src` and decides, for every environment call and every
//!   call chain the parse resolves, whether an accepted shape of holding the
//!   crate lock covers it. It has two sides. The hold side (`# Holding the
//!   lock`) fails closed: it accepts a short list of shapes and nothing else,
//!   so code it does not understand counts as not holding the lock. The reach
//!   side (`# The indirect rule`) cannot be complete: it sees test code no
//!   test runs, but not what a method call (`receiver.name()`, 56% of the
//!   calls) calls, nor what code outside the crate does with a crate value;
//!   the last section lists that and the rest it does not see, each with the
//!   figure the real-crate tests print.
//! - **The runtime check** (`sqry-daemon/src/env_trace.rs`, Linux with glibc
//!   only) runs every lib unit test that `cargo test` runs, each alone in a
//!   process of its own with `--test-threads=1`, with `getenv`,
//!   `secure_getenv`, `setenv`, `unsetenv`, `putenv` and `clearenv`
//!   interposed, and reads whether the crate lock is held from the lock
//!   itself at each access. It fails on a write a test makes without the
//!   lock, and on a read without the lock of a variable some test writes. It
//!   sees method calls and code outside the crate, but only the code the lib
//!   tests execute, on Linux with glibc, with the features of the build; it
//!   is what backs the executed lib tests where the reach side is blind, and
//!   its own module doc says what else it does not see.
//!
//! The set of files, functions and calls is derived from the parse, never
//! listed here, so a new offender fails this test instead of joining an
//! exclusion list. Which nodes are paths naming a value (a path in a grammar
//! slot whose declared types include `_expression`; the real-crate test
//! asserts no such slot also accepts `_pattern`) and which statements are
//! items a body walk does not enter are read from the grammar's `NODE_TYPES`
//! (`GrammarFacts`), not listed here either. Every planted tree the tests
//! below build is compiled with the toolchain's `rustc`, as a test build and
//! as a non-test build (`assert_compiles`), so the cases are Rust rustc
//! accepts; U24's tree uses if-let guards and is compiled with
//! `RUSTC_BOOTSTRAP=1` for that feature gate.
//!
//! # Names
//!
//! Names are resolved over a model of the crates the cargo roots define
//! (`CrateModel`):
//!
//! - the module tree from each root, by rustc's file rules (`#[path]`
//!   included); a `#[path]` inside an inline module, an `include!`, a file
//!   two declarations reach, and a file the liveness model reaches and this
//!   tree does not are unmodelled sites, which the real tests assert away;
//! - every module's and every block's items, explicit imports and glob
//!   imports: an item or an explicit import shadows a glob, a module's
//!   private entry is visible to that module and the modules nested in it
//!   only, and a lookup visits each scope once per module it is made from,
//!   so glob imports that reach one another cost one visit per scope;
//! - in a body, its local bindings (`local_bindings`).
//!
//! A bare name is the innermost of a local binding and an item of a block
//! around it (an item is visible in its whole block, before its declaration
//! too; a nested function sees the items of the blocks around it but not
//! their locals), then a generic parameter, then the module's own items and
//! imports, then the crate's extern prelude (an `extern crate` at the root;
//! `extern crate self as name` names the crate), and otherwise outside the
//! crate. A path walks from `crate`, `self`, `super`, `Self`, `::name`, a
//! `<T as Trait>`, `<T>` or `<dyn Trait>` qualifier, or a name resolved as
//! above, through modules (generic arguments skipped) to an item.
//! `Type::name` is an enum variant, then an inherent impl's item, then a
//! trait impl's item (blanket impls `impl<T> Trait for T` included, their
//! bounds not read) or a method a derive generates (`DERIVED_METHODS`), then
//! a crate trait's default; an impl written on a type alias is the aliased
//! type's, and `Alias::name` names the aliased type's item; `<dyn
//! Trait>::name` and `<u32 as Trait>::name` for a crate trait name the
//! trait's item in every impl. A name with several candidates (the cfg
//! variants of one item) is an edge to each.
//!
//! A `macro_rules!` name is looked up as rustc does: in textual scope first
//! (the last definition before the use in a block or module around it, a
//! module's scope continuing into the modules declared after the definition,
//! and a `#[macro_use]` module's definitions after the module), then by path
//! (a `#[macro_export]` macro at its crate root, a `use` of one). A glob
//! never brings a `macro_rules!` in. A crate macro the body invokes is
//! expanded in place, its transcriber's paths resolved at the call site, as
//! rustc resolves an item path a macro expands to (checked against rustc:
//! the same macro calls a quiet function in its own module and a reading one
//! where a `use` brings that in), without the call site's local bindings. A
//! crate `macro_rules!` invoked in item position (a module's or an impl's
//! item list), which could write a `#[test]` function or a helper, is not
//! expanded: every such invocation is listed (`item-position invocations of a
//! crate macro_rules!`), and the real-crate test asserts there is none.
//!
//! A path is outside the crate when its first segment names nothing the
//! parse binds (the standard library, a dependency, the prelude, an item a
//! macro produces) or names an extern crate; a path the model cannot follow
//! inside the crate (through a generic parameter, to an associated item no
//! impl or derive in the parse defines, through a segment a macro builds) is
//! unresolved, makes no edge, and is printed with its reason and site.
//!
//! # The direct rule
//!
//! - An environment call is a call, or a path named as a value, that resolves
//!   to `std::env::set_var`, `remove_var`, `var`, `var_os`, `vars` or
//!   `vars_os` (through a `use` import, a rename, a glob or `::std` alike), or
//!   that is written ending in `env::<accessor>` and resolves to no crate
//!   item. It is read in a body's own code, in a macro invocation's tokens,
//!   in the transcriber of a crate `macro_rules!` the body invokes (placed at
//!   the invocation), and in the methods clap's derives generate for a type a
//!   field of which carries `#[arg(.., env ..)]` or `#[clap(.., env ..)]`
//!   (clap reads the variable while it builds the command).
//! - An environment call is test code when a test build compiles it and
//!   either no non-test build does or it is in a function a test build runs
//!   as a test (`is_test_function`: `#[test]`, `#[tokio::test]`, or a
//!   `cfg_attr` whose predicate holds in a test build carrying one), over the
//!   model of `sqry_core::test_support::rust_liveness` (design W4-D18) in
//!   both builds (`BuildCfg::for_test_build`). So a `#[cfg(test)]` item,
//!   module, file or statement is test code; a `#[cfg(not(test))]` statement
//!   inside a test is in no test build and is not; a `#[cfg_attr(test,
//!   test)]` function is.
//! - A function item with an environment call in test code that no accepted
//!   shape of its own body covers (`# Holding the lock`) is an offender.
//! - A function in a file no walk from a root reaches is compiled by no
//!   build: it is skipped and counted.
//!
//! # Holding the lock (decision D-i7-envlock-1)
//!
//! Three rounds modelled more of the ways a guard can be kept, moved,
//! carried or released, and in each an independent audit found valid Rust
//! the model read as holding the lock while the lock was free at run time.
//! That cannot converge, so this side fails closed: it accepts only the
//! shapes below, whose holding is evident from the syntax, and any other use
//! of the lock or of a guard holds nothing. An accepted shape is never
//! widened by a cleverer model; a guard named in any way the list does not
//! allow stops covering at that mention. The decision is recorded as D-i7-envlock-1
//! in `docs/development/surface-parity/04_PROGRESS-surface-parity.md`.
//!
//! The crate lock is the `static TEST_ENV_LOCK` declared directly in a crate
//! root's file. Its guard type is found by resolution: what the crate lock's
//! own `lock` returns (`LockResult<G>`'s `G`: the real crate's
//! `crate::TestEnvGuard`), or `std::sync::MutexGuard` for a
//! `std::sync::Mutex` (`CrateModel::lock_guard_types`).
//!
//! A lock path names the crate lock only when the resolution decides it
//! does (decision D-i8-42). The resolution expands no macro into items, so a
//! scope it consults for a segment of the path (a block around the call, a
//! module, a module a glob or an import leads to) that holds a macro
//! invocation in item or statement position, other than a standard-library
//! macro of `IN_PLACE_MACROS`, and does not hold the name among its own items
//! and explicit imports, leaves the answer undecided (`CrateModel::undecided`):
//! a `macro_rules!` there can write `static TEST_ENV_LOCK`, a `const` or a
//! `use` of another mutex under the name, and that item, not the crate lock,
//! is what the call locks. Such a call holds nothing; it is still counted as
//! an acquisition of the crate lock, so the nesting side reports more and the
//! refused figure names it. A block the call is not in, and a scope that
//! holds the name itself (a macro-made duplicate of it is an error), decide
//! nothing. A `use`, a `let` (only where no static of the name is in scope),
//! a block `static` or `const`, a re-export of another mutex under the name
//! and a receiver hidden behind a call or a method chain are no lock path
//! that resolves to the crate lock, and hold nothing either.
//!
//! 1. **A guard** (shape 1) is `let <name> = <lock call>;` with a plain name
//!    (no `mut`, `ref`, type, pattern or `else`, and no attribute but a lint
//!    level), where the lock call is `<path>.lock()`, the path a name or a
//!    path (a `use` alias included; no parentheses, `&` or local) that
//!    resolves to the crate lock, then at most one of `.unwrap()`,
//!    `.expect(<string literal>)`, `.unwrap_or_else(|p| p.into_inner())` and
//!    `.unwrap_or_else(<a path that resolves to
//!    std::sync::PoisonError::into_inner>)`, the whole path matched. It covers
//!    the sites of its own region from the end of the `let` to the end of the
//!    block that holds it, and stops earlier at the first mention of its name
//!    after the `let`: any identifier or macro token of that name, a raw
//!    identifier included, whatever the mention does (a move, a borrow, a
//!    method call, a field access, a dereference, an argument, a capture by a
//!    closure or an async block, `mem::take`, `replace`, `swap` or `forget`).
//!    A mention inside a loop, a closure, an async block, a macro invocation,
//!    a `macro_rules!` or an item stops the guard where the outermost of them
//!    (below the guard's block) starts, so a guard bound outside a loop covers
//!    nothing in a loop that names it, and a loop that never names it is
//!    covered. Its region is the innermost closure or async block around the
//!    `let`, or the body; a site is in the region of the innermost closure,
//!    async block, or macro invocation that does not run its tokens in place
//!    around it (only the standard library's macros of `IN_PLACE_MACROS` run
//!    them in place), so a closure, an async block, a thread spawned, a crate
//!    `macro_rules!` or a dependency's macro written inside a guard's range is
//!    not covered by it.
//! 2. **A helper** (shape 2) is a free function, not `async`, whose body is
//!    exactly one accepted lock call as its tail, or exactly `let <name> =
//!    <lock call>; <name>`. `let <name> = <helper call>;`, a call by a name or
//!    a path whose every candidate is a helper, is a guard as in shape 1. No
//!    other function returns a guard, so no fixpoint is needed.
//! 3. **A holder** (shape 3) is a struct with named fields, exactly one of the
//!    guard type, no attribute on that field, and none on the struct but
//!    `INERT_ATTRIBUTES` (no derive, no attribute macro, no `cfg_attr`),
//!    declared once in its module (not in cfg variants). It is built at least
//!    once, and only by struct literals a body's walk reads (a type alias
//!    names its type), each with no `..base` and the guard field given an
//!    accepted lock call, a helper call, or the name of a shape-1 guard at the
//!    very mention that ends that guard. Its guard field is named nowhere else:
//!    no field access (in its own `drop` neither), no struct pattern, no token
//!    of a macro or a `macro_rules!`, no literal of a type the resolution does
//!    not decide. A tuple struct, whose constructor can be named as a value,
//!    and an enum are never holders. The holder's `Drop::drop` is covered at
//!    its own straight-line sites (outside every closure, async block and
//!    macro that does not run its tokens in place), because the guard field
//!    drops after `drop` returns. A holder covers nothing else: the function
//!    that builds or keeps one holds the lock only by its own guards.
//!
//! Nothing else holds the lock: not `Box::new(..)`, `Some(..)`, a tuple, an
//! array, a crate constructor, an `if` or `match` value, `?`, an `if let`, a
//! `while let`, a match arm, `let Ok(g) = .. else`, a local holding `&` the
//! lock, a closure, an `async` block, a method or an `async` function
//! returning a guard. An acquisition (a `.lock()` whose receiver resolves to
//! the crate lock through parentheses, `&` and `*`, a path call ending in
//! `lock` given it, as `TestEnvLock::lock(&TEST_ENV_LOCK)`, a call of
//! helpers) that no accepted shape keeps is refused and counted (`acquisitions kept by no accepted shape`, and the
//! functions with one), and the real-crate test asserts there is none.
//!
//! Rust that holds the lock at run time but is no listed shape is reported
//! where it touches the environment, by design, not as a defect of the gate:
//! `vec![g]`, `let _ = g`, `match g {..}`, `mem::forget(g)`, `g[0]`, `if let
//! Some(_g) = o`, `let f = acquire; let _g = f();`, `Box::new(lock())`, an
//! `if` whose every branch takes the lock, a borrow of the guard before a
//! read, a closure or async block run while the guard lives, and a test that
//! keeps a holder (`let fix = TestCfg::new();`). Four real tests that kept a
//! `NotifySocketGuard` and read through a call the gate follows take the
//! crate lock with a top-of-body `let` instead.
//!
//! # The indirect rule
//!
//! U21, U22 and U23 (round 4, design W4-D27) carry the direct rule to a read
//! a test reaches through a call, which observes a plant exactly as a read in
//! the test's own body does. Every call and every path named as a value in a
//! body's own code and in the macro tokens it holds is resolved as above and
//! is an edge to each crate body it names: a function; the initializer of a
//! const or static, walked as a body of its own; a method a derive generates
//! (`DERIVED_METHODS`), modelled as calls of the same method of the crate
//! types its fields name (for `default`, the field's own type, and through
//! `DEFAULT_DELEGATING` what a `Box` or an `Arc` holds), serde's
//! `deserialize` also calling what the `#[serde(default = ..)]`,
//! `deserialize_with` and `with` attributes on the type, a variant or a field
//! name and, for a bare `#[serde(default)]`, `default` of the field's type (of
//! the type itself, on the container), and clap's methods calling each of
//! `augment_args` and `augment_subcommands`, derived or written in a
//! hand-written impl, that the crate types a flattened field, a subcommand
//! field or a variant names have (both when a type has both, as the parse
//! does not tell which one clap calls; a type alias followed to its type; a
//! type with neither an unresolved reference with its reason, and one out of
//! the crate a reference out of the crate, never dropped).
//! A call through an external trait's path (`Default::default()`) is
//! narrowed by the type written where it stands (a `let` type, a struct
//! literal's field or base), and is otherwise an edge to every crate impl or
//! derive of that method. A call out of the crate whose value is written as a
//! type naming a crate type (a turbofish, or the type of the `let` it
//! reaches, generic arguments included) is an edge to every body code outside
//! the crate can run on that type: its derived methods and the items of its
//! impls of traits outside the crate.
//!
//! A test-code function item with a reference no accepted shape of its own
//! covers that reaches, over references no accepted shape of their own
//! bodies covers, a body with an environment call nothing covers, is an
//! offender with its chain. A const or static initializer and a derived
//! method are links of a chain, never offenders themselves: the function that
//! reaches them is.
//!
//! An operator (`*x`, `a == b`, `a + b`) and a value going out of scope run a
//! crate impl with no edge from where they run, so every impl of an operator
//! trait or of `Drop` (`OPERATOR_TRAITS`) is checked on its own: one whose
//! item reads the environment without the lock, itself or over the edges,
//! is an operator or drop hazard, and the real-crate test asserts there is
//! none.
//!
//! # Nesting
//!
//! `TEST_ENV_LOCK` wraps a `std::sync::Mutex`, which is not reentrant, so a
//! second acquisition while the lock is held would hang (design W4-D27 part
//! 4). A body takes the lock when it holds an acquisition, or a `.lock()` on
//! the crate lock in its macro tokens. This check fails closed too, which
//! here means reporting more, and it covers every way a guard can be held,
//! not only the accepted shapes (decision D-i8-45):
//!
//! - a shape-1 guard holds the lock from its `let` to the end of its block, in
//!   every region (a closure written there may run while it lives);
//! - so does a confined `let`: `let <name> = <call>;` (plain name) whose call
//!   is a lock call (through `.unwrap()`, `.expect(..)`, `.unwrap_or_else(..)`,
//!   whatever its path resolves to) or a call of a body that can hand the lock
//!   on, its name never mentioned after the `let` or only as the sole argument
//!   of a straight-line `drop(..)` statement with `drop` out of the crate;
//! - every other lock call and call of such a body is an unbounded hold, whose
//!   guard may sit in a struct or tuple literal, an `Option`, a `Box`, a
//!   binding a mention moved it to (`outer = g`), or a value the call
//!   returned: it holds the lock at every later site of its body, except in
//!   another branch of one `if` or `match`, and throughout the outermost loop,
//!   closure, async block or macro invocation around it, its own site
//!   included, since that code can run again while the first guard lives
//!   (`references whose guard's lifetime no confined let bounds`: 19). A call
//!   written as the sole argument of `drop(..)` is dropped where it is made,
//!   and holds nothing;
//! - a body with an unbounded hold, or a `.lock()` in its macro tokens, can
//!   hand a held lock to its caller (by returning it, through a parameter, in
//!   a value it returns), to a fixpoint over the edges (`functions that can
//!   hand a held lock to their caller`: 19, the real crate's four bodies that
//!   build a holder, the two `TestCfg::new` and `NotifySocketGuard::set` and
//!   `unset`, and the tests that keep a `TestCfg` they name later); a body
//!   whose every acquisition is confined releases the lock before it returns,
//!   and is none;
//! - a body with a `.lock()` in its macro tokens, whose site the walk does
//!   not place, holds the lock throughout, and two such, or one beside a lock
//!   call, are a nesting site of the body itself; a holder's `drop` holds it
//!   throughout.
//!
//! At such a site an acquisition, or a reference that reaches over every edge a
//! body that takes the lock (the body itself included, so a function that
//! calls itself, or two that call each other, while a guard lives is caught),
//! is a nesting site; a lock taken after `drop(guard)` in the same block is
//! one too, by design, and the plain fix is an inner block for the first
//! guard. A guard stored in a `static` or a thread-local, or leaked
//! (`mem::forget`, `Box::leak`), outlives every body, and a lock taken through
//! it later is no nesting site the model sees; neither is one whose value a
//! crate method (`receiver.name()`, which never resolves) hands on. The acquisition a guard's own `let` makes runs before the guard's
//! coverage starts, so it is no nesting site of its own. A `Drop` or operator
//! impl that takes the lock, itself or over the edges, is a nesting hazard
//! wherever it runs, a guard live there or not, because no edge leads to it
//! from where a value drops or an operator stands (`operator and drop impls
//! that take the crate lock`); the real-crate test asserts there is none. An
//! acquisition through a local holding `&` the lock or through a function
//! pointer is no acquisition the model sees: such a hang is loud at run time,
//! where the runtime check's per-test timeout reports it.
//!
//! # What a pass does not show
//!
//! Stated so nobody reads more into it. Each item names the figure the
//! real-crate tests print for it; the count after it is that figure on this
//! tree, and `every_test_that_reaches_the_environment_through_a_call_holds_the_crate_lock`
//! asserts that this doc states each one as the run prints it
//! (`documented_figures`), so an edit of `sqry-daemon/src` that moves a figure
//! fails that test until the doc is updated. The figures depend on the tree
//! and the grammar only: the build configuration is read from the manifest's
//! feature table, not from the features or the target of the test build. The reach side resolves calls,
//! macros, derives, serde and clap, but it cannot be complete: a test can
//! reach the environment through a shape below without this gate seeing it,
//! and only the runtime check backs the lib tests it executes.
//!
//! - A method call (`receiver.name()`) never resolves, because the
//!   receiver's type is not in this parse (`method unresolved`: 9878 of the
//!   17536 calls walked, 56%).
//! - Code outside the crate given a crate value is not followed into that
//!   value's impls (`format!` into `Display`, `unwrap_or_default()` into
//!   `Default`, `?` into `From`, `for` into `Iterator`). The impls of traits
//!   outside the crate, other than the operator traits and `Drop`, that reach
//!   an unlocked environment read are listed (`impls of other traits outside
//!   the crate that reach an unlocked environment read`: 1,
//!   `DaemonConfig::default`, through `runtime_dir`).
//! - A call out of the crate whose crate type is not written where its value
//!   lands (inferred, or generic) makes no edge to that type's bodies: of the
//!   4198 calls out of the crate, the 45 at a written crate type are followed
//!   (`path out of the crate at a crate type`: 45) and the other 4153 are not
//!   (`path out of the crate`: 4153); the printed classes, these two among
//!   them, sum to the 17536 calls walked.
//! - A path the model cannot follow is unresolved (`not followed to one
//!   body, unresolved`: 7, each a derived `clone` whose field wraps a crate
//!   type that does not implement `Clone`); an ambiguous path is followed to
//!   every candidate (`ambiguous`: 21).
//! - A binding written in a macro's tokens is not a local (a closure
//!   parameter after `|`, `macro token paths after a |`: 83), so a token that
//!   names one resolves to an item of that name; a local name in a
//!   transcriber resolves where the macro is defined in Rust and is not read
//!   (`macro_rules! definitions`: 0).
//! - `default` of a field whose type is outside the crate and not in
//!   `DEFAULT_DELEGATING` is taken to build no default of the crate type it
//!   wraps, which is what `Option` and the collections do (`defaults not
//!   followed into a wrapped crate type`: 15).
//! - A derive, a serde attribute or a clap attribute inside a `cfg_attr` is
//!   not read (`cfg_attr attributes carrying an attribute the model reads`:
//!   0); serialization (`serialize_with`, `Serialize`) is not modelled.
//! - A cfg the model keeps as unknown (`any(test, feature = "test-hooks")`, a
//!   target) is code some non-test build compiles, so an environment call
//!   under it is not test code here; every cfg variant of an item is a
//!   candidate of its name, and a name with several candidates is an edge to
//!   each.
//! - On the hold side, an attribute macro on a function or a module
//!   (`#[tokio::test]` among them) is not expanded, so one that writes an
//!   item of the lock's name into a body or a module is outside the parse;
//!   a `macro_rules!` or a dependency's function-like macro is not, as
//!   above.
//! - On the hold side, which reports more rather than less, two things are
//!   outside the parse: a value of a holder type made by `unsafe` code with no
//!   struct literal (`transmute`, `ptr::read`), and a `#[macro_use] extern
//!   crate` (with one at a root no macro runs in place: `a #[macro_use] extern
//!   crate`: false).
//!
//! Round 3's disposition, that a transitive reader cannot be bounded because a
//! name-based closure over names such as `load` and `reset` reaches most of
//! the crate, is what the resolution rule replaces: design section 1.4
//! measures that a name-based closure is neither a superset nor a subset of a
//! resolved one, so it is an arbitrary approximation and not a conservative
//! one.
//!
//! Record: `docs/development/surface-parity/06_TEST_EXECUTION-surface-parity.md`.

use std::cell::RefCell;
use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;

use sqry_core::test_support::rust_liveness::{
    self as liveness, BuildCfg, CfgPredicate, FileLiveness, RustSource, Tri,
};
use tree_sitter::{Node, Parser, Tree};

/// The environment accessors, as the last segment of a `std::env` path.
const ENV_ACCESSORS: [&str; 6] = ["set_var", "remove_var", "var", "var_os", "vars", "vars_os"];

/// The name of the crate-wide lock every environment-touching test must hold:
/// the `static` of this name declared at a crate root.
const CRATE_LOCK: &str = "TEST_ENV_LOCK";

fn daemon_src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn collect_rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(dir).expect("read the source directory");
    for entry in entries {
        let entry = entry.expect("directory entry");
        let path = entry.path();
        let file_type = entry.file_type().expect("file type");
        if file_type.is_dir() {
            collect_rust_sources(&path, out);
        } else if file_type.is_file() && path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

fn rust_sources_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect_rust_sources(dir, &mut out);
    out.sort();
    out
}

fn rust_parser() -> Parser {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .expect("the Rust grammar loads");
    parser
}

fn text<'a>(node: Node<'_>, source: &'a [u8]) -> &'a str {
    node.utf8_text(source)
        .expect("tree-sitter spans are UTF-8 boundaries")
}

/// A path's text with whitespace removed, so `std :: env :: var` and
/// `std::env::var` compare equal.
fn compact(node: Node<'_>, source: &[u8]) -> String {
    text(node, source)
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect()
}

/// A name as written, without a raw identifier's `r#`.
fn name_text(node: Node<'_>, source: &[u8]) -> String {
    text(node, source).trim_start_matches("r#").to_string()
}

fn children_of(node: Node<'_>) -> Vec<Node<'_>> {
    let mut cursor = node.walk();
    node.children(&mut cursor).collect()
}

fn named_children_of(node: Node<'_>) -> Vec<Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

fn is_comment(node: Node<'_>) -> bool {
    matches!(node.kind(), "line_comment" | "block_comment")
}

/// The attribute items that directly precede `node` among its siblings.
fn preceding_attributes<'t>(node: Node<'t>) -> Vec<Node<'t>> {
    let mut out = Vec::new();
    let mut current = node.prev_sibling();
    while let Some(sibling) = current {
        match sibling.kind() {
            "attribute_item" => out.push(sibling),
            "line_comment" | "block_comment" => {}
            _ => break,
        }
        current = sibling.prev_sibling();
    }
    out
}

/// Whether an attribute path is a test harness's: `test`, or one ending in
/// `::test` (`tokio::test`).
fn is_test_path(path: &str) -> bool {
    path == "test" || path.ends_with("::test")
}

/// The tokens of an attribute's argument list split at its top-level commas,
/// each part as the source text it spans.
fn attribute_argument_parts(attribute: Node<'_>, source: &[u8]) -> Vec<String> {
    let Some(arguments) = attribute.child_by_field_name("arguments") else {
        return Vec::new();
    };
    let tokens = children_of(arguments);
    let inner = match tokens.len() {
        0..=2 => &[][..],
        length => &tokens[1..length - 1],
    };
    let mut parts = Vec::new();
    let mut start: Option<usize> = None;
    let mut end = 0usize;
    for token in inner.iter().filter(|token| !is_comment(**token)) {
        if text(*token, source) == "," && !token.is_named() {
            if let Some(from) = start.take() {
                parts.push(String::from_utf8_lossy(&source[from..end]).into_owned());
            }
            continue;
        }
        start.get_or_insert(token.start_byte());
        end = token.end_byte();
    }
    if let Some(from) = start {
        parts.push(String::from_utf8_lossy(&source[from..end]).into_owned());
    }
    parts
}

/// Whether a test build runs `function` as a test: an attribute whose path
/// is a test harness's (`#[test]`, `#[tokio::test(..)]`), or a `cfg_attr`
/// whose predicate is not false in a test build and whose attribute list
/// holds one (`#[cfg_attr(test, test)]`). This decides the `#[test]
/// functions` instrument and, with the liveness model, which code is test
/// code (`FileContext::is_test_code`).
fn is_test_function(function: Node<'_>, source: &[u8], test_cfg: &BuildCfg) -> bool {
    preceding_attributes(function)
        .into_iter()
        .any(|attribute_item| {
            let Some(attribute) = attribute_item.named_child(0) else {
                return false;
            };
            let Some(path) = attribute.named_child(0) else {
                return false;
            };
            let path = compact(path, source);
            if is_test_path(&path) {
                return true;
            }
            if path != "cfg_attr" {
                return false;
            }
            let parts = attribute_argument_parts(attribute, source);
            let Some((predicate, attributes)) = parts.split_first() else {
                return false;
            };
            let predicate_holds = CfgPredicate::parse(predicate)
                .is_none_or(|predicate| liveness::evaluate_cfg(&predicate, test_cfg) != Tri::False);
            predicate_holds
                && attributes.iter().any(|attribute| {
                    let path: String = attribute
                        .split('(')
                        .next()
                        .unwrap_or_default()
                        .chars()
                        .filter(|c| !c.is_whitespace())
                        .collect();
                    is_test_path(&path)
                })
        })
}

/// The statement kinds of the grammar's `_declaration_statement` supertype
/// that run as part of a body or declare nothing. Every other kind of that
/// supertype is an item, which is never part of the run-time body of the
/// block that holds it (`GrammarFacts::block_item_kinds`).
const RUNTIME_STATEMENTS: [&str; 5] = [
    "let_declaration",
    "macro_invocation",
    "empty_statement",
    "attribute_item",
    "inner_attribute_item",
];

/// What the grammar says about where a path names a value and what a block
/// holds, read once from `tree_sitter_rust::NODE_TYPES` rather than listed
/// here.
#[derive(Debug, Default)]
struct GrammarFacts {
    /// Per parent node kind, the fields whose declared types include the
    /// `_expression` supertype; the empty name stands for the unnamed
    /// children. A path in one of these slots names a value.
    expression_slots: BTreeMap<String, BTreeSet<String>>,
    /// How many slots that is.
    expression_slot_count: usize,
    /// How many of them also accept the `_pattern` supertype. A path in such
    /// a slot could be a binding as well as a value; the real-crate test
    /// asserts there are none, which is what lets the slot alone decide.
    slots_accepting_a_pattern: usize,
    /// The kinds of `_declaration_statement` that are items.
    block_item_kinds: BTreeSet<String>,
    /// The kinds of `RUNTIME_STATEMENTS` found in `_declaration_statement`.
    runtime_statements_found: BTreeSet<String>,
}

impl GrammarFacts {
    /// Whether `node` sits in an expression slot of its parent.
    fn in_expression_slot(&self, node: Node<'_>) -> bool {
        let Some(parent) = node.parent() else {
            return false;
        };
        let field = field_of_child(parent, node).unwrap_or("");
        self.expression_slots
            .get(parent.kind())
            .is_some_and(|fields| fields.contains(field))
    }
}

/// The field `child` fills in `parent`, if any.
fn field_of_child(parent: Node<'_>, child: Node<'_>) -> Option<&'static str> {
    let mut cursor = parent.walk();
    if !cursor.goto_first_child() {
        return None;
    }
    loop {
        if cursor.node().id() == child.id() {
            return cursor.field_name();
        }
        if !cursor.goto_next_sibling() {
            return None;
        }
    }
}

fn grammar() -> &'static GrammarFacts {
    static FACTS: std::sync::OnceLock<GrammarFacts> = std::sync::OnceLock::new();
    FACTS.get_or_init(|| {
        let document: serde_json::Value =
            serde_json::from_str(tree_sitter_rust::NODE_TYPES).expect("NODE_TYPES parses");
        let mut facts = GrammarFacts::default();
        for entry in document.as_array().into_iter().flatten() {
            let Some(kind) = entry.get("type").and_then(|kind| kind.as_str()) else {
                continue;
            };
            if kind == "_declaration_statement" {
                for subtype in entry
                    .get("subtypes")
                    .and_then(|subtypes| subtypes.as_array())
                    .into_iter()
                    .flatten()
                    .filter_map(|subtype| subtype.get("type").and_then(|t| t.as_str()))
                {
                    if RUNTIME_STATEMENTS.contains(&subtype) {
                        facts.runtime_statements_found.insert(subtype.to_string());
                    } else {
                        facts.block_item_kinds.insert(subtype.to_string());
                    }
                }
            }
            let mut slots: Vec<(String, &serde_json::Value)> = Vec::new();
            if let Some(fields) = entry.get("fields").and_then(|fields| fields.as_object()) {
                for (field, slot) in fields {
                    slots.push((field.clone(), slot));
                }
            }
            if let Some(children) = entry.get("children") {
                slots.push((String::new(), children));
            }
            for (field, slot) in slots {
                let types: BTreeSet<&str> = slot
                    .get("types")
                    .and_then(|types| types.as_array())
                    .into_iter()
                    .flatten()
                    .filter_map(|t| t.get("type").and_then(|t| t.as_str()))
                    .collect();
                if types.contains("_expression") {
                    facts.expression_slot_count += 1;
                    if types.contains("_pattern") {
                        facts.slots_accepting_a_pattern += 1;
                    }
                    facts
                        .expression_slots
                        .entry(kind.to_string())
                        .or_default()
                        .insert(field);
                }
            }
        }
        facts
    })
}

/// The nodes of `body` excluding every item it holds: a nested function, a
/// const or a static (each walked as a body of its own), and every other kind
/// of `GrammarFacts::block_item_kinds`, whose names the resolution reads from
/// the block's scope instead.
fn body_nodes<'t>(body: Node<'t>) -> Vec<Node<'t>> {
    let items = &grammar().block_item_kinds;
    let mut out = Vec::new();
    let mut stack = vec![body];
    while let Some(node) = stack.pop() {
        out.push(node);
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if !items.contains(child.kind()) {
                stack.push(child);
            }
        }
    }
    out
}

/// What a function's liveness is decided against: the non-test build and
/// the test build of the package (`BuildCfg::for_test_build`).
struct FileContext<'a> {
    cfg: &'a BuildCfg,
    test_cfg: &'a BuildCfg,
    /// The file's place in the crate's module tree, in each build.
    class: FileLiveness,
    test_class: FileLiveness,
}

impl FileContext<'_> {
    /// Whether a test build compiles `node`.
    fn in_a_test_build(&self, node: Node<'_>, source: &[u8]) -> bool {
        self.test_class == FileLiveness::Live && liveness::node_is_live(node, source, self.test_cfg)
    }

    /// A node is test code when a test build compiles it and either no
    /// non-test build does or it is in a function a test build runs as a test
    /// (`is_test_function`: `#[test]`, or `#[cfg_attr(test, test)]` on a
    /// function a non-test build compiles as a plain one). A `#[cfg(not(test))]`
    /// statement inside a test is in no test build, so it is not test code.
    fn is_test_code(&self, node: Node<'_>, source: &[u8], in_a_test_function: bool) -> bool {
        let outside_non_test_builds =
            self.class != FileLiveness::Live || !liveness::node_is_live(node, source, self.cfg);
        self.in_a_test_build(node, source) && (outside_non_test_builds || in_a_test_function)
    }
}

/// A path with `.` and `..` components folded, so a `#[path]` value and a
/// directory listing name one file the same way.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Name resolution (the module tree, item scopes, imports, and paths)
// ---------------------------------------------------------------------------

/// A namespace of the resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Ns {
    Value,
    Type,
    Macro,
}

/// One segment of a path, as written.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Seg {
    /// A leading `::`.
    Global,
    Crate,
    /// `self` as a path segment: the enclosing module.
    SelfModule,
    Super,
    /// `Self`: the enclosing `impl`'s type, or the enclosing trait.
    SelfType,
    Name(String),
    /// `<T>` or `<T as Trait>`: the type's path and the trait's.
    Qualified(Vec<Seg>, Option<Vec<Seg>>),
    /// `<dyn Trait>`: the trait's path.
    Dyn(Vec<Seg>),
    /// A segment no parse names: a macro metavariable, or a type that is not a
    /// path (a tuple, a slice, a primitive).
    Opaque,
}

/// What a name resolves to.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Def {
    /// A declaration, by index into the declaration table: a function item, a
    /// const or static whose initializer is walked as a body, or a method a std
    /// derive generates.
    Decl(usize),
    /// A tuple or unit struct's constructor, or an enum variant's, by index
    /// into the type table: no crate code runs.
    Constructor(usize),
    /// A struct, enum, union or type alias, by index into the type table.
    Type(usize),
    Trait(usize),
    Module(usize),
    Macro(usize),
    /// A local binding of the body being walked, by index into its locals.
    Local(usize),
    /// The receiver `self`, named as a value.
    SelfValue,
    /// A generic parameter of an enclosing item.
    Generic,
    /// Outside the crate: the standard library, a dependency, the prelude, or
    /// an extern crate, with the path as resolved.
    External(Vec<String>),
}

/// A resolution: one or more definitions, or the reason an intra-crate path is
/// not followed.
type Lookup = Result<Vec<Def>, &'static str>;

/// What one module or block declares: its items and explicit imports by
/// namespace and name, each with whether it is `pub`, and its glob imports.
#[derive(Debug, Default)]
struct ItemScope {
    named: BTreeMap<(Ns, String), Vec<(Entry, bool)>>,
    globs: Vec<(usize, bool)>,
}

#[derive(Debug, Clone)]
enum Entry {
    Def(Def),
    /// An explicit import, by index into the import table.
    Import(usize),
}

/// One parsed source file of the model.
struct SourceFile<'t> {
    path: PathBuf,
    /// The path relative to the source directory, as the report prints it.
    label: String,
    source: &'t [u8],
    root: Node<'t>,
    /// The file's place in the module tree of a non-test build and of a test
    /// build.
    class: FileLiveness,
    test_class: FileLiveness,
}

/// One module of a crate: a crate root, a file a `mod name;` declaration
/// names, or an inline `mod name { .. }`.
struct ModuleInfo<'t> {
    file: usize,
    /// The crate root module of the crate this module belongs to.
    root: usize,
    parent: Option<usize>,
    /// The node whose named children are the module's items: the file's
    /// `source_file`, or the inline module's `declaration_list`.
    items: Node<'t>,
    /// The `mod` item that declares it (its file and node), `None` for a
    /// crate root.
    declaration: Option<(usize, Node<'t>)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeclKind {
    Function,
    Const,
    Static,
    /// A method a std derive generates (`DERIVED_METHODS`), whose body calls
    /// the same method of every crate type its fields name.
    Derived,
}

/// What a declaration belongs to, read from the node that holds it directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Owner {
    /// A module or a block: a free item, in scope where that container is.
    Free,
    /// An `impl` block, by index into the impl table.
    Impl(usize),
    /// A trait's default, by index into the trait table.
    Trait(usize),
    /// A std derive of a type, by index into the type table.
    Derived(usize),
}

/// A function item, a const or static with an initializer, or a method a std
/// derive generates: every body the walk reads.
struct Decl<'t> {
    file: usize,
    node: Node<'t>,
    name: String,
    kind: DeclKind,
    owner: Owner,
    /// The static declared at a crate root (directly in a root file's
    /// `source_file`) under the crate lock's name.
    crate_lock: bool,
    /// What the report names before `name`: the `impl` type, the trait, or the
    /// functions a nested item sits in.
    owner_label: Option<String>,
}

enum TypeShape<'t> {
    /// A struct; `constructor` when it is a tuple or unit struct, whose name is
    /// a value too.
    Struct {
        constructor: bool,
    },
    Enum(BTreeSet<String>),
    Union,
    /// A type alias and the type it names.
    Alias(Option<Node<'t>>),
}

struct TypeInfo<'t> {
    file: usize,
    node: Node<'t>,
    name: String,
    shape: TypeShape<'t>,
    /// The traits its `#[derive(..)]` attributes name, by last segment.
    derives: BTreeSet<String>,
    /// Each method a derive generates for it (`DERIVED_METHODS`): the trait
    /// the method belongs to, and the declaration standing for the generated
    /// body.
    derived: BTreeMap<String, (String, usize)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TraitRef {
    Crate(usize),
    External(Vec<String>),
    Unresolved,
}

struct TraitInfo<'t> {
    node: Node<'t>,
    /// The declarations the trait itself provides a body for, by name.
    members: BTreeMap<String, Vec<usize>>,
}

struct ImplInfo<'t> {
    file: usize,
    node: Node<'t>,
    /// The crate types the impl is for: one, each cfg variant of it, or none
    /// when the resolution names no crate type. An impl written on a type
    /// alias is for the type the alias names.
    self_types: Vec<usize>,
    /// A blanket impl (`impl<T: Bound> Trait for T`): for every type, its
    /// bounds not read.
    blanket: bool,
    /// The last segment of the type's path, without its generic arguments,
    /// as the report prints it.
    self_label: String,
    trait_ref: Option<TraitRef>,
    members: BTreeMap<String, Vec<usize>>,
}

struct MacroInfo<'t> {
    file: usize,
    node: Node<'t>,
    /// It carries `#[macro_export]`, so its crate root holds it by path.
    exported: bool,
}

/// One name a `use` declaration binds, or one glob it imports.
struct ImportInfo<'t> {
    file: usize,
    node: Node<'t>,
    /// The module the declaration is in.
    module: usize,
    /// The name bound, or `None` for a glob.
    name: Option<String>,
    path: Vec<Seg>,
}

/// One step of a path walk: what the prefix resolved to.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Cursor {
    Module(usize),
    Type(usize),
    Trait(usize),
    /// `<T as Trait>` with the trait, or `<T>` without.
    Qualified(usize, Option<TraitRef>),
    External(Vec<String>),
}

/// Where a lexical lookup starts: a node of a file, the locals of the body
/// being walked, and that body (the locals are visible inside it only).
struct Site<'a, 't> {
    file: usize,
    node: Node<'t>,
    locals: &'a [(String, usize, usize)],
    body: Option<Node<'t>>,
}

/// The innermost scope a lexical lookup has found so far: its key (where the
/// scope ends, where it starts, reversed, and 0 for an item before 1 for a
/// local, the least key being the innermost) and what it binds the name to.
type Innermost = ((usize, Reverse<usize>, u8), Vec<Def>);

/// How deep a chain of glob imports or type aliases is followed.
const RESOLUTION_DEPTH: usize = 16;

/// The methods derives generate, by the derive's name, the trait the method
/// belongs to, and the method. Each is modelled as a body
/// (`DeclKind::Derived`, `derived_calls`):
///
/// - a std derive's method calls the same method of every crate type the
///   fields name (for `default`, of the field's own type and, through
///   `DEFAULT_DELEGATING`, of what it wraps);
/// - serde's `deserialize` does too, and calls what the type's `#[serde(..)]`
///   attributes name (`serde_calls`);
/// - clap's `Parser`, `Args` and `Subcommand` methods build the command, so
///   each reads the environment when a field of the type carries a clap `env`
///   attribute, and calls `augment_args` or `augment_subcommands` of every
///   crate type a flattened field, a subcommand field or a tuple variant
///   names (`clap_calls`). A derive named `Parser`, `Args` or `Subcommand` is
///   taken as clap's.
///
/// A path to a trait method the derive does not generate (`ne`, `lt`, `max`)
/// is unresolved.
const DERIVED_METHODS: [(&str, &str, &str); 22] = [
    ("Default", "Default", "default"),
    ("Clone", "Clone", "clone"),
    ("PartialEq", "PartialEq", "eq"),
    ("PartialOrd", "PartialOrd", "partial_cmp"),
    ("Ord", "Ord", "cmp"),
    ("Hash", "Hash", "hash"),
    ("Debug", "Debug", "fmt"),
    ("Deserialize", "Deserialize", "deserialize"),
    ("Parser", "Parser", "parse"),
    ("Parser", "Parser", "try_parse"),
    ("Parser", "Parser", "parse_from"),
    ("Parser", "Parser", "try_parse_from"),
    ("Parser", "Parser", "update_from"),
    ("Parser", "Parser", "try_update_from"),
    ("Parser", "CommandFactory", "command"),
    ("Parser", "CommandFactory", "command_for_update"),
    ("Parser", "Args", "augment_args"),
    ("Parser", "Args", "augment_args_for_update"),
    ("Args", "Args", "augment_args"),
    ("Args", "Args", "augment_args_for_update"),
    ("Subcommand", "Subcommand", "augment_subcommands"),
    ("Subcommand", "Subcommand", "augment_subcommands_for_update"),
];

/// The derives clap's model covers (`clap_calls`).
const CLAP_DERIVES: [&str; 3] = ["Parser", "Args", "Subcommand"];

/// The std types whose `Default` builds their type parameter's default (a
/// `Box<T>` holds a `T::default()`), so a `default` of a field of one of them
/// calls `default` of the crate types it wraps; every other type outside the
/// crate (`Option<T>`, `Vec<T>`) is taken to hold no default of its
/// parameter, which is what `Option` and the collections do.
const DEFAULT_DELEGATING: [&str; 10] = [
    "Box",
    "Rc",
    "Arc",
    "Cell",
    "RefCell",
    "Mutex",
    "RwLock",
    "ManuallyDrop",
    "Wrapping",
    "Reverse",
];

/// The traits an item's `#[derive(..)]` attributes name, by last segment. A
/// derive inside a `cfg_attr` is not read.
fn derived_traits(item: Node<'_>, source: &[u8]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for attribute_item in preceding_attributes(item) {
        let Some(attribute) = attribute_item.named_child(0) else {
            continue;
        };
        if attribute
            .named_child(0)
            .is_none_or(|path| compact(path, source) != "derive")
        {
            continue;
        }
        let Some(arguments) = attribute.child_by_field_name("arguments") else {
            continue;
        };
        let tokens = children_of(arguments);
        let mut index = 0;
        while index < tokens.len() {
            match token_path(&tokens, index, source) {
                Some((segs, end)) => {
                    if let Some(Seg::Name(name)) = segs.last() {
                        out.insert(name.clone());
                    }
                    index = end.max(index + 1);
                }
                None => index += 1,
            }
        }
    }
    out
}

/// The model of the crates the roots define: the module tree, every item and
/// import, and the resolution over them.
struct CrateModel<'t> {
    files: Vec<SourceFile<'t>>,
    modules: Vec<ModuleInfo<'t>>,
    module_by_items: BTreeMap<(usize, usize), usize>,
    module_by_declaration: BTreeMap<(usize, usize), usize>,
    decls: Vec<Decl<'t>>,
    decl_by_node: BTreeMap<(usize, usize), usize>,
    types: Vec<TypeInfo<'t>>,
    type_by_node: BTreeMap<(usize, usize), usize>,
    traits: Vec<TraitInfo<'t>>,
    trait_by_node: BTreeMap<(usize, usize), usize>,
    impls: Vec<ImplInfo<'t>>,
    impl_by_node: BTreeMap<(usize, usize), usize>,
    impls_of_type: BTreeMap<usize, Vec<usize>>,
    impls_of_trait: BTreeMap<usize, Vec<usize>>,
    macros: Vec<MacroInfo<'t>>,
    macro_by_node: BTreeMap<(usize, usize), usize>,
    imports: Vec<ImportInfo<'t>>,
    imports_by_use: BTreeMap<(usize, usize), Vec<usize>>,
    scopes: RefCell<BTreeMap<(usize, usize), Rc<ItemScope>>>,
    import_memo: RefCell<BTreeMap<(usize, Ns), Vec<Def>>>,
    import_busy: RefCell<BTreeSet<(usize, Ns)>>,
    /// The `#[macro_export]` macros of each crate, by its root module: the
    /// root holds them by path (`crate::name!`).
    exported_macros: BTreeMap<usize, Vec<usize>>,
    /// The names an `extern crate` at a crate root binds, by root module and
    /// name: the extern prelude every module of that crate sees. `extern
    /// crate self as name` binds the root module itself.
    extern_prelude: BTreeMap<(usize, String), Def>,
    /// How many module or block scopes the lookups visited, so a lookup whose
    /// glob imports fan out is visible (each lookup visits a scope once).
    scope_visits: std::cell::Cell<usize>,
    /// The `default` of a field whose type is outside the crate and holds no
    /// default of its parameter, although the parameter names a crate type
    /// (`default_calls`): not followed, counted.
    defaults_not_followed: std::cell::Cell<usize>,
    /// Sites the module tree does not model, named by file.
    unmodelled: Vec<String>,
    /// Set by a lookup that consulted a scope holding an invocation that may
    /// expand to an item (`expands_items`) without finding the name among the
    /// scope's own items and explicit imports: a macro-made item there would
    /// shadow what the lookup found, so the answer is not decided by the
    /// parse. Read by the hold side only (`bare_lock_call`), which fails
    /// closed on it.
    undecided: std::cell::Cell<bool>,
    /// `undecided` for each memoized import, so a memo hit carries it.
    import_undecided: RefCell<BTreeMap<(usize, Ns), bool>>,
    /// `expands_items`, memoized per scope.
    item_expanding: RefCell<BTreeMap<(usize, usize), bool>>,
    /// Set while `expands_items` resolves macro names, so its own lookups do
    /// not consult it again.
    deciding: std::cell::Cell<bool>,
    /// A crate root holds `#[macro_use] extern crate`, memoized
    /// (`has_macro_use_extern_crate`).
    macro_use_extern_crate: std::cell::OnceCell<bool>,
}

/// The string value of a `#[path = ".."]` attribute item.
fn path_attribute(attribute_item: Node<'_>, source: &[u8]) -> Option<String> {
    let attribute = attribute_item.named_child(0)?;
    let path = attribute.named_child(0)?;
    if compact(path, source) != "path" {
        return None;
    }
    let value = attribute.child_by_field_name("value")?;
    let raw = text(value, source);
    Some(raw.trim_matches('"').to_string())
}

/// Whether an attribute item's path is `path` (`#[macro_export]`,
/// `#[macro_use]`).
fn attribute_path_is(attribute_item: Node<'_>, source: &[u8], path: &str) -> bool {
    attribute_item
        .named_child(0)
        .and_then(|attribute| attribute.named_child(0))
        .is_some_and(|written| compact(written, source) == path)
}

/// Whether an item carries a visibility modifier (`pub`, `pub(crate)`, ...).
fn is_public(item: Node<'_>) -> bool {
    named_children_of(item)
        .iter()
        .any(|child| child.kind() == "visibility_modifier")
}

/// The segments of a path node: an expression path, a type path, a use path,
/// or a macro name.
fn path_segs(node: Node<'_>, source: &[u8], out: &mut Vec<Seg>) {
    match node.kind() {
        "identifier" | "type_identifier" => {
            let name = name_text(node, source);
            out.push(if name == "Self" {
                Seg::SelfType
            } else {
                Seg::Name(name)
            });
        }
        "crate" => out.push(Seg::Crate),
        "self" => out.push(Seg::SelfModule),
        "super" => out.push(Seg::Super),
        "metavariable" => out.push(if text(node, source) == "$crate" {
            Seg::Crate
        } else {
            Seg::Opaque
        }),
        "scoped_identifier" | "scoped_type_identifier" => {
            match node.child_by_field_name("path") {
                Some(path) => path_segs(path, source, out),
                None => out.push(Seg::Global),
            }
            match node.child_by_field_name("name") {
                Some(name) => path_segs(name, source, out),
                None => out.push(Seg::Opaque),
            }
        }
        "generic_type" | "generic_type_with_turbofish" => match node.child_by_field_name("type") {
            Some(inner) => path_segs(inner, source, out),
            None => out.push(Seg::Opaque),
        },
        "generic_function" => match node.child_by_field_name("function") {
            Some(inner) => path_segs(inner, source, out),
            None => out.push(Seg::Opaque),
        },
        "bracketed_type" => {
            let inner = named_children_of(node)
                .into_iter()
                .find(|c| !is_comment(*c));
            match inner {
                Some(dynamic) if dynamic.kind() == "dynamic_type" => {
                    let mut tr = Vec::new();
                    match dynamic.child_by_field_name("trait") {
                        Some(t) => type_segs(t, source, &mut tr),
                        None => tr.push(Seg::Opaque),
                    }
                    out.push(Seg::Dyn(tr));
                }
                Some(qualified) if qualified.kind() == "qualified_type" => {
                    let mut ty = Vec::new();
                    if let Some(t) = qualified.child_by_field_name("type") {
                        type_segs(t, source, &mut ty);
                    }
                    let mut tr = Vec::new();
                    if let Some(t) = qualified.child_by_field_name("alias") {
                        type_segs(t, source, &mut tr);
                    }
                    out.push(Seg::Qualified(ty, Some(tr)));
                }
                Some(ty_node) => {
                    let mut ty = Vec::new();
                    type_segs(ty_node, source, &mut ty);
                    out.push(Seg::Qualified(ty, None));
                }
                None => out.push(Seg::Opaque),
            }
        }
        _ => out.push(Seg::Opaque),
    }
}

/// The segments of a type node; a reference names the type it refers to.
fn type_segs(node: Node<'_>, source: &[u8], out: &mut Vec<Seg>) {
    match node.kind() {
        "reference_type" | "pointer_type" => match node.child_by_field_name("type") {
            Some(inner) => type_segs(inner, source, out),
            None => out.push(Seg::Opaque),
        },
        _ => path_segs(node, source, out),
    }
}

fn segs_of(node: Node<'_>, source: &[u8]) -> Vec<Seg> {
    let mut out = Vec::new();
    path_segs(node, source, &mut out);
    out
}

/// Every name a use tree binds, with its path; a glob binds `None`.
fn flatten_use(
    tree: Node<'_>,
    source: &[u8],
    prefix: &[Seg],
    out: &mut Vec<(Option<String>, Vec<Seg>)>,
) {
    match tree.kind() {
        "use_as_clause" => {
            let mut path = prefix.to_vec();
            if let Some(inner) = tree.child_by_field_name("path") {
                path_segs(inner, source, &mut path);
            }
            let alias = tree
                .child_by_field_name("alias")
                .map(|alias| name_text(alias, source));
            // `use Trait as _;` binds no name.
            if let Some(alias) = alias.filter(|alias| alias != "_") {
                if path.last() == Some(&Seg::SelfModule) {
                    path.pop();
                }
                out.push((Some(alias), path));
            }
        }
        "use_list" => {
            for child in named_children_of(tree) {
                if !is_comment(child) {
                    flatten_use(child, source, prefix, out);
                }
            }
        }
        "scoped_use_list" => {
            let mut path = prefix.to_vec();
            if let Some(inner) = tree.child_by_field_name("path") {
                path_segs(inner, source, &mut path);
            }
            if let Some(list) = tree.child_by_field_name("list") {
                flatten_use(list, source, &path, out);
            }
        }
        "use_wildcard" => {
            let mut path = prefix.to_vec();
            if let Some(inner) = named_children_of(tree)
                .into_iter()
                .find(|c| !is_comment(*c))
            {
                path_segs(inner, source, &mut path);
            }
            out.push((None, path));
        }
        _ => {
            let mut path = prefix.to_vec();
            path_segs(tree, source, &mut path);
            // `use a::{self}` binds `a`.
            if path.last() == Some(&Seg::SelfModule) && path.len() > 1 {
                path.pop();
            }
            let name = match path.last() {
                Some(Seg::Name(name)) => Some(name.clone()),
                Some(Seg::Crate) => None,
                _ => None,
            };
            if let Some(name) = name {
                out.push((Some(name), path));
            }
        }
    }
}

/// Whether `path` is one of the environment accessors of `std::env`.
fn is_env_path(path: &[String]) -> bool {
    matches!(path, [std, env, accessor]
        if std == "std" && env == "env" && ENV_ACCESSORS.contains(&accessor.as_str()))
}

/// Whether a written path ends in `env::<accessor>`, the textual form the
/// resolution falls back to when the path does not resolve to a crate item.
fn written_env_path(written: &str) -> bool {
    let segments: Vec<&str> = written.split("::").collect();
    matches!(segments.as_slice(), [.., "env", accessor] if ENV_ACCESSORS.contains(accessor))
}

impl<'t> CrateModel<'t> {
    fn build(files: Vec<SourceFile<'t>>, roots: &[usize]) -> Self {
        let mut model = CrateModel {
            files,
            modules: Vec::new(),
            module_by_items: BTreeMap::new(),
            module_by_declaration: BTreeMap::new(),
            decls: Vec::new(),
            decl_by_node: BTreeMap::new(),
            types: Vec::new(),
            type_by_node: BTreeMap::new(),
            traits: Vec::new(),
            trait_by_node: BTreeMap::new(),
            impls: Vec::new(),
            impl_by_node: BTreeMap::new(),
            impls_of_type: BTreeMap::new(),
            impls_of_trait: BTreeMap::new(),
            macros: Vec::new(),
            macro_by_node: BTreeMap::new(),
            imports: Vec::new(),
            imports_by_use: BTreeMap::new(),
            scopes: RefCell::new(BTreeMap::new()),
            import_memo: RefCell::new(BTreeMap::new()),
            import_busy: RefCell::new(BTreeSet::new()),
            exported_macros: BTreeMap::new(),
            extern_prelude: BTreeMap::new(),
            scope_visits: std::cell::Cell::new(0),
            defaults_not_followed: std::cell::Cell::new(0),
            unmodelled: Vec::new(),
            undecided: std::cell::Cell::new(false),
            import_undecided: RefCell::new(BTreeMap::new()),
            item_expanding: RefCell::new(BTreeMap::new()),
            deciding: std::cell::Cell::new(false),
            macro_use_extern_crate: std::cell::OnceCell::new(),
        };
        model.build_module_tree(roots);
        let modelled: BTreeSet<usize> = model.modules.iter().map(|module| module.file).collect();
        for (position, file) in model.files.iter().enumerate() {
            if file.class != FileLiveness::Unreachable && !modelled.contains(&position) {
                model.unmodelled.push(format!(
                    "{}: a file the liveness model reaches and the module tree does not",
                    file.label
                ));
            }
        }
        let files: Vec<usize> = modelled.into_iter().collect();
        for &file in &files {
            model.collect_items(file);
        }
        for &file in &files {
            model.collect_decls(file);
        }
        model.collect_macro_scopes();
        model.collect_derived();
        model.resolve_impls();
        model
    }

    /// The `#[macro_export]` macros of each crate and each crate's extern
    /// prelude (the `extern crate` items at its root).
    fn collect_macro_scopes(&mut self) {
        for (index, info) in self.macros.iter().enumerate() {
            if info.exported {
                let root = self.modules[self.module_at(info.file, info.node)].root;
                self.exported_macros.entry(root).or_default().push(index);
            }
        }
        for module in 0..self.modules.len() {
            if self.modules[module].parent.is_some() {
                continue;
            }
            let info = &self.modules[module];
            let source = self.files[info.file].source;
            for item in named_children_of(info.items) {
                if item.kind() != "extern_crate_declaration" {
                    continue;
                }
                let Some(crate_name) = item
                    .child_by_field_name("name")
                    .map(|name| name_text(name, source))
                else {
                    continue;
                };
                let bound = item
                    .child_by_field_name("alias")
                    .map(|alias| name_text(alias, source))
                    .unwrap_or_else(|| crate_name.clone());
                let def = if crate_name == "self" {
                    Def::Module(module)
                } else {
                    Def::External(vec![crate_name])
                };
                self.extern_prelude.insert((module, bound), def);
            }
        }
    }

    /// The module tree from every root, with rustc's file rules: the base
    /// directory of a declaring file is its own directory when it is a root or
    /// is named `mod.rs`, and the directory named after its stem otherwise;
    /// enclosing inline module names are appended; `name.rs` and
    /// `name/mod.rs` are the candidates and exactly one must be among the
    /// files; a `#[path = "p"]` outside every inline module resolves against
    /// the declaring file's directory.
    fn build_module_tree(&mut self, roots: &[usize]) {
        let index: BTreeMap<PathBuf, usize> = self
            .files
            .iter()
            .enumerate()
            .map(|(position, file)| (file.path.clone(), position))
            .collect();
        let mut owner: BTreeMap<usize, usize> = BTreeMap::new();
        let mut queue: VecDeque<usize> = VecDeque::new();
        for &root in roots {
            let id = self.modules.len();
            self.modules.push(ModuleInfo {
                file: root,
                root: id,
                parent: None,
                items: self.files[root].root,
                declaration: None,
            });
            self.module_by_items
                .insert((root, self.files[root].root.id()), id);
            owner.insert(root, id);
            queue.push_back(id);
        }
        while let Some(module) = queue.pop_front() {
            let file = self.modules[module].file;
            let source = self.files[file].source;
            let mut stack: Vec<Node<'t>> = children_of(self.modules[module].items);
            while let Some(node) = stack.pop() {
                if node.kind() != "mod_item" {
                    stack.extend(children_of(node));
                    continue;
                }
                let name = node
                    .child_by_field_name("name")
                    .map(|name| name_text(name, source))
                    .unwrap_or_default();
                let root = self.modules[module].root;
                if let Some(body) = node.child_by_field_name("body") {
                    let id = self.modules.len();
                    self.modules.push(ModuleInfo {
                        file,
                        root,
                        parent: Some(module),
                        items: body,
                        declaration: Some((file, node)),
                    });
                    self.module_by_items.insert((file, body.id()), id);
                    self.module_by_declaration.insert((file, node.id()), id);
                    queue.push_back(id);
                    continue;
                }
                match self.declared_file(module, node, &name, &index) {
                    Ok(target) => {
                        if let Some(previous) = owner.get(&target) {
                            self.unmodelled.push(format!(
                                "{}: reached by two module declarations (modules {previous} and {module})",
                                self.files[target].label
                            ));
                            continue;
                        }
                        let id = self.modules.len();
                        self.modules.push(ModuleInfo {
                            file: target,
                            root,
                            parent: Some(module),
                            items: self.files[target].root,
                            declaration: Some((file, node)),
                        });
                        self.module_by_items
                            .insert((target, self.files[target].root.id()), id);
                        self.module_by_declaration.insert((file, node.id()), id);
                        owner.insert(target, id);
                        queue.push_back(id);
                    }
                    Err(reason) => self
                        .unmodelled
                        .push(format!("{}: {reason}", self.files[file].label)),
                }
            }
        }
    }

    /// The file a `mod name;` declaration in `module` names.
    fn declared_file(
        &self,
        module: usize,
        declaration: Node<'t>,
        name: &str,
        index: &BTreeMap<PathBuf, usize>,
    ) -> Result<usize, String> {
        let file = self.modules[module].file;
        let source = self.files[file].source;
        let path = &self.files[file].path;
        // The inline modules between the declaration and its file's root.
        let mut inline = Vec::new();
        let mut current = Some(module);
        while let Some(here) = current {
            if self.modules[here].items.kind() == "source_file" {
                break;
            }
            let declared = self.modules[here].items.parent();
            inline.push(
                declared
                    .and_then(|item| item.child_by_field_name("name"))
                    .map(|name| name_text(name, source))
                    .unwrap_or_default(),
            );
            current = self.modules[here].parent;
        }
        inline.reverse();
        let directory = path.parent().unwrap_or_else(|| Path::new(""));
        let explicit = preceding_attributes(declaration)
            .into_iter()
            .find_map(|attribute| path_attribute(attribute, source));
        if let Some(explicit) = explicit {
            if !inline.is_empty() {
                return Err(format!(
                    "a #[path] inside an inline module on `mod {name};`"
                ));
            }
            let target = normalize(&directory.join(explicit));
            return index
                .get(&target)
                .copied()
                .ok_or_else(|| format!("the #[path] of `mod {name};` names no parsed file"));
        }
        let is_root = self
            .modules
            .iter()
            .any(|m| m.parent.is_none() && m.file == file);
        let is_mod_rs = path.file_name().is_some_and(|n| n == "mod.rs");
        let mut base = if is_root || is_mod_rs {
            directory.to_path_buf()
        } else {
            directory.join(path.file_stem().map(PathBuf::from).unwrap_or_default())
        };
        for segment in &inline {
            base.push(segment);
        }
        let flat = normalize(&base.join(format!("{name}.rs")));
        let nested = normalize(&base.join(name).join("mod.rs"));
        match (index.get(&flat), index.get(&nested)) {
            (Some(found), None) | (None, Some(found)) => Ok(*found),
            (Some(_), Some(_)) => Err(format!("`mod {name};` has two candidate files")),
            (None, None) => Err(format!("`mod {name};` has no candidate file")),
        }
    }

    /// The module whose items hold `node`.
    fn module_at(&self, file: usize, node: Node<'t>) -> usize {
        let mut current = Some(node);
        while let Some(here) = current {
            if let Some(module) = self.module_by_items.get(&(file, here.id())) {
                return *module;
            }
            current = here.parent();
        }
        self.module_by_items[&(file, self.files[file].root.id())]
    }

    /// Whether `module` is `ancestor` or nested in it.
    fn within(&self, module: usize, ancestor: usize) -> bool {
        let mut current = Some(module);
        while let Some(here) = current {
            if here == ancestor {
                return true;
            }
            current = self.modules[here].parent;
        }
        false
    }

    /// Types, traits, impls, macros and imports of one file.
    fn collect_items(&mut self, file: usize) {
        let source = self.files[file].source;
        let mut stack = vec![self.files[file].root];
        while let Some(node) = stack.pop() {
            stack.extend(children_of(node));
            let key = (file, node.id());
            let name = node
                .child_by_field_name("name")
                .map(|name| name_text(name, source))
                .unwrap_or_default();
            match node.kind() {
                "struct_item" | "enum_item" | "union_item" | "type_item" => {
                    let shape = match node.kind() {
                        "struct_item" => TypeShape::Struct {
                            constructor: node
                                .child_by_field_name("body")
                                .is_none_or(|body| body.kind() == "ordered_field_declaration_list"),
                        },
                        "enum_item" => TypeShape::Enum(
                            node.child_by_field_name("body")
                                .map(|body| {
                                    named_children_of(body)
                                        .into_iter()
                                        .filter(|v| v.kind() == "enum_variant")
                                        .filter_map(|v| v.child_by_field_name("name"))
                                        .map(|v| name_text(v, source))
                                        .collect()
                                })
                                .unwrap_or_default(),
                        ),
                        "union_item" => TypeShape::Union,
                        _ => TypeShape::Alias(node.child_by_field_name("type")),
                    };
                    self.type_by_node.insert(key, self.types.len());
                    self.types.push(TypeInfo {
                        file,
                        node,
                        name,
                        shape,
                        derives: derived_traits(node, source),
                        derived: BTreeMap::new(),
                    });
                }
                "trait_item" => {
                    self.trait_by_node.insert(key, self.traits.len());
                    self.traits.push(TraitInfo {
                        node,
                        members: BTreeMap::new(),
                    });
                }
                "impl_item" => {
                    let self_label = node
                        .child_by_field_name("type")
                        .map(|ty| type_label(ty, source))
                        .unwrap_or_default();
                    self.impl_by_node.insert(key, self.impls.len());
                    self.impls.push(ImplInfo {
                        file,
                        node,
                        self_types: Vec::new(),
                        blanket: false,
                        self_label,
                        trait_ref: None,
                        members: BTreeMap::new(),
                    });
                }
                "macro_definition" => {
                    let exported = preceding_attributes(node)
                        .into_iter()
                        .any(|attribute| attribute_path_is(attribute, source, "macro_export"));
                    self.macro_by_node.insert(key, self.macros.len());
                    self.macros.push(MacroInfo {
                        file,
                        node,
                        exported,
                    });
                }
                "use_declaration" => {
                    let module = self.module_at(file, node);
                    let mut bound = Vec::new();
                    if let Some(argument) = node.child_by_field_name("argument") {
                        flatten_use(argument, source, &[], &mut bound);
                    }
                    let mut indices = Vec::new();
                    for (name, path) in bound {
                        indices.push(self.imports.len());
                        self.imports.push(ImportInfo {
                            file,
                            node,
                            module,
                            name,
                            path,
                        });
                    }
                    self.imports_by_use.insert(key, indices);
                }
                _ => {}
            }
        }
    }

    /// Function items, and consts and statics with an initializer, of one
    /// file, each with the owner its direct container names.
    fn collect_decls(&mut self, file: usize) {
        let source = self.files[file].source;
        let root = self.files[file].root;
        let mut stack = vec![root];
        let mut found = Vec::new();
        while let Some(node) = stack.pop() {
            stack.extend(children_of(node));
            let kind = match node.kind() {
                "function_item" => DeclKind::Function,
                "const_item" if node.child_by_field_name("value").is_some() => DeclKind::Const,
                "static_item" if node.child_by_field_name("value").is_some() => DeclKind::Static,
                _ => continue,
            };
            found.push((node, kind));
        }
        found.sort_by_key(|(node, _)| node.start_byte());
        for (node, kind) in found {
            let name = node
                .child_by_field_name("name")
                .map(|name| name_text(name, source))
                .unwrap_or_default();
            let container = node.parent();
            let holder = container
                .filter(|list| list.kind() == "declaration_list")
                .and_then(|list| list.parent());
            let owner = match holder.map(|holder| (holder.kind(), holder)) {
                Some(("impl_item", holder)) => self
                    .impl_by_node
                    .get(&(file, holder.id()))
                    .map_or(Owner::Free, |i| Owner::Impl(*i)),
                Some(("trait_item", holder)) => self
                    .trait_by_node
                    .get(&(file, holder.id()))
                    .map_or(Owner::Free, |t| Owner::Trait(*t)),
                _ => Owner::Free,
            };
            let owner_label = match owner {
                Owner::Impl(index) => Some(self.impls[index].self_label.clone()),
                Owner::Trait(index) => self.traits[index]
                    .node
                    .child_by_field_name("name")
                    .map(|name| name_text(name, source)),
                Owner::Free | Owner::Derived(_) => enclosing_functions(node, source),
            };
            let crate_lock = kind == DeclKind::Static
                && name == CRATE_LOCK
                && container.is_some_and(|c| c.kind() == "source_file")
                && self
                    .modules
                    .iter()
                    .any(|module| module.parent.is_none() && module.file == file);
            let index = self.decls.len();
            self.decl_by_node.insert((file, node.id()), index);
            match owner {
                Owner::Impl(i) => self.impls[i]
                    .members
                    .entry(name.clone())
                    .or_default()
                    .push(index),
                Owner::Trait(t) => self.traits[t]
                    .members
                    .entry(name.clone())
                    .or_default()
                    .push(index),
                Owner::Free | Owner::Derived(_) => {}
            }
            self.decls.push(Decl {
                file,
                node,
                name,
                kind,
                owner,
                crate_lock,
                owner_label,
            });
        }
    }

    /// A declaration for each method a derive generates (`DERIVED_METHODS`).
    fn collect_derived(&mut self) {
        for t in 0..self.types.len() {
            for (derive, trait_name, method) in DERIVED_METHODS {
                if !self.types[t].derives.contains(derive)
                    || self.types[t].derived.contains_key(method)
                {
                    continue;
                }
                let index = self.decls.len();
                self.decls.push(Decl {
                    file: self.types[t].file,
                    node: self.types[t].node,
                    name: method.to_string(),
                    kind: DeclKind::Derived,
                    owner: Owner::Derived(t),
                    crate_lock: false,
                    owner_label: Some(self.types[t].name.clone()),
                });
                self.types[t]
                    .derived
                    .insert(method.to_string(), (trait_name.to_string(), index));
            }
        }
    }

    /// Each impl's type and trait, resolved where the impl stands.
    fn resolve_impls(&mut self) {
        for index in 0..self.impls.len() {
            let file = self.impls[index].file;
            let node = self.impls[index].node;
            let source = self.files[file].source;
            let site = Site {
                file,
                node,
                locals: &[],
                body: None,
            };
            let mut self_segs = Vec::new();
            if let Some(ty) = node.child_by_field_name("type") {
                type_segs(ty, source, &mut self_segs);
            }
            let blanket = matches!(self_segs.as_slice(), [Seg::Name(name)]
                if declares_generic(node, name, Ns::Type, source));
            let mut self_types: Vec<usize> = Vec::new();
            if !blanket {
                for def in self
                    .resolve_path(&self_segs, Ns::Type, &site)
                    .unwrap_or_default()
                {
                    if let Def::Type(t) = def {
                        for aliased in self.alias_targets(t, 0) {
                            if !self_types.contains(&aliased) {
                                self_types.push(aliased);
                            }
                        }
                    }
                }
            }
            let trait_ref = node.child_by_field_name("trait").map(|tr| {
                let mut segs = Vec::new();
                type_segs(tr, source, &mut segs);
                match self.resolve_path(&segs, Ns::Type, &site) {
                    Ok(defs) => match defs.as_slice() {
                        [Def::Trait(k)] => TraitRef::Crate(*k),
                        [Def::External(path)] => TraitRef::External(path.clone()),
                        _ => TraitRef::Unresolved,
                    },
                    Err(_) => TraitRef::Unresolved,
                }
            });
            for t in &self_types {
                self.impls_of_type.entry(*t).or_default().push(index);
            }
            if let Some(TraitRef::Crate(k)) = &trait_ref {
                self.impls_of_trait.entry(*k).or_default().push(index);
            }
            self.impls[index].self_types = self_types;
            self.impls[index].blanket = blanket;
            self.impls[index].trait_ref = trait_ref;
        }
    }

    /// The crate types a type names through its aliases: itself when it is
    /// not an alias, else what the alias's type resolves to where the alias
    /// stands, followed `RESOLUTION_DEPTH` deep (an alias of a type outside
    /// the crate names no crate type).
    fn alias_targets(&self, t: usize, depth: usize) -> Vec<usize> {
        let info = &self.types[t];
        let TypeShape::Alias(target) = &info.shape else {
            return vec![t];
        };
        let Some(target) = target else {
            return Vec::new();
        };
        if depth >= RESOLUTION_DEPTH {
            return Vec::new();
        }
        let site = Site {
            file: info.file,
            node: info.node,
            locals: &[],
            body: None,
        };
        let mut segs = Vec::new();
        type_segs(*target, self.files[info.file].source, &mut segs);
        let mut out = Vec::new();
        for def in self
            .resolve_path(&segs, Ns::Type, &site)
            .unwrap_or_default()
        {
            if let Def::Type(aliased) = def {
                out.extend(self.alias_targets(aliased, depth + 1));
            }
        }
        out
    }

    /// The items and imports one module or block declares.
    fn scope(&self, file: usize, container: Node<'t>) -> Rc<ItemScope> {
        let key = (file, container.id());
        if let Some(scope) = self.scopes.borrow().get(&key) {
            return Rc::clone(scope);
        }
        let source = self.files[file].source;
        let mut scope = ItemScope::default();
        let mut add = |ns: Ns, name: String, entry: Entry, public: bool| {
            scope
                .named
                .entry((ns, name))
                .or_default()
                .push((entry, public));
        };
        let mut globs = Vec::new();
        for child in named_children_of(container) {
            let public = is_public(child);
            let key = (file, child.id());
            let name = child
                .child_by_field_name("name")
                .map(|name| name_text(name, source));
            match (child.kind(), name) {
                ("function_item" | "const_item" | "static_item", Some(name)) => {
                    if let Some(decl) = self.decl_by_node.get(&key) {
                        add(Ns::Value, name, Entry::Def(Def::Decl(*decl)), public);
                    }
                }
                ("struct_item" | "enum_item" | "union_item" | "type_item", Some(name)) => {
                    if let Some(t) = self.type_by_node.get(&key) {
                        add(Ns::Type, name.clone(), Entry::Def(Def::Type(*t)), public);
                        if matches!(
                            self.types[*t].shape,
                            TypeShape::Struct { constructor: true }
                        ) {
                            add(Ns::Value, name, Entry::Def(Def::Constructor(*t)), public);
                        }
                    }
                }
                ("trait_item", Some(name)) => {
                    if let Some(k) = self.trait_by_node.get(&key) {
                        add(Ns::Type, name, Entry::Def(Def::Trait(*k)), public);
                    }
                }
                ("mod_item", Some(name)) => {
                    if let Some(m) = self.module_by_declaration.get(&key) {
                        add(Ns::Type, name, Entry::Def(Def::Module(*m)), public);
                    }
                }
                ("use_declaration", _) => {
                    for import in self.imports_by_use.get(&key).into_iter().flatten() {
                        match &self.imports[*import].name {
                            Some(name) => {
                                for ns in [Ns::Value, Ns::Type, Ns::Macro] {
                                    add(ns, name.clone(), Entry::Import(*import), public);
                                }
                            }
                            None => globs.push((*import, public)),
                        }
                    }
                }
                ("extern_crate_declaration", _) => {
                    let crate_name = child
                        .child_by_field_name("name")
                        .map(|name| name_text(name, source));
                    let bound = child
                        .child_by_field_name("alias")
                        .map(|alias| name_text(alias, source))
                        .or_else(|| crate_name.clone());
                    if let (Some(crate_name), Some(bound)) = (crate_name, bound) {
                        // `extern crate self as name` names this crate's root.
                        let def = if crate_name == "self" {
                            Def::Module(self.modules[self.module_at(file, child)].root)
                        } else {
                            Def::External(vec![crate_name])
                        };
                        add(Ns::Type, bound, Entry::Def(def), public);
                    }
                }
                ("foreign_mod_item", _) => {
                    for item in child
                        .child_by_field_name("body")
                        .map(named_children_of)
                        .unwrap_or_default()
                    {
                        if let Some(name) = item.child_by_field_name("name") {
                            let name = name_text(name, source);
                            add(
                                Ns::Value,
                                name.clone(),
                                Entry::Def(Def::External(vec![name])),
                                public,
                            );
                        }
                    }
                }
                _ => {}
            }
        }
        // A `macro_rules!` is not an item of its module (its scope is
        // textual, `CrateModel::textual_macro`), except that a crate root
        // holds its crate's `#[macro_export]` macros by path.
        if let Some(module) = self.module_by_items.get(&key)
            && self.modules[*module].parent.is_none()
        {
            for index in self.exported_macros.get(module).into_iter().flatten() {
                let info = &self.macros[*index];
                if let Some(name) = info.node.child_by_field_name("name") {
                    let name = name_text(name, self.files[info.file].source);
                    add(Ns::Macro, name, Entry::Def(Def::Macro(*index)), true);
                }
            }
        }
        scope.globs = globs;
        let scope = Rc::new(scope);
        self.scopes.borrow_mut().insert(key, Rc::clone(&scope));
        scope
    }

    /// A name in one module's or block's scope: its items and explicit imports
    /// first, then its glob imports. `owner` is the module for a module scope,
    /// whose private entries are visible from that module and the modules
    /// nested in it only, and `None` for a block; `from` is the module the
    /// lookup is made from.
    fn lookup_in(
        &self,
        file: usize,
        container: Node<'t>,
        ns: Ns,
        name: &str,
        owner: Option<usize>,
        from: usize,
    ) -> Vec<Def> {
        let mut visited = BTreeSet::new();
        self.lookup_visiting(file, container, ns, name, owner, from, &mut visited)
    }

    /// `lookup_in` that visits each scope once per lookup, as seen from one
    /// module (`visited`), so glob imports that reach one another (a cycle,
    /// or several routes to one module) cost one visit per scope rather than
    /// one per route. A scope reached again adds nothing it did not add the
    /// first time.
    #[allow(clippy::too_many_arguments)]
    fn lookup_visiting(
        &self,
        file: usize,
        container: Node<'t>,
        ns: Ns,
        name: &str,
        owner: Option<usize>,
        from: usize,
        visited: &mut BTreeSet<(usize, usize, usize)>,
    ) -> Vec<Def> {
        if !visited.insert((file, container.id(), from)) {
            return Vec::new();
        }
        self.scope_visits.set(self.scope_visits.get() + 1);
        let scope = self.scope(file, container);
        let visible = |public: bool| public || owner.is_none_or(|owner| self.within(from, owner));
        let mut out: Vec<Def> = Vec::new();
        let push = |out: &mut Vec<Def>, def: Def| {
            if !out.contains(&def) {
                out.push(def);
            }
        };
        if let Some(entries) = scope.named.get(&(ns, name.to_string())) {
            for (entry, public) in entries {
                if !visible(*public) {
                    continue;
                }
                match entry {
                    Entry::Def(def) => push(&mut out, def.clone()),
                    Entry::Import(import) => {
                        for def in self.resolve_import(*import, ns) {
                            push(&mut out, def);
                        }
                    }
                }
            }
        }
        // An item or an explicit import shadows every glob of the scope.
        if !out.is_empty() {
            return out;
        }
        // No item or explicit import of the scope holds the name, so an item
        // a macro of the scope expands to would (a duplicate of an item or an
        // import is an error, so a scope that holds the name is decided).
        if ns != Ns::Macro && self.expands_items(file, container) {
            self.undecided.set(true);
        }
        for (glob, public) in &scope.globs {
            if !visible(*public) {
                continue;
            }
            for def in self.glob_lookup(*glob, ns, name, visited) {
                push(&mut out, def);
            }
        }
        out
    }

    /// A name a glob import brings in: from a crate module, its entries as the
    /// glob's module sees them; from an enum, a variant; from a module outside
    /// the crate, nothing but a `std::env` accessor.
    fn glob_lookup(
        &self,
        glob: usize,
        ns: Ns,
        name: &str,
        visited: &mut BTreeSet<(usize, usize, usize)>,
    ) -> Vec<Def> {
        let from = self.imports[glob].module;
        let mut out = Vec::new();
        for target in self.resolve_import(glob, Ns::Type) {
            match target {
                Def::Module(module) => {
                    let module_info = &self.modules[module];
                    out.extend(self.lookup_visiting(
                        module_info.file,
                        module_info.items,
                        ns,
                        name,
                        Some(module),
                        from,
                        visited,
                    ));
                }
                Def::Type(t) => {
                    if ns == Ns::Value
                        && let TypeShape::Enum(variants) = &self.types[t].shape
                        && variants.contains(name)
                    {
                        out.push(Def::Constructor(t));
                    }
                }
                Def::External(path) => {
                    let mut full = path.clone();
                    full.push(name.to_string());
                    if ns == Ns::Value && is_env_path(&full) {
                        out.push(Def::External(full));
                    }
                }
                _ => {}
            }
        }
        out
    }

    /// What one import binds in `ns`, memoized; an import that reaches itself
    /// through other imports binds nothing on the second visit.
    fn resolve_import(&self, import: usize, ns: Ns) -> Vec<Def> {
        if let Some(defs) = self.import_memo.borrow().get(&(import, ns)) {
            if self.import_undecided.borrow().get(&(import, ns)) == Some(&true) {
                self.undecided.set(true);
            }
            return defs.clone();
        }
        if !self.import_busy.borrow_mut().insert((import, ns)) {
            return Vec::new();
        }
        let outer = self.undecided.replace(false);
        let info = &self.imports[import];
        let path = &info.path;
        let defs = match (&info.name, path.split_last()) {
            (_, None) => Vec::new(),
            (None, Some(_)) => {
                if ns == Ns::Type {
                    self.use_path(import, path, Ns::Type)
                } else {
                    Vec::new()
                }
            }
            (Some(_), Some(_)) => self.use_path(import, path, ns),
        };
        self.import_busy.borrow_mut().remove(&(import, ns));
        self.import_memo
            .borrow_mut()
            .insert((import, ns), defs.clone());
        let undecided = self.undecided.get();
        self.import_undecided
            .borrow_mut()
            .insert((import, ns), undecided);
        self.undecided.set(outer || undecided);
        defs
    }

    /// A use path, resolved the way the 2018 edition reads one: `crate`,
    /// `self` and `super` are the crate root, this module and its parent; a
    /// leading `::` is an extern crate; any other first name is looked up where
    /// the declaration stands and, failing that, is an extern crate.
    fn use_path(&self, import: usize, path: &[Seg], ns: Ns) -> Vec<Def> {
        let info = &self.imports[import];
        let site = Site {
            file: info.file,
            node: info.node,
            locals: &[],
            body: None,
        };
        let Some((first, rest)) = path.split_first() else {
            return Vec::new();
        };
        let starts = match first {
            Seg::Name(name) => {
                let wanted = if rest.is_empty() { ns } else { Ns::Type };
                let mut found = self.lexical_items(name, wanted, &site);
                if found.is_empty()
                    && let Some(def) = self.in_extern_prelude(name, wanted, info.module)
                {
                    found.push(def);
                }
                match (found.is_empty(), rest.is_empty()) {
                    (true, true) => return vec![Def::External(vec![name.clone()])],
                    (true, false) => vec![Cursor::External(vec![name.clone()])],
                    (false, true) => return found,
                    (false, false) => found.iter().filter_map(cursor_of).collect(),
                }
            }
            other => match self.start(other, &site) {
                Ok(cursors) => cursors,
                Err(_) => return Vec::new(),
            },
        };
        self.walk(starts, rest, ns, &site).unwrap_or_default()
    }

    /// A name looked up in the blocks around `site` and then in its module,
    /// without local bindings and without the extern-crate fallback; a macro
    /// name in textual scope first (`textual_macro`).
    fn lexical_items(&self, name: &str, ns: Ns, site: &Site<'_, 't>) -> Vec<Def> {
        if ns == Ns::Macro
            && let Some(found) = self.textual_macro(name, site)
        {
            return vec![Def::Macro(found)];
        }
        let module = self.module_at(site.file, site.node);
        let mut node = Some(site.node);
        while let Some(here) = node {
            if self.module_by_items.contains_key(&(site.file, here.id())) {
                break;
            }
            if here.kind() == "block" {
                let found = self.lookup_in(site.file, here, ns, name, None, module);
                if !found.is_empty() {
                    return found;
                }
            }
            node = here.parent();
        }
        let items = self.modules[module].items;
        self.lookup_in(site.file, items, ns, name, Some(module), module)
    }

    /// A bare name, resolved as Rust scopes it: a local binding or an item of
    /// a block inside the walked body, whichever is innermost (the one whose
    /// scope ends first; on a tie, the one whose scope starts later; on a
    /// tie, the item); then the items of the blocks and the generic parameters
    /// around the body; then the module's items, explicit imports and glob
    /// imports; then the crate's extern prelude (`extern crate` at the root);
    /// and anything else is outside the crate. A macro name is looked up in
    /// textual scope first (`textual_macro`), then by path the same way.
    fn lexical(&self, name: &str, ns: Ns, site: &Site<'_, 't>) -> Lookup {
        if ns == Ns::Macro
            && let Some(found) = self.textual_macro(name, site)
        {
            return Ok(vec![Def::Macro(found)]);
        }
        let at = site.node.start_byte();
        let module = self.module_at(site.file, site.node);
        // `undecided` counts only the scopes up to the one whose entry wins:
        // a block outside the winning one cannot shadow it.
        let outer = self.undecided.replace(false);
        let finish = |found: Vec<Def>, undecided: bool| {
            self.undecided.set(outer || undecided);
            Ok(found)
        };
        let mut best: Option<Innermost> = None;
        let mut undecided_at_best = false;
        if ns == Ns::Value
            && let Some((index, (_, from, to))) = site
                .locals
                .iter()
                .enumerate()
                .filter(|(_, (bound, from, to))| *bound == name && *from <= at && at < *to)
                .min_by_key(|(_, (_, from, to))| (*to, Reverse(*from)))
        {
            best = Some(((*to, Reverse(*from), 1), vec![Def::Local(index)]));
        }
        let mut node = Some(site.node);
        let mut inside_body = site.body.is_some();
        while let Some(here) = node {
            if self.module_by_items.contains_key(&(site.file, here.id())) {
                break;
            }
            if here.kind() == "block" {
                let found = self.lookup_in(site.file, here, ns, name, None, module);
                if !found.is_empty() {
                    let key = (here.end_byte(), Reverse(here.start_byte()), 0);
                    if !inside_body {
                        let undecided = self.undecided.get();
                        return finish(found, undecided);
                    }
                    if best.as_ref().is_none_or(|(best_key, _)| key < *best_key) {
                        best = Some((key, found));
                        undecided_at_best = self.undecided.get();
                    }
                }
            }
            if !inside_body && declares_generic(here, name, ns, self.files[site.file].source) {
                let undecided = self.undecided.get();
                return finish(vec![Def::Generic], undecided);
            }
            if site.body.is_some_and(|body| body.id() == here.id()) {
                if let Some((_, found)) = best.take() {
                    return finish(found, undecided_at_best);
                }
                inside_body = false;
            }
            node = here.parent();
        }
        if let Some((_, found)) = best {
            return finish(found, undecided_at_best);
        }
        let items = self.modules[module].items;
        let found = self.lookup_in(site.file, items, ns, name, Some(module), module);
        let undecided = self.undecided.get();
        if !found.is_empty() {
            return finish(found, undecided);
        }
        if let Some(def) = self.in_extern_prelude(name, ns, module) {
            return finish(vec![def], undecided);
        }
        finish(vec![Def::External(vec![name.to_string()])], undecided)
    }

    /// Whether `container` (a block, or a module's item list) holds a macro
    /// invocation where it may expand to items: an item of a module, or a
    /// statement of a block (`m!();`, `m! { .. }`, a block's tail `m!()`),
    /// other than a standard-library macro of `IN_PLACE_MACROS`, which
    /// expands to an expression. The gate expands no such invocation into
    /// items, so a name looked up past it is not decided by the parse
    /// (`undecided`).
    fn expands_items(&self, file: usize, container: Node<'t>) -> bool {
        if self.deciding.get() {
            return false;
        }
        let key = (file, container.id());
        if let Some(known) = self.item_expanding.borrow().get(&key) {
            return *known;
        }
        self.deciding.set(true);
        let outer = self.undecided.get();
        let source = self.files[file].source;
        let answer = named_children_of(container).into_iter().any(|child| {
            let invocation = match child.kind() {
                "macro_invocation" => child,
                "expression_statement" => match named_children_of(child)
                    .into_iter()
                    .find(|inner| !is_comment(*inner))
                {
                    Some(inner) if inner.kind() == "macro_invocation" => inner,
                    _ => return false,
                },
                _ => return false,
            };
            let in_place = !self.has_macro_use_extern_crate()
                && invocation.child_by_field_name("macro").is_some_and(|name| {
                    let site = Site {
                        file,
                        node: name,
                        locals: &[],
                        body: None,
                    };
                    match self
                        .resolve_path(&segs_of(name, source), Ns::Macro, &site)
                        .as_deref()
                    {
                        Ok([Def::External(path)]) => match path.as_slice() {
                            [only] => IN_PLACE_MACROS.contains(&only.as_str()),
                            [krate, only] => {
                                STD_CRATES.contains(&krate.as_str())
                                    && IN_PLACE_MACROS.contains(&only.as_str())
                            }
                            _ => false,
                        },
                        _ => false,
                    }
                });
            !in_place
        });
        self.undecided.set(outer);
        self.deciding.set(false);
        self.item_expanding.borrow_mut().insert(key, answer);
        answer
    }

    /// Whether a crate root a build reaches holds `#[macro_use] extern
    /// crate`, whose macros a bare name may reach, so that no bare name
    /// names the standard library's macro for sure.
    fn has_macro_use_extern_crate(&self) -> bool {
        *self.macro_use_extern_crate.get_or_init(|| {
            self.files.iter().any(|file| {
                if file.class == FileLiveness::Unreachable
                    && file.test_class == FileLiveness::Unreachable
                {
                    return false;
                }
                let mut stack = vec![file.root];
                while let Some(node) = stack.pop() {
                    if node.kind() == "extern_crate_declaration"
                        && preceding_attributes(node)
                            .into_iter()
                            .any(|attribute| attribute_path_is(attribute, file.source, "macro_use"))
                    {
                        return true;
                    }
                    stack.extend(children_of(node));
                }
                false
            })
        })
    }

    /// What the extern prelude of `module`'s crate binds `name` to in the
    /// type namespace: an `extern crate` at the crate root (`extern crate
    /// self as name` binds the root module).
    fn in_extern_prelude(&self, name: &str, ns: Ns, module: usize) -> Option<Def> {
        (ns == Ns::Type)
            .then(|| {
                self.extern_prelude
                    .get(&(self.modules[module].root, name.to_string()))
                    .cloned()
            })
            .flatten()
    }

    /// A `macro_rules!` name in textual scope at `site`, as rustc scopes one:
    /// the last definition of the name before `site` among the items of a
    /// block or module around it, where a module's textual scope continues
    /// into the modules it declares after the definition (from a file module,
    /// at its `mod` item in the declaring file), and a `#[macro_use]`
    /// module's definitions stay in scope after the module's own item. A
    /// glob import never brings a `macro_rules!` in: only a `use` of one or a
    /// `#[macro_export]` (a crate root item) gives it a path.
    fn textual_macro(&self, name: &str, site: &Site<'_, 't>) -> Option<usize> {
        let mut file = site.file;
        let mut child = site.node;
        loop {
            let mut current = child.parent();
            while let Some(container) = current {
                if matches!(
                    container.kind(),
                    "block" | "declaration_list" | "source_file"
                ) && let Some(found) =
                    self.last_macro_before(file, container, Some(child.start_byte()), name)
                {
                    return Some(found);
                }
                child = container;
                current = container.parent();
            }
            let module = self.module_by_items.get(&(file, child.id()))?;
            let (declaring_file, declaration) = self.modules[*module].declaration?;
            file = declaring_file;
            child = declaration;
        }
    }

    /// The last definition of `name` among the items of `container` that
    /// start before `before` (all of them for `None`), a `#[macro_use]`
    /// module's own last definition counting where the module stands.
    fn last_macro_before(
        &self,
        file: usize,
        container: Node<'t>,
        before: Option<usize>,
        name: &str,
    ) -> Option<usize> {
        let source = self.files[file].source;
        let mut found = None;
        for item in named_children_of(container) {
            if before.is_some_and(|before| item.start_byte() >= before) {
                break;
            }
            match item.kind() {
                "macro_definition"
                    if item
                        .child_by_field_name("name")
                        .is_some_and(|written| name_text(written, source) == name) =>
                {
                    if let Some(index) = self.macro_by_node.get(&(file, item.id())) {
                        found = Some(*index);
                    }
                }
                "mod_item"
                    if preceding_attributes(item)
                        .into_iter()
                        .any(|attribute| attribute_path_is(attribute, source, "macro_use")) =>
                {
                    if let Some(module) = self.module_by_declaration.get(&(file, item.id())) {
                        let info = &self.modules[*module];
                        if let Some(inner) =
                            self.last_macro_before(info.file, info.items, None, name)
                        {
                            found = Some(inner);
                        }
                    }
                }
                _ => {}
            }
        }
        found
    }

    /// A path in `ns`, from where `site` stands. A prefix that names several
    /// candidates (the cfg variants of one type, each an item of the same
    /// module) is followed through each, and the results are joined.
    fn resolve_path(&self, segs: &[Seg], ns: Ns, site: &Site<'_, 't>) -> Lookup {
        match segs {
            [] => Err("an empty path"),
            [Seg::Name(name)] => self.lexical(name, ns, site),
            // The receiver `self`, named as a value.
            [Seg::SelfModule] if ns == Ns::Value => Ok(vec![Def::SelfValue]),
            [first, rest @ ..] => {
                let cursors = self.start(first, site)?;
                self.walk(cursors, rest, ns, site)
            }
        }
    }

    /// What a path's first segment names.
    fn start(&self, first: &Seg, site: &Site<'_, 't>) -> Result<Vec<Cursor>, &'static str> {
        let module = self.module_at(site.file, site.node);
        match first {
            Seg::Global => Ok(vec![Cursor::External(Vec::new())]),
            Seg::Crate => Ok(vec![Cursor::Module(self.modules[module].root)]),
            Seg::SelfModule => Ok(vec![Cursor::Module(module)]),
            Seg::Super => self.modules[module]
                .parent
                .map(|parent| vec![Cursor::Module(parent)])
                .ok_or("a super beyond the crate root"),
            Seg::SelfType => self.self_type_at(site),
            Seg::Name(name) => {
                let found = self.lexical(name, Ns::Type, site)?;
                if found.contains(&Def::Generic) {
                    return Err("a path through a generic parameter");
                }
                let cursors: Vec<Cursor> = found.iter().filter_map(cursor_of).collect();
                if cursors.is_empty() {
                    Err("a path through a name that is not a module or a type")
                } else {
                    Ok(cursors)
                }
            }
            Seg::Qualified(ty, tr) => {
                let trait_ref = match tr {
                    None => None,
                    Some(tr) => Some(match self.resolve_path(tr, Ns::Type, site)?.as_slice() {
                        [Def::Trait(k)] => TraitRef::Crate(*k),
                        [Def::External(path)] => TraitRef::External(path.clone()),
                        _ => TraitRef::Unresolved,
                    }),
                };
                // A type outside the crate under a crate trait
                // (`<u32 as crate::Trait>::f`) names that trait's items, in
                // every impl of it; outside the crate otherwise. A type that is
                // not a path (`<[u8]>::to_vec`) is never a crate type.
                let outside = |path: Vec<String>| match &trait_ref {
                    Some(TraitRef::Crate(k)) => Cursor::Trait(*k),
                    _ => Cursor::External(path),
                };
                if ty.as_slice() == [Seg::Opaque] {
                    return Ok(vec![outside(Vec::new())]);
                }
                let types = self.resolve_path(ty, Ns::Type, site)?;
                let mut cursors = Vec::new();
                for def in types {
                    match def {
                        Def::Type(t) => cursors.push(Cursor::Qualified(t, trait_ref.clone())),
                        Def::External(path) => cursors.push(outside(path)),
                        Def::Generic => return Err("a path through a generic parameter"),
                        _ => {}
                    }
                }
                if cursors.is_empty() {
                    Err("a qualified path through a type the parse does not name")
                } else {
                    Ok(cursors)
                }
            }
            // `<dyn Trait>::f`: the trait's item, in every impl of it.
            Seg::Dyn(tr) => match self.resolve_path(tr, Ns::Type, site)?.as_slice() {
                [Def::Trait(k)] => Ok(vec![Cursor::Trait(*k)]),
                [Def::External(path)] => Ok(vec![Cursor::External(path.clone())]),
                _ => Err("a dyn type whose trait the parse does not name"),
            },
            Seg::Opaque => Err("a path segment a macro builds, or a type that is not a path"),
        }
    }

    /// `Self`: the types of the nearest enclosing `impl` (one, or each cfg
    /// variant of it), or the nearest enclosing trait.
    fn self_type_at(&self, site: &Site<'_, 't>) -> Result<Vec<Cursor>, &'static str> {
        let mut node = Some(site.node);
        while let Some(here) = node {
            match here.kind() {
                "impl_item" => {
                    return match self.impl_by_node.get(&(site.file, here.id())) {
                        Some(index) if !self.impls[*index].self_types.is_empty() => Ok(self.impls
                            [*index]
                            .self_types
                            .iter()
                            .map(|t| Cursor::Type(*t))
                            .collect()),
                        Some(_) => Err("Self of an impl for a type the parse does not declare"),
                        None => Err("Self of an impl the model does not hold"),
                    };
                }
                "trait_item" => {
                    return self
                        .trait_by_node
                        .get(&(site.file, here.id()))
                        .map(|k| vec![Cursor::Trait(*k)])
                        .ok_or("Self of a trait the model does not hold");
                }
                _ => {}
            }
            node = here.parent();
        }
        Err("Self outside an impl or a trait")
    }

    /// The rest of a path after its first segment, from every candidate the
    /// prefix leaves; a candidate that does not resolve is dropped, and the
    /// path is unresolved only when none does.
    fn walk(&self, cursors: Vec<Cursor>, rest: &[Seg], ns: Ns, site: &Site<'_, 't>) -> Lookup {
        let from = self.module_at(site.file, site.node);
        let Some((last, middle)) = rest.split_last() else {
            return Ok(cursors
                .iter()
                .map(|cursor| self.value_of(cursor, ns))
                .collect());
        };
        let mut cursors = cursors;
        for seg in middle {
            let mut next: Vec<Cursor> = Vec::new();
            let mut reason = None;
            for cursor in &cursors {
                match self.step(cursor, seg, from) {
                    Ok(found) => {
                        for cursor in found {
                            if !next.contains(&cursor) {
                                next.push(cursor);
                            }
                        }
                    }
                    Err(why) => reason = reason.or(Some(why)),
                }
            }
            if next.is_empty() {
                return Err(reason.unwrap_or("a path shape the model does not follow"));
            }
            cursors = next;
        }
        let mut out: Vec<Def> = Vec::new();
        let mut reason = None;
        for cursor in &cursors {
            match self.last_segment(cursor, last, ns, from) {
                Ok(found) => {
                    for def in found {
                        if !out.contains(&def) {
                            out.push(def);
                        }
                    }
                }
                Err(why) => reason = reason.or(Some(why)),
            }
        }
        if out.is_empty() {
            Err(reason.unwrap_or("a path shape the model does not follow"))
        } else {
            Ok(out)
        }
    }

    /// A middle segment of a path.
    fn step(&self, cursor: &Cursor, seg: &Seg, from: usize) -> Result<Vec<Cursor>, &'static str> {
        match (cursor, seg) {
            (Cursor::Module(module), Seg::Super) => self.modules[*module]
                .parent
                .map(|parent| vec![Cursor::Module(parent)])
                .ok_or("a super beyond the crate root"),
            (Cursor::Module(module), Seg::Name(name)) => {
                let info = &self.modules[*module];
                let found =
                    self.lookup_in(info.file, info.items, Ns::Type, name, Some(*module), from);
                if found.is_empty() {
                    return Err("a name a crate module does not hold");
                }
                let cursors: Vec<Cursor> = found.iter().filter_map(cursor_of).collect();
                if cursors.is_empty() {
                    Err("a path through a name that is not a module or a type")
                } else {
                    Ok(cursors)
                }
            }
            (Cursor::External(path), Seg::Name(name)) => {
                // `::name` where an `extern crate self as name` binds the crate.
                if path.is_empty()
                    && let Some(Def::Module(module)) = self.in_extern_prelude(name, Ns::Type, from)
                {
                    return Ok(vec![Cursor::Module(module)]);
                }
                let mut path = path.clone();
                path.push(name.clone());
                Ok(vec![Cursor::External(path)])
            }
            (Cursor::Type(_) | Cursor::Trait(_) | Cursor::Qualified(..), Seg::Name(_)) => {
                Err("a path through an associated type")
            }
            (_, Seg::Opaque) => Err("a path segment a macro builds, or a type that is not a path"),
            _ => Err("a path shape the model does not follow"),
        }
    }

    /// The last segment of a path.
    fn last_segment(&self, cursor: &Cursor, seg: &Seg, ns: Ns, from: usize) -> Lookup {
        match (cursor, seg) {
            (_, Seg::Opaque) => Err("a path segment a macro builds, or a type that is not a path"),
            (Cursor::Module(module), Seg::Name(name)) => {
                let info = &self.modules[*module];
                let found = self.lookup_in(info.file, info.items, ns, name, Some(*module), from);
                if found.is_empty() {
                    Err("a name a crate module does not hold")
                } else {
                    Ok(found)
                }
            }
            (Cursor::Module(module), Seg::Super) => self.modules[*module]
                .parent
                .map(|parent| vec![Def::Module(parent)])
                .ok_or("a super beyond the crate root"),
            (Cursor::External(path), Seg::Name(name)) => {
                let mut path = path.clone();
                path.push(name.clone());
                Ok(vec![Def::External(path)])
            }
            (Cursor::Type(t), Seg::Name(name)) if ns == Ns::Value => {
                self.associated(*t, name, None, 0)
            }
            (Cursor::Qualified(t, tr), Seg::Name(name)) if ns == Ns::Value => {
                self.associated(*t, name, tr.as_ref(), 0)
            }
            (Cursor::Trait(k), Seg::Name(name)) if ns == Ns::Value => self.trait_member(*k, name),
            (Cursor::Type(_) | Cursor::Qualified(..) | Cursor::Trait(_), _) => {
                Err("a path through an associated type")
            }
            _ => Err("a path shape the model does not follow"),
        }
    }

    /// What a cursor stands for when the path ends on it: in the value
    /// namespace a tuple or unit struct is its constructor.
    fn value_of(&self, cursor: &Cursor, ns: Ns) -> Def {
        match cursor {
            Cursor::Type(t)
                if ns == Ns::Value
                    && matches!(
                        self.types[*t].shape,
                        TypeShape::Struct { constructor: true }
                    ) =>
            {
                Def::Constructor(*t)
            }
            other => def_of(other),
        }
    }

    /// `Type::name`: an enum's variant, then the type's inherent impls, then
    /// its trait impls and every blanket impl (`impl<T> Trait for T`, its
    /// bounds not read) and the methods its derives generate
    /// (`DERIVED_METHODS`), then the defaults of the crate traits those impls
    /// implement, each restricted to `filter`'s trait when the path names
    /// one. A type alias is followed to the type it names.
    fn associated(&self, t: usize, name: &str, filter: Option<&TraitRef>, depth: usize) -> Lookup {
        match &self.types[t].shape {
            TypeShape::Alias(Some(target)) if depth < RESOLUTION_DEPTH => {
                let info = &self.types[t];
                let site = Site {
                    file: info.file,
                    node: info.node,
                    locals: &[],
                    body: None,
                };
                let mut segs = Vec::new();
                type_segs(*target, self.files[info.file].source, &mut segs);
                // An alias of a type that is not a path (`u64`, a tuple) names
                // no crate type.
                if segs.as_slice() == [Seg::Opaque] {
                    return Ok(vec![Def::External(vec![name.to_string()])]);
                }
                let mut out = Vec::new();
                for def in self.resolve_path(&segs, Ns::Type, &site)? {
                    match def {
                        Def::Type(aliased) => {
                            out.extend(self.associated(aliased, name, filter, depth + 1)?)
                        }
                        Def::External(mut path) => {
                            path.push(name.to_string());
                            out.push(Def::External(path));
                        }
                        _ => return Err("a path through a type alias the model does not follow"),
                    }
                }
                return Ok(out);
            }
            TypeShape::Enum(variants) if filter.is_none() && variants.contains(name) => {
                return Ok(vec![Def::Constructor(t)]);
            }
            _ => {}
        }
        // The type's own impls, then every blanket impl (`impl<T> Trait for
        // T`), whose bounds are not read.
        let mut impls: Vec<usize> = self.impls_of_type.get(&t).cloned().unwrap_or_default();
        impls.extend(
            self.impls
                .iter()
                .enumerate()
                .filter(|(_, info)| info.blanket)
                .map(|(index, _)| index),
        );
        let trait_matches = |trait_ref: Option<&TraitRef>| match (filter, trait_ref) {
            (None, _) => true,
            (Some(TraitRef::Crate(k)), Some(TraitRef::Crate(j))) => k == j,
            (Some(TraitRef::External(a)), Some(TraitRef::External(b))) => a.last() == b.last(),
            _ => false,
        };
        let members = |inherent: bool| -> Vec<Def> {
            impls
                .iter()
                .filter(|i| self.impls[**i].trait_ref.is_none() == inherent)
                .filter(|i| trait_matches(self.impls[**i].trait_ref.as_ref()))
                .flat_map(|i| self.impls[*i].members.get(name).into_iter().flatten())
                .map(|decl| Def::Decl(*decl))
                .collect()
        };
        if filter.is_none() {
            let inherent = members(true);
            if !inherent.is_empty() {
                return Ok(inherent);
            }
        }
        let mut from_traits = members(false);
        if let Some((trait_name, decl)) = self.types[t].derived.get(name)
            && trait_matches(Some(&TraitRef::External(vec![trait_name.clone()])))
        {
            from_traits.push(Def::Decl(*decl));
        }
        if !from_traits.is_empty() {
            return Ok(from_traits);
        }
        let mut defaults = Vec::new();
        for impl_index in impls
            .iter()
            .filter(|i| trait_matches(self.impls[**i].trait_ref.as_ref()))
        {
            let info = &self.impls[*impl_index];
            if let Some(TraitRef::Crate(k)) = &info.trait_ref
                && !info.members.contains_key(name)
            {
                for decl in self.traits[*k].members.get(name).into_iter().flatten() {
                    let def = Def::Decl(*decl);
                    if !defaults.contains(&def) {
                        defaults.push(def);
                    }
                }
            }
        }
        if defaults.is_empty() {
            Err("an associated item no impl or derive in the parse defines")
        } else {
            Ok(defaults)
        }
    }

    /// `Trait::name` or `Self::name` inside a trait: the trait's own default
    /// and every crate impl's item of that name.
    fn trait_member(&self, k: usize, name: &str) -> Lookup {
        let mut out: Vec<Def> = self.traits[k]
            .members
            .get(name)
            .into_iter()
            .flatten()
            .map(|decl| Def::Decl(*decl))
            .collect();
        for impl_index in self.impls_of_trait.get(&k).into_iter().flatten() {
            for decl in self.impls[*impl_index]
                .members
                .get(name)
                .into_iter()
                .flatten()
            {
                out.push(Def::Decl(*decl));
            }
        }
        if out.is_empty() {
            Err("a trait item no impl or default in the parse defines")
        } else {
            Ok(out)
        }
    }

    /// The crate bodies a call through an external trait's path
    /// (`Default::default()`, `From::from(x)`) can run, its type being
    /// inferred: every crate impl of a trait so named that defines the
    /// method, and every std derive that generates it.
    fn trait_dispatch(&self, trait_name: &str, method: &str) -> Vec<usize> {
        let mut out = Vec::new();
        for info in &self.impls {
            if let Some(TraitRef::External(path)) = &info.trait_ref
                && path.last().is_some_and(|last| last == trait_name)
            {
                out.extend(info.members.get(method).into_iter().flatten().copied());
            }
        }
        for info in &self.types {
            if let Some((derived_trait, decl)) = info.derived.get(method)
                && derived_trait == trait_name
            {
                out.push(*decl);
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    /// The bodies code outside the crate can run on type `t`: the methods its
    /// derives generate and the items of its impls of traits outside the crate.
    fn bodies_outside_code_can_run(&self, t: usize) -> Vec<usize> {
        let mut out: Vec<usize> = self.types[t]
            .derived
            .values()
            .map(|(_, decl)| *decl)
            .collect();
        for impl_index in self.impls_of_type.get(&t).into_iter().flatten() {
            let info = &self.impls[*impl_index];
            if matches!(info.trait_ref, Some(TraitRef::External(_))) {
                out.extend(info.members.values().flatten().copied());
            }
        }
        out
    }

    /// One reference's class, candidates and reason, from its resolution: one
    /// function or derived method is resolved; a const or static is an edge to
    /// its initializer; several declarations are ambiguous, an edge to each; a
    /// constructor runs no crate code; a `std::env` accessor, or a path written
    /// ending in `env::<accessor>` that resolves to no crate item, is an
    /// environment call; another path out of the crate whose last two segments
    /// name a trait the crate implements and a method its impls define is a
    /// trait dispatch (`trait_dispatch`), and otherwise out of the crate.
    fn classify(
        &self,
        lookup: Lookup,
        written: &str,
    ) -> (EdgeClass, Vec<usize>, Option<&'static str>) {
        match lookup {
            Err(_) if written_env_path(written) => (EdgeClass::Environment, Vec::new(), None),
            Err(reason) => (EdgeClass::PathUnresolved, Vec::new(), Some(reason)),
            Ok(defs) => match defs.as_slice() {
                [Def::Decl(decl)]
                    if matches!(
                        self.decls[*decl].kind,
                        DeclKind::Function | DeclKind::Derived
                    ) =>
                {
                    (EdgeClass::PathResolved, vec![*decl], None)
                }
                [Def::Decl(decl)] => (EdgeClass::PathConstOrStatic, vec![*decl], None),
                [Def::Local(_) | Def::SelfValue] => (EdgeClass::LocalBinding, Vec::new(), None),
                [Def::Constructor(_)] => (EdgeClass::PathConstructor, Vec::new(), None),
                // The cfg variants of one tuple or unit struct.
                many if many.iter().all(|def| matches!(def, Def::Constructor(_))) => {
                    (EdgeClass::PathConstructor, Vec::new(), None)
                }
                [Def::External(path)] if is_env_path(path) || written_env_path(written) => {
                    (EdgeClass::Environment, Vec::new(), None)
                }
                [Def::External(path)] => {
                    let dispatch = match path.as_slice() {
                        [.., owner, method] if owner.starts_with(char::is_uppercase) => {
                            self.trait_dispatch(owner, method)
                        }
                        _ => Vec::new(),
                    };
                    if dispatch.is_empty() {
                        (EdgeClass::PathExternal, Vec::new(), None)
                    } else {
                        (EdgeClass::PathTraitDispatch, dispatch, None)
                    }
                }
                [Def::Generic] => (
                    EdgeClass::PathUnresolved,
                    Vec::new(),
                    Some("a generic parameter"),
                ),
                [_] => (
                    EdgeClass::PathUnresolved,
                    Vec::new(),
                    Some("a name that is not a value"),
                ),
                many => (
                    EdgeClass::PathAmbiguous,
                    many.iter()
                        .filter_map(|def| match def {
                            Def::Decl(decl) => Some(*decl),
                            _ => None,
                        })
                        .collect(),
                    None,
                ),
            },
        }
    }
}

/// The cursor a definition names as a path prefix.
fn cursor_of(def: &Def) -> Option<Cursor> {
    match def {
        Def::Module(module) => Some(Cursor::Module(*module)),
        Def::Type(t) => Some(Cursor::Type(*t)),
        Def::Trait(k) => Some(Cursor::Trait(*k)),
        Def::External(path) => Some(Cursor::External(path.clone())),
        _ => None,
    }
}

/// The definition a cursor stands for when the path ends there.
fn def_of(cursor: &Cursor) -> Def {
    match cursor {
        Cursor::Module(module) => Def::Module(*module),
        Cursor::Type(t) | Cursor::Qualified(t, _) => Def::Type(*t),
        Cursor::Trait(k) => Def::Trait(*k),
        Cursor::External(path) => Def::External(path.clone()),
    }
}

/// Whether `item` declares a generic parameter `name` in `ns`: a type
/// parameter in the type namespace, a const parameter in the value one.
fn declares_generic(item: Node<'_>, name: &str, ns: Ns, source: &[u8]) -> bool {
    let Some(parameters) = item.child_by_field_name("type_parameters") else {
        return false;
    };
    named_children_of(parameters).into_iter().any(|parameter| {
        let wanted = match parameter.kind() {
            "type_parameter" => ns == Ns::Type,
            "const_parameter" => ns == Ns::Value,
            _ => false,
        };
        wanted
            && parameter
                .child_by_field_name("name")
                .is_some_and(|n| name_text(n, source) == name)
    })
}

/// A type's name as the report prints it: the path's last segment, without
/// generic arguments.
fn type_label(ty: Node<'_>, source: &[u8]) -> String {
    let mut segs = Vec::new();
    type_segs(ty, source, &mut segs);
    match segs.last() {
        Some(Seg::Name(name)) => name.clone(),
        _ => compact(ty, source),
    }
}

/// The names of the functions an item is nested in, outermost first, joined
/// with `::`, or `None` for an item no function holds.
fn enclosing_functions(node: Node<'_>, source: &[u8]) -> Option<String> {
    let mut names = Vec::new();
    let mut current = node.parent();
    while let Some(here) = current {
        if matches!(here.kind(), "function_item" | "const_item" | "static_item")
            && let Some(name) = here.child_by_field_name("name")
        {
            names.push(name_text(name, source));
        }
        if here.kind() == "impl_item" {
            if let Some(ty) = here.child_by_field_name("type") {
                names.push(type_label(ty, source));
            }
            break;
        }
        if here.kind() == "trait_item" {
            if let Some(name) = here.child_by_field_name("name") {
                names.push(name_text(name, source));
            }
            break;
        }
        current = here.parent();
    }
    if names.is_empty() {
        return None;
    }
    names.reverse();
    Some(names.join("::"))
}

/// A struct's fields with their declared types: named fields by name,
/// positional fields by index. Not a struct: none.
fn struct_fields<'t>(item: Node<'t>, source: &[u8]) -> Vec<(String, Node<'t>)> {
    let mut out = Vec::new();
    if item.kind() != "struct_item" {
        return out;
    }
    let Some(body) = item.child_by_field_name("body") else {
        return out;
    };
    match body.kind() {
        "field_declaration_list" => {
            for field in named_children_of(body) {
                if field.kind() == "field_declaration"
                    && let (Some(name), Some(field_type)) = (
                        field.child_by_field_name("name"),
                        field.child_by_field_name("type"),
                    )
                {
                    out.push((name_text(name, source), field_type));
                }
            }
        }
        "ordered_field_declaration_list" => {
            let mut cursor = body.walk();
            for (position, field_type) in
                body.children_by_field_name("type", &mut cursor).enumerate()
            {
                out.push((position.to_string(), field_type));
            }
        }
        _ => {}
    }
    out
}

// ---------------------------------------------------------------------------
// What each body does: references, guard flows, constructions
// ---------------------------------------------------------------------------

/// The forms of holding the crate lock a report prints, one per function
/// (decision D-i7-envlock-1): a shape-1 guard of its own body, the body of a shape-2
/// helper, the `drop` of a shape-3 holder, an acquisition in no accepted
/// shape, or none of these.
const HELD_NONE: &str = "none";
const HELD_IN_BODY: &str = "a guard bound in its own body (shape 1)";
const HELD_HELPER: &str = "a guard-returning helper (shape 2)";
const HELD_BY_DROP: &str = "the drop of a holder (shape 3)";
const HELD_REFUSED: &str = "an acquisition in no accepted shape";

/// The written form of a reference (design W4-D27 part 1). The accounting
/// prints every resolution class under it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Qualifier {
    /// `name`: one segment, resolved lexically (`CrateModel::lexical`).
    Bare,
    /// `Self::name`.
    SelfType,
    /// `<T>::name` or `<T as Trait>::name`.
    Qualified,
    /// Any other path of two or more segments: `crate::name`, `module::name`,
    /// `Type::name`, `Type::<T>::name`, `std::env::var`.
    Path,
    /// `receiver.name()`: the receiver's type is not in this parse, so the
    /// call never resolves.
    Method,
    /// A callee that is neither a path (parenthesised or not) nor a method: a
    /// call of a call, of a closure literal, of an index expression, of a
    /// parenthesised field (`(s.f)()`).
    Other,
}

impl Qualifier {
    /// The class the accounting prints this reference under.
    fn class(&self) -> &'static str {
        match self {
            Qualifier::Bare => "bare",
            Qualifier::SelfType => "Self",
            Qualifier::Qualified => "qualified",
            Qualifier::Path => "path",
            Qualifier::Method => "method",
            Qualifier::Other => "other",
        }
    }
}

/// The written form of a path's segments.
fn qualifier_of(segs: &[Seg]) -> Qualifier {
    match segs {
        [Seg::Name(_)] => Qualifier::Bare,
        [Seg::SelfType, ..] => Qualifier::SelfType,
        [Seg::Qualified(..), ..] => Qualifier::Qualified,
        _ => Qualifier::Path,
    }
}

/// Where a reference stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Position {
    /// The callee of a `call_expression`.
    Call,
    /// A path in an expression slot that is not a callee: a function named as
    /// a value is an edge to it, as a call is.
    Value,
    /// A path followed by a parenthesised token group in a macro's tokens.
    MacroCall,
    /// Any other path in a macro's tokens that is not a binding, an item name,
    /// a field name or a type.
    MacroValue,
}

impl Position {
    fn is_call(self) -> bool {
        matches!(self, Position::Call | Position::MacroCall)
    }
}

/// `|p| p.into_inner()`, exactly: a closure with no `move`, `async` or
/// `static`, one untyped parameter that is a plain name, and a body that is
/// the call `into_inner()` with no argument on that name, not in a block.
fn is_into_inner_closure(closure: Node<'_>, source: &[u8]) -> bool {
    if closure.kind() != "closure_expression"
        || children_of(closure)
            .iter()
            .any(|child| matches!(child.kind(), "move" | "async" | "static"))
    {
        return false;
    }
    let parameters: Vec<Node<'_>> = closure
        .child_by_field_name("parameters")
        .map(named_children_of)
        .unwrap_or_default()
        .into_iter()
        .filter(|parameter| !is_comment(*parameter))
        .collect();
    let [parameter] = parameters.as_slice() else {
        return false;
    };
    if parameter.kind() != "identifier" {
        return false;
    }
    let name = text(*parameter, source);
    let Some(body) = closure.child_by_field_name("body") else {
        return false;
    };
    body.kind() == "call_expression"
        && body
            .child_by_field_name("arguments")
            .is_some_and(|arguments| arguments_of(arguments).is_empty())
        && body
            .child_by_field_name("function")
            .filter(|function| function.kind() == "field_expression")
            .is_some_and(|function| {
                function
                    .child_by_field_name("field")
                    .is_some_and(|field| text(field, source) == "into_inner")
                    && function.child_by_field_name("value").is_some_and(|value| {
                        value.kind() == "identifier" && text(value, source) == name
                    })
            })
}

/// The named children of an `arguments` node that are arguments: comments
/// and attributes left out.
fn arguments_of(arguments: Node<'_>) -> Vec<Node<'_>> {
    named_children_of(arguments)
        .into_iter()
        .filter(|argument| !is_comment(*argument) && argument.kind() != "attribute_item")
        .collect()
}

/// Whether `value` is the tail of `block`, what gives the block its value:
/// its last child. The grammar wraps a trailing `if`, `match`, block,
/// `unsafe` block or loop in an expression statement with no `;`, which is
/// the tail too; the one caller, `written_let_type`, climbs into an
/// expression statement only when it has no `;`, so `value` is never one
/// that ends in `;`.
fn is_tail(block: Node<'_>, value: Node<'_>) -> bool {
    named_children_of(block)
        .into_iter()
        .rfind(|child| !is_comment(*child))
        .is_some_and(|last| last.id() == value.id())
}

/// Whether an expression statement ends in `;`.
fn ends_in_a_semicolon(statement: Node<'_>) -> bool {
    children_of(statement)
        .iter()
        .any(|child| child.kind() == ";")
}

/// What the resolution makes of one reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EdgeClass {
    /// Exactly one function or derived method: an edge to it.
    PathResolved,
    /// A const or static: an edge to its initializer, walked as a body, so a
    /// function its initializer names is reached through it.
    PathConstOrStatic,
    /// More than one candidate (the cfg variants of one function, an item two
    /// glob imports both bring in, a method two trait impls both define): the
    /// callee is not decided, so the reference is an edge to every candidate.
    PathAmbiguous,
    /// A struct's or an enum variant's constructor: no crate code runs.
    PathConstructor,
    /// Outside the crate: the standard library, a dependency, the prelude, an
    /// extern crate, or a name no item, import or local binding of the parse
    /// binds (an item a macro produces).
    PathExternal,
    /// A call through an external trait's path (`Default::default()`,
    /// `From::from(x)`), whose type is inferred: an edge to every crate body
    /// that can run (`CrateModel::trait_dispatch`).
    PathTraitDispatch,
    /// A call out of the crate whose value is written as a crate type (a
    /// turbofish, or the type of the `let` it reaches): code outside the crate
    /// reaches that type only through its trait impls, so an edge to each of
    /// its derived methods and items of impls of external traits
    /// (`BodyWalk::external_bodies`), as `toml::from_str` reaches a derived
    /// `deserialize`.
    PathExternalAtCrateType,
    /// An intra-crate path the model does not follow; the reason is counted.
    PathUnresolved,
    /// A method call, which never resolves.
    MethodUnresolved,
    /// A call or a value of a `std::env` accessor.
    Environment,
    /// A callee that is neither a path nor a method (`Qualifier::Other`).
    OtherCallee,
    /// A name the body's own scope binds, or the receiver `self`.
    LocalBinding,
    /// A path reference not yet classified. No report may carry one, and the
    /// gate asserts that.
    PathPending,
}

impl EdgeClass {
    fn name(self) -> &'static str {
        match self {
            EdgeClass::PathResolved => "path resolved",
            EdgeClass::PathConstOrStatic => "path to a const or static",
            EdgeClass::PathAmbiguous => "path ambiguous",
            EdgeClass::PathConstructor => "path to a constructor",
            EdgeClass::PathExternal => "path out of the crate",
            EdgeClass::PathTraitDispatch => "path through an external trait",
            EdgeClass::PathExternalAtCrateType => "path out of the crate at a crate type",
            EdgeClass::PathUnresolved => "path unresolved",
            EdgeClass::MethodUnresolved => "method unresolved",
            EdgeClass::Environment => "environment call",
            EdgeClass::OtherCallee => "other callee",
            EdgeClass::LocalBinding => "local binding",
            EdgeClass::PathPending => "path pending",
        }
    }
}

/// One reference in a body (a call, or a path naming a value, from the parse
/// or from a macro's tokens) and what the resolution makes of it.
#[derive(Debug, Clone)]
struct CallTarget {
    /// The last segment of the path, or the method name.
    name: String,
    /// The written form of the reference.
    qualifier: Qualifier,
    position: Position,
    /// Where the reference is written: its byte offset in its file (for a
    /// reference a crate `macro_rules!` expands to, the invocation's), which
    /// a guard's coverage holds or not (`cover`).
    site: usize,
    /// The innermost closure, async block or macro invocation around the
    /// site that does not run its code where it is written (`opaque_region`),
    /// by its byte range: only a guard bound in the same region covers it.
    region: Option<(usize, usize)>,
    /// For an environment call: the call is test code
    /// (`FileContext::is_test_code`).
    test_code: bool,
    /// The call takes the crate lock: `.lock()` on a receiver that resolves
    /// to the crate lock (through parentheses, `&` and `*`), or a path call
    /// ending in `lock` whose first argument does (`TestEnvLock::lock(&..)`).
    lock_call: bool,
    /// The declarations the resolution leaves, by index into the declaration
    /// table.
    candidates: Vec<usize>,
    class: EdgeClass,
    /// Why an unresolved path is not followed.
    reason: Option<&'static str>,
}

impl CallTarget {
    /// The declarations this reference is an edge to: its candidates, when
    /// the resolution names crate bodies.
    fn edges(&self) -> &[usize] {
        match self.class {
            EdgeClass::PathResolved
            | EdgeClass::PathConstOrStatic
            | EdgeClass::PathAmbiguous
            | EdgeClass::PathTraitDispatch
            | EdgeClass::PathExternalAtCrateType => &self.candidates,
            _ => &[],
        }
    }
}

/// Every reference the resolution saw, by class and by qualifier class. No
/// absolute figure is asserted, because each moves with unrelated code; the
/// partition is asserted and every class is printed, so a blind spot is
/// visible instead of silent (design W4-D27 part 1).
#[derive(Debug, Default)]
struct EdgeAccounting {
    calls_walked: usize,
    path_resolved: usize,
    path_const_or_static: usize,
    path_ambiguous: usize,
    path_constructor: usize,
    path_external: usize,
    path_trait_dispatch: usize,
    path_external_at_crate_type: usize,
    path_unresolved: usize,
    method_unresolved: usize,
    env_calls: usize,
    other_callee: usize,
    local_binding: usize,
    pending: usize,
    /// How many of them were read from a macro's tokens.
    from_macros: usize,
    /// (qualifier class, class) to the number of references, so a qualifier
    /// class that resolves nothing is visible.
    by_qualifier: BTreeMap<(&'static str, &'static str), usize>,
    /// Why each unresolved path is not followed.
    by_reason: BTreeMap<&'static str, usize>,
}

impl EdgeAccounting {
    fn count(&mut self, call: &CallTarget) {
        self.calls_walked += 1;
        match call.class {
            EdgeClass::PathResolved => self.path_resolved += 1,
            EdgeClass::PathConstOrStatic => self.path_const_or_static += 1,
            EdgeClass::PathAmbiguous => self.path_ambiguous += 1,
            EdgeClass::PathConstructor => self.path_constructor += 1,
            EdgeClass::PathExternal => self.path_external += 1,
            EdgeClass::PathTraitDispatch => self.path_trait_dispatch += 1,
            EdgeClass::PathExternalAtCrateType => self.path_external_at_crate_type += 1,
            EdgeClass::PathUnresolved => self.path_unresolved += 1,
            EdgeClass::MethodUnresolved => self.method_unresolved += 1,
            EdgeClass::Environment => self.env_calls += 1,
            EdgeClass::OtherCallee => self.other_callee += 1,
            EdgeClass::LocalBinding => self.local_binding += 1,
            EdgeClass::PathPending => self.pending += 1,
        }
        if matches!(call.position, Position::MacroCall | Position::MacroValue) {
            self.from_macros += 1;
        }
        *self
            .by_qualifier
            .entry((call.qualifier.class(), call.class.name()))
            .or_default() += 1;
        if let Some(reason) = call.reason {
            *self.by_reason.entry(reason).or_default() += 1;
        }
    }

    /// The classes, which must sum to the references walked.
    fn partition(&self) -> usize {
        self.path_resolved
            + self.path_const_or_static
            + self.path_ambiguous
            + self.path_constructor
            + self.path_external
            + self.path_trait_dispatch
            + self.path_external_at_crate_type
            + self.path_unresolved
            + self.method_unresolved
            + self.env_calls
            + self.other_callee
            + self.local_binding
            + self.pending
    }
}

/// Where a shape-1 guard gets its value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GuardSource {
    /// `<the crate lock>.lock()`, with at most one accepted step after it.
    LockCall,
    /// A call of shape-2 helpers only.
    HelperCall,
}

/// Where a shape-1 guard's `let` stands in its body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Place {
    /// A statement of the body itself.
    Top,
    /// A statement of an inner block, outside every closure and async block.
    Inner,
    /// A statement inside a closure or an async block.
    Closure,
}

/// An accepted guard (shape 1, decision D-i7-envlock-1): `let <name> = <lock call>;`
/// or `let <name> = <helper call>;`, with no `mut`, type, pattern or `else`.
/// It covers the sites of its own region from the end of the `let` to `to`:
/// the end of the block that holds the `let`, or the first mention of the
/// name after it (`coverage_end`), whichever comes first.
#[derive(Debug, Clone)]
struct Guard {
    name: String,
    source: GuardSource,
    /// The acquisition the `let` keeps (the `.lock()` call or the helper
    /// call), by position in the body's references.
    acquisition: usize,
    /// The coverage, as byte offsets: `from` is the end of the `let`.
    from: usize,
    to: usize,
    /// The end of the block that holds the `let`.
    block_end: usize,
    /// Where the first mention after the binding stands, when one ends the
    /// coverage before the block does.
    mentioned_at: Option<usize>,
    /// The innermost closure or async block around the `let`.
    region: Option<(usize, usize)>,
    place: Place,
}

/// A plain `let <name> = <value>;` (`plain_let`) whose value is a call: a
/// lock call (through `.unwrap()`, `.expect(..)` and `.unwrap_or_else(..)`)
/// or any other call, by its position (decision D-i8-45). When the call is a
/// hold source (a lock call, or a call of a body that can hand the lock on)
/// and the binding is confined, the guard it made lives at most to the end
/// of the block that holds the `let`, whatever the lock path resolves to.
#[derive(Debug, Clone)]
struct HeldLet {
    acquisition: usize,
    /// The end of the `let`, and the end of the block that holds it.
    from: usize,
    block_end: usize,
    /// The name is never mentioned after the `let`, or only as the sole
    /// argument of `drop(..)`, an expression statement with `drop` out of
    /// the crate, outside every loop, closure, async block, macro and item
    /// below the block (`released_only_by_drop`): the guard is in no other
    /// binding, so the block bounds it.
    confined: bool,
}

/// Whether `call` is `drop(<one argument>)`, an expression statement, whose
/// `drop` the resolution sends out of the crate (`std::mem::drop` from the
/// prelude): it consumes its argument where it stands.
fn is_drop_statement(call: Node<'_>, source: &[u8], walk: &BodyWalk<'_, '_>) -> bool {
    call.kind() == "call_expression"
        && call
            .child_by_field_name("function")
            .is_some_and(|function| {
                function.kind() == "identifier" && text(function, source) == "drop"
            })
        && call
            .child_by_field_name("arguments")
            .is_some_and(|arguments| named_children_of(arguments).len() == 1)
        && call
            .parent()
            .is_some_and(|parent| parent.kind() == "expression_statement")
        && walk
            .call_at
            .get(&call.id())
            .is_some_and(|position| walk.calls[*position].class == EdgeClass::PathExternal)
}

/// Whether every mention of `name` in `block` after `from` is the sole
/// argument of a `drop(..)` statement (`is_drop_statement`) with no loop,
/// closure, async block, macro or item between it and `block`; true when
/// there is no mention. Any other mention may move the value to a binding
/// that outlives the block, so it leaves the binding unconfined.
fn released_only_by_drop(
    block: Node<'_>,
    name: &str,
    from: usize,
    source: &[u8],
    walk: &BodyWalk<'_, '_>,
) -> bool {
    let wanted = unraw(name);
    let items = &grammar().block_item_kinds;
    let mut stack = vec![block];
    while let Some(node) = stack.pop() {
        if node.end_byte() <= from {
            continue;
        }
        if node.child_count() > 0 {
            stack.extend(children_of(node));
            continue;
        }
        if node.start_byte() < from
            || !matches!(node.kind(), "identifier" | "shorthand_field_identifier")
            || unraw(text(node, source)) != wanted
        {
            continue;
        }
        let Some(call) = node
            .parent()
            .filter(|parent| parent.kind() == "arguments")
            .and_then(|arguments| arguments.parent())
        else {
            return false;
        };
        if node.kind() != "identifier" || !is_drop_statement(call, source, walk) {
            return false;
        }
        let mut current = call.parent();
        while let Some(here) = current {
            if here.id() == block.id() {
                break;
            }
            if ENDS_AT_ITS_START.contains(&here.kind()) || items.contains(here.kind()) {
                return false;
            }
            current = here.parent();
        }
    }
    true
}

/// The lock-call steps a `HeldLet` looks through to the lock call.
const HELD_LET_STEPS: [&str; 3] = ["unwrap", "expect", "unwrap_or_else"];

/// The body's plain `let`s whose value is a call, and its `drop(<call>)`
/// statements (decision D-i8-45), read after every reference is resolved.
fn held_lets_and_drops(
    nodes: &[Node<'_>],
    source: &[u8],
    walk: &BodyWalk<'_, '_>,
) -> (Vec<HeldLet>, BTreeSet<usize>) {
    let mut lets = Vec::new();
    let mut dropped = BTreeSet::new();
    for node in nodes.iter().copied() {
        if is_drop_statement(node, source, walk)
            && let Some(argument) = node
                .child_by_field_name("arguments")
                .and_then(|arguments| named_children_of(arguments).into_iter().next())
            && let Some(position) = walk.call_at.get(&argument.id())
        {
            dropped.insert(*position);
        }
        let Some((name, value)) = plain_let(node, source) else {
            continue;
        };
        let Some(block) = node.parent() else {
            continue;
        };
        // The lock call under at most the accepted steps, or the value itself.
        let mut acquisition = None;
        let mut current = Some(value);
        while let Some(call) = current.filter(|call| call.kind() == "call_expression") {
            if let Some(position) = walk.call_at.get(&call.id())
                && walk.calls[*position].lock_call
            {
                acquisition = Some(*position);
                break;
            }
            current = call
                .child_by_field_name("function")
                .filter(|function| function.kind() == "field_expression")
                .filter(|function| {
                    function
                        .child_by_field_name("field")
                        .is_some_and(|field| HELD_LET_STEPS.contains(&text(field, source)))
                })
                .and_then(|function| function.child_by_field_name("value"));
        }
        let Some(acquisition) = acquisition.or_else(|| walk.call_at.get(&value.id()).copied())
        else {
            continue;
        };
        let from = node.end_byte();
        lets.push(HeldLet {
            acquisition,
            from,
            block_end: block.end_byte(),
            confined: released_only_by_drop(block, &name, from, source, walk),
        });
    }
    (lets, dropped)
}

/// The branches of every `if` and `match` among `nodes`.
fn branch_groups(nodes: &[Node<'_>]) -> Vec<Vec<(usize, usize)>> {
    let span = |node: Node<'_>| (node.start_byte(), node.end_byte());
    nodes
        .iter()
        .filter_map(|node| match node.kind() {
            "if_expression" => {
                let branches: Vec<(usize, usize)> = ["consequence", "alternative"]
                    .into_iter()
                    .filter_map(|field| node.child_by_field_name(field))
                    .map(span)
                    .collect();
                (branches.len() == 2).then_some(branches)
            }
            "match_expression" => node.child_by_field_name("body").map(|body| {
                named_children_of(body)
                    .into_iter()
                    .filter(|arm| arm.kind() == "match_arm")
                    .map(span)
                    .collect()
            }),
            _ => None,
        })
        .collect()
}

/// A struct literal of a holder candidate (shape 3).
#[derive(Debug, Clone)]
struct HolderConstruction {
    /// The candidate, by index into the type table.
    ty: usize,
    /// The literal's file and node id, which the crate-wide scan of literals
    /// matches.
    file: usize,
    node: usize,
    /// Why it is not accepted, or `None` when it is: its guard field is
    /// initialised with an accepted lock call, an accepted helper call or the
    /// name of a shape-1 guard at the mention that ends it, and it has no
    /// `..base`.
    refused: Option<&'static str>,
    /// The acquisition its guard field's value makes (a lock call or a helper
    /// call written there), by position.
    acquisition: Option<usize>,
}

/// One body (a function item, a const or static initializer, or a method a
/// derive generates) and what it does.
#[derive(Debug)]
struct FunctionFacts {
    file: String,
    name: String,
    /// What the report names before `name`: the `impl` type, the trait, or
    /// the functions a nested item sits in.
    owner: Option<String>,
    kind: DeclKind,
    /// The crate types this function is the `Drop::drop` of (one, or each cfg
    /// variant of it).
    drop_of: Vec<usize>,
    /// The trait the impl it belongs to implements, when it is outside the
    /// crate (`Drop`, `Deref`, `PartialEq`), by last segment.
    external_trait: Option<String>,
    /// A test build runs it as a test (`is_test_function`, the instrument).
    is_test: bool,
    /// The item itself is test code.
    in_test_code: bool,
    /// It is a shape-2 helper.
    helper: bool,
    /// Environment calls in its own body that are test code although no test
    /// build runs the function as a test.
    test_code_calls_by_liveness_alone: usize,
    /// Every reference in its own body (for a derived method, the calls it
    /// models).
    calls: Vec<CallTarget>,
    /// Per reference, whether an accepted shape covers its site (`cover`).
    covered: Vec<bool>,
    /// Per reference, whether its site is between a guard's `let` and the end
    /// of its block, or in a holder's `drop`, in any region: where a second
    /// acquisition could run while the lock is held (`nesting_sites`).
    under_a_guard: Vec<bool>,
    /// Its shape-1 guards.
    guards: Vec<Guard>,
    /// The byte ranges of the loops, closures, async blocks and macro
    /// invocations in its body: code that can run again, so a guard an
    /// unconfined hold (`hold_starts`) leaves alive is live there before the
    /// hold's site in the text too.
    rerun_spans: Vec<(usize, usize)>,
    /// Per `if` and `match` in its body, the byte ranges of its branches (the
    /// consequence and the `else`, or the arms): one evaluation runs one of
    /// them, so a hold in one branch is not live in another, short of a loop
    /// around both (`rerun_spans`).
    branch_groups: Vec<Vec<(usize, usize)>>,
    /// Its plain `let`s whose value is a call (`HeldLet`).
    held_lets: Vec<HeldLet>,
    /// The calls written as the sole argument of `drop(..)`, an expression
    /// statement, with `drop` out of the crate: their value is dropped where
    /// it is made.
    dropped_at_once: BTreeSet<usize>,
    /// The references that take the crate lock or call a shape-2 helper, by
    /// position: the acquisitions an accepted shape must keep.
    acquisitions: Vec<usize>,
    /// The acquisitions a helper's body returns or a holder's struct literal
    /// keeps. A shape-1 guard's is read from `guards`, so it is not here.
    kept: BTreeSet<usize>,
    /// The struct literals of holder candidates it writes.
    constructions: Vec<HolderConstruction>,
    /// Sites inside a guard's range that are in another region than the
    /// guard's (a closure, an async block or a macro that does not run its
    /// tokens where they stand), which the guard does not cover.
    sites_in_another_region: Vec<String>,
    /// The `.lock()` calls on a path that does not resolve to the crate lock but
    /// whose last segment is written as the crate lock's name.
    lock_calls_named_like_the_crate_lock: usize,
    /// `.lock()` on the crate lock written in a macro's tokens, which no
    /// accepted shape keeps.
    token_lock_calls: usize,
    /// Paths in a macro's tokens written right after a `|`.
    token_paths_after_a_bar: usize,
}

impl FunctionFacts {
    /// Whether it takes the crate lock anywhere: an acquisition in its body
    /// or a `.lock()` on the crate lock in its macro tokens. Reached while the
    /// lock is held, it would wait for itself.
    fn locks(&self) -> bool {
        self.token_lock_calls > 0 || self.calls.iter().any(|call| call.lock_call)
    }

    /// Whether an environment call of its own body is not covered.
    fn reads_unlocked(&self) -> bool {
        self.calls
            .iter()
            .zip(&self.covered)
            .any(|(call, covered)| call.class == EdgeClass::Environment && !covered)
    }

    /// Whether its body holds an environment call that is test code.
    fn touches_env_in_test_code(&self) -> bool {
        self.calls
            .iter()
            .any(|call| call.class == EdgeClass::Environment && call.test_code)
    }
}

/// How a token of a macro's tokens is used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TokenUse {
    /// A path followed by a parenthesised group.
    Call,
    /// A path that names a value.
    Value,
    /// A name after a `.` followed by a parenthesised group.
    Method,
    /// A path followed by `!` and a group: a macro invocation.
    Invocation,
    /// An upper-case path followed by a braced group: a struct literal's
    /// type, which names no value. A holder's literal written in tokens
    /// names its guard field there, which keeps the type from being a holder
    /// (`holder_field_uses`).
    StructLiteral,
}

/// One path read from a macro's tokens.
struct TokenRef<'t> {
    /// The path's first token, for its position and liveness.
    at: Node<'t>,
    segs: Vec<Seg>,
    /// The path's tokens, concatenated.
    written: String,
    usage: TokenUse,
    /// For a method, the path written just before its `.`, if any.
    receiver: Option<Vec<Seg>>,
    /// The path is written right after a `|`, where a closure parameter may
    /// stand.
    after_a_bar: bool,
}

/// The tokens after which a path in a macro's tokens is a binding, an item
/// name, a type or a lifetime, not a value. `mut` is not one: the grammar
/// lexes it as a `mutable_specifier`, and after `&mut` a path is a value (`let
/// mut x =` is caught by the `=` that follows, `TOKEN_NAMING_FOLLOWERS`).
const TOKEN_NAMING_KEYWORDS: [&str; 16] = [
    "let", "ref", "fn", "struct", "enum", "union", "mod", "trait", "type", "const", "static",
    "impl", "use", "as", "for", "'",
];

/// The tokens before which a path in a macro's tokens is a field name, a named
/// argument, a binding or a match pattern, not a value.
const TOKEN_NAMING_FOLLOWERS: [&str; 3] = [":", "=", "=>"];

/// Rust's weak keywords that are also valid names (`Default::default`,
/// `union` as a function), which the grammar lexes inside a token tree as
/// keyword tokens rather than as `identifier`.
const WEAK_KEYWORDS: [&str; 5] = ["default", "union", "raw", "safe", "macro_rules"];

/// Whether a token of a token tree is a name: an `identifier`, or a weak
/// keyword (`WEAK_KEYWORDS`).
fn is_token_name(token: Node<'_>) -> bool {
    token.kind() == "identifier" || WEAK_KEYWORDS.contains(&token.kind())
}

/// One path segment of a macro token.
fn token_segment(token: Node<'_>, source: &[u8]) -> Option<Seg> {
    match token.kind() {
        _ if is_token_name(token) => Some(match text(token, source) {
            "Self" => Seg::SelfType,
            name => Seg::Name(name.trim_start_matches("r#").to_string()),
        }),
        "crate" => Some(Seg::Crate),
        "self" => Some(Seg::SelfModule),
        "super" => Some(Seg::Super),
        "metavariable" => Some(if text(token, source) == "$crate" {
            Seg::Crate
        } else {
            Seg::Opaque
        }),
        _ => None,
    }
}

/// The index after the `>` that closes the `<` at `open`.
fn skip_angles(tokens: &[Node<'_>], open: usize) -> Option<usize> {
    let mut depth: i32 = 0;
    for (index, token) in tokens.iter().enumerate().skip(open) {
        depth += match token.kind() {
            "<" => 1,
            ">" => -1,
            ">>" => -2,
            _ => 0,
        };
        if depth <= 0 {
            return (depth == 0).then_some(index + 1);
        }
    }
    None
}

/// The segments of a type written in tokens (`T`, `a::T<U>`), or `[Opaque]`.
fn token_type_segs(tokens: &[Node<'_>], source: &[u8]) -> Vec<Seg> {
    match token_path(tokens, 0, source) {
        Some((segs, _)) => segs,
        None => vec![Seg::Opaque],
    }
}

/// The path that starts at `start` in a token sequence: an optional leading
/// `::` or `<T as Trait>::`, then segments joined by `::` (`$crate` is a
/// `metavariable` token, so `token_segment` reads it), with any turbofish
/// skipped. Returns the segments and the index after the path.
fn token_path(tokens: &[Node<'_>], start: usize, source: &[u8]) -> Option<(Vec<Seg>, usize)> {
    let mut segs = Vec::new();
    let mut index = start;
    let first = *tokens.get(index)?;
    match first.kind() {
        "::" => {
            let seg = token_segment(*tokens.get(index + 1)?, source)?;
            segs.push(Seg::Global);
            segs.push(seg);
            index += 2;
        }
        "<" => {
            let after = skip_angles(tokens, index)?;
            if tokens.get(after)?.kind() != "::" {
                return None;
            }
            let inner = &tokens[index + 1..after - 1];
            let split = inner.iter().position(|token| token.kind() == "as");
            let ty = token_type_segs(&inner[..split.unwrap_or(inner.len())], source);
            let tr = split.map(|at| token_type_segs(&inner[at + 1..], source));
            segs.push(Seg::Qualified(ty, tr));
            index = after;
        }
        _ => {
            segs.push(token_segment(first, source)?);
            index += 1;
        }
    }
    while tokens.get(index).is_some_and(|token| token.kind() == "::") {
        match tokens.get(index + 1) {
            Some(next) if next.kind() == "<" => index = skip_angles(tokens, index + 1)?,
            Some(next) => match token_segment(*next, source) {
                Some(seg) => {
                    segs.push(seg);
                    index += 2;
                }
                None => break,
            },
            None => break,
        }
    }
    if matches!(segs.last(), Some(Seg::Qualified(..)) | Some(Seg::Global)) {
        return None;
    }
    Some((segs, index))
}

/// Every path in a token tree and how it is used: a path followed by a
/// parenthesised group is a call; one after `.` is a method; one followed by
/// `!` is a macro invocation; an upper-case name before a braced group is a
/// struct literal's type; one after a keyword of `TOKEN_NAMING_KEYWORDS` or
/// before a token of `TOKEN_NAMING_FOLLOWERS` is not a reference; any other
/// path is a value. Nested groups are read the same way.
fn scan_tokens<'t>(tree: Node<'t>, source: &[u8], out: &mut Vec<TokenRef<'t>>) {
    let tokens: Vec<Node<'t>> = children_of(tree)
        .into_iter()
        .filter(|token| !is_comment(*token))
        .collect();
    let opens = |token: Option<Node<'_>>, delimiter: &str| {
        token.is_some_and(|group| {
            group.kind() == "token_tree"
                && group.child(0).is_some_and(|open| open.kind() == delimiter)
        })
    };
    let mut index = 0;
    let mut previous_path: Option<(Vec<Seg>, usize)> = None;
    while index < tokens.len() {
        let token = tokens[index];
        if matches!(token.kind(), "token_tree" | "token_repetition") {
            scan_tokens(token, source, out);
            index += 1;
            continue;
        }
        let Some((segs, end)) = token_path(&tokens, index, source) else {
            index += 1;
            continue;
        };
        let receiver = previous_path
            .take()
            .filter(|(_, path_end)| index >= 1 && *path_end == index - 1)
            .map(|(receiver, _)| receiver);
        previous_path = Some((segs.clone(), end));
        let previous = index.checked_sub(1).map(|at| tokens[at].kind());
        let next = tokens.get(end).copied();
        let next_kind = next.map(|token| token.kind());
        let parenthesised = opens(next, "(");
        let upper =
            matches!(segs.last(), Some(Seg::Name(name)) if name.starts_with(char::is_uppercase));
        let usage = if previous == Some(".") {
            parenthesised.then_some(TokenUse::Method)
        } else if previous.is_some_and(|previous| TOKEN_NAMING_KEYWORDS.contains(&previous)) {
            None
        } else if next_kind == Some("!") {
            Some(TokenUse::Invocation)
        } else if parenthesised {
            Some(TokenUse::Call)
        } else if upper && opens(next, "{") {
            Some(TokenUse::StructLiteral)
        } else if next_kind.is_some_and(|next| TOKEN_NAMING_FOLLOWERS.contains(&next)) {
            None
        } else {
            Some(TokenUse::Value)
        };
        if let Some(usage) = usage {
            let written = tokens[index..end]
                .iter()
                .map(|token| text(*token, source))
                .collect::<String>();
            out.push(TokenRef {
                at: token,
                segs,
                written,
                usage,
                receiver: if usage == TokenUse::Method {
                    receiver
                } else {
                    None
                },
                after_a_bar: previous == Some("|"),
            });
        }
        index = end.max(index + 1);
    }
}

/// Whether a path node in an expression slot names a value: it is not the
/// callee of a call (a parenthesised callee included), and it is in an
/// expression slot of the grammar or is a struct literal's shorthand field.
fn is_value_reference(node: Node<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    if parent.kind() == "shorthand_field_initializer" {
        return true;
    }
    if !grammar().in_expression_slot(node) {
        return false;
    }
    let mut outer = node;
    while let Some(parent) = outer.parent()
        && parent.kind() == "parenthesized_expression"
    {
        outer = parent;
    }
    if outer.id() != node.id()
        && let Some(call) = outer.parent()
        && call.kind() == "call_expression"
        && field_of_child(call, outer) == Some("function")
    {
        return false;
    }
    true
}

/// A call's turbofish type argument: the first type in `f::<T>(..)`.
fn turbofish_type(call: Node<'_>) -> Option<Node<'_>> {
    let callee = call
        .child_by_field_name("function")
        .filter(|callee| callee.kind() == "generic_function")?;
    named_children_of(callee.child_by_field_name("type_arguments")?)
        .into_iter()
        .find(|argument| {
            !matches!(
                argument.kind(),
                "lifetime" | "line_comment" | "block_comment"
            )
        })
}

/// The type written on the `let` a value reaches: through `?`, parentheses, a
/// method chain on it, and the tail of a block, an `unsafe` block, an `if` or a
/// `match` arm.
fn written_let_type(start: Node<'_>) -> Option<Node<'_>> {
    let mut value = start;
    loop {
        let parent = value.parent()?;
        let field = field_of_child(parent, value);
        match parent.kind() {
            "let_declaration" => {
                return (field == Some("value"))
                    .then(|| parent.child_by_field_name("type"))
                    .flatten();
            }
            "try_expression"
            | "parenthesized_expression"
            | "unsafe_block"
            | "else_clause"
            | "match_block" => value = parent,
            "field_expression" if field == Some("value") => {
                value = parent.parent().filter(|call| {
                    call.kind() == "call_expression"
                        && call
                            .child_by_field_name("function")
                            .is_some_and(|function| function.id() == parent.id())
                })?;
            }
            "expression_statement" if !ends_in_a_semicolon(parent) => value = parent,
            "block" if is_tail(parent, value) => value = parent,
            "if_expression" if matches!(field, Some("consequence" | "alternative")) => {
                value = parent
            }
            "match_arm" if field == Some("value") => value = parent,
            "match_expression" if field == Some("body") => value = parent,
            _ => return None,
        }
    }
}

/// The standard library's macros that evaluate their tokens where they are
/// written, once (decision D-i7-envlock-1): a site in one of these is in the region
/// around the invocation. A site in any other macro's tokens (a dependency's,
/// a crate `macro_rules!`, which may wrap them in a closure or a loop) is in
/// a region of its own, which no guard covers (`CrateModel::runs_in_place`).
const IN_PLACE_MACROS: [&str; 21] = [
    "assert",
    "assert_eq",
    "assert_ne",
    "debug_assert",
    "debug_assert_eq",
    "debug_assert_ne",
    "format",
    "format_args",
    "print",
    "println",
    "eprint",
    "eprintln",
    "write",
    "writeln",
    "panic",
    "vec",
    "matches",
    "dbg",
    "unreachable",
    "todo",
    "unimplemented",
];

/// The crates whose macros `IN_PLACE_MACROS` names, as a path's first segment.
const STD_CRATES: [&str; 3] = ["std", "core", "alloc"];

/// The attributes a holder may carry (shape 3): none of them can generate a
/// constructor or a value of the type. A derive, an attribute macro, a
/// `cfg_attr` (which can carry either) or any attribute not named here keeps
/// a struct from being a holder.
const INERT_ATTRIBUTES: [&str; 9] = [
    "doc", "allow", "expect", "warn", "deny", "forbid", "cfg", "must_use", "repr",
];

/// The node kinds, between a guard's block and a mention of its name, that
/// make the mention end the guard where the outermost of them starts: a
/// loop runs the code before the mention again after it, and a closure, an
/// async block, a macro's tokens and an item run when they are called, which
/// may be after the mention (`Guard`, `coverage_end`).
const ENDS_AT_ITS_START: [&str; 7] = [
    "loop_expression",
    "while_expression",
    "for_expression",
    "closure_expression",
    "async_block",
    "macro_invocation",
    "macro_definition",
];

/// The accepted shapes the walk of every body needs to know before it
/// starts: the shape-2 helpers and the shape-3 holder candidates.
struct Shapes {
    /// The shape-2 helpers, by index into the declaration table.
    helpers: BTreeSet<usize>,
    /// The holder candidates, by index into the type table, with the name of
    /// their one guard field.
    candidates: BTreeMap<usize, String>,
    /// A crate root holds `#[macro_use] extern crate`, whose macros a bare
    /// name may reach, so a bare name no longer names the standard library's
    /// macro for sure: no macro then runs in place.
    macro_use_extern_crate: bool,
}

/// The crate lock's guard type, by resolution (`CrateModel::lock_guard_types`).
#[derive(Debug, Clone, PartialEq, Eq)]
enum GuardType {
    /// A crate type, by index into the type table: the real crate's
    /// `TestEnvGuard`, which `TestEnvLock::lock` returns in a `LockResult`.
    Crate(usize),
    /// A type outside the crate, by its resolved path: `std::sync::MutexGuard`
    /// for a crate lock that is a `std::sync::Mutex`.
    External(Vec<String>),
}

/// What a struct literal or a struct pattern names, resolved where it stands
/// (`CrateModel::named_type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NamedType {
    /// Exactly one crate type, through its aliases.
    Crate(usize),
    /// Exactly one type outside the crate.
    Outside,
    /// Nothing the resolution decides: unresolved, an enum variant, or more
    /// than one candidate.
    Unknown,
}

/// `std::sync::Mutex`, whose `lock` hands out `std::sync::MutexGuard`.
const STD_MUTEX: [&str; 3] = ["std", "sync", "Mutex"];
const STD_MUTEX_GUARD: [&str; 3] = ["std", "sync", "MutexGuard"];

/// The one path `unwrap_or_else` may name in a shape-1 lock call, matched as
/// the whole resolved path, never by its last segment.
const POISON_INTO_INNER: [&str; 4] = ["std", "sync", "PoisonError", "into_inner"];

/// Where each holder candidate's guard field is named across the crate
/// (`CrateModel::holder_field_uses`).
#[derive(Debug, Default)]
struct HolderUses {
    /// Per candidate, every place its guard field's name stands that is not
    /// its declaration and not the guard field of one of its own struct
    /// literals: a field access, a struct pattern, a token of a macro or of a
    /// `macro_rules!`, a literal whose type the resolution does not decide.
    mentions: BTreeMap<usize, Vec<String>>,
    /// Per candidate, every struct literal of it in the crate, as (file, node
    /// id), which the walks must have read.
    literals: BTreeMap<usize, Vec<(usize, usize)>>,
}

/// A `let` statement's name and value when it has the shape-1 form: a plain
/// name (no `mut`, `ref` or pattern), no type, no `else`, and no attribute
/// but a lint level (`LINT_ATTRIBUTES`).
fn plain_let<'t>(statement: Node<'t>, source: &[u8]) -> Option<(String, Node<'t>)> {
    if statement.kind() != "let_declaration" {
        return None;
    }
    let pattern = statement.child_by_field_name("pattern")?;
    if pattern.kind() != "identifier"
        || statement.child_by_field_name("type").is_some()
        || statement.child_by_field_name("alternative").is_some()
        || children_of(statement)
            .iter()
            .any(|child| child.kind() == "mutable_specifier")
        || preceding_attributes(statement)
            .into_iter()
            .any(|attribute| {
                !LINT_ATTRIBUTES
                    .iter()
                    .any(|lint| attribute_path_is(attribute, source, lint))
            })
    {
        return None;
    }
    Some((
        text(pattern, source).to_string(),
        statement.child_by_field_name("value")?,
    ))
}

impl<'t> CrateModel<'t> {
    /// A type node resolved where `at` stands: a generic type by its path.
    fn resolve_type_node(&self, file: usize, at: Node<'t>, type_node: Node<'t>) -> Lookup {
        let path = if type_node.kind() == "generic_type" {
            match type_node.child_by_field_name("type") {
                Some(path) => path,
                None => return Err("a generic type with no path"),
            }
        } else {
            type_node
        };
        let mut segs = Vec::new();
        type_segs(path, self.files[file].source, &mut segs);
        let site = Site {
            file,
            node: at,
            locals: &[],
            body: None,
        };
        self.resolve_path(&segs, Ns::Type, &site)
    }

    /// The guard type of each crate lock, by resolution: for a crate lock
    /// whose declared type is a crate type, what that type's own `lock`
    /// returns (the first type argument of its return type, `LockResult<G>`'s
    /// `G`, or the return type itself when it has none), resolved where `lock`
    /// stands; for a `std::sync::Mutex`, `std::sync::MutexGuard`. A crate lock
    /// of any other type has none, and then nothing can be a holder.
    fn lock_guard_types(&self) -> Vec<GuardType> {
        let mut out = Vec::new();
        for decl in self.decls.iter().filter(|decl| decl.crate_lock) {
            let Some(written) = decl.node.child_by_field_name("type") else {
                continue;
            };
            let found = match self
                .resolve_type_node(decl.file, decl.node, written)
                .as_deref()
            {
                Ok([Def::External(path)]) if path.iter().map(String::as_str).eq(STD_MUTEX) => Some(
                    GuardType::External(STD_MUTEX_GUARD.iter().map(|s| s.to_string()).collect()),
                ),
                Ok([Def::Type(t)]) => match self.alias_targets(*t, 0).as_slice() {
                    [lock_type] => self.lock_method_guard(*lock_type),
                    _ => None,
                },
                _ => None,
            };
            if let Some(found) = found
                && !out.contains(&found)
            {
                out.push(found);
            }
        }
        out
    }

    /// What a crate lock type's own `lock` returns (`lock_guard_types`).
    fn lock_method_guard(&self, t: usize) -> Option<GuardType> {
        let found = self.associated(t, "lock", None, 0);
        let Ok([Def::Decl(lock)]) = found.as_deref() else {
            return None;
        };
        let decl = &self.decls[*lock];
        let returned = decl.node.child_by_field_name("return_type")?;
        let guard = if returned.kind() == "generic_type" {
            named_children_of(returned.child_by_field_name("type_arguments")?)
                .into_iter()
                .find(|argument| !is_comment(*argument) && argument.kind() != "lifetime")?
        } else {
            returned
        };
        match self
            .resolve_type_node(decl.file, decl.node, guard)
            .as_deref()
        {
            Ok([Def::Type(g)]) => match self.alias_targets(*g, 0).as_slice() {
                [only] => Some(GuardType::Crate(*only)),
                _ => None,
            },
            Ok([Def::External(path)]) => Some(GuardType::External(path.clone())),
            _ => None,
        }
    }

    /// Whether a declared type is the crate lock's guard type: a path (its
    /// generic arguments aside) that resolves to it where `at` stands, a crate
    /// type through its aliases.
    fn is_the_guard_type(
        &self,
        file: usize,
        at: Node<'t>,
        type_node: Node<'t>,
        guards: &[GuardType],
    ) -> bool {
        if !matches!(
            type_node.kind(),
            "generic_type" | "type_identifier" | "scoped_type_identifier"
        ) {
            return false;
        }
        match self.resolve_type_node(file, at, type_node).as_deref() {
            Ok([Def::Type(t)]) => matches!(self.alias_targets(*t, 0).as_slice(),
                [only] if guards.contains(&GuardType::Crate(*only))),
            Ok([Def::External(path)]) => guards.contains(&GuardType::External(path.clone())),
            _ => false,
        }
    }

    /// Whether a type names the crate lock's guard type anywhere in it,
    /// generic arguments included (`Option<TestEnvGuard>`).
    fn names_the_guard_type(
        &self,
        file: usize,
        at: Node<'t>,
        type_node: Node<'t>,
        guards: &[GuardType],
    ) -> bool {
        type_paths(type_node)
            .into_iter()
            .any(|path| self.is_the_guard_type(file, at, path, guards))
    }

    /// A struct's fields whose declared type is the crate lock's guard type,
    /// with their declarations' nodes (a positional field by its index).
    fn guard_fields(&self, t: usize, guards: &[GuardType]) -> Vec<(String, Node<'t>)> {
        let info = &self.types[t];
        struct_fields(info.node, self.files[info.file].source)
            .into_iter()
            .filter(|(_, field_type)| {
                self.is_the_guard_type(info.file, info.node, *field_type, guards)
            })
            .collect()
    }

    /// Whether a struct with a guard field is a holder candidate (shape 3):
    /// named fields, exactly one of the guard type, no attribute on the field,
    /// and none on the struct but the inert ones (`INERT_ATTRIBUTES`), so no
    /// derive and no attribute macro builds a value of it. `Ok` with the
    /// guard field's name, or `Err` with why not; `None` for a type with no
    /// guard field.
    fn holder_candidate(
        &self,
        t: usize,
        guards: &[GuardType],
    ) -> Option<Result<String, &'static str>> {
        let info = &self.types[t];
        if info.node.kind() != "struct_item" {
            return None;
        }
        let fields = self.guard_fields(t, guards);
        let [(name, field_type)] = fields.as_slice() else {
            return (!fields.is_empty()).then_some(Err("more than one field of the guard type"));
        };
        let source = self.files[info.file].source;
        Some(
            if info
                .node
                .child_by_field_name("body")
                .is_none_or(|body| body.kind() != "field_declaration_list")
            {
                Err("a tuple struct, whose constructor can be named as a value")
            } else if preceding_attributes(info.node)
                .into_iter()
                .any(|attribute| {
                    !INERT_ATTRIBUTES
                        .iter()
                        .any(|inert| attribute_path_is(attribute, source, inert))
                })
            {
                Err("an attribute that is not inert (a derive, an attribute macro, a cfg_attr)")
            } else if field_type
                .parent()
                .is_some_and(|declaration| !preceding_attributes(declaration).is_empty())
            {
                Err("an attribute on its guard field")
            } else {
                Ok(name.clone())
            },
        )
    }

    /// The shape-2 helpers: free functions, not `async`, whose body is
    /// exactly one accepted lock call as its tail, or exactly `let <name> =
    /// <lock call>; <name>`. A helper call inside one does not count, so no
    /// fixpoint is needed.
    fn helpers(&self) -> BTreeSet<usize> {
        (0..self.decls.len())
            .filter(|index| self.is_helper(*index))
            .collect()
    }

    fn is_helper(&self, index: usize) -> bool {
        let decl = &self.decls[index];
        if decl.kind != DeclKind::Function || decl.owner != Owner::Free {
            return false;
        }
        let node = decl.node;
        let is_async = children_of(node).iter().any(|child| {
            child.kind() == "function_modifiers"
                && children_of(*child)
                    .iter()
                    .any(|modifier| modifier.kind() == "async")
        });
        let Some(body) = node.child_by_field_name("body") else {
            return false;
        };
        if is_async {
            return false;
        }
        let source = self.files[decl.file].source;
        let locals = local_bindings(node, &body_nodes(body), source);
        let statements: Vec<Node<'t>> = named_children_of(body)
            .into_iter()
            .filter(|child| !is_comment(*child))
            .collect();
        match statements.as_slice() {
            [tail] => self
                .accepted_lock_call(*tail, decl.file, &locals, Some(body))
                .is_some(),
            [statement, tail] => {
                tail.kind() == "identifier"
                    && plain_let(*statement, source).is_some_and(|(name, value)| {
                        unraw(&name) == unraw(text(*tail, source))
                            && self
                                .accepted_lock_call(value, decl.file, &locals, Some(body))
                                .is_some()
                    })
            }
            _ => false,
        }
    }

    /// `<the crate lock>.lock()`: a `.lock()` with no argument on a receiver
    /// that is a name or a path (no parentheses, no `&`, no local holding a
    /// reference) and resolves, where it stands, to the crate lock.
    fn bare_lock_call(
        &self,
        node: Node<'t>,
        file: usize,
        locals: &[(String, usize, usize)],
        body: Option<Node<'t>>,
    ) -> bool {
        if node.kind() != "call_expression"
            || !node
                .child_by_field_name("arguments")
                .is_some_and(|arguments| arguments_of(arguments).is_empty())
        {
            return false;
        }
        let source = self.files[file].source;
        let Some(function) = node
            .child_by_field_name("function")
            .filter(|function| function.kind() == "field_expression")
        else {
            return false;
        };
        if function
            .child_by_field_name("field")
            .is_none_or(|field| text(field, source) != "lock")
        {
            return false;
        }
        let Some(receiver) = function
            .child_by_field_name("value")
            .filter(|receiver| matches!(receiver.kind(), "identifier" | "scoped_identifier"))
        else {
            return false;
        };
        let site = Site {
            file,
            node: receiver,
            locals,
            body,
        };
        // Fails closed where the resolution is not decided by the parse: a
        // scope it consulted may hold a macro-made item of the name.
        let outer = self.undecided.replace(false);
        let resolved = matches!(
            self.resolve_path(&segs_of(receiver, source), Ns::Value, &site).as_deref(),
            Ok([Def::Decl(decl)]) if self.decls[*decl].crate_lock
        );
        let decided = !self.undecided.get();
        self.undecided.set(outer);
        resolved && decided
    }

    /// An accepted lock call (shape 1), with the bare `.lock()` call it holds:
    /// `bare_lock_call`, then at most one of `.unwrap()`, `.expect(<string
    /// literal>)`, `.unwrap_or_else(|p| p.into_inner())`
    /// (`is_into_inner_closure`) or `.unwrap_or_else(<a path that resolves to
    /// std::sync::PoisonError::into_inner>)`.
    fn accepted_lock_call(
        &self,
        node: Node<'t>,
        file: usize,
        locals: &[(String, usize, usize)],
        body: Option<Node<'t>>,
    ) -> Option<Node<'t>> {
        if self.bare_lock_call(node, file, locals, body) {
            return Some(node);
        }
        if node.kind() != "call_expression" {
            return None;
        }
        let function = node
            .child_by_field_name("function")
            .filter(|function| function.kind() == "field_expression")?;
        let inner = function.child_by_field_name("value")?;
        if !self.bare_lock_call(inner, file, locals, body) {
            return None;
        }
        let source = self.files[file].source;
        let method = text(function.child_by_field_name("field")?, source);
        let arguments = arguments_of(node.child_by_field_name("arguments")?);
        let accepted = match (method, arguments.as_slice()) {
            ("unwrap", []) => true,
            ("expect", [message]) => {
                matches!(message.kind(), "string_literal" | "raw_string_literal")
            }
            ("unwrap_or_else", [handler]) => {
                is_into_inner_closure(*handler, source)
                    || (matches!(handler.kind(), "identifier" | "scoped_identifier")
                        && matches!(
                            self.resolve_path(
                                &segs_of(*handler, source),
                                Ns::Value,
                                &Site {
                                    file,
                                    node: *handler,
                                    locals,
                                    body,
                                },
                            )
                            .as_deref(),
                            Ok([Def::External(path)])
                                if path.iter().map(String::as_str).eq(POISON_INTO_INNER)
                        ))
            }
            _ => false,
        };
        accepted.then_some(inner)
    }

    /// A call of shape-2 helpers only: a call whose callee is a name or a
    /// path (no parentheses, no turbofish) and resolves, where it stands, to
    /// helpers and nothing else.
    fn is_helper_call(
        &self,
        node: Node<'t>,
        file: usize,
        locals: &[(String, usize, usize)],
        body: Option<Node<'t>>,
        helpers: &BTreeSet<usize>,
    ) -> bool {
        node.kind() == "call_expression"
            && node
                .child_by_field_name("function")
                .filter(|callee| matches!(callee.kind(), "identifier" | "scoped_identifier"))
                .is_some_and(|callee| {
                    let site = Site {
                        file,
                        node: callee,
                        locals,
                        body,
                    };
                    matches!(
                        self.resolve_path(&segs_of(callee, self.files[file].source), Ns::Value, &site),
                        Ok(defs) if !defs.is_empty()
                            && defs.iter().all(|def| matches!(def, Def::Decl(decl) if helpers.contains(decl)))
                    )
                })
    }

    /// What a struct literal (`field` "name") or a struct pattern (`field`
    /// "type") names, resolved where it stands.
    fn named_type(&self, file: usize, node: Node<'t>, field: &str) -> NamedType {
        let Some(written) = node.child_by_field_name(field) else {
            return NamedType::Unknown;
        };
        let mut segs = Vec::new();
        type_segs(written, self.files[file].source, &mut segs);
        let site = Site {
            file,
            node,
            locals: &[],
            body: None,
        };
        match self.resolve_path(&segs, Ns::Type, &site).as_deref() {
            Ok([Def::Type(t)]) => match self.alias_targets(*t, 0).as_slice() {
                [only] => NamedType::Crate(*only),
                _ => NamedType::Unknown,
            },
            Ok([Def::External(_)]) => NamedType::Outside,
            _ => NamedType::Unknown,
        }
    }

    /// Where each holder candidate's guard field is named, and every struct
    /// literal of each candidate, over every file a build compiles
    /// (`HolderUses`). A name stands for a candidate's guard field when it is
    /// written as a field (a field access, a field of a struct literal or of
    /// a struct pattern) or as a token of a macro or of a `macro_rules!`;
    /// a field of a literal or a pattern whose type the resolution decides is
    /// that type's, and a field written in tokens is every candidate's.
    fn holder_field_uses(&self, candidates: &BTreeMap<usize, String>) -> HolderUses {
        let mut uses = HolderUses::default();
        for (file, info) in self.files.iter().enumerate() {
            if info.class == FileLiveness::Unreachable
                && info.test_class == FileLiveness::Unreachable
            {
                continue;
            }
            let source = info.source;
            let mut stack = vec![info.root];
            while let Some(node) = stack.pop() {
                stack.extend(children_of(node));
                if node.kind() == "struct_expression" {
                    if let NamedType::Crate(t) = self.named_type(file, node, "name")
                        && candidates.contains_key(&t)
                    {
                        uses.literals.entry(t).or_default().push((file, node.id()));
                    }
                    continue;
                }
                if node.child_count() > 0 {
                    continue;
                }
                let written = unraw(text(node, source));
                let owners: Vec<usize> = candidates
                    .iter()
                    .filter(|(_, field)| unraw(field) == written)
                    .map(|(t, _)| *t)
                    .collect();
                if owners.is_empty() {
                    continue;
                }
                let Some((named, how)) = self.field_use(file, node) else {
                    continue;
                };
                for t in owners {
                    let mention = match named {
                        Some(NamedType::Crate(other)) => other == t && how != "a struct literal",
                        Some(NamedType::Outside) => false,
                        Some(NamedType::Unknown) | None => true,
                    };
                    if mention {
                        uses.mentions.entry(t).or_default().push(format!(
                            "{}::{}: {how}",
                            info.label,
                            enclosing_functions(node, source)
                                .unwrap_or_else(|| "(item)".to_string())
                        ));
                    }
                }
            }
        }
        uses
    }

    /// How a leaf whose text is a guard field's name uses it: `None` when it
    /// is no field (a local, a path), else the type it is a field of (`None`
    /// inside when no type says, as for a field access or a token) and how it
    /// is written. A field's own declaration is no use.
    fn field_use(&self, file: usize, leaf: Node<'t>) -> Option<(Option<NamedType>, &'static str)> {
        let parent = leaf.parent()?;
        let in_tokens = || {
            let mut current = leaf.parent();
            while let Some(here) = current {
                if matches!(here.kind(), "token_tree" | "macro_definition") {
                    return true;
                }
                current = here.parent();
            }
            false
        };
        match (leaf.kind(), parent.kind()) {
            ("field_identifier", "field_declaration") => None,
            ("field_identifier", "field_initializer") => {
                let literal = parent.parent()?.parent()?;
                Some((
                    Some(self.named_type(file, literal, "name")),
                    "a struct literal",
                ))
            }
            ("identifier", "shorthand_field_initializer") => {
                let literal = parent.parent()?.parent()?;
                Some((
                    Some(self.named_type(file, literal, "name")),
                    "a struct literal",
                ))
            }
            ("field_identifier" | "shorthand_field_identifier", "field_pattern") => {
                let pattern = parent.parent()?;
                Some((
                    Some(self.named_type(file, pattern, "type")),
                    "a struct pattern",
                ))
            }
            ("field_identifier", "field_expression") => Some((None, "a field access")),
            ("field_identifier", _) => Some((None, "a field name")),
            ("identifier", _) if in_tokens() => Some((None, "a macro token")),
            _ => None,
        }
    }
}

/// The walk of one body.
struct BodyWalk<'m, 't> {
    model: &'m CrateModel<'t>,
    shapes: &'m Shapes,
    file: usize,
    source: &'t [u8],
    context: &'m FileContext<'m>,
    body: Node<'t>,
    locals: Vec<(String, usize, usize)>,
    /// A test build runs the body's function as a test.
    is_test: bool,
    calls: Vec<CallTarget>,
    /// Call node id to its position in `calls`.
    call_at: BTreeMap<usize, usize>,
    /// The byte ranges of its closures and async blocks.
    regions: Vec<(usize, usize)>,
    /// Per macro invocation node id: whether it runs its tokens in place.
    in_place: BTreeMap<usize, bool>,
    guards: Vec<Guard>,
    constructions: Vec<HolderConstruction>,
    test_code_calls_by_liveness_alone: usize,
    lock_calls_named_like_the_crate_lock: usize,
    token_lock_calls: usize,
    token_paths_after_a_bar: usize,
}

impl<'t> BodyWalk<'_, 't> {
    fn site(&self, node: Node<'t>) -> Site<'_, 't> {
        Site {
            file: self.file,
            node,
            locals: &self.locals,
            body: Some(self.body),
        }
    }

    /// Whether an environment call at `at` (in this body's file) is test code.
    fn is_test_code(&mut self, at: Node<'t>) -> bool {
        let test_code = self.context.is_test_code(at, self.source, self.is_test);
        if test_code && !self.is_test {
            self.test_code_calls_by_liveness_alone += 1;
        }
        test_code
    }

    /// The class, the candidates and the reason of a reference, from its
    /// resolution; an environment accessor is an environment call at
    /// `env_at`, test code or not.
    fn settle(&mut self, call: &mut CallTarget, lookup: Lookup, written: &str, env_at: Node<'t>) {
        let (class, candidates, reason) = self.model.classify(lookup, written);
        call.class = class;
        call.candidates = candidates;
        call.reason = reason;
        if class == EdgeClass::Environment {
            call.test_code = self.is_test_code(env_at);
        }
    }

    /// Resolves a path call the classification left pending.
    fn resolve_call(&mut self, call: &mut CallTarget, segs: &[Seg], at: Node<'t>, written: &str) {
        if !matches!(call.class, EdgeClass::PathPending) {
            return;
        }
        let lookup = match &call.qualifier {
            Qualifier::Method | Qualifier::Other => return,
            _ => self.model.resolve_path(segs, Ns::Value, &self.site(at)),
        };
        self.settle(call, lookup, written, at);
    }

    /// The crate bodies code outside the crate can run when a call out of the
    /// crate produces a value of a crate type the syntax writes: the call's
    /// turbofish type argument (`from_str::<T>(..)`), or the type of the `let`
    /// its value reaches (`written_let_type`), every crate type the written
    /// type names counted, generic arguments included (`Vec<T>`,
    /// `Option<T>`). Such code reaches those types only through their trait
    /// impls, so the bodies are their derived methods and the items of their
    /// impls of traits outside the crate. `None` when no crate type is
    /// written or none has such a body.
    fn external_bodies(&self, call: Node<'t>) -> Option<Vec<usize>> {
        let model = self.model;
        let type_node = turbofish_type(call).or_else(|| written_let_type(call))?;
        let mut out = Vec::new();
        for path in type_paths(type_node) {
            let mut segs = Vec::new();
            type_segs(path, self.source, &mut segs);
            for def in model
                .resolve_path(&segs, Ns::Type, &self.site(path))
                .unwrap_or_default()
            {
                if let Def::Type(t) = def {
                    out.extend(model.bodies_outside_code_can_run(t));
                }
            }
        }
        out.sort_unstable();
        out.dedup();
        (!out.is_empty()).then_some(out)
    }

    /// The bodies a call through an external trait's path can run, narrowed by
    /// the type the syntax expects where the call stands: a struct literal's
    /// field (its declared type) or base (the literal's type), or a `let` with
    /// a type. A crate type narrows the call to that type's method under the
    /// trait; a type outside the crate leaves no crate body, except that
    /// `Default::default` of one goes on into the crate types it builds a
    /// default of (`default_calls`). `None` when no type is written there,
    /// which keeps every candidate.
    fn narrow_dispatch(
        &self,
        call: Node<'t>,
        trait_name: &str,
        method: &str,
    ) -> Option<Vec<usize>> {
        let parent = call.parent()?;
        let model = self.model;
        if trait_name == "Default" && method == "default" {
            let written: Option<(usize, Node<'t>)> = match parent.kind() {
                "field_initializer" if field_of_child(parent, call) == Some("value") => {
                    let literal = parent.parent()?.parent()?;
                    let field = name_text(parent.child_by_field_name("field")?, self.source);
                    let mut segs = Vec::new();
                    type_segs(literal.child_by_field_name("name")?, self.source, &mut segs);
                    match model
                        .resolve_path(&segs, Ns::Type, &self.site(literal))
                        .ok()?
                        .as_slice()
                    {
                        [Def::Type(t)] => {
                            let info = &model.types[*t];
                            field_type_node(info.node, &field, model.files[info.file].source)
                                .map(|field_type| (info.file, field_type))
                        }
                        _ => None,
                    }
                }
                "let_declaration" if field_of_child(parent, call) == Some("value") => parent
                    .child_by_field_name("type")
                    .map(|written| (self.file, written)),
                _ => None,
            };
            if let Some((file, written)) = written {
                let mut references = Vec::new();
                default_calls(model, file, written, &mut references);
                let mut out: Vec<usize> = references
                    .iter()
                    .flat_map(|reference| reference.candidates.iter().copied())
                    .collect();
                out.sort_unstable();
                out.dedup();
                return Some(out);
            }
        }
        let types_of = |segs: &[Seg], site: &Site<'_, 't>| -> Option<Vec<Def>> {
            model.resolve_path(segs, Ns::Type, site).ok()
        };
        let expected: Vec<Def> = match parent.kind() {
            "field_initializer" if field_of_child(parent, call) == Some("value") => {
                let literal = parent.parent()?.parent()?;
                let field = name_text(parent.child_by_field_name("field")?, self.source);
                let mut segs = Vec::new();
                type_segs(literal.child_by_field_name("name")?, self.source, &mut segs);
                let mut out = Vec::new();
                for def in types_of(&segs, &self.site(literal))? {
                    match def {
                        Def::Type(t) => {
                            let info = &model.types[t];
                            let field_type =
                                field_type_node(info.node, &field, model.files[info.file].source)?;
                            let mut field_segs = Vec::new();
                            type_segs(field_type, model.files[info.file].source, &mut field_segs);
                            if field_segs.as_slice() == [Seg::Opaque] {
                                continue;
                            }
                            let site = Site {
                                file: info.file,
                                node: field_type,
                                locals: &[],
                                body: None,
                            };
                            out.extend(types_of(&field_segs, &site)?);
                        }
                        other => out.push(other),
                    }
                }
                out
            }
            "base_field_initializer" => {
                let literal = parent.parent()?.parent()?;
                let mut segs = Vec::new();
                type_segs(literal.child_by_field_name("name")?, self.source, &mut segs);
                types_of(&segs, &self.site(literal))?
            }
            "let_declaration" if field_of_child(parent, call) == Some("value") => {
                let mut segs = Vec::new();
                type_segs(parent.child_by_field_name("type")?, self.source, &mut segs);
                if segs.as_slice() == [Seg::Opaque] {
                    return Some(Vec::new());
                }
                types_of(&segs, &self.site(parent))?
            }
            _ => return None,
        };
        let filter = TraitRef::External(vec![trait_name.to_string()]);
        let mut out = Vec::new();
        for def in expected {
            match def {
                Def::Type(t) => {
                    for found in model
                        .associated(t, method, Some(&filter), 0)
                        .unwrap_or_default()
                    {
                        if let Def::Decl(decl) = found {
                            out.push(decl);
                        }
                    }
                }
                Def::External(_) => {}
                _ => return None,
            }
        }
        Some(out)
    }

    /// Whether a lock receiver is a path that resolves to the crate lock,
    /// through parentheses, `&` and `*`, and the receiver's last written
    /// segment. This decides what counts as taking the lock (the nesting and
    /// refused figures); a shape-1 guard needs the plain path
    /// (`CrateModel::bare_lock_call`).
    fn lock_receiver(&self, receiver: Node<'t>) -> (bool, Option<String>) {
        let mut node = receiver;
        loop {
            node = match node.kind() {
                "parenthesized_expression" | "unary_expression"
                    if node.kind() == "parenthesized_expression"
                        || children_of(node).iter().any(|child| child.kind() == "*") =>
                {
                    match named_children_of(node)
                        .into_iter()
                        .find(|c| !is_comment(*c))
                    {
                        Some(inner) => inner,
                        None => return (false, None),
                    }
                }
                "reference_expression" => match node.child_by_field_name("value") {
                    Some(inner) => inner,
                    None => return (false, None),
                },
                _ => break,
            };
        }
        if !matches!(node.kind(), "identifier" | "scoped_identifier") {
            return (false, None);
        }
        let segs = segs_of(node, self.source);
        let last = match segs.last() {
            Some(Seg::Name(name)) => Some(name.clone()),
            _ => None,
        };
        let is_lock = matches!(
            self.model.resolve_path(&segs, Ns::Value, &self.site(node)).as_deref(),
            Ok([Def::Decl(decl)]) if self.model.decls[*decl].crate_lock
        );
        (is_lock, last)
    }

    fn call(&mut self, node: Node<'t>) {
        // `f::<T>()` names the callee inside a `generic_function`, and
        // `(crate::f)()` inside parentheses; `(s.f)()` calls a field's value,
        // which is another callee, not a method.
        let mut callee = node.child_by_field_name("function");
        while let Some(inner) = callee {
            callee = match inner.kind() {
                "generic_function" => inner.child_by_field_name("function"),
                "parenthesized_expression" => {
                    let path = named_children_of(inner)
                        .into_iter()
                        .find(|child| !is_comment(*child))
                        .filter(|child| {
                            matches!(
                                child.kind(),
                                "identifier"
                                    | "scoped_identifier"
                                    | "generic_function"
                                    | "parenthesized_expression"
                            )
                        });
                    if path.is_none() {
                        callee = None;
                        break;
                    }
                    path
                }
                _ => break,
            };
        }
        let mut lock_call = false;
        let (name, qualifier, segs) = match callee.map(|callee| (callee, callee.kind())) {
            Some((callee, "identifier" | "scoped_identifier")) => {
                let segs = segs_of(callee, self.source);
                let name = match segs.last() {
                    Some(Seg::Name(name)) => name.clone(),
                    _ => compact(callee, self.source),
                };
                // `TestEnvLock::lock(&TEST_ENV_LOCK)`: a path call ending in
                // `lock` whose first argument is the crate lock takes it.
                if name == "lock"
                    && let Some(first) = node
                        .child_by_field_name("arguments")
                        .map(arguments_of)
                        .and_then(|arguments| arguments.first().copied())
                {
                    let (is_lock, last) = self.lock_receiver(first);
                    lock_call = is_lock;
                    if !is_lock && last.as_deref() == Some(CRATE_LOCK) {
                        self.lock_calls_named_like_the_crate_lock += 1;
                    }
                }
                (name, qualifier_of(&segs), segs)
            }
            Some((callee, "field_expression")) => {
                let method = callee
                    .child_by_field_name("field")
                    .map(|field| text(field, self.source).to_string())
                    .unwrap_or_default();
                if method == "lock"
                    && let Some(receiver) = callee.child_by_field_name("value")
                {
                    let (is_lock, last) = self.lock_receiver(receiver);
                    lock_call = is_lock;
                    if !is_lock && last.as_deref() == Some(CRATE_LOCK) {
                        self.lock_calls_named_like_the_crate_lock += 1;
                    }
                }
                (method, Qualifier::Method, Vec::new())
            }
            _ => (String::new(), Qualifier::Other, Vec::new()),
        };
        let class = match &qualifier {
            Qualifier::Method => EdgeClass::MethodUnresolved,
            Qualifier::Other => EdgeClass::OtherCallee,
            _ => EdgeClass::PathPending,
        };
        let region = self.opaque_region(node);
        let mut call = CallTarget {
            name,
            qualifier,
            position: Position::Call,
            site: node.start_byte(),
            region,
            test_code: false,
            lock_call,
            candidates: Vec::new(),
            class,
            reason: None,
        };
        if let Some(callee) = callee {
            let written = compact(callee, self.source);
            self.resolve_call(&mut call, &segs, callee, &written);
            let dispatched = call.class == EdgeClass::PathTraitDispatch;
            if dispatched
                && let [.., Seg::Name(owner), Seg::Name(method)] = segs.as_slice()
                && let Some(narrowed) = self.narrow_dispatch(node, owner, method)
            {
                call.class = match narrowed.len() {
                    0 => EdgeClass::PathExternal,
                    1 => EdgeClass::PathResolved,
                    _ => EdgeClass::PathAmbiguous,
                };
                call.candidates = narrowed;
            }
            // A call through an external trait narrowed to no crate body stays
            // out of the crate: the type written is where its value goes, and
            // it says which body runs.
            if !dispatched
                && call.class == EdgeClass::PathExternal
                && let Some(bodies) = self.external_bodies(node)
            {
                call.class = EdgeClass::PathExternalAtCrateType;
                call.candidates = bodies;
            }
        }
        self.call_at.insert(node.id(), self.calls.len());
        self.calls.push(call);
    }

    /// A path naming a value.
    fn value(&mut self, node: Node<'t>) {
        let segs = segs_of(node, self.source);
        let name = match segs.last() {
            Some(Seg::Name(name)) => name.clone(),
            _ => compact(node, self.source),
        };
        let lookup = self.model.resolve_path(&segs, Ns::Value, &self.site(node));
        let region = self.opaque_region(node);
        let mut reference = CallTarget {
            name,
            qualifier: qualifier_of(&segs),
            position: Position::Value,
            site: node.start_byte(),
            region,
            test_code: false,
            lock_call: false,
            candidates: Vec::new(),
            class: EdgeClass::PathPending,
            reason: None,
        };
        let written = compact(node, self.source);
        self.settle(&mut reference, lookup, &written, node);
        self.calls.push(reference);
    }

    /// A macro invocation: its own tokens are read where they stand, with the
    /// body's locals; a crate `macro_rules!` it names is expanded in place.
    fn invocation(&mut self, node: Node<'t>) {
        if let Some(name) = node.child_by_field_name("macro") {
            let segs = segs_of(name, self.source);
            if let Ok(defs) = self.model.resolve_path(&segs, Ns::Macro, &self.site(name))
                && let [Def::Macro(index)] = defs.as_slice()
            {
                let mut visiting = BTreeSet::new();
                self.expand(*index, node, &mut visiting);
            }
        }
        for tree in named_children_of(node)
            .into_iter()
            .filter(|child| child.kind() == "token_tree")
        {
            let mut tokens = Vec::new();
            scan_tokens(tree, self.source, &mut tokens);
            let mut visiting = BTreeSet::new();
            for token in tokens {
                self.token_reference(token, None, node, &mut visiting);
            }
        }
    }

    /// Expands a crate `macro_rules!` at `invocation`: every reference in its
    /// rules' transcribers is resolved at the invocation, as rustc resolves an
    /// item path a `macro_rules!` expands to (at the call site), without the
    /// call site's local bindings (a local name in a transcriber resolves
    /// where the macro is defined, which the model does not read), and
    /// belongs to this body; an environment call in one is an environment
    /// call at the invocation.
    fn expand(&mut self, index: usize, invocation: Node<'t>, visiting: &mut BTreeSet<usize>) {
        if !visiting.insert(index) {
            return;
        }
        let info = &self.model.macros[index];
        let (file, node) = (info.file, info.node);
        let source = self.model.files[file].source;
        let mut tokens = Vec::new();
        for rule in named_children_of(node)
            .into_iter()
            .filter(|rule| rule.kind() == "macro_rule")
        {
            if let Some(right) = rule.child_by_field_name("right") {
                scan_tokens(right, source, &mut tokens);
            }
        }
        for token in tokens {
            self.token_reference(token, Some(source), invocation, visiting);
        }
        visiting.remove(&index);
    }

    /// One path of a macro's tokens: `transcriber` is the source of an
    /// expanded `macro_rules!` (resolved at the invocation, with no locals),
    /// or `None` for an invocation's own tokens (resolved where they stand,
    /// with the body's locals). A reference a crate `macro_rules!` expands to
    /// is in the invocation's own region, which no guard covers: the
    /// transcriber may put it in a closure or a loop.
    fn token_reference(
        &mut self,
        token: TokenRef<'t>,
        transcriber: Option<&[u8]>,
        invocation: Node<'t>,
        visiting: &mut BTreeSet<usize>,
    ) {
        let name = match token.segs.last() {
            Some(Seg::Name(name)) => name.clone(),
            _ => token.written.clone(),
        };
        let expanded = transcriber.is_some();
        let resolve = |walk: &Self, segs: &[Seg], ns: Ns| {
            if expanded {
                walk.model.resolve_path(
                    segs,
                    ns,
                    &Site {
                        file: walk.file,
                        node: invocation,
                        locals: &[],
                        body: Some(walk.body),
                    },
                )
            } else {
                walk.model.resolve_path(segs, ns, &walk.site(token.at))
            }
        };
        if token.after_a_bar {
            self.token_paths_after_a_bar += 1;
        }
        let (site, region) = if expanded {
            (
                invocation.start_byte(),
                Some((invocation.start_byte(), invocation.end_byte())),
            )
        } else {
            (token.at.start_byte(), self.opaque_region(token.at))
        };
        if token.usage == TokenUse::Method
            && name == "lock"
            && let Some(receiver) = &token.receiver
            && matches!(resolve(self, receiver, Ns::Value).as_deref(),
                Ok([Def::Decl(decl)]) if self.model.decls[*decl].crate_lock)
        {
            self.token_lock_calls += 1;
        }
        match token.usage {
            TokenUse::Method => self.calls.push(CallTarget {
                name,
                qualifier: Qualifier::Method,
                position: Position::MacroCall,
                site,
                region,
                test_code: false,
                lock_call: false,
                candidates: Vec::new(),
                class: EdgeClass::MethodUnresolved,
                reason: None,
            }),
            TokenUse::Invocation => {
                if let Ok(defs) = resolve(self, &token.segs, Ns::Macro)
                    && let [Def::Macro(index)] = defs.as_slice()
                {
                    self.expand(*index, invocation, visiting);
                }
            }
            TokenUse::StructLiteral => {}
            TokenUse::Call | TokenUse::Value => {
                let lookup = resolve(self, &token.segs, Ns::Value);
                let mut reference = CallTarget {
                    name,
                    qualifier: qualifier_of(&token.segs),
                    position: if token.usage == TokenUse::Call {
                        Position::MacroCall
                    } else {
                        Position::MacroValue
                    },
                    site,
                    region,
                    test_code: false,
                    lock_call: false,
                    candidates: Vec::new(),
                    class: EdgeClass::PathPending,
                    reason: None,
                };
                let env_at = if expanded { invocation } else { token.at };
                self.settle(&mut reference, lookup, &token.written, env_at);
                self.calls.push(reference);
            }
        }
    }

    /// The innermost closure or async block holding `offset`.
    fn region_of(&self, offset: usize) -> Option<(usize, usize)> {
        self.regions
            .iter()
            .filter(|(start, end)| *start <= offset && offset < *end)
            .min_by_key(|(start, end)| end - start)
            .copied()
    }

    /// The region a node's code runs in (decision D-i7-envlock-1): the innermost
    /// closure, async block, or macro invocation that does not run its tokens
    /// in place (`runs_in_place`), between the node and the body, by its byte
    /// range; `None` for the body's own straight-line code. A guard covers
    /// only the sites of its own region, because code in a closure, an async
    /// block or such a macro may run after the guard is gone.
    fn opaque_region(&mut self, node: Node<'t>) -> Option<(usize, usize)> {
        let mut current = node.parent();
        while let Some(here) = current {
            if here.id() == self.body.id() {
                return None;
            }
            let opaque = match here.kind() {
                "closure_expression" | "async_block" => true,
                "macro_invocation" => !self.runs_in_place(here),
                _ => false,
            };
            if opaque {
                return Some((here.start_byte(), here.end_byte()));
            }
            current = here.parent();
        }
        None
    }

    /// Whether a macro invocation runs its tokens where they stand, once: it
    /// names one of `IN_PLACE_MACROS`, resolved out of the crate by a bare
    /// name or a path from `std`, `core` or `alloc`, and no crate root holds
    /// a `#[macro_use] extern crate`.
    fn runs_in_place(&mut self, invocation: Node<'t>) -> bool {
        if let Some(known) = self.in_place.get(&invocation.id()) {
            return *known;
        }
        let answer = !self.shapes.macro_use_extern_crate
            && invocation.child_by_field_name("macro").is_some_and(|name| {
                let segs = segs_of(name, self.source);
                match self
                    .model
                    .resolve_path(&segs, Ns::Macro, &self.site(name))
                    .as_deref()
                {
                    Ok([Def::External(path)]) => match path.as_slice() {
                        [only] => IN_PLACE_MACROS.contains(&only.as_str()),
                        [krate, only] => {
                            STD_CRATES.contains(&krate.as_str())
                                && IN_PLACE_MACROS.contains(&only.as_str())
                        }
                        _ => false,
                    },
                    _ => false,
                }
            });
        self.in_place.insert(invocation.id(), answer);
        answer
    }

    /// What a `let` or a holder's guard field is given, when it is accepted:
    /// an accepted lock call (`CrateModel::accepted_lock_call`) or a call of
    /// shape-2 helpers, with the node of the acquisition it makes (the
    /// `.lock()` call, or the helper call).
    fn accepted_value(&self, value: Node<'t>) -> Option<(GuardSource, Node<'t>)> {
        if let Some(lock) =
            self.model
                .accepted_lock_call(value, self.file, &self.locals, Some(self.body))
        {
            return Some((GuardSource::LockCall, lock));
        }
        self.model
            .is_helper_call(
                value,
                self.file,
                &self.locals,
                Some(self.body),
                &self.shapes.helpers,
            )
            .then_some((GuardSource::HelperCall, value))
    }

    /// A shape-1 guard, when `statement` is one: a `let` of the plain form
    /// (`plain_let`) whose value is accepted (`accepted_value`).
    fn guard(&self, statement: Node<'t>) -> Option<Guard> {
        let (name, value) = plain_let(statement, self.source)?;
        let (source, acquisition) = self.accepted_value(value)?;
        let acquisition = *self.call_at.get(&acquisition.id())?;
        let block = statement.parent()?;
        let from = statement.end_byte();
        let (to, mentioned_at) = coverage_end(block, &name, from, self.source);
        let region = self.region_of(statement.start_byte());
        let place = if region.is_some() {
            Place::Closure
        } else if block.id() == self.body.id() {
            Place::Top
        } else {
            Place::Inner
        };
        Some(Guard {
            name,
            source,
            acquisition,
            from,
            to,
            block_end: block.end_byte(),
            mentioned_at,
            region,
            place,
        })
    }

    /// The body's shape-1 guards, then its struct literals of holder
    /// candidates, read after every reference is resolved.
    fn shapes(&mut self, nodes: &[Node<'t>]) {
        for node in nodes.iter().copied() {
            if node.kind() == "let_declaration"
                && let Some(guard) = self.guard(node)
            {
                self.guards.push(guard);
            }
        }
        self.guards.sort_by_key(|guard| guard.from);
        for node in nodes.iter().copied() {
            if node.kind() == "struct_expression" {
                self.struct_literal(node);
            }
        }
    }

    /// Whether `name` (an identifier node) is the name of a shape-1 guard of
    /// this body at the mention that ends it: the guard's coverage ends right
    /// there, not earlier and not at a loop, closure, async block or macro
    /// around it, and in the guard's own region.
    fn ends_a_guard(&self, name: Node<'t>) -> bool {
        let found =
            self.model
                .resolve_path(&segs_of(name, self.source), Ns::Value, &self.site(name));
        let Ok([Def::Local(local)]) = found.as_deref() else {
            return false;
        };
        let (bound, from, _) = &self.locals[*local];
        let at = name.start_byte();
        self.guards.iter().any(|guard| {
            guard.name == *bound
                && guard.from == *from
                && guard.mentioned_at == Some(at)
                && guard.to == at
                && guard.region == self.region_of(at)
        })
    }

    /// A struct literal of a holder candidate (shape 3), and whether it is
    /// accepted: no `..base`, and its guard field, with no attribute on its
    /// initializer, given an accepted value (`accepted_value`) or the name of
    /// a shape-1 guard at the mention that ends it (`ends_a_guard`).
    fn struct_literal(&mut self, node: Node<'t>) {
        let NamedType::Crate(ty) = self.model.named_type(self.file, node, "name") else {
            return;
        };
        let Some(field) = self.shapes.candidates.get(&ty) else {
            return;
        };
        let initializers: Vec<Node<'t>> = node
            .child_by_field_name("body")
            .map(named_children_of)
            .unwrap_or_default();
        let mut refused = None;
        let mut acquisition = None;
        if initializers
            .iter()
            .any(|initializer| initializer.kind() == "base_field_initializer")
        {
            refused = Some("it has a ..base");
        }
        let guard_field =
            initializers
                .iter()
                .copied()
                .find(|initializer| match initializer.kind() {
                    "field_initializer" => initializer
                        .child_by_field_name("field")
                        .is_some_and(|name| text(name, self.source) == field),
                    "shorthand_field_initializer" => {
                        named_children_of(*initializer).into_iter().any(|name| {
                            name.kind() == "identifier" && text(name, self.source) == field
                        })
                    }
                    _ => false,
                });
        match guard_field {
            None => {
                refused.get_or_insert("its guard field is not written");
            }
            Some(initializer)
                if children_of(initializer)
                    .iter()
                    .any(|child| child.kind() == "attribute_item") =>
            {
                refused.get_or_insert("its guard field's initializer carries an attribute");
            }
            Some(initializer) => {
                let value = match initializer.kind() {
                    "field_initializer" => initializer.child_by_field_name("value"),
                    _ => named_children_of(initializer)
                        .into_iter()
                        .find(|name| name.kind() == "identifier"),
                };
                match value {
                    Some(value) => {
                        if let Some((_, made)) = self.accepted_value(value) {
                            acquisition = self.call_at.get(&made.id()).copied();
                        } else if !(value.kind() == "identifier" && self.ends_a_guard(value)) {
                            refused.get_or_insert(
                                "its guard field is given no accepted lock call, helper call or \
                                 guard",
                            );
                        }
                    }
                    None => {
                        refused.get_or_insert("its guard field has no value");
                    }
                }
            }
        }
        self.constructions.push(HolderConstruction {
            ty,
            file: self.file,
            node: node.id(),
            refused,
            acquisition,
        });
    }
}

/// The attributes a shape-1 guard's `let` may carry: lint levels only. A
/// `cfg` could leave the guard out of the build that keeps the sites it
/// covers.
const LINT_ATTRIBUTES: [&str; 5] = ["allow", "expect", "warn", "deny", "forbid"];

/// A name as Rust reads it: `r#name` and `name` are one identifier.
fn unraw(name: &str) -> &str {
    name.strip_prefix("r#").unwrap_or(name)
}

/// Where a guard bound at `from` in `block` stops covering (`Guard`): the
/// end of the block, or the first mention of its name after `from` (an
/// identifier, a struct pattern's shorthand field, a token of a macro or of
/// a `macro_rules!` written there, a name in an item written there), moved
/// back to the start of the outermost loop, closure, async block, macro
/// invocation, macro definition or item between the block and the mention
/// (`ENDS_AT_ITS_START`), whichever is earliest. The second value is where
/// the mention that ends it is written, when one does.
fn coverage_end(block: Node<'_>, name: &str, from: usize, source: &[u8]) -> (usize, Option<usize>) {
    let wanted = unraw(name);
    let items = &grammar().block_item_kinds;
    let mut end = block.end_byte();
    let mut mentioned_at = None;
    let mut stack = vec![block];
    while let Some(node) = stack.pop() {
        if node.end_byte() <= from {
            continue;
        }
        if node.child_count() > 0 {
            stack.extend(children_of(node));
            continue;
        }
        if node.start_byte() < from
            || !matches!(node.kind(), "identifier" | "shorthand_field_identifier")
            || unraw(text(node, source)) != wanted
        {
            continue;
        }
        let mut at = node.start_byte();
        let mut current = node.parent();
        while let Some(here) = current {
            if here.id() == block.id() {
                break;
            }
            if ENDS_AT_ITS_START.contains(&here.kind()) || items.contains(here.kind()) {
                at = here.start_byte();
            }
            current = here.parent();
        }
        if at < end {
            end = at;
            mentioned_at = Some(node.start_byte());
        }
    }
    (end, mentioned_at)
}

/// The references of a method a derive generates (`DERIVED_METHODS`): for a
/// std or serde derive, the same method, under the derived trait, of every
/// crate type the type's fields name (`default` through `default_calls`),
/// and for `deserialize` what the `#[serde(..)]` attributes name
/// (`serde_calls`); for a clap derive, `clap_calls`.
fn derived_calls(model: &CrateModel<'_>, t: usize, method: &str) -> Vec<CallTarget> {
    let info = &model.types[t];
    let Some((trait_name, _)) = info.derived.get(method) else {
        return Vec::new();
    };
    if matches!(
        trait_name.as_str(),
        "Parser" | "CommandFactory" | "Args" | "Subcommand"
    ) && CLAP_DERIVES
        .iter()
        .any(|derive| info.derives.contains(*derive))
    {
        return clap_calls(model, t);
    }
    let filter = TraitRef::External(vec![trait_name.clone()]);
    let mut out = Vec::new();
    if method == "default" {
        for field_type in field_types(info.node) {
            default_calls(model, info.file, field_type, &mut out);
        }
    } else {
        for path in field_type_paths(info.node) {
            type_method_calls(model, info.file, path, method, &filter, &mut out);
        }
    }
    if trait_name == "Deserialize" {
        serde_calls(model, t, &mut out);
    }
    out
}

/// `Default::default` of a field's declared type: the type's own `default`
/// when it is a crate type; through a std type whose default builds its
/// parameter's (`DEFAULT_DELEGATING`), a tuple or an array, the default of
/// what it holds; any other type outside the crate (`Option<T>`, `Vec<T>`)
/// holds no default of its parameter, and when that parameter names a crate
/// type the field is counted (`CrateModel::defaults_not_followed`).
fn default_calls(
    model: &CrateModel<'_>,
    file: usize,
    field_type: Node<'_>,
    out: &mut Vec<CallTarget>,
) {
    let source = model.files[file].source;
    let filter = TraitRef::External(vec!["Default".to_string()]);
    match field_type.kind() {
        "tuple_type" | "array_type" => {
            for element in named_children_of(field_type) {
                if !is_comment(element) && element.kind() != "integer_literal" {
                    default_calls(model, file, element, out);
                }
            }
        }
        "type_identifier" | "scoped_type_identifier" | "generic_type" => {
            let mut segs = Vec::new();
            type_segs(field_type, source, &mut segs);
            let site = Site {
                file,
                node: field_type,
                locals: &[],
                body: None,
            };
            let defs = model
                .resolve_path(&segs, Ns::Type, &site)
                .unwrap_or_default();
            if defs.iter().any(|def| matches!(def, Def::Type(_))) {
                type_method_calls(model, file, field_type, "default", &filter, out);
                return;
            }
            let arguments: Vec<Node<'_>> = field_type
                .child_by_field_name("type_arguments")
                .map(named_children_of)
                .unwrap_or_default()
                .into_iter()
                .filter(|argument| !is_comment(*argument) && argument.kind() != "lifetime")
                .collect();
            let delegating = matches!(segs.last(), Some(Seg::Name(last)) if DEFAULT_DELEGATING.contains(&last.as_str()));
            if delegating {
                for argument in arguments {
                    default_calls(model, file, argument, out);
                }
            } else if arguments.iter().any(|argument| {
                type_paths(*argument).into_iter().any(|path| {
                    let mut segs = Vec::new();
                    type_segs(path, source, &mut segs);
                    model
                        .resolve_path(&segs, Ns::Type, &site)
                        .unwrap_or_default()
                        .iter()
                        .any(|def| matches!(def, Def::Type(_)))
                })
            }) {
                model
                    .defaults_not_followed
                    .set(model.defaults_not_followed.get() + 1);
            }
        }
        _ => {}
    }
}

/// The derives clap generates build the command, so each of them reads the
/// environment when a field of the type (or of one of its variants) carries a
/// clap `env` attribute (`clap_env_fields`, one synthetic environment call),
/// and calls, of every crate type a subcommand or flattened field or a
/// variant names (`clap_children`, a type alias followed to the type it
/// names), each of `augment_args` and `augment_subcommands` it has, derived
/// or written in a hand-written impl. Clap calls the one the field's
/// attribute asks for; the parse does not tell them apart here, so a type
/// with both is followed to both. Every type path a child's declared type
/// holds is a reference of the derived body, so none is dropped: a crate
/// type with neither method is an unresolved reference, printed with its
/// reason (an impl on the trait's defaults, which the parse does not hold,
/// or no impl), as is a path the resolution leaves unresolved; a type
/// outside the crate is a reference out of the crate.
fn clap_calls(model: &CrateModel<'_>, t: usize) -> Vec<CallTarget> {
    let info = &model.types[t];
    let source = model.files[info.file].source;
    let mut out = Vec::new();
    if !clap_env_fields(info.node, source).is_empty() {
        out.push(CallTarget {
            name: "env".to_string(),
            qualifier: Qualifier::Path,
            position: Position::Call,
            site: 0,
            region: None,
            test_code: false,
            lock_call: false,
            candidates: Vec::new(),
            class: EdgeClass::Environment,
            reason: None,
        });
    }
    let site = Site {
        file: info.file,
        node: info.node,
        locals: &[],
        body: None,
    };
    let unresolved = |name: String, reason: &'static str| CallTarget {
        name,
        qualifier: Qualifier::Path,
        position: Position::Call,
        site: 0,
        region: None,
        test_code: false,
        lock_call: false,
        candidates: Vec::new(),
        class: EdgeClass::PathUnresolved,
        reason: Some(reason),
    };
    for field_type in clap_children(info.node, source) {
        for path in type_paths(field_type) {
            let mut segs = Vec::new();
            type_segs(path, source, &mut segs);
            let written = compact(path, source);
            let defs = match model.resolve_path(&segs, Ns::Type, &site) {
                Ok(defs) => defs,
                Err(reason) => {
                    out.push(unresolved(written, reason));
                    continue;
                }
            };
            for def in defs {
                let children = match def {
                    Def::Type(child) => model.alias_targets(child, 0),
                    Def::External(_) => {
                        out.push(CallTarget {
                            class: EdgeClass::PathExternal,
                            reason: None,
                            ..unresolved(written.clone(), "")
                        });
                        continue;
                    }
                    _ => {
                        out.push(unresolved(
                            written.clone(),
                            "a clap child that names no type",
                        ));
                        continue;
                    }
                };
                if children.is_empty() {
                    out.push(unresolved(
                        written.clone(),
                        "a clap child whose type alias names no crate type",
                    ));
                }
                for child in children {
                    // Each augment method the child has, derived or written:
                    // the parse does not tell which one the parent calls.
                    // With neither, unresolved, with the reason.
                    let mut written_impl = false;
                    for (method, trait_name) in [
                        ("augment_args", "Args"),
                        ("augment_subcommands", "Subcommand"),
                    ] {
                        let filter = TraitRef::External(vec![trait_name.to_string()]);
                        if let Ok(defs) = model.associated(child, method, Some(&filter), 0)
                            && defs.iter().any(|def| matches!(def, Def::Decl(_)))
                        {
                            out.push(derived_reference(model, method, Ok(defs)));
                            written_impl = true;
                        }
                    }
                    if !written_impl {
                        out.push(unresolved(
                            written.clone(),
                            "a clap child whose crate type derives no clap trait and \
                             writes no augment method (a hand-written impl on the trait's \
                             defaults, or no impl)",
                        ));
                    }
                }
            }
        }
    }
    out
}

/// Whether an attribute item is clap's field attribute (`#[arg(..)]` or
/// `#[clap(..)]`) carrying `env`, bare (`env`, the variable named after the
/// field) or with a value (`env = "NAME"`).
fn is_clap_env_attribute(attribute_item: Node<'_>, source: &[u8]) -> bool {
    let Some(attribute) = attribute_item.named_child(0) else {
        return false;
    };
    let is_clap = attribute
        .named_child(0)
        .is_some_and(|path| matches!(compact(path, source).as_str(), "arg" | "clap"));
    is_clap
        && attribute_argument_parts(attribute, source)
            .iter()
            .any(|part| {
                let name: String = part
                    .split('=')
                    .next()
                    .unwrap_or_default()
                    .chars()
                    .filter(|c| !c.is_whitespace())
                    .collect();
                name == "env"
            })
}

/// Whether an attribute item is clap's `flatten` or `subcommand` (`#[command(..)]`,
/// `#[clap(..)]` or `#[arg(..)]` holding either word).
fn is_clap_child_attribute(attribute_item: Node<'_>, source: &[u8]) -> bool {
    let Some(attribute) = attribute_item.named_child(0) else {
        return false;
    };
    let is_clap = attribute
        .named_child(0)
        .is_some_and(|path| matches!(compact(path, source).as_str(), "command" | "clap" | "arg"));
    is_clap
        && attribute_argument_parts(attribute, source)
            .iter()
            .any(|part| matches!(part.trim(), "flatten" | "subcommand"))
}

/// The fields of a struct, or of an enum's variants, with their declared
/// types and their attribute items.
fn fields_with_attributes<'t>(item: Node<'t>) -> Vec<(Node<'t>, Vec<Node<'t>>)> {
    named_fields_with_attributes(item)
        .into_iter()
        .map(|(_, field_type, attributes)| (field_type, attributes))
        .collect()
}

/// `fields_with_attributes`, each field with its name node (`None` for a
/// positional field).
#[allow(clippy::type_complexity)]
fn named_fields_with_attributes<'t>(
    item: Node<'t>,
) -> Vec<(Option<Node<'t>>, Node<'t>, Vec<Node<'t>>)> {
    let Some(body) = item.child_by_field_name("body") else {
        return Vec::new();
    };
    let lists: Vec<Node<'t>> = if body.kind() == "enum_variant_list" {
        named_children_of(body)
            .into_iter()
            .filter(|variant| variant.kind() == "enum_variant")
            .filter_map(|variant| variant.child_by_field_name("body"))
            .collect()
    } else {
        vec![body]
    };
    let mut out = Vec::new();
    for list in lists {
        match list.kind() {
            "field_declaration_list" => {
                for field in named_children_of(list) {
                    if field.kind() == "field_declaration"
                        && let Some(field_type) = field.child_by_field_name("type")
                    {
                        out.push((
                            field.child_by_field_name("name"),
                            field_type,
                            preceding_attributes(field),
                        ));
                    }
                }
            }
            "ordered_field_declaration_list" => {
                let mut cursor = list.walk();
                for field_type in list.children_by_field_name("type", &mut cursor) {
                    out.push((None, field_type, positional_field_attributes(field_type)));
                }
            }
            _ => {}
        }
    }
    out
}

/// The attribute items of a positional field: those before its type, past
/// the field's visibility modifier (`#[serde(default)] pub u8`).
fn positional_field_attributes(field_type: Node<'_>) -> Vec<Node<'_>> {
    let mut out = Vec::new();
    let mut current = field_type.prev_sibling();
    while let Some(sibling) = current {
        match sibling.kind() {
            "attribute_item" => out.push(sibling),
            "visibility_modifier" | "line_comment" | "block_comment" => {}
            _ => break,
        }
        current = sibling.prev_sibling();
    }
    out
}

/// The fields of a clap-derived type that carry a clap `env` attribute, by
/// name (a positional field by its declared type).
fn clap_env_fields(item: Node<'_>, source: &[u8]) -> Vec<String> {
    named_fields_with_attributes(item)
        .into_iter()
        .filter(|(_, _, attributes)| {
            attributes
                .iter()
                .any(|attribute| is_clap_env_attribute(*attribute, source))
        })
        .map(|(name, field_type, _)| {
            name.map_or_else(
                || compact(field_type, source),
                |name| name_text(name, source),
            )
        })
        .collect()
}

/// The declared types of a clap-derived type's flattened and subcommand
/// fields, and of every field of an enum's tuple variants (a `Subcommand`
/// variant's arguments).
fn clap_children<'t>(item: Node<'t>, source: &[u8]) -> Vec<Node<'t>> {
    let is_enum = item.kind() == "enum_item";
    fields_with_attributes(item)
        .into_iter()
        .filter(|(field_type, attributes)| {
            attributes
                .iter()
                .any(|attribute| is_clap_child_attribute(*attribute, source))
                || (is_enum
                    && field_type
                        .parent()
                        .is_some_and(|list| list.kind() == "ordered_field_declaration_list"))
        })
        .map(|(field_type, _)| field_type)
        .collect()
}

/// A reference to `method`, under `filter`'s trait, of every crate type the
/// type path `path` names.
fn type_method_calls(
    model: &CrateModel<'_>,
    file: usize,
    path: Node<'_>,
    method: &str,
    filter: &TraitRef,
    out: &mut Vec<CallTarget>,
) {
    let mut segs = Vec::new();
    type_segs(path, model.files[file].source, &mut segs);
    let site = Site {
        file,
        node: path,
        locals: &[],
        body: None,
    };
    for def in model
        .resolve_path(&segs, Ns::Type, &site)
        .unwrap_or_default()
    {
        if let Def::Type(field_type) = def {
            let lookup = model.associated(field_type, method, Some(filter), 0);
            out.push(derived_reference(model, method, lookup));
        }
    }
}

/// A call a derived body makes, classified from its resolution.
fn derived_reference(model: &CrateModel<'_>, name: &str, lookup: Lookup) -> CallTarget {
    let (class, candidates, reason) = model.classify(lookup, "");
    CallTarget {
        name: name.to_string(),
        qualifier: Qualifier::Path,
        position: Position::Call,
        site: 0,
        region: None,
        test_code: false,
        lock_call: false,
        candidates,
        class,
        reason,
    }
}

/// What one item's `#[serde(..)]` attributes say about deserializing it: a
/// bare `default`, and the paths `default = ".."`, `deserialize_with = ".."`
/// and `with = ".."` name (the last one's `deserialize`).
#[derive(Debug, Default)]
struct SerdeAttributes {
    bare_default: bool,
    paths: Vec<Vec<Seg>>,
}

/// The `#[serde(..)]` attributes that precede `item`.
fn serde_attributes(attribute_items: &[Node<'_>], source: &[u8]) -> SerdeAttributes {
    let mut out = SerdeAttributes::default();
    for &attribute_item in attribute_items {
        let Some(attribute) = attribute_item.named_child(0) else {
            continue;
        };
        if attribute
            .named_child(0)
            .is_none_or(|path| compact(path, source) != "serde")
        {
            continue;
        }
        let Some(arguments) = attribute.child_by_field_name("arguments") else {
            continue;
        };
        let tokens: Vec<Node<'_>> = children_of(arguments)
            .into_iter()
            .filter(|token| !is_comment(*token))
            .collect();
        for (index, token) in tokens.iter().enumerate() {
            if !is_token_name(*token) {
                continue;
            }
            let assigned = tokens.get(index + 1).is_some_and(|next| next.kind() == "=");
            let value = tokens
                .get(index + 2)
                .filter(|value| assigned && value.kind() == "string_literal")
                .map(|value| text(*value, source).trim_matches('"').to_string());
            match (text(*token, source), value) {
                ("default", None) if !assigned => out.bare_default = true,
                ("default" | "deserialize_with", Some(path)) => out.paths.push(string_path(&path)),
                ("with", Some(path)) => {
                    let mut segs = string_path(&path);
                    segs.push(Seg::Name("deserialize".to_string()));
                    out.paths.push(segs);
                }
                _ => {}
            }
        }
    }
    out
}

/// The segments of a path written in a string (`"crate::config::default_x"`).
fn string_path(path: &str) -> Vec<Seg> {
    let mut out = Vec::new();
    if path.starts_with("::") {
        out.push(Seg::Global);
    }
    for segment in path.split("::").filter(|segment| !segment.is_empty()) {
        out.push(match segment {
            "crate" => Seg::Crate,
            "self" => Seg::SelfModule,
            "super" => Seg::Super,
            "Self" => Seg::SelfType,
            name => Seg::Name(name.to_string()),
        });
    }
    out
}

/// What serde's derived `deserialize` of type `t` runs beyond its fields' own
/// `deserialize`: every function a `#[serde(default = "..")]`,
/// `#[serde(deserialize_with = "..")]` or `#[serde(with = "..")]` names, on the
/// type, on a variant or on a field (named or positional), and `default` of
/// the type (a bare `#[serde(default)]` on it) or of the field's type
/// (`default_calls`, a bare one on the field).
fn serde_calls(model: &CrateModel<'_>, t: usize, out: &mut Vec<CallTarget>) {
    let info = &model.types[t];
    let source = model.files[info.file].source;
    let default = TraitRef::External(vec!["Default".to_string()]);
    let mut holders: Vec<(Node<'_>, Vec<Node<'_>>, Option<Node<'_>>)> =
        vec![(info.node, preceding_attributes(info.node), None)];
    if let Some(body) = info.node.child_by_field_name("body")
        && body.kind() == "enum_variant_list"
    {
        for variant in named_children_of(body) {
            if variant.kind() == "enum_variant" {
                holders.push((variant, preceding_attributes(variant), None));
            }
        }
    }
    for (field_type, attributes) in fields_with_attributes(info.node) {
        holders.push((field_type, attributes, Some(field_type)));
    }
    for (holder, attribute_items, field_type) in holders {
        let attributes = serde_attributes(&attribute_items, source);
        for path in &attributes.paths {
            let site = Site {
                file: info.file,
                node: holder,
                locals: &[],
                body: None,
            };
            let name = match path.last() {
                Some(Seg::Name(name)) => name.clone(),
                _ => "deserialize".to_string(),
            };
            out.push(derived_reference(
                model,
                &name,
                model.resolve_path(path, Ns::Value, &site),
            ));
        }
        if attributes.bare_default {
            match field_type {
                Some(field_type) => default_calls(model, info.file, field_type, out),
                None if holder.id() == info.node.id() => {
                    out.push(derived_reference(
                        model,
                        "default",
                        model.associated(t, "default", Some(&default), 0),
                    ));
                }
                None => {}
            }
        }
    }
}

/// The declared type of a struct's named field.
fn field_type_node<'t>(item: Node<'t>, field: &str, source: &[u8]) -> Option<Node<'t>> {
    let body = item.child_by_field_name("body")?;
    named_children_of(body).into_iter().find_map(|declaration| {
        (declaration.kind() == "field_declaration"
            && declaration
                .child_by_field_name("name")
                .is_some_and(|name| name_text(name, source) == field))
        .then(|| declaration.child_by_field_name("type"))
        .flatten()
    })
}

/// The declared types of a struct's or an enum's fields.
fn field_types(item: Node<'_>) -> Vec<Node<'_>> {
    fields_with_attributes(item)
        .into_iter()
        .map(|(field_type, _)| field_type)
        .collect()
}

/// The type paths a struct's or an enum's fields name: every
/// `type_identifier`, `scoped_type_identifier` and generic type's path in a
/// field's declared type, generic arguments included.
fn field_type_paths(item: Node<'_>) -> Vec<Node<'_>> {
    field_types(item).into_iter().flat_map(type_paths).collect()
}

/// The type paths a type names: every `type_identifier`,
/// `scoped_type_identifier` and generic type's path in it, generic arguments
/// included.
fn type_paths(type_node: Node<'_>) -> Vec<Node<'_>> {
    let mut out = Vec::new();
    let mut stack = vec![type_node];
    while let Some(node) = stack.pop() {
        match node.kind() {
            "type_identifier" | "scoped_type_identifier" => out.push(node),
            "generic_type" => {
                if let Some(path) = node.child_by_field_name("type") {
                    out.push(path);
                }
                if let Some(arguments) = node.child_by_field_name("type_arguments") {
                    stack.extend(children_of(arguments));
                }
            }
            _ => stack.extend(children_of(node)),
        }
    }
    out
}

/// What one body does: its references resolved, its acquisitions, its
/// shape-1 guards and its struct literals of holder candidates. Which sites
/// are covered is decided once the holders are known (`cover`).
fn function_facts(
    model: &CrateModel<'_>,
    shapes: &Shapes,
    index: usize,
    context: &FileContext<'_>,
) -> FunctionFacts {
    let decl = &model.decls[index];
    let file = &model.files[decl.file];
    let source = file.source;
    let node = decl.node;
    let is_test =
        decl.kind == DeclKind::Function && is_test_function(node, source, context.test_cfg);
    let in_test_code = context.is_test_code(node, source, is_test);
    let impl_info = match decl.owner {
        Owner::Impl(i) => Some(&model.impls[i]),
        _ => None,
    };
    let external_trait = impl_info.and_then(|info| match &info.trait_ref {
        Some(TraitRef::External(path)) => path.last().cloned(),
        _ => None,
    });
    let drop_of = impl_info
        .filter(|info| {
            decl.name == "drop"
                && matches!(&info.trait_ref, Some(TraitRef::External(path))
                    if path.last().is_some_and(|last| last == "Drop"))
        })
        .map(|info| info.self_types.clone())
        .unwrap_or_default();
    let body = match decl.kind {
        DeclKind::Function => node.child_by_field_name("body"),
        DeclKind::Const | DeclKind::Static => node.child_by_field_name("value"),
        DeclKind::Derived => None,
    };
    let mut facts = FunctionFacts {
        file: file.label.clone(),
        name: decl.name.clone(),
        owner: decl.owner_label.clone(),
        kind: decl.kind,
        drop_of,
        external_trait,
        is_test,
        in_test_code,
        helper: shapes.helpers.contains(&index),
        test_code_calls_by_liveness_alone: 0,
        calls: Vec::new(),
        covered: Vec::new(),
        under_a_guard: Vec::new(),
        guards: Vec::new(),
        rerun_spans: Vec::new(),
        branch_groups: Vec::new(),
        held_lets: Vec::new(),
        dropped_at_once: BTreeSet::new(),
        acquisitions: Vec::new(),
        kept: BTreeSet::new(),
        constructions: Vec::new(),
        sites_in_another_region: Vec::new(),
        lock_calls_named_like_the_crate_lock: 0,
        token_lock_calls: 0,
        token_paths_after_a_bar: 0,
    };
    if let Owner::Derived(t) = decl.owner {
        facts.calls = derived_calls(model, t, &decl.name);
        return facts;
    }
    let Some(body) = body else {
        return facts;
    };
    let nodes = body_nodes(body);
    facts.rerun_spans = nodes
        .iter()
        .filter(|node| ENDS_AT_ITS_START.contains(&node.kind()))
        .map(|node| (node.start_byte(), node.end_byte()))
        .collect();
    let locals = local_bindings(node, &nodes, source);
    let regions = nodes
        .iter()
        .filter(|node| matches!(node.kind(), "closure_expression" | "async_block"))
        .map(|node| (node.start_byte(), node.end_byte()))
        .collect();
    let mut walk = BodyWalk {
        model,
        shapes,
        file: decl.file,
        source,
        context,
        body,
        locals,
        is_test,
        calls: Vec::new(),
        call_at: BTreeMap::new(),
        regions,
        in_place: BTreeMap::new(),
        guards: Vec::new(),
        constructions: Vec::new(),
        test_code_calls_by_liveness_alone: 0,
        lock_calls_named_like_the_crate_lock: 0,
        token_lock_calls: 0,
        token_paths_after_a_bar: 0,
    };
    for node in nodes.iter().copied() {
        match node.kind() {
            "call_expression" => walk.call(node),
            "identifier" | "scoped_identifier" | "generic_function" if is_value_reference(node) => {
                walk.value(node);
            }
            "macro_invocation" => walk.invocation(node),
            _ => {}
        }
    }
    walk.shapes(&nodes);
    (facts.held_lets, facts.dropped_at_once) = held_lets_and_drops(&nodes, source, &walk);
    facts.branch_groups = branch_groups(&nodes);
    facts.acquisitions = walk
        .calls
        .iter()
        .enumerate()
        .filter(|(_, call)| call.lock_call || is_helper_call_target(call, &shapes.helpers))
        .map(|(position, _)| position)
        .collect();
    if facts.helper {
        // A helper's body is one accepted lock call and nothing else, which
        // it hands to its caller.
        facts.kept.extend(facts.acquisitions.iter().copied());
    }
    for call in &walk.calls {
        if call.class != EdgeClass::Environment && call.edges().is_empty() {
            continue;
        }
        if walk.guards.iter().any(|guard| {
            guard.from <= call.site && call.site < guard.to && guard.region != call.region
        }) {
            facts
                .sites_in_another_region
                .push(format!("{}: {}", label(&facts), call.name));
        }
    }
    facts.test_code_calls_by_liveness_alone = walk.test_code_calls_by_liveness_alone;
    facts.lock_calls_named_like_the_crate_lock = walk.lock_calls_named_like_the_crate_lock;
    facts.token_lock_calls = walk.token_lock_calls;
    facts.token_paths_after_a_bar = walk.token_paths_after_a_bar;
    facts.calls = walk.calls;
    facts.guards = walk.guards;
    facts.constructions = walk.constructions;
    facts
}

/// Whether a reference is a call of shape-2 helpers only (every candidate,
/// at least one): an acquisition, which only a shape-1 `let` keeps.
fn is_helper_call_target(call: &CallTarget, helpers: &BTreeSet<usize>) -> bool {
    call.position.is_call()
        && matches!(
            call.class,
            EdgeClass::PathResolved | EdgeClass::PathAmbiguous
        )
        && !call.candidates.is_empty()
        && call
            .candidates
            .iter()
            .all(|candidate| helpers.contains(candidate))
}

/// The names the function's own scope binds, each with the byte range it is
/// visible over, as Rust scopes them. In expression position a visible local
/// binding shadows an item of the module or of an enclosing block (and an item
/// of a block inside the binding's scope shadows the binding:
/// `CrateModel::lexical` takes the innermost), so a bare call of one of these
/// names inside its range is not a call of the crate's function of that name;
/// outside it, it is. Every binding site of the grammar is read, and every
/// name a pattern binds (`pattern_bindings`):
///
/// - a parameter: the whole function;
/// - a `let`: from the end of its own statement (so not in its initializer or
///   its `else` block) to the end of the block that holds it;
/// - a closure parameter, typed or not: from the end of the parameter list to
///   the end of the closure;
/// - a `for` pattern: the loop body;
/// - an `if let` or `while let` pattern, alone or in a let chain: from the end
///   of its own condition (so the later conditions of the chain see it) to the
///   end of the block it guards, never the `else` branch;
/// - a match arm's pattern: the rest of the arm, guard and value.
///
/// An item declared in a block is not a local binding: the resolution reads it
/// from the block's scope (`CrateModel::scope`), so within the block a call of
/// a nested `fn` is an edge to that function, which is walked as a function of
/// its own, and outside the block the name means what it meant before.
fn local_bindings(
    function: Node<'_>,
    body_nodes: &[Node<'_>],
    source: &[u8],
) -> Vec<(String, usize, usize)> {
    let mut out: Vec<(String, usize, usize)> = Vec::new();
    let mut bind = |pattern: Node<'_>, from: usize, to: usize| {
        let mut names = Vec::new();
        pattern_bindings(pattern, source, &mut names);
        out.extend(names.into_iter().map(|name| (name, from, to)));
    };
    if let Some(parameters) = function.child_by_field_name("parameters") {
        let mut cursor = parameters.walk();
        for parameter in parameters.children(&mut cursor) {
            if parameter.kind() != "parameter" {
                continue;
            }
            if let Some(pattern) = parameter.child_by_field_name("pattern") {
                bind(pattern, function.start_byte(), function.end_byte());
            }
        }
    }
    for node in body_nodes {
        match node.kind() {
            "let_declaration" => {
                if let Some(pattern) = node.child_by_field_name("pattern") {
                    let scope_end = node.parent().map_or(function.end_byte(), |p| p.end_byte());
                    bind(pattern, node.end_byte(), scope_end);
                }
            }
            "closure_parameters" => {
                let closure_end = node.parent().map_or(function.end_byte(), |p| p.end_byte());
                let mut cursor = node.walk();
                for child in node.named_children(&mut cursor) {
                    // `|reader: fn()|` is a `parameter` holding the pattern;
                    // `|reader|` and `|(a, b)|` are the pattern itself.
                    let pattern = if child.kind() == "parameter" {
                        child.child_by_field_name("pattern")
                    } else {
                        Some(child)
                    };
                    if let Some(pattern) = pattern {
                        bind(pattern, node.end_byte(), closure_end);
                    }
                }
            }
            "for_expression" => {
                if let (Some(pattern), Some(body)) = (
                    node.child_by_field_name("pattern"),
                    node.child_by_field_name("body"),
                ) {
                    bind(pattern, body.start_byte(), body.end_byte());
                }
            }
            "let_condition" => {
                if let Some(pattern) = node.child_by_field_name("pattern")
                    && let Some(scope_end) = let_condition_scope_end(*node)
                {
                    bind(pattern, node.end_byte(), scope_end);
                }
            }
            "match_arm" => {
                if let Some(pattern) = node.child_by_field_name("pattern") {
                    bind(pattern, pattern.start_byte(), node.end_byte());
                }
            }
            _ => {}
        }
    }
    out
}

/// Where an `if let` or `while let` binding stops being visible: the end of the
/// block the condition guards. A let chain is walked through to its `if` or
/// `while`; a let condition in a match guard is visible to the rest of the arm.
fn let_condition_scope_end(condition: Node<'_>) -> Option<usize> {
    let mut owner = condition.parent()?;
    while owner.kind() == "let_chain" {
        owner = owner.parent()?;
    }
    match owner.kind() {
        "if_expression" => owner
            .child_by_field_name("consequence")
            .map(|b| b.end_byte()),
        "while_expression" => owner.child_by_field_name("body").map(|b| b.end_byte()),
        "match_pattern" => owner.parent().map(|arm| arm.end_byte()),
        _ => None,
    }
}

/// Every name `pattern` binds, by the grammar's `_pattern` kinds. A path in a
/// pattern (`Some` in `Some(x)`, `Point` in `Point { x }`) and a match guard
/// bind nothing; a kind not named here binds nothing either, so a shape this
/// does not know leaves a call free, which the gate reports, rather than local,
/// which would hide it.
fn pattern_bindings(pattern: Node<'_>, source: &[u8], out: &mut Vec<String>) {
    match pattern.kind() {
        "identifier" | "shorthand_field_identifier" => {
            out.push(text(pattern, source).to_string());
        }
        "field_pattern" => {
            // `Point { x: inner }` binds `inner`; the shorthand `Point { x }`
            // binds `x`.
            if let Some(inner) = pattern.child_by_field_name("pattern") {
                pattern_bindings(inner, source, out);
            } else if let Some(name) = pattern.child_by_field_name("name") {
                pattern_bindings(name, source, out);
            }
        }
        "tuple_struct_pattern" | "struct_pattern" | "match_pattern" => {
            // The path (`type`) and the guard (`condition`) are not patterns.
            let skipped: Vec<usize> = ["type", "condition"]
                .iter()
                .filter_map(|field| pattern.child_by_field_name(field))
                .map(|node| node.id())
                .collect();
            let mut cursor = pattern.walk();
            for child in pattern.named_children(&mut cursor) {
                if !skipped.contains(&child.id()) {
                    pattern_bindings(child, source, out);
                }
            }
        }
        "captured_pattern" | "mut_pattern" | "ref_pattern" | "reference_pattern"
        | "tuple_pattern" | "slice_pattern" | "or_pattern" => {
            let mut cursor = pattern.walk();
            for child in pattern.named_children(&mut cursor) {
                pattern_bindings(child, source, out);
            }
        }
        _ => {}
    }
}

/// Per reference, whether an accepted shape covers its site
/// (`FunctionFacts::covered`, decision D-i7-envlock-1): a shape-1 guard of its own
/// body whose region is the site's covers it from the end of the guard's
/// `let` to the guard's end (`Guard`); the `drop` of a holder (shape 3)
/// covers its own straight-line sites, outside every closure, async block and
/// macro that does not run its tokens in place, because the holder's guard
/// field drops after `drop` returns. Nothing else covers anything. The
/// nesting sites fail closed the other way (`FunctionFacts::under_a_guard`):
/// a guard is taken to hold the lock to the end of its block whatever names
/// it, in every region, since a closure written there may run while it
/// lives; and every unconfined hold (`hold_starts`, decision D-i8-45) is
/// taken to hold it at every site after it in the text and throughout the
/// outermost loop, closure, async block or macro invocation around it,
/// since the guard it made may live in any binding until the body returns
/// and run into code that runs again. A body with a `.lock()` on the crate
/// lock in its macro tokens, whose site the walk does not place, is taken
/// to hold the lock throughout.
fn cover(functions: &mut [FunctionFacts], holders: &BTreeSet<usize>, hands: &BTreeSet<usize>) {
    for function in functions.iter_mut() {
        let by_drop = is_drop_of_a_holder(function, holders);
        let throughout = by_drop || function.token_lock_calls > 0;
        let holds: Vec<(usize, Option<(usize, usize)>)> = hold_starts(function, hands)
            .into_iter()
            .map(|position| {
                let site = function.calls[position].site;
                let outermost = function
                    .rerun_spans
                    .iter()
                    .filter(|(start, end)| *start <= site && site < *end)
                    .min_by_key(|(start, end)| (*start, Reverse(*end)))
                    .copied();
                (site, outermost)
            })
            .collect();
        function.covered = function
            .calls
            .iter()
            .map(|call| {
                (by_drop && call.region.is_none())
                    || function.guards.iter().any(|guard| {
                        guard.region == call.region
                            && guard.from <= call.site
                            && call.site < guard.to
                    })
            })
            .collect();
        function.under_a_guard = function
            .calls
            .iter()
            .map(|call| {
                throughout
                    || function
                        .guards
                        .iter()
                        .any(|guard| guard.from <= call.site && call.site < guard.block_end)
                    || confined_lets(function, hands)
                        .any(|held| held.from <= call.site && call.site < held.block_end)
                    || holds.iter().any(|(site, outermost)| {
                        (call.site > *site && !in_exclusive_branches(function, *site, call.site))
                            || outermost
                                .is_some_and(|(start, end)| start <= call.site && call.site < end)
                    })
            })
            .collect();
    }
}

/// Whether the reference at `position` can leave the crate lock held: a lock
/// call, or a call of a body that can hand the lock on (`hands_the_lock_on`).
fn is_hold_source(function: &FunctionFacts, position: usize, hands: &BTreeSet<usize>) -> bool {
    let call = &function.calls[position];
    call.lock_call || call.edges().iter().any(|target| hands.contains(target))
}

/// The confined `let`s (`HeldLet::confined`) whose call is a hold source.
fn confined_lets<'f>(
    function: &'f FunctionFacts,
    hands: &BTreeSet<usize>,
) -> impl Iterator<Item = &'f HeldLet> {
    function
        .held_lets
        .iter()
        .filter(move |held| held.confined && is_hold_source(function, held.acquisition, hands))
}

/// Whether two sites are in different branches of one `if` or `match`
/// (`FunctionFacts::branch_groups`).
fn in_exclusive_branches(function: &FunctionFacts, a: usize, b: usize) -> bool {
    function.branch_groups.iter().any(|group| {
        let branch_of = |site: usize| {
            group
                .iter()
                .position(|(start, end)| *start <= site && site < *end)
        };
        matches!((branch_of(a), branch_of(b)), (Some(x), Some(y)) if x != y)
    })
}

/// The references of a body whose guard's lifetime the gate does not bound
/// (decision D-i8-45): every hold source (`is_hold_source`: a lock call, or a
/// call of a body that can hand a held lock to its caller, shape-2 helpers
/// among them) except the call of a confined `let` (`confined_lets`) and a
/// call dropped where it is made (`FunctionFacts::dropped_at_once`). The
/// guard such a reference makes may sit in a struct or tuple literal, an
/// `Option`, a `Box`, a binding a mention moves it to, or a value the call
/// returns, and the gate reads none of that.
fn hold_starts(function: &FunctionFacts, hands: &BTreeSet<usize>) -> Vec<usize> {
    let confined: BTreeSet<usize> = confined_lets(function, hands)
        .map(|held| held.acquisition)
        .collect();
    (0..function.calls.len())
        .filter(|position| {
            is_hold_source(function, *position, hands)
                && !confined.contains(position)
                && !function.dropped_at_once.contains(position)
        })
        .collect()
}

/// The bodies that can hand a held crate lock to their caller (decision
/// D-i8-45), to a fixpoint: one with an unconfined hold of its own
/// (`hold_starts`), which may return the guard, store it through a
/// parameter or keep it in a value it returns, or with a `.lock()` on the
/// crate lock in its macro tokens. A body whose every acquisition is a
/// confined shape-1 guard releases the lock before it returns, and is not
/// one.
fn hands_the_lock_on(functions: &[FunctionFacts]) -> BTreeSet<usize> {
    let mut hands = BTreeSet::new();
    loop {
        let before = hands.len();
        for (index, function) in functions.iter().enumerate() {
            if !hands.contains(&index)
                && (function.token_lock_calls > 0 || !hold_starts(function, &hands).is_empty())
            {
                hands.insert(index);
            }
        }
        if hands.len() == before {
            return hands;
        }
    }
}

/// The `drop` of an `impl Drop` for a holder (shape 3); for an impl of a
/// type's cfg variants, every variant must be one.
fn is_drop_of_a_holder(function: &FunctionFacts, holders: &BTreeSet<usize>) -> bool {
    !function.drop_of.is_empty() && function.drop_of.iter().all(|ty| holders.contains(ty))
}

/// How the report names a function: its file, its owner when it has one (the
/// `impl` type, the trait, or the functions a nested item sits in), and its
/// name.
fn label(function: &FunctionFacts) -> String {
    format!(
        "{}::{}{}",
        function.file,
        function
            .owner
            .as_ref()
            .map(|owner| format!("{owner}::"))
            .unwrap_or_default(),
        function.name
    )
}

/// The shortest chain of edges (`CallTarget::edges`) from `start` to a body
/// with an environment call no guard of its own covers, or `None` when there
/// is none (design W4-D27 part 2). Only the references a body's own guards
/// do not cover are followed (`FunctionFacts::covered`), because a read
/// reached while the lock is held cannot observe a plant: a planting test
/// holds the lock while it plants. `start` itself is never the answer; its
/// own environment calls are the direct rule's business.
fn reaches_unlocked_env_read(start: usize, functions: &[FunctionFacts]) -> Option<Vec<usize>> {
    let mut queue = VecDeque::from([start]);
    let mut seen = BTreeSet::from([start]);
    let mut came_from: BTreeMap<usize, usize> = BTreeMap::new();
    while let Some(current) = queue.pop_front() {
        let function = &functions[current];
        for (position, call) in function.calls.iter().enumerate() {
            if function.covered[position] {
                continue;
            }
            for target in call.edges().iter().copied() {
                if !seen.insert(target) {
                    continue;
                }
                came_from.insert(target, current);
                if functions[target].reads_unlocked() {
                    let mut chain = vec![target];
                    let mut node = target;
                    while let Some(previous) = came_from.get(&node) {
                        if *previous == start {
                            break;
                        }
                        chain.push(*previous);
                        node = *previous;
                    }
                    chain.reverse();
                    return Some(chain);
                }
                queue.push_back(target);
            }
        }
    }
    None
}

/// Every pair (a function, a function that takes the crate lock) where the
/// first, at a site between a guard's `let` and the end of its block or in a
/// holder's `drop`, in any region (`FunctionFacts::under_a_guard`), takes the
/// lock again or
/// reaches the second over the edges (`CallTarget::edges`), every edge of the
/// reached bodies followed, back to the first too: a function that calls
/// itself while its guard lives reaches itself. `TEST_ENV_LOCK` is a
/// `std::sync::Mutex`, which is not reentrant, so the second acquisition would
/// hang (design W4-D27 part 4). The acquisition a guard's own `let` makes runs
/// before the guard's coverage starts, so it is never a nesting site of its
/// own; a second one while the first guard lives is.
fn nesting_sites(functions: &[FunctionFacts]) -> Vec<(String, String)> {
    let mut out: BTreeSet<(String, String)> = BTreeSet::new();
    for function in functions {
        // A `.lock()` in a macro's tokens has no site the walk places, so
        // one beside any other acquisition, or two, may nest.
        if function.token_lock_calls > 1
            || (function.token_lock_calls > 0 && function.calls.iter().any(|call| call.lock_call))
        {
            out.insert((label(function), label(function)));
        }
        let mut queue: VecDeque<usize> = VecDeque::new();
        let mut seen = BTreeSet::new();
        for (position, call) in function.calls.iter().enumerate() {
            if !function.under_a_guard[position] {
                continue;
            }
            if call.lock_call {
                out.insert((label(function), label(function)));
            }
            for target in call.edges() {
                if seen.insert(*target) {
                    queue.push_back(*target);
                }
            }
        }
        while let Some(current) = queue.pop_front() {
            if functions[current].locks() {
                out.insert((label(function), label(&functions[current])));
                continue;
            }
            for target in functions[current]
                .calls
                .iter()
                .flat_map(CallTarget::edges)
                .copied()
            {
                if seen.insert(target) {
                    queue.push_back(target);
                }
            }
        }
    }
    out.into_iter().collect()
}

/// The first function that takes the crate lock which `start` reaches over
/// every edge, `start` itself included, with the chain to it (empty when
/// `start` takes it in its own body); `None` when it reaches none.
fn reaches_a_locker(start: usize, functions: &[FunctionFacts]) -> Option<Vec<usize>> {
    if functions[start].locks() {
        return Some(Vec::new());
    }
    let mut queue = VecDeque::from([start]);
    let mut seen = BTreeSet::from([start]);
    let mut came_from: BTreeMap<usize, usize> = BTreeMap::new();
    while let Some(current) = queue.pop_front() {
        for target in functions[current]
            .calls
            .iter()
            .flat_map(CallTarget::edges)
            .copied()
        {
            if !seen.insert(target) {
                continue;
            }
            came_from.insert(target, current);
            if functions[target].locks() {
                let mut chain = vec![target];
                let mut node = target;
                while let Some(previous) = came_from.get(&node) {
                    if *previous == start {
                        break;
                    }
                    chain.push(*previous);
                    node = *previous;
                }
                chain.reverse();
                return Some(chain);
            }
            queue.push_back(target);
        }
    }
    None
}

/// The traits whose methods an operator runs without a path at its site
/// (`*x`, `x[i]`, `a + b`, `a += b`, `-a`, `!a`, `a == b`, `a < b`), from the
/// Rust reference's operator overloading and comparison sections. With
/// `Drop`, whose `drop` runs where a value goes out of scope, they are the
/// impls `lock_discipline` checks on their own (`operator_and_drop_hazards`),
/// because the model follows no edge into them from where they run.
const OPERATOR_TRAITS: [&str; 28] = [
    "Deref",
    "DerefMut",
    "Index",
    "IndexMut",
    "Neg",
    "Not",
    "Add",
    "Sub",
    "Mul",
    "Div",
    "Rem",
    "BitAnd",
    "BitOr",
    "BitXor",
    "Shl",
    "Shr",
    "AddAssign",
    "SubAssign",
    "MulAssign",
    "DivAssign",
    "RemAssign",
    "BitAndAssign",
    "BitOrAssign",
    "BitXorAssign",
    "ShlAssign",
    "ShrAssign",
    "PartialEq",
    "PartialOrd",
];

/// Every figure the lock discipline computes over one source directory, and
/// its offenders.
#[derive(Debug, Default)]
struct LockDiscipline {
    files: usize,
    parsed: usize,
    root_errors: Vec<String>,
    live_files: usize,
    test_only_files: usize,
    unreachable_files: usize,
    non_live_files: Vec<String>,
    /// The files a test build compiles.
    test_build_live_files: usize,
    unmodelled: Vec<String>,
    grammar_containers: BTreeSet<String>,
    declared_containers: BTreeSet<String>,
    /// The grammar's expression slots, and how many accept a pattern too.
    expression_slots: usize,
    slots_accepting_a_pattern: usize,
    /// The item kinds a body walk does not enter, from the grammar.
    block_item_kinds: BTreeSet<String>,
    /// The kinds of `RUNTIME_STATEMENTS` the grammar's `_declaration_statement`
    /// holds.
    runtime_statements_found: BTreeSet<String>,
    /// The modules of the module tree, and the sites it does not model.
    modules: usize,
    module_tree_unmodelled: Vec<String>,
    /// `macro_rules!` definitions in reached files, and how many carry
    /// `#[macro_export]`.
    macro_definitions: usize,
    exported_macros: usize,
    /// Invocations of a crate `macro_rules!` in item position (a module's or
    /// a block's item list), whose expansion (a test function, a helper) the
    /// model does not read.
    item_position_macro_invocations: Vec<String>,
    functions: usize,
    /// Const and static initializers walked as bodies.
    initializers: usize,
    /// Methods a derive generates (`DERIVED_METHODS`), modelled as bodies.
    derived_methods: usize,
    functions_skipped_unreachable: usize,
    test_functions: usize,
    test_code_functions: usize,
    env_test_code: usize,
    env_tests: usize,
    test_code_calls_by_liveness_alone: usize,
    lock_acquirers: usize,
    /// The crate lock's guard type, as the report prints it (one per crate
    /// lock, normally one).
    guard_type: Vec<String>,
    /// The structs with a field whose declared type is the crate lock's guard
    /// type, and those of them that are holders (shape 3).
    guard_types: BTreeSet<(String, String)>,
    holders: BTreeSet<(String, String)>,
    /// Why each struct of `guard_types` that is not a holder is not one.
    not_holders: BTreeMap<String, String>,
    /// Holder candidates declared more than once in one module (cfg
    /// variants): a construction names every variant, so none is a holder.
    guard_types_in_cfg_variants: Vec<String>,
    /// The shape-2 helpers.
    helpers: BTreeSet<String>,
    /// The shape-1 guards, by where their `let` stands and by what gives
    /// them their value.
    guards_by_place: BTreeMap<&'static str, usize>,
    guards_by_source: BTreeMap<&'static str, usize>,
    /// Guards whose coverage a mention of their name ends before their block
    /// does, as `function: name`.
    guards_ended_by_a_mention: Vec<String>,
    /// Sites inside a guard's range that are in another region than the
    /// guard's, which the guard does not cover, as `function: name`.
    sites_in_another_region: Vec<String>,
    /// Every acquisition (a lock call or a helper call) by the shape that
    /// keeps it, or none.
    acquisitions_by_shape: BTreeMap<&'static str, usize>,
    held: usize,
    held_by_drop: usize,
    offenders: Vec<String>,
    /// What the resolution made of every call, by class and by qualifier class
    /// (design W4-D27 part 1).
    edges: EdgeAccounting,
    /// What it made of every path naming a value outside a callee position.
    values: EdgeAccounting,
    /// Every function by the form it holds the lock in, the `HELD_` constants.
    holders_by_form: BTreeMap<&'static str, usize>,
    /// Functions with an acquisition no accepted shape keeps.
    refused_acquisitions: Vec<String>,
    /// Test-code functions with a reference no guard of theirs covers that
    /// reaches an environment read no guard covers (design W4-D27 part 2).
    indirect_offenders: Vec<String>,
    /// Each indirect offender's shortest chain to that read.
    indirect_chains: BTreeMap<String, Vec<String>>,
    /// How many indirect offenders a test build runs as a test.
    indirect_offenders_with_a_test_attribute: usize,
    /// Pairs of functions where the first, at a site an accepted shape
    /// covers, takes the lock again or reaches a function that takes it
    /// (design W4-D27 part 4).
    nesting_sites: Vec<(String, String)>,
    /// The bodies that can hand a held lock to their caller
    /// (`hands_the_lock_on`, decision D-i8-45).
    lock_handing_functions: Vec<String>,
    /// The references whose guard's lifetime the gate does not bound
    /// (`hold_starts`), over every body.
    unconfined_holds: usize,
    /// Impls of `Drop` or of an operator trait whose item takes the crate
    /// lock, itself or over the edges, with the chain: one runs where a value
    /// goes out of scope or an operator stands, a guard live there or not.
    nesting_hazards: BTreeMap<String, Vec<String>>,
    /// `.lock()` calls on a path written with the crate lock's name that
    /// resolves to another mutex.
    lock_calls_named_like_the_crate_lock: usize,
    /// The documented limits of the model, each counted where it occurs:
    /// `.lock()` on the crate lock written in a macro's tokens, paths written
    /// right after a `|` in a macro's tokens (a closure parameter there binds
    /// nothing), a `default` not followed into a crate type a field's outside
    /// type wraps, and the scope visits the lookups made.
    token_lock_calls: usize,
    token_paths_after_a_bar: usize,
    defaults_not_followed: usize,
    scope_visits: usize,
    /// A crate root holds `#[macro_use] extern crate`, so no macro runs in
    /// place (`Shapes::macro_use_extern_crate`).
    macro_use_extern_crate: bool,
    /// `cfg_attr` attributes that carry a `derive`, `serde`, `arg`, `clap` or
    /// `command` attribute, which the model does not read.
    attributes_in_cfg_attr: Vec<String>,
    /// Enums with a variant field whose type names the crate lock's guard
    /// type, resolved: an enum is never a holder.
    guard_carrying_enums: Vec<String>,
    /// Impls of other traits outside the crate (`Default`, `Display`,
    /// `From`, `Iterator`) whose item reaches an environment read without
    /// the lock: code outside the crate can run one on a crate value it is
    /// handed (`unwrap_or_default()`, `format!`, `?`, `for`), and the model
    /// follows no edge from there.
    external_trait_impls_reaching_reads: BTreeMap<String, Vec<String>>,
    /// The fields of a clap-derived type that carry a clap `env` attribute,
    /// as `file::Type::field`.
    clap_env_fields: Vec<String>,
    /// Impls of an operator trait or of `Drop` (`OPERATOR_TRAITS`) whose item
    /// reads the environment without the lock, itself or over the edges,
    /// with the chain: an operator or a value going out of scope runs one
    /// with no edge the model follows.
    operator_and_drop_hazards: BTreeMap<String, Vec<String>>,
    /// Every reference the resolution does not follow to exactly one body (an
    /// unresolved path, by its reason; an ambiguous path and a call through an
    /// external trait, by their candidates), as `function: name`, so the
    /// report names them rather than counting them only.
    not_followed: BTreeMap<String, Vec<String>>,
}

/// The lock discipline over every `.rs` file under `src`, with the crate
/// roots and the build configuration given (design W4-D20). The real tests
/// and the planted-tree tests all call this.
fn lock_discipline(src: &Path, roots: &[PathBuf], cfg: &BuildCfg) -> LockDiscipline {
    let paths = rust_sources_under(src);
    let mut parser = rust_parser();
    let grammar = grammar();
    let test_cfg = cfg.for_test_build();
    let mut report = LockDiscipline {
        files: paths.len(),
        grammar_containers: liveness::attribute_containers_in_grammar().expect("NODE_TYPES parses"),
        declared_containers: liveness::declared_attribute_containers(),
        expression_slots: grammar.expression_slot_count,
        slots_accepting_a_pattern: grammar.slots_accepting_a_pattern,
        block_item_kinds: grammar.block_item_kinds.clone(),
        runtime_statements_found: grammar.runtime_statements_found.clone(),
        ..LockDiscipline::default()
    };
    let mut parsed: Vec<(PathBuf, String, Tree)> = Vec::new();
    for path in &paths {
        let source = std::fs::read_to_string(path).expect("read source");
        let Some(tree) = parser.parse(&source, None) else {
            continue;
        };
        parsed.push((path.clone(), source, tree));
    }
    report.parsed = parsed.len();
    let relative =
        |path: &Path| -> String { path.strip_prefix(src).unwrap_or(path).display().to_string() };
    let views: Vec<RustSource<'_>> = parsed
        .iter()
        .map(|(path, source, tree)| RustSource {
            path,
            source: source.as_bytes(),
            tree,
        })
        .collect();
    let crate_liveness = liveness::crate_liveness(roots, &views, cfg);
    let test_liveness = liveness::crate_liveness(roots, &views, &test_cfg);
    report.live_files = crate_liveness.count(FileLiveness::Live);
    report.test_only_files = crate_liveness.count(FileLiveness::TestOnly);
    report.unreachable_files = crate_liveness.count(FileLiveness::Unreachable);
    report.test_build_live_files = test_liveness.count(FileLiveness::Live);
    report.non_live_files = crate_liveness
        .files
        .iter()
        .filter(|(_, class)| **class != FileLiveness::Live)
        .map(|(path, class)| format!("{class:?}: {}", relative(path)))
        .collect();
    report.unmodelled = crate_liveness.unmodelled.clone();

    let mut files: Vec<SourceFile<'_>> = Vec::new();
    for (path, source, tree) in &parsed {
        let label = relative(path);
        if tree.root_node().has_error() {
            report.root_errors.push(label.clone());
        }
        let class = crate_liveness
            .get(path)
            .expect("every parsed file is classified");
        let test_class = test_liveness
            .get(path)
            .expect("every parsed file is classified in a test build");
        if class == FileLiveness::Unreachable {
            let mut stack = vec![tree.root_node()];
            while let Some(node) = stack.pop() {
                if node.kind() == "function_item" {
                    report.functions_skipped_unreachable += 1;
                }
                stack.extend(children_of(node));
            }
        }
        files.push(SourceFile {
            path: normalize(path),
            label,
            source: source.as_bytes(),
            root: tree.root_node(),
            class,
            test_class,
        });
    }
    let root_indices: Vec<usize> = roots
        .iter()
        .filter_map(|root| {
            let root = normalize(root);
            files.iter().position(|file| file.path == root)
        })
        .collect();
    let model = CrateModel::build(files, &root_indices);

    // The crate lock's guard type, the shape-2 helpers and the shape-3
    // holder candidates, which every body's walk needs (decision D-i7-envlock-1).
    let guard_types = model.lock_guard_types();
    report.guard_type = guard_types
        .iter()
        .map(|guard| match guard {
            GuardType::Crate(t) => format!(
                "{}::{}",
                model.files[model.types[*t].file].label, model.types[*t].name
            ),
            GuardType::External(path) => path.join("::"),
        })
        .collect();
    let mut declarations: BTreeMap<(usize, &str), usize> = BTreeMap::new();
    for info in &model.types {
        *declarations
            .entry((model.module_at(info.file, info.node), info.name.as_str()))
            .or_default() += 1;
    }
    let mut candidates: BTreeMap<usize, String> = BTreeMap::new();
    for (t, info) in model.types.iter().enumerate() {
        if model.files[info.file].class == FileLiveness::Unreachable
            && model.files[info.file].test_class == FileLiveness::Unreachable
        {
            continue;
        }
        let Some(candidate) = model.holder_candidate(t, &guard_types) else {
            continue;
        };
        let key = (model.files[info.file].label.clone(), info.name.clone());
        report.guard_types.insert(key.clone());
        let name = format!("{}::{}", key.0, key.1);
        let in_cfg_variants =
            declarations[&(model.module_at(info.file, info.node), info.name.as_str())] > 1;
        if in_cfg_variants {
            report.guard_types_in_cfg_variants.push(name.clone());
        }
        match candidate {
            Ok(_) if in_cfg_variants => {
                report.not_holders.insert(
                    name,
                    "declared in cfg variants, whose literals name every variant".to_string(),
                );
            }
            Ok(field) => {
                candidates.insert(t, field);
            }
            Err(why) => {
                report.not_holders.insert(name, why.to_string());
            }
        }
    }
    report.guard_types_in_cfg_variants.sort();
    report.guard_types_in_cfg_variants.dedup();
    let mut macro_use_extern_crate = false;
    for (index, file) in model.files.iter().enumerate() {
        if file.class == FileLiveness::Unreachable && file.test_class == FileLiveness::Unreachable {
            continue;
        }
        let mut stack = vec![file.root];
        while let Some(node) = stack.pop() {
            stack.extend(children_of(node));
            match node.kind() {
                "attribute_item" if attribute_path_is(node, file.source, "cfg_attr") => {
                    let written = compact(node, file.source);
                    if ["derive(", "serde(", "arg(", "clap(", "command("]
                        .iter()
                        .any(|attribute| written.contains(attribute))
                    {
                        report
                            .attributes_in_cfg_attr
                            .push(format!("{}: {written}", file.label));
                    }
                }
                "enum_item"
                    if field_types(node).into_iter().any(|field_type| {
                        model.names_the_guard_type(index, node, field_type, &guard_types)
                    }) =>
                {
                    if let Some(name) = node.child_by_field_name("name") {
                        report.guard_carrying_enums.push(format!(
                            "{}::{}",
                            file.label,
                            name_text(name, file.source)
                        ));
                    }
                }
                "extern_crate_declaration"
                    if preceding_attributes(node).into_iter().any(|attribute| {
                        attribute_path_is(attribute, file.source, "macro_use")
                    }) =>
                {
                    macro_use_extern_crate = true;
                }
                "macro_invocation"
                    if node
                        .parent()
                        .map(|parent| {
                            if parent.kind() == "expression_statement" {
                                parent.parent()
                            } else {
                                Some(parent)
                            }
                        })
                        .is_some_and(|holder| {
                            holder.is_some_and(|holder| {
                                matches!(holder.kind(), "source_file" | "declaration_list")
                            })
                        }) =>
                {
                    if let Some(name) = node.child_by_field_name("macro") {
                        let site = Site {
                            file: index,
                            node,
                            locals: &[],
                            body: None,
                        };
                        if let Ok([Def::Macro(_)]) = model
                            .resolve_path(&segs_of(name, file.source), Ns::Macro, &site)
                            .as_deref()
                        {
                            report.item_position_macro_invocations.push(format!(
                                "{}: {}!",
                                file.label,
                                compact(name, file.source)
                            ));
                        }
                    }
                }
                _ => {}
            }
        }
    }
    report.attributes_in_cfg_attr.sort();
    report.guard_carrying_enums.sort();
    report.item_position_macro_invocations.sort();
    report.macro_use_extern_crate = macro_use_extern_crate;
    assert_eq!(
        macro_use_extern_crate,
        model.has_macro_use_extern_crate(),
        "instrument: the hold side's resolution reads the same #[macro_use] extern crate"
    );
    let shapes = Shapes {
        helpers: model.helpers(),
        candidates,
        macro_use_extern_crate,
    };
    report.modules = model.modules.len();
    report.module_tree_unmodelled = model.unmodelled.clone();
    report.macro_definitions = model.macros.len();
    report.exported_macros = model.macros.iter().filter(|info| info.exported).count();
    for info in &model.types {
        if CLAP_DERIVES
            .iter()
            .any(|derive| info.derives.contains(*derive))
        {
            let source = model.files[info.file].source;
            for field in clap_env_fields(info.node, source) {
                report.clap_env_fields.push(format!(
                    "{}::{}::{field}",
                    model.files[info.file].label, info.name,
                ));
            }
        }
    }

    // What every body does, then the resolved call graph, before every figure
    // below, because they decide them (design W4-D27).
    let mut functions: Vec<FunctionFacts> = (0..model.decls.len())
        .map(|index| {
            let decl = &model.decls[index];
            let context = FileContext {
                cfg,
                test_cfg: &test_cfg,
                class: model.files[decl.file].class,
                test_class: model.files[decl.file].test_class,
            };
            function_facts(&model, &shapes, index, &context)
        })
        .collect();
    for call in functions.iter().flat_map(|function| function.calls.iter()) {
        if call.position.is_call() {
            report.edges.count(call);
        } else {
            report.values.count(call);
        }
    }
    for function in &functions {
        for call in &function.calls {
            let heading = match call.class {
                EdgeClass::PathUnresolved => format!(
                    "unresolved, {}",
                    call.reason.unwrap_or("no reason recorded")
                ),
                EdgeClass::PathAmbiguous => "ambiguous".to_string(),
                EdgeClass::PathTraitDispatch => "through an external trait".to_string(),
                EdgeClass::PathExternalAtCrateType => {
                    "out of the crate at a crate type".to_string()
                }
                _ => continue,
            };
            let candidates: Vec<String> = call
                .candidates
                .iter()
                .map(|decl| {
                    let decl = &model.decls[*decl];
                    format!(
                        "{}::{}{}",
                        model.files[decl.file].label,
                        decl.owner_label
                            .as_ref()
                            .map(|owner| format!("{owner}::"))
                            .unwrap_or_default(),
                        decl.name
                    )
                })
                .collect();
            report
                .not_followed
                .entry(heading)
                .or_default()
                .push(format!("{}: {} {candidates:?}", label(function), call.name));
        }
    }

    // The holders (shape 3): every struct literal of the candidate in the
    // crate is one a walk read and accepted, at least one exists, and its
    // guard field is named nowhere else. Then which sites the shapes cover.
    let uses = model.holder_field_uses(&shapes.candidates);
    let mut holders: BTreeSet<usize> = BTreeSet::new();
    for (t, field) in &shapes.candidates {
        let info = &model.types[*t];
        let name = format!("{}::{}", model.files[info.file].label, info.name);
        let constructions: Vec<&HolderConstruction> = functions
            .iter()
            .flat_map(|function| function.constructions.iter())
            .filter(|construction| construction.ty == *t)
            .collect();
        let read: BTreeSet<(usize, usize)> = constructions
            .iter()
            .map(|construction| (construction.file, construction.node))
            .collect();
        let why = if constructions.is_empty() {
            Some("never built".to_string())
        } else if let Some(refused) = constructions
            .iter()
            .find_map(|construction| construction.refused)
        {
            Some(format!("a struct literal of it is not accepted: {refused}"))
        } else if uses
            .literals
            .get(t)
            .into_iter()
            .flatten()
            .any(|literal| !read.contains(literal))
        {
            Some("a struct literal of it that no body's walk reads".to_string())
        } else {
            uses.mentions
                .get(t)
                .and_then(|mentions| mentions.first())
                .map(|first| {
                    format!("its guard field `{field}` is named outside its literals: {first}")
                })
        };
        match why {
            None => {
                holders.insert(*t);
                report
                    .holders
                    .insert((model.files[info.file].label.clone(), info.name.clone()));
            }
            Some(why) => {
                report.not_holders.insert(name, why);
            }
        }
    }
    for function in &mut functions {
        let kept: Vec<usize> = function
            .constructions
            .iter()
            .filter(|construction| {
                holders.contains(&construction.ty) && construction.refused.is_none()
            })
            .filter_map(|construction| construction.acquisition)
            .collect();
        function.kept.extend(kept);
    }
    let hands = hands_the_lock_on(&functions);
    report.lock_handing_functions = hands.iter().map(|i| label(&functions[*i])).collect();
    report.lock_handing_functions.sort();
    report.unconfined_holds = functions
        .iter()
        .map(|function| hold_starts(function, &hands).len())
        .sum();
    cover(&mut functions, &holders, &hands);

    let is_function = |f: &&FunctionFacts| f.kind == DeclKind::Function;
    report.functions = functions.iter().filter(is_function).count();
    report.initializers = functions
        .iter()
        .filter(|f| matches!(f.kind, DeclKind::Const | DeclKind::Static))
        .count();
    report.derived_methods = functions
        .iter()
        .filter(|f| f.kind == DeclKind::Derived)
        .count();
    report.test_functions = functions.iter().filter(|f| f.is_test).count();
    report.test_code_functions = functions
        .iter()
        .filter(is_function)
        .filter(|f| f.in_test_code)
        .count();
    report.test_code_calls_by_liveness_alone = functions
        .iter()
        .map(|f| f.test_code_calls_by_liveness_alone)
        .sum();
    report.lock_calls_named_like_the_crate_lock = functions
        .iter()
        .map(|f| f.lock_calls_named_like_the_crate_lock)
        .sum();
    report.token_lock_calls = functions.iter().map(|f| f.token_lock_calls).sum();
    report.token_paths_after_a_bar = functions.iter().map(|f| f.token_paths_after_a_bar).sum();
    report.defaults_not_followed = model.defaults_not_followed.get();
    report.helpers = functions.iter().filter(|f| f.helper).map(label).collect();

    // Per function: its guards, the shapes that keep its acquisitions, and
    // the form it holds the lock in.
    for function in &mut functions {
        for guard in &function.guards {
            *report
                .guards_by_place
                .entry(match guard.place {
                    Place::Top => "a let at the top of the body",
                    Place::Inner => "a let in an inner block",
                    Place::Closure => "a let in a closure or async block",
                })
                .or_default() += 1;
            *report
                .guards_by_source
                .entry(match guard.source {
                    GuardSource::LockCall => "a lock call",
                    GuardSource::HelperCall => "a helper call",
                })
                .or_default() += 1;
            if guard.to < guard.block_end {
                report.guards_ended_by_a_mention.push(format!(
                    "{}: {}",
                    label(function),
                    guard.name
                ));
            }
        }
        report
            .sites_in_another_region
            .extend(function.sites_in_another_region.iter().cloned());
        let guard_kept: BTreeSet<usize> = function
            .guards
            .iter()
            .map(|guard| guard.acquisition)
            .collect();
        let mut refused = function.token_lock_calls > 0;
        for position in &function.acquisitions {
            let shape = if function.helper && function.kept.contains(position) {
                "returned by a shape-2 helper"
            } else if guard_kept.contains(position) {
                "kept by a shape-1 let"
            } else if function.kept.contains(position) {
                "kept in a holder's guard field"
            } else {
                refused = true;
                "kept by no accepted shape"
            };
            *report.acquisitions_by_shape.entry(shape).or_default() += 1;
        }
        if function.token_lock_calls > 0 {
            *report
                .acquisitions_by_shape
                .entry("kept by no accepted shape")
                .or_default() += function.token_lock_calls;
        }
        if refused {
            report.refused_acquisitions.push(label(function));
        }
    }
    report.refused_acquisitions.sort();
    report.guards_ended_by_a_mention.sort();
    report.sites_in_another_region.sort();
    for function in &functions {
        let form = if !function.guards.is_empty() {
            HELD_IN_BODY
        } else if function.helper {
            HELD_HELPER
        } else if is_drop_of_a_holder(function, &holders) {
            HELD_BY_DROP
        } else if report.refused_acquisitions.contains(&label(function)) {
            HELD_REFUSED
        } else {
            HELD_NONE
        };
        *report.holders_by_form.entry(form).or_default() += 1;
    }
    report.lock_acquirers = functions
        .iter()
        .filter(|f| !f.guards.is_empty() || f.helper)
        .count();

    // The indirect rule (design W4-D27 part 2): a test-code function item
    // with a reference its own shapes do not cover that reaches, over
    // uncovered references, an environment call nothing covers. A const or
    // static initializer and a derived method are never run as code of their
    // own: they are links of a chain, and the function that reaches them is
    // the one reported.
    for (index, function) in functions.iter().enumerate() {
        if function.kind != DeclKind::Function || !function.in_test_code {
            continue;
        }
        if let Some(chain) = reaches_unlocked_env_read(index, &functions) {
            let name = label(function);
            if function.is_test {
                report.indirect_offenders_with_a_test_attribute += 1;
            }
            report.indirect_chains.insert(
                name.clone(),
                chain.iter().map(|hop| label(&functions[*hop])).collect(),
            );
            report.indirect_offenders.push(name);
        }
    }
    report.indirect_offenders.sort();
    report.nesting_sites = nesting_sites(&functions);

    // Operators and implicit drops run a crate impl with no edge from where
    // they run: each such impl must not reach an unlocked read itself, nor
    // take the lock (a guard may be live where it runs). Other traits outside
    // the crate are listed, not refused.
    for (index, function) in functions.iter().enumerate() {
        let Some(trait_name) = &function.external_trait else {
            continue;
        };
        let runs_unseen = trait_name == "Drop" || OPERATOR_TRAITS.contains(&trait_name.as_str());
        if runs_unseen && let Some(chain) = reaches_a_locker(index, &functions) {
            report.nesting_hazards.insert(
                label(function),
                chain.iter().map(|hop| label(&functions[*hop])).collect(),
            );
        }
        let chain = if function.reads_unlocked() {
            Some(Vec::new())
        } else {
            reaches_unlocked_env_read(index, &functions)
        };
        let Some(chain) = chain else {
            continue;
        };
        let chain: Vec<String> = chain.iter().map(|hop| label(&functions[*hop])).collect();
        if runs_unseen {
            report
                .operator_and_drop_hazards
                .insert(label(function), chain);
        } else {
            report
                .external_trait_impls_reaching_reads
                .insert(format!("{} ({trait_name})", label(function)), chain);
        }
    }

    // The direct rule: a test-code function item's own environment calls.
    let env_test_code: Vec<&FunctionFacts> = functions
        .iter()
        .filter(|f| f.kind == DeclKind::Function)
        .filter(|f| f.touches_env_in_test_code())
        .collect();
    report.env_test_code = env_test_code.len();
    report.env_tests = env_test_code.iter().filter(|f| f.is_test).count();
    for function in &env_test_code {
        let uncovered = function
            .calls
            .iter()
            .zip(&function.covered)
            .any(|(call, covered)| {
                call.class == EdgeClass::Environment && call.test_code && !covered
            });
        if uncovered {
            report.offenders.push(label(function));
        } else if is_drop_of_a_holder(function, &holders) {
            report.held_by_drop += 1;
        } else {
            report.held += 1;
        }
    }
    report.offenders.sort();
    report.clap_env_fields.sort();
    report.scope_visits = model.scope_visits.get();
    report
}

#[test]
fn every_environment_touching_test_holds_the_crate_lock() {
    let src = daemon_src_dir();
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let cfg = BuildCfg::from_cargo_metadata(Path::new(env!("CARGO")), &manifest)
        .unwrap_or_else(|error| panic!("instrument: the build configuration: {error}"));
    let report = lock_discipline(&src, cfg.roots(), &cfg);

    // Instrument: the roots, the parse, the model and what it found.
    println!("crate roots (cargo metadata): {}", cfg.roots().len());
    for root in cfg.roots() {
        println!("  {}", root.display());
    }
    println!(
        "files parsed: {} (.rs files under sqry-daemon/src: {})",
        report.parsed, report.files
    );
    assert!(report.files > 0, "instrument: no .rs files found");
    assert_eq!(report.parsed, report.files, "instrument: every file parses");
    println!(
        "parsed trees with an error at the root: {}",
        report.root_errors.len()
    );
    assert!(
        report.root_errors.is_empty(),
        "instrument: every parse is clean: {:?}",
        report.root_errors
    );
    println!(
        "attribute containers: grammar {}, declared {}",
        report.grammar_containers.len(),
        report.declared_containers.len()
    );
    assert_eq!(
        report.grammar_containers, report.declared_containers,
        "instrument: the grammar's attribute containers must equal the model's declared sets"
    );
    println!(
        "files: live {}, test-only {}, unreachable {} (parsed {}); compiled by a test build {}",
        report.live_files,
        report.test_only_files,
        report.unreachable_files,
        report.parsed,
        report.test_build_live_files
    );
    for file in &report.non_live_files {
        println!("  {file}");
    }
    assert_eq!(
        report.live_files + report.test_only_files + report.unreachable_files,
        report.parsed,
        "instrument: every parsed file is classified once"
    );
    for root in cfg.roots() {
        assert!(
            root.starts_with(&src),
            "instrument: the root {} is under sqry-daemon/src",
            root.display()
        );
    }
    println!("unmodelled sites: {}", report.unmodelled.len());
    for site in &report.unmodelled {
        println!("  {site}");
    }
    assert!(
        report.unmodelled.is_empty(),
        "instrument: nothing may be unmodelled: {:?}",
        report.unmodelled
    );
    println!(
        "grammar expression slots: {}, of them accepting a pattern too: {}",
        report.expression_slots, report.slots_accepting_a_pattern
    );
    assert!(
        report.expression_slots > 0,
        "instrument: the grammar's expression slots were read"
    );
    assert_eq!(
        report.slots_accepting_a_pattern, 0,
        "instrument: a path's slot alone decides whether it names a value only while no \
         slot accepts both an expression and a pattern"
    );
    println!(
        "block item kinds the body walk does not enter: {} {:?}",
        report.block_item_kinds.len(),
        report.block_item_kinds
    );
    let runtime: BTreeSet<String> = RUNTIME_STATEMENTS
        .iter()
        .map(|kind| kind.to_string())
        .collect();
    assert_eq!(
        report.runtime_statements_found, runtime,
        "instrument: every run-time statement kind named here is in the grammar's \
         _declaration_statement, so the item kinds are that supertype without them"
    );
    assert!(
        [
            "function_item",
            "const_item",
            "static_item",
            "use_declaration",
            "macro_definition"
        ]
        .iter()
        .all(|kind| report.block_item_kinds.contains(*kind)),
        "instrument: the derived item kinds hold the ones the resolution reads"
    );
    println!(
        "modules in the module tree: {}, sites it does not model: {}",
        report.modules,
        report.module_tree_unmodelled.len()
    );
    for site in &report.module_tree_unmodelled {
        println!("  {site}");
    }
    assert!(
        report.modules >= cfg.roots().len(),
        "instrument: every crate root is a module"
    );
    assert!(
        report.module_tree_unmodelled.is_empty(),
        "instrument: the module tree reaches every file the liveness model does, once: {:?}",
        report.module_tree_unmodelled
    );
    println!(
        "macro_rules! definitions: {}, of them #[macro_export]: {}",
        report.macro_definitions, report.exported_macros
    );
    assert!(
        report.test_build_live_files >= report.live_files,
        "instrument: a test build compiles every file a non-test build does"
    );
    println!("function items: {}", report.functions);
    println!(
        "const and static initializers walked as bodies: {}",
        report.initializers
    );
    println!(
        "methods derives generate (std, and serde's Deserialize), modelled as bodies: {}",
        report.derived_methods
    );
    println!(
        "function items skipped in unreachable files: {}",
        report.functions_skipped_unreachable
    );
    println!("#[test] functions: {}", report.test_functions);
    println!("functions in test code: {}", report.test_code_functions);
    println!(
        "environment-touching functions in test code: {}",
        report.env_test_code
    );
    println!(
        "environment-touching #[test] functions: {}",
        report.env_tests
    );
    println!(
        "environment calls in test code outside a #[test] function (by liveness alone): {}",
        report.test_code_calls_by_liveness_alone
    );
    println!(
        "functions that acquire {CRATE_LOCK}: {}",
        report.lock_acquirers
    );
    assert!(report.live_files > 0, "instrument: no file is live");
    assert!(
        report.functions > report.test_code_functions,
        "instrument: the parse found production functions as well as test code"
    );
    assert!(
        report.test_functions > 0,
        "instrument: no #[test] function found"
    );
    assert!(
        report.test_code_functions >= report.test_functions,
        "instrument: every #[test] function is test code"
    );
    assert!(
        report.env_tests > 0,
        "instrument: no environment-touching #[test] function found"
    );
    assert!(
        report.lock_acquirers > 0,
        "instrument: no function acquires {CRATE_LOCK}"
    );
    println!("the crate lock's guard type: {:?}", report.guard_type);
    assert_eq!(
        report.guard_type.len(),
        1,
        "instrument: the crate lock's guard type is resolved, and there is one"
    );
    println!(
        "structs with a field of the guard type: {}",
        report.guard_types.len()
    );
    for (file, name) in &report.guard_types {
        println!("  {file}::{name}");
    }
    println!("holders (shape 3): {}", report.holders.len());
    for (file, name) in &report.holders {
        println!("  {file}::{name}");
    }
    println!(
        "lock calls on a path written {CRATE_LOCK} that resolves to another mutex: {}",
        report.lock_calls_named_like_the_crate_lock
    );
    println!(
        "environment-touching test code every environment call of which a guard covers: {}",
        report.held
    );
    println!(
        "drop of a type that holds the lock: {}",
        report.held_by_drop
    );
    assert!(
        report.held_by_drop <= report.holders.len(),
        "a type has one Drop impl, so accepted drops cannot outnumber holders"
    );
    assert_eq!(
        report.held + report.held_by_drop + report.offenders.len(),
        report.env_test_code,
        "every environment-touching function in test code is classified once"
    );

    println!(
        "environment-touching test code without {CRATE_LOCK}: {}",
        report.offenders.len()
    );
    for offender in &report.offenders {
        println!("  {offender}");
    }
    assert!(
        report.offenders.is_empty(),
        "every function in test code that touches the environment must hold {CRATE_LOCK} \
         (a module-local mutex serialises nothing against the crate's other tests): \
         {:#?}",
        report.offenders
    );
}

/// U15 (surface parity W4 round 3, design W4-D20; round 7, decision D-i7-envlock-1):
/// the lock discipline decides test code and guard types from code. The guard
/// type is the crate lock's own, by resolution (`std::sync::MutexGuard` for
/// this tree's `std::sync::Mutex`): a struct whose only mention of
/// `MutexGuard` is a comment has no field of it, and neither has one whose
/// field holds another lock's guard (an `RwLockWriteGuard`), one whose field
/// is a crate struct that happens to be named `MutexGuard`, or one whose field
/// is a struct with a guard field or an `Option` of one (only a field of the
/// guard type itself counts, so nothing is followed two levels deep). A
/// helper in a file a `#[cfg(test)] mod` declares, a `#[cfg(test)]` statement
/// in a production function, and a `#[cfg_attr(test, test)]` function (a
/// test in a test build, a plain function otherwise) are test code that must
/// lock, and so is a `#[cfg_attr(not(test), test)]` function, which a
/// non-test build strips as a test and only a test build compiles; the same
/// with the lock are not offenders; a production reader and a
/// `#[cfg(not(test))]` statement inside a test (compiled by no build that
/// runs it) are not test code.
#[test]
fn the_lock_discipline_decides_test_code_and_guards_from_code() {
    let mut failed: Vec<String> = Vec::new();
    let mut checks = 0usize;

    // A planted crate tree with explicit roots.
    let planted = tempfile::tempdir().expect("a temporary directory");
    let src = planted.path().join("src");
    let files: BTreeMap<&str, &str> = [
        (
            "lib.rs",
            "\
#![allow(dead_code)]\n\
pub static TEST_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());\n\
#[cfg(test)]\nmod helpers;\n\
#[cfg(test)]\nmod locked_helpers;\n\
pub fn production_statement() {\n    #[cfg(test)]\n    let _v = std::env::var_os(\"W4\");\n}\n\
pub fn production_statement_locked() {\n    let _env = crate::TEST_ENV_LOCK.lock();\n    #[cfg(test)]\n    let _v = std::env::var_os(\"W4\");\n}\n\
pub fn production_reader() -> Option<std::ffi::OsString> {\n    std::env::var_os(\"W4\")\n}\n\
#[cfg_attr(test, test)]\nfn cfg_attr_test_function() {\n    unsafe { std::env::set_var(\"W4\", \"1\") };\n}\n\
#[cfg_attr(not(test), test)]\nfn cfg_attr_only_a_test_build_compiles() {\n    unsafe { std::env::set_var(\"W4\", \"1\") };\n}\n\
#[cfg(test)]\nmod t {\n    #[test]\n    fn statement_compiled_only_without_test() {\n        #[cfg(not(test))]\n        unsafe {\n            std::env::set_var(\"W4\", \"1\")\n        };\n    }\n}\n\
mod parking_lot {\n    pub struct MutexGuard<'a, T>(pub &'a T);\n}\n\
struct CommentOnly {\n    // holds a MutexGuard in spirit only\n    value: u8,\n}\n\
struct RealGuard {\n    _guard: std::sync::MutexGuard<'static, ()>,\n}\n\
struct RwOnly {\n    _guard: std::sync::RwLockWriteGuard<'static, ()>,\n}\n\
struct TupleGuard(parking_lot::MutexGuard<'static, ()>);\n\
struct Nested {\n    _inner: RealGuard,\n}\n\
struct DeepNested(Option<Nested>);\n",
        ),
        (
            "helpers.rs",
            "pub fn helper() {\n    unsafe { std::env::set_var(\"W4\", \"1\") };\n}\n",
        ),
        (
            "locked_helpers.rs",
            "pub fn helper_locked() {\n    let _env = crate::TEST_ENV_LOCK.lock();\n    unsafe { std::env::set_var(\"W4\", \"1\") };\n}\n",
        ),
    ]
    .into_iter()
    .collect();
    for (relative, text) in &files {
        let path = src.join(relative);
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("mkdir");
        std::fs::write(&path, text).expect("write");
    }
    assert_eq!(assert_compiles(&src), 2, "instrument: both builds compiled");
    let roots = vec![src.join("lib.rs")];
    let cfg = BuildCfg::new(roots.clone(), BTreeMap::new());
    let report = lock_discipline(&src, &roots, &cfg);
    println!(
        "planted: files {}, live {}, test-only {}, unreachable {}, unmodelled {}, root errors {}",
        report.files,
        report.live_files,
        report.test_only_files,
        report.unreachable_files,
        report.unmodelled.len(),
        report.root_errors.len()
    );
    checks += 1;
    if (
        report.files,
        report.live_files,
        report.test_only_files,
        report.unreachable_files,
    ) != (3, 1, 2, 0)
        || !report.unmodelled.is_empty()
        || !report.root_errors.is_empty()
    {
        failed.push(format!(
            "the planted tree classified as {:?} with unmodelled {:?} and root errors {:?}",
            report.non_live_files, report.unmodelled, report.root_errors
        ));
    }
    let expected_offenders = vec![
        "helpers.rs::helper".to_string(),
        "lib.rs::cfg_attr_only_a_test_build_compiles".to_string(),
        "lib.rs::cfg_attr_test_function".to_string(),
        "lib.rs::production_statement".to_string(),
    ];
    println!(
        "planted offenders: {:?} (expected {expected_offenders:?}); held {}, environment-touching test code {}, by liveness alone {}",
        report.offenders,
        report.held,
        report.env_test_code,
        report.test_code_calls_by_liveness_alone
    );
    checks += 1;
    if report.offenders != expected_offenders {
        failed.push(format!(
            "offenders {:?}, expected {expected_offenders:?}",
            report.offenders
        ));
    }
    checks += 1;
    if (
        report.held,
        report.env_test_code,
        report.test_code_calls_by_liveness_alone,
        report.test_functions,
    ) != (2, 6, 5, 2)
    {
        failed.push(format!(
            "held {}, environment-touching test code {}, by liveness alone {}, test functions {}; expected 2, 6, 5, 2",
            report.held,
            report.env_test_code,
            report.test_code_calls_by_liveness_alone,
            report.test_functions
        ));
    }
    // Guard types by resolution of the field's declared type, not from the
    // struct body's text, and never through another struct.
    let expected_guards: BTreeSet<(String, String)> = ["RealGuard"]
        .into_iter()
        .map(|name| ("lib.rs".to_string(), name.to_string()))
        .collect();
    println!(
        "guard-carrying structs: {:?} (expected {expected_guards:?})",
        report.guard_types
    );
    checks += 1;
    if report.guard_types != expected_guards {
        failed.push(format!(
            "guard types {:?}, expected {expected_guards:?}",
            report.guard_types
        ));
    }
    assert_eq!(checks, 4, "U15 runs every check it declares");
    println!("U15: checks {checks}, failed {}", failed.len());
    assert!(
        failed.is_empty(),
        "{} check(s) failed:\n{}",
        failed.len(),
        failed.join("\n")
    );
}

/// Print every figure the call resolution and the accepted shapes produced,
/// each listing under its own heading, so a blind spot is visible instead of
/// silent (design W4-D27 part 1, decision D-i7-envlock-1). The round 4 tests and the
/// planted-tree tests share it; each asserts its own figures.
fn print_resolution(report: &LockDiscipline) {
    for (what, accounting) in [
        ("call expressions", &report.edges),
        ("value paths", &report.values),
    ] {
        println!(
            "{what} walked: {} (of them read from macro tokens: {})",
            accounting.calls_walked, accounting.from_macros
        );
        println!(
            "  path resolved {}, path to a const or static {}, path ambiguous {}, path to a constructor {}, path out of the crate {}, path through an external trait {}, path out of the crate at a crate type {}, path unresolved {}",
            accounting.path_resolved,
            accounting.path_const_or_static,
            accounting.path_ambiguous,
            accounting.path_constructor,
            accounting.path_external,
            accounting.path_trait_dispatch,
            accounting.path_external_at_crate_type,
            accounting.path_unresolved
        );
        println!(
            "  method unresolved {}, environment calls {}, other callee {}, local binding {}, pending {}",
            accounting.method_unresolved,
            accounting.env_calls,
            accounting.other_callee,
            accounting.local_binding,
            accounting.pending
        );
        for ((qualifier, class), count) in &accounting.by_qualifier {
            println!("  qualifier {qualifier}, {class}: {count}");
        }
        for (reason, count) in &accounting.by_reason {
            println!("  unresolved, {reason}: {count}");
        }
    }
    for (heading, sites) in &report.not_followed {
        println!("not followed to one body, {heading}: {}", sites.len());
        for site in sites {
            println!("  {site}");
        }
    }
    println!("the crate lock's guard type: {:?}", report.guard_type);
    for (form, count) in &report.holders_by_form {
        println!("holding the lock, {form}: {count}");
    }
    for (shape, count) in &report.acquisitions_by_shape {
        println!("acquisitions {shape}: {count}");
    }
    for (place, count) in &report.guards_by_place {
        println!("shape-1 guards, {place}: {count}");
    }
    for (source, count) in &report.guards_by_source {
        println!("shape-1 guards given {source}: {count}");
    }
    println!("shape-2 helpers: {}", report.helpers.len());
    for name in &report.helpers {
        println!("  {name}");
    }
    println!(
        "structs with a field of the guard type: {}",
        report.guard_types.len()
    );
    for (file, name) in &report.guard_types {
        println!("  {file}::{name}");
    }
    println!("holders (shape 3): {}", report.holders.len());
    for (file, name) in &report.holders {
        println!("  {file}::{name}");
    }
    println!(
        "structs with a field of the guard type that are no holder: {}",
        report.not_holders.len()
    );
    for (name, why) in &report.not_holders {
        println!("  {name}: {why}");
    }
    println!(
        "guard types declared in cfg variants, never holders: {}",
        report.guard_types_in_cfg_variants.len()
    );
    for site in &report.guard_types_in_cfg_variants {
        println!("  {site}");
    }
    println!(
        "guards a mention of their name ends before their block does: {}",
        report.guards_ended_by_a_mention.len()
    );
    for site in &report.guards_ended_by_a_mention {
        println!("  {site}");
    }
    println!(
        "sites inside a guard's range in another region (a closure, an async block, a macro that does not run its tokens in place), not covered: {}",
        report.sites_in_another_region.len()
    );
    for site in &report.sites_in_another_region {
        println!("  {site}");
    }
    println!(
        "lock calls on a path written {CRATE_LOCK} that resolves to another mutex: {}",
        report.lock_calls_named_like_the_crate_lock
    );
    println!(
        "documented limits: .lock() on the crate lock in macro tokens {}, macro token paths after a | {}, defaults not followed into a wrapped crate type {}, scope visits {}",
        report.token_lock_calls,
        report.token_paths_after_a_bar,
        report.defaults_not_followed,
        report.scope_visits
    );
    println!(
        "a #[macro_use] extern crate, so no macro runs in place: {}",
        report.macro_use_extern_crate
    );
    println!(
        "item-position invocations of a crate macro_rules!, not expanded: {}",
        report.item_position_macro_invocations.len()
    );
    for site in &report.item_position_macro_invocations {
        println!("  {site}");
    }
    println!(
        "cfg_attr attributes carrying an attribute the model reads, not read there: {}",
        report.attributes_in_cfg_attr.len()
    );
    for site in &report.attributes_in_cfg_attr {
        println!("  {site}");
    }
    println!(
        "enums with a variant field naming the guard type, never holders: {}",
        report.guard_carrying_enums.len()
    );
    for site in &report.guard_carrying_enums {
        println!("  {site}");
    }
    println!(
        "impls of other traits outside the crate that reach an unlocked environment read: {}",
        report.external_trait_impls_reaching_reads.len()
    );
    for (name, chain) in &report.external_trait_impls_reaching_reads {
        println!("  {name}: {}", chain.join(" -> "));
    }
    println!("clap env attributes: {}", report.clap_env_fields.len());
    for field in &report.clap_env_fields {
        println!("  {field}");
    }
    println!(
        "operator and drop impls that reach an unlocked environment read: {}",
        report.operator_and_drop_hazards.len()
    );
    for (name, chain) in &report.operator_and_drop_hazards {
        println!("  {name}: {}", chain.join(" -> "));
    }
    println!(
        "operator and drop impls that take the crate lock: {}",
        report.nesting_hazards.len()
    );
    for (name, chain) in &report.nesting_hazards {
        println!("  {name}: {}", chain.join(" -> "));
    }
    println!(
        "functions with an acquisition no accepted shape keeps: {}",
        report.refused_acquisitions.len()
    );
    for name in &report.refused_acquisitions {
        println!("  {name}");
    }
    println!(
        "references whose guard's lifetime no confined let bounds: {}",
        report.unconfined_holds
    );
    println!(
        "functions that can hand a held lock to their caller: {}",
        report.lock_handing_functions.len()
    );
    for name in &report.lock_handing_functions {
        println!("  {name}");
    }
    println!("nesting sites: {}", report.nesting_sites.len());
    for (holder, reached) in &report.nesting_sites {
        println!("  {holder} reaches {reached}");
    }
    println!(
        "environment-touching test code without {CRATE_LOCK} (the direct rule): {}",
        report.offenders.len()
    );
    for name in &report.offenders {
        println!("  {name}");
    }
    println!(
        "test code that reaches an unlocked environment read through a call: {}, of them carrying a #[test]-shaped attribute {}",
        report.indirect_offenders.len(),
        report.indirect_offenders_with_a_test_attribute
    );
    for name in &report.indirect_offenders {
        let chain = report
            .indirect_chains
            .get(name)
            .map(|chain| chain.join(" -> "))
            .unwrap_or_default();
        println!("  {name}: {chain}");
    }
    let mut by_file: BTreeMap<&str, usize> = BTreeMap::new();
    let mut first_hops: BTreeMap<&str, usize> = BTreeMap::new();
    for name in &report.indirect_offenders {
        let file = name.split("::").next().unwrap_or(name);
        *by_file.entry(file).or_default() += 1;
        if let Some(hop) = report
            .indirect_chains
            .get(name)
            .and_then(|chain| chain.first())
        {
            *first_hops.entry(hop.as_str()).or_default() += 1;
        }
    }
    println!("indirect offenders by file: {}", by_file.len());
    for (file, count) in &by_file {
        println!("  {file}: {count}");
    }
    println!("indirect offenders by the first hop of the chain:");
    for (hop, count) in &first_hops {
        println!("  {hop}: {count}");
    }
}

/// The figures this file's module doc states (`# What a pass does not
/// show`, and the method share in `# Two checks`), each written as the doc
/// writes it, from `report`. The real-crate test asserts the doc holds every
/// one, so a stale figure fails it (review R7 should-fix 5).
fn documented_figures(report: &LockDiscipline) -> Vec<String> {
    let walked = report.edges.calls_walked;
    let methods = report.edges.method_unresolved;
    let percent = (methods * 100 + walked / 2) / walked.max(1);
    let external = report.edges.path_external;
    let at_type = report.edges.path_external_at_crate_type;
    let not_followed = |prefix: &str| -> usize {
        report
            .not_followed
            .iter()
            .filter(|(heading, _)| heading.starts_with(prefix))
            .map(|(_, sites)| sites.len())
            .sum()
    };
    let strip_file = |name: &str| -> String {
        let name = name.split(" (").next().unwrap_or(name);
        name.split_once("::")
            .map_or(name, |(_, rest)| rest)
            .to_string()
    };
    let impls: Vec<String> = report
        .external_trait_impls_reaching_reads
        .iter()
        .map(|(name, chain)| {
            format!(
                "`{}`, through `{}`",
                strip_file(name),
                chain
                    .last()
                    .map_or_else(String::new, |last| strip_file(last))
            )
        })
        .collect();
    vec![
        format!("(`receiver.name()`, {percent}% of the calls)"),
        format!("(`method unresolved`: {methods} of the {walked} calls walked, {percent}%)"),
        format!(
            "of the {} calls out of the crate, the {at_type} at a written crate type are \
             followed (`path out of the crate at a crate type`: {at_type}) and the other \
             {external} are not (`path out of the crate`: {external}); the printed classes, \
             these two among them, sum to the {walked} calls walked.",
            external + at_type
        ),
        format!(
            "(`impls of other traits outside the crate that reach an unlocked environment \
             read`: {}, {})",
            impls.len(),
            impls.join("; ")
        ),
        format!(
            "(`not followed to one body, unresolved`: {}, each a derived `clone`",
            not_followed("unresolved")
        ),
        format!("(`ambiguous`: {})", not_followed("ambiguous")),
        format!(
            "`macro token paths after a |`: {})",
            report.token_paths_after_a_bar
        ),
        format!("(`macro_rules! definitions`: {})", report.macro_definitions),
        format!(
            "(`references whose guard's lifetime no confined let bounds`: {})",
            report.unconfined_holds
        ),
        format!(
            "(`functions that can hand a held lock to their caller`: {},",
            report.lock_handing_functions.len()
        ),
        format!(
            "(`defaults not followed into a wrapped crate type`: {})",
            report.defaults_not_followed
        ),
        format!(
            "(`cfg_attr attributes carrying an attribute the model reads`: {})",
            report.attributes_in_cfg_attr.len()
        ),
        format!(
            "`a #[macro_use] extern crate`: {})",
            report.macro_use_extern_crate
        ),
    ]
}

/// This file's module doc, its `//!` lines joined by single spaces.
fn module_doc() -> String {
    include_str!("env_lock_discipline.rs")
        .lines()
        .filter_map(|line| line.strip_prefix("//!"))
        .flat_map(str::split_whitespace)
        .collect::<Vec<_>>()
        .join(" ")
}

/// The build configuration of `sqry-daemon` as cargo reports it.
fn daemon_build_cfg() -> BuildCfg {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    BuildCfg::from_cargo_metadata(Path::new(env!("CARGO")), &manifest)
        .unwrap_or_else(|error| panic!("instrument: the build configuration: {error}"))
}

/// U22 (surface parity W4 round 4, design W4-D27 parts 1 and 2; round 7): a
/// test that reaches the environment through a call observes a plant exactly
/// as one that reads it in its own body, so the call must be made under the
/// crate lock too. The edges are the resolved intra-crate references (calls
/// and functions named as values, macro tokens included); the walk follows
/// only the references no guard of their own body covers; and every class of
/// the resolution is printed with both partitions asserted and every
/// reference not followed to one body named, because an unclassified
/// reference would be a blind spot the count of offenders could hide. No
/// impl of an operator trait or of `Drop` may read without the lock, because
/// no edge leads to one from where it runs.
#[test]
fn every_test_that_reaches_the_environment_through_a_call_holds_the_crate_lock() {
    let src = daemon_src_dir();
    let cfg = daemon_build_cfg();
    let report = lock_discipline(&src, cfg.roots(), &cfg);
    println!(
        "files parsed: {} (.rs files under sqry-daemon/src: {}), function items {}",
        report.parsed, report.files, report.functions
    );
    print_resolution(&report);

    assert_eq!(
        report.edges.partition(),
        report.edges.calls_walked,
        "instrument: every call the walk saw is counted in exactly one class"
    );
    assert_eq!(
        report.edges.pending, 0,
        "instrument: every path call is classified by the resolution"
    );
    assert_eq!(
        report.values.partition(),
        report.values.calls_walked,
        "instrument: every value path the walk saw is counted in exactly one class"
    );
    assert_eq!(
        report.values.pending, 0,
        "instrument: every value path is classified by the resolution"
    );
    assert!(
        report.values.path_resolved + report.values.path_const_or_static > 0,
        "instrument: no function or const named as a value was resolved, so the value edges are a zero-case"
    );
    assert!(
        report.edges.from_macros > 0 && report.edges.env_calls > 0,
        "instrument: the macro tokens and the environment calls were read"
    );
    assert!(
        report.module_tree_unmodelled.is_empty(),
        "instrument: the module tree reaches every file: {:?}",
        report.module_tree_unmodelled
    );
    assert!(
        report.edges.calls_walked > 0,
        "instrument: the walk saw no call at all"
    );
    assert!(
        report.edges.path_resolved > 0,
        "instrument: the resolution left no edge, so the walk below proves nothing"
    );
    assert!(
        report.edges.method_unresolved > 0,
        "instrument: no method call was seen, so the unresolved class is a zero-case"
    );
    assert_eq!(
        methods_resolved(&report.edges),
        0,
        "instrument: a method call never resolves, because the receiver's type is not in \
         this parse; looking a method's name up as a path is model B of design section 1.4"
    );
    assert!(
        report.functions > 0 && report.test_code_functions > 0,
        "instrument: the parse found no function or no test code"
    );
    assert!(
        report.indirect_offenders_with_a_test_attribute <= report.indirect_offenders.len(),
        "instrument: an offender carrying a #[test] attribute is an offender"
    );
    assert_eq!(
        report.refused_acquisitions,
        Vec::<String>::new(),
        "an acquisition whose guard dies at once holds nothing: {:#?}",
        report.refused_acquisitions
    );
    assert_eq!(
        report.indirect_offenders,
        Vec::<String>::new(),
        "every function in test code that reaches an environment read over a resolved call \
         chain must hold {CRATE_LOCK} where it makes the call, or reach that read through a \
         function that holds it there: {:#?}",
        report.indirect_chains
    );
    assert!(
        report.operator_and_drop_hazards.is_empty(),
        "an operator or a value going out of scope runs a crate impl with no edge the walk \
         follows, so no impl of an operator trait or of Drop may reach an environment read \
         without {CRATE_LOCK}: {:#?}",
        report.operator_and_drop_hazards
    );
    assert_eq!(
        report.item_position_macro_invocations,
        Vec::<String>::new(),
        "a crate macro_rules! invoked in item position is not expanded, so it could write a \
         test or a helper this gate never reads"
    );
    // The module doc states this run's figures (review R7 should-fix 5).
    let doc = module_doc();
    let figures = documented_figures(&report);
    assert_eq!(
        figures.len(),
        13,
        "instrument: every documented figure is derived"
    );
    let stale: Vec<&String> = figures
        .iter()
        .filter(|figure| !doc.contains(figure.as_str()))
        .collect();
    assert!(
        stale.is_empty(),
        "the module doc must state each figure as this run prints it; update the doc to: \
         {stale:#?}"
    );
    assert!(
        report
            .not_followed
            .iter()
            .filter(|(heading, _)| heading.starts_with("unresolved"))
            .flat_map(|(_, sites)| sites)
            .all(|site| site
                .split(": ")
                .next()
                .is_some_and(|at| at.ends_with("::clone"))),
        "the doc says every unresolved path is a derived clone: {:#?}",
        report.not_followed
    );
}

/// U23 (surface parity W4 round 4, design W4-D27 part 4; round 7, decision
/// D-i7-envlock-1): no function, at a site an accepted shape covers, takes the crate
/// lock again or reaches a function that takes it over the resolved edges
/// (itself included), and no `Drop` or operator impl takes it. `TEST_ENV_LOCK`
/// wraps a `std::sync::Mutex`, which is not reentrant, so the second
/// acquisition would hang. There are no nesting sites and no such impl on this
/// tree, so this test asserts empty lists here. The row that empties
/// `nesting_sites` (W4-M57) leaves this test green and is killed by U21's
/// planted nesting case.
#[test]
fn no_lock_acquirer_reaches_another_acquirer() {
    let src = daemon_src_dir();
    let cfg = daemon_build_cfg();
    let report = lock_discipline(&src, cfg.roots(), &cfg);
    let holders: usize = report
        .holders_by_form
        .iter()
        .filter(|(form, _)| **form != HELD_NONE && **form != HELD_REFUSED)
        .map(|(_, count)| *count)
        .sum();
    println!("functions that hold {CRATE_LOCK}: {holders}");
    for (form, count) in &report.holders_by_form {
        println!("  {form}: {count}");
    }
    println!("nesting sites: {}", report.nesting_sites.len());
    for (holder, reached) in &report.nesting_sites {
        println!("  {holder} reaches {reached}");
    }
    assert!(
        holders > 0,
        "instrument: no function holds the lock, so this test would be vacuous"
    );
    assert_eq!(
        report.edges.partition(),
        report.edges.calls_walked,
        "instrument: every call the walk saw is counted in exactly one class"
    );
    assert_eq!(
        report.nesting_sites,
        Vec::<(String, String)>::new(),
        "a function that holds {CRATE_LOCK} must not reach another holder: the second \
         acquisition of a non-reentrant mutex would hang: {:#?}",
        report.nesting_sites
    );
    assert!(
        report.nesting_hazards.is_empty(),
        "a Drop or operator impl that takes {CRATE_LOCK} runs where a value drops or an \
         operator stands, a guard live there or not: {:#?}",
        report.nesting_hazards
    );
}

/// U21 (surface parity W4 round 4, design W4-D27): the model itself, over a
/// planted tree driven through `lock_discipline`, which is the same entry point
/// the real gate uses. Seven cases: a test that calls an unguarded reader is an
/// offender and its chain is reported; a test two calls away is an offender; a
/// test whose chain runs through a guarded helper is not; a test whose only
/// route is a method call is not, and that call is counted as unresolved; a
/// test whose only route is an ambiguous path call (two cfg variants of one
/// function, one of which reads) is an offender, because an ambiguous call is
/// an edge to every candidate, and that call is counted as ambiguous; a test
/// whose acquisition is dropped at once is an offender and the refused count
/// is 1; and a guarded test that calls a guarded fixture is a nesting site.
#[test]
fn the_lock_discipline_follows_a_call_to_an_environment_reader() {
    let mut failed: Vec<String> = Vec::new();
    let mut checks = 0usize;

    let planted = tempfile::tempdir().expect("a temporary directory");
    let src = planted.path().join("src");
    let files: BTreeMap<&str, &str> = [
        (
            "lib.rs",
            "\
#![allow(let_underscore_lock)]\n\
pub static TEST_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());\n\
#[cfg(test)]\nmod tests;\n\
pub fn reader() -> Option<std::ffi::OsString> {\n    std::env::var_os(\"W4\")\n}\n\
pub fn middle() -> Option<std::ffi::OsString> {\n    reader()\n}\n\
pub fn guarded_middle() -> Option<std::ffi::OsString> {\n    let _env = crate::TEST_ENV_LOCK.lock();\n    reader()\n}\n\
#[cfg(unix)]\npub fn ambiguous_reader() -> Option<std::ffi::OsString> {\n    std::env::var_os(\"W4\")\n}\n\
#[cfg(not(unix))]\npub fn ambiguous_reader() -> Option<std::ffi::OsString> {\n    None\n}\n\
pub struct Holder;\n\
impl Holder {\n    pub fn reads_through_a_method(&self) -> Option<std::ffi::OsString> {\n        std::env::var_os(\"W4\")\n    }\n}\n",
        ),
        (
            "tests.rs",
            "\
#[test]\nfn calls_an_unguarded_reader() {\n    let _ = crate::reader();\n}\n\
#[test]\nfn two_calls_away() {\n    let _ = crate::middle();\n}\n\
#[test]\nfn through_a_guarded_helper() {\n    let _ = crate::guarded_middle();\n}\n\
#[test]\nfn only_a_method_call_reaches_the_read() {\n    let holder = crate::Holder;\n    let _ = holder.reads_through_a_method();\n}\n\
#[test]\nfn an_ambiguous_path_call_reaches_the_read() {\n    let _ = crate::ambiguous_reader();\n}\n\
#[test]\nfn drops_the_guard_at_once() {\n    let _ = crate::TEST_ENV_LOCK.lock();\n    let _ = crate::reader();\n}\n\
#[test]\nfn guarded_and_calls_a_guarded_fixture() {\n    let _env = crate::TEST_ENV_LOCK.lock();\n    let _ = fixture();\n}\n\
fn fixture() -> Option<std::ffi::OsString> {\n    let _env = crate::TEST_ENV_LOCK.lock();\n    crate::reader()\n}\n",
        ),
    ]
    .into_iter()
    .collect();
    for (relative, text) in &files {
        let path = src.join(relative);
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("mkdir");
        std::fs::write(&path, text).expect("write");
    }
    assert_eq!(assert_compiles(&src), 2, "instrument: both builds compiled");
    let roots = vec![src.join("lib.rs")];
    let cfg = BuildCfg::new(roots.clone(), BTreeMap::new());
    let report = lock_discipline(&src, &roots, &cfg);
    print_resolution(&report);

    // The planted tree is classified as the cases assume.
    println!(
        "U21 planted: files {}, live {}, test-only {}, unreachable {}, unmodelled {}, root errors {}",
        report.files,
        report.live_files,
        report.test_only_files,
        report.unreachable_files,
        report.unmodelled.len(),
        report.root_errors.len()
    );
    checks += 1;
    if (
        report.files,
        report.live_files,
        report.test_only_files,
        report.unreachable_files,
    ) != (2, 1, 1, 0)
        || !report.unmodelled.is_empty()
        || !report.root_errors.is_empty()
    {
        failed.push(format!(
            "the planted tree classified as {:?} with unmodelled {:?} and root errors {:?}",
            report.non_live_files, report.unmodelled, report.root_errors
        ));
    }

    // Cases 1, 2, 3, 5 and 6: who reaches an unlocked read through a call.
    let expected_offenders = vec![
        "tests.rs::an_ambiguous_path_call_reaches_the_read".to_string(),
        "tests.rs::calls_an_unguarded_reader".to_string(),
        "tests.rs::drops_the_guard_at_once".to_string(),
        "tests.rs::two_calls_away".to_string(),
    ];
    checks += 1;
    if report.indirect_offenders != expected_offenders {
        failed.push(format!(
            "indirect offenders {:?}, expected {expected_offenders:?}",
            report.indirect_offenders
        ));
    }

    // Cases 1, 2 and 5: the chain is reported, shortest first hop first.
    let expected_chains: BTreeMap<&str, Vec<&str>> = [
        (
            "tests.rs::an_ambiguous_path_call_reaches_the_read",
            vec!["lib.rs::ambiguous_reader"],
        ),
        (
            "tests.rs::calls_an_unguarded_reader",
            vec!["lib.rs::reader"],
        ),
        (
            "tests.rs::two_calls_away",
            vec!["lib.rs::middle", "lib.rs::reader"],
        ),
        ("tests.rs::drops_the_guard_at_once", vec!["lib.rs::reader"]),
    ]
    .into_iter()
    .collect();
    for (name, expected) in &expected_chains {
        let actual: Vec<&str> = report
            .indirect_chains
            .get(*name)
            .map(|chain| chain.iter().map(String::as_str).collect())
            .unwrap_or_default();
        println!("U21 chain of {name}: {actual:?} (expected {expected:?})");
        checks += 1;
        if actual != *expected {
            failed.push(format!(
                "the chain of {name} is {actual:?}, expected {expected:?}"
            ));
        }
    }

    // Case 6: the acquisition that dies at once holds nothing.
    let expected_refused = vec!["tests.rs::drops_the_guard_at_once".to_string()];
    checks += 1;
    if report.refused_acquisitions != expected_refused {
        failed.push(format!(
            "refused acquisitions {:?}, expected {expected_refused:?}",
            report.refused_acquisitions
        ));
    }

    // Case 7: a guarded test that calls a guarded fixture would hang.
    let expected_nesting = vec![(
        "tests.rs::guarded_and_calls_a_guarded_fixture".to_string(),
        "tests.rs::fixture".to_string(),
    )];
    checks += 1;
    if report.nesting_sites != expected_nesting {
        failed.push(format!(
            "nesting sites {:?}, expected {expected_nesting:?}",
            report.nesting_sites
        ));
    }

    // Cases 4 and 5: the method call the resolution refuses to invent and the
    // ambiguous call it follows to every candidate, counted.
    let edges = (
        report.edges.calls_walked,
        report.edges.path_resolved,
        report.edges.path_ambiguous,
        report.edges.path_external,
        report.edges.method_unresolved,
        report.edges.env_calls,
        report.edges.other_callee,
        report.edges.local_binding,
        report.edges.pending,
    );
    println!(
        "U21 edges (walked, resolved, ambiguous, out of the crate, method, env, other, local, pending): {edges:?}"
    );
    checks += 1;
    if edges != (18, 8, 1, 1, 5, 3, 0, 0, 0) {
        failed.push(format!(
            "the planted tree's edges are {edges:?}, expected (18, 8, 1, 1, 5, 3, 0, 0, 0)"
        ));
    }
    checks += 1;
    if report.edges.partition() != report.edges.calls_walked {
        failed.push(format!(
            "the edge classes sum to {}, not to the {} calls walked",
            report.edges.partition(),
            report.edges.calls_walked
        ));
    }

    // The acquisitions by the shape that keeps them, and the direct rule,
    // which this tree is a zero-case for: every environment call in it is
    // production code, so U15 keeps the direct rule's cases and this test adds
    // none.
    let shapes: Vec<(&str, usize)> = report
        .acquisitions_by_shape
        .iter()
        .map(|(shape, count)| (*shape, *count))
        .collect();
    println!("U21 acquisitions by shape: {shapes:?}");
    checks += 1;
    if shapes
        != vec![
            ("kept by a shape-1 let", 3),
            ("kept by no accepted shape", 1),
        ]
    {
        failed.push(format!(
            "acquisitions by shape {shapes:?}, expected 3 kept by a shape-1 let and 1 by none"
        ));
    }
    println!(
        "U21 direct rule: environment-touching test code {}, offenders {:?}",
        report.env_test_code, report.offenders
    );
    checks += 1;
    if report.env_test_code != 0 || !report.offenders.is_empty() {
        failed.push(format!(
            "the planted tree's direct rule found {} environment-touching test-code functions and offenders {:?}, expected none of either",
            report.env_test_code, report.offenders
        ));
    }

    println!("U21: checks {checks}, failed {}", failed.len());
    assert_eq!(checks, 12, "U21 runs every check it declares");
    assert!(
        failed.is_empty(),
        "{} check(s) failed:\n{}",
        failed.len(),
        failed.join("\n")
    );
}

/// U24 (integration of W1 and W4): local bindings shadow a free function only
/// where Rust scopes them. One planted file tries every binding site the walk
/// reads (a parameter, a `let` and a `let` with `else`, a closure parameter
/// typed or not, `for`, `if let`, `while let` and a let chain, a match arm and
/// an if-let guard) and every binding pattern (identifier, tuple, slice, or,
/// struct field and shorthand, tuple-struct, captured, `mut`, `ref`, `&`) from
/// both sides. Inside a binding's scope a bare call of `reader` is the local
/// and is not followed; outside it (after its block, in its own initializer,
/// in a later argument of the call that holds the closure, in the `else`
/// branch, in a later arm, in the scrutinee or the iterator) the call is the
/// crate's `reader` and is followed. A path in a pattern (a struct's path, a
/// tuple struct's path, an associated const) and a match guard bind nothing,
/// a bare name as the guard included (`if READY`, a static whose initializer
/// names the reader, which is how a guard read as a pattern loses the edge:
/// W4-M77). The test functions named in `expected` are the second side;
/// every other test function is the first. A tuple struct's path read as a
/// binding changes no verdict (W4-M76): Rust resolves a tuple-struct pattern's
/// path in the value namespace, so a call of that name in the arm is the same
/// constructor, and a constructor, like a local, is followed to no body. So
/// `a_tuple_struct_path_is_not_a_binding` pins it by count instead: its call
/// is the planted file's one call to a crate constructor. (A struct pattern's
/// path is a type identifier, which no pattern kind binds, so it cannot pin
/// this.) An item of an inner block beats a local of an outer one: the call
/// in the block is the item, and is followed (the item itself is test code
/// that reads, so it is reported too).
#[test]
fn the_lock_discipline_scopes_local_bindings_like_rust() {
    let planted = tempfile::tempdir().expect("a temporary directory");
    let src = planted.path().join("src");
    let files: BTreeMap<&str, &str> = [
        (
            "lib.rs",
            "\
#![feature(if_let_guard)]\n\
pub static TEST_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());\n\
#[cfg(test)]\nmod tests;\n\
pub fn reader() -> Option<std::ffi::OsString> {\n    std::env::var_os(\"W4\")\n}\n\
pub static READY: bool = {\n    let _named = crate::reader;\n    true\n};\n",
        ),
        (
            "tests.rs",
            "\
use crate::reader;\n\
use crate::READY;\n\
#[test]\nfn a_bare_match_guard_is_not_a_binding() {\n    match Some(0u8) {\n        Some(_) if READY => {}\n        _ => {}\n    }\n}\n\
#[test]\nfn shadow_ends_with_its_block() {\n    {\n        let reader = || None::<std::ffi::OsString>;\n        let _ = reader();\n    }\n    let _ = reader();\n}\n\
#[test]\nfn initializer_is_not_in_scope() {\n    let reader = reader();\n    let _ = reader;\n}\n\
#[test]\nfn closure_parameter_ends_with_its_closure() {\n    let f = |reader: Option<std::ffi::OsString>| reader;\n    let _ = f(None);\n    let _ = reader();\n}\n\
#[test]\nfn a_visible_local_shadows_the_reader() {\n    let reader = || None::<std::ffi::OsString>;\n    let _ = reader();\n}\n\
fn helper_with_a_destructured_parameter((reader, _n): (fn() -> Option<std::ffi::OsString>, u8)) {\n    let _ = reader();\n}\n\
#[test]\nfn a_destructured_parameter_shadows() {\n    helper_with_a_destructured_parameter((|| None, 0));\n}\n\
#[test]\nfn an_untyped_closure_parameter_shadows_inside_its_closure() {\n    fn apply(f: impl Fn(fn() -> Option<std::ffi::OsString>) -> Option<std::ffi::OsString>) -> Option<std::ffi::OsString> {\n        f(|| None)\n    }\n    let _ = apply(|reader| reader());\n}\n\
#[test]\nfn a_while_let_binding_shadows_in_its_body() {\n    while let Some(reader) = Some(|| None::<std::ffi::OsString>) {\n        let _ = reader();\n        break;\n    }\n}\n\
#[test]\nfn a_let_else_binding_shadows_after_it() {\n    let Some(reader) = Some(|| None::<std::ffi::OsString>) else {\n        return;\n    };\n    let _ = reader();\n}\n\
#[test]\nfn a_field_pattern_binds_its_sub_pattern() {\n    struct Holder { inner: fn() -> Option<std::ffi::OsString> }\n    let Holder { inner: reader } = Holder { inner: || None };\n    let _ = reader();\n}\n\
#[test]\nfn a_slice_pattern_shadows() {\n    let [reader] = [|| None::<std::ffi::OsString>];\n    let _ = reader();\n}\n\
#[test]\nfn an_or_pattern_shadows() {\n    match Ok::<fn() -> Option<std::ffi::OsString>, fn() -> Option<std::ffi::OsString>>(|| None) {\n        Ok(reader) | Err(reader) => {\n            let _ = reader();\n        }\n    }\n}\n\
#[test]\nfn a_captured_pattern_shadows() {\n    let reader @ _ = || None::<std::ffi::OsString>;\n    let _ = reader();\n}\n\
#[test]\nfn a_mut_pattern_shadows() {\n    let (mut reader, _n) = (|| None::<std::ffi::OsString>, 0);\n    let _ = reader();\n}\n\
#[test]\nfn a_ref_pattern_shadows() {\n    let (ref reader, _n) = (|| None::<std::ffi::OsString>, 0);\n    let _ = reader();\n}\n\
#[test]\nfn a_reference_pattern_shadows() {\n    let (&reader, _n) = (&(|| None::<std::ffi::OsString>), 0);\n    let _ = reader();\n}\n\
#[test]\nfn an_if_let_guard_binding_shadows_in_its_arm() {\n    match Some(0) {\n        Some(_) if let Some(reader) = Some(|| None::<std::ffi::OsString>) => {\n            let _ = reader();\n        }\n        _ => {}\n    }\n}\n\
#[test]\nfn a_while_let_binding_ends_with_its_loop() {\n    while let Some(reader) = Some(0) {\n        let _ = reader;\n        break;\n    }\n    let _ = reader();\n}\n\
#[test]\nfn a_let_else_binding_is_not_in_its_else() {\n    let Some(reader) = Some(0) else {\n        let _ = reader();\n        return;\n    };\n    let _ = reader;\n}\n\
#[test]\nfn a_field_name_is_not_a_binding() {\n    struct Holder { reader: u8 }\n    let Holder { reader: _inner } = Holder { reader: 0 };\n    let _ = reader();\n}\n\
#[test]\nfn an_if_let_guard_binding_ends_with_its_arm() {\n    match Some(0) {\n        Some(_) if let Some(reader) = Some(0) => {\n            let _ = reader;\n        }\n        _ => {\n            let _ = reader();\n        }\n    }\n}\n\
#[test]\nfn a_for_iterator_is_not_in_scope() {\n    for reader in [reader()] {\n        let _ = reader;\n    }\n}\n\
#[test]\nfn an_if_let_scrutinee_is_not_in_scope() {\n    if let Some(reader) = reader() {\n        let _ = reader;\n    }\n}\n\
#[test]\nfn a_while_let_scrutinee_is_not_in_scope() {\n    while let Some(reader) = reader() {\n        let _ = reader;\n        break;\n    }\n}\n\
#[test]\nfn an_inner_block_item_beats_an_outer_local() {\n    let reader = || None::<std::ffi::OsString>;\n    let _ = reader();\n    {\n        fn reader() -> Option<std::ffi::OsString> {\n            crate::reader()\n        }\n        let _ = reader();\n    }\n}\n\
#[test]\nfn a_typed_closure_parameter_shadows_inside_its_closure() {\n    let f = |reader: fn() -> Option<std::ffi::OsString>| reader();\n    let _ = f(|| None);\n}\n\
#[test]\nfn a_destructured_let_shadows() {\n    let (reader, _n) = (|| None::<std::ffi::OsString>, 0);\n    let _ = reader();\n}\n\
#[test]\nfn a_struct_shorthand_let_shadows() {\n    struct Holder { reader: fn() -> Option<std::ffi::OsString> }\n    let Holder { reader } = Holder { reader: || None };\n    let _ = reader();\n}\n\
#[test]\nfn an_if_let_binding_shadows_in_its_block() {\n    if let Some(reader) = Some(|| None::<std::ffi::OsString>) {\n        let _ = reader();\n    }\n}\n\
#[test]\nfn a_let_chain_binding_shadows_in_later_conditions() {\n    if let Some(reader) = Some(|| Some(std::ffi::OsString::new())) && reader().is_some() {\n        let _ = reader();\n    }\n}\n\
#[test]\nfn a_match_arm_binding_shadows_in_its_arm() {\n    match Some(|| None::<std::ffi::OsString>) {\n        Some(reader) if reader().is_none() => {\n            let _ = reader();\n        }\n        _ => {}\n    }\n}\n\
#[test]\nfn a_for_binding_shadows_in_its_body() {\n    for reader in [|| None::<std::ffi::OsString>] {\n        let _ = reader();\n    }\n}\n\
#[test]\nfn a_destructured_initializer_is_not_in_scope() {\n    let (reader, _n) = (reader(), 0);\n    let _ = reader;\n}\n\
#[test]\nfn an_if_let_binding_ends_with_its_block() {\n    if let Some(reader) = Some(0) {\n        let _ = reader;\n    }\n    let _ = reader();\n}\n\
#[test]\nfn an_if_let_binding_is_not_in_its_else() {\n    if let Some(reader) = Some(0) {\n        let _ = reader;\n    } else {\n        let _ = reader();\n    }\n}\n\
#[test]\nfn a_match_arm_binding_ends_with_its_arm() {\n    match Some(0) {\n        Some(reader) => {\n            let _ = reader;\n        }\n        None => {\n            let _ = reader();\n        }\n    }\n}\n\
#[test]\nfn a_match_guard_is_not_a_binding() {\n    match Some(0) {\n        Some(_) if { let _ = reader; true } => {\n            let _ = reader();\n        }\n        _ => {}\n    }\n}\n\
#[test]\nfn a_pattern_path_is_not_a_binding() {\n    #[allow(non_camel_case_types)]\n    struct reader {\n        field: u8,\n    }\n    match (reader { field: 0 }) {\n        reader { field: _ } => {\n            let _ = reader();\n        }\n    }\n}\n\
#[test]\nfn a_for_binding_ends_with_its_body() {\n    for reader in [0] {\n        let _ = reader;\n    }\n    let _ = reader();\n}\n\
#[test]\nfn a_closure_parameter_is_not_in_a_later_argument() {\n    fn take(_f: impl Fn(u8) -> u8, _v: Option<std::ffi::OsString>) {}\n    take(|reader: u8| reader, reader());\n}\n\
#[test]\nfn a_let_in_an_if_is_not_in_its_else() {\n    if std::hint::black_box(false) {\n        let reader = || None::<std::ffi::OsString>;\n        let _ = reader();\n    } else {\n        let _ = reader();\n    }\n}\n\
#[test]\nfn an_associated_const_path_pattern_binds_nothing() {\n    struct P;\n    impl P {\n        #[allow(non_upper_case_globals)]\n        const reader: u8 = 5;\n    }\n    match 5u8 {\n        P::reader => {\n            let _ = reader();\n        }\n        _ => {}\n    }\n}\n\
#[test]\nfn a_tuple_struct_path_is_not_a_binding() {\n    #[allow(non_camel_case_types, dead_code)]\n    struct reader(u8);\n    match None {\n        Some(reader(_n)) => {\n            let _ = reader(0);\n        }\n        None => {}\n    }\n}\n",
        ),
    ]
    .into_iter()
    .collect();
    for (relative, text) in &files {
        let path = src.join(relative);
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("mkdir");
        std::fs::write(&path, text).expect("write");
    }
    assert_eq!(assert_compiles(&src), 2, "instrument: both builds compiled");
    let roots = vec![src.join("lib.rs")];
    let cfg = BuildCfg::new(roots.clone(), BTreeMap::new());
    let report = lock_discipline(&src, &roots, &cfg);
    print_resolution(&report);
    let mut expected: Vec<String> = [
        "closure_parameter_ends_with_its_closure",
        "initializer_is_not_in_scope",
        "shadow_ends_with_its_block",
        "a_destructured_initializer_is_not_in_scope",
        "an_if_let_binding_ends_with_its_block",
        "an_if_let_binding_is_not_in_its_else",
        "a_match_arm_binding_ends_with_its_arm",
        "a_pattern_path_is_not_a_binding",
        "a_match_guard_is_not_a_binding",
        "a_for_binding_ends_with_its_body",
        "a_while_let_binding_ends_with_its_loop",
        "a_let_else_binding_is_not_in_its_else",
        "a_field_name_is_not_a_binding",
        "an_if_let_guard_binding_ends_with_its_arm",
        "a_for_iterator_is_not_in_scope",
        "an_if_let_scrutinee_is_not_in_scope",
        "a_while_let_scrutinee_is_not_in_scope",
        "a_closure_parameter_is_not_in_a_later_argument",
        "a_let_in_an_if_is_not_in_its_else",
        "an_associated_const_path_pattern_binds_nothing",
        "a_bare_match_guard_is_not_a_binding",
        "an_inner_block_item_beats_an_outer_local",
        "an_inner_block_item_beats_an_outer_local::reader",
    ]
    .iter()
    .map(|name| format!("tests.rs::{name}"))
    .collect();
    expected.sort();
    assert_eq!(
        report.indirect_offenders, expected,
        "every out-of-scope call is followed to the reader; no call a visible local \
         binds is (every binding site: parameter, let and let-else, closure \
         parameter typed or not, for, if let, while let and let chain, match arm \
         and if-let guard; every binding pattern: identifier, tuple, slice, or, \
         struct field and shorthand, tuple-struct, captured, mut, ref, &)"
    );
    // A tuple struct's path in a pattern binds nothing (W4-M76): the call of
    // that name in the arm is the struct's constructor, the one crate
    // constructor the planted file calls. A constructor and a local are both
    // followed to no body, so no verdict can tell them apart; the count does.
    assert_eq!(
        report.edges.path_constructor, 1,
        "the call in a_tuple_struct_path_is_not_a_binding's arm is the tuple struct's \
         constructor, not a local the pattern's path binds"
    );
}

// ---------------------------------------------------------------------------
// The class tests: one planted crate per class of the 2026-10-01 audit, each
// from both sides. A case named `ok_*` is code Rust runs safely, which the
// gate must not report; a case named `off_*` is code the gate must report.
// ---------------------------------------------------------------------------

/// The crate root every class test starts from: the crate lock, an
/// environment reader and a function that reads nothing.
const PLANTED_ROOT: &str = r#"pub static TEST_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub fn reader() -> Option<std::ffi::OsString> {
    std::env::var_os("W4")
}

pub fn no_read() -> Option<std::ffi::OsString> {
    None
}
"#;

/// The stand-in for serde: its `Deserialize` derive, which generates nothing,
/// and the `serde` helper attribute the planted trees write.
const STAND_IN_SERDE: &str = r#"
extern crate proc_macro;
use proc_macro::TokenStream;
#[proc_macro_derive(Deserialize, attributes(serde))]
pub fn deserialize(_input: TokenStream) -> TokenStream {
    TokenStream::new()
}
"#;

/// The stand-in for serde_json: `from_str`, whose result type is the
/// caller's, and its error.
const STAND_IN_SERDE_JSON: &str = r#"
#[derive(Debug)]
pub struct Error;
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a stand-in")
    }
}
impl std::error::Error for Error {}
pub fn from_str<T>(_text: &str) -> Result<T, Error> {
    Err(Error)
}
"#;

/// The stand-in for clap's derives: each implements the traits the derive
/// implements for the type it is written on (no generics).
const STAND_IN_CLAP_DERIVE: &str = r#"
extern crate proc_macro;
use proc_macro::{TokenStream, TokenTree};
fn name(input: TokenStream) -> String {
    let mut tokens = input.into_iter();
    while let Some(token) = tokens.next() {
        if let TokenTree::Ident(ident) = &token {
            let word = ident.to_string();
            if word == "struct" || word == "enum" {
                if let Some(TokenTree::Ident(name)) = tokens.next() {
                    return name.to_string();
                }
            }
        }
    }
    String::new()
}
fn implement(input: TokenStream, traits: &[&str]) -> TokenStream {
    let name = name(input);
    traits
        .iter()
        .map(|t| format!("impl ::clap::{t} for {name} {{}}"))
        .collect::<String>()
        .parse()
        .expect("impls")
}
#[proc_macro_derive(Parser, attributes(arg, command, clap))]
pub fn parser(input: TokenStream) -> TokenStream {
    implement(input, &["Parser", "CommandFactory", "Args"])
}
#[proc_macro_derive(Args, attributes(arg, command, clap))]
pub fn args(input: TokenStream) -> TokenStream {
    implement(input, &["Args"])
}
#[proc_macro_derive(Subcommand, attributes(arg, command, clap))]
pub fn subcommand(input: TokenStream) -> TokenStream {
    implement(input, &["Subcommand"])
}
"#;

/// The stand-in for clap: its derives and the traits they implement, with
/// the parsing methods the planted trees call, and an `assert!` of its own.
const STAND_IN_CLAP: &str = r#"
pub use clap_derive::{Args, Parser, Subcommand};
pub struct Command;
#[derive(Debug)]
pub struct Error;
pub trait CommandFactory {
    fn command() -> Command {
        Command
    }
}
pub trait Args {
    fn augment_args(command: Command) -> Command {
        command
    }
}
pub trait Subcommand {
    fn augment_subcommands(command: Command) -> Command {
        command
    }
}
/// A trait with a method named `drop`, which is not `Drop::drop`.
pub trait Finish {
    fn drop(&mut self);
}
/// A macro named like the standard library's `assert!` that does not run
/// its tokens in place: they run on another thread, after the caller's guard
/// may be gone.
#[macro_export]
macro_rules! assert {
    ($e:expr) => {
        let _ = ::std::thread::spawn(move || {
            let _ = $e;
        });
    };
}
pub trait Parser: Sized + CommandFactory {
    fn parse() -> Self {
        unimplemented!("a stand-in")
    }
    fn try_parse_from<I, T>(_arguments: I) -> Result<Self, Error>
    where
        I: IntoIterator<Item = T>,
    {
        Err(Error)
    }
}
"#;

/// The toolchain's `rustc`: beside the `cargo` that builds this test.
fn toolchain_rustc() -> PathBuf {
    Path::new(env!("CARGO")).with_file_name(format!("rustc{}", std::env::consts::EXE_SUFFIX))
}

/// Runs the toolchain's `rustc` and fails with its output when it fails. A
/// planted tree that enables a feature gate (`#![feature(..)]`, U24's
/// if-let guards) is nightly Rust: it is compiled with `RUSTC_BOOTSTRAP=1`,
/// which lets the pinned toolchain accept the gate.
fn run_rustc(arguments: &[std::ffi::OsString], what: &str) {
    run_rustc_with(arguments, what, false);
}

fn run_rustc_with(arguments: &[std::ffi::OsString], what: &str, bootstrap: bool) {
    let mut command = std::process::Command::new(toolchain_rustc());
    if bootstrap {
        command.env("RUSTC_BOOTSTRAP", "1");
    }
    let output = command
        .args(arguments)
        .output()
        .unwrap_or_else(|error| panic!("instrument: rustc could not start for {what}: {error}"));
    assert!(
        output.status.success(),
        "{what} does not compile with {}:\n{}",
        toolchain_rustc().display(),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The stand-ins built once per toolchain and source (`serde`,
/// `serde_json`, `clap`), as the `--extern` and `-L` arguments a planted tree
/// is compiled with. They are kept under cargo's target `tmp` directory, keyed
/// by a hash of their sources and of the toolchain's `rustc` path, so a later
/// run reuses them and no run leaves a directory behind; a build goes to a
/// directory of its own first and is renamed into place, so two test
/// processes building at once never read a half-written one.
fn stand_ins() -> &'static Vec<std::ffi::OsString> {
    static STAND_INS: std::sync::OnceLock<Vec<std::ffi::OsString>> = std::sync::OnceLock::new();
    STAND_INS.get_or_init(|| {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        (
            STAND_IN_SERDE,
            STAND_IN_SERDE_JSON,
            STAND_IN_CLAP_DERIVE,
            STAND_IN_CLAP,
        )
            .hash(&mut hasher);
        toolchain_rustc().hash(&mut hasher);
        let base = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("env-lock-stand-ins-{:016x}", hasher.finish()));
        let library = |dir: &Path, name: &str| {
            dir.join(format!(
                "{}{name}{}",
                std::env::consts::DLL_PREFIX,
                std::env::consts::DLL_SUFFIX
            ))
        };
        let rlib = |dir: &Path, name: &str| dir.join(format!("lib{name}.rlib"));
        if !base.join("ready").exists() {
            let building = base.with_extension(format!("building-{}", std::process::id()));
            std::fs::create_dir_all(&building).expect("a directory for the stand-ins");
            let build =
                |name: &str, text: &str, kind: &str, out: &Path, externs: &[(&str, &Path)]| {
                    let source = building.join(format!("{name}.rs"));
                    std::fs::write(&source, text).expect("write a stand-in");
                    let mut arguments: Vec<std::ffi::OsString> = [
                        "--edition",
                        "2024",
                        "--crate-type",
                        kind,
                        "--crate-name",
                        name,
                        "--cap-lints",
                        "allow",
                        "-o",
                    ]
                    .iter()
                    .map(Into::into)
                    .collect();
                    arguments.push(out.into());
                    for (crate_name, path) in externs {
                        arguments.push("--extern".into());
                        let mut value = std::ffi::OsString::from(format!("{crate_name}="));
                        value.push(path);
                        arguments.push(value);
                    }
                    arguments.push(source.into());
                    run_rustc(&arguments, &format!("the {name} stand-in"));
                };
            build(
                "serde",
                STAND_IN_SERDE,
                "proc-macro",
                &library(&building, "serde"),
                &[],
            );
            build(
                "serde_json",
                STAND_IN_SERDE_JSON,
                "rlib",
                &rlib(&building, "serde_json"),
                &[],
            );
            build(
                "clap_derive",
                STAND_IN_CLAP_DERIVE,
                "proc-macro",
                &library(&building, "clap_derive"),
                &[],
            );
            build(
                "clap",
                STAND_IN_CLAP,
                "rlib",
                &rlib(&building, "clap"),
                &[("clap_derive", &library(&building, "clap_derive"))],
            );
            std::fs::write(building.join("ready"), b"").expect("mark the stand-ins built");
            if std::fs::rename(&building, &base).is_err() {
                // Another process put its build in place first; use that one.
                let _ = std::fs::remove_dir_all(&building);
            }
        }
        assert!(
            base.join("ready").exists(),
            "instrument: the stand-ins are built at {}",
            base.display()
        );
        let mut externs: Vec<std::ffi::OsString> = Vec::new();
        for (name, path) in [
            ("serde", library(&base, "serde")),
            ("serde_json", rlib(&base, "serde_json")),
            ("clap", rlib(&base, "clap")),
        ] {
            externs.push("--extern".into());
            let mut value = std::ffi::OsString::from(format!("{name}="));
            value.push(path);
            externs.push(value);
        }
        externs.push("-L".into());
        let mut search = std::ffi::OsString::from("dependency=");
        search.push(&base);
        externs.push(search);
        externs
    })
}

/// Every planted tree is valid Rust: compiled with the toolchain's `rustc`
/// (`--edition 2024 --crate-type lib`, `lib.rs` its root), as a test build
/// (`--test`) and as a non-test build, against the stand-ins for serde,
/// serde_json and clap (`stand_ins`), so a fixture that rots fails its test
/// with rustc's errors. Returns the number of builds checked.
fn assert_compiles(src: &Path) -> usize {
    let out = tempfile::tempdir().expect("a directory for the build");
    let mut builds = 0;
    for test in [true, false] {
        let mut arguments: Vec<std::ffi::OsString> = [
            "--edition",
            "2024",
            "--crate-type",
            "lib",
            "--crate-name",
            "planted",
            "--emit=metadata",
            "-o",
        ]
        .iter()
        .map(Into::into)
        .collect();
        arguments.push(out.path().join("planted.rmeta").into());
        if test {
            arguments.push("--test".into());
        }
        arguments.extend(stand_ins().iter().cloned());
        arguments.push(src.join("lib.rs").into());
        let gated = std::fs::read_to_string(src.join("lib.rs"))
            .expect("read the planted root")
            .contains("#![feature(");
        run_rustc_with(
            &arguments,
            &format!(
                "the planted tree at {} ({})",
                src.display(),
                if test {
                    "a test build"
                } else {
                    "a non-test build"
                }
            ),
            gated,
        );
        builds += 1;
    }
    builds
}

/// A planted crate: `files` under a temporary `src`, `lib.rs` its only root,
/// and the lock discipline over it, with the instruments the real gate
/// asserts asserted here too: every file parses cleanly and is reached, the
/// module tree models it all, and every reference is classified once.
fn planted(files: &[(&str, String)]) -> LockDiscipline {
    let planted = tempfile::tempdir().expect("a temporary directory");
    let src = planted.path().join("src");
    for (relative, text) in files {
        let path = src.join(relative);
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("mkdir");
        std::fs::write(&path, text).expect("write");
    }
    assert_eq!(assert_compiles(&src), 2, "instrument: both builds compiled");
    let roots = vec![src.join("lib.rs")];
    let cfg = BuildCfg::new(roots.clone(), BTreeMap::new());
    let report = lock_discipline(&src, &roots, &cfg);
    print_resolution(&report);
    assert_eq!(
        (report.files, report.parsed),
        (files.len(), files.len()),
        "instrument: every planted file is parsed"
    );
    assert!(
        report.root_errors.is_empty(),
        "instrument: every planted parse is clean: {:?}",
        report.root_errors
    );
    assert!(
        report.unmodelled.is_empty() && report.module_tree_unmodelled.is_empty(),
        "instrument: the planted tree is modelled: {:?} {:?}",
        report.unmodelled,
        report.module_tree_unmodelled
    );
    assert_eq!(
        report.unreachable_files, 0,
        "instrument: every planted file is reached from the root"
    );
    for accounting in [&report.edges, &report.values] {
        assert_eq!(
            accounting.partition(),
            accounting.calls_walked,
            "instrument: every reference is counted in exactly one class"
        );
        assert_eq!(accounting.pending, 0, "instrument: nothing is left pending");
        assert_eq!(
            methods_resolved(accounting),
            0,
            "instrument: a method call never resolves"
        );
    }
    report
}

/// The references written as a method call that the resolution classified as
/// anything but an unresolved method. The model never resolves one, so this
/// is zero whenever the model is the one this file documents.
fn methods_resolved(accounting: &EdgeAccounting) -> usize {
    accounting
        .by_qualifier
        .iter()
        .filter(|((qualifier, class), _)| {
            *qualifier == Qualifier::Method.class() && *class != EdgeClass::MethodUnresolved.name()
        })
        .map(|(_, count)| *count)
        .sum()
}

/// Labels of `names` in `file`, sorted, as the report prints them.
fn labels(file: &str, names: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = names.iter().map(|name| format!("{file}::{name}")).collect();
    out.sort();
    out
}

/// Finding 1: only the crate's own `TEST_ENV_LOCK` counts as held. The crate
/// lock is the static declared at the crate root, reached by a path that
/// resolves to it (`crate::`, `self::` at the root, `super::` from a child),
/// by a `use` alias of it in scope where it is used, or by a bare name no
/// closer declaration shadows (a glob import brings it in, and it is in scope
/// again after an inner block's static of that name ends). A module-local,
/// block-local or `let`-bound mutex of the same name (Rust allows the `let`
/// only where no static of the name is in scope), a path to another module's
/// static of that name, an alias of another mutex, and an alias declared in
/// another function are not the crate lock.
#[test]
fn the_lock_discipline_knows_the_crate_lock_by_resolution() {
    let report = planted(&[
        (
            "lib.rs",
            format!(
                r#"{PLANTED_ROOT}
pub mod local_lock {{
    pub static TEST_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
}}

pub mod other {{
    pub static OTHER: std::sync::Mutex<()> = std::sync::Mutex::new(());
}}

#[cfg(test)]
mod copied_lock_tests;
#[cfg(test)]
mod shadow_tests;
#[cfg(test)]
mod glob_tests;
#[cfg(test)]
mod alias_tests;
#[cfg(test)]
mod other_alias_tests;

#[test]
fn ok_bare_name_at_the_crate_root() {{
    let _g = TEST_ENV_LOCK.lock();
    unsafe {{ std::env::set_var("W4", "1") }};
}}

#[test]
fn ok_self_path_at_the_crate_root() {{
    let _g = self::TEST_ENV_LOCK.lock();
    unsafe {{ std::env::set_var("W4", "1") }};
}}
"#
            ),
        ),
        (
            "copied_lock_tests.rs",
            r#"static TEST_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn off_module_copy_of_the_crate_lock() {
    let _g = TEST_ENV_LOCK.lock().unwrap();
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn off_module_copy_of_the_crate_lock_indirect() {
    let _g = TEST_ENV_LOCK.lock().unwrap();
    let _ = crate::reader();
}

#[test]
fn ok_crate_path_past_the_module_copy() {
    let _g = crate::TEST_ENV_LOCK.lock().unwrap();
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn ok_super_path_past_the_module_copy() {
    let _g = super::TEST_ENV_LOCK.lock().unwrap();
    unsafe { std::env::set_var("W4", "1") };
}
"#
            .to_string(),
        ),
        (
            "shadow_tests.rs",
            r#"#![allow(non_snake_case)]

#[test]
fn off_local_mutex_named_like_the_crate_lock() {
    let TEST_ENV_LOCK = std::sync::Mutex::new(());
    let _g = TEST_ENV_LOCK.lock();
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn off_block_static_named_like_the_crate_lock() {
    static TEST_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _g = TEST_ENV_LOCK.lock();
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn off_module_local_static_by_its_path() {
    let _g = crate::local_lock::TEST_ENV_LOCK.lock();
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn off_local_mutex_named_like_the_crate_lock_indirect() {
    let TEST_ENV_LOCK = std::sync::Mutex::new(());
    let _g = TEST_ENV_LOCK.lock();
    let _ = crate::reader();
}
"#
            .to_string(),
        ),
        (
            "glob_tests.rs",
            r#"use super::*;

#[test]
fn ok_glob_imported_crate_lock() {
    let _g = TEST_ENV_LOCK.lock();
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn ok_crate_lock_after_an_inner_static_ends() {
    {
        static TEST_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _inner = TEST_ENV_LOCK.lock();
    }
    let _g = TEST_ENV_LOCK.lock();
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn off_block_static_shadows_the_glob_import() {
    static TEST_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _g = TEST_ENV_LOCK.lock();
    let _ = reader();
}
"#
            .to_string(),
        ),
        (
            "alias_tests.rs",
            r#"use crate::TEST_ENV_LOCK as ENV_LOCK;

static L: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn ok_module_alias_of_the_crate_lock() {
    let _g = ENV_LOCK.lock();
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn ok_block_alias_of_the_crate_lock() {
    use crate::TEST_ENV_LOCK as L;
    let _g = L.lock();
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn off_alias_in_another_function_is_not_in_scope() {
    let _g = L.lock();
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn off_block_alias_of_another_mutex() {
    use crate::other::OTHER as L;
    let _g = L.lock();
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn off_block_static_shadows_the_module_alias() {
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _g = ENV_LOCK.lock();
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn off_alias_of_a_module_local_copy() {
    use crate::local_lock::TEST_ENV_LOCK as L;
    let _g = L.lock();
    unsafe { std::env::set_var("W4", "1") };
}
"#
            .to_string(),
        ),
        (
            "other_alias_tests.rs",
            r#"use crate::other::OTHER as ENV_LOCK;

static LOCAL_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn off_alias_of_another_mutex() {
    let _g = ENV_LOCK.lock();
    let _ = crate::reader();
}

#[test]
fn off_module_local_env_lock() {
    let _g = LOCAL_ENV_LOCK.lock();
    let _ = crate::reader();
}
"#
            .to_string(),
        ),
    ]);
    let mut direct = labels(
        "alias_tests.rs",
        &[
            "off_alias_in_another_function_is_not_in_scope",
            "off_alias_of_a_module_local_copy",
            "off_block_alias_of_another_mutex",
            "off_block_static_shadows_the_module_alias",
        ],
    );
    direct.extend(labels(
        "copied_lock_tests.rs",
        &["off_module_copy_of_the_crate_lock"],
    ));
    direct.extend(labels(
        "shadow_tests.rs",
        &[
            "off_block_static_named_like_the_crate_lock",
            "off_local_mutex_named_like_the_crate_lock",
            "off_module_local_static_by_its_path",
        ],
    ));
    assert_eq!(
        report.offenders, direct,
        "a lock call counts as held only on the crate lock"
    );
    let mut indirect = labels(
        "copied_lock_tests.rs",
        &["off_module_copy_of_the_crate_lock_indirect"],
    );
    indirect.extend(labels(
        "glob_tests.rs",
        &["off_block_static_shadows_the_glob_import"],
    ));
    indirect.extend(labels(
        "other_alias_tests.rs",
        &["off_alias_of_another_mutex", "off_module_local_env_lock"],
    ));
    indirect.extend(labels(
        "shadow_tests.rs",
        &["off_local_mutex_named_like_the_crate_lock_indirect"],
    ));
    assert_eq!(
        report.indirect_offenders, indirect,
        "a test holding another mutex is not stopped at"
    );
    assert_eq!(
        (report.held, report.env_test_code),
        (8, 16),
        "the eight ok cases hold the crate lock; the eight direct off cases do not"
    );
    assert_eq!(
        report
            .acquisitions_by_shape
            .iter()
            .map(|(shape, count)| (*shape, *count))
            .collect::<Vec<_>>(),
        vec![("kept by a shape-1 let", 8)],
        "every crate lock call of the tree is an ok case's"
    );
    assert_eq!(
        report.lock_calls_named_like_the_crate_lock, 8,
        "eight lock calls are written TEST_ENV_LOCK and resolve to another mutex"
    );
    assert!(report.refused_acquisitions.is_empty() && report.nesting_sites.is_empty());
}

/// Review R7 should-fix 4 (round 8, decision D-i8-42): the hold side fails
/// closed on every lock expression its resolution does not decide is the
/// crate lock. The resolution expands no macro into items, so a block or a
/// module scope it consults, without finding the name among that scope's own
/// items and explicit imports, while the scope holds a macro invocation in
/// item or statement position (other than a standard-library macro that runs
/// in place) may hold a macro-made item of the name that shadows what it
/// found: such a lock call holds nothing. Planted: a `macro_rules!` that
/// writes `static TEST_ENV_LOCK` (the review's case: rustc compiles it and
/// the lock call takes that static, not the crate lock), invoked before the
/// lock call, after it, or in a block around it, one that writes a `const` of that
/// name, one that writes a `use` of another mutex under it, one given the
/// name by its caller, and one invoked at a test module's item position over
/// its glob import. A `use` or a block static of another mutex under the name,
/// a re-export of another mutex under it, and a method chain that hides the
/// receiver are refused by the resolution as before. Controls that stay held:
/// a macro in an inner block the lock call is not in, a standard-library
/// macro beside the guard, a `crate::` path past a shadowing macro (its first
/// segment consults no block), and a re-export of the crate lock under another
/// name.
#[test]
fn the_lock_discipline_fails_closed_where_its_resolution_is_not_decided() {
    let report = planted(&[
        (
            "lib.rs",
            format!(
                r#"{PLANTED_ROOT}
pub mod other {{
    pub static OTHER: std::sync::Mutex<()> = std::sync::Mutex::new(());
}}

pub mod reexport {{
    pub use crate::TEST_ENV_LOCK as THE_LOCK;
    pub use crate::other::OTHER as TEST_ENV_LOCK;
}}

#[cfg(test)]
mod body_tests;
#[cfg(test)]
mod module_tests;
"#
            ),
        ),
        (
            "body_tests.rs",
            r#"#![allow(non_upper_case_globals)]
use super::*;

macro_rules! shadow_static {
    () => {
        static TEST_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    };
}

macro_rules! shadow_const {
    () => {
        const TEST_ENV_LOCK: &std::sync::Mutex<()> = &crate::other::OTHER;
    };
}

macro_rules! shadow_use {
    () => {
        use crate::other::OTHER as TEST_ENV_LOCK;
    };
}

macro_rules! shadow_named {
    ($name:ident) => {
        static $name: std::sync::Mutex<()> = std::sync::Mutex::new(());
    };
}

#[test]
fn off_macro_made_static() {
    shadow_static!();
    let _g = TEST_ENV_LOCK.lock();
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn off_macro_made_static_after_the_lock_call() {
    let _g = TEST_ENV_LOCK.lock();
    unsafe { std::env::set_var("W4", "1") };
    shadow_static!();
}

#[test]
fn off_macro_made_const() {
    shadow_const!();
    let _g = TEST_ENV_LOCK.lock();
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn off_macro_made_use() {
    shadow_use!();
    let _g = TEST_ENV_LOCK.lock();
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn off_macro_given_the_name() {
    shadow_named!(TEST_ENV_LOCK);
    let _g = TEST_ENV_LOCK.lock();
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn off_macro_made_static_in_an_outer_block() {
    shadow_static!();
    {
        let _g = TEST_ENV_LOCK.lock();
        unsafe { std::env::set_var("W4", "1") };
    }
}

#[test]
fn off_block_use_of_another_mutex() {
    use crate::other::OTHER as TEST_ENV_LOCK;
    let _g = TEST_ENV_LOCK.lock();
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn off_block_const_of_another_mutex() {
    const TEST_ENV_LOCK: &std::sync::Mutex<()> = &crate::other::OTHER;
    let _g = TEST_ENV_LOCK.lock();
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn off_reexport_of_another_mutex_under_the_name() {
    let _g = crate::reexport::TEST_ENV_LOCK.lock();
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn off_method_chain_hides_the_receiver() {
    let _g = std::convert::identity(&crate::other::OTHER).lock();
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn off_method_chain_over_the_crate_lock() {
    let _g = std::convert::identity(&TEST_ENV_LOCK).lock();
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn ok_macro_in_an_inner_block_elsewhere() {
    {
        shadow_static!();
        let _inner = TEST_ENV_LOCK.lock();
    }
    let _g = TEST_ENV_LOCK.lock();
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn ok_in_place_macro_beside_the_guard() {
    assert!(true);
    let _g = TEST_ENV_LOCK.lock();
    println!("held");
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn ok_crate_path_past_a_shadowing_macro() {
    shadow_static!();
    let _g = crate::TEST_ENV_LOCK.lock();
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn ok_reexport_of_the_crate_lock_under_another_name() {
    let _g = crate::reexport::THE_LOCK.lock();
    unsafe { std::env::set_var("W4", "1") };
}
"#
            .to_string(),
        ),
        (
            "module_tests.rs",
            r#"use super::*;

macro_rules! module_static {
    () => {
        static TEST_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    };
}

module_static!();

#[test]
fn off_module_macro_shadows_the_glob() {
    let _g = TEST_ENV_LOCK.lock();
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn ok_crate_path_past_the_module_macro() {
    let _g = crate::TEST_ENV_LOCK.lock();
    unsafe { std::env::set_var("W4", "1") };
}
"#
            .to_string(),
        ),
    ]);
    let mut direct = labels(
        "body_tests.rs",
        &[
            "off_block_const_of_another_mutex",
            "off_block_use_of_another_mutex",
            "off_macro_given_the_name",
            "off_macro_made_const",
            "off_macro_made_static",
            "off_macro_made_static_after_the_lock_call",
            "off_macro_made_static_in_an_outer_block",
            "off_macro_made_use",
            "off_method_chain_hides_the_receiver",
            "off_method_chain_over_the_crate_lock",
            "off_reexport_of_another_mutex_under_the_name",
        ],
    );
    direct.extend(labels(
        "module_tests.rs",
        &["off_module_macro_shadows_the_glob"],
    ));
    assert_eq!(
        report.offenders, direct,
        "a lock call the resolution does not decide is the crate lock holds nothing"
    );
    assert_eq!(
        (report.held, report.env_test_code),
        (5, 17),
        "the five ok cases hold the crate lock; the twelve off cases do not"
    );
    // A lock call whose path resolves to the crate lock in an undecided
    // scope is still an acquisition of it (the nesting side reports more),
    // kept by no accepted shape: the six macro cases in a test body, the
    // module one, and the inner block's own call in the control.
    let mut refused = labels(
        "body_tests.rs",
        &[
            "off_macro_given_the_name",
            "off_macro_made_const",
            "off_macro_made_static",
            "off_macro_made_static_after_the_lock_call",
            "off_macro_made_static_in_an_outer_block",
            "off_macro_made_use",
            "ok_macro_in_an_inner_block_elsewhere",
        ],
    );
    refused.extend(labels(
        "module_tests.rs",
        &["off_module_macro_shadows_the_glob"],
    ));
    assert_eq!(report.refused_acquisitions, refused);
    assert_eq!(
        report
            .acquisitions_by_shape
            .iter()
            .map(|(shape, count)| (*shape, *count))
            .collect::<Vec<_>>(),
        vec![
            ("kept by a shape-1 let", 5),
            ("kept by no accepted shape", 8)
        ],
        "five kept guards, eight undecided acquisitions"
    );
    assert_eq!(
        report.lock_calls_named_like_the_crate_lock, 3,
        "a block use, a block const and a re-export of another mutex are written TEST_ENV_LOCK"
    );
    assert!(report.nesting_sites.is_empty() && report.indirect_offenders.is_empty());
}

/// Decision D-i7-envlock-1, shapes 1 and 2, and the fail-closed side of round 7's
/// fourth audit: a guard holds only in the listed shapes, and every other use
/// of the lock holds nothing. A `let` of a plain name (with a lint attribute
/// at most) given the crate lock's `.lock()` holds, alone or through exactly
/// one of `unwrap()`, `expect(<string literal>)`, `unwrap_or_else(|p|
/// p.into_inner())` and `unwrap_or_else` with a path that resolves to
/// `std::sync::PoisonError::into_inner` (written in full, or through a `use`),
/// on the lock reached by a path or a `use` alias; so does such a `let` given a
/// call of a shape-2 helper (a free function whose body is one accepted lock
/// call, or `let g = <lock call>; g`). Everything else is refused, and these
/// cases, Rust that holds the lock at run time, are reported by design: `let
/// mut`, `ref`, a typed `let`, a `cfg` on the `let`, a `let` with an `else`, a
/// pattern (`Ok(g)` with `else`, `if let`, a match arm), `&` or parentheses
/// around the call, `?`, a second step, a `match`, an `if`, `Box::new`,
/// `Some`, a crate constructor, a local holding `&` the lock, an `expect`
/// given no literal, `unwrap_or_else` with a `move` closure, a typed
/// parameter, a block body, another closure or a crate function named
/// `into_inner`, and a helper call with a step after it, through a local
/// function pointer, or of a function that is a method, `async`, labelled,
/// releases the guard first, or binds the guard and returns another value. The
/// shapes that keep nothing (`is_ok()`, `drop(..)`, a bare statement, `_`)
/// are refused as before, and every acquisition no shape keeps is counted.
#[test]
fn the_lock_discipline_accepts_only_guards_that_are_kept() {
    let report = planted(&[
        (
            "lib.rs",
            format!(
                "#![allow(let_underscore_lock, unused_must_use, dropping_references, clippy::all)]\n{PLANTED_ROOT}\npub mod my {{\n    pub fn into_inner<T>(_e: T) -> std::sync::MutexGuard<'static, ()> {{\n        crate::tests_support::OTHER.lock().unwrap()\n    }}\n}}\n#[cfg(test)]\npub mod tests_support {{\n    pub static OTHER: std::sync::Mutex<()> = std::sync::Mutex::new(());\n}}\n#[cfg(not(test))]\npub mod tests_support {{\n    pub static OTHER: std::sync::Mutex<()> = std::sync::Mutex::new(());\n}}\n#[cfg(test)]\nmod tests;\n"
            ),
        ),
        (
            "tests.rs",
            r#"use crate::TEST_ENV_LOCK as ENV_LOCK;
use crate::tests_support::OTHER;
use std::sync::PoisonError;

type Guard = std::sync::MutexGuard<'static, ()>;

fn acquire() -> Guard {
    crate::TEST_ENV_LOCK.lock().unwrap()
}

fn acquire_by_a_let() -> Guard {
    let guard = crate::TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    guard
}

fn acquire_result() -> std::sync::LockResult<Guard> {
    crate::TEST_ENV_LOCK.lock()
}

fn acquire_released_first() -> Option<Guard> {
    let mut held = Some(crate::TEST_ENV_LOCK.lock().unwrap());
    held.take();
    held
}

fn acquire_labelled() -> Guard {
    'found: {
        if std::hint::black_box(false) {
            break 'found OTHER.lock().unwrap();
        }
        crate::TEST_ENV_LOCK.lock().unwrap()
    }
}

async fn acquire_later() -> Guard {
    crate::TEST_ENV_LOCK.lock().unwrap()
}

struct Fixture;
impl Fixture {
    fn acquire() -> Guard {
        crate::TEST_ENV_LOCK.lock().unwrap()
    }
}

struct Both(Guard, u8);

fn acquire_returning_another(fallback: Guard) -> Guard {
    let _guard = crate::TEST_ENV_LOCK.lock().unwrap();
    fallback
}

#[test]
fn ok_the_lock_result_bound() {
    let _g = crate::TEST_ENV_LOCK.lock();
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn ok_unwrap() {
    let _g = crate::TEST_ENV_LOCK.lock().unwrap();
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn ok_expect() {
    let _g = crate::TEST_ENV_LOCK.lock().expect("the crate lock");
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn ok_unwrap_or_else_into_inner() {
    let _g = crate::TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn ok_unwrap_or_else_into_inner_by_its_path() {
    let _g = crate::TEST_ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn ok_unwrap_or_else_into_inner_through_a_use() {
    let _g = crate::TEST_ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn ok_a_use_alias_of_the_lock() {
    #[allow(unused_variables)]
    let _g = ENV_LOCK.lock().unwrap();
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn ok_a_helper_call() {
    let _g = acquire();
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn ok_a_helper_by_a_let() {
    let _g = acquire_by_a_let();
    let _ = crate::reader();
}

#[test]
fn ok_a_helper_returning_the_lock_result() {
    let _g = acquire_result();
    let _ = crate::reader();
}

#[test]
fn off_let_mut() {
    let mut g = crate::TEST_ENV_LOCK.lock().unwrap();
    unsafe { std::env::set_var("W4", "1") };
    let _ = &mut g;
}

#[test]
fn off_let_ref() {
    let ref _g = crate::TEST_ENV_LOCK.lock();
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn off_a_typed_let() {
    let _g: Guard = crate::TEST_ENV_LOCK.lock().unwrap();
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn off_a_cfg_on_the_let() {
    #[cfg(test)]
    let _g = crate::TEST_ENV_LOCK.lock().unwrap();
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn off_a_borrowed_guard() {
    let _g = &crate::TEST_ENV_LOCK.lock().unwrap();
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn off_a_parenthesised_guard() {
    let _g = (crate::TEST_ENV_LOCK.lock().unwrap());
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn off_a_parenthesised_lock() {
    let _g = (&crate::TEST_ENV_LOCK).lock().unwrap();
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn off_let_ok_binding_else() {
    let Ok(_guard) = crate::TEST_ENV_LOCK.lock() else {
        return;
    };
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn off_the_try_operator() -> Result<(), Box<dyn std::error::Error>> {
    let _g = crate::TEST_ENV_LOCK.lock()?;
    unsafe { std::env::set_var("W4", "1") };
    Ok(())
}

#[test]
fn off_two_steps_after_the_lock() {
    let _g = crate::TEST_ENV_LOCK.lock().map_err(|e| e).unwrap();
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn off_expect_given_no_literal() {
    let message = "the crate lock";
    let _g = crate::TEST_ENV_LOCK.lock().expect(message);
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn off_unwrap_or_else_another_closure() {
    let _g = crate::TEST_ENV_LOCK.lock().unwrap_or_else(|_| OTHER.lock().unwrap());
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn off_unwrap_or_else_into_inner_of_another_error() {
    let other = OTHER.lock().unwrap_err();
    let _g = crate::TEST_ENV_LOCK.lock().unwrap_or_else(|_e| other.into_inner());
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn off_unwrap_or_else_a_crate_function_named_into_inner() {
    let _g = crate::TEST_ENV_LOCK.lock().unwrap_or_else(crate::my::into_inner);
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn off_is_ok() {
    let _ok = crate::TEST_ENV_LOCK.lock().is_ok();
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn off_drop_call() {
    drop(crate::TEST_ENV_LOCK.lock());
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn off_bare_statement() {
    let _ = 0;
    crate::TEST_ENV_LOCK.lock();
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn off_wildcard() {
    let _ = crate::TEST_ENV_LOCK.lock();
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn off_a_match_on_the_lock_result() {
    let _g = match crate::TEST_ENV_LOCK.lock() {
        Ok(g) => g,
        Err(e) => e.into_inner(),
    };
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn off_every_branch_takes_the_crate_lock() {
    let _g = if std::hint::black_box(true) {
        crate::TEST_ENV_LOCK.lock().unwrap()
    } else {
        crate::TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    };
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn off_a_boxed_guard() {
    let _g = Box::new(crate::TEST_ENV_LOCK.lock());
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn off_a_box_of_a_borrowed_temporary() {
    let _g = Box::new(&crate::TEST_ENV_LOCK.lock().unwrap());
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn off_a_guard_in_some() {
    let _g = Some(crate::TEST_ENV_LOCK.lock().unwrap());
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn off_a_guard_in_a_crate_constructor() {
    let _g = Both(crate::TEST_ENV_LOCK.lock().unwrap(), 0);
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn off_a_local_holding_the_lock() {
    let lock = &crate::TEST_ENV_LOCK;
    let _g = lock.lock();
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn off_a_guard_bound_by_a_match_arm() {
    match crate::TEST_ENV_LOCK.lock() {
        Ok(_g) => unsafe { std::env::set_var("W4", "1") },
        Err(_e) => {}
    }
}

#[test]
fn off_a_guard_bound_by_if_let() {
    if let Ok(_g) = crate::TEST_ENV_LOCK.lock() {
        unsafe { std::env::set_var("W4", "1") };
    }
}

#[test]
fn off_a_helper_call_with_a_step_after_it() {
    let _g = acquire_result().unwrap();
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn off_a_helper_that_releases_first() {
    let _g = acquire_released_first();
    let _ = crate::reader();
}

#[test]
fn off_a_labelled_helper() {
    let _g = acquire_labelled();
    let _ = crate::reader();
}

#[test]
fn off_an_async_helper() {
    let _g = acquire_later();
    let _ = crate::reader();
}

#[test]
fn off_a_method_helper() {
    let _g = Fixture::acquire();
    let _ = crate::reader();
}

#[test]
fn off_a_helper_through_a_local() {
    let f = acquire;
    let _g = f();
    let _ = crate::reader();
}

#[test]
fn off_a_helper_call_discarded() {
    let _ = acquire();
    let _ = crate::reader();
}

#[test]
fn off_a_let_with_an_else() {
    #[allow(irrefutable_let_patterns)]
    let _g = crate::TEST_ENV_LOCK.lock() else {
        return;
    };
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn off_into_inner_in_a_move_closure() {
    let _g = crate::TEST_ENV_LOCK.lock().unwrap_or_else(move |e| e.into_inner());
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn off_into_inner_with_a_typed_parameter() {
    let _g = crate::TEST_ENV_LOCK
        .lock()
        .unwrap_or_else(|e: std::sync::PoisonError<Guard>| e.into_inner());
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn off_into_inner_in_a_block() {
    let _g = crate::TEST_ENV_LOCK.lock().unwrap_or_else(|e| { e.into_inner() });
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn off_a_helper_returning_another_name() {
    let _g = acquire_returning_another(OTHER.lock().unwrap());
    let _ = crate::reader();
}
"#
            .to_string(),
        ),
    ]);
    let direct = labels(
        "tests.rs",
        &[
            "off_let_mut",
            "off_let_ref",
            "off_a_typed_let",
            "off_a_cfg_on_the_let",
            "off_a_borrowed_guard",
            "off_a_parenthesised_guard",
            "off_a_parenthesised_lock",
            "off_let_ok_binding_else",
            "off_the_try_operator",
            "off_two_steps_after_the_lock",
            "off_expect_given_no_literal",
            "off_unwrap_or_else_another_closure",
            "off_unwrap_or_else_into_inner_of_another_error",
            "off_unwrap_or_else_a_crate_function_named_into_inner",
            "off_is_ok",
            "off_drop_call",
            "off_bare_statement",
            "off_wildcard",
            "off_a_match_on_the_lock_result",
            "off_every_branch_takes_the_crate_lock",
            "off_a_boxed_guard",
            "off_a_box_of_a_borrowed_temporary",
            "off_a_guard_in_some",
            "off_a_guard_in_a_crate_constructor",
            "off_a_local_holding_the_lock",
            "off_a_guard_bound_by_a_match_arm",
            "off_a_guard_bound_by_if_let",
            "off_a_helper_call_with_a_step_after_it",
            "off_a_let_with_an_else",
            "off_into_inner_in_a_move_closure",
            "off_into_inner_with_a_typed_parameter",
            "off_into_inner_in_a_block",
        ],
    );
    assert_eq!(
        report.offenders, direct,
        "a guard holds only in the listed shapes"
    );
    let indirect = labels(
        "tests.rs",
        &[
            "off_a_helper_that_releases_first",
            "off_a_labelled_helper",
            "off_an_async_helper",
            "off_a_method_helper",
            "off_a_helper_through_a_local",
            "off_a_helper_call_discarded",
            "off_a_helper_returning_another_name",
        ],
    );
    assert_eq!(
        report.indirect_offenders, indirect,
        "only a call of a shape-2 helper bound by a shape-1 let holds"
    );
    assert_eq!(
        report.helpers,
        labels(
            "tests.rs",
            &["acquire", "acquire_by_a_let", "acquire_result"]
        )
        .into_iter()
        .collect::<BTreeSet<_>>(),
        "a helper is a free function whose body is one accepted lock call, alone or bound and \
         returned"
    );
    let refused = labels(
        "tests.rs",
        &[
            "Fixture::acquire",
            "acquire_labelled",
            "acquire_later",
            "acquire_released_first",
            "off_a_borrowed_guard",
            "off_a_box_of_a_borrowed_temporary",
            "off_a_boxed_guard",
            "off_a_cfg_on_the_let",
            "off_a_guard_bound_by_a_match_arm",
            "off_a_guard_bound_by_if_let",
            "off_a_guard_in_a_crate_constructor",
            "off_a_guard_in_some",
            "off_a_helper_call_discarded",
            "off_a_helper_call_with_a_step_after_it",
            "off_a_match_on_the_lock_result",
            "off_a_parenthesised_guard",
            "off_a_parenthesised_lock",
            "off_a_typed_let",
            "off_bare_statement",
            "off_drop_call",
            "off_every_branch_takes_the_crate_lock",
            "off_a_let_with_an_else",
            "off_expect_given_no_literal",
            "off_into_inner_in_a_block",
            "off_into_inner_in_a_move_closure",
            "off_into_inner_with_a_typed_parameter",
            "off_is_ok",
            "off_let_mut",
            "off_let_ok_binding_else",
            "off_let_ref",
            "off_the_try_operator",
            "off_two_steps_after_the_lock",
            "off_unwrap_or_else_a_crate_function_named_into_inner",
            "off_unwrap_or_else_another_closure",
            "off_unwrap_or_else_into_inner_of_another_error",
            "off_wildcard",
        ],
    );
    assert_eq!(
        report.refused_acquisitions, refused,
        "every acquisition no accepted shape keeps is refused (a lock taken through a local \
         holding `&` the lock, and a call through a local function pointer, are no acquisition \
         the model sees)"
    );
    assert_eq!(
        report
            .acquisitions_by_shape
            .iter()
            .map(|(shape, count)| (*shape, *count))
            .collect::<Vec<_>>(),
        vec![
            ("kept by a shape-1 let", 11),
            ("kept by no accepted shape", 37),
            ("returned by a shape-2 helper", 3),
        ],
        "every acquisition is counted by the shape that keeps it"
    );
    assert_eq!(
        (report.held, report.env_test_code),
        (8, 40),
        "the eight direct ok cases hold the lock; the thirty-two direct off cases do not"
    );
    assert!(
        report.nesting_sites.is_empty(),
        "no shape takes the lock twice"
    );
}

/// The crate root of the planted trees that use a crate lock of the real
/// crate's own kind: a `TestEnvLock` whose `lock` returns
/// `LockResult<TestEnvGuard>`, so the guard type is a crate type found by
/// resolution (`CrateModel::lock_guard_types`), and another lock of that type.
const PLANTED_WRAPPED_ROOT: &str = r#"#![allow(dead_code)]
pub mod test_env_lock {
    use std::sync::{LockResult, Mutex, MutexGuard, PoisonError};

    #[derive(Debug)]
    pub struct TestEnvLock {
        mutex: Mutex<()>,
    }

    #[derive(Debug)]
    pub struct TestEnvGuard {
        _guard: MutexGuard<'static, ()>,
    }

    impl TestEnvLock {
        pub const fn new() -> Self {
            Self {
                mutex: Mutex::new(()),
            }
        }

        pub fn lock(&'static self) -> LockResult<TestEnvGuard> {
            match self.mutex.lock() {
                Ok(guard) => Ok(TestEnvGuard { _guard: guard }),
                Err(poisoned) => Err(PoisonError::new(TestEnvGuard {
                    _guard: poisoned.into_inner(),
                })),
            }
        }
    }
}

pub static TEST_ENV_LOCK: test_env_lock::TestEnvLock = test_env_lock::TestEnvLock::new();
pub static OTHER: test_env_lock::TestEnvLock = test_env_lock::TestEnvLock::new();
pub use test_env_lock::TestEnvGuard;

pub fn reader() -> Option<std::ffi::OsString> {
    std::env::var_os("W4")
}

#[cfg(test)]
mod tests;
"#;

/// Decision D-i7-envlock-1, shape 3, and round 7's fourth audit (blocker 4): the
/// `drop` of a holder runs under the lock, and a holder is only a struct
/// with named fields, exactly one of them of the crate lock's guard type (by
/// resolution: `crate::TestEnvGuard` here, a crate type found from what the
/// crate lock's own `lock` returns), no attribute but inert ones, declared
/// once, built at least once and only by struct literals the walks read and
/// accept (the guard field given the lock call, a helper call, or a shape-1
/// guard's name at the mention that ends it, the innermost binding of that
/// name, and no `..base`), its guard field named nowhere else (no field
/// access, in its `drop` either, no struct pattern, no macro token). A type
/// alias names its type. Not holders, so
/// their `drop` is reported: a tuple struct (built here by its constructor
/// named as a value), a derive or a `cfg_attr`, two guard fields, a field
/// that only wraps the guard type, the guard field named in the `drop`, in an
/// `assert!` or in a struct pattern, a literal of another lock's guard, one
/// whose guard name was mentioned before it, one in a closure, one with an
/// attribute on its guard field, one written in a macro's tokens (whose lock
/// call then no shape keeps), a type never built, one declared in cfg
/// variants, a `drop` written over two modules' structs of one name chosen
/// by cfg'd imports when one of them is no holder (D03), and a holder whose
/// `drop` reads inside a closure. A holder covers its own `drop` only (its
/// `Drop::drop`, not another trait's method named `drop`): the function that
/// keeps one holds nothing. An enum naming the guard type is
/// counted by resolution, never by the text `MutexGuard`.
#[test]
fn the_lock_discipline_accepts_a_drop_only_under_the_crate_guard() {
    let report = planted(&[
        ("lib.rs", PLANTED_WRAPPED_ROOT.to_string()),
        (
            "tests.rs",
            r#"#![allow(clippy::all)]
use crate::TestEnvGuard;
use std::ffi::OsString;

fn acquire() -> TestEnvGuard {
    crate::TEST_ENV_LOCK.lock().unwrap()
}

macro_rules! restore_on_drop {
    ($name:ident) => {
        impl Drop for $name {
            fn drop(&mut self) {
                unsafe { std::env::remove_var("W4") };
            }
        }
    };
}

struct BoundGuard {
    _guard: TestEnvGuard,
}
impl BoundGuard {
    fn new() -> Self {
        let guard = crate::TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        Self { _guard: guard }
    }
}
impl Drop for BoundGuard {
    fn drop(&mut self) {
        unsafe { std::env::remove_var("W4") };
    }
}

/// Built from a guard that shadows an outer local of the same name: the
/// name in the literal is the inner guard, at the mention that ends it.
struct ShadowedGuard {
    _shadowed: TestEnvGuard,
}
impl ShadowedGuard {
    fn new() -> Self {
        let guard = 0u8;
        let _ = guard;
        {
            let guard = crate::TEST_ENV_LOCK.lock().unwrap();
            Self { _shadowed: guard }
        }
    }
}
impl Drop for ShadowedGuard {
    fn drop(&mut self) {
        unsafe { std::env::remove_var("W4") };
    }
}

/// A holder with another trait's method named `drop`, which no drop runs.
struct FinishGuard {
    _finish: TestEnvGuard,
}
impl FinishGuard {
    fn new() -> Self {
        FinishGuard {
            _finish: crate::TEST_ENV_LOCK.lock().unwrap(),
        }
    }
}
impl Drop for FinishGuard {
    fn drop(&mut self) {}
}
impl clap::Finish for FinishGuard {
    fn drop(&mut self) {
        unsafe { std::env::remove_var("W4") };
    }
}

/// Restores the variable on drop.
struct ShorthandGuard {
    previous: Option<OsString>,
    _lock: TestEnvGuard,
}
impl ShorthandGuard {
    fn new() -> Self {
        let _lock = crate::TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let previous = std::env::var_os("W4");
        Self { previous, _lock }
    }
}
impl Drop for ShorthandGuard {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => unsafe { std::env::set_var("W4", value) },
            None => unsafe { std::env::remove_var("W4") },
        }
    }
}

struct DirectGuard {
    _g: TestEnvGuard,
}
impl DirectGuard {
    fn new() -> Self {
        DirectGuard {
            _g: crate::TEST_ENV_LOCK.lock().unwrap(),
        }
    }
}
impl Drop for DirectGuard {
    fn drop(&mut self) {
        unsafe { std::env::remove_var("W4") };
    }
}

struct HelperGuard {
    _g: TestEnvGuard,
}
impl HelperGuard {
    fn new() -> Self {
        HelperGuard { _g: acquire() }
    }
}
impl Drop for HelperGuard {
    fn drop(&mut self) {
        unsafe { std::env::remove_var("W4") };
    }
}

struct AliasGuard {
    _g: TestEnvGuard,
}
type Alias = AliasGuard;
fn build_alias() -> Alias {
    let guard = crate::TEST_ENV_LOCK.lock().unwrap();
    Alias { _g: guard }
}
impl Drop for AliasGuard {
    fn drop(&mut self) {
        unsafe { std::env::remove_var("W4") };
    }
}

struct TupleHolder(TestEnvGuard);
fn build_tuple() -> Option<TupleHolder> {
    crate::OTHER.lock().map(TupleHolder).ok()
}
impl Drop for TupleHolder {
    fn drop(&mut self) {
        unsafe { std::env::remove_var("W4") };
    }
}

#[derive(Debug)]
struct Derived {
    _g: TestEnvGuard,
}
fn build_derived() -> Derived {
    let guard = crate::TEST_ENV_LOCK.lock().unwrap();
    Derived { _g: guard }
}
impl Drop for Derived {
    fn drop(&mut self) {
        unsafe { std::env::remove_var("W4") };
    }
}

#[cfg_attr(test, derive(Debug))]
struct CfgAttr {
    _g: TestEnvGuard,
}
fn build_cfg_attr() -> CfgAttr {
    let guard = crate::TEST_ENV_LOCK.lock().unwrap();
    CfgAttr { _g: guard }
}
impl Drop for CfgAttr {
    fn drop(&mut self) {
        unsafe { std::env::remove_var("W4") };
    }
}

struct TwoGuards {
    first: TestEnvGuard,
    second: TestEnvGuard,
}
impl Drop for TwoGuards {
    fn drop(&mut self) {
        unsafe { std::env::remove_var("W4") };
    }
}

#[derive(Default)]
struct OptionGuard {
    _g: Option<TestEnvGuard>,
}
impl Drop for OptionGuard {
    fn drop(&mut self) {
        unsafe { std::env::remove_var("W4") };
    }
}

struct NamedInDrop {
    _accessed: TestEnvGuard,
}
fn build_named_in_drop() -> NamedInDrop {
    let guard = crate::TEST_ENV_LOCK.lock().unwrap();
    NamedInDrop { _accessed: guard }
}
impl Drop for NamedInDrop {
    fn drop(&mut self) {
        let _ = &self._accessed;
        unsafe { std::env::remove_var("W4") };
    }
}

struct NamedInAMacro {
    _held: TestEnvGuard,
}
fn build_named_in_a_macro() -> NamedInAMacro {
    let guard = crate::TEST_ENV_LOCK.lock().unwrap();
    let built = NamedInAMacro { _held: guard };
    assert!(std::mem::size_of_val(&built._held) > 0);
    built
}
impl Drop for NamedInAMacro {
    fn drop(&mut self) {
        unsafe { std::env::remove_var("W4") };
    }
}

struct Destructured {
    pulled: TestEnvGuard,
    other: u8,
}
fn build_destructured() -> u8 {
    let guard = crate::TEST_ENV_LOCK.lock().unwrap();
    let built = Destructured {
        pulled: guard,
        other: 0,
    };
    let Destructured {
        pulled: _held,
        other,
    } = &built;
    *other
}
impl Drop for Destructured {
    fn drop(&mut self) {
        unsafe { std::env::remove_var("W4") };
    }
}

struct OtherGuard {
    _g: TestEnvGuard,
}
fn build_other() -> OtherGuard {
    OtherGuard {
        _g: crate::OTHER.lock().unwrap(),
    }
}
impl Drop for OtherGuard {
    fn drop(&mut self) {
        unsafe { std::env::remove_var("W4") };
    }
}

struct TwoWays {
    _g: TestEnvGuard,
}
impl TwoWays {
    fn locked() -> Self {
        let guard = crate::TEST_ENV_LOCK.lock().unwrap();
        TwoWays { _g: guard }
    }
    fn other() -> Self {
        TwoWays {
            _g: crate::OTHER.lock().unwrap(),
        }
    }
}
impl Drop for TwoWays {
    fn drop(&mut self) {
        unsafe { std::env::remove_var("W4") };
    }
}

struct NeverBuilt {
    _g: TestEnvGuard,
}
impl Drop for NeverBuilt {
    fn drop(&mut self) {
        unsafe { std::env::remove_var("W4") };
    }
}

struct LateName {
    _g: TestEnvGuard,
}
fn build_late_name() -> LateName {
    let guard = crate::TEST_ENV_LOCK.lock().unwrap();
    let _ = &guard;
    LateName { _g: guard }
}
impl Drop for LateName {
    fn drop(&mut self) {
        unsafe { std::env::remove_var("W4") };
    }
}

struct InAClosure {
    _g: TestEnvGuard,
}
fn build_in_a_closure() -> InAClosure {
    let guard = crate::TEST_ENV_LOCK.lock().unwrap();
    let make = move || InAClosure { _g: guard };
    make()
}
impl Drop for InAClosure {
    fn drop(&mut self) {
        unsafe { std::env::remove_var("W4") };
    }
}

struct AttributeOnField {
    #[allow(dead_code)]
    _g: TestEnvGuard,
}
fn build_attribute_on_field() -> AttributeOnField {
    let guard = crate::TEST_ENV_LOCK.lock().unwrap();
    AttributeOnField { _g: guard }
}
impl Drop for AttributeOnField {
    fn drop(&mut self) {
        unsafe { std::env::remove_var("W4") };
    }
}

struct BuiltInAMacro {
    _in_tokens: TestEnvGuard,
}
fn build_in_a_macro() -> Vec<BuiltInAMacro> {
    vec![BuiltInAMacro {
        _in_tokens: crate::OTHER.lock().unwrap(),
    }]
}
fn build_in_code() -> BuiltInAMacro {
    BuiltInAMacro {
        _in_tokens: crate::TEST_ENV_LOCK.lock().unwrap(),
    }
}
impl Drop for BuiltInAMacro {
    fn drop(&mut self) {
        unsafe { std::env::remove_var("W4") };
    }
}

struct ClosureInDrop {
    _g: TestEnvGuard,
}
fn build_closure_in_drop() -> ClosureInDrop {
    let guard = crate::TEST_ENV_LOCK.lock().unwrap();
    ClosureInDrop { _g: guard }
}
impl Drop for ClosureInDrop {
    fn drop(&mut self) {
        let restore = || unsafe { std::env::remove_var("W4") };
        restore();
    }
}

struct ByAMacroImpl {
    _g: TestEnvGuard,
}
fn build_by_a_macro_impl() -> ByAMacroImpl {
    let guard = crate::TEST_ENV_LOCK.lock().unwrap();
    ByAMacroImpl { _g: guard }
}
restore_on_drop!(ByAMacroImpl);

#[cfg(unix)]
struct Variants {
    _variant: TestEnvGuard,
}
#[cfg(not(unix))]
struct Variants {
    _variant: TestEnvGuard,
}
fn build_variants() -> Variants {
    let guard = crate::TEST_ENV_LOCK.lock().unwrap();
    Variants { _variant: guard }
}
impl Drop for Variants {
    fn drop(&mut self) {
        unsafe { std::env::remove_var("W4") };
    }
}

mod variant_a {
    pub struct Chosen {
        _chosen: crate::TestEnvGuard,
    }
    pub fn make() -> Chosen {
        let guard = crate::TEST_ENV_LOCK.lock().unwrap();
        Chosen { _chosen: guard }
    }
}

mod variant_b {
    pub struct Chosen {
        _chosen: crate::TestEnvGuard,
    }
    pub fn make() -> Chosen {
        Chosen {
            _chosen: crate::OTHER.lock().unwrap(),
        }
    }
}

#[cfg(unix)]
use variant_a::Chosen;
#[cfg(not(unix))]
use variant_b::Chosen;

impl Drop for Chosen {
    fn drop(&mut self) {
        unsafe { std::env::remove_var("W4") };
    }
}

struct SharedName {
    _shared: TestEnvGuard,
}
fn build_shared_name() -> SharedName {
    let guard = crate::TEST_ENV_LOCK.lock().unwrap();
    SharedName { _shared: guard }
}
impl Drop for SharedName {
    fn drop(&mut self) {
        unsafe { std::env::remove_var("W4") };
    }
}

enum Shapes {
    Named { _shared: u8 },
}
fn build_a_variant_named_like_the_field() -> Shapes {
    Shapes::Named { _shared: 0 }
}

struct WithBase {
    _base: TestEnvGuard,
    n: u8,
}
fn build_with_base() -> WithBase {
    let guard = crate::TEST_ENV_LOCK.lock().unwrap();
    WithBase { _base: guard, n: 1 }
}
fn rebuild_with_base(previous: &WithBase) -> WithBase {
    let guard = crate::TEST_ENV_LOCK.lock().unwrap();
    WithBase {
        _base: guard,
        ..*previous
    }
}
impl Drop for WithBase {
    fn drop(&mut self) {
        unsafe { std::env::remove_var("W4") };
    }
}

struct AttributedInit {
    _attributed: TestEnvGuard,
}
fn build_attributed_init() -> AttributedInit {
    let guard = crate::TEST_ENV_LOCK.lock().unwrap();
    AttributedInit {
        #[allow(unused)]
        _attributed: guard,
    }
}
impl Drop for AttributedInit {
    fn drop(&mut self) {
        unsafe { std::env::remove_var("W4") };
    }
}

enum GuardEnum {
    Held(TestEnvGuard),
    Free,
}

enum WrappedGuardEnum {
    Held(Option<crate::TestEnvGuard>),
}

enum OtherGuardEnum {
    Held(std::sync::MutexGuard<'static, ()>),
}

#[test]
fn off_the_lock_taken_through_its_type() {
    let _g = crate::test_env_lock::TestEnvLock::lock(&crate::TEST_ENV_LOCK);
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn off_a_holder_does_not_cover_its_owner() {
    let _held = BoundGuard::new();
    let _ = crate::reader();
}

#[test]
fn builds_every_holder() {
    drop(ShorthandGuard::new());
    drop(ShadowedGuard::new());
    drop(FinishGuard::new());
    drop(DirectGuard::new());
    drop(HelperGuard::new());
    drop(build_alias());
}
"#
            .to_string(),
        ),
    ]);
    assert_eq!(
        report.guard_type,
        vec!["lib.rs::TestEnvGuard".to_string()],
        "the guard type is what the crate lock's own lock returns, by resolution"
    );
    let direct = labels(
        "tests.rs",
        &[
            "TupleHolder::drop",
            "Derived::drop",
            "CfgAttr::drop",
            "TwoGuards::drop",
            "OptionGuard::drop",
            "NamedInDrop::drop",
            "NamedInAMacro::drop",
            "Destructured::drop",
            "OtherGuard::drop",
            "TwoWays::drop",
            "NeverBuilt::drop",
            "LateName::drop",
            "InAClosure::drop",
            "AttributeOnField::drop",
            "BuiltInAMacro::drop",
            "ClosureInDrop::drop",
            "Variants::drop",
            "Chosen::drop",
            "off_the_lock_taken_through_its_type",
            "SharedName::drop",
            "WithBase::drop",
            "AttributedInit::drop",
            "FinishGuard::drop",
        ],
    );
    assert_eq!(
        report.offenders, direct,
        "a drop is covered only for a holder, and only outside a closure"
    );
    assert_eq!(
        report.indirect_offenders,
        labels("tests.rs", &["off_a_holder_does_not_cover_its_owner"]),
        "a holder covers its own drop, not the function that keeps it"
    );
    assert_eq!(
        report.holders,
        [
            "AliasGuard",
            "BoundGuard",
            "ByAMacroImpl",
            "Chosen",
            "ClosureInDrop",
            "DirectGuard",
            "FinishGuard",
            "HelperGuard",
            "ShadowedGuard",
            "ShorthandGuard",
        ]
        .iter()
        .map(|name| ("tests.rs".to_string(), name.to_string()))
        .collect::<BTreeSet<_>>(),
        "the holders"
    );
    let not_holders: BTreeMap<String, String> = report
        .not_holders
        .iter()
        .map(|(name, why)| (name.clone(), why.clone()))
        .collect();
    for (name, why) in [
        ("TupleHolder", "a tuple struct"),
        ("Derived", "an attribute that is not inert"),
        ("CfgAttr", "an attribute that is not inert"),
        ("TwoGuards", "more than one field of the guard type"),
        ("NamedInDrop", "a field access"),
        ("NamedInAMacro", "a macro token"),
        ("Destructured", "a struct pattern"),
        ("OtherGuard", "no accepted lock call"),
        ("TwoWays", "no accepted lock call"),
        ("NeverBuilt", "never built"),
        ("LateName", "no accepted lock call"),
        ("InAClosure", "no accepted lock call"),
        ("AttributeOnField", "an attribute on its guard field"),
        ("BuiltInAMacro", "a macro token"),
        ("Variants", "cfg variants"),
        ("Chosen", "no accepted lock call"),
        ("SharedName", "a struct literal"),
        ("WithBase", "..base"),
        ("AttributedInit", "carries an attribute"),
    ] {
        let found = not_holders
            .get(&format!("tests.rs::{name}"))
            .cloned()
            .unwrap_or_default();
        assert!(
            found.contains(why),
            "{name} is no holder because of {why:?}; the report says {found:?}"
        );
    }
    assert_eq!(
        not_holders.len(),
        19,
        "every struct with a guard field that is no holder says why: {not_holders:#?}"
    );
    assert_eq!(
        report.guard_types.len(),
        28,
        "every struct with a field of the guard type, the OptionGuard aside: {:?}",
        report.guard_types
    );
    assert_eq!(
        report.guard_types_in_cfg_variants,
        vec!["tests.rs::Variants".to_string()],
        "a struct declared in cfg variants is never a holder"
    );
    assert_eq!(
        report.refused_acquisitions,
        labels(
            "tests.rs",
            &["build_in_code", "off_the_lock_taken_through_its_type"]
        ),
        "a lock call in the guard field of a struct that is no holder, and the lock taken \
         through its type's path, are kept by no shape"
    );
    assert_eq!(
        (report.held, report.held_by_drop, report.env_test_code),
        (1, 6, 30),
        "ShorthandGuard::new holds in its body; six holders' drops hold by their guard \
         field, and ClosureInDrop's reads in a closure"
    );
    assert_eq!(
        report.guard_carrying_enums,
        vec![
            "tests.rs::GuardEnum".to_string(),
            "tests.rs::WrappedGuardEnum".to_string()
        ],
        "an enum whose variant names the guard type, resolved, is counted (and is never a \
         holder); one holding another lock's guard type is not"
    );
    assert_eq!(
        report.item_position_macro_invocations,
        vec!["tests.rs: restore_on_drop!".to_string()],
        "the macro that writes ByAMacroImpl's drop is in item position, not expanded, and \
         reported"
    );
    assert!(
        report.nesting_sites.is_empty() && report.nesting_hazards.is_empty(),
        "no holder takes the lock twice"
    );
}

/// Finding 4 and the false offenders of item 7: an item resolves where Rust
/// scopes it. A function, const, static, tuple struct or `use` declared in a
/// block names that item within the block (also before its declaration) and
/// nowhere else, so another test's call of the crate's `reader` stays a call
/// of it; a nested function's own calls are followed; a trait's default method
/// and a function nested in a method are not free functions of the module. A
/// module's own items and imports shadow a glob import the same way, and a
/// glob brings in no private item of another module.
#[test]
fn the_lock_discipline_scopes_items_like_rust() {
    let report = planted(&[
        (
            "lib.rs",
            format!(
                r#"{PLANTED_ROOT}
pub trait Defaults {{
    fn reader() -> Option<std::ffi::OsString> {{
        None
    }}
}}

pub struct Holder;
impl Holder {{
    pub fn read_all() -> Option<std::ffi::OsString> {{
        fn inner() -> Option<std::ffi::OsString> {{
            crate::reader()
        }}
        inner()
    }}
}}

pub fn quiet() {{}}

pub mod hidden {{
    #[allow(dead_code)]
    fn quiet() {{
        let _ = std::env::var_os("W4");
    }}
}}

#[cfg(test)]
mod glob_visibility;
#[cfg(test)]
mod module_items;
#[cfg(test)]
mod tests;
"#
            ),
        ),
        (
            "glob_visibility.rs",
            r#"use super::*;
#[allow(unused_imports)]
use crate::hidden::*;

#[test]
fn ok_a_private_item_is_not_glob_imported() {
    quiet();
}
"#
            .to_string(),
        ),
        (
            "module_items.rs",
            r#"#![allow(non_upper_case_globals, non_camel_case_types)]
use std::ffi::OsString;

const reader: fn() -> Option<OsString> = crate::no_read;

#[test]
fn ok_module_const_shadows_the_reader() {
    let _ = reader();
}

mod renamed {
    use crate::no_read as reader;

    #[test]
    fn ok_module_use_rename_shadows_the_reader() {
        let _ = reader();
    }
}

mod tuple_struct {
    struct reader(u8);

    #[test]
    fn ok_module_tuple_struct_named_reader() {
        let _ = reader(0);
    }
}
"#
            .to_string(),
        ),
        (
            "tests.rs",
            r#"#![allow(non_upper_case_globals, non_camel_case_types)]
use super::*;
use std::ffi::OsString;
type R = fn() -> Option<OsString>;

#[test]
fn ok_nested_fn_shadows_the_reader() {
    fn reader() -> Option<OsString> {
        None
    }
    let _ = reader();
}

#[test]
fn ok_nested_fn_used_before_its_declaration() {
    let _ = reader();
    fn reader() -> Option<OsString> {
        None
    }
}

#[test]
fn off_nested_fn_scope_ends_with_its_block() {
    {
        fn reader() -> Option<OsString> {
            None
        }
        let _ = reader();
    }
    let _ = reader();
}

#[test]
fn off_another_test_calls_the_crate_reader() {
    let _ = reader();
}

#[test]
fn off_another_test_calls_the_crate_reader_by_its_path() {
    let _ = crate::reader();
}

#[test]
fn off_nested_fn_that_reads() {
    fn helper() -> Option<OsString> {
        std::env::var_os("W4")
    }
    let _ = helper();
}

#[test]
fn ok_nested_fn_reads_under_its_own_lock() {
    fn helper() -> Option<OsString> {
        let _g = crate::TEST_ENV_LOCK.lock().unwrap();
        std::env::var_os("W4")
    }
    let _ = helper();
}

#[test]
fn off_reaches_the_reader_through_a_fn_nested_in_a_method() {
    let _ = Holder::read_all();
}

#[test]
fn ok_trait_default_through_an_impl() {
    struct S;
    impl Defaults for S {}
    let _ = <S as Defaults>::reader();
    let _ = S::reader();
}

#[test]
fn ok_block_const_fn_pointer_named_reader() {
    const reader: R = crate::no_read;
    let _ = reader();
}

#[test]
fn ok_block_static_fn_pointer_named_reader() {
    static reader: R = crate::no_read;
    let _ = reader();
}

#[test]
fn ok_block_tuple_struct_named_reader() {
    struct reader(u8);
    let _ = reader(0);
}

#[test]
fn ok_block_use_rename_named_reader() {
    use crate::no_read as reader;
    let _ = reader();
}

#[test]
fn ok_block_const_used_before_its_declaration() {
    let _ = reader();
    const reader: R = crate::no_read;
}

#[test]
fn off_block_const_scope_ends_with_its_block() {
    {
        const reader: R = crate::no_read;
        let _ = reader();
    }
    let _ = reader();
}

#[test]
fn off_block_const_naming_the_reader() {
    const via: R = crate::reader;
    let _ = via();
}
"#
            .to_string(),
        ),
    ]);
    assert_eq!(
        report.offenders,
        labels("tests.rs", &["off_nested_fn_that_reads::helper"]),
        "a nested function's own body is walked as a function of its own"
    );
    let indirect = labels(
        "tests.rs",
        &[
            "off_nested_fn_scope_ends_with_its_block",
            "off_another_test_calls_the_crate_reader",
            "off_another_test_calls_the_crate_reader_by_its_path",
            "off_nested_fn_that_reads",
            "off_reaches_the_reader_through_a_fn_nested_in_a_method",
            "off_block_const_scope_ends_with_its_block",
            "off_block_const_naming_the_reader",
        ],
    );
    assert_eq!(
        report.indirect_offenders, indirect,
        "a block item names its item in its block only; outside it the crate's reader is called"
    );
    let chains: Vec<(&str, Vec<&str>)> = vec![
        (
            "tests.rs::off_reaches_the_reader_through_a_fn_nested_in_a_method",
            vec![
                "lib.rs::Holder::read_all",
                "lib.rs::Holder::read_all::inner",
                "lib.rs::reader",
            ],
        ),
        (
            "tests.rs::off_nested_fn_that_reads",
            vec!["tests.rs::off_nested_fn_that_reads::helper"],
        ),
        (
            "tests.rs::off_block_const_naming_the_reader",
            vec![
                "tests.rs::off_block_const_naming_the_reader::via",
                "lib.rs::reader",
            ],
        ),
    ];
    for (name, expected) in chains {
        let actual: Vec<&str> = report
            .indirect_chains
            .get(name)
            .map(|chain| chain.iter().map(String::as_str).collect())
            .unwrap_or_default();
        assert_eq!(actual, expected, "the chain of {name}");
    }
    assert_eq!(
        (report.edges.path_ambiguous, report.values.path_ambiguous),
        (0, 0),
        "no block item makes a crate-wide call ambiguous"
    );
    assert_eq!(
        (report.held, report.env_test_code),
        (1, 2),
        "the locked nested helper holds; the reading one does not"
    );
}

/// Finding 5: an environment call is read however it is written: a `std::env`
/// accessor imported into a function, renamed, brought in by a glob, or
/// imported into the module (`use std::env;` with `env::set_var`), a global
/// path, `env::var_os` after `use std::*`, a path a macro builds from a crate
/// name (`$krate::env::var_os`), a call inside a macro's arguments (nested
/// macros included), an accessor named as a value, and a call in a
/// `macro_rules!` body the test invokes. A reader called inside a macro's arguments, through a
/// `macro_rules!` body (`$crate::` included), passed to a macro, or through a
/// macro defined in the test is followed. A crate function named `var`,
/// `env!` and `option_env!` (compile time), a method named like an accessor, a
/// macro that is defined but not invoked, and an environment call in
/// production code are not environment calls of the test.
#[test]
fn the_lock_discipline_reads_environment_calls_written_other_ways() {
    let report = planted(&[
        (
            "lib.rs",
            format!(
                r#"{PLANTED_ROOT}
pub mod config {{
    pub fn var() -> u8 {{
        0
    }}
}}

macro_rules! read_env {{
    () => {{
        std::env::var_os("W4")
    }};
}}

macro_rules! call_reader {{
    () => {{
        $crate::reader()
    }};
}}

macro_rules! call_it {{
    ($f:path) => {{
        $f()
    }};
}}

macro_rules! through_a_crate_name {{
    ($krate:ident) => {{
        $krate::env::var_os("W4")
    }};
}}

pub fn production_reader() -> Option<std::ffi::OsString> {{
    use std::env::var_os;
    var_os("W4")
}}

#[cfg(test)]
mod env_imports;
#[cfg(test)]
mod tests;
"#
            ),
        ),
        (
            "env_imports.rs",
            r#"use std::env;
use std::env::remove_var;

#[test]
fn off_short_path() {
    unsafe { env::set_var("W4", "1") };
}

#[test]
fn off_module_imported_accessor() {
    unsafe { remove_var("W4") };
}
"#
            .to_string(),
        ),
        (
            "tests.rs",
            r#"use super::*;

#[test]
fn off_use_set_var_in_the_fn() {
    use std::env::set_var;
    unsafe { set_var("W4", "1") };
}

#[test]
fn off_use_var_renamed() {
    use std::env::var as getenv;
    let _ = getenv("W4");
}

#[test]
fn off_glob_of_std_env() {
    use std::env::*;
    let _ = var_os("W4");
}

#[test]
fn off_global_path() {
    let _ = ::std::env::var("W4");
}

#[test]
fn off_env_call_inside_assert() {
    assert!(std::env::var_os("W4_NEVER").is_none());
}

#[test]
fn off_env_call_inside_format() {
    let _ = format!("{:?}", std::env::var_os("W4"));
}

#[test]
fn off_env_call_inside_nested_macros() {
    assert!(!format!("{:?}", std::env::vars().count()).is_empty());
}

#[test]
fn off_env_accessor_named_as_a_value() {
    let _ = ["W4"].into_iter().map(std::env::var).count();
}

#[test]
fn off_env_module_from_a_glob_of_std() {
    use std::*;
    let _ = env::var_os("W4");
}

#[test]
fn off_env_path_a_macro_builds_from_a_crate_name() {
    let _ = through_a_crate_name!(std);
}

#[test]
fn off_macro_rules_body_reads() {
    let _ = read_env!();
}

#[test]
fn off_reader_inside_assert() {
    assert!(reader().is_none() || true);
}

#[test]
fn off_reader_inside_macro_rules() {
    let _ = call_reader!();
}

#[test]
fn off_reader_passed_to_a_macro() {
    let _ = call_it!(crate::reader);
}

#[test]
fn off_macro_defined_in_the_fn() {
    macro_rules! go {
        () => {
            crate::reader()
        };
    }
    let _ = go!();
}

#[test]
fn ok_crate_fn_named_var() {
    let _ = config::var();
}

#[test]
fn ok_imported_crate_fn_named_var() {
    use crate::config::var;
    let _ = var();
}

#[test]
fn ok_compile_time_env_macros() {
    let _ = env!("PATH");
    let _ = option_env!("W4");
}

#[test]
fn ok_a_method_named_like_an_accessor() {
    struct S;
    impl S {
        fn var(&self) -> u8 {
            0
        }
    }
    let _ = S.var();
    assert!(S.var() == 0);
}

#[test]
fn ok_macro_defined_but_not_invoked() {
    macro_rules! unused {
        () => {
            std::env::var_os("W4")
        };
    }
}

#[test]
fn ok_held_with_the_env_call_in_a_macro() {
    let _g = crate::TEST_ENV_LOCK.lock().unwrap();
    assert!(std::env::var_os("W4_NEVER").is_none());
}

fn take(value: &mut Option<std::ffi::OsString>) -> Option<std::ffi::OsString> {
    value.take()
}

#[test]
fn off_reader_after_amp_mut_in_macro_tokens() {
    assert!(take(&mut crate::reader()).is_none() || true);
}
"#
            .to_string(),
        ),
    ]);
    let mut direct = labels(
        "env_imports.rs",
        &["off_short_path", "off_module_imported_accessor"],
    );
    direct.extend(labels(
        "tests.rs",
        &[
            "off_use_set_var_in_the_fn",
            "off_use_var_renamed",
            "off_glob_of_std_env",
            "off_global_path",
            "off_env_call_inside_assert",
            "off_env_call_inside_format",
            "off_env_call_inside_nested_macros",
            "off_env_accessor_named_as_a_value",
            "off_macro_rules_body_reads",
            "off_env_module_from_a_glob_of_std",
            "off_env_path_a_macro_builds_from_a_crate_name",
        ],
    ));
    direct.sort();
    assert_eq!(
        report.offenders, direct,
        "an environment call is read whatever its path, import or macro"
    );
    assert_eq!(
        report.indirect_offenders,
        labels(
            "tests.rs",
            &[
                "off_reader_inside_assert",
                "off_reader_inside_macro_rules",
                "off_reader_passed_to_a_macro",
                "off_macro_defined_in_the_fn",
                "off_reader_after_amp_mut_in_macro_tokens",
            ],
        ),
        "a reader reached through a macro is followed (after `&mut` too)"
    );
    assert_eq!(
        (report.held, report.env_test_code),
        (1, 14),
        "only the locked macro case holds"
    );
    assert_eq!(
        (report.edges.env_calls, report.values.env_calls),
        (15, 1),
        "fifteen environment calls (the crate's reader, the production reader, the \
         thirteen test cases) and one accessor named as a value"
    );
    assert!(
        report.edges.from_macros > 0 && report.values.from_macros > 0,
        "the macro tokens were read"
    );
}

/// Finding 6: a function named as a value is an edge to it, as a call is, so a
/// call through a local, a parenthesised path, a field, a closure parameter,
/// an array or a thread is reached from where the function is named; and an
/// intra-crate path resolves through `use` renames (in a block, in a module,
/// re-exported), turbofish, generic and bracketed types, qualified
/// `<T as Trait>` paths and `impl` targets written as paths. A quiet function
/// named as a value, a qualified path to a quiet impl, and paths out of the
/// crate are not followed to a read.
#[test]
fn the_lock_discipline_follows_functions_named_as_values() {
    let report = planted(&[
        (
            "lib.rs",
            format!(
                r#"{PLANTED_ROOT}
pub trait ReadsEnv {{
    fn read() -> Option<std::ffi::OsString>;
}}

pub struct Holder;
impl ReadsEnv for Holder {{
    fn read() -> Option<std::ffi::OsString> {{
        crate::reader()
    }}
}}

pub struct Quiet;
impl ReadsEnv for Quiet {{
    fn read() -> Option<std::ffi::OsString> {{
        None
    }}
}}

pub struct G<T>(pub T);
impl<T> G<T> {{
    pub fn read_generic() -> Option<std::ffi::OsString> {{
        crate::reader()
    }}
}}

pub enum Pick {{
    One(u8),
}}

pub struct Holder2;
impl crate::Holder2 {{
    pub fn read_via_path_impl() -> Option<std::ffi::OsString> {{
        crate::reader()
    }}
}}

pub fn generic_reader<T>() -> Option<std::ffi::OsString> {{
    crate::reader()
}}

pub mod renamed {{
    pub use crate::reader as fetch;
}}

#[cfg(test)]
mod module_renames;
#[cfg(test)]
mod tests;
"#
            ),
        ),
        (
            "module_renames.rs",
            r#"use crate::reader as module_fetch;

#[test]
fn off_module_use_rename() {
    let _ = module_fetch();
}
"#
            .to_string(),
        ),
        (
            "tests.rs",
            r#"use super::*;
use std::ffi::OsString;
type R = fn() -> Option<OsString>;

#[test]
fn off_fn_item_held_in_a_local() {
    let r = crate::reader;
    let _ = r();
}

#[test]
fn off_local_named_reader_holding_the_crate_reader() {
    let reader = reader;
    let _ = reader();
}

#[test]
fn off_parenthesised_callee() {
    let _ = (crate::reader)();
}

#[test]
fn off_fn_pointer_field() {
    struct S {
        f: R,
    }
    let s = S { f: reader };
    let _ = (s.f)();
}

#[test]
fn off_fn_passed_to_a_method() {
    let _ = None::<OsString>.or_else(reader);
}

#[test]
fn off_fn_passed_to_thread_spawn() {
    let _ = std::thread::spawn(reader).join();
}

#[test]
fn off_fn_passed_to_a_closure() {
    let f = |g: R| g();
    let _ = f(reader);
}

#[test]
fn off_fn_pointer_array() {
    for f in [reader as R] {
        let _ = f();
    }
}

#[test]
fn off_turbofish_call() {
    let _ = generic_reader::<u8>();
}

#[test]
fn off_turbofish_value() {
    let f = generic_reader::<u8>;
    let _ = f();
}

#[test]
fn off_trait_impl_called_by_its_type() {
    let _ = Holder::read();
}

#[test]
fn off_trait_impl_called_by_a_qualified_path() {
    let _ = <Holder as ReadsEnv>::read();
}

#[test]
fn off_generic_impl_called_with_a_turbofish_type() {
    let _ = G::<u8>::read_generic();
}

#[test]
fn off_generic_impl_called_by_a_bracketed_type() {
    let _ = <G<u8>>::read_generic();
}

#[test]
fn off_impl_declared_with_a_path_type() {
    let _ = Holder2::read_via_path_impl();
}

#[test]
fn off_block_use_rename() {
    use crate::reader as fetch;
    let _ = fetch();
}

#[test]
fn off_reexported_rename() {
    let _ = crate::renamed::fetch();
}

#[test]
fn off_closure_called_through_method_syntax() {
    let c = || reader();
    let _ = Some(&c).map(|c| c());
}

#[test]
fn ok_local_holding_a_quiet_fn() {
    let r = crate::no_read;
    let _ = r();
}

#[test]
fn ok_quiet_fn_passed_as_a_value() {
    let _ = None::<OsString>.or_else(no_read);
}

#[test]
fn ok_qualified_path_to_a_quiet_impl() {
    let _ = <Quiet as ReadsEnv>::read();
}

#[test]
fn ok_a_crate_enum_variant_is_a_constructor() {
    let _ = Pick::One(0);
    let _make: fn(u8) -> Pick = Pick::One;
}

#[test]
fn ok_paths_out_of_the_crate() {
    let _ = std::path::Path::new("x");
    let _ = String::from("x");
    let _ = <[u8]>::to_vec(&[1u8]);
}
"#
            .to_string(),
        ),
    ]);
    assert!(
        report.offenders.is_empty(),
        "no test touches the environment itself"
    );
    let mut indirect = labels("module_renames.rs", &["off_module_use_rename"]);
    indirect.extend(labels(
        "tests.rs",
        &[
            "off_fn_item_held_in_a_local",
            "off_local_named_reader_holding_the_crate_reader",
            "off_parenthesised_callee",
            "off_fn_pointer_field",
            "off_fn_passed_to_a_method",
            "off_fn_passed_to_thread_spawn",
            "off_fn_passed_to_a_closure",
            "off_fn_pointer_array",
            "off_turbofish_call",
            "off_turbofish_value",
            "off_trait_impl_called_by_its_type",
            "off_trait_impl_called_by_a_qualified_path",
            "off_generic_impl_called_with_a_turbofish_type",
            "off_generic_impl_called_by_a_bracketed_type",
            "off_impl_declared_with_a_path_type",
            "off_block_use_rename",
            "off_reexported_rename",
            "off_closure_called_through_method_syntax",
        ],
    ));
    indirect.sort();
    assert_eq!(
        report.indirect_offenders, indirect,
        "a function named as a value, or through a rename or a type path, is reached"
    );
    for accounting in [&report.edges, &report.values] {
        assert_eq!(
            (
                accounting.path_unresolved,
                accounting.path_ambiguous,
                accounting.path_trait_dispatch
            ),
            (0, 0, 0),
            "every path of this tree resolves to one body, a constructor (a crate enum's \
             variant among them) or out of the crate: {:?}",
            accounting.by_reason
        );
    }
}

/// The audit's surviving mutants of the body walk and the graph walk: a reader
/// called inside a closure, an async block, a loop or another call's arguments
/// is reached; a lock taken in a nested function does not cover the outer body;
/// each accessor alone is an environment call; the walk follows every callee,
/// not the first or the last only, and goes deeper than two calls (`Self::`
/// included); a helper call bound to `_` or written as a statement is
/// refused; and a call ambiguous between cfg variants holds the lock only when
/// every variant is a helper, and reaches a read when any variant reads.
#[test]
fn the_lock_discipline_walks_every_part_of_a_body() {
    let report = planted(&[
        (
            "lib.rs",
            format!(
                r#"#![allow(let_underscore_lock, unused_must_use)]
{PLANTED_ROOT}
pub fn clean() {{}}

pub struct SelfReader;
impl SelfReader {{
    pub fn outer() -> Option<std::ffi::OsString> {{
        Self::inner()
    }}
    fn inner() -> Option<std::ffi::OsString> {{
        crate::reader()
    }}
}}

pub fn fixture_guard() -> std::sync::MutexGuard<'static, ()> {{
    let guard = crate::TEST_ENV_LOCK.lock().unwrap();
    guard
}}

#[cfg(unix)]
pub fn acquire() -> std::sync::MutexGuard<'static, ()> {{
    crate::TEST_ENV_LOCK.lock().unwrap()
}}

#[cfg(not(unix))]
pub fn acquire() -> Option<u8> {{
    None
}}

#[cfg(unix)]
pub fn acquire_everywhere() -> std::sync::MutexGuard<'static, ()> {{
    crate::TEST_ENV_LOCK.lock().unwrap()
}}

#[cfg(not(unix))]
pub fn acquire_everywhere() -> std::sync::MutexGuard<'static, ()> {{
    crate::TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}}

#[cfg(unix)]
pub fn variant_reader() -> Option<std::ffi::OsString> {{
    std::env::var_os("W4")
}}

#[cfg(not(unix))]
pub fn variant_reader() -> Option<std::ffi::OsString> {{
    None
}}

#[cfg(test)]
mod tests;
"#
            ),
        ),
        (
            "tests.rs",
            r#"use super::*;

#[test]
fn off_reader_called_inside_a_closure() {
    let f = || reader();
    let _ = f();
}

#[test]
fn off_reader_called_inside_an_async_block() {
    let _fut = async { reader() };
}

#[test]
fn off_reader_called_inside_a_loop() {
    loop {
        let _ = reader();
        break;
    }
}

#[test]
fn off_reader_called_inside_arguments() {
    let _ = Some(reader());
}

#[test]
fn off_a_nested_fn_lock_does_not_cover_the_outer_body() {
    fn never_called() {
        let _g = crate::TEST_ENV_LOCK.lock();
    }
    let _ = reader();
}

#[test]
fn off_only_remove_var() {
    unsafe { std::env::remove_var("W4") };
}

#[test]
fn off_only_var() {
    let _ = std::env::var("W4");
}

#[test]
fn off_only_vars() {
    let _ = std::env::vars().count();
}

#[test]
fn off_only_vars_os() {
    let _ = std::env::vars_os().count();
}

#[test]
fn off_first_callee_reads() {
    let _ = reader();
    clean();
}

#[test]
fn off_second_callee_reads() {
    clean();
    let _ = reader();
}

#[test]
fn off_self_call_three_hops() {
    let _ = SelfReader::outer();
}

#[test]
fn off_guard_returning_call_discarded() {
    let _ = fixture_guard();
    let _ = reader();
}

#[test]
fn off_guard_returning_call_as_a_statement() {
    fixture_guard();
    let _ = reader();
}

#[test]
fn ok_guard_returning_call_bound() {
    let _g = fixture_guard();
    let _ = reader();
}

#[test]
fn off_one_cfg_variant_returns_no_guard() {
    let _g = acquire();
    let _ = reader();
}

#[test]
fn ok_every_cfg_variant_returns_the_guard() {
    let _g = acquire_everywhere();
    let _ = reader();
}

#[test]
fn off_one_cfg_variant_reads() {
    let _ = variant_reader();
}

#[test]
fn ok_no_callee_reads() {
    clean();
}
"#
            .to_string(),
        ),
    ]);
    assert_eq!(
        report.offenders,
        labels(
            "tests.rs",
            &[
                "off_only_remove_var",
                "off_only_var",
                "off_only_vars",
                "off_only_vars_os"
            ],
        ),
        "every accessor alone is an environment call"
    );
    let indirect = labels(
        "tests.rs",
        &[
            "off_reader_called_inside_a_closure",
            "off_reader_called_inside_an_async_block",
            "off_reader_called_inside_a_loop",
            "off_reader_called_inside_arguments",
            "off_a_nested_fn_lock_does_not_cover_the_outer_body",
            "off_first_callee_reads",
            "off_second_callee_reads",
            "off_self_call_three_hops",
            "off_guard_returning_call_discarded",
            "off_guard_returning_call_as_a_statement",
            "off_one_cfg_variant_returns_no_guard",
            "off_one_cfg_variant_reads",
        ],
    );
    assert_eq!(
        report.indirect_offenders, indirect,
        "every part of a body is walked"
    );
    assert_eq!(
        report
            .indirect_chains
            .get("tests.rs::off_self_call_three_hops")
            .cloned()
            .unwrap_or_default(),
        vec![
            "lib.rs::SelfReader::outer",
            "lib.rs::SelfReader::inner",
            "lib.rs::reader"
        ],
        "the chain runs three calls deep through Self::"
    );
    assert_eq!(
        report.refused_acquisitions,
        labels(
            "tests.rs",
            &[
                "off_guard_returning_call_discarded",
                "off_guard_returning_call_as_a_statement"
            ],
        ),
        "a guard-returning call whose guard dies at once is refused"
    );
    assert_eq!(
        report.helpers,
        [
            "lib.rs::acquire",
            "lib.rs::acquire_everywhere",
            "lib.rs::fixture_guard"
        ]
        .iter()
        .map(|name| name.to_string())
        .collect::<BTreeSet<_>>(),
        "a cfg variant whose body is one accepted lock call is a helper on its own"
    );
    assert_eq!(
        (report.edges.path_ambiguous, report.values.path_ambiguous),
        (3, 0),
        "the three calls of a cfg-variant function are ambiguous"
    );
    assert!(report.nesting_sites.is_empty(), "no holder reaches another");
}

/// The derive and trait-dispatch model, from both sides: a method a std
/// derive generates calls the same method of every crate type its fields name,
/// so `Wrapper::default()` reaches a field type's reading `default`, and so
/// does `<Wrapper as Default>::default()`; a call through an external trait's
/// path is narrowed by the type the syntax expects (a `let` type, a struct
/// literal's field or base), and with no type written it is an edge to every
/// crate body of that method. Serde's derived `deserialize` calls the function
/// a `#[serde(default = "..")]` names and the field type's `default` for a bare
/// `#[serde(default)]`, and a call out of the crate (`serde_json::from_str`)
/// whose value is written as a crate type, by a `let` type or a turbofish,
/// reaches that type's derived methods, as `toml::from_str` reaches
/// `DaemonConfig::deserialize` in the real crate. A `default` of a field
/// reaches the crate type a `Box` holds, and not the one an `Option` holds
/// (`None` builds no default of it), whether the derive is std's or serde's.
/// A derive over fields of types outside the crate, a trait path or a parse
/// whose type is outside the crate (a primitive, an external struct's
/// field), and a type whose serde attributes name no reader reach no crate
/// body that reads. `#[serde(deserialize_with = "..")]` and `#[serde(with =
/// "..")]` on a field, a positional field or a variant, and a bare
/// `#[serde(default)]` on the container, are followed; a derived `Hash`
/// reaches its fields' `hash`.
#[test]
fn the_lock_discipline_models_derives_and_trait_dispatch() {
    let report = planted(&[
        (
            "lib.rs",
            format!(
                r#"{PLANTED_ROOT}
pub struct Settings {{
    pub path: Option<std::ffi::OsString>,
}}

impl Default for Settings {{
    fn default() -> Self {{
        Settings {{ path: crate::reader() }}
    }}
}}

#[derive(Default)]
pub struct Wrapper {{
    pub settings: Settings,
    pub count: u32,
}}

#[derive(Default)]
pub struct Quiet {{
    pub count: u32,
    pub name: String,
}}

pub struct Loud;

impl Clone for Loud {{
    fn clone(&self) -> Self {{
        let _ = crate::reader();
        Loud
    }}
}}

#[derive(Clone)]
pub struct Cloned {{
    pub inner: Loud,
}}

pub fn default_path() -> Option<std::ffi::OsString> {{
    crate::reader()
}}

#[derive(serde::Deserialize)]
pub struct Config {{
    #[serde(default = "default_path")]
    pub path: Option<std::ffi::OsString>,
    pub count: u32,
}}

impl Config {{
    pub fn from_text(text: &str) -> Result<Self, serde_json::Error> {{
        let config: Self = serde_json::from_str(text)?;
        Ok(config)
    }}
}}

#[derive(serde::Deserialize)]
pub struct Tuned {{
    pub level: u8,
}}

impl Default for Tuned {{
    fn default() -> Self {{
        let _ = crate::reader();
        Tuned {{ level: 0 }}
    }}
}}

#[derive(serde::Deserialize)]
pub struct WithTuned {{
    #[serde(default)]
    pub tuned: Tuned,
}}

#[derive(serde::Deserialize)]
pub struct Plain {{
    pub count: u32,
}}

#[derive(serde::Deserialize)]
pub struct WithOptionalTuned {{
    #[serde(default)]
    pub tuned: Option<Tuned>,
}}

#[derive(serde::Deserialize)]
pub struct WithBoxedTuned {{
    #[serde(default)]
    pub tuned: Box<Tuned>,
}}

#[derive(Default)]
pub struct OptionalSettings {{
    pub settings: Option<Settings>,
}}

#[derive(Default)]
pub struct BoxedSettings {{
    pub settings: Box<Settings>,
}}

pub fn read_field() -> u8 {{
    let _ = crate::reader();
    0
}}

pub mod codec {{
    pub fn deserialize() -> u8 {{
        let _ = crate::reader();
        0
    }}
}}

#[derive(serde::Deserialize)]
pub struct WithDeserializer {{
    #[serde(deserialize_with = "crate::read_field")]
    pub level: u8,
}}

#[derive(serde::Deserialize)]
pub struct WithCodec {{
    #[serde(with = "crate::codec")]
    pub level: u8,
}}

#[derive(serde::Deserialize)]
pub enum Variants {{
    #[serde(deserialize_with = "crate::read_field")]
    Level(u8),
    Off,
}}

#[derive(serde::Deserialize)]
#[serde(default)]
pub struct ContainerDefault {{
    pub level: u8,
}}

impl Default for ContainerDefault {{
    fn default() -> Self {{
        let _ = crate::reader();
        ContainerDefault {{ level: 0 }}
    }}
}}

#[derive(serde::Deserialize)]
pub struct Wrapping {{
    pub inner: ContainerDefault,
}}

#[derive(serde::Deserialize)]
pub struct Positional(#[serde(deserialize_with = "crate::read_field")] pub u8);

pub struct Hashed;

impl std::hash::Hash for Hashed {{
    fn hash<H: std::hash::Hasher>(&self, _state: &mut H) {{
        let _ = crate::reader();
    }}
}}

#[derive(Hash)]
pub struct HashedOuter {{
    pub inner: Hashed,
}}

#[cfg(test)]
mod tests;
"#
            ),
        ),
        (
            "tests.rs",
            r#"use super::*;

fn take(_settings: Settings) {}

#[test]
fn off_derived_default_reaches_a_field_default() {
    let _ = Wrapper::default();
}

#[test]
fn off_qualified_derived_default() {
    let _ = <Wrapper as Default>::default();
}

#[test]
fn off_trait_path_with_a_crate_type_written() {
    let _w: Wrapper = Default::default();
}

#[test]
fn off_trait_path_in_a_struct_literal_base() {
    let _ = Wrapper {
        count: 1,
        ..Default::default()
    };
}

#[test]
fn off_trait_path_with_no_type_written() {
    take(Default::default());
}

#[test]
fn off_derived_clone_reaches_a_field_clone() {
    let c = Cloned { inner: Loud };
    let _ = Cloned::clone(&c);
}

#[test]
fn ok_derived_default_of_fields_outside_the_crate() {
    let _ = Quiet::default();
}

#[test]
fn ok_trait_path_with_a_primitive_written() {
    let _n: u32 = Default::default();
}

#[test]
fn ok_trait_path_in_a_field_of_a_struct_outside_the_crate() {
    let _ = std::ops::Range {
        start: 0u32,
        end: Default::default(),
    };
}

#[test]
fn off_derived_default_inside_a_macro() {
    assert_eq!(Wrapper::default().count, 0);
}

#[test]
fn off_parse_reaches_a_serde_default_function() {
    let _ = Config::from_text("{\"count\": 1}");
}

#[test]
fn off_turbofish_parse_reaches_a_serde_default_function() {
    let _ = serde_json::from_str::<Config>("{\"count\": 1}");
}

#[test]
fn off_parse_reaches_a_field_default() {
    let _parsed: WithTuned = serde_json::from_str("{}").unwrap();
}

#[test]
fn ok_parse_of_a_type_whose_attributes_name_no_reader() {
    let _plain: Plain = serde_json::from_str("{\"count\": 1}").unwrap();
}

#[test]
fn ok_parse_at_a_type_outside_the_crate() {
    let _n: u32 = serde_json::from_str("1").unwrap();
}

#[test]
fn ok_parse_of_an_optional_field_with_a_default() {
    let _parsed: WithOptionalTuned = serde_json::from_str("{}").unwrap();
}

#[test]
fn off_parse_of_a_boxed_field_with_a_default() {
    let _parsed: WithBoxedTuned = serde_json::from_str("{}").unwrap();
}

#[test]
fn ok_derived_default_of_an_optional_field() {
    let _ = OptionalSettings::default();
}

#[test]
fn off_derived_default_of_a_boxed_field() {
    let _ = BoxedSettings::default();
}

#[test]
fn off_trait_path_with_a_boxed_crate_type_written() {
    let _boxed: Box<Settings> = Default::default();
}

#[test]
fn ok_trait_path_with_an_optional_crate_type_written() {
    let _optional: Option<Settings> = Default::default();
}

#[test]
fn off_parse_reaches_a_deserialize_with_function() {
    let _parsed: WithDeserializer = serde_json::from_str("{}").unwrap();
}

#[test]
fn off_parse_reaches_a_with_modules_deserialize() {
    let _parsed: WithCodec = serde_json::from_str("{}").unwrap();
}

#[test]
fn off_parse_reaches_a_variants_deserialize_with() {
    let _parsed: Variants = serde_json::from_str("{}").unwrap();
}

#[test]
fn off_parse_reaches_a_container_default() {
    let _parsed: Wrapping = serde_json::from_str("{}").unwrap();
}

#[test]
fn off_parse_reaches_a_positional_fields_deserialize_with() {
    let _parsed: Positional = serde_json::from_str("1").unwrap();
}

#[test]
fn off_derived_hash_reaches_a_field_hash() {
    use std::hash::Hash;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    HashedOuter::hash(&HashedOuter { inner: Hashed }, &mut hasher);
}
"#
            .to_string(),
        ),
    ]);
    assert!(
        report.offenders.is_empty(),
        "no test touches the environment itself"
    );
    assert_eq!(
        report.indirect_offenders,
        labels(
            "tests.rs",
            &[
                "off_derived_default_reaches_a_field_default",
                "off_qualified_derived_default",
                "off_trait_path_with_a_crate_type_written",
                "off_trait_path_in_a_struct_literal_base",
                "off_trait_path_with_no_type_written",
                "off_derived_clone_reaches_a_field_clone",
                "off_parse_reaches_a_serde_default_function",
                "off_turbofish_parse_reaches_a_serde_default_function",
                "off_parse_reaches_a_field_default",
                "off_derived_default_inside_a_macro",
                "off_parse_of_a_boxed_field_with_a_default",
                "off_derived_default_of_a_boxed_field",
                "off_trait_path_with_a_boxed_crate_type_written",
                "off_parse_reaches_a_deserialize_with_function",
                "off_parse_reaches_a_with_modules_deserialize",
                "off_parse_reaches_a_variants_deserialize_with",
                "off_parse_reaches_a_container_default",
                "off_parse_reaches_a_positional_fields_deserialize_with",
                "off_derived_hash_reaches_a_field_hash",
            ],
        ),
        "a derived method and a trait path reach the crate bodies that can run"
    );
    let chains: Vec<(&str, Vec<&str>)> = vec![
        (
            "tests.rs::off_derived_default_reaches_a_field_default",
            vec![
                "lib.rs::Wrapper::default",
                "lib.rs::Settings::default",
                "lib.rs::reader",
            ],
        ),
        (
            "tests.rs::off_trait_path_with_a_crate_type_written",
            vec![
                "lib.rs::Wrapper::default",
                "lib.rs::Settings::default",
                "lib.rs::reader",
            ],
        ),
        (
            "tests.rs::off_trait_path_with_no_type_written",
            vec!["lib.rs::Settings::default", "lib.rs::reader"],
        ),
        (
            "tests.rs::off_derived_clone_reaches_a_field_clone",
            vec![
                "lib.rs::Cloned::clone",
                "lib.rs::Loud::clone",
                "lib.rs::reader",
            ],
        ),
        (
            "tests.rs::off_parse_reaches_a_serde_default_function",
            vec![
                "lib.rs::Config::from_text",
                "lib.rs::Config::deserialize",
                "lib.rs::default_path",
                "lib.rs::reader",
            ],
        ),
        (
            "tests.rs::off_turbofish_parse_reaches_a_serde_default_function",
            vec![
                "lib.rs::Config::deserialize",
                "lib.rs::default_path",
                "lib.rs::reader",
            ],
        ),
        (
            "tests.rs::off_parse_reaches_a_field_default",
            vec![
                "lib.rs::WithTuned::deserialize",
                "lib.rs::Tuned::default",
                "lib.rs::reader",
            ],
        ),
        (
            "tests.rs::off_parse_reaches_a_with_modules_deserialize",
            vec![
                "lib.rs::WithCodec::deserialize",
                // A label names no inline module: this is `codec::deserialize`.
                "lib.rs::deserialize",
                "lib.rs::reader",
            ],
        ),
        (
            "tests.rs::off_parse_reaches_a_container_default",
            vec![
                "lib.rs::Wrapping::deserialize",
                "lib.rs::ContainerDefault::deserialize",
                "lib.rs::ContainerDefault::default",
                "lib.rs::reader",
            ],
        ),
        (
            "tests.rs::off_derived_hash_reaches_a_field_hash",
            vec![
                "lib.rs::HashedOuter::hash",
                "lib.rs::Hashed::hash",
                "lib.rs::reader",
            ],
        ),
    ];
    for (name, expected) in chains {
        let actual: Vec<&str> = report
            .indirect_chains
            .get(name)
            .map(|chain| chain.iter().map(String::as_str).collect())
            .unwrap_or_default();
        assert_eq!(actual, expected, "the chain of {name}");
    }
    assert_eq!(
        report.derived_methods, 18,
        "four default, Cloned::clone, HashedOuter::hash and twelve deserialize are modelled"
    );
    assert_eq!(
        report.edges.path_external_at_crate_type, 11,
        "eleven calls out of the crate write a crate type for their value; the u32 one does not"
    );
    assert_eq!(
        report.defaults_not_followed, 3,
        "the two Option fields' defaults and the Option let's hold no default of the crate type they wrap"
    );
    assert_eq!(
        report.edges.path_trait_dispatch, 1,
        "only the trait path with no type written stays an edge to every candidate"
    );
}

/// Decision D-i7-envlock-1, a shape-1 guard's coverage, and round 7's fourth audit
/// (blockers 1 and 3): a guard covers the sites of its own region from the
/// end of its `let` to the end of its block, or to the first mention of its
/// name after the `let`, whatever the mention does (a move, a borrow, a method
/// call on it, a dereference, an argument, a capture, a token of a macro, a
/// raw identifier of the same name), whichever comes first; a mention inside
/// a loop, a closure, an async block or a macro's tokens ends it where the
/// outermost of them starts. Its region is the innermost closure or async
/// block around its `let`, or the body: a site in another closure, async
/// block, or a macro that does not run its tokens in place (a crate
/// `macro_rules!`, which may put them in a closure) is not covered, though the
/// standard library's `assert!` runs its tokens in place, and its
/// `stringify!` (not in `IN_PLACE_MACROS`) and a dependency's macro named
/// `assert!` do not. So a guard in an inner block covers that block only, one
/// in a closure or an async block covers that closure's or block's own reads,
/// a loop that never names the guard is covered, and a `for`, `while` or
/// `loop` that names it, or a struct pattern that shadows its name, ends it. These, sound at run time, are reported by design: a
/// closure or async block run while the guard lives, a closure that owns the
/// guard, a borrow of the guard before a read, and `mem::forget`.
#[test]
fn the_lock_discipline_covers_a_site_by_its_guards_live_range() {
    let report = planted(&[
        (
            "lib.rs",
            format!(
                "#![allow(unused_assignments, unused_variables, unreachable_code, forgetting_copy_types, clippy::all)]\n{PLANTED_ROOT}\n#[cfg(test)]\nmod tests;\n"
            ),
        ),
        (
            "tests.rs",
            r#"macro_rules! later {
    ($read:expr) => {
        $read
    };
}

#[test]
fn ok_block_scoped_guard_covers_its_read() {
    let config = {
        let _env = crate::TEST_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        crate::reader()
    };
    let _ = config;
}

#[test]
fn ok_block_scoped_guard_covers_its_env_call() {
    {
        let _env = crate::TEST_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        unsafe { std::env::set_var("W4", "1") };
    }
}

#[test]
fn ok_guard_in_a_closure_covers_the_closures_read() {
    let f = || {
        let _g = crate::TEST_ENV_LOCK.lock().unwrap();
        crate::reader()
    };
    let _ = f();
}

#[test]
fn ok_guard_in_an_inner_closure_covers_its_own_read() {
    let outer = || {
        let inner = || {
            let _g = crate::TEST_ENV_LOCK.lock().unwrap();
            crate::reader()
        };
        inner()
    };
    let _ = outer();
}

#[test]
fn ok_guard_in_an_async_block_covers_the_blocks_read() {
    let _future = async {
        let _g = crate::TEST_ENV_LOCK.lock().unwrap();
        crate::reader()
    };
}

#[test]
fn ok_an_assert_runs_its_tokens_in_place() {
    let _g = crate::TEST_ENV_LOCK.lock().unwrap();
    assert!(crate::reader().is_none() || std::hint::black_box(true));
    assert_eq!(std::env::var_os("W4").is_some(), std::env::var_os("W4").is_some());
}

#[test]
fn ok_a_loop_that_never_names_the_guard() {
    let _g = crate::TEST_ENV_LOCK.lock().unwrap();
    for _ in 0..2 {
        unsafe { std::env::set_var("W4", "1") };
    }
}

#[test]
fn ok_a_labelled_block_that_never_names_the_guard() {
    let _g = crate::TEST_ENV_LOCK.lock().unwrap();
    'early: {
        if std::hint::black_box(false) {
            break 'early;
        }
        unsafe { std::env::set_var("W4", "1") };
    }
}

#[test]
fn ok_a_mention_after_the_env_call() {
    let g = crate::TEST_ENV_LOCK.lock().unwrap();
    unsafe { std::env::set_var("W4", "1") };
    drop(g);
}

#[test]
fn off_read_after_the_guards_block() {
    let config = {
        let _env = crate::TEST_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        crate::reader()
    };
    let _ = (config, crate::reader());
}

#[test]
fn off_env_call_after_the_guards_block() {
    {
        let _env = crate::TEST_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    }
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn off_read_before_the_binding_in_its_block() {
    {
        let _ = crate::reader();
        let _env = crate::TEST_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    }
}

#[test]
fn off_env_call_before_the_lock() {
    unsafe { std::env::set_var("W4", "1") };
    let _g = crate::TEST_ENV_LOCK.lock().unwrap();
}

#[test]
fn off_guard_in_a_closure_does_not_cover_a_read_after_it() {
    let f = || {
        let _g = crate::TEST_ENV_LOCK.lock().unwrap();
    };
    f();
    let _ = crate::reader();
}

#[test]
fn off_guard_in_an_async_block_does_not_cover_a_read_after_it() {
    let _future = async {
        let _g = crate::TEST_ENV_LOCK.lock().unwrap();
    };
    let _ = crate::reader();
}

#[test]
fn off_an_outer_guard_does_not_cover_a_closure() {
    let _g = crate::TEST_ENV_LOCK.lock().unwrap();
    let f = || crate::reader();
    let _ = f();
}

#[test]
fn off_a_closure_called_after_the_guard_drops() {
    let g = crate::TEST_ENV_LOCK.lock().unwrap();
    let f = || crate::reader();
    drop(g);
    let _ = f();
}

#[test]
fn off_a_closure_made_under_a_guard_and_run_after_it() {
    let f = {
        let _g = crate::TEST_ENV_LOCK.lock().unwrap();
        || crate::reader()
    };
    let _ = f();
}

#[test]
fn off_a_thread_spawned_under_a_guard() {
    let g = crate::TEST_ENV_LOCK.lock().unwrap();
    let handle = std::thread::spawn(|| crate::reader());
    drop(g);
    let _ = handle.join();
}

#[test]
fn off_an_async_block_under_a_guard() {
    let _g = crate::TEST_ENV_LOCK.lock().unwrap();
    let _future = async { crate::reader() };
}

#[test]
fn off_a_crate_macro_does_not_run_its_tokens_in_place() {
    let _g = crate::TEST_ENV_LOCK.lock().unwrap();
    let _ = later!(crate::reader());
}

#[test]
fn off_guard_dropped_before_the_env_call() {
    let g = crate::TEST_ENV_LOCK.lock().unwrap();
    drop(g);
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn off_guard_shadowed_before_the_env_call() {
    let g = crate::TEST_ENV_LOCK.lock().unwrap();
    let g = ();
    unsafe { std::env::set_var("W4", "1") };
    let _ = g;
}

#[test]
fn off_guard_moved_into_an_inner_block() {
    let g = crate::TEST_ENV_LOCK.lock().unwrap();
    {
        let _moved = g;
    }
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn off_guard_moved_into_a_closure_called_at_once() {
    let g = crate::TEST_ENV_LOCK.lock().unwrap();
    (move || {
        let _inner = g;
    })();
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn off_a_closure_that_owns_the_guard() {
    let g = crate::TEST_ENV_LOCK.lock().unwrap();
    let f = move || {
        let _ = crate::reader();
        drop(g);
    };
    f();
}

#[test]
fn off_a_closure_keeps_the_guard_it_took() {
    let g = crate::TEST_ENV_LOCK.lock().unwrap();
    let f = move || drop(g);
    let _ = crate::reader();
    f();
}

#[test]
fn off_a_method_call_on_the_lock_result() {
    let result = crate::TEST_ENV_LOCK.lock();
    result.unwrap();
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn off_a_dereference_of_the_guard() {
    let g = crate::TEST_ENV_LOCK.lock().unwrap();
    let () = *g;
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn off_a_borrow_of_the_guard() {
    let g = crate::TEST_ENV_LOCK.lock().unwrap();
    let _borrow = &g;
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn off_the_guard_passed_to_a_call() {
    let g = crate::TEST_ENV_LOCK.lock().unwrap();
    let _ = std::mem::size_of_val(&g);
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn off_the_guard_forgotten() {
    let g = crate::TEST_ENV_LOCK.lock().unwrap();
    std::mem::forget(g);
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn off_a_mention_in_a_loop_after_the_env_call() {
    let g = crate::TEST_ENV_LOCK.lock().unwrap();
    for _ in 0..2 {
        unsafe { std::env::set_var("W4", "1") };
        let _ = &g;
    }
}

#[test]
fn off_a_mention_in_a_while_body_after_the_env_call() {
    let g = crate::TEST_ENV_LOCK.lock().unwrap();
    let mut turns = 0;
    while turns < 2 {
        unsafe { std::env::set_var("W4", "1") };
        let _ = &g;
        turns += 1;
    }
}

#[test]
fn off_a_mention_in_a_while_condition() {
    let g = crate::TEST_ENV_LOCK.lock().unwrap();
    while std::mem::size_of_val(&g) == 0 {
        unsafe { std::env::set_var("W4", "1") };
    }
}

#[test]
fn off_a_mention_in_an_assert_after_the_env_call() {
    let g = crate::TEST_ENV_LOCK.lock().unwrap();
    assert!({
        unsafe { std::env::set_var("W4", "1") };
        std::mem::size_of_val(&g) > 0
    });
}

#[test]
fn off_a_raw_identifier_of_the_same_name() {
    let r#g = crate::TEST_ENV_LOCK.lock().unwrap();
    drop(g);
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn off_a_mention_in_a_loop_expression() {
    let g = crate::TEST_ENV_LOCK.lock().unwrap();
    loop {
        unsafe { std::env::set_var("W4", "1") };
        let _ = &g;
        break;
    }
}

#[test]
fn off_the_name_shadowed_by_a_struct_pattern() {
    struct Named {
        g: u8,
    }
    let g = crate::TEST_ENV_LOCK.lock().unwrap();
    let Named { g } = Named { g: 0 };
    unsafe { std::env::set_var("W4", "1") };
    let _ = g;
}

#[test]
fn off_a_std_macro_not_in_the_list() {
    let _g = crate::TEST_ENV_LOCK.lock().unwrap();
    let _ = stringify!(crate::reader());
}

#[test]
fn off_a_dependency_macro_named_like_a_std_one() {
    let _g = crate::TEST_ENV_LOCK.lock().unwrap();
    clap::assert!(crate::reader());
}

#[test]
fn off_a_labelled_break_after_a_mention() {
    let g = crate::TEST_ENV_LOCK.lock().unwrap();
    'early: {
        if std::hint::black_box(true) {
            drop(g);
            break 'early;
        }
    }
    unsafe { std::env::set_var("W4", "1") };
}

#[test]
fn off_covered_env_call_then_an_unlocked_read() {
    {
        let _env = crate::TEST_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        unsafe { std::env::set_var("W4", "1") };
    }
    let _ = crate::reader();
}
"#
            .to_string(),
        ),
    ]);
    assert_eq!(
        report.offenders,
        labels(
            "tests.rs",
            &[
                "off_env_call_after_the_guards_block",
                "off_env_call_before_the_lock",
                "off_guard_dropped_before_the_env_call",
                "off_guard_shadowed_before_the_env_call",
                "off_guard_moved_into_an_inner_block",
                "off_guard_moved_into_a_closure_called_at_once",
                "off_a_method_call_on_the_lock_result",
                "off_a_dereference_of_the_guard",
                "off_a_borrow_of_the_guard",
                "off_the_guard_passed_to_a_call",
                "off_the_guard_forgotten",
                "off_a_mention_in_a_loop_after_the_env_call",
                "off_a_mention_in_a_while_body_after_the_env_call",
                "off_a_mention_in_a_while_condition",
                "off_a_mention_in_an_assert_after_the_env_call",
                "off_a_raw_identifier_of_the_same_name",
                "off_a_labelled_break_after_a_mention",
                "off_a_mention_in_a_loop_expression",
                "off_the_name_shadowed_by_a_struct_pattern",
            ]
        ),
        "an environment call outside a guard's coverage is reported"
    );
    assert_eq!(
        report.indirect_offenders,
        labels(
            "tests.rs",
            &[
                "off_read_after_the_guards_block",
                "off_read_before_the_binding_in_its_block",
                "off_guard_in_a_closure_does_not_cover_a_read_after_it",
                "off_guard_in_an_async_block_does_not_cover_a_read_after_it",
                "off_an_outer_guard_does_not_cover_a_closure",
                "off_a_closure_called_after_the_guard_drops",
                "off_a_closure_made_under_a_guard_and_run_after_it",
                "off_a_thread_spawned_under_a_guard",
                "off_an_async_block_under_a_guard",
                "off_a_crate_macro_does_not_run_its_tokens_in_place",
                "off_a_closure_that_owns_the_guard",
                "off_a_closure_keeps_the_guard_it_took",
                "off_covered_env_call_then_an_unlocked_read",
                "off_a_std_macro_not_in_the_list",
                "off_a_dependency_macro_named_like_a_std_one",
            ]
        ),
        "a read reached outside a guard's coverage is reported, by a test whose own \
         environment calls are covered too"
    );
    assert_eq!(
        (report.held, report.env_test_code),
        (6, 25),
        "the block-scoped guards (twice), the assert, the loop, the labelled block and the \
         mention after the call cover their environment calls"
    );
    assert!(
        report.refused_acquisitions.is_empty(),
        "every lock call is a shape-1 let: {:?}",
        report.refused_acquisitions
    );
    assert_eq!(
        report
            .guards_by_place
            .iter()
            .map(|(place, count)| (*place, *count))
            .collect::<Vec<_>>(),
        vec![
            ("a let at the top of the body", 31),
            ("a let in a closure or async block", 5),
            ("a let in an inner block", 7),
        ],
        "every guard is counted where its let stands"
    );
    assert_eq!(
        report.guards_ended_by_a_mention.len(),
        22,
        "every guard a mention ends early is listed: {:?}",
        report.guards_ended_by_a_mention
    );
    assert_eq!(
        report.sites_in_another_region,
        labels(
            "tests.rs",
            &[
                "off_an_outer_guard_does_not_cover_a_closure: reader",
                "off_a_closure_called_after_the_guard_drops: reader",
                "off_a_closure_made_under_a_guard_and_run_after_it: reader",
                "off_a_thread_spawned_under_a_guard: reader",
                "off_an_async_block_under_a_guard: reader",
                "off_a_crate_macro_does_not_run_its_tokens_in_place: reader",
                "off_a_std_macro_not_in_the_list: reader",
                "off_a_dependency_macro_named_like_a_std_one: reader",
            ]
        ),
        "a site inside a guard's range in another region is listed, not covered"
    );
    assert!(report.nesting_sites.is_empty(), "no guard is taken twice");
}

/// Round 7, blockers 3 and 4: a crate `macro_rules!` is found where rustc
/// finds it and expanded with its transcriber resolved at the call site. A
/// bare name is in textual scope after its definition, in the modules
/// declared after it (file modules too), and after a `#[macro_use]` module;
/// `crate::name!` reaches a `#[macro_export]` macro, and a path reaches one
/// a module re-exports with `use`. An item path in a transcriber names what
/// the call site sees (checked against rustc, which resolves it there), so
/// the same macro reads in one module and not in another; a macro defined
/// after the call, or in a module without `#[macro_use]`, is not in scope,
/// and of two definitions of one name the call sees the one before it. A
/// crate macro invoked inside another macro's tokens is expanded too; a field
/// name in tokens (`Named { reader: 1 }`) is no reference; and an environment
/// call a production macro expands to is test code where a test-only helper
/// invokes it. Round 7's third audit (A41 to A48): of two definitions before
/// the call the last is in scope, a `#[macro_use]` module's definition
/// shadows an earlier one, `crate::name!` reaches the exported macro and not
/// a private one of that name in another module, every rule's transcriber is
/// read, a transcriber's local name does not see the call site's locals, and
/// a macro a transcriber invokes is expanded.
#[test]
fn the_lock_discipline_finds_and_expands_macros_as_rustc_does() {
    let report = planted(&[
        (
            "lib.rs",
            format!(
                r#"#![allow(unused_macros, unused_imports, dead_code)]
{PLANTED_ROOT}
pub mod exported {{
    #[macro_export]
    macro_rules! read_env {{
        () => {{
            std::env::var_os("W4")
        }};
    }}
}}

#[macro_use]
mod textual {{
    macro_rules! plant {{
        () => {{
            unsafe {{ std::env::set_var("W4", "1") }}
        }};
    }}
}}

mod unexported {{
    macro_rules! plant_unexported {{
        () => {{
            unsafe {{ std::env::set_var("W4", "1") }}
        }};
    }}
}}

pub mod quiet_place {{
    pub fn reader() -> Option<std::ffi::OsString> {{
        None
    }}
    macro_rules! call_reader {{
        () => {{
            reader()
        }};
    }}
    pub(crate) use call_reader;

    #[cfg(test)]
    mod here {{
        use super::reader;

        #[test]
        fn ok_transcriber_resolved_where_reader_is_quiet() {{
            let _ = super::call_reader!();
        }}
    }}
}}

macro_rules! defined_before_the_tests {{
    () => {{
        crate::reader()
    }};
}}

macro_rules! defined_twice {{
    () => {{
        None::<std::ffi::OsString>
    }};
}}

macro_rules! first_then_last {{
    () => {{
        crate::no_read()
    }};
}}

macro_rules! first_then_last {{
    () => {{
        crate::reader()
    }};
}}

macro_rules! shadowed_by_macro_use {{
    () => {{
        crate::no_read()
    }};
}}

#[macro_use]
mod later {{
    macro_rules! shadowed_by_macro_use {{
        () => {{
            crate::reader()
        }};
    }}
}}

#[macro_export]
macro_rules! exported_reader {{
    () => {{
        $crate::reader()
    }};
}}

mod private_namesake {{
    macro_rules! exported_reader {{
        () => {{
            0
        }};
    }}
}}

macro_rules! by_rule {{
    (a) => {{
        crate::no_read()
    }};
    (b) => {{
        crate::reader()
    }};
}}

macro_rules! item_path {{
    () => {{
        reader()
    }};
}}

macro_rules! inner_reads {{
    () => {{
        crate::reader()
    }};
}}

macro_rules! outer_calls_inner {{
    () => {{
        inner_reads!()
    }};
}}

#[cfg(test)]
mod tests;

macro_rules! defined_after_the_tests {{
    () => {{
        crate::reader()
    }};
}}

macro_rules! defined_twice {{
    () => {{
        crate::reader()
    }};
}}

#[cfg(test)]
mod inline_tests {{
    fn off_helper_expands_a_production_macro() -> Option<std::ffi::OsString> {{
        crate::read_env!()
    }}
}}
"#
            ),
        ),
        (
            "tests.rs",
            r#"use crate::reader;

#[test]
fn off_macro_export_by_crate_path() {
    let _ = crate::read_env!();
}

#[test]
fn off_macro_use_textual_scope() {
    plant!();
}

#[test]
fn off_transcriber_item_resolves_at_the_call_site() {
    let _ = crate::quiet_place::call_reader!();
}

#[test]
fn off_textual_scope_reaches_a_file_module() {
    let _ = defined_before_the_tests!();
}

#[test]
fn ok_the_definition_before_the_module_is_in_scope() {
    let _ = defined_twice!();
}

#[test]
fn off_a_crate_macro_inside_another_macros_tokens() {
    let _ = format!("{:?}", defined_before_the_tests!());
}

#[test]
fn off_the_last_definition_before_the_call_reads() {
    let _ = first_then_last!();
}

#[test]
fn off_a_macro_use_module_shadows_an_earlier_definition() {
    let _ = shadowed_by_macro_use!();
}

#[test]
fn off_an_exported_macro_beside_a_private_namesake() {
    let _ = crate::exported_reader!();
}

#[test]
fn off_a_rule_after_the_first_reads() {
    let _ = by_rule!(b);
}

#[test]
fn off_a_transcriber_path_is_not_a_call_site_local() {
    let reader = || None::<std::ffi::OsString>;
    let _ = reader();
    let _ = item_path!();
}

#[test]
fn off_a_macro_a_transcriber_invokes() {
    let _ = outer_calls_inner!();
}

#[test]
fn off_a_crate_macro_twice_in_one_token_tree() {
    let _ = format!("{:?}{:?}", inner_reads!(), inner_reads!());
}

struct Named {
    reader: u8,
}

#[test]
fn ok_a_field_name_in_tokens_is_not_a_reference() {
    let named = vec![Named { reader: 1 }];
    assert_eq!(named[0].reader, 1);
}

mod hygiene {
    #[allow(dead_code)]
    fn reader() -> Option<std::ffi::OsString> {
        None
    }

    macro_rules! call_local_reader {
        () => {
            reader()
        };
    }

    #[test]
    fn ok_same_module_call_is_quiet() {
        let _ = call_local_reader!();
    }

    mod inner {
        use crate::reader;

        #[test]
        fn off_inner_call_reads() {
            let _ = call_local_reader!();
        }
    }
}
"#
            .to_string(),
        ),
    ]);
    let mut direct = labels(
        "tests.rs",
        &[
            "off_macro_export_by_crate_path",
            "off_macro_use_textual_scope",
        ],
    );
    direct.extend(labels("lib.rs", &["off_helper_expands_a_production_macro"]));
    direct.sort();
    assert_eq!(
        report.offenders, direct,
        "a macro reached by `crate::` or through `#[macro_use]` is expanded, and an \
         environment call it expands to stands where it is invoked"
    );
    let mut indirect = labels(
        "tests.rs",
        &[
            "off_transcriber_item_resolves_at_the_call_site",
            "off_textual_scope_reaches_a_file_module",
            "off_inner_call_reads",
            "off_a_crate_macro_inside_another_macros_tokens",
            "off_the_last_definition_before_the_call_reads",
            "off_a_macro_use_module_shadows_an_earlier_definition",
            "off_an_exported_macro_beside_a_private_namesake",
            "off_a_rule_after_the_first_reads",
            "off_a_transcriber_path_is_not_a_call_site_local",
            "off_a_macro_a_transcriber_invokes",
            "off_a_crate_macro_twice_in_one_token_tree",
        ],
    );
    indirect.sort();
    assert_eq!(
        report.indirect_offenders, indirect,
        "a transcriber's item path names what the call site sees"
    );
    assert_eq!(
        (report.macro_definitions, report.exported_macros),
        (19, 2),
        "nineteen macro_rules! definitions, two exported"
    );
    assert_eq!(
        (report.edges.calls_walked, report.edges.from_macros),
        (21, 18),
        "every expansion is read, a crate macro invoked twice in one token tree twice"
    );
}

/// Round 7, blocker 5 and the notes on impls: a path into the crate is
/// followed or reported unresolved, never silently taken out of the crate.
/// `extern crate self as me` makes `me::` and `::me::` this crate; `<dyn
/// crate::Trait>::f` and `<u32 as crate::Trait>::f` name the trait's items in
/// every impl; a call out of the crate whose type names a crate type in a
/// generic argument reaches that type's derived methods; an impl written on a
/// type alias is the aliased type's, and `Alias::f` reaches the aliased
/// type's items; a blanket impl's item is every type's, its bounds not read;
/// a call out of the crate at a crate type runs that type's impls of traits
/// outside the crate (`Clone` through `repeat_n`), the type read through a
/// block's trailing `if` to its `let` too; a `#[path]` module is
/// found; `self` is a value. An explicit import shadows a glob, and glob
/// imports that reach one another are visited once per scope. A generic
/// parameter that shares a crate type's name names no crate type: its path is
/// the tree's one unresolved path.
#[test]
fn the_lock_discipline_follows_every_path_into_the_crate() {
    let report = planted(&[
        (
            "lib.rs",
            format!(
                r#"#![allow(dead_code, unused_imports)]
extern crate self as me;
{PLANTED_ROOT}
pub mod quiet {{
    pub fn reader() -> Option<std::ffi::OsString> {{
        None
    }}
}}

pub type Alias = Holder;
pub struct Holder;
impl Alias {{
    pub fn read_via_alias_impl() -> Option<std::ffi::OsString> {{
        crate::reader()
    }}
}}
impl Holder {{
    pub fn by_value(self) -> Self {{
        let _ = format!("{{:p}}", &self);
        self
    }}
}}

pub struct Copied;
impl Clone for Copied {{
    fn clone(&self) -> Self {{
        let _ = crate::reader();
        Copied
    }}
}}

pub struct Probe;
impl Probe {{
    pub fn go() -> u8 {{
        let _ = crate::reader();
        0
    }}
}}
pub trait Go {{
    fn go() -> u8;
}}

#[path = "elsewhere/renamed_file.rs"]
pub mod renamed;

pub trait Marker {{}}
impl Marker for Holder {{}}
pub trait Blanket {{
    fn blanket_read() -> Option<std::ffi::OsString>;
}}
impl<T: Marker> Blanket for T {{
    fn blanket_read() -> Option<std::ffi::OsString> {{
        crate::reader()
    }}
}}

pub trait Dyn {{
    fn dyn_read(&self) -> Option<std::ffi::OsString>;
}}
impl Dyn for Holder {{
    fn dyn_read(&self) -> Option<std::ffi::OsString> {{
        crate::reader()
    }}
}}

pub trait ForPrimitives {{
    fn prim_read() -> Option<std::ffi::OsString>;
}}
impl ForPrimitives for u32 {{
    fn prim_read() -> Option<std::ffi::OsString> {{
        crate::reader()
    }}
}}

pub fn default_count() -> u32 {{
    let _ = crate::reader();
    0
}}

#[derive(serde::Deserialize)]
pub struct Plain {{
    #[serde(default = "crate::default_count")]
    pub count: u32,
}}

pub mod m0 {{
    pub use super::m1::*;
    pub use super::m2::*;
    pub fn quiet_fn() {{}}
}}
pub mod m1 {{
    pub use super::m2::*;
    pub use super::m3::*;
}}
pub mod m2 {{
    pub use super::m3::*;
    pub use super::m4::*;
}}
pub mod m3 {{
    pub use super::m4::*;
    pub use super::m5::*;
}}
pub mod m4 {{
    pub use super::m5::*;
    pub use super::m6::*;
}}
pub mod m5 {{
    pub use super::m6::*;
    pub use super::m7::*;
}}
pub mod m6 {{
    pub use super::m7::*;
    pub use super::m0::*;
}}
pub mod m7 {{
    pub use super::m0::*;
    pub use super::m1::*;
}}

#[cfg(test)]
mod tests;

#[cfg(test)]
#[test]
fn off_extern_crate_self_alias_at_the_root() {{
    let _ = me::reader();
}}
"#
            ),
        ),
        (
            "elsewhere/renamed_file.rs",
            r#"pub fn read() -> Option<std::ffi::OsString> {
    crate::reader()
}
"#
            .to_string(),
        ),
        (
            "tests.rs",
            r#"use crate::Probe;

#[test]
fn off_extern_crate_self_alias() {
    let _ = me::reader();
}

#[test]
fn off_alias_path_to_the_real_types_impl() {
    let _ = crate::Alias::read_via_alias_impl();
}

#[test]
fn off_clone_impl_run_by_a_call_out_of_the_crate() {
    let _copies: Vec<crate::Copied> = std::iter::repeat_n(crate::Copied, 2).collect();
}

#[test]
fn off_a_module_found_by_its_path_attribute() {
    let _ = crate::renamed::read();
}

struct QuietGo;
impl crate::Go for QuietGo {
    fn go() -> u8 {
        0
    }
}

fn through_a_generic<Probe: crate::Go>() -> u8 {
    Probe::go()
}

#[test]
fn ok_a_generic_parameter_shadows_a_crate_type() {
    let _ = through_a_generic::<QuietGo>();
}

#[test]
fn off_the_crate_type_the_generic_shadows_elsewhere() {
    let _ = Probe::go();
}

#[test]
fn off_extern_crate_self_global_path() {
    let _ = ::me::reader();
}

#[test]
fn ok_explicit_import_shadows_a_glob() {
    use crate::*;
    use crate::quiet::reader;
    let _ = reader();
}

#[test]
fn off_explicit_import_over_a_quiet_glob() {
    use crate::quiet::*;
    use crate::reader;
    let _ = reader();
}

#[test]
fn off_impl_written_on_a_type_alias() {
    let _ = crate::Holder::read_via_alias_impl();
}

#[test]
fn off_blanket_impl_by_type() {
    use crate::Blanket;
    let _ = crate::Holder::blanket_read();
}

#[test]
fn off_blanket_impl_qualified() {
    let _ = <crate::Holder as crate::Blanket>::blanket_read();
}

#[test]
fn off_dyn_qualified_path() {
    let _ = <dyn crate::Dyn>::dyn_read(&crate::Holder);
}

#[test]
fn off_crate_trait_on_a_primitive() {
    let _ = <u32 as crate::ForPrimitives>::prim_read();
}

#[test]
fn off_parse_at_a_crate_type_in_a_generic_argument() {
    let _ = serde_json::from_str::<Option<crate::Plain>>("null");
}

#[test]
fn off_parse_into_a_let_whose_type_wraps_a_crate_type() {
    let _plain: Result<Vec<crate::Plain>, serde_json::Error> = serde_json::from_str("[]");
}

#[test]
fn off_parse_in_a_blocks_trailing_if_at_a_crate_type() {
    let _parsed: Result<Vec<crate::Plain>, serde_json::Error> = {
        if std::hint::black_box(true) {
            serde_json::from_str("[]")
        } else {
            serde_json::from_str("[{}]")
        }
    };
}

#[test]
fn ok_a_glob_cycle_resolves() {
    crate::m5::quiet_fn();
}
"#
            .to_string(),
        ),
    ]);
    assert!(
        report.offenders.is_empty(),
        "no test touches the environment itself"
    );
    let mut indirect = labels(
        "tests.rs",
        &[
            "off_extern_crate_self_alias",
            "off_extern_crate_self_global_path",
            "off_explicit_import_over_a_quiet_glob",
            "off_impl_written_on_a_type_alias",
            "off_blanket_impl_by_type",
            "off_blanket_impl_qualified",
            "off_dyn_qualified_path",
            "off_crate_trait_on_a_primitive",
            "off_parse_at_a_crate_type_in_a_generic_argument",
            "off_parse_into_a_let_whose_type_wraps_a_crate_type",
            "off_alias_path_to_the_real_types_impl",
            "off_parse_in_a_blocks_trailing_if_at_a_crate_type",
            "off_clone_impl_run_by_a_call_out_of_the_crate",
            "off_a_module_found_by_its_path_attribute",
            "off_the_crate_type_the_generic_shadows_elsewhere",
        ],
    );
    indirect.extend(labels(
        "lib.rs",
        &["off_extern_crate_self_alias_at_the_root"],
    ));
    indirect.sort();
    assert_eq!(
        report.indirect_offenders, indirect,
        "every path into the crate is followed"
    );
    // The one path that does not resolve is the generic parameter's, which
    // names no crate type whatever crate type shares its name.
    assert_eq!(
        (report.edges.path_unresolved, report.values.path_unresolved),
        (1, 0),
        "every path of this tree resolves but the generic one: {:?} {:?}",
        report.edges.by_reason,
        report.values.by_reason
    );
    assert_eq!(
        report
            .edges
            .by_reason
            .get("a path through a generic parameter"),
        Some(&1),
        "the generic parameter's path is unresolved for that reason: {:?}",
        report.edges.by_reason
    );
    println!("scope visits: {}", report.scope_visits);
    assert!(
        report.scope_visits < 2_000,
        "the glob cycle is visited once per scope and lookup, not once per route: {} visits",
        report.scope_visits
    );
}

/// Round 7, blocker 6 (mutant F21), and round 7's fourth audit (blocker 5,
/// recursion; the `Drop` that locks): taking the lock while a guard lives
/// would hang, `TEST_ENV_LOCK` being a `std::sync::Mutex`. Inside a guard's
/// range (in any region: a closure written there may run while it lives), a
/// second lock call, a lock taken by `Mutex::lock(&..)` or through `*&`, a
/// helper call, a call of a function that takes the lock, a call of the
/// function itself, and two functions calling each other are nesting sites; a
/// `Drop` impl or an operator impl that takes the lock, itself or through a
/// call, is a nesting hazard wherever a value of its type drops or the
/// operator stands. A guard is taken to hold the lock to the end of its block
/// whatever names it, so a lock taken after the guard moved to another
/// binding, or after `drop(guard)` in the same block (by design), is a
/// nesting site too. A helper call that gives a body its only guard is no
/// nesting site, and neither is a lock taken after an earlier guard's block
/// ended. The walk stops at the first function that takes the lock: a body
/// that takes it and then calls another that does is one nesting site, named
/// by the first.
#[test]
fn the_lock_discipline_refuses_a_second_acquisition_while_a_guard_lives() {
    let report = planted(&[
        (
            "lib.rs",
            format!(
                r#"#![allow(dead_code, unconditional_recursion)]
{PLANTED_ROOT}
pub fn fixture_guard() -> std::sync::MutexGuard<'static, ()> {{
    crate::TEST_ENV_LOCK.lock().unwrap()
}}

pub fn takes_the_lock_itself() {{
    let _g = crate::TEST_ENV_LOCK.lock().unwrap();
}}

pub fn locks_then_calls_a_locker() {{
    {{
        let _g = crate::TEST_ENV_LOCK.lock().unwrap();
    }}
    takes_the_lock_itself();
}}

pub struct Relocks;
impl Drop for Relocks {{
    fn drop(&mut self) {{
        let _g = crate::TEST_ENV_LOCK.lock();
    }}
}}

pub struct ReachesALocker;
impl Drop for ReachesALocker {{
    fn drop(&mut self) {{
        takes_the_lock_itself();
    }}
}}

pub struct Relocking;
impl std::ops::Deref for Relocking {{
    type Target = ();
    fn deref(&self) -> &() {{
        let _g = crate::TEST_ENV_LOCK.lock();
        &()
    }}
}}

#[cfg(test)]
mod tests;
"#
            ),
        ),
        (
            "tests.rs",
            r#"#[test]
fn off_lock_then_a_guard_returning_call() {
    let _a = crate::TEST_ENV_LOCK.lock().unwrap();
    let _b = crate::fixture_guard();
}

#[test]
fn off_lock_twice() {
    let _a = crate::TEST_ENV_LOCK.lock().unwrap();
    let _b = crate::TEST_ENV_LOCK.lock().unwrap();
}

#[test]
fn off_lock_through_its_type() {
    let _a = crate::TEST_ENV_LOCK.lock().unwrap();
    let _b = std::sync::Mutex::lock(&crate::TEST_ENV_LOCK);
}

#[test]
fn off_lock_through_a_dereference() {
    let _a = crate::TEST_ENV_LOCK.lock().unwrap();
    let _b = (*&crate::TEST_ENV_LOCK).lock();
}

#[test]
fn off_a_locking_call_inside_a_guards_block() {
    {
        let _a = crate::TEST_ENV_LOCK.lock().unwrap();
        crate::takes_the_lock_itself();
    }
}

#[test]
fn off_a_call_of_a_body_that_locks_twice() {
    let _a = crate::TEST_ENV_LOCK.lock().unwrap();
    crate::locks_then_calls_a_locker();
}

#[test]
fn off_a_closure_that_locks_under_a_guard() {
    let _a = crate::TEST_ENV_LOCK.lock().unwrap();
    let relock = || crate::takes_the_lock_itself();
    relock();
}

fn recurse(depth: u8) {
    let _g = crate::TEST_ENV_LOCK.lock().unwrap();
    if depth > 0 {
        recurse(depth - 1);
    }
}

fn ping(depth: u8) {
    let _g = crate::TEST_ENV_LOCK.lock().unwrap();
    if depth > 0 {
        pong(depth - 1);
    }
}

fn pong(depth: u8) {
    if depth > 0 {
        ping(depth - 1);
    }
}

#[test]
fn ok_a_guard_returning_call_alone() {
    let _b = crate::fixture_guard();
    let _ = crate::reader();
}

#[test]
fn ok_a_lock_after_an_earlier_guards_block() {
    {
        let _a = crate::TEST_ENV_LOCK.lock().unwrap();
    }
    crate::takes_the_lock_itself();
}

#[test]
fn off_a_lock_after_the_guard_is_dropped_by_name() {
    let a = crate::TEST_ENV_LOCK.lock().unwrap();
    drop(a);
    let _b = crate::TEST_ENV_LOCK.lock().unwrap();
}

#[test]
fn off_a_lock_after_the_guard_moved() {
    let result = crate::TEST_ENV_LOCK.lock();
    let kept = result.unwrap();
    let _again = crate::fixture_guard();
    drop(kept);
}

#[test]
fn ok_a_lock_after_the_guards_block_with_a_drop() {
    {
        let a = crate::TEST_ENV_LOCK.lock().unwrap();
        drop(a);
    }
    let _b = crate::TEST_ENV_LOCK.lock().unwrap();
}
"#
            .to_string(),
        ),
    ]);
    assert_eq!(
        report.nesting_sites,
        vec![
            (
                "tests.rs::off_a_call_of_a_body_that_locks_twice".to_string(),
                "lib.rs::locks_then_calls_a_locker".to_string(),
            ),
            (
                "tests.rs::off_a_closure_that_locks_under_a_guard".to_string(),
                "lib.rs::takes_the_lock_itself".to_string(),
            ),
            (
                "tests.rs::off_a_lock_after_the_guard_is_dropped_by_name".to_string(),
                "tests.rs::off_a_lock_after_the_guard_is_dropped_by_name".to_string(),
            ),
            (
                "tests.rs::off_a_lock_after_the_guard_moved".to_string(),
                "lib.rs::fixture_guard".to_string(),
            ),
            (
                "tests.rs::off_a_locking_call_inside_a_guards_block".to_string(),
                "lib.rs::takes_the_lock_itself".to_string(),
            ),
            (
                "tests.rs::off_lock_then_a_guard_returning_call".to_string(),
                "lib.rs::fixture_guard".to_string(),
            ),
            (
                "tests.rs::off_lock_through_a_dereference".to_string(),
                "tests.rs::off_lock_through_a_dereference".to_string(),
            ),
            (
                "tests.rs::off_lock_through_its_type".to_string(),
                "tests.rs::off_lock_through_its_type".to_string(),
            ),
            (
                "tests.rs::off_lock_twice".to_string(),
                "tests.rs::off_lock_twice".to_string(),
            ),
            ("tests.rs::ping".to_string(), "tests.rs::ping".to_string(),),
            (
                "tests.rs::recurse".to_string(),
                "tests.rs::recurse".to_string(),
            ),
        ],
        "a second acquisition while a guard lives is a nesting site, through a closure, a \
         call of the function itself or of a function that calls it back"
    );
    assert_eq!(
        report.nesting_hazards,
        [
            (
                "lib.rs::ReachesALocker::drop".to_string(),
                vec!["lib.rs::takes_the_lock_itself".to_string()],
            ),
            ("lib.rs::Relocking::deref".to_string(), Vec::new()),
            ("lib.rs::Relocks::drop".to_string(), Vec::new()),
        ]
        .into_iter()
        .collect::<BTreeMap<_, _>>(),
        "a Drop or operator impl that takes the lock, itself or through a call, is a nesting \
         hazard"
    );
    assert!(
        report.indirect_offenders.is_empty(),
        "the guard-returning call covers the read: {:?}",
        report.indirect_offenders
    );
    assert_eq!(
        report.refused_acquisitions,
        labels(
            "tests.rs",
            &[
                "off_lock_through_a_dereference",
                "off_lock_through_its_type"
            ]
        ),
        "a lock taken through its type's path or a dereference is an acquisition no shape \
         keeps"
    );
}

/// Round 8 review, note (c), decision D-i8-45: a guard the gate cannot bound
/// to a block is taken to hold the lock at every later site of its body and
/// throughout a loop, closure or macro around it, and a body that can hand a
/// held lock on makes its every caller's call such a hold. Before D-i8-45 the
/// nesting side started only at a shape-1 guard, so each `off_` case below
/// (a guard in a struct literal, a tuple, `Some`, a `Box`, a holder a helper
/// returns, directly or two calls away, a guard moved to an outer binding,
/// one stored through a parameter, one pushed in a loop, a closure that locks
/// called twice, a crate macro that locks beside a guard) reported nothing
/// and hangs at run time. The `ok_` controls release the first guard before
/// the second lock: a holder kept in an inner block, a made holder dropped at
/// once, a fixture whose guard is confined or released by `drop`, a holder
/// dropped by name in an inner block, and the two arms of one `if`.
#[test]
fn the_lock_discipline_takes_every_unbounded_hold_to_nest() {
    let report = planted(&[
        (
            "lib.rs",
            format!(
                r#"#![allow(dead_code, unused)]
{PLANTED_ROOT}
pub struct Holder {{
    _guard: std::sync::MutexGuard<'static, ()>,
}}

pub fn make_holder() -> Holder {{
    Holder {{
        _guard: crate::TEST_ENV_LOCK.lock().unwrap(),
    }}
}}

pub fn make_through() -> Holder {{
    make_holder()
}}

pub fn store_into(slot: &mut Option<std::sync::MutexGuard<'static, ()>>) {{
    *slot = Some(crate::TEST_ENV_LOCK.lock().unwrap());
}}

pub fn takes_the_lock_itself() {{
    let _g = crate::TEST_ENV_LOCK.lock().unwrap();
}}

pub fn confined_fixture() {{
    let _g = crate::TEST_ENV_LOCK.lock().unwrap();
}}

pub fn released_by_drop() {{
    let g = crate::TEST_ENV_LOCK.lock().unwrap();
    drop(g);
}}

#[cfg(test)]
mod tests;
"#
            ),
        ),
        (
            "tests.rs",
            r#"#![allow(unused)]
macro_rules! relock {
    () => {
        let _x = crate::TEST_ENV_LOCK.lock();
    };
}

#[test]
fn off_a_guard_in_a_struct_literal() {
    let _h = crate::Holder {
        _guard: crate::TEST_ENV_LOCK.lock().unwrap(),
    };
    let _b = crate::TEST_ENV_LOCK.lock().unwrap();
}

#[test]
fn off_a_guard_in_a_tuple() {
    let _t = (crate::TEST_ENV_LOCK.lock().unwrap(), 0u8);
    let _b = crate::TEST_ENV_LOCK.lock().unwrap();
}

#[test]
fn off_a_guard_in_some() {
    let _o = Some(crate::TEST_ENV_LOCK.lock().unwrap());
    let _b = crate::TEST_ENV_LOCK.lock().unwrap();
}

#[test]
fn off_a_boxed_guard() {
    let _x = Box::new(crate::TEST_ENV_LOCK.lock().unwrap());
    let _b = crate::TEST_ENV_LOCK.lock().unwrap();
}

#[test]
fn off_a_returned_holder() {
    let _h = crate::make_holder();
    let _b = crate::TEST_ENV_LOCK.lock().unwrap();
}

#[test]
fn off_a_holder_two_calls_away() {
    let _h = crate::make_through();
    crate::takes_the_lock_itself();
}

#[test]
fn off_a_guard_moved_to_an_outer_binding() {
    let outer;
    {
        let g = crate::TEST_ENV_LOCK.lock().unwrap();
        outer = g;
    }
    let _b = crate::TEST_ENV_LOCK.lock().unwrap();
    let _ = &outer;
}

#[test]
fn off_a_guard_stored_through_a_parameter() {
    let mut slot = None;
    crate::store_into(&mut slot);
    let _b = crate::TEST_ENV_LOCK.lock().unwrap();
}

#[test]
fn off_a_guard_pushed_in_a_loop() {
    let mut held = Vec::new();
    for _ in 0..2 {
        held.push(crate::TEST_ENV_LOCK.lock().unwrap());
    }
}

#[test]
fn off_a_closure_that_locks_called_twice() {
    let take = || crate::TEST_ENV_LOCK.lock().unwrap();
    let _a = take();
    let _b = take();
}

#[test]
fn off_a_macro_that_locks_beside_a_guard() {
    let _a = crate::TEST_ENV_LOCK.lock().unwrap();
    relock!();
}

#[test]
fn ok_a_holder_in_an_inner_block() {
    {
        let _h = crate::make_holder();
    }
    let _b = crate::TEST_ENV_LOCK.lock().unwrap();
}

#[test]
fn ok_a_made_holder_dropped_at_once() {
    drop(crate::make_holder());
    let _b = crate::TEST_ENV_LOCK.lock().unwrap();
}

#[test]
fn ok_a_confined_fixture_first() {
    crate::confined_fixture();
    let _b = crate::TEST_ENV_LOCK.lock().unwrap();
}

#[test]
fn ok_a_fixture_that_drops_its_guard_first() {
    crate::released_by_drop();
    let _b = crate::TEST_ENV_LOCK.lock().unwrap();
}

#[test]
fn ok_a_holder_dropped_by_name_in_an_inner_block() {
    {
        let h = crate::make_holder();
        drop(h);
    }
    let _b = crate::TEST_ENV_LOCK.lock().unwrap();
}

#[test]
fn ok_two_arms_of_one_if() {
    let _g = if std::hint::black_box(true) {
        crate::TEST_ENV_LOCK.lock().unwrap()
    } else {
        crate::TEST_ENV_LOCK.lock().unwrap()
    };
}
"#
            .to_string(),
        ),
    ]);
    let pairs = |rows: &[(&str, &str)]| -> Vec<(String, String)> {
        rows.iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    };
    assert_eq!(
        report.nesting_sites,
        pairs(&[
            ("tests.rs::off_a_boxed_guard", "tests.rs::off_a_boxed_guard"),
            (
                "tests.rs::off_a_closure_that_locks_called_twice",
                "tests.rs::off_a_closure_that_locks_called_twice"
            ),
            (
                "tests.rs::off_a_guard_in_a_struct_literal",
                "tests.rs::off_a_guard_in_a_struct_literal"
            ),
            (
                "tests.rs::off_a_guard_in_a_tuple",
                "tests.rs::off_a_guard_in_a_tuple"
            ),
            (
                "tests.rs::off_a_guard_in_some",
                "tests.rs::off_a_guard_in_some"
            ),
            (
                "tests.rs::off_a_guard_moved_to_an_outer_binding",
                "tests.rs::off_a_guard_moved_to_an_outer_binding"
            ),
            (
                "tests.rs::off_a_guard_pushed_in_a_loop",
                "tests.rs::off_a_guard_pushed_in_a_loop"
            ),
            (
                "tests.rs::off_a_guard_stored_through_a_parameter",
                "tests.rs::off_a_guard_stored_through_a_parameter"
            ),
            (
                "tests.rs::off_a_holder_two_calls_away",
                "lib.rs::takes_the_lock_itself"
            ),
            (
                "tests.rs::off_a_macro_that_locks_beside_a_guard",
                "tests.rs::off_a_macro_that_locks_beside_a_guard"
            ),
            (
                "tests.rs::off_a_returned_holder",
                "tests.rs::off_a_returned_holder"
            ),
        ]),
        "every hold the gate cannot bound to a block nests with a later lock; the ok_ \
         controls release the first guard first"
    );
    // A body hands the lock on when it has a hold no confined `let` or
    // `drop` bounds, or a lock in a macro's tokens; a test that keeps a
    // returned holder in a confined `let` does not, nor does a fixture whose
    // guard is confined or released by `drop`.
    let mut handing = labels("lib.rs", &["make_holder", "make_through", "store_into"]);
    handing.extend(labels(
        "tests.rs",
        &[
            "off_a_boxed_guard",
            "off_a_closure_that_locks_called_twice",
            "off_a_guard_in_a_struct_literal",
            "off_a_guard_in_a_tuple",
            "off_a_guard_in_some",
            "off_a_guard_moved_to_an_outer_binding",
            "off_a_guard_pushed_in_a_loop",
            "off_a_guard_stored_through_a_parameter",
            "off_a_macro_that_locks_beside_a_guard",
            "ok_two_arms_of_one_if",
        ],
    ));
    handing.sort();
    assert_eq!(
        report.lock_handing_functions, handing,
        "the bodies that can hand a held lock to their caller"
    );
    // One unbounded hold in each handing body but the macro's (its lock is
    // in tokens), and two in `ok_two_arms_of_one_if`.
    assert_eq!(report.unconfined_holds, 13);
    assert_eq!(
        report.token_lock_calls, 1,
        "the macro's lock is in its tokens"
    );
    assert!(report.nesting_hazards.is_empty());
}

/// Round 7, the note on operators and implicit drops: an operator (`*x`,
/// `a == b`, `a + b`) or a value going out of scope runs a crate impl with no
/// edge from where it runs, so every impl of an operator trait or of `Drop`
/// is checked on its own: one that reads the environment without the lock,
/// itself or over the edges, is an operator or drop hazard, wherever it is
/// used. A lock-holding type's `drop` and an impl that reads nothing are not.
#[test]
fn the_lock_discipline_checks_operator_and_drop_impls_on_their_own() {
    let report = planted(&[
        (
            "lib.rs",
            format!(
                r#"#![allow(dead_code)]
{PLANTED_ROOT}
pub struct Wrap;
impl std::ops::Deref for Wrap {{
    type Target = str;
    fn deref(&self) -> &str {{
        let _ = crate::reader();
        ""
    }}
}}
impl PartialEq for Wrap {{
    fn eq(&self, _other: &Self) -> bool {{
        crate::reader().is_some()
    }}
}}
impl std::ops::Add for Wrap {{
    type Output = Wrap;
    fn add(self, _other: Wrap) -> Wrap {{
        let _ = crate::reader();
        Wrap
    }}
}}
impl std::ops::Neg for Wrap {{
    type Output = Wrap;
    fn neg(self) -> Wrap {{
        Wrap
    }}
}}

pub struct ProdRestore;
impl Drop for ProdRestore {{
    fn drop(&mut self) {{
        let _ = std::env::var_os("W4");
    }}
}}

pub struct Held {{
    _guard: std::sync::MutexGuard<'static, ()>,
}}
impl Held {{
    pub fn new() -> Self {{
        Held {{ _guard: crate::TEST_ENV_LOCK.lock().unwrap() }}
    }}
}}
impl Drop for Held {{
    fn drop(&mut self) {{
        unsafe {{ std::env::remove_var("W4") }};
    }}
}}

#[cfg(test)]
mod tests;
"#
            ),
        ),
        (
            "tests.rs",
            r#"#[test]
fn uses_every_operator() {
    let w = crate::Wrap;
    let _s: &str = &*w;
    let _ = crate::Wrap == crate::Wrap;
    let _ = crate::Wrap + crate::Wrap;
    let _ = -crate::Wrap;
    let _r = crate::ProdRestore;
    let _h = crate::Held::new();
}
"#
            .to_string(),
        ),
    ]);
    assert_eq!(
        report
            .operator_and_drop_hazards
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        vec![
            "lib.rs::ProdRestore::drop".to_string(),
            "lib.rs::Wrap::add".to_string(),
            "lib.rs::Wrap::deref".to_string(),
            "lib.rs::Wrap::eq".to_string(),
        ],
        "every operator or drop impl that reads without the lock is a hazard"
    );
    assert_eq!(
        report.operator_and_drop_hazards.get("lib.rs::Wrap::add"),
        Some(&vec!["lib.rs::reader".to_string()]),
        "a hazard names its chain"
    );
    assert!(
        report.offenders.is_empty() && report.indirect_offenders.is_empty(),
        "the operators themselves are not followed: {:?} {:?}",
        report.offenders,
        report.indirect_offenders
    );
}

/// Round 7, the clap finding: a field attribute `#[arg(.., env = "..")]` or
/// `#[clap(.., env = "..")]` (or a bare `env`) makes clap read that variable
/// while it builds the command,
/// so `parse`, `try_parse_from`, `command` and `augment_args` of the type
/// read the environment, and so does every parser that reaches the type
/// through a subcommand or a flattened field (a type alias of the child
/// followed to its type), and a parse through the trait's own path with no
/// type written (`Parser::parse()`, every derived parse its candidate). A
/// child with no clap derive is followed to the augment method its
/// hand-written impl writes, and one on the trait's defaults, or an alias of
/// a type out of the crate, is reported unresolved rather than dropped; a
/// child out of the crate is counted as a reference out of the crate. A child
/// that derives `Args` and writes `Subcommand`'s method is followed to both,
/// as the parse does not tell which one its parent calls. A parser with no `env` field reads
/// nothing, and a parse under the lock holds it. With a `#[macro_use] extern
/// crate` at the root no macro runs in place, so a read in `assert!` under
/// the lock is reported.
#[test]
fn the_lock_discipline_reads_clap_env_attributes() {
    let report = planted(&[
        (
            "lib.rs",
            format!(
                r#"#![allow(dead_code)]
{PLANTED_ROOT}
#[macro_use]
extern crate serde_json;

#[derive(clap::Parser)]
pub struct Cli {{
    #[arg(long, env = "W4")]
    pub config: Option<String>,
}}

#[derive(clap::Subcommand)]
pub enum Commands {{
    Start(StartArgs),
    Stop,
}}

#[derive(clap::Args)]
pub struct StartArgs {{
    #[arg(long, env)]
    pub detach: Option<String>,
}}

#[derive(clap::Parser)]
pub struct WithSubcommand {{
    #[command(subcommand)]
    pub command: Option<Commands>,
}}

#[derive(clap::Parser)]
pub struct Flattened {{
    #[command(flatten)]
    pub start: StartArgs,
}}

#[derive(clap::Parser)]
pub struct Quiet {{
    #[arg(long)]
    pub level: Option<String>,
}}

#[derive(clap::Parser)]
pub struct OldStyle {{
    #[clap(long, env = "W4")]
    pub level: Option<String>,
}}

pub type StartAlias = StartArgs;

#[derive(clap::Parser)]
pub struct ThroughAlias {{
    #[command(flatten)]
    pub start: StartAlias,
}}

pub struct ManualReads;
impl clap::Args for ManualReads {{
    fn augment_args(command: clap::Command) -> clap::Command {{
        let _ = std::env::var_os("W4");
        command
    }}
}}

#[derive(clap::Parser)]
pub struct WithManualReads {{
    #[command(flatten)]
    pub manual: ManualReads,
}}

pub struct ManualDefaults;
impl clap::Args for ManualDefaults {{}}

#[derive(clap::Parser)]
pub struct WithManualDefaults {{
    #[command(flatten)]
    pub manual: ManualDefaults,
}}

pub type ExternalAlias = clap::Command;

#[derive(clap::Args)]
pub struct BothWays {{
    #[arg(long)]
    pub level: Option<String>,
}}
impl clap::Subcommand for BothWays {{
    fn augment_subcommands(command: clap::Command) -> clap::Command {{
        let _ = std::env::var_os("W4");
        command
    }}
}}

#[derive(clap::Parser)]
pub struct WithBothWays {{
    #[command(flatten)]
    pub both: BothWays,
}}

#[derive(clap::Parser)]
pub struct WithExternalAlias {{
    #[command(flatten)]
    pub external: ExternalAlias,
}}

#[cfg(test)]
mod tests;
"#
            ),
        ),
        (
            "tests.rs",
            r#"use clap::Parser;

#[test]
fn off_try_parse_from_reads_the_env_attribute() {
    let _ = crate::Cli::try_parse_from(["sqryd"]);
}

#[test]
fn off_command_reads_the_env_attribute() {
    let _ = <crate::Cli as clap::CommandFactory>::command();
}

#[test]
fn off_a_subcommands_env_attribute() {
    let _ = crate::WithSubcommand::try_parse_from(["sqryd"]);
}

#[test]
fn off_a_flattened_env_attribute() {
    let _ = crate::Flattened::try_parse_from(["sqryd"]);
}

#[test]
fn ok_a_parser_without_env_attributes() {
    let _ = crate::Quiet::try_parse_from(["sqryd"]);
}

#[test]
fn ok_a_parse_under_the_lock() {
    let _env = crate::TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _ = crate::Cli::try_parse_from(["sqryd"]);
}

fn take(_cli: crate::Cli) {}

#[test]
fn off_a_parse_through_the_traits_path() {
    take(Parser::parse());
}

#[test]
fn off_a_clap_attribute_with_env() {
    let _ = crate::OldStyle::try_parse_from(["sqryd"]);
}

#[test]
fn off_a_flattened_alias() {
    let _ = crate::ThroughAlias::try_parse_from(["sqryd"]);
}

#[test]
fn off_a_hand_written_child() {
    let _ = crate::WithManualReads::try_parse_from(["sqryd"]);
}

#[test]
fn unresolved_a_child_on_the_traits_defaults() {
    let _ = crate::WithManualDefaults::try_parse_from(["sqryd"]);
}

#[test]
fn off_a_child_with_both_augment_methods() {
    let _ = crate::WithBothWays::try_parse_from(["sqryd"]);
}

#[test]
fn unresolved_an_alias_of_an_external_child() {
    let _ = crate::WithExternalAlias::try_parse_from(["sqryd"]);
}

#[test]
fn off_an_assert_when_a_crate_is_macro_use() {
    let _env = crate::TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    assert!(crate::reader().is_none() || std::hint::black_box(true));
}
"#
            .to_string(),
        ),
    ]);
    assert_eq!(
        report.indirect_offenders,
        labels(
            "tests.rs",
            &[
                "off_a_flattened_env_attribute",
                "off_a_subcommands_env_attribute",
                "off_command_reads_the_env_attribute",
                "off_try_parse_from_reads_the_env_attribute",
                "off_a_parse_through_the_traits_path",
                "off_a_flattened_alias",
                "off_a_hand_written_child",
                "off_a_child_with_both_augment_methods",
                "off_an_assert_when_a_crate_is_macro_use",
                "off_a_clap_attribute_with_env",
            ]
        ),
        "a parse that builds a command with an env attribute reads the environment, \
         through the trait's path with no type written too (every derived parse)"
    );
    assert_eq!(
        report
            .indirect_chains
            .get("tests.rs::off_a_subcommands_env_attribute")
            .cloned()
            .unwrap_or_default(),
        vec![
            "lib.rs::WithSubcommand::try_parse_from",
            "lib.rs::Commands::augment_subcommands",
            "lib.rs::StartArgs::augment_args",
        ],
        "the subcommand's arguments are reached through augment_subcommands"
    );
    assert_eq!(
        report.clap_env_fields,
        vec![
            "lib.rs::Cli::config".to_string(),
            "lib.rs::OldStyle::level".to_string(),
            "lib.rs::StartArgs::detach".to_string(),
        ],
        "every clap env attribute is counted"
    );
    assert_eq!(
        report
            .indirect_chains
            .get("tests.rs::off_a_hand_written_child")
            .cloned()
            .unwrap_or_default(),
        vec![
            "lib.rs::WithManualReads::try_parse_from",
            "lib.rs::ManualReads::augment_args",
        ],
        "a child with no clap derive is followed to the augment method its hand-written \
         impl writes"
    );
    assert!(
        report.macro_use_extern_crate,
        "a #[macro_use] extern crate at the root is seen, so no macro runs in place"
    );
    let unresolved: BTreeMap<&String, &Vec<String>> = report
        .not_followed
        .iter()
        .filter(|(heading, _)| heading.contains("a clap child"))
        .collect();
    let by_reason = |reason: &str, parent: &str, child: &str| {
        unresolved
            .iter()
            .filter(|(heading, _)| heading.contains(reason))
            .flat_map(|(_, sites)| sites.iter())
            .all(|site| site.starts_with(&format!("lib.rs::{parent}::")) && site.contains(child))
            && unresolved
                .iter()
                .any(|(heading, sites)| heading.contains(reason) && !sites.is_empty())
    };
    assert!(
        unresolved.len() == 2
            && by_reason(
                "writes no augment method",
                "WithManualDefaults",
                ": ManualDefaults"
            )
            && by_reason(
                "whose type alias names no crate type",
                "WithExternalAlias",
                ": ExternalAlias"
            ),
        "a child on the trait's defaults, and an alias of a type out of the crate, are each \
         reported unresolved with their own reason, by every derived method of their parent, \
         and no other child is: {unresolved:#?}"
    );

    // A child out of the crate is a reference out of the crate in every
    // derived method of its parent, never dropped: the same parser with the
    // field not marked as a child has one such reference fewer per method.
    let parser = |child: &str| {
        planted(&[(
            "lib.rs",
            format!(
                "{PLANTED_ROOT}\n#[derive(clap::Parser)]\npub struct WithExternalChild {{\n    \
                 {child}\n    pub command: clap::Command,\n}}\n"
            ),
        )])
    };
    let (child, field) = (parser("#[command(flatten)]"), parser(""));
    assert!(
        child.derived_methods > 0 && child.derived_methods == field.derived_methods,
        "instrument: both parsers derive the same methods ({} and {})",
        child.derived_methods,
        field.derived_methods
    );
    assert_eq!(
        child.edges.path_external - field.edges.path_external,
        child.derived_methods,
        "every derived method holds the child out of the crate as a reference out of the crate"
    );
}
