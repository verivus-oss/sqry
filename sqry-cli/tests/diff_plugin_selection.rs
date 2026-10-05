//! `sqry diff` builds both refs with one plugin selection: the one the
//! index at the repository root records, or the fast-path default when there
//! is no index. It compares what was built, so it takes no override that
//! would build with another plugin set.
//!
//! Before round 7's notes, every explicit selection was refused, the
//! `SQRY_*` variables included, even one naming the recorded set: an exported
//! `SQRY_INCLUDE_HIGH_COST=1` over an index built with it refused every diff
//! of two refs, while two refs naming one commit took a shortcut that never
//! looked. An override naming the recorded set is now accepted and one
//! naming another set refused, and identical and distinct refs are answered
//! alike, an unreadable manifest included.

mod common;

use std::path::Path;
use std::process::Output;

use common::sqry_bin;
use tempfile::TempDir;

const SELECTION_VARIABLES: [&str; 4] = [
    "SQRY_INCLUDE_HIGH_COST",
    "SQRY_EXCLUDE_HIGH_COST",
    "SQRY_ENABLE_PLUGINS",
    "SQRY_DISABLE_PLUGINS",
];

/// A repository of two commits, the second adding `gamma`, with a scratch
/// home, config directory and temporary directory of its own.
struct Repo {
    dir: TempDir,
    scratch: TempDir,
}

