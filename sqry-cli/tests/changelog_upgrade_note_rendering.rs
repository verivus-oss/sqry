//! The changelog must be able to carry a migration instruction.
//!
//! `release-plz.toml` renders `{{ commit.message }}`, which git-cliff resolves
//! to the commit SUBJECT alone. An instruction written in a commit BODY is
//! therefore dropped, which is exactly how the declaration-span change nearly
//! shipped a mandatory `sqry index --force` that no user would ever read: the
//! instruction existed, the changelog did not carry it.
//!
//! This is a test rather than a CI job on purpose. Wiring it into `ci.yml`
//! would put this PR into release-control review scope (see
//! `docs/reviews/release-workflows/sanitization-review-contract.toml`), and a
//! changelog-template guard does not warrant that. `cargo test --workspace`
//! already gates every change.
//!
//! `scripts/ci/check-upgrade-note-rendering.sh` is the same contract for
//! operators, and it additionally renders the template with git-cliff when that
//! binary is available.

use std::path::PathBuf;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("sqry-cli has a parent directory")
        .to_path_buf()
}

/// The entire `[changelog]` table, verbatim.
///
/// Runs from the header to the line before the next top-level table, with
/// trailing blank and comment lines dropped so that an unrelated comment
/// written above the next section does not read as a template change.
/// `release-plz.toml` is release-control tooling and is default-denied by
/// `release-manifest.toml`, so the sanitized OSS mirror has no copy of it.
/// `sqry-cli/tests/` DOES ship, and the mirror runs `cargo test --workspace`
/// through the same `ci.yml`, so panicking on the missing file turns the
/// mirror's CI red on a file that is absent by design.
///
/// Verified against the live mirror rather than inferred: the contents API
/// returns 404 for `release-plz.toml` and lists this directory.
///
/// Skipping is named, not silent, and it is the same shape `ci.yml` already
/// uses for `CLAUDE.md` in the `claude-md-size` job.
fn sanitized_tree_without_release_config() -> bool {
    let absent = !workspace_root().join("release-plz.toml").exists();
    if absent {
        eprintln!(
            "release-plz.toml absent (sanitized tree): skipping the changelog template contract"
        );
    }
    absent
}

fn changelog_section() -> String {
    let path = workspace_root().join("release-plz.toml");
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));

    let mut collecting = false;
    let mut lines: Vec<&str> = Vec::new();
    for line in text.lines() {
        if line.trim_end() == "[changelog]" {
            collecting = true;
            continue;
        }
        if collecting && line.starts_with('[') {
            break;
        }
        if collecting {
            lines.push(line);
        }
    }
    assert!(
        collecting,
        "release-plz.toml must declare a [changelog] section"
    );

    while lines
        .last()
        .is_some_and(|l| l.trim().is_empty() || l.trim_start().starts_with('#'))
    {
        lines.pop();
    }

    let mut section = lines.join("\n");
    section.push('\n');
    section
}

fn changelog_body() -> String {
    let section = changelog_section();

    // The body is the only triple-quoted string in the table, so pulling it out
    // this way avoids a toml dependency in a test that exists to guard one
    // template.
    let body = section
        .split_once("body = \"\"\"")
        .expect("[changelog] must declare a body template")
        .1;
    body.split_once("\"\"\"")
        .expect("the body template must be terminated")
        .0
        .to_string()
}

