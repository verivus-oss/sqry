//! `sqry index` never drops a macro build option on a path that builds
//! nothing, checks every input before it writes anything, and resolves a
//! hand-edited relative record against the workspace root.
//!
//! - Over an existing index without `--force`, `--cfg`, `--expand-cache` and
//!   `--no-macro-options` used to print "Index already exists" and exit 0
//!   with the flag ignored; they are refused, non-zero, nothing written,
//!   as both MCP hosts refuse the same request.
//! - `--status` builds nothing, so clap refuses the macro flags beside it.
//! - `--add-to-gitignore` wrote `.gitignore` before the macro options were
//!   resolved, so a refused build still edited the repository. It is now
//!   written once every input of the leg is accepted, where it always stood
//!   in the output: before the build's banner, or before the early exit
//!   reports the index.
//! - Every other input is checked before anything is written: a missing or
//!   file root, a `.sqry` that is not a directory, and a `--classpath-file`
//!   that cannot be read the way the classpath pipeline reads it.
//! - An empty, blank or padded (` test`) `--cfg` is a usage error.
//! - A requested directory that is missing, or a file, is named as such and
//!   the remedy is a directory that exists, not `--no-macro-options`.
//! - A relative directory a hand-edited manifest records resolves against the
//!   workspace root, wherever `sqry update` is run from.

mod common;

use std::path::Path;
use std::process::Output;

use common::sqry_bin;
use sha2::{Digest, Sha256};
use sqry_core::graph::unified::persistence::GraphStorage;
use tempfile::TempDir;

const LIB_RS: &str = "#[cfg(test)]\npub fn gated() {}\npub fn plain() {}\n";

fn fixture() -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    std::fs::create_dir_all(dir.path().join("src")).expect("src");
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"r7\"\nversion = \"0.1.0\"\n",
    )
    .expect("Cargo.toml");
    std::fs::write(dir.path().join("src").join("lib.rs"), LIB_RS).expect("lib.rs");
    dir
}

