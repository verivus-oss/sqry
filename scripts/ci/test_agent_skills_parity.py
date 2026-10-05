#!/usr/bin/env python3
"""Fixture corpus for check_agent_skills_parity.py.

WHY THIS EXISTS. That gate was a shell script through four rewrites, each one
closing fail-opens found by hand in review and introducing more. Three external
reviewers and three validation passes found seventeen distinct routes to "PASS
on a wrong tree". Nothing in the repository would have caught any of them,
because the gate had no harness while the other CI scripts here do.

Every case builds a throwaway git repository, so the real one is never touched.

A rejection case asserts the MESSAGE, not merely a non-zero exit. Six cases in
the shell version of this corpus asserted the bare string "FAIL", which the
final verdict line prints on every rejection, so those six had collapsed to
"exit non-zero" and a mutation that broke the behaviour they were named for
survived. The total case count is asserted too: without that, deleting a case
left the harness green.
"""

from __future__ import annotations

import os
import pathlib
import shutil
import subprocess
import sys
import tempfile

HERE = pathlib.Path(__file__).resolve().parent
GATE = HERE / "check_agent_skills_parity.py"

# Bump deliberately when adding or removing a case. A harness that quietly runs
# fewer checks than it did yesterday is the failure this number exists to catch.
EXPECTED_CASES = 63

SRC = b"---\nname: %s\nversion: 9.9.9\n---\n%s body\n"
LOCAL = b"---\nname: local-tool\n---\na contributor skill, not a copy\n"


def git(repo: pathlib.Path, *args: str) -> None:
    """Run git in a fixture, and say what happened when it fails.

    stderr used to go to DEVNULL, so a fixture that failed to build produced a
    CalledProcessError with no cause attached and took the rest of the run with
    it. A harness that cannot say why it died costs someone a bisect.
    """
    result = subprocess.run(
        ["git", *args], cwd=repo, check=False,
        stdout=subprocess.DEVNULL, stderr=subprocess.PIPE,
    )
    if result.returncode != 0:
        raise RuntimeError(
            f"fixture setup failed: git {' '.join(args)} in {repo} exited "
            f"{result.returncode}: {result.stderr.decode('utf-8', 'replace').strip()}"
        )


def write(path: pathlib.Path | bytes, data: bytes) -> None:
    p = path if isinstance(path, bytes) else os.fsencode(str(path))
    os.makedirs(os.path.dirname(p), exist_ok=True)
    with open(p, "wb") as handle:
        handle.write(data)


def mkfixture(tmp: pathlib.Path, name: str) -> pathlib.Path:
    """Two sources, both copied into the one copy root, plus a local skill.

    The local skill is load-bearing: .claude/skills/ legitimately holds
    contributor skills, and a gate that swept them in would be unusable.
    """
    repo = tmp / name
    if repo.exists():
        shutil.rmtree(repo)
    repo.mkdir(parents=True)
    for skill in (b"alpha", b"beta"):
        body = SRC % (skill, skill)
        write(repo / "agent-skills" / "skills" / skill.decode() / "SKILL.md", body)
        write(repo / ".claude" / "skills" / skill.decode() / "SKILL.md", body)
    write(repo / ".claude" / "skills" / "local-tool" / "SKILL.md", LOCAL)
    write(repo / ".gitignore", b"benchmarks/swebench/image/skills/\n")
    # -b, because a host with init.defaultBranch=main otherwise made the
    # unmerged-index case check out a branch that does not exist, and the
    # traceback skipped every later case and the total assertion with it.
    git(repo, "init", "-q", "-b", "master")
    git(repo, "config", "user.email", "t@t")
    git(repo, "config", "user.name", "t")
    git(repo, "add", "-A")
    git(repo, "commit", "-qm", "fixture")
    return repo


def stage_generated(repo: pathlib.Path) -> None:
    """What prepare.sh leaves on disk: generated, untracked, gitignored."""
    body = (repo / "agent-skills/skills/alpha/SKILL.md").read_bytes()
    for agent in ("claude", "codex", "gemini"):
        write(repo / "benchmarks/swebench/image/skills" / agent / "alpha" / "SKILL.md", body)


def run_gate(repo: pathlib.Path) -> tuple[int, str]:
    result = subprocess.run(
        [sys.executable, str(GATE)], cwd=repo,
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT, check=False,
    )
    return result.returncode, result.stdout.decode("utf-8", "replace")