/// The WHOLE `[changelog]` table, byte for byte.
///
/// Five revisions of this test have now been broken by reviewers, and each time
/// the mutation lived just outside whatever the test happened to pin:
///
///   - substrings only: `==` -> `!=`, `{{ footer.value }}` -> `{{ footer.token }}`,
///     and a whitespace-equivalent rewrite all survived.
///   - a pinned snippet of the footer loop: deleting one dash from the COMMIT
///     loop's `{%- endfor %}` on the next line survived, and reproduced the
///     loose-list regression byte for byte on the real range. All three
///     reviewers found that one independently.
///   - the whole `body`: `commit_parsers` sits OUTSIDE it and decides which
///     commits reach the changelog at all. Flipping `^fix` to `skip = true`
///     deletes every note in a release while the body stays byte-identical.
///
/// The pin has moved outward twice for the same reason, so it now covers the
/// whole table rather than one key inside it. The named tests below still exist
/// so a failure says WHICH property broke rather than just printing a diff.
///
/// Changing this table deliberately means updating this literal. Before you do:
/// render it with
/// `scripts/ci/check-upgrade-note-rendering.sh --range <base>..HEAD`, which
/// applies these parsers and checks the rendered list shape, not just that the
/// words appear.
const EXPECTED_SECTION: &str = r#"protect_breaking_commits = true
body = """
## [{{ version }}]{% if release_link %}({{ release_link }}){% endif %} - {{ timestamp | date(format="%Y-%m-%d") }}
{% for group, commits in commits | group_by(attribute="group") %}
### {{ group | upper_first }}
{% for commit in commits %}
- {% if commit.scope %}*({{ commit.scope }})* {% endif %}{% if commit.breaking %}[**breaking**] {% endif %}{{ commit.message }}
{%- for footer in commit.footers | default(value=[]) %}{% if footer.token == "Upgrade-note" %}
{{ "  " }}- **Upgrade note:** {{ footer.value }}{% endif %}{% endfor %}
{%- endfor %}
{% endfor %}
"""
commit_parsers = [
    { message = "^feat", group = "Added" },
    { message = "^fix", group = "Fixed" },
    { message = "^docs", group = "Documentation" },
    { message = "^perf", group = "Performance" },
    { message = "^refactor", group = "Changed" },
    { message = "^build", group = "Build" },
    { message = "^deps", group = "Dependencies" },
    { message = "^test", skip = true },
    { message = "^chore: release", skip = true },
    { message = "^chore\\(release\\)", skip = true },
    { message = "^chore", group = "Other" },
    { message = "^ci", skip = true },
]
"#;

#[test]
fn changelog_table_matches_the_pinned_section_byte_for_byte() {
    if sanitized_tree_without_release_config() {
        return;
    }
    assert_eq!(
        changelog_section(),
        EXPECTED_SECTION,
        "the [changelog] table changed. Every reviewer-found mutation so far \
         lived outside whatever this test pinned, so it pins the whole table: a \
         diff here is either a deliberate edit (update the literal, after \
         rendering it) or the mutation this test exists to catch."
    );
}

#[test]
fn the_commit_that_carries_a_note_is_not_skipped_by_the_parsers() {
    if sanitized_tree_without_release_config() {
        return;
    }
    // The body can be perfect and still render nothing: if a commit's type is
    // skipped, any note it carries never reaches a user. Named separately from
    // the byte pin so the failure says which property broke.
    //
    // Every type a release can ship a migration instruction on, not just the
    // two that carry one today. An earlier revision checked `^feat` and `^fix`
    // only, so skipping `^perf` while updating the byte pin left it green.
    let section = changelog_section();
    for kind in [
        "^feat",
        "^fix",
        "^perf",
        "^refactor",
        "^docs",
        "^build",
        "^deps",
    ] {
        let entry = section
            .lines()
            .find(|l| l.contains(&format!("message = \"{kind}\"")))
            .unwrap_or_else(|| panic!("commit_parsers must classify {kind} commits"));
        assert!(
            !entry.contains("skip = true"),
            "{kind} commits are skipped, so any Upgrade-note they carry is \
             dropped from the changelog: {entry}"
        );
    }
}

/// The first of two whitespace controls, named so its failure says what broke.
///
/// `{%- for` absorbs the newline between the commit bullet and the footer loop.
/// Written as `{% for`, every bullet gains a blank line and markdown reads the
/// changelog as a loose list.
#[test]
fn the_footer_loop_does_not_loosen_the_commit_list() {
    if sanitized_tree_without_release_config() {
        return;
    }
    let body = changelog_body();
    assert!(
        body.contains("{%- for footer in commit.footers "),
        "the footer loop must open with `{{%- for` so it absorbs the newline \
         after the commit bullet; `{{% for` leaves a blank line after EVERY \
         bullet and makes the changelog a markdown loose list.\n\ntemplate:\n{body}"
    );
}