fn sqry_in(cwd: &Path, args: &[&str]) -> Output {
    let output = std::process::Command::new(sqry_bin())
        .args(args)
        .current_dir(cwd)
        .env("NO_COLOR", "1")
        .output()
        .expect("run sqry");
    println!(
        "sqry {args:?}: {:?}\nstdout: {}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn index_digest(root: &Path) -> (String, String) {
    let storage = GraphStorage::new(root);
    let digest = |path: &Path| hex::encode(Sha256::digest(std::fs::read(path).expect("read")));
    (
        digest(storage.manifest_path()),
        digest(storage.snapshot_path()),
    )
}

fn root_of(dir: &TempDir) -> String {
    dir.path()
        .canonicalize()
        .expect("canonical")
        .to_string_lossy()
        .into_owned()
}

/// Over an existing index, each macro flag without `--force` is refused
/// with a non-zero exit, names `--force`, and writes nothing; the plain
/// call is the accepted control and `--force` builds.
#[test]
fn index_without_force_refuses_macro_flags_over_an_existing_index() {
    let dir = fixture();
    let root = root_of(&dir);
    let cwd = dir.path();
    assert!(sqry_in(cwd, &["index", &root]).status.success());
    std::fs::create_dir(dir.path().join("cache")).expect("cache");
    let before = index_digest(dir.path());
    for flags in [
        vec!["--cfg", "test"],
        vec!["--expand-cache", "cache"],
        vec!["--expand-cache", "missing"],
        vec!["--no-macro-options"],
    ] {
        let mut args = vec!["index"];
        args.extend(&flags);
        args.push(&root);
        let output = sqry_in(cwd, &args);
        assert!(!output.status.success(), "{flags:?} must be refused");
        let err = stderr(&output);
        assert!(
            err.contains("need --force") && err.contains("nothing was built"),
            "{flags:?}: {err}"
        );
        assert!(
            !String::from_utf8_lossy(&output.stdout).contains("Index already exists"),
            "{flags:?}"
        );
        assert_eq!(index_digest(dir.path()), before, "{flags:?} wrote nothing");
    }

    let output = sqry_in(cwd, &["index", &root]);
    assert!(output.status.success(), "the plain call reports the index");
    assert!(String::from_utf8_lossy(&output.stdout).contains("Index already exists"));

    let output = sqry_in(cwd, &["index", "--force", "--cfg", "test", &root]);
    assert!(output.status.success(), "--force builds");
    let record = GraphStorage::new(dir.path())
        .load_manifest()
        .expect("manifest")
        .macro_options
        .expect("recorded");
    assert_eq!(record.cfg_flags, vec!["test".to_string()]);
}

/// `--status` builds nothing: clap refuses each macro flag beside it, and
/// `--status` alone is the accepted control.
#[test]
fn index_status_refuses_macro_flags() {
    let dir = fixture();
    let root = root_of(&dir);
    let cwd = dir.path();
    assert!(sqry_in(cwd, &["index", &root]).status.success());
    for flags in [
        vec!["--cfg", "test"],
        vec!["--expand-cache", "cache"],
        vec!["--no-macro-options"],
    ] {
        let mut args = vec!["index", "--status"];
        args.extend(&flags);
        args.push(&root);
        let output = sqry_in(cwd, &args);
        assert_eq!(output.status.code(), Some(2), "{flags:?}: clap usage error");
        assert!(
            stderr(&output).contains("cannot be used with"),
            "{flags:?}: {}",
            stderr(&output)
        );
    }
    assert!(sqry_in(cwd, &["index", "--status", &root]).status.success());
}

fn git_repo() -> TempDir {
    let dir = fixture();
    let status = std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(dir.path())
        .status()
        .expect("git init");
    assert!(status.success());
    dir
}

/// Every refusal comes before the `.gitignore` write: a missing
/// `--expand-cache`, an empty `--cfg` value and a macro flag over an
/// existing index without `--force` leave `.gitignore` absent. The accepted
/// controls write it: a fresh build, and the early exit over an existing
/// index with no macro flag.
#[test]
fn index_writes_gitignore_only_after_every_input_is_accepted() {
    let gitignore = |dir: &TempDir| dir.path().join(".gitignore");

    let dir = git_repo();
    let root = root_of(&dir);
    let output = sqry_in(
        dir.path(),
        &[
            "index",
            "--add-to-gitignore",
            "--expand-cache",
            "missing-cache",
            &root,
        ],
    );
    assert!(!output.status.success(), "a missing cache is refused");
    assert!(!gitignore(&dir).exists(), "nothing was written");
    assert!(
        !GraphStorage::new(dir.path()).exists(),
        "no index was written"
    );

    let output = sqry_in(
        dir.path(),
        &["index", "--add-to-gitignore", "--cfg", " ", &root],
    );
    assert!(!output.status.success(), "a blank cfg flag is refused");
    assert!(!gitignore(&dir).exists(), "nothing was written");

    let output = sqry_in(dir.path(), &["index", "--add-to-gitignore", &root]);
    assert!(output.status.success());
    let written = std::fs::read_to_string(gitignore(&dir)).expect(".gitignore written");
    assert!(written.contains(".sqry/"), "{written}");

    let dir = git_repo();
    let root = root_of(&dir);
    assert!(sqry_in(dir.path(), &["index", &root]).status.success());
    let output = sqry_in(
        dir.path(),
        &["index", "--add-to-gitignore", "--cfg", "test", &root],
    );
    assert!(!output.status.success(), "the flag needs --force");
    assert!(!gitignore(&dir).exists(), "nothing was written");
    let output = sqry_in(dir.path(), &["index", "--add-to-gitignore", &root]);
    assert!(
        output.status.success(),
        "the early exit still adds the entry"
    );
    assert!(gitignore(&dir).exists());
}

/// One value parser refuses every unusable `--cfg` alike: an empty, a blank
/// and a padded value are each a usage error (exit 2) with the core's
/// message, and nothing is built. An empty value used to be clap's error
/// and a blank one the request check's (exit 1), and a padded one was
/// recorded as given. Real flags, one with inner spacing included, are the
/// accepted control.
#[test]
fn index_refuses_an_empty_blank_or_padded_cfg_flag() {
    let dir = fixture();
    let root = root_of(&dir);
    for (value, says) in [
        ("", "a cfg flag is empty"),
        ("  ", "a cfg flag is empty"),
        (" test", "has leading or trailing whitespace"),
        ("test ", "has leading or trailing whitespace"),
        ("\ttest", "has leading or trailing whitespace"),
    ] {
        let output = sqry_in(dir.path(), &["index", "--cfg", value, &root]);
        assert_eq!(output.status.code(), Some(2), "{value:?}: a usage error");
        assert!(
            stderr(&output).contains(says),
            "{value:?}: {}",
            stderr(&output)
        );
        assert!(
            !GraphStorage::new(dir.path()).exists(),
            "{value:?}: nothing was built"
        );
    }
    assert!(
        sqry_in(
            dir.path(),
            &["index", "--cfg", "test", "--cfg", "feature = \"x\"", &root]
        )
        .status
        .success()
    );
    let record = GraphStorage::new(dir.path())
        .load_manifest()
        .expect("manifest")
        .macro_options
        .expect("recorded");
    assert_eq!(
        record.cfg_flags,
        vec!["test".to_string(), "feature = \"x\"".to_string()]
    );
}

/// A requested directory that is missing, or a file, is refused with the
/// core's reason for its shape ("expand cache directory <DIR> does not exist
/// or is not a directory"), said to come from `--expand-cache`, and the
/// remedy is a directory that exists: `--no-macro-options` given beside it
/// is not suggested back.
#[test]
fn index_names_a_missing_or_file_expand_cache_without_suggesting_the_reset() {
    let dir = fixture();
    let root = root_of(&dir);
    std::fs::write(dir.path().join("a-file"), b"x").expect("file");
    for (name, extra) in [
        ("missing-cache", None),
        ("a-file", None),
        ("missing-cache", Some("--no-macro-options")),
    ] {
        let mut args = vec!["index", "--expand-cache", name];
        if let Some(flag) = extra {
            args.push(flag);
        }
        args.push(&root);
        let output = sqry_in(dir.path(), &args);
        assert!(!output.status.success(), "{name} {extra:?}");
        let err = stderr(&output);
        assert!(
            err.contains(&format!(
                "expand cache directory {} does not exist or is not a directory (from \
                 --expand-cache)",
                Path::new(&root).join(name).display()
            )) && err.contains("pass a directory that exists"),
            "{name} {extra:?}: {err}"
        );
        assert!(
            !err.contains("--no-macro-options"),
            "the reset is not the remedy for a requested directory: {err}"
        );
        assert!(!GraphStorage::new(dir.path()).exists(), "nothing was built");
    }
}

/// A recorded expand cache directory that is gone is refused with both
/// ways out: name one that exists with `--expand-cache`, or drop the record
/// with `--no-macro-options` (a requested one gets only the first, above).
/// `sqry index --force` and `sqry update` refuse alike and write nothing;
/// each remedy is the accepted control and builds.
#[test]
fn index_names_both_remedies_for_a_missing_recorded_expand_cache() {
    let dir = fixture();
    let root = root_of(&dir);
    let cache = dir.path().join("expand-cache");
    std::fs::create_dir(&cache).expect("cache");
    let cache_arg = cache.to_string_lossy().into_owned();
    assert!(
        sqry_in(
            dir.path(),
            &[
                "index",
                "--cfg",
                "test",
                "--expand-cache",
                &cache_arg,
                &root
            ]
        )
        .status
        .success()
    );
    std::fs::remove_dir(&cache).expect("remove the cache");
    let before = index_digest(dir.path());
    let says = format!(
        "expand cache directory {} does not exist or is not a directory (recorded in the index \
         manifest); pass --expand-cache <DIR> naming a directory that exists, or drop the record \
         with --no-macro-options",
        // The cache is gone, so canonicalise its parent: the refusal names
        // the canonical path even when TMPDIR is reached through a symlink.
        Path::new(&root).join("expand-cache").display()
    );
    for args in [vec!["index", "--force", &root], vec!["update", &root]] {
        let output = sqry_in(dir.path(), &args);
        assert_eq!(output.status.code(), Some(1), "{args:?}");
        let err = stderr(&output);
        assert!(err.contains(&says), "{args:?}: {err}");
        assert_eq!(
            index_digest(dir.path()),
            before,
            "{args:?}: nothing was written"
        );
    }

    let output = sqry_in(
        dir.path(),
        &["index", "--force", "--no-macro-options", &root],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    std::fs::create_dir(&cache).expect("cache again");
    let output = sqry_in(
        dir.path(),
        &["index", "--force", "--expand-cache", &cache_arg, &root],
    );
    assert!(output.status.success(), "{}", stderr(&output));
}

/// A relative directory recorded in the manifest (only a hand edit writes
/// one) resolves against the workspace root: `sqry update` run from another
/// directory reuses it and records the canonical path. Before, it resolved
/// against the process's working directory, so the same update succeeded
/// from the root and was refused from elsewhere.
#[test]
fn update_resolves_a_relative_recorded_expand_cache_against_the_root() {
    let dir = fixture();
    let root = root_of(&dir);
    let cache = dir.path().join("rel-cache");
    std::fs::create_dir(&cache).expect("cache");
    assert!(
        sqry_in(
            dir.path(),
            &["index", "--expand-cache", &cache.to_string_lossy(), &root]
        )
        .status
        .success()
    );
    let storage = GraphStorage::new(dir.path());
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(storage.manifest_path()).expect("manifest"))
            .expect("json");
    manifest["macro_options"]["expand_cache_dir"] = serde_json::json!("rel-cache");
    std::fs::write(
        storage.manifest_path(),
        serde_json::to_vec_pretty(&manifest).expect("json"),
    )
    .expect("hand edit");

    let elsewhere = TempDir::new().expect("elsewhere");
    assert!(
        !elsewhere.path().join("rel-cache").exists(),
        "precondition: the other directory has no rel-cache"
    );
    let output = sqry_in(elsewhere.path(), &["update", &root]);
    assert!(output.status.success(), "{}", stderr(&output));
    let record = storage
        .load_manifest()
        .expect("manifest")
        .macro_options
        .expect("recorded");
    assert_eq!(
        record.expand_cache_dir.as_deref(),
        Some(
            cache
                .canonicalize()
                .expect("canonical")
                .to_string_lossy()
                .as_ref()
        ),
        "the record is the root's directory, canonical"
    );
}

/// Hand-edit the manifest at `root` so its record names `dir` as given.
fn record_expand_cache_as(root: &Path, dir: &str) {
    let storage = GraphStorage::new(root);
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(storage.manifest_path()).expect("manifest"))
            .expect("json");
    manifest["macro_options"]["expand_cache_dir"] = serde_json::json!(dir);
    std::fs::write(
        storage.manifest_path(),
        serde_json::to_vec_pretty(&manifest).expect("json"),
    )
    .expect("hand edit");
}