class Harness:
    def __init__(self) -> None:
        self.passed = 0
        self.failed = 0
        self.skipped = 0
        self.ran = 0

    def ok(self, label: str) -> None:
        print(f"  PASS  {label}")
        self.passed += 1
        self.ran += 1

    def no(self, label: str, why: str) -> None:
        print(f"  FAIL  {label}: {why}")
        self.failed += 1
        self.ran += 1

    def declare(self, why: str) -> None:
        """A gap named on purpose, so the suite cannot be read as covering it."""
        print(f"  SKIP  {why}")
        self.skipped += 1
        self.ran += 1

    def must_pass(self, label: str, repo: pathlib.Path) -> None:
        rc, out = run_gate(repo)
        if rc == 0:
            self.ok(label)
        else:
            first = next((l for l in out.splitlines() if "FAIL" in l), out.strip())
            self.no(label, f"gate REJECTED a correct tree: {first[:100]}")

    def must_fail(self, label: str, repo: pathlib.Path, want: str) -> None:
        rc, out = run_gate(repo)
        if rc == 0:
            self.no(label, "gate PASSED a tree it must reject")
        elif want not in out:
            first = next((l for l in out.splitlines() if "FAIL" in l), out.strip())
            self.no(label, f"rejected but never said {want!r}; said: {first[:100]}")
        else:
            self.ok(label)


def main() -> int:
    if not GATE.is_file():
        print(f"FATAL: no gate at {GATE}", file=sys.stderr)
        return 1
    h = Harness()
    print("agent-skills parity harness\n")
    tmp = pathlib.Path(tempfile.mkdtemp(prefix="parity-corpus-"))
    try:
        cases(h, tmp)
    finally:
        shutil.rmtree(tmp, ignore_errors=True)

    print(f"\nparity harness: {h.passed} passed, {h.failed} failed, {h.skipped} skipped")
    if h.ran != EXPECTED_CASES:
        print(
            f"agent-skills parity harness: FAIL "
            f"({h.ran} cases ran, {EXPECTED_CASES} declared)"
        )
        return 1
    if h.failed:
        print("agent-skills parity harness: FAIL")
        return 1
    print("agent-skills parity harness: PASS")
    return 0


