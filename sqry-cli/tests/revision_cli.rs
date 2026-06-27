//! RWS12 CLI revision surface regression tests.

mod common;

use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn daemon_load_revision_help_lists_public_source_modes_only() {
    Command::new(common::sqry_bin())
        .args(["daemon", "load-revision", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("raw-git-objects"))
        .stdout(predicate::str::contains("dirty-snapshot"))
        .stdout(predicate::str::contains("checkout-bytes").not());
}

#[test]
fn daemon_revision_commands_are_discoverable() {
    Command::new(common::sqry_bin())
        .args(["daemon", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("load-revision"))
        .stdout(predicate::str::contains("list-revisions"))
        .stdout(predicate::str::contains("revision-status"))
        .stdout(predicate::str::contains("unload-revision"))
        .stdout(predicate::str::contains("prune-revisions"));
}