/// The roots that name the workspace `ws` through a symlink in `parent`
/// (relative and absolute), for commands run from `parent`. Only Unix
/// creates the link; elsewhere there is no such case.
fn symlinked_root_cases(ws: &Path, parent: &Path) -> Vec<Vec<String>> {
    #[cfg(unix)]
    {
        let link = parent.join("link");
        std::os::unix::fs::symlink(ws, &link).expect("symlink");
        vec![
            vec!["update".into(), "link".into()],
            vec!["index".into(), "--force".into(), "link".into()],
            vec!["update".into(), link.to_string_lossy().into_owned()],
        ]
    }
    #[cfg(not(unix))]
    {
        let _ = (ws, parent);
        Vec::new()
    }
}

/// The expand cache directory the manifest at `root` records.
fn recorded_expand_cache(root: &Path) -> Option<String> {
    GraphStorage::new(root)
        .load_manifest()
        .expect("manifest")
        .macro_options
        .and_then(|record| record.expand_cache_dir)
}

/// A relative record resolves against the directory being indexed even when
/// that directory is itself given relative to the caller (`.`, the default
/// when no path is given, or `outer/ws`) or through a symlink. Joined to a
/// relative root the record stayed relative and was refused unread as
/// missing ("./rel-cache does not exist"), so `sqry update` and
/// `sqry index --force` failed from the workspace while they succeeded at
/// `70ed55e5b`. Each call must succeed and record the canonical directory.
#[test]
fn a_relative_record_is_reused_from_a_relative_or_symlinked_root() {
    let parent = TempDir::new().expect("parent");
    let ws = parent.path().join("outer").join("ws");
    std::fs::create_dir_all(ws.join("src")).expect("src");
    std::fs::write(
        ws.join("Cargo.toml"),
        "[package]\nname = \"r7\"\nversion = \"0.1.0\"\n",
    )
    .expect("Cargo.toml");
    std::fs::write(ws.join("src").join("lib.rs"), LIB_RS).expect("lib.rs");
    let cache = ws.join("rel-cache");
    std::fs::create_dir(&cache).expect("cache");
    let canonical_cache = cache
        .canonicalize()
        .expect("canonical")
        .to_string_lossy()
        .into_owned();
    let ws_text = ws.to_string_lossy().into_owned();
    assert!(
        sqry_in(
            &ws,
            &["index", "--expand-cache", &canonical_cache, &ws_text]
        )
        .status
        .success()
    );
    let mut cases: Vec<(&Path, Vec<String>)> = vec![
        (&ws, vec!["update".into()]),
        (&ws, vec!["update".into(), ".".into()]),
        (&ws, vec!["index".into(), "--force".into()]),
        (&ws, vec!["index".into(), "--force".into(), ".".into()]),
        (parent.path(), vec!["update".into(), "outer/ws".into()]),
        (
            parent.path(),
            vec!["index".into(), "--force".into(), "outer/ws".into()],
        ),
        (&ws, vec!["update".into(), "../ws".into()]),
    ];
    cases.extend(
        symlinked_root_cases(&ws, parent.path())
            .into_iter()
            .map(|args| (parent.path(), args)),
    );
    for (cwd, args) in &cases {
        record_expand_cache_as(&ws, "rel-cache");
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let output = sqry_in(cwd, &args);
        assert!(
            output.status.success(),
            "{args:?} from {}: {}",
            cwd.display(),
            stderr(&output)
        );
        assert_eq!(
            recorded_expand_cache(&ws).as_deref(),
            Some(canonical_cache.as_str()),
            "{args:?}: the record is the root's directory, canonical"
        );
    }

    // The control from the other side: the same relative roots with a
    // record naming a directory that is not under the root are refused,
    // naming the directory anchored to the absolute root.
    for (cwd, args) in [
        (ws.as_path(), vec!["update"]),
        (parent.path(), vec!["update", "outer/ws"]),
    ] {
        record_expand_cache_as(&ws, "no-such-cache");
        let output = sqry_in(cwd, &args);
        assert!(!output.status.success(), "{args:?}");
        let err = stderr(&output);
        assert!(
            err.contains(&format!(
                "{} does not exist or is not a directory",
                ws.canonicalize()
                    .expect("canonical")
                    .join("no-such-cache")
                    .display()
            )),
            "{args:?}: the refusal names the absolute directory: {err}"
        );
    }
}