impl Repo {
    fn new() -> Self {
        let repo = Self {
            dir: TempDir::new().expect("repo"),
            scratch: TempDir::new().expect("scratch"),
        };
        std::fs::create_dir_all(repo.path().join("src")).expect("src");
        std::fs::write(
            repo.path().join("Cargo.toml"),
            "[package]\nname = \"dsel\"\nversion = \"0.1.0\"\n",
        )
        .expect("Cargo.toml");
        std::fs::write(repo.path().join("src/lib.rs"), "pub fn alpha() {}\n").expect("lib.rs");
        repo.git(&["init", "-q"]);
        repo.git(&["add", "."]);
        repo.git(&["commit", "-q", "-m", "one"]);
        std::fs::write(
            repo.path().join("src/lib.rs"),
            "pub fn alpha() {}\npub fn gamma() {}\n",
        )
        .expect("lib.rs");
        repo.git(&["commit", "-q", "-am", "two"]);
        repo
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn git(&self, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(self.path())
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .status()
            .expect("git");
        assert!(status.success(), "git {args:?}");
    }

    /// `sqry <args>` with no plugin-selection variable but `env`.
    fn sqry(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let tmp = self.scratch.path().join("tmp");
        std::fs::create_dir_all(&tmp).expect("tmp");
        let mut command = std::process::Command::new(sqry_bin());
        command
            .args(args)
            .current_dir(self.path())
            .env("SQRY_CONFIG_DIR", self.scratch.path().join("cfg"))
            .env("SQRY_DAEMON_SOCKET", self.scratch.path().join("no.sock"))
            .env("TMPDIR", &tmp)
            .env("NO_COLOR", "1");
        for variable in SELECTION_VARIABLES {
            command.env_remove(variable);
        }
        for (key, value) in env {
            command.env(key, value);
        }
        let output = command.output().expect("run sqry");
        println!(
            "sqry {args:?} {env:?}: {:?}\nstdout: {}\nstderr: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn index(&self, env: &[(&str, &str)]) {
        assert!(self.sqry(&["index", "."], env).status.success());
    }
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

const REF_PAIRS: [[&str; 2]; 2] = [["HEAD~1", "HEAD"], ["HEAD", "HEAD"]];

/// The index a case starts from.
#[derive(Debug, Clone, Copy)]
enum Index {
    /// No index: the diff builds with the fast-path default.
    Absent,
    /// Built with no selection variable: the fast path.
    FastPath,
    /// Built with `SQRY_INCLUDE_HIGH_COST=1`: every plugin.
    IncludeAll,
}

impl Index {
    fn build(self, repo: &Repo) {
        match self {
            Self::Absent => {}
            Self::FastPath => repo.index(&[]),
            Self::IncludeAll => repo.index(&[INCLUDE]),
        }
    }
}
const INCLUDE: (&str, &str) = ("SQRY_INCLUDE_HIGH_COST", "1");
const EXCLUDE: (&str, &str) = ("SQRY_EXCLUDE_HIGH_COST", "1");

/// An override naming the plugin set the index records overrides nothing:
/// `SQRY_INCLUDE_HIGH_COST=1` over an index built with it, and
/// `SQRY_EXCLUDE_HIGH_COST=1` over a fast-path index (or no index, whose
/// default is the fast path), are each accepted for identical and distinct
/// refs, and the distinct refs report `gamma`.
#[test]
fn diff_accepts_an_override_naming_the_recorded_plugin_set() {
    for (index, accepted) in [
        (Index::IncludeAll, INCLUDE),
        (Index::FastPath, EXCLUDE),
        (Index::Absent, EXCLUDE),
    ] {
        let repo = Repo::new();
        index.build(&repo);
        for refs in REF_PAIRS {
            let output = repo.sqry(&["--json", "diff", refs[0], refs[1]], &[accepted]);
            assert_eq!(
                output.status.code(),
                Some(0),
                "{index:?} {accepted:?} {refs:?}: {}",
                stderr(&output)
            );
            let json: serde_json::Value =
                serde_json::from_slice(&output.stdout).expect("diff JSON");
            let added = json["summary"]["added"].as_u64().expect("added");
            assert_eq!(
                added,
                u64::from(refs[0] != refs[1]),
                "{index:?} {refs:?}: {json}"
            );
        }
    }
}

/// An override naming another plugin set is refused, for identical and
/// distinct refs alike, before anything is built: `SQRY_INCLUDE_HIGH_COST=1`
/// over a fast-path index or no index, and `SQRY_EXCLUDE_HIGH_COST=1` over
/// an index built with high-cost plugins. The refusal says what is refused:
/// the set the diff builds with (the recorded one, or the default when there
/// is no index), and the plugins the variables add and drop. It used to say
/// overrides "are not allowed", though one naming the same set is.
#[test]
fn diff_refuses_an_override_naming_another_plugin_set() {
    for (index, refused) in [
        (Index::FastPath, INCLUDE),
        (Index::Absent, INCLUDE),
        (Index::IncludeAll, EXCLUDE),
    ] {
        let repo = Repo::new();
        index.build(&repo);
        let manifest = sqry_core::graph::unified::persistence::GraphStorage::new(repo.path())
            .manifest_path()
            .display()
            .to_string();
        let (built_with, change, remedy) = match index {
            Index::Absent => (
                "the fast-path default plugin set (there is no index at the root)".to_string(),
                "it adds json and drops nothing",
                "index the workspace with the desired plugins first",
            ),
            Index::FastPath => (
                format!("the plugin set the index records (manifest {manifest})"),
                "it adds json and drops nothing",
                "rebuild or update the indexed workspace with the desired plugins first",
            ),
            Index::IncludeAll => (
                format!("the plugin set the index records (manifest {manifest})"),
                "it adds nothing and drops json",
                "rebuild or update the indexed workspace with the desired plugins first",
            ),
        };
        let says = format!(
            "Error: Diff command failed: plugin-selection override refused: `sqry diff` builds \
             both refs with {built_with}, and the SQRY_* plugin-selection variables name another \
             set: {change}. Variables naming the same set are accepted; {remedy}\n"
        );
        for refs in REF_PAIRS {
            let output = repo.sqry(&["diff", refs[0], refs[1]], &[refused]);
            assert_eq!(
                output.status.code(),
                Some(1),
                "{index:?} {refused:?} {refs:?}"
            );
            assert_eq!(stderr(&output), says, "{index:?} {refused:?} {refs:?}");
        }
        let control = repo.sqry(&["diff", "HEAD~1", "HEAD"], &[]);
        assert!(
            control.status.success(),
            "the control: {}",
            stderr(&control)
        );
    }
}

/// A manifest that cannot be read is refused naming the file, for identical
/// and distinct refs alike (two refs naming one commit used to skip it and
/// exit 0); the readable manifest is the control.
#[test]
fn diff_refuses_an_unreadable_manifest_for_identical_and_distinct_refs() {
    let repo = Repo::new();
    repo.index(&[]);
    for refs in REF_PAIRS {
        assert!(
            repo.sqry(&["diff", refs[0], refs[1]], &[]).status.success(),
            "{refs:?}"
        );
    }
    let manifest = sqry_core::graph::unified::persistence::GraphStorage::new(repo.path())
        .manifest_path()
        .to_path_buf();
    std::fs::write(&manifest, b"{").expect("unreadable manifest");
    for refs in REF_PAIRS {
        let output = repo.sqry(&["diff", refs[0], refs[1]], &[]);
        assert_eq!(output.status.code(), Some(1), "{refs:?}");
        let err = stderr(&output);
        assert!(
            err.contains("failed to load manifest for plugin selection")
                && err.contains("is unreadable"),
            "{refs:?}: {err}"
        );
    }
}

/// Round 7 surfaces audit: the recorded set and the override's were
/// compared as ordered lists, so a manifest listing the same plugins in
/// another order, or one id twice, refused a variable naming that very set.
/// They are compared as sets: the reordered manifest with a repeated id
/// accepts `SQRY_EXCLUDE_HIGH_COST=1` (the fast path it records) for
/// identical and distinct refs, and still refuses `SQRY_INCLUDE_HIGH_COST=1`.
#[test]
fn diff_compares_the_plugin_set_not_the_order_the_manifest_lists_it_in() {
    let repo = Repo::new();
    repo.index(&[]);
    let manifest_path = sqry_core::graph::unified::persistence::GraphStorage::new(repo.path())
        .manifest_path()
        .to_path_buf();
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).expect("manifest")).expect("json");
    let ids = manifest["plugin_selection"]["active_plugin_ids"]
        .as_array_mut()
        .expect("ids");
    ids.reverse();
    let first = ids[0].clone();
    ids.push(first);
    std::fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).expect("json"),
    )
    .expect("reorder the recorded ids");

    for refs in REF_PAIRS {
        let output = repo.sqry(&["diff", refs[0], refs[1]], &[EXCLUDE]);
        assert_eq!(
            output.status.code(),
            Some(0),
            "{refs:?}: {}",
            stderr(&output)
        );
        let refused = repo.sqry(&["diff", refs[0], refs[1]], &[INCLUDE]);
        assert_eq!(refused.status.code(), Some(1), "{refs:?}");
        assert!(
            stderr(&refused).contains("it adds json and drops nothing"),
            "{refs:?}: {}",
            stderr(&refused)
        );
    }
}