def cases(h: Harness, tmp: pathlib.Path) -> None:
    # ---- trees that must be accepted --------------------------------------
    repo = mkfixture(tmp, "pristine")
    rc, out = run_gate(repo)
    expect = "2 source(s), 2 of 2 copy/copies compared, 0 missing"
    if rc == 0 and expect in out:
        h.ok("(P1) pristine fixture, with the counters it must report")
    else:
        h.no("(P1) pristine fixture, with the counters it must report",
             f"rc={rc}; wanted {expect!r}; got: {out.strip().splitlines()[0] if out.strip() else '(nothing)'}")

    repo = mkfixture(tmp, "mirror")
    git(repo, "rm", "-rq", "--cached", ".claude")
    shutil.rmtree(repo / ".claude")
    git(repo, "commit", "-qm", "mirror")
    rc, out = run_gate(repo)
    if rc == 0 and "no copy tree in this checkout (sanitized tree)" in out:
        h.ok("(P2) sanitized mirror shape, and the MIRROR arm is what passed it")
    else:
        h.no("(P2) sanitized mirror shape, and the MIRROR arm is what passed it",
             f"rc={rc}; the mirror line was "
             f"{'present' if 'sanitized tree' in out else 'MISSING'}")

    repo = mkfixture(tmp, "localdrift")
    write(repo / ".claude/skills/local-tool/SKILL.md", b"rewritten by its owner\n")
    git(repo, "commit", "-qam", "localdrift")
    h.must_pass("(P3) a local skill with no source may drift freely", repo)

    repo = mkfixture(tmp, "foreign")
    write(repo / "skills/alpha/SKILL.md", b"---\nname: alpha\n---\ndifferent artifact\n")
    git(repo, "add", "-A"); git(repo, "commit", "-qm", "foreign")
    h.must_pass("(P4) a foreign artifact sharing a source name is not a copy", repo)

    repo = mkfixture(tmp, "newsource")
    write(repo / "agent-skills/skills/gamma/SKILL.md", SRC % (b"gamma", b"gamma"))
    git(repo, "add", "-A"); git(repo, "commit", "-qm", "gamma")
    h.must_pass("(P5) a new source with no copies yet", repo)

    repo = mkfixture(tmp, "generated")
    stage_generated(repo)
    h.must_pass("(P6) generated, untracked staged skills are invisible", repo)

    # A skill directory named "-e" was passed to grep as an option by the shell
    # version, grep exited 2, and the name-match predicate returned false.
    repo = mkfixture(tmp, "dashname")
    body = SRC % (b"-e", b"-e")
    write(repo / "agent-skills/skills/-e/SKILL.md", body)
    write(repo / ".claude/skills/-e/SKILL.md", body)
    git(repo, "add", "-A"); git(repo, "commit", "-qm", "dashname")
    h.must_pass("(P7) a skill named -e is matched, not read as an option", repo)

    # The shell version's verdict flipped with core.quotePath, because it parsed
    # git's quoted output. This must be decided by the tree, not by config.
    # core.quotePath only changes git's output for non-ASCII or control bytes,
    # so this must carry such a path or it compares two identical runs and
    # asserts nothing beyond (P1). The tree is WRONG on purpose: a misplaced
    # copy at a non-ASCII path must be rejected under either setting.
    repo = mkfixture(tmp, "quotepath")
    dest = os.fsencode(str(repo)) + b"/skills/revi\xc3\xb6w/alpha"
    os.makedirs(dest, exist_ok=True)
    os.rename(os.fsencode(str(repo / ".claude/skills/alpha/SKILL.md")), dest + b"/SKILL.md")
    os.rmdir(os.fsencode(str(repo / ".claude/skills/alpha")))
    git(repo, "add", "-A")
    git(repo, "commit", "-qm", "quotepath")
    verdicts = {}
    for setting in ("true", "false"):
        git(repo, "config", "core.quotePath", setting)
        rc, out = run_gate(repo)
        verdicts[setting] = (rc, "outside the copy root" in out)
    if verdicts["true"] == verdicts["false"] == (1, True):
        h.ok("(P8) a non-ASCII misplaced copy is rejected under either core.quotePath")
    else:
        h.no("(P8) a non-ASCII misplaced copy is rejected under either core.quotePath",
             f"quotePath=true -> {verdicts['true']}, false -> {verdicts['false']}")

    # ---- trees that must be rejected --------------------------------------
    repo = mkfixture(tmp, "drift")
    write(repo / ".claude/skills/alpha/SKILL.md",
          b"---\nname: alpha\nversion: 9.9.9\n---\ndrifted body\n")
    git(repo, "commit", "-qam", "drift")
    h.must_fail("(F1) a copy diverged from its source", repo, "diverged from source")

    repo = mkfixture(tmp, "copygone")
    (repo / ".claude/skills/alpha/SKILL.md").unlink()
    h.must_fail("(F2) a tracked copy is missing from disk", repo, "but it is not on disk")

    repo = mkfixture(tmp, "srcgone")
    (repo / "agent-skills/skills/alpha/SKILL.md").unlink()
    h.must_fail("(F3) a tracked source is missing from disk", repo, "tracks source")

    repo = mkfixture(tmp, "srcuncached")
    git(repo, "rm", "-q", "--cached", "agent-skills/skills/alpha/SKILL.md")
    git(repo, "commit", "-qm", "uncached")
    h.must_fail("(F4) a source left the index, its copy remains", repo, "orphaned copy")

    repo = mkfixture(tmp, "srcrenamed")
    git(repo, "mv", "agent-skills/skills/alpha", "agent-skills/skills/alpha2")
    git(repo, "commit", "-qm", "renamed")
    h.must_fail("(F5) a source directory renamed, its copy orphaned", repo, "orphaned copy")

    repo = mkfixture(tmp, "copyrenamed")
    git(repo, "mv", ".claude/skills/alpha", ".claude/skills/alpha-old")
    git(repo, "commit", "-qm", "copyrenamed")
    h.must_fail("(F6) a copy directory renamed under the copy root", repo, "orphaned copy")

    repo = mkfixture(tmp, "copymoved")
    (repo / "docs/vendored").mkdir(parents=True)
    git(repo, "mv", ".claude/skills/alpha", "docs/vendored/alpha")
    git(repo, "commit", "-qm", "moved")
    h.must_fail("(F7) a copy moved outside the copy root", repo, "outside the copy root")

    repo = mkfixture(tmp, "intosrc")
    (repo / "agent-skills/skills/staged").mkdir(parents=True)
    git(repo, "mv", ".claude/skills/beta", "agent-skills/skills/staged/beta")
    git(repo, "commit", "-qm", "intosrc")
    h.must_fail("(F8) a copy moved inside the source tree", repo,
                "sits inside the source tree but is not a source")

    repo = mkfixture(tmp, "rootrenamed")
    git(repo, "mv", ".claude/skills", ".claude/agent-skills")
    git(repo, "commit", "-qm", "rootrenamed")
    h.must_fail("(F9) the copy root renamed", repo, "outside the copy root")

    repo = mkfixture(tmp, "rootgone")
    shutil.rmtree(repo / ".claude/skills")
    h.must_fail("(F10) the copy root deleted from disk", repo, "but it is not on disk")

    repo = mkfixture(tmp, "stripped")
    write(repo / ".claude/skills/alpha/SKILL.md",
          b"---\nname: alpha\n---\nmarker stripped and body replaced\n")
    git(repo, "commit", "-qam", "stripped")
    h.must_fail("(F11) a copy with its version marker stripped", repo,
                "diverged from source")

    repo = mkfixture(tmp, "truncated")
    write(repo / ".claude/skills/alpha/SKILL.md", b"")
    h.must_fail("(F12) a copy truncated to an empty file", repo, "diverged from source")

    repo = mkfixture(tmp, "pusheddown")
    filler = b"".join(b"filler %d\n" % i for i in range(40))
    write(repo / ".claude/skills/alpha/SKILL.md",
          b"---\nname: alpha\n" + filler + b"version: 9.9.9\n---\nbody\n")
    git(repo, "commit", "-qam", "pusheddown")
    h.must_fail("(F13) a copy whose marker sits far down the file", repo,
                "diverged from source")

    repo = mkfixture(tmp, "longline")
    write(repo / ".claude/skills/alpha/SKILL.md",
          b"---\nname: alpha\nversion: 9.9.9\n---\n" + b"x" * 400000 + b"\n")
    git(repo, "commit", "-qam", "longline")
    h.must_fail("(F14) a divergent copy holding a very long line", repo,
                "diverged from source")

    repo = mkfixture(tmp, "untracked")
    write(repo / ".claude/skills/gamma/SKILL.md",
          b"---\nname: gamma\nversion: 9.9.9\n---\nstale untracked copy\n")
    h.must_fail("(F15) an untracked version-marked copy under the copy root", repo,
                "untracked skill copy")

    repo = mkfixture(tmp, "symlink_index")
    target = repo / ".claude/skills/alpha/SKILL.md"
    target.unlink()
    target.symlink_to("../../../agent-skills/skills/alpha/SKILL.md")
    git(repo, "add", "-A"); git(repo, "commit", "-qm", "symlink")
    h.must_fail("(F16) a copy replaced by a symlink and staged", repo, "tracked symlink")

    repo = mkfixture(tmp, "symlink_disk")
    target = repo / ".claude/skills/alpha/SKILL.md"
    target.unlink()
    target.symlink_to("../../../agent-skills/skills/alpha/SKILL.md")
    h.must_fail("(F17) a path tracked as a file but a symlink on disk", repo,
                "is a symlink on disk")

    repo = mkfixture(tmp, "forged_mirror")
    git(repo, "mv", ".claude", "dot-claude")
    for skill in ("alpha", "beta"):
        p = repo / "dot-claude/skills" / skill / "SKILL.md"
        p.write_bytes(p.read_bytes().replace(b"version: 9.9.9\n", b""))
    git(repo, "add", "-A"); git(repo, "commit", "-qm", "forged")
    h.must_fail("(F18) the internal tree cannot forge the sanitized-tree verdict", repo,
                "the index holds")

    repo = mkfixture(tmp, "multidrift")
    for skill in ("alpha", "beta"):
        write(repo / ".claude/skills" / skill / "SKILL.md",
              b"---\nname: x\nversion: 9.9.9\n---\ndrifted " + skill.encode() + b"\n")
    git(repo, "commit", "-qam", "multidrift")
    rc, out = run_gate(repo)
    n = out.count("diverged from source")
    if rc != 0 and n == 2 and "agent-skills parity: FAIL" in out:
        h.ok("(F19) both divergences reported, with the summary line")
    else:
        h.no("(F19) both divergences reported, with the summary line",
             f"rc={rc}, {n} of 2 reported, summary "
             f"{'present' if 'parity: FAIL' in out else 'MISSING'}")

    for label, segment, case in (
        ("(F20) relocated into a path containing a space", b"review probe", "spaced"),
        ("(F24) relocated into a path with a non-ASCII byte", b"revi\xc3\xb6w", "unicode"),
        ("(F25) relocated into a path containing a newline", b"re\nview", "newline"),
        ("(F26) relocated into a path containing a tab", b"re\tview", "tabbed"),
    ):
        repo = mkfixture(tmp, case)
        dest = os.fsencode(str(repo)) + b"/skills/" + segment + b"/alpha"
        os.makedirs(dest, exist_ok=True)
        src = os.fsencode(str(repo / ".claude/skills/alpha/SKILL.md"))
        os.rename(src, dest + b"/SKILL.md")
        os.rmdir(os.fsencode(str(repo / ".claude/skills/alpha")))
        git(repo, "add", "-A"); git(repo, "commit", "-qm", case)
        h.must_fail(label, repo, "outside the copy root")

    repo = mkfixture(tmp, "intoskills")
    (repo / "skills").mkdir()
    git(repo, "mv", ".claude/skills/alpha", "skills/alpha")
    git(repo, "commit", "-qm", "intoskills")
    h.must_fail("(F21) a version-marked copy parked under a top-level skills/ tree",
                repo, "outside the copy root")

    repo = mkfixture(tmp, "longlineorphan")
    git(repo, "mv", ".claude/skills/alpha", ".claude/skills/alpha-renamed")
    write(repo / ".claude/skills/alpha-renamed/SKILL.md",
          b"---\nname: alpha\nversion: 9.9.9\n---\n" + b"x" * 400000 + b"\n")
    git(repo, "commit", "-qam", "longlineorphan")
    h.must_fail("(F22) an orphaned copy holding a very long line", repo, "orphaned copy")

    repo = mkfixture(tmp, "recommitted")
    stage_generated(repo)
    git(repo, "add", "-f", "benchmarks/swebench/image/skills")
    git(repo, "commit", "-qm", "recommitted")
    h.must_fail("(F23) a generated staged copy force-added back into the index", repo,
                "outside the copy root")

    # An orphan named -e: the grep-option bug was fail-open outside the root and
    # produced a wrong message inside it.
    repo = mkfixture(tmp, "dashorphan")
    write(repo / ".claude/skills/-e/SKILL.md", SRC % (b"-e", b"-e"))
    git(repo, "add", "-A"); git(repo, "commit", "-qm", "dashorphan")
    h.must_fail("(F27) an orphan named -e is reported, not swallowed by grep", repo,
                "orphaned copy")

    # The working tree matches but the index does not, so what ships differs
    # from what a local consumer reads.
    repo = mkfixture(tmp, "indexdrift")
    p = repo / ".claude/skills/alpha/SKILL.md"
    original = p.read_bytes()
    write(p, b"---\nname: alpha\nversion: 9.9.9\n---\nstaged drift\n")
    git(repo, "add", ".claude/skills/alpha/SKILL.md")
    write(p, original)
    h.must_fail("(F28) the staged copy differs from the staged source", repo,
                "staged copy differs from staged source")

    # M2: reading the marker from DISK instead of the index. The index blob
    # carries version:, the working copy has had it stripped. Reading disk
    # demotes a real orphan to a local skill and the tree passes.
    repo = mkfixture(tmp, "markerindexonly")
    git(repo, "mv", ".claude/skills/alpha", ".claude/skills/alpha-orphan")
    git(repo, "commit", "-qm", "orphaned")
    p_orphan = repo / ".claude/skills/alpha-orphan/SKILL.md"
    write(p_orphan, p_orphan.read_bytes().replace(b"version: 9.9.9\n", b""))
    h.must_fail("(F30) an orphan whose marker survives only in the index", repo,
                "orphaned copy")

    # M7: mirror-shaped (no copy root, nothing outside the source tree) but a
    # failure is already standing. The mirror arm must not print PASS over it.
    repo = mkfixture(tmp, "mirror_with_failure")
    git(repo, "rm", "-rq", "--cached", ".claude")
    shutil.rmtree(repo / ".claude")
    git(repo, "commit", "-qm", "mirror")
    (repo / "agent-skills/skills/alpha/SKILL.md").unlink()
    h.must_fail("(F31) a mirror-shaped tree with a missing source still fails", repo,
                "tracks source")

    # M8: a copy detected but never compared. The symlink arm returns before the
    # comparison, so the population and the compared count disagree, and that
    # reconciliation is the only thing that says so.
    repo = mkfixture(tmp, "detected_not_compared")
    target = repo / ".claude/skills/alpha/SKILL.md"
    target.unlink()
    target.symlink_to("../../../agent-skills/skills/alpha/SKILL.md")
    h.must_fail("(F32) a detected copy that was never compared is reported", repo,
                "copy/copies detected but")

    # M20: an unmerged index. Stages 1/2/3 make "the" object id for a path
    # meaningless, so the gate must refuse rather than pick one.
    repo = mkfixture(tmp, "unmerged")
    git(repo, "checkout", "-q", "-b", "side")
    write(repo / ".claude/skills/alpha/SKILL.md", b"---\nname: alpha\nversion: 9.9.9\n---\nside\n")
    git(repo, "commit", "-qam", "side")
    git(repo, "checkout", "-q", "master")
    write(repo / ".claude/skills/alpha/SKILL.md", b"---\nname: alpha\nversion: 9.9.9\n---\nmain\n")
    git(repo, "commit", "-qam", "main")
    subprocess.run(["git", "merge", "side"], cwd=repo, check=False,
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    h.must_fail("(F33) an unmerged index is refused, not silently resolved", repo,
                "unmerged at stage")

    # --- cases closing mutation survivors an adversarial pass demonstrated ----

    # A path handed to a git REVISION parser rather than passed as data. git's
    # ":<stage>:<path>" grammar eats a leading 0:, so a source named 0:alpha
    # made the marker read answer about a different file, or fail and be
    # swallowed. Addressed by blob id now, which needs no parsing.
    repo = mkfixture(tmp, "revshaped")
    body = SRC % (b"revshaped", b"revshaped")
    write(repo / "agent-skills/skills/0:alpha/SKILL.md", body)
    write(repo / ".claude/skills/0:alpha/SKILL.md", body)
    write(repo / "0:alpha/SKILL.md", b"---\nname: x\nversion: 9.9.9\n---\nmisplaced\n")
    git(repo, "add", "-A"); git(repo, "commit", "-qm", "revshaped")
    h.must_fail("(F34) a misplaced copy whose source name looks like a git revision",
                repo, "outside the copy root")

    # An untracked symlinked ALIAS of a sibling skill. A single global set of
    # visited inodes suppressed it entirely: that is alias suppression, not
    # loop protection, and a local agent session loads the alias by its own
    # name. Two aliases must BOTH be named, or the verdict depends on
    # directory iteration order.
    repo = mkfixture(tmp, "aliases")
    write(repo / ".claude/skills/gamma/SKILL.md",
          b"---\nname: gamma\nversion: 9.9.9\n---\nstale untracked\n")
    os.symlink("gamma", repo / ".claude/skills/aaa")
    os.symlink("gamma", repo / ".claude/skills/bbb")
    rc, out = run_gate(repo)
    named = sum(1 for alias in ("aaa", "bbb")
                if f".claude/skills/{alias}/SKILL.md is an untracked skill copy" in out)
    if rc != 0 and named == 2:
        h.ok("(F35) every symlinked alias of a stale skill is named, not just the first")
    else:
        h.no("(F35) every symlinked alias of a stale skill is named, not just the first",
             f"rc={rc}, {named} of 2 aliases named")

    # A genuine loop must terminate rather than recurse forever, and must not
    # be confused with an alias.
    repo = mkfixture(tmp, "loop")
    os.symlink("..", repo / ".claude/skills/self")
    h.must_pass("(P9) a symlink loop under the copy root terminates without a false report", repo)

    # A tracked SOURCE replaced by a symlink on disk. The release sanitizer
    # walks the disk and dies on the first symlink, so this gate checks the
    # disk too, and nothing exercised that arm for sources.
    repo = mkfixture(tmp, "srcsymlink")
    target = repo / "agent-skills/skills/alpha/SKILL.md"
    target.unlink()
    target.symlink_to("../../../.claude/skills/alpha/SKILL.md")
    h.must_fail("(F36) a source replaced by a symlink on disk", repo, "is a symlink on disk")

    # The copy root present on disk while every copy has left the index. The
    # population is empty but the tree is not a mirror, so the derivation is
    # broken rather than the tree being clean.
    repo = mkfixture(tmp, "rootnocopies")
    git(repo, "rm", "-rq", "--cached", ".claude/skills")
    git(repo, "commit", "-qm", "uncached")
    h.must_fail("(F37) the copy root is present but holds no tracked copies", repo,
                "0 copies were detected")

    # The copy root replaced by a regular FILE. `is_dir()` is the right test;
    # `exists()` would call this a present root and then find nothing in it.
    repo = mkfixture(tmp, "rootisfile")
    shutil.rmtree(repo / ".claude/skills")
    write(repo / ".claude/skills", b"not a directory\n")
    h.must_fail("(F38) the copy root replaced by a regular file", repo, "FAIL")

    # A version-marked file outside the root whose name matches NO source is
    # somebody else's artifact. Dropping the name half of that rule would
    # police the whole repository.
    repo = mkfixture(tmp, "foreignversioned")
    write(repo / "vendor/unrelated/SKILL.md",
          b"---\nname: unrelated\nversion: 1.0.0\n---\nnot ours\n")
    git(repo, "add", "-A"); git(repo, "commit", "-qm", "foreignversioned")
    h.must_pass("(P10) a version-marked SKILL.md outside the root with no matching source", repo)

    # A sibling directory whose name merely starts with the copy root's name.
    # Without the separator, `.claude/skills-archive/` is read as inside it.
    repo = mkfixture(tmp, "siblingprefix")
    write(repo / ".claude/skills-archive/alpha/SKILL.md",
          b"---\nname: alpha\nversion: 9.9.9\n---\narchived, not a live copy\n")
    git(repo, "add", "-A"); git(repo, "commit", "-qm", "siblingprefix")
    h.must_fail("(F39) a version-marked copy in a sibling of the copy root", repo,
                "outside the copy root")

    # A file literally named SKILL.md at the repository root has no directory
    # to take a name from.
    repo = mkfixture(tmp, "rootskill")
    # Version-marked on purpose: without a marker this case passes for a
    # skill_dir_name that invents a name, because rule 4 needs both halves.
    write(repo / "SKILL.md", b"---\nname: nothing\nversion: 9.9.9\n---\nno skill directory\n")
    git(repo, "add", "-A"); git(repo, "commit", "-qm", "rootskill")
    h.must_pass("(P11) a SKILL.md at the repository root is not a skill", repo)

    # An ORPHAN whose marker sits far down the file. Orphans are the one path
    # where the marker is actually read, so a truncated read demotes them.
    repo = mkfixture(tmp, "deepmarkerorphan")
    git(repo, "mv", ".claude/skills/alpha", ".claude/skills/alpha-gone")
    filler = b"".join(b"filler %d\n" % i for i in range(60))
    write(repo / ".claude/skills/alpha-gone/SKILL.md",
          b"---\nname: alpha\n" + filler + b"version: 9.9.9\n---\nbody\n")
    git(repo, "commit", "-qam", "deepmarkerorphan")
    h.must_fail("(F40) an orphan whose version marker sits far down the file", repo,
                "orphaned copy")

    # A contributor skill whose PROSE contains the text "version:" mid-line is
    # not version-managed. The marker is a line start, not a substring.
    repo = mkfixture(tmp, "prosemarker")
    write(repo / ".claude/skills/local-tool/SKILL.md",
          b"---\nname: local-tool\n---\nSee the version: field in the manifest.\n")
    git(repo, "commit", "-qam", "prosemarker")
    h.must_pass("(P12) 'version:' inside a line does not make a file version-managed", repo)

    # An untracked copy under a source-named directory with NO marker. The name
    # half of the untracked predicate is what catches it.
    repo = mkfixture(tmp, "untrackednomarker")
    write(repo / ".claude/skills/beta/SKILL.md", b"stale, and it lost its marker\n")
    git(repo, "rm", "-q", "--cached", ".claude/skills/beta/SKILL.md")
    git(repo, "commit", "-qm", "untrackednomarker")
    h.must_fail("(F41) an untracked markerless copy under a source-named directory",
                repo, "untracked skill copy")

    # A tracked file whose name merely ENDS with SKILL.md.
    repo = mkfixture(tmp, "skillbak")
    write(repo / ".claude/skills/alpha/SKILL.md.bak", b"a backup, not a skill\n")
    git(repo, "add", "-A"); git(repo, "commit", "-qm", "skillbak")
    h.must_pass("(P13) a file merely ending in SKILL.md is not one", repo)

    # A path whose bytes are not valid UTF-8. Dropping such paths from the
    # population would hide a real copy behind an encoding.
    repo = mkfixture(tmp, "badutf8")
    dest = os.fsencode(str(repo)) + b"/skills/bad\xffname/alpha"
    os.makedirs(dest, exist_ok=True)
    os.rename(os.fsencode(str(repo / ".claude/skills/alpha/SKILL.md")), dest + b"/SKILL.md")
    os.rmdir(os.fsencode(str(repo / ".claude/skills/alpha")))
    git(repo, "add", "-A"); git(repo, "commit", "-qm", "badutf8")
    h.must_fail("(F42) a misplaced copy at a path that is not valid UTF-8", repo,
                "outside the copy root")

    # A space in the SKILL DIRECTORY name itself, not in an ancestor segment.
    repo = mkfixture(tmp, "spacedname")
    body = SRC % (b"two words", b"two words")
    write(repo / "agent-skills/skills/two words/SKILL.md", body)
    write(repo / ".claude/skills/two words/SKILL.md", b"---\nname: x\nversion: 9.9.9\n---\ndrifted\n")
    git(repo, "add", "-A"); git(repo, "commit", "-qm", "spacedname")
    h.must_fail("(F43) a skill whose own directory name contains a space", repo,
                "diverged from source")

    # (S5) Deleting a copy from BOTH the index and the disk is invisible, and
    # it is invisible on purpose rather than by oversight. The copy set is
    # DERIVED, so "this source has no copy" and "this source's copy was just
    # deleted" are the same tree. Requiring every source to have a copy would
    # be wrong here: seven of the eight sources deliberately have none, and
    # (P5) pins that a new source with no copy yet is legitimate. Telling them
    # apart needs a declared copy set, which is the list this gate's design
    # refuses to keep. With exactly one copy the zero-detected arm masks it;
    # with two it would pass. Declared, not covered.
    h.declare("(S5) removing a copy from both the index and the disk cannot be "
              "distinguished from a source that never had one, because the copy "
              "set is derived; (F2) covers removal from disk alone")

    # A tree with no sources at all. The derivation returning nothing is a
    # broken tree, not a clean one, and that refusal had no case.
    repo = mkfixture(tmp, "nosources")
    git(repo, "rm", "-rq", "--cached", "agent-skills")
    shutil.rmtree(repo / "agent-skills")
    git(repo, "commit", "-qm", "nosources")
    h.must_fail("(F45) a tree holding no tracked sources at all", repo,
                "0 tracked sources")

    # The copy root present as a regular FILE with no copies tracked. Testing
    # only is_dir() would let this take the mirror arm and print PASS.
    repo = mkfixture(tmp, "rootfilemirror")
    git(repo, "rm", "-rq", "--cached", ".claude/skills")
    shutil.rmtree(repo / ".claude/skills")
    write(repo / ".claude/skills", b"not a directory\n")
    git(repo, "commit", "-qm", "rootfilemirror")
    h.must_fail("(F46) the copy root existing as a file is not the mirror shape",
                repo, "exists but is not a directory")

    # Invoked from a subdirectory. resolve_root names the extracted-tarball
    # trap as its reason for existing, and nothing exercised it: every other
    # case runs with cwd already at the fixture root.
    repo = mkfixture(tmp, "fromsubdir")
    result = subprocess.run(
        [sys.executable, str(GATE)], cwd=repo / "agent-skills",
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT, check=False,
    )
    text = result.stdout.decode("utf-8", "replace")
    if result.returncode != 0 and "run this from the repository root" in text:
        h.ok("(F47) invoked from a subdirectory, the gate refuses rather than guessing")
    else:
        h.no("(F47) invoked from a subdirectory, the gate refuses rather than guessing",
             f"rc={result.returncode}: {text.strip().splitlines()[0] if text.strip() else '(nothing)'}")

    # A staged blob git cannot read. Swallowing that returned "no marker",
    # which demotes an orphan to a local skill and passes.
    repo = mkfixture(tmp, "unreadableblob")
    git(repo, "mv", ".claude/skills/alpha", ".claude/skills/alpha-orphan")
    git(repo, "commit", "-qm", "orphan")
    oid = subprocess.run(
        ["git", "rev-parse", ":.claude/skills/alpha-orphan/SKILL.md"],
        cwd=repo, capture_output=True, text=True, check=True,
    ).stdout.strip()
    obj = repo / ".git" / "objects" / oid[:2] / oid[2:]
    if obj.exists():
        obj.unlink()
    h.must_fail("(F48) a staged blob the gate cannot read is reported, not skipped",
                repo, "could not read the staged blob")

    # An unreadable directory under the copy root. os.walk's default onerror
    # swallows this, which is the same shape as the git failure above.
    repo = mkfixture(tmp, "unreadabledir")
    secret = repo / ".claude/skills/secret"
    secret.mkdir()
    write(secret / "SKILL.md", b"---\nname: alpha\nversion: 9.9.9\n---\nstale\n")
    os.chmod(secret, 0o000)
    try:
        h.must_fail("(F49) a directory under the copy root the gate cannot read", repo,
                    "could not read")
    finally:
        os.chmod(secret, 0o755)

    # An unreadable untracked SKILL.md. Same class, the other branch.
    repo = mkfixture(tmp, "unreadablefile")
    stale = repo / ".claude/skills/stale-one/SKILL.md"
    write(stale, b"---\nname: stale-one\nversion: 9.9.9\n---\nstale\n")
    os.chmod(stale, 0o000)
    try:
        h.must_fail("(F50) an untracked SKILL.md the gate cannot read", repo,
                    "could not read")
    finally:
        os.chmod(stale, 0o644)

    # A skill directory replaced by a symlink to different content. find without
    # -L could not see it, and the local agent session reads it.
    repo = mkfixture(tmp, "symlinkdir")
    write(repo / "stale/gamma/SKILL.md",
          b"---\nname: gamma\nversion: 0.0.1\n---\nwrong content a local agent loads\n")
    os.symlink("../../stale/gamma", repo / ".claude/skills/gamma")
    h.must_fail("(F29) a symlinked skill directory under the copy root", repo,
                "untracked skill copy")


if __name__ == "__main__":
    sys.exit(main())
