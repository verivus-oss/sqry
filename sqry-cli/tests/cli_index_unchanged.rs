//! T3 Cluster H2 — pin the "no new `sqry index` flags" contract.
//!
//! Per 01_SPEC §4.4 and CLI_INTEGRATION.md §2.4, the T3 tier ships
//! end-to-end Go-error-chain + context-propagation + build-tag stamping
//! WITHOUT adding any new `sqry index` flags. Every gated behaviour is
//! emitted unconditionally during the build (build_constraints,
//! ContextPropagationQuery is a derived query, Wraps emission is part
//! of the Go plugin's standard relations pass).
//!
//! This test pins the contract against accidental regression: if a
//! future commit adds `--enable-go-error-chains` / `--enable-build-tags`
//! / similar, this test fails — forcing the contributor to either
//! revert the flag or update the spec.

mod common;
use common::sqry_bin;

use assert_cmd::Command;

/// Run `sqry index --help` and assert NONE of the would-be regression
/// flags appear in the help text. The list is the union of:
///
/// - 01_SPEC §4.4's "no flags" list (T3.6 + T3.7 + T3.8).
/// - Plausible variants a contributor might invent if they retrofitted
///   gating later (`--no-error-chains`, `--enable-context-propagation`,
///   `--with-build-tags`, …).
#[test]
fn cli_index_no_new_t3_flags() {
    let assert = Command::new(sqry_bin())
        .arg("index")
        .arg("--help")
        .env("NO_COLOR", "1")
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);

    for forbidden in [
        // T3.6 (error chains)
        "--enable-go-error-chains",
        "--no-error-chains",
        "--enable-wraps",
        "--disable-wraps",
        // T3.7 (context propagation)
        "--enable-context-propagation",
        "--no-context-propagation",
        "--enable-ctx-checker",
        // T3.8 (build tags)
        "--enable-build-tags",
        "--no-build-tags",
        "--with-build-constraints",
        "--no-cfg-stamping",
    ] {
        assert!(
            !stdout.contains(forbidden),
            "`sqry index --help` MUST NOT advertise `{forbidden}` \
             (T3 tier ships every gated behaviour unconditionally per \
             01_SPEC §4.4); full help output: {stdout}",
        );
    }
}