/// The second whitespace control, and the one that shipped past three reviewers
/// before all three found it in the same round.
///
/// The commits loop's `{%- endfor %}` absorbs the newline after the footer
/// loop, once per commit. Drop that dash and every bullet gains a blank line,
/// with or without a trailer, which is the same loose list from the other end.
#[test]
fn the_commit_loop_close_does_not_loosen_the_commit_list() {
    if sanitized_tree_without_release_config() {
        return;
    }
    let body = changelog_body();
    assert!(
        body.contains("{% endif %}{% endfor %}\n{%- endfor %}"),
        "the commits loop must close with `{{%- endfor %}}` directly after the \
         footer loop: it absorbs the newline the footer loop leaves behind, \
         once per commit. Written as `{{% endfor %}}` every bullet gains a \
         blank line and the changelog becomes a markdown loose \
         list.\n\ntemplate:\n{body}"
    );
}

#[test]
fn the_changelog_body_still_renders_the_commit_itself() {
    if sanitized_tree_without_release_config() {
        return;
    }
    // Guards against "fixing" the assertion above by rendering only footers.
    let body = changelog_body();
    assert!(
        body.contains("commit.message"),
        "the changelog template no longer renders commit messages: {body}"
    );
}

/// The operator-facing check, and its own oracle, run on every PR.
///
/// A reviewer found that `scripts/ci/check-upgrade-note-rendering.sh` was
/// invoked by no workflow: six review rounds built a control that ran only
/// when somebody remembered, guarding a file that is edited by exactly the
/// kind of PR that breaks it.
///
/// The obvious fix, a job in `ci.yml`, is the wrong one here.
/// `.github/workflows/ci.yml` is a hash-pinned covered path in
/// `docs/reviews/release-workflows/sanitization-review-contract.toml`, so
/// editing it drags this change into release-control review scope and
/// re-pinning the hash wrong stalls Stage 1. The module comment above already
/// recorded that decision for the template contract; it applies to the script
/// too.
///
/// `cargo test --workspace` is already the per-PR gate, so the script runs
/// here instead. `scripts/release/` turned out to be pinned by the same
/// contract, so the `--range` orchestration lives in
/// `scripts/ci/check-upgrade-note-range.sh`, which no contract covers. The
/// range form IS run from here, through that wrapper, which self-skips when no
/// `v*` tag is reachable rather than requiring one.
///
/// This asserts WHICH suite the oracle ran, not merely that it exited 0.
/// Without `git-cliff` on PATH the oracle drops to the handful of assertions
/// that need no renderer and still succeeds, so an earlier version of this test
/// reported a passing gate while a fraction of it ran. Both counts are pinned,
/// and full mode is REQUIRED unless `SQRY_ALLOW_REDUCED_CHANGELOG_ORACLE=1`
/// says the renderer is genuinely unavailable. Pinning the counts alone only
/// labelled the problem; it did not stop a partial gate reporting a pass.
#[cfg(unix)]
#[test]
fn the_operator_check_and_its_oracle_pass() {
    if sanitized_tree_without_release_config() {
        return;
    }

    /// Assertions the oracle runs with `git-cliff` available, and without it.
    /// The reduced suite still covers argument handling and the template
    /// contract; only the end-to-end render assertions need the renderer.
    const ORACLE_ASSERTIONS_FULL: u32 = 81;
    const ORACLE_ASSERTIONS_REDUCED: u32 = 6;

    let mut oracle_stdout = String::new();

    for script in [
        "scripts/ci/check-upgrade-note-rendering.sh",
        "scripts/ci/check-upgrade-note-range.sh",
        "scripts/ci/test-check-upgrade-note-rendering.sh",
    ] {
        let path = workspace_root().join(script);
        assert!(
            path.exists(),
            "{script} is missing: the template contract has no operator-facing check"
        );

        let output = std::process::Command::new("bash")
            .arg(&path)
            .current_dir(workspace_root())
            .output()
            .unwrap_or_else(|e| panic!("run {script}: {e}"));

        assert!(
            output.status.success(),
            "{script} failed ({}):\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );

        if script.ends_with("test-check-upgrade-note-rendering.sh") {
            oracle_stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        }
    }

    let mode = oracle_stdout
        .lines()
        .find_map(|l| l.strip_prefix("mode: "))
        .unwrap_or_else(|| {
            panic!("oracle printed no `mode:` line:\n{oracle_stdout}");
        })
        .trim();

    let ran: u32 = oracle_stdout
        .lines()
        .find_map(|l| l.strip_prefix("ok: "))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| {
            panic!("oracle printed no parseable `ok: N` line:\n{oracle_stdout}");
        });

    let expected = match mode {
        "full" => ORACLE_ASSERTIONS_FULL,
        "reduced" => ORACLE_ASSERTIONS_REDUCED,
        other => panic!("oracle reported an unknown mode {other:?}:\n{oracle_stdout}"),
    };

    assert_eq!(
        ran, expected,
        "oracle ran {ran} assertions in {mode} mode, expected {expected}. \
         Adding or removing an oracle case must update the constant in this test."
    );

    // Counting is not gating. `reduced` means git-cliff was absent and nothing
    // rendered, which is the state this whole control exists to prevent, so it
    // is a failure here unless the environment says the renderer is genuinely
    // unavailable. A reviewer measured the old behaviour: with git-cliff off
    // PATH this test passed in 0.20s having rendered nothing.
    if mode == "reduced" {
        assert_eq!(
            std::env::var("SQRY_ALLOW_REDUCED_CHANGELOG_ORACLE").as_deref(),
            Ok("1"),
            "the changelog oracle ran in reduced mode: git-cliff is not on PATH, \
             so none of the end-to-end render assertions executed. Install \
             git-cliff, or set SQRY_ALLOW_REDUCED_CHANGELOG_ORACLE=1 to accept a \
             gate that does not render."
        );
    }
}

