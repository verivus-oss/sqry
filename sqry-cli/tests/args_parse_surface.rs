//! T6's behavioural companion (surface parity W4 round 2, plan step S8).
//!
//! T6 is the three `args::tests::w4_*` unit tests in `sqry-cli/src/args/mod.rs`.
//! Their committed command form, `cargo test -p sqry-cli --lib --
//! args::tests::w4_`, matched no test at round 1's pre-change head and printed
//! `test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 670 filtered
//! out` with exit 0: a zero-case, neither a red nor a green.
//!
//! This file carries the same three claims as assertions over the parse
//! surface seen from outside the crate, through `sqry_cli::args::Cli`, and it
//! reads no field of a subcommand variant (it goes through clap's
//! `ArgMatches` for those), so it compiles at round 1's pre-change head as
//! well as here and its red there is a failing assertion:
//!
//! - every argument round 1 removed is rejected as an unknown argument;
//! - `--threshold-orphaned-files` parses inside 0.0 to 1.0 and is refused,
//!   naming the value, outside it;
//! - the macro-options arguments round 1 added parse on `index` and on
//!   `daemon rebuild`, and carry the values given.
//!
//! Every check is collected and printed, and the counts are asserted, so a
//! red run names every claim that failed rather than the first.
//!
//! Record: `docs/development/surface-parity/06_TEST_EXECUTION-surface-parity.md`.

use std::path::PathBuf;

use clap::error::ErrorKind;
use clap::{CommandFactory, Parser};
use sqry_cli::args::Cli;

