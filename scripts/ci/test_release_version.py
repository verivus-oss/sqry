#!/usr/bin/env python3
"""Fixture corpus for check_release_version.py.

The bug this gate exists for was invisible for one reason: the old check read
one token per file and reported a tick for the rest. So the cases here are
mostly about the SECOND occurrence and about the file set not quietly shrinking.
"""

from __future__ import annotations

import os
import pathlib
import shutil
import subprocess
import sys
import tempfile

GATE = pathlib.Path(__file__).resolve().parent / "check_release_version.py"
EXPECTED_CASES = 14

CARGO = """[workspace]
members = []

[workspace.package]
version = "1.2.3"
edition = "2024"
"""
SKILL = """---
name: %s
version: %s
---
Use this skill with sqry v%s for semantic code search.
"""
PLUGIN = '{\n  "name": "sqry",\n  "version": "%s"\n}\n'


def git(repo: pathlib.Path, *args: str) -> None:
    r = subprocess.run(["git", *args], cwd=repo, check=False,
                       stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    if r.returncode != 0:
        raise RuntimeError(f"fixture setup failed: git {' '.join(args)}: "
                           f"{r.stderr.decode('utf-8','replace').strip()}")


def write(path: pathlib.Path, text: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")


def mkfixture(tmp: pathlib.Path, name: str, field="1.2.3", prose="1.2.3") -> pathlib.Path:
    repo = tmp / name
    if repo.exists():
        shutil.rmtree(repo)
    repo.mkdir(parents=True)
    write(repo / "Cargo.toml", CARGO)
    for skill in ("alpha", "beta"):
        write(repo / "agent-skills/skills" / skill / "SKILL.md", SKILL % (skill, field, prose))
    write(repo / "agent-skills/.claude-plugin/plugin.json", PLUGIN % field)
    git(repo, "init", "-q", "-b", "master")
    git(repo, "config", "user.email", "t@t")
    git(repo, "config", "user.name", "t")
    git(repo, "add", "-A")
    git(repo, "commit", "-qm", "fixture")
    return repo


def run(repo: pathlib.Path, *args: str, cwd: pathlib.Path | None = None):
    r = subprocess.run([sys.executable, str(GATE), *args], cwd=cwd or repo,
                       stdout=subprocess.PIPE, stderr=subprocess.STDOUT, check=False)
    return r.returncode, r.stdout.decode("utf-8", "replace")


class H:
    def __init__(self): self.p = self.f = self.ran = 0
    def ok(self, l): print(f"  PASS  {l}"); self.p += 1; self.ran += 1
    def no(self, l, w): print(f"  FAIL  {l}: {w}"); self.f += 1; self.ran += 1
    def expect(self, l, repo, rc_want, needle=None, cwd=None):
        rc, out = run(repo, cwd=cwd)
        if rc != rc_want:
            return self.no(l, f"rc={rc}, wanted {rc_want}")
        if needle and needle not in out:
            return self.no(l, f"never said {needle!r}")
        self.ok(l)


def main() -> int:
    h = H()
    print("release-version harness\n")
    tmp = pathlib.Path(tempfile.mkdtemp(prefix="relver-"))
    try:
        h.expect("(P1) every document names the release", mkfixture(tmp, "clean"), 0, "PASS")

        # The whole point: the field is current, the PROSE is not.
        h.expect("(F1) prose stale while the version field is current",
                 mkfixture(tmp, "prose", field="1.2.3", prose="1.0.0"), 1, "names v1.0.0")

        h.expect("(F2) the version field itself stale",
                 mkfixture(tmp, "field", field="1.0.0", prose="1.2.3"), 1,
                 "declares version 1.0.0")

        repo = mkfixture(tmp, "plugin")
        write(repo / "agent-skills/.claude-plugin/plugin.json", PLUGIN % "9.9.9")
        h.expect("(F3) the plugin manifest stale", repo, 1,
                 "plugin.json declares version 9.9.9")

        repo = mkfixture(tmp, "fixable", field="1.2.3", prose="1.0.0")
        rc, _ = run(repo, "--fix")
        left = subprocess.run(["grep", "-rho", "1.0.0", "agent-skills"], cwd=repo,
                              capture_output=True, text=True).stdout.split()
        if rc == 0 and not left:
            h.ok("(P2) --fix rewrites every occurrence, not just the first")
        else:
            h.no("(P2) --fix rewrites every occurrence, not just the first",
                 f"rc={rc}, {len(left)} stale token(s) left")

        # A skill added later must be covered without editing the gate.
        repo = mkfixture(tmp, "newskill")
        write(repo / "agent-skills/skills/gamma/SKILL.md", SKILL % ("gamma", "1.2.3", "0.0.1"))
        git(repo, "add", "-A"); git(repo, "commit", "-qm", "gamma")
        h.expect("(F4) a newly added skill is covered by the derived set", repo, 1,
                 "gamma/SKILL.md names v0.0.1")

        # An untracked skill is not shipped, so it is not this gate's business.
        repo = mkfixture(tmp, "untracked")
        write(repo / "agent-skills/skills/delta/SKILL.md", SKILL % ("delta", "0.0.1", "0.0.1"))
        h.expect("(P3) an untracked skill is not a shipped document", repo, 0, "PASS")

        # An empty derived set is a broken derivation, not a clean tree.
        repo = mkfixture(tmp, "empty")
        git(repo, "rm", "-rq", "--cached", "agent-skills")
        git(repo, "commit", "-qm", "empty")
        h.expect("(F5) an empty pinned set is refused", repo, 1,
                 "0 release-pinned documents")

        # A pinned document the gate cannot read is a document it cannot
        # police. Reporting it without counting it exits 0.
        repo = mkfixture(tmp, "unreadable")
        target = repo / "agent-skills/skills/alpha/SKILL.md"
        os.chmod(target, 0o000)
        try:
            h.expect("(F7) an unreadable pinned document fails, not just warns", repo, 1,
                     "cannot read")
        finally:
            os.chmod(target, 0o644)

        # A pinned document with NO version is unpinned, not clean. Zero stale
        # out of zero tokens took the success branch, so an empty SKILL.md
        # passed this gate and the parity gate together.
        repo = mkfixture(tmp, "noversion")
        write(repo / "agent-skills/skills/alpha/SKILL.md", "")
        h.expect("(F8) a pinned document with no version at all", repo, 1,
                 "has no frontmatter block")

        # The field deleted while the PROSE still names the release. This is
        # the case the old (F9) label claimed and did not build: it stripped the
        # prose too, so it passed for a gate that only counted version-shaped
        # bytes. A six-byte document cleared that gate and the parity gate.
        repo = mkfixture(tmp, "field_deleted_prose_kept")
        write(repo / "agent-skills/skills/alpha/SKILL.md",
              "---\nname: alpha\n---\nUse this skill with sqry v1.2.3.\n")
        h.expect("(F9) the version field deleted while the prose still names it",
                 repo, 1, "carries no `version:` frontmatter field")

        # The whole document is only version-shaped bytes.
        repo = mkfixture(tmp, "sixbytes")
        write(repo / "agent-skills/skills/alpha/SKILL.md", "1.2.3")
        h.expect("(F10) a document that is nothing but a version token", repo, 1,
                 "has no frontmatter block")

        # A version inside a fenced block or a URL is prose, not a declaration.
        repo = mkfixture(tmp, "fenced")
        write(repo / "agent-skills/skills/alpha/SKILL.md",
              "---\nname: alpha\n---\n```\nversion: 1.2.3\n```\n")
        h.expect("(F11) a version only inside a fenced code block", repo, 1,
                 "carries no `version:` frontmatter field")

        repo = mkfixture(tmp, "subdir")
        h.expect("(F6) invoked from a subdirectory, the gate refuses", repo, 1,
                 "run this from the repository root", cwd=repo / "agent-skills")
    finally:
        shutil.rmtree(tmp, ignore_errors=True)

    print(f"\nrelease-version harness: {h.p} passed, {h.f} failed")
    if h.ran != EXPECTED_CASES:
        print(f"release-version harness: FAIL ({h.ran} cases ran, {EXPECTED_CASES} declared)")
        return 1
    if h.f:
        print("release-version harness: FAIL")
        return 1
    print("release-version harness: PASS")
    return 0


if __name__ == "__main__":
    sys.exit(main())