/// Neither shipped wrapper may reach the oracle-only escape hatch.
///
/// `--test-only-skip-fold-guard` disables the single-line guard so the oracle
/// can still drive the downstream shape rules with fixtures that the guard now
/// refuses before rendering. It is doubly fenced: the validator rejects it
/// unless `SQRY_UPGRADE_NOTE_ORACLE=1` is also set, and nothing but the oracle
/// sets that. This asserts the second fence, that no shipped caller passes the
/// flag, because a control with a documented bypass is only as good as the
/// evidence that the bypass is unreachable in production.
#[cfg(unix)]
#[test]
fn no_shipped_wrapper_passes_the_fold_guard_escape_hatch() {
    if sanitized_tree_without_release_config() {
        return;
    }

    for script in [
        "scripts/ci/check-upgrade-note-range.sh",
        "scripts/ci/check-upgrade-note-rendering.sh",
    ] {
        let path = workspace_root().join(script);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let uses = text
            .lines()
            .filter(|l| l.contains("--test-only-skip-fold-guard"))
            .filter(|l| !l.trim_start().starts_with('#'))
            .filter(|l| !l.contains("SQRY_UPGRADE_NOTE_ORACLE"))
            .collect::<Vec<_>>();
        // The rendering script declares the flag in its own argument parser;
        // that declaration names the env fence on the following lines and is
        // filtered above. Any other live mention is a caller passing it.
        assert!(
            uses.iter()
                .all(|l| l.contains("--test-only-skip-fold-guard)")),
            "{script} passes or forwards the oracle-only escape hatch: {uses:?}"
        );
    }
}
