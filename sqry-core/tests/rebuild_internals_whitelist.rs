//! [A2 §H, Gate 0c] CI-enforced whitelist for the `rebuild-internals`
//! feature on `sqry-core`.
//!
//! Per the sqryd daemon plan
//! (`docs/superpowers/plans/2026-03-19-sqryd-daemon.md` §H "Placement
//! and feature gate for `clone_for_rebuild`"):
//!
//! > `sqry-daemon/Cargo.toml` enables the feature; with cargo resolver
//! > v2, features are not unified across workspace members that do not
//! > enable them, so `sqry-cli` (which does not enable it) cannot
//! > resolve `clone_for_rebuild` or `RebuildGraph`. Cargo does NOT
//! > *reserve* the feature to `sqry-daemon` — that is a CI-policy
//! > check: a CI step greps every `Cargo.toml` for `rebuild-internals`
//! > and only `sqry-daemon/Cargo.toml` is whitelisted.
//!
//! This test is that CI step. It:
//!
//! 1. Parses the workspace manifest at `../Cargo.toml` to enumerate
//!    every workspace-member crate directory.
//! 2. Parses every member's `Cargo.toml` and collects every mention
//!    of `rebuild-internals` — in `[features]` keys, in
//!    `[dependencies.sqry-core]` / `[dev-dependencies.sqry-core]`
//!    tables (the `features = [...]` array), and in the inline
//!    `features = [...]` of an inline-table dependency spec.
//! 3. Asserts that every mention lives in a crate whose directory
//!    name appears in [`WHITELIST`].
//!
//! The whitelist is intentionally small: only `sqry-core` (where the
//! feature is *defined*; defining a feature is not the same as
//! *enabling* it in a dep-spec) and `sqry-daemon` (once it lands in
//! Task 5). Every other workspace crate must route through the public
//! API of `sqry-core` without the rebuild surface.
//!
//! # Running
//!
//! ```sh
//! cargo test -p sqry-core --test rebuild_internals_whitelist
//! ```
//!
//! The test does not enable `rebuild-internals` itself. It is a pure
//! text-level audit of the workspace manifests.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// Crate directories (relative to the workspace root) allowed to
/// mention `rebuild-internals` in any capacity:
/// - as a feature definition (currently: only `sqry-core`),
/// - as an enabling dep-spec attribute (currently: `sqry-daemon`,
///   which lands in Task 5 of the sqryd plan; the test is forward-
///   compatible — it passes whether or not the directory exists yet).
///
/// Adding a new crate to this list requires explicit reviewer sign-off
/// per plan §H "a code-owner rule on `sqry-core/Cargo.toml` requires
/// review before the feature definition itself can change".
const WHITELIST: &[&str] = &[
    "sqry-core",   // defines the feature
    "sqry-daemon", // Task 5 — the only crate permitted to enable it
];

/// Workspace root = the directory two levels up from the
/// `sqry-core/tests/` test file (which at compile time is this module's
/// `CARGO_MANIFEST_DIR`).
fn workspace_root() -> PathBuf {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .expect("sqry-core has a parent directory (workspace root)")
        .to_path_buf()
}

/// Parse the workspace manifest and return every `members = [..]` entry
/// as a workspace-root-relative path, with `[workspace.exclude]` paths
/// filtered out. Excluded crates are not part of the workspace build
/// and therefore cannot opt into a workspace feature — auditing them
/// here would be a false positive.
///
/// Cargo's own glob semantics for `workspace.members` support `*`
/// patterns (e.g. `crates/*`); we do **not** expand globs because this
/// repository's root `Cargo.toml` enumerates members literally. If a
/// future manifest change introduces a glob in `members`, the whitelist
/// audit will see the literal string and the assertion below will
/// panic with a clear message pointing to this function — surfacing the
/// drift for explicit reviewer attention rather than silently ignoring
/// the pattern.
fn workspace_members(root: &Path) -> Vec<String> {
    let manifest_path = root.join("Cargo.toml");
    let contents = fs::read_to_string(&manifest_path)
        .unwrap_or_else(|e| panic!("read workspace manifest {manifest_path:?}: {e}"));
    let parsed: toml::Value = toml::from_str(&contents)
        .unwrap_or_else(|e| panic!("parse workspace manifest {manifest_path:?}: {e}"));
    extract_workspace_members(&parsed)
}