/// Clap's deep subcommand tree needs more than the default test-thread
/// stack; run the body on 64 MB, as the census test does.
fn on_large_stack<T: Send + 'static>(body: impl FnOnce() -> T + Send + 'static) -> T {
    let joined = std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(body)
        .expect("spawn parse-surface thread")
        .join();
    match joined {
        Ok(value) => value,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

/// The ten argument forms round 1 removed, as a user would type them.
const REMOVED: [&[&str]; 10] = [
    &["sqry", "-s", "main"],
    &["sqry", "--semantic", "main"],
    &["sqry", "--threshold-dangling-refs", "0.05", "main"],
    &["sqry", "--threshold-id-gaps", "0.1", "main"],
    &["sqry", "index", "--no-classpath", "."],
    &["sqry", "update", "--no-classpath"],
    &["sqry", "watch", "--no-classpath"],
    &["sqry", "update", "--no-incremental"],
    &["sqry", "graph", "cross-language", "--min-confidence", "0.5"],
    &["sqry", "alias", "import", "--local", "f.json"],
];

/// The three values outside 0.0 to 1.0 (or not numbers) the orphan
/// threshold refuses, spelled `--flag=value` so a leading minus reaches the
/// value parser instead of being read as a short flag.
const OUT_OF_RANGE: [&str; 3] = ["1.5", "-0.1", "ratio"];

#[test]
fn the_parse_surface_rejects_what_round_1_removed_and_accepts_what_it_added() {
    on_large_stack(|| {
        let mut failed: Vec<String> = Vec::new();

        // Claim 1: every removed form is an unknown argument.
        let mut rejected_as_unknown = 0usize;
        for argv in REMOVED {
            match Cli::try_parse_from(argv) {
                Ok(_) => failed.push(format!("removed form still parses: {argv:?}")),
                Err(err) if err.kind() == ErrorKind::UnknownArgument => rejected_as_unknown += 1,
                Err(err) => failed.push(format!(
                    "removed form rejected for another reason ({:?}): {argv:?}: {err}",
                    err.kind()
                )),
            }
        }
        println!(
            "removed forms rejected as unknown arguments: {rejected_as_unknown} of {}",
            REMOVED.len()
        );

        // Claim 2: the orphan threshold is bounded at parse time.
        match Cli::try_parse_from(["sqry", "--threshold-orphaned-files", "0.6", "main"]) {
            Ok(cli) if cli.threshold_orphaned_files == Some(0.6) => {}
            Ok(cli) => failed.push(format!(
                "--threshold-orphaned-files 0.6 parsed as {:?}",
                cli.threshold_orphaned_files
            )),
            Err(err) => failed.push(format!("--threshold-orphaned-files 0.6 refused: {err}")),
        }
        let mut refused_out_of_range = 0usize;
        for bad in OUT_OF_RANGE {
            let flag = format!("--threshold-orphaned-files={bad}");
            match Cli::try_parse_from(["sqry", flag.as_str(), "main"]) {
                Ok(cli) => failed.push(format!(
                    "{flag} parsed as {:?}; a ratio outside 0.0 to 1.0 must be refused",
                    cli.threshold_orphaned_files
                )),
                Err(err) if err.to_string().contains(bad) => refused_out_of_range += 1,
                Err(err) => failed.push(format!("{flag} refused without naming {bad}: {err}")),
            }
        }
        println!(
            "out-of-range orphan thresholds refused naming the value: {refused_out_of_range} of {}",
            OUT_OF_RANGE.len()
        );

        // Claim 3: the added macro-options arguments parse and carry values.
        let mut added_forms_checked = 0usize;
        match Cli::command().try_get_matches_from(["sqry", "index", "--no-macro-options", "."]) {
            Ok(matches) => {
                let index = matches.subcommand_matches("index");
                let reset =
                    index.and_then(|m| m.try_get_one::<bool>("no_macro_options").ok().flatten());
                if reset == Some(&true) {
                    added_forms_checked += 1;
                } else {
                    failed.push(format!(
                        "index --no-macro-options parsed, but the flag reads {reset:?}"
                    ));
                }
            }
            Err(err) => failed.push(format!("index --no-macro-options refused: {err}")),
        }
        let rebuild_argv = [
            "sqry",
            "daemon",
            "rebuild",
            "--cfg",
            "test",
            "--cfg",
            "feature=serde",
            "--expand-cache",
            "/cache/dir",
            "--no-macro-options",
            "/repo",
        ];
        match Cli::command().try_get_matches_from(rebuild_argv) {
            Ok(matches) => {
                let rebuild = matches
                    .subcommand_matches("daemon")
                    .and_then(|m| m.subcommand_matches("rebuild"));
                let cfg: Option<Vec<String>> = rebuild.and_then(|m| {
                    m.try_get_many::<String>("cfg_flags")
                        .ok()
                        .flatten()
                        .map(|values| values.cloned().collect())
                });
                let cache = rebuild.and_then(|m| {
                    m.try_get_one::<PathBuf>("expand_cache")
                        .ok()
                        .flatten()
                        .cloned()
                });
                let reset =
                    rebuild.and_then(|m| m.try_get_one::<bool>("no_macro_options").ok().flatten());
                let expected_cfg = vec!["test".to_string(), "feature=serde".to_string()];
                if cfg.as_ref() == Some(&expected_cfg)
                    && cache.as_deref() == Some(std::path::Path::new("/cache/dir"))
                    && reset == Some(&true)
                {
                    added_forms_checked += 1;
                } else {
                    failed.push(format!(
                        "daemon rebuild parsed, but carries cfg {cfg:?}, expand cache {cache:?}, \
                         reset {reset:?}"
                    ));
                }
            }
            Err(err) => failed.push(format!(
                "daemon rebuild with the macro flags refused: {err}"
            )),
        }
        match Cli::command().try_get_matches_from(["sqry", "daemon", "rebuild", "/repo"]) {
            Ok(matches) => {
                let rebuild = matches
                    .subcommand_matches("daemon")
                    .and_then(|m| m.subcommand_matches("rebuild"));
                let cfg_count = rebuild
                    .and_then(|m| m.try_get_many::<String>("cfg_flags").ok().flatten())
                    .map_or(0, Iterator::count);
                let reset =
                    rebuild.and_then(|m| m.try_get_one::<bool>("no_macro_options").ok().flatten());
                if cfg_count == 0 && reset == Some(&false) {
                    added_forms_checked += 1;
                } else {
                    failed.push(format!(
                        "daemon rebuild without the flags carries {cfg_count} cfg flag(s) and \
                         reset {reset:?}"
                    ));
                }
            }
            Err(err) => failed.push(format!("daemon rebuild without the flags refused: {err}")),
        }
        println!(
            "added macro-options forms that parse with their values: {added_forms_checked} of 3"
        );

        println!("failed checks: {}", failed.len());
        for failure in &failed {
            println!("  {failure}");
        }
        assert_eq!(rejected_as_unknown, REMOVED.len(), "{failed:#?}");
        assert_eq!(refused_out_of_range, OUT_OF_RANGE.len(), "{failed:#?}");
        assert_eq!(added_forms_checked, 3, "{failed:#?}");
        assert!(failed.is_empty(), "{failed:#?}");
    });
}