/// Every entry of `dir`, recursively, as paths relative to it: the evidence
/// that a refusal wrote nothing.
fn tree(dir: &Path) -> Vec<String> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.file_name().is_some_and(|name| name == ".git") {
                continue;
            }
            out.push(
                path.strip_prefix(base)
                    .expect("under the base")
                    .to_string_lossy()
                    .into_owned(),
            );
            if path.is_dir() {
                walk(base, &path, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

/// S7 (round 7): `--classpath-file` is read the way the classpath pipeline
/// reads it before anything is written. A missing file, a directory and a
/// file that is not UTF-8 text (read line by line, as the pipeline reads
/// it, not as bytes) are refused in the pipeline's own words, and the repository is left exactly
/// as it was: no `.gitignore`, no `.sqry` (the pipeline used to write
/// `.gitignore` and could write `.sqry/classpath` first), and the refusal
/// comes before the build starts. A readable file is the accepted control:
/// it builds and writes the entry.
#[cfg(feature = "jvm-classpath")]
#[test]
fn index_reads_the_classpath_file_before_writing_anything() {
    let dir = git_repo();
    let root = root_of(&dir);
    std::fs::create_dir(dir.path().join("cp-dir")).expect("a directory");
    std::fs::write(dir.path().join("cp-bin.txt"), b"lib.jar\n\xff\xfe\n").expect("not text");
    let before = tree(dir.path());
    for (file, says) in [
        (
            "/nonexistent/r7-cp.txt",
            "Cannot open classpath file /nonexistent/r7-cp.txt",
        ),
        ("cp-dir", "Error reading classpath file cp-dir"),
        (
            "cp-bin.txt",
            "Error reading classpath file cp-bin.txt: stream did not contain valid UTF-8",
        ),
    ] {
        let output = sqry_in(
            dir.path(),
            &[
                "index",
                "--add-to-gitignore",
                "--classpath",
                "--classpath-file",
                file,
                &root,
            ],
        );
        assert_eq!(output.status.code(), Some(1), "{file}");
        let err = stderr(&output);
        assert!(
            err.contains("Classpath pipeline failed") && err.contains(says),
            "{file}: {err}"
        );
        let out = String::from_utf8_lossy(&output.stdout);
        assert!(
            !out.contains("Building index") && !out.contains("Running JVM classpath analysis"),
            "{file}: refused before the build started: {out}"
        );
        assert_eq!(tree(dir.path()), before, "{file}: nothing was written");
    }

    std::fs::write(dir.path().join("cp.txt"), "# no jars\n").expect("cp.txt");
    let output = sqry_in(
        dir.path(),
        &[
            "index",
            "--add-to-gitignore",
            "--classpath",
            "--classpath-file",
            "cp.txt",
            &root,
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(GraphStorage::new(dir.path()).exists());
    assert!(
        std::fs::read_to_string(dir.path().join(".gitignore"))
            .expect(".gitignore")
            .contains(".sqry/")
    );
}

/// S7: the root is checked before anything is written. A root that does not
/// exist used to be refused by the build only after `.gitignore` was
/// written in the enclosing repository, and with a classpath file it was
/// created through `.sqry/classpath` and indexed; a file root failed only
/// at the persist. Each is now refused naming the path, with the repository
/// unchanged and the missing root not created.
#[test]
fn index_refuses_a_missing_or_file_root_before_writing_anything() {
    let dir = git_repo();
    std::fs::write(dir.path().join("cp.txt"), "# no jars\n").expect("cp.txt");
    let before = tree(dir.path());
    for (args, says) in [
        (
            vec!["index", "--add-to-gitignore", "missing-root"],
            "Path missing-root does not exist",
        ),
        (
            vec![
                "index",
                "--classpath",
                "--classpath-file",
                "cp.txt",
                "missing-root",
            ],
            "Path missing-root does not exist",
        ),
        (
            vec!["index", "--add-to-gitignore", "src/lib.rs"],
            "Path src/lib.rs is not a directory",
        ),
    ] {
        let output = sqry_in(dir.path(), &args);
        assert_eq!(output.status.code(), Some(1), "{args:?}");
        let err = stderr(&output);
        assert!(
            err.contains(says) && err.contains("nothing was written"),
            "{args:?}: {err}"
        );
        assert_eq!(tree(dir.path()), before, "{args:?}: nothing was written");
    }
}

/// S7: a `.sqry` that is not a directory is refused before anything is
/// written: the persist cannot create `.sqry/graph` under it, and it used to
/// fail only after the build, with the `.gitignore` entry already added. A
/// file and a dangling link are each refused naming the path, the repository
/// unchanged. The accepted control builds and writes the entry.
#[test]
fn index_refuses_a_non_directory_index_dir_before_writing_anything() {
    let dir = git_repo();
    let root = root_of(&dir);
    let index_dir = dir.path().join(".sqry");
    let mut cases = vec![("a file", "is not a directory")];
    if cfg!(unix) {
        cases.push(("a dangling link", "is a link to nothing"));
    }
    for (case, says) in cases {
        if case == "a file" {
            std::fs::write(&index_dir, b"not a directory").expect(".sqry file");
        } else {
            #[cfg(unix)]
            std::os::unix::fs::symlink(dir.path().join("gone"), &index_dir).expect(".sqry link");
        }
        let before = tree(dir.path());
        let output = sqry_in(dir.path(), &["index", "--add-to-gitignore", &root]);
        assert_eq!(output.status.code(), Some(1), "{case}: refused");
        let err = stderr(&output);
        assert!(
            err.contains(&format!("Path {root}/.sqry {says}"))
                && err.contains("nothing was written"),
            "{case}: {err}"
        );
        assert!(
            !String::from_utf8_lossy(&output.stdout).contains("Building index"),
            "{case}: refused before the build started"
        );
        assert_eq!(tree(dir.path()), before, "{case}: nothing was written");
        std::fs::remove_file(&index_dir).expect("remove the entry");
    }

    let output = sqry_in(dir.path(), &["index", "--add-to-gitignore", &root]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(GraphStorage::new(dir.path()).exists());
    assert!(
        std::fs::read_to_string(dir.path().join(".gitignore"))
            .expect(".gitignore")
            .contains(".sqry/")
    );
}

/// S7 notes (round 7): the `.gitignore` entry stands where it always stood
/// in the output, once every input of the leg is accepted. On the build leg
/// "Added '.sqry/' to .gitignore" is the first line, before the banner; on
/// the early exit it comes before "Index already exists". Without
/// `--add-to-gitignore` the recommendation to add it opens stderr. Moving
/// the write after the build had put the line after the build's own lines.
#[test]
fn index_reports_the_gitignore_entry_where_it_always_stood() {
    let lines = |output: &Output| -> Vec<String> {
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::to_string)
            .collect()
    };

    let dir = git_repo();
    let root = root_of(&dir);
    let output = sqry_in(dir.path(), &["index", "--add-to-gitignore", &root]);
    assert!(output.status.success(), "{}", stderr(&output));
    let built = lines(&output);
    assert_eq!(built[0], "Added '.sqry/' to .gitignore", "{built:?}");
    assert!(
        built[1].starts_with(&format!("Building index for {root}")),
        "{built:?}"
    );

    let dir = git_repo();
    let root = root_of(&dir);
    let output = sqry_in(dir.path(), &["index", &root]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stderr(&output).starts_with(
            "\n\u{26a0}\u{fe0f} Warning: It is recommended to add the '.sqry/' directory"
        ),
        "the recommendation opens stderr: {}",
        stderr(&output)
    );
    assert!(lines(&output)[0].starts_with("Building index for "));
    let output = sqry_in(dir.path(), &["index", "--add-to-gitignore", &root]);
    assert!(output.status.success(), "{}", stderr(&output));
    let early = lines(&output);
    assert_eq!(early[0], "Added '.sqry/' to .gitignore", "{early:?}");
    assert!(
        early[1].starts_with("Index already exists at "),
        "{early:?}"
    );
}

/// S9 (round 7): the early exit over an existing index writes `.gitignore`
/// only after the recorded selection is classified. A manifest naming a
/// plugin id this binary did not compile, and one that cannot be read, are
/// refused with `--add-to-gitignore` given, and no `.gitignore` is written.
/// The readable manifest is the accepted control: the early exit writes it.
#[test]
fn index_early_exit_writes_gitignore_only_after_the_selection_is_classified() {
    for case in ["uncompiled-id", "unreadable"] {
        let dir = git_repo();
        let root = root_of(&dir);
        assert!(sqry_in(dir.path(), &["index", &root]).status.success());
        let storage = GraphStorage::new(dir.path());
        if case == "uncompiled-id" {
            let mut manifest: serde_json::Value =
                serde_json::from_slice(&std::fs::read(storage.manifest_path()).expect("manifest"))
                    .expect("json");
            manifest["plugin_selection"]["active_plugin_ids"]
                .as_array_mut()
                .expect("ids")
                .push(serde_json::json!("r7-uncompiled-plugin"));
            std::fs::write(
                storage.manifest_path(),
                serde_json::to_vec_pretty(&manifest).expect("json"),
            )
            .expect("plant the id");
        } else {
            std::fs::write(storage.manifest_path(), b"{").expect("unreadable manifest");
        }
        let output = sqry_in(dir.path(), &["index", "--add-to-gitignore", &root]);
        assert_eq!(output.status.code(), Some(1), "{case}: refused");
        assert!(
            !dir.path().join(".gitignore").exists(),
            "{case}: the refusal wrote no .gitignore: {}",
            stderr(&output)
        );
    }

    let dir = git_repo();
    let root = root_of(&dir);
    assert!(sqry_in(dir.path(), &["index", &root]).status.success());
    let output = sqry_in(dir.path(), &["index", "--add-to-gitignore", &root]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("Index already exists"));
    assert!(
        dir.path().join(".gitignore").exists(),
        "the control writes it"
    );
}

/// Round 7 surfaces audit (CL4b, CL5b): a plugin id this binary does not
/// know, and a fresh index nested under another project's index, are each
/// refused before the build leg writes the `.gitignore` entry: with
/// `--add-to-gitignore` given, the repository is left exactly as it was.
/// The accepted controls write the entry: a known plugin id, and the nested
/// index with `--allow-nested`.
#[test]
fn index_refuses_an_unknown_plugin_or_a_nested_index_before_writing_anything() {
    let dir = git_repo();
    let root = root_of(&dir);
    let before = tree(dir.path());
    let output = sqry_in(
        dir.path(),
        &[
            "index",
            "--add-to-gitignore",
            "--enable-plugin",
            "r7-no-such-plugin",
            &root,
        ],
    );
    assert_eq!(
        output.status.code(),
        Some(1),
        "an unknown plugin is refused"
    );
    assert!(
        stderr(&output).contains("unknown plugin ids: r7-no-such-plugin"),
        "{}",
        stderr(&output)
    );
    assert_eq!(tree(dir.path()), before, "nothing was written");
    let output = sqry_in(
        dir.path(),
        &[
            "index",
            "--add-to-gitignore",
            "--enable-plugin",
            "rust",
            &root,
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        std::fs::read_to_string(dir.path().join(".gitignore"))
            .expect("the control writes .gitignore")
            .contains(".sqry/")
    );

    let dir = git_repo();
    let root = root_of(&dir);
    assert!(sqry_in(dir.path(), &["index", &root]).status.success());
    std::fs::create_dir_all(dir.path().join("sub")).expect("sub");
    std::fs::write(
        dir.path().join("sub").join("inner.rs"),
        "pub fn inner() {}\n",
    )
    .expect("inner.rs");
    let nested = format!("{root}/sub");
    let before = tree(dir.path());
    let output = sqry_in(dir.path(), &["index", "--add-to-gitignore", &nested]);
    assert_eq!(output.status.code(), Some(1), "a nested index is refused");
    let err = stderr(&output);
    assert!(
        err.contains("nested .sqry/ index") && err.contains("--allow-nested"),
        "{err}"
    );
    assert_eq!(tree(dir.path()), before, "nothing was written");
    let output = sqry_in(
        dir.path(),
        &["index", "--add-to-gitignore", "--allow-nested", &nested],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(GraphStorage::new(&dir.path().join("sub")).exists());
    assert!(
        std::fs::read_to_string(dir.path().join(".gitignore"))
            .expect("the control writes .gitignore")
            .contains(".sqry/")
    );
}

/// Round 7 surfaces audit (CL2b, CL3), Unix: the accepted and the
/// unreadable sides of the `.sqry` check. A `.sqry` that is a link to a
/// directory is accepted: the index is built through it, into the
/// directory it names. A `.sqry` that cannot be read (a link that names
/// itself) is refused naming the path, before anything is written.
#[cfg(unix)]
#[test]
fn index_builds_through_a_linked_index_dir_and_refuses_one_it_cannot_read() {
    let dir = git_repo();
    let root = root_of(&dir);
    let target = TempDir::new().expect("link target");
    std::os::unix::fs::symlink(target.path(), dir.path().join(".sqry")).expect(".sqry link");
    let output = sqry_in(dir.path(), &["index", "--add-to-gitignore", &root]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        target.path().join("graph").join("manifest.json").is_file(),
        "the index is written into the directory the link names"
    );
    assert!(GraphStorage::new(dir.path()).exists());

    let dir = git_repo();
    let root = root_of(&dir);
    std::os::unix::fs::symlink(".sqry", dir.path().join(".sqry")).expect(".sqry loop");
    let before = tree(dir.path());
    let output = sqry_in(dir.path(), &["index", "--add-to-gitignore", &root]);
    assert_eq!(
        output.status.code(),
        Some(1),
        "an unreadable .sqry is refused"
    );
    let err = stderr(&output);
    assert!(
        err.contains(&format!("Path {root}/.sqry cannot be read"))
            && err.contains("nothing was written"),
        "{err}"
    );
    assert!(
        !String::from_utf8_lossy(&output.stdout).contains("Building index"),
        "refused before the build started"
    );
    assert_eq!(tree(dir.path()), before, "nothing was written");
}