/// Extract the resolved workspace-member list from an already-parsed
/// `Cargo.toml`. Reads `[workspace.members]`, rejects glob patterns in
/// either `members` or `exclude`, and filters out `[workspace.exclude]`
/// entries. Separated from `workspace_members()` so unit tests can
/// drive it with synthetic manifests (including an `exclude` list)
/// without touching the live root manifest.
fn extract_workspace_members(parsed: &toml::Value) -> Vec<String> {
    let workspace = parsed
        .get("workspace")
        .expect("[workspace] table in root Cargo.toml");
    let members = workspace
        .get("members")
        .expect("workspace.members array")
        .as_array()
        .expect("workspace.members is an array");
    let member_strings: Vec<String> = members
        .iter()
        .filter_map(|v| v.as_str().map(String::from))
        .collect();
    for member in &member_strings {
        assert!(
            !member.contains('*') && !member.contains('?') && !member.contains('['),
            "rebuild_internals_whitelist: workspace.members entry {member:?} appears to \
             be a glob pattern. This audit relies on literal member paths; extend \
             workspace_members() in sqry-core/tests/rebuild_internals_whitelist.rs to \
             expand globs before shipping a glob in the root Cargo.toml."
        );
    }

    // Filter out anything listed in [workspace.exclude]. An exclude
    // entry is a literal path (or glob, which we reject with the same
    // check as above). Entries missing from `members` but present in
    // `exclude` are a no-op — Cargo already skips them; we match that
    // semantics here.
    let exclude_strings: Vec<String> = workspace
        .get("exclude")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    for excluded in &exclude_strings {
        assert!(
            !excluded.contains('*') && !excluded.contains('?') && !excluded.contains('['),
            "rebuild_internals_whitelist: workspace.exclude entry {excluded:?} appears to \
             be a glob pattern. Extend workspace_members() to expand globs before \
             shipping a glob in the root Cargo.toml."
        );
    }
    let excluded_set: BTreeSet<&str> = exclude_strings.iter().map(String::as_str).collect();

    member_strings
        .into_iter()
        .filter(|m| !excluded_set.contains(m.as_str()))
        .collect()
}

/// Scan a single `Cargo.toml` file for references to `rebuild-internals`.
///
/// Returns a list of `(section, detail)` tuples describing each
/// occurrence. Any occurrence counts — defining the feature in
/// `[features]`, enabling it on a dep in `features = [...]`, or even
/// referencing the string in a comment — because the whitelist is
/// about the *textual mention* just as much as the semantic enable.
/// A stray mention in a crate that is not on the whitelist is either
/// a mistake (the feature is being enabled somewhere unexpected) or
/// documentation drift (the feature is mentioned in a comment in a
/// crate where it should not even be referenced); both are findings
/// the whitelist exists to surface.
fn find_rebuild_internals_mentions(manifest_path: &Path) -> Vec<String> {
    let contents = match fs::read_to_string(manifest_path) {
        Ok(s) => s,
        Err(_) => return Vec::new(), // Missing manifest is a distinct problem; ignore here.
    };
    let mut findings = Vec::new();
    for (line_no, line) in contents.lines().enumerate() {
        if line.contains("rebuild-internals") || line.contains("rebuild_internals") {
            // Describe the finding with 1-based line numbers so the
            // output matches an IDE's "jump to line".
            findings.push(format!(
                "  {}:{}: {}",
                manifest_path.display(),
                line_no + 1,
                line.trim_end()
            ));
        }
    }
    findings
}

#[test]
fn rebuild_internals_mentions_are_whitelisted() {
    let root = workspace_root();
    let members = workspace_members(&root);
    assert!(
        !members.is_empty(),
        "expected a non-empty workspace members list"
    );

    let whitelist: BTreeSet<&str> = WHITELIST.iter().copied().collect();
    let mut offenders: Vec<String> = Vec::new();

    for member in &members {
        let manifest = root.join(member).join("Cargo.toml");
        let mentions = find_rebuild_internals_mentions(&manifest);
        if mentions.is_empty() {
            continue;
        }
        // `member` may be a nested path like `crates/tree-sitter-vue-sqry`;
        // match against the last path component — the crate directory
        // name — which is what the whitelist enumerates.
        let crate_dir_name = Path::new(member)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(member);
        if !whitelist.contains(crate_dir_name) {
            offenders.push(format!(
                "crate `{crate_dir_name}` mentions `rebuild-internals` \
                 but is not whitelisted (mentions below):\n{}",
                mentions.join("\n")
            ));
        }
    }

    assert!(
        offenders.is_empty(),
        "rebuild-internals feature whitelist violated by {} crate(s):\n\n{}\n\n\
         Add the crate to `WHITELIST` in \
         `sqry-core/tests/rebuild_internals_whitelist.rs` only with \
         explicit reviewer sign-off per plan §H code-owner rule.",
        offenders.len(),
        offenders.join("\n\n")
    );
}

