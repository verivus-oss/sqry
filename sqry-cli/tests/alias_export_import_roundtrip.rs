//! Regression guard for `sqry alias export` -> `sqry alias import`.
//!
//! The export-then-import pair must round-trip the alias set exactly. This
//! covers verivus-oss/sqry#216, where the importer rejected an
//! alias-export file (the file produced by export must always be a JSON
//! object with `version`, `exported_at`, and `aliases`, never an array).

mod common;
use common::sqry_bin;

use assert_cmd::Command;
use predicates::prelude::*;
use sqry_cli::persistence::{AliasManager, PersistenceConfig, StorageScope, open_shared_index};
use std::fs;
use tempfile::TempDir;

/// Build a fresh isolated environment: temp project root + temp global config.
struct Env {
    _tmp: TempDir,
    project: std::path::PathBuf,
    config_dir: std::path::PathBuf,
}

impl Env {
    fn new() -> Self {
        let tmp = TempDir::new().expect("create tempdir");
        let project = tmp.path().join("proj");
        let config_dir = tmp.path().join("cfg");
        fs::create_dir_all(&project).expect("create project dir");
        fs::create_dir_all(&config_dir).expect("create config dir");
        Self {
            _tmp: tmp,
            project,
            config_dir,
        }
    }

    /// Build a `sqry` Command pre-wired with the isolated config dir and
    /// the project as cwd.
    fn cmd(&self) -> Command {
        let mut cmd = Command::new(sqry_bin());
        cmd.current_dir(&self.project)
            .env("SQRY_CONFIG_DIR", &self.config_dir)
            .env_remove("SQRY_NO_REDACT");
        cmd
    }

    fn seed_alias(&self, name: &str, command: &str, args: &[&str]) {
        // SAFETY: tests in this binary do not mutate process env in parallel
        // with this seeding step; SQRY_CONFIG_DIR must be visible to the
        // library opening the user metadata index.
        unsafe {
            std::env::set_var("SQRY_CONFIG_DIR", &self.config_dir);
        }
        let config = PersistenceConfig::from_env();
        let index =
            open_shared_index(Some(&self.project), config).expect("open user metadata index");
        let manager = AliasManager::new(index);
        let owned: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
        manager
            .save(name, command, &owned, None, StorageScope::Local)
            .expect("seed alias");
        unsafe {
            std::env::remove_var("SQRY_CONFIG_DIR");
        }
    }
}

#[test]
fn alias_export_then_import_roundtrip_succeeds() {
    let env = Env::new();
    env.seed_alias("regression-216", "query", &["kind:function"]);

    let export_path = env.project.join("aliases.json");

    // Export.
    env.cmd()
        .arg("alias")
        .arg("export")
        .arg(&export_path)
        .assert()
        .success()
        .stdout(predicate::str::contains("Exported 1 aliases"));

    // Sanity: the file the exporter wrote must be a JSON object with the
    // documented shape, not a bare array. This is the exact invariant that
    // #216 reported broken.
    let written = fs::read_to_string(&export_path).expect("read exported file");
    let trimmed = written.trim_start();
    assert!(
        trimmed.starts_with('{'),
        "exporter must produce a JSON object; got:\n{written}"
    );
    let parsed: serde_json::Value =
        serde_json::from_str(&written).expect("exported file must be valid JSON");
    assert_eq!(parsed["version"], serde_json::json!(1));
    assert!(parsed["exported_at"].is_string(), "missing exported_at");
    assert!(parsed["aliases"].is_object(), "aliases must be an object");
    assert!(
        parsed["aliases"]["regression-216"].is_object(),
        "seeded alias missing from export"
    );

    // Wipe the seeded alias so import has work to do.
    let user_index = env.project.join(".sqry-index.user");
    if user_index.exists() {
        fs::remove_file(&user_index).expect("remove seeded user metadata");
    }

    // Import.
    env.cmd()
        .arg("alias")
        .arg("import")
        .arg(&export_path)
        .assert()
        .success()
        .stdout(predicate::str::contains("Imported 1 aliases"));

    // Confirm the alias is back.
    env.cmd()
        .arg("alias")
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::contains("regression-216"));
}

#[test]
fn alias_import_rejects_bare_json_array_with_helpful_error() {
    // The original bug surfaced as serde's terse "invalid length 0,
    // expected struct AliasExportFile with 3 elements at line 1 column 2"
    // when the importer was fed `[]`. We can't reproduce the harness
    // sequence that wrote `[]` to the file, but we can guarantee that
    // when this *does* happen, the error tells the operator what's wrong.
    let env = Env::new();
    let path = env.project.join("aliases.json");
    fs::write(&path, "[]").expect("write probe file");

    env.cmd()
        .arg("alias")
        .arg("import")
        .arg(&path)
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("contains a JSON array, not a JSON object")
                .and(predicate::str::contains("`sqry alias export`")),
        );
}

#[test]
fn alias_import_rejects_empty_file_with_helpful_error() {
    let env = Env::new();
    let path = env.project.join("aliases.json");
    fs::write(&path, "").expect("write empty probe file");

    env.cmd()
        .arg("alias")
        .arg("import")
        .arg(&path)
        .assert()
        .failure()
        .stderr(predicate::str::contains("is empty"));
}
