//! A `sqry index` whose build fails still says which inputs it attempted
//! (audit item F follow-up, round 8): the "Plugin selection:" and "Macro
//! options:" lines are printed after the build, about the inputs it used,
//! so a failure must not swallow them.

mod common;

use std::path::Path;
use std::process::{Command, Output};

use common::sqry_bin;
use sqry_core::graph::unified::persistence::GraphStorage;

fn sqry(root: &Path, args: &[&str]) -> Output {
    let mut command = Command::new(sqry_bin());
    command
        .args(args)
        .current_dir(root)
        .env("NO_COLOR", "1")
        .env("SQRY_FORCE_STANDALONE", "1");
    for key in [
        "SQRY_INCLUDE_HIGH_COST",
        "SQRY_EXCLUDE_HIGH_COST",
        "SQRY_ENABLE_PLUGINS",
        "SQRY_DISABLE_PLUGINS",
    ] {
        command.env_remove(key);
    }
    command.output().expect("run sqry")
}

#[test]
fn a_failed_index_build_reports_the_inputs_it_attempted() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/lib.rs"), "pub fn a() -> u32 { 1 }\n").unwrap();
    let first = sqry(&root, &["index", "--cfg", "test", "."]);
    assert!(
        first.status.success(),
        "the first index: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    // Make the next persist fail: the snapshot path becomes a directory,
    // which the atomic rename cannot replace.
    let storage = GraphStorage::new(&root);
    std::fs::remove_file(storage.snapshot_path()).unwrap();
    std::fs::create_dir_all(storage.snapshot_path()).unwrap();

    let failed = sqry(&root, &["index", "--force", "."]);
    let stdout = String::from_utf8_lossy(&failed.stdout);
    let stderr = String::from_utf8_lossy(&failed.stderr);
    assert!(
        !failed.status.success(),
        "precondition: the build fails\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("Plugin selection: fast_path_default"),
        "a failed build must say which selection it attempted\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("Macro options: cfg [test] (recorded in the manifest)"),
        "a failed build must say which macro options it attempted\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