#[test]
fn sqry_core_still_defines_the_feature() {
    // If someone removes the feature definition in `sqry-core/Cargo.toml`
    // the whitelist test above still passes vacuously. Guard the
    // feature's existence explicitly: the whole point of Gate 0c is
    // that `sqry-core` *must* expose the feature for `sqry-daemon` to
    // enable it.
    let manifest = workspace_root().join("sqry-core").join("Cargo.toml");
    let contents = fs::read_to_string(&manifest).expect("read sqry-core manifest");
    assert!(
        contents.contains("rebuild-internals = []"),
        "sqry-core/Cargo.toml must define `rebuild-internals = []` in \
         its [features] table. Found neither the feature definition \
         nor any variant of it. See plan §H 'Placement and feature \
         gate for clone_for_rebuild'."
    );
}

#[test]
fn extract_workspace_members_honours_exclude_list() {
    // Synthetic manifest: two members, one of them excluded.
    let manifest = r#"
[workspace]
members = ["sqry-core", "sqry-extra"]
exclude = ["sqry-extra"]
"#;
    let parsed: toml::Value = toml::from_str(manifest).expect("parse synthetic manifest");
    let resolved = extract_workspace_members(&parsed);
    assert_eq!(resolved, vec!["sqry-core".to_string()]);
}

#[test]
fn extract_workspace_members_no_exclude_returns_members_unchanged() {
    let manifest = r#"
[workspace]
members = ["sqry-core", "sqry-cli"]
"#;
    let parsed: toml::Value = toml::from_str(manifest).expect("parse synthetic manifest");
    let resolved = extract_workspace_members(&parsed);
    assert_eq!(
        resolved,
        vec!["sqry-core".to_string(), "sqry-cli".to_string()]
    );
}

#[test]
#[should_panic(expected = "glob pattern")]
fn extract_workspace_members_rejects_glob_in_members() {
    let manifest = r#"
[workspace]
members = ["crates/*"]
"#;
    let parsed: toml::Value = toml::from_str(manifest).expect("parse synthetic manifest");
    let _ = extract_workspace_members(&parsed);
}

#[test]
#[should_panic(expected = "glob pattern")]
fn extract_workspace_members_rejects_glob_in_exclude() {
    let manifest = r#"
[workspace]
members = ["sqry-core"]
exclude = ["crates/*"]
"#;
    let parsed: toml::Value = toml::from_str(manifest).expect("parse synthetic manifest");
    let _ = extract_workspace_members(&parsed);
}

#[test]
fn whitelist_is_non_empty_and_sorted_has_sqry_core_first() {
    // Sanity: `sqry-core` must always be the first whitelist entry
    // because it *defines* the feature. Reordering is fine, but
    // removing `sqry-core` is a latent bug in the whitelist.
    //
    // The length check is an assertion that would only become
    // inoperative if someone edited the WHITELIST const below to be
    // empty; clippy's `const_is_empty` lint helpfully reports that
    // `!WHITELIST.is_empty()` is trivially true today, which means
    // our existing content is correct — we still want the runtime
    // check as a regression guard against accidental edits.
    #[allow(clippy::const_is_empty)]
    {
        assert!(
            !WHITELIST.is_empty(),
            "WHITELIST is empty — Gate 0c is inoperative"
        );
    }
    assert_eq!(
        WHITELIST[0], "sqry-core",
        "first WHITELIST entry must be `sqry-core` (the feature-defining crate)"
    );
    // `sqry-daemon` must be present even before the crate lands
    // physically; this locks in the intent.
    assert!(
        WHITELIST.contains(&"sqry-daemon"),
        "WHITELIST must include `sqry-daemon` — the only crate permitted \
         to enable `rebuild-internals`"
    );
}
