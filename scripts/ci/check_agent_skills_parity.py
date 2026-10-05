#!/usr/bin/env python3
"""Prove every committed copy of a vendored agent skill still matches its source.

`agent-skills/skills/<name>/SKILL.md` is the source of truth for the consumer
agent skills. One deliberate copy is committed, under `.claude/skills/`, because
that is what Claude Code auto-loads in this repository and it has to be present
in a fresh clone.

WHY A GATE. The same document once existed twice with nothing comparing them,
and the copies drifted 816 lines apart.

WHY COPIES AND NOT SYMLINKS. `scripts/release/sanitize-for-oss.sh` Phase 2 runs
`find . -type l` over the whole tree and dies on the first hit, before the Phase
3 allowlist. One symlink anywhere stops the release, public path or not. That
check walks the DISK, so this gate checks the disk as well as the index: a path
tracked as a regular file but replaced by a symlink in the working tree passed
the index check and then killed the release.

WHY PYTHON. This gate was a shell script through four rewrites. Three external
reviewers and three validation passes found seventeen ways to make it report
PASS on a wrong tree, and the largest single class was shell path handling:
`awk` splitting `git ls-files -s` on whitespace so any path with a space left
the population; git's quoted form for a path holding a newline, a tab or a
non-ASCII byte defeating the filter that was meant to catch it; a `find | while
read` loop splitting on a newline inside a path; and a directory named `-e`
being passed to `grep` as an option. Every one of those is a bash-string bug,
not a logic bug, and each fix closed one instance and left the class open one
byte over. Reading `git ls-files -z` as bytes and splitting on NUL removes the
class instead of patching it: git never quotes in `-z` mode, and a path here is
a `bytes` object rather than a word in a command line.

TWO TREES, TWO MEANINGS OF ABSENT. Here `.claude/skills/` exists and a missing
copy is a bug. On the sanitized OSS mirror it does not ship at all, by
`[selection.dirs]` design, so the source stands alone and there is nothing to
compare. The mirror is not merely "the copy root is gone": renaming the root
satisfied that and left every copy sitting in the index, and the gate called the
wreckage a sanitized tree. The mirror is the copy root absent AND the index
holding no `SKILL.md` outside the source tree.

HOW A FILE IS CLASSIFIED. Location decides first, because location is what the
repository actually declares. Content decides only the question location cannot:
whether a file OUTSIDE the copy root is a misplaced copy or a foreign artifact
that merely shares a name.

    under the copy root, name matches a source  -> a copy, compare it
    under the copy root, no source, has version -> an orphaned copy
    under the copy root, no source, no version  -> a local skill, ignore
    outside the root, name matches a source, version -> a misplaced copy
    outside the root, anything else             -> not ours, ignore

The second row is what makes renaming detectable: a copy that loses its source
(renamed, deleted, or removed from the index) keeps its version marker, while
the contributor skills that legitimately live beside it never had one. The
marker is read from the INDEX, so editing or truncating the working copy cannot
change how that copy is classified.

`scripts/ci/test_agent_skills_parity.py` is the fixture corpus. Every rule above
has a case there. Change a rule, run that.
"""

from __future__ import annotations

import os
import pathlib
import subprocess
import sys

SRC_PREFIX = b"agent-skills/skills/"
COPY_ROOT = b".claude/skills"
SKILL = b"SKILL.md"
GIT_SYMLINK_MODE = b"120000"


class GitError(RuntimeError):
    """git failed, so nothing below was measured."""


def git_bytes(root: pathlib.Path, *args: str) -> bytes:
    """Run git in `root` and return raw stdout.

    Fails loud rather than returning empty: an earlier version read a git error
    as "0 tracked symlink(s)" and printed it as a measurement.
    """
    result = subprocess.run(
        ["git", *args],
        cwd=root,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if result.returncode != 0:
        raise GitError(
            f"git {' '.join(args)} exited {result.returncode}: "
            f"{result.stderr.decode('utf-8', 'replace').strip()}"
        )
    return result.stdout


def index_entries(root: pathlib.Path) -> list[tuple[bytes, bytes, bytes]]:
    """(mode, object id, path) for every stage-zero index entry.

    `git ls-files -sz` emits "<mode> <oid> <stage>\\t<path>\\0". NUL-delimited
    output is never quoted and never escaped, which is the whole reason this is
    read as bytes: the path is whatever bytes the filesystem holds.
    """
    raw = git_bytes(root, "ls-files", "-s", "-z")
    entries: list[tuple[bytes, bytes, bytes]] = []
    for record in raw.split(b"\0"):
        if not record:
            continue
        meta, _, path = record.partition(b"\t")
        if not path:
            raise GitError(f"unparseable index record: {record!r}")
        fields = meta.split(b" ")
        if len(fields) != 3:
            raise GitError(f"unparseable index metadata: {meta!r}")
        mode, oid, stage = fields
        if stage != b"0":
            raise GitError(
                f"path is unmerged at stage {stage.decode()}: "
                f"{os.fsdecode(path)}; resolve the conflict first"
            )
        entries.append((mode, oid, path))
    return entries


def source_name(path: bytes) -> bytes | None:
    """The skill name if `path` is exactly agent-skills/skills/<name>/SKILL.md.

    The shape is checked rather than globbed. A `*/SKILL.md` pathspec matches
    across "/" in both git and the shell, so a copy moved to
    agent-skills/skills/staged/<name>/SKILL.md once counted as an extra source.
    """
    if not path.startswith(SRC_PREFIX):
        return None
    rest = path[len(SRC_PREFIX) :]
    parts = rest.split(b"/")
    if len(parts) != 2 or parts[1] != SKILL or not parts[0]:
        return None
    return parts[0]


def under_copy_root(path: bytes) -> bool:
    return path.startswith(COPY_ROOT + b"/")


def skill_dir_name(path: bytes) -> bytes:
    return path.rsplit(b"/", 2)[-2] if b"/" in path else b""


def has_version_marker(
    root: pathlib.Path, path: bytes, oid: bytes | None, report: Report
) -> bool:
    """Does the frontmatter carry a `version:` line?

    Addressed by BLOB ID when the path is tracked, never by a rev expression.
    `git show ":<path>"` looked like it was reading the index, but `:<path>` is
    a REVISION, and git's `:<stage>:<path>` grammar eats a leading `0:`, `1:`,
    `2:` or `3:`. A skill directory named `0:alpha` therefore made git answer
    about `alpha/SKILL.md`, or fail; the failure was swallowed and the copy
    demoted itself out of the population. That is the same class as the shell
    quoting bugs this file was rewritten to escape: a path handed to a parser
    instead of being passed as data. The blob id needs no parsing at all.

    Read whole and scanned line by line: an earlier `head -n 20 | grep -q`
    returned 141 under pipefail when grep exited first and head took SIGPIPE,
    which demoted a copy for holding a long line.

    Any failure is REPORTED, not swallowed. A file this gate cannot read is a
    file it cannot police, and answering False for it is a silent pass.
    """
    if oid is not None:
        try:
            blob = git_bytes(root, "cat-file", "blob", os.fsdecode(oid))
        except GitError as exc:
            report.fail(f"could not read the staged blob for {show(path)}: {exc}")
            return False
    else:
        try:
            blob = (root / os.fsdecode(path)).read_bytes()
        except OSError as exc:
            report.fail(f"could not read {show(path)} from disk: {exc}")
            return False
    return any(line.startswith(b"version:") for line in blob.splitlines())


def show(path: bytes) -> str:
    """A path rendered for a human, with unprintable bytes made visible."""
    return os.fsdecode(path).encode("unicode_escape").decode("ascii")


class Report:
    def __init__(self) -> None:
        self.failures: list[str] = []
        self.checked = 0
        self.expected = 0
        self.missing = 0

    def fail(self, message: str, *detail: str) -> None:
        self.failures.append(message)
        print(f"FAIL: {message}", file=sys.stderr)
        for line in detail:
            print(f"  {line}", file=sys.stderr)


def run(root: pathlib.Path) -> int:
    report = Report()
    entries = index_entries(root)

    symlinks = [path for mode, _, path in entries if mode == GIT_SYMLINK_MODE]
    if symlinks:
        report.fail(
            f"{len(symlinks)} tracked symlink(s); "
            "sanitize-for-oss.sh Phase 2 fails closed on these",
            *(show(p) for p in symlinks),
        )

    skill_paths = [path for _, _, path in entries if path.rsplit(b"/", 1)[-1] == SKILL]
    tracked_skills = set(skill_paths)
    oid_of = {path: oid for _, oid, path in entries}

    sources: dict[bytes, bytes] = {}
    for path in skill_paths:
        name = source_name(path)
        if name is not None:
            sources[name] = path
    if not sources:
        report.fail(
            "0 tracked sources matching agent-skills/skills/<name>/SKILL.md",
            "the tree moved, or the derivation is broken",
        )
        return verdict(report, 0)

    for name, path in sorted(sources.items()):
        on_disk = root / os.fsdecode(path)
        if on_disk.is_symlink():
            report.fail(
                f"source {show(path)} is a symlink on disk; "
                "the release sanitizer dies on it"
            )
        elif not on_disk.is_file():
            report.missing += 1
            report.fail(f"git tracks source {show(path)} but it is not on disk")

    outside_source = 0
    for path in sorted(skill_paths):
        if source_name(path) is not None:
            continue
        outside_source += 1
        name = skill_dir_name(path)
        if under_copy_root(path):
            if name in sources:
                compare_copy(root, path, sources[name], oid_of, report)
            elif has_version_marker(root, path, oid_of.get(path), report):
                report.fail(
                    f"{show(path)} is an orphaned copy: version-managed, "
                    f"but no source named {show(name)!r}"
                )
            continue
        if path.startswith(SRC_PREFIX):
            report.fail(f"{show(path)} sits inside the source tree but is not a source")
            continue
        if name in sources and has_version_marker(root, path, oid_of.get(path), report):
            report.fail(
                f"{show(path)} is a vendored copy but sits outside the copy root"
            )

    scan_untracked(root, sources, tracked_skills, report)

    root_path = root / os.fsdecode(COPY_ROOT)
    root_present = root_path.is_dir()
    if not root_present and root_path.exists():
        # Absent means absent. A copy root that exists as a regular file or a
        # symlink is not the mirror shape, and testing only is_dir() would let
        # it take the mirror arm.
        report.fail(
            f"the copy root {show(COPY_ROOT)} exists but is not a directory"
        )
    if not root_present and outside_source == 0 and not report.failures:
        print(
            f"agent-skills parity: {len(sources)} source(s), "
            "no copy tree in this checkout (sanitized tree), nothing to compare"
        )
        print("agent-skills parity: PASS")
        return 0

    if not root_present and outside_source > 0:
        report.fail(
            f"the copy root is not on disk, but the index holds {outside_source} "
            "SKILL.md outside the source tree",
            "a copy root was renamed or moved; the mirror has no such files at all",
        )

    if root_present and report.expected == 0:
        report.fail(
            "the copy tree is present but 0 copies were detected; "
            "the derivation is broken"
        )

    if report.checked != report.expected and report.missing == 0:
        report.fail(
            f"{report.expected} copy/copies detected but {report.checked} were compared"
        )

    return verdict(report, len(sources))


def compare_copy(
    root: pathlib.Path,
    path: bytes,
    src_path: bytes,
    oid_of: dict[bytes, bytes],
    report: Report,
) -> None:
    report.expected += 1
    copy_on_disk = root / os.fsdecode(path)
    src_on_disk = root / os.fsdecode(src_path)
    if copy_on_disk.is_symlink():
        report.fail(
            f"copy {show(path)} is a symlink on disk; "
            "the release sanitizer dies on it"
        )
        return
    if not copy_on_disk.is_file():
        report.missing += 1
        report.fail(f"git tracks {show(path)} but it is not on disk")
        return
    if not src_on_disk.is_file():
        # Already reported as a missing source; diffing here would relabel it.
        return
    report.checked += 1

    # Compare the working tree, because that is what a consumer loads, and then
    # the index, because that is what ships. Either differing is drift.
    if copy_on_disk.read_bytes() != src_on_disk.read_bytes():
        report.fail(
            f"diverged from source: {show(path)}",
            f"fix: cp {os.fsdecode(src_path)} {os.fsdecode(path)}",
        )
        return
    if oid_of.get(path) != oid_of.get(src_path):
        report.fail(
            f"staged copy differs from staged source: {show(path)}",
            "the working tree matches but the index does not; re-stage both",
        )


def scan_untracked(
    root: pathlib.Path,
    sources: dict[bytes, bytes],
    tracked_skills: set[bytes],
    report: Report,
) -> None:
    """Untracked SKILL.md on disk under the copy root.

    `.claude/skills/` is read from disk by the local agent session, so a stale
    untracked copy there misleads a real consumer even though git never sees it.
    Symlinked directories are followed on purpose: an earlier `find` without -L
    could not see a skill directory replaced by a symlink to different content.
    """
    base = root / os.fsdecode(COPY_ROOT)
    if not base.is_dir():
        return

    def walk(directory: pathlib.Path, ancestry: frozenset[tuple[int, int]]) -> None:
        """Descend, following symlinks, stopping only on a genuine LOOP.

        The first version kept ONE set of visited inodes for the whole walk and
        skipped a directory it had already seen, before looking at the file in
        it. That is not loop protection, it is alias suppression: an untracked
        `.claude/skills/zzz` symlinked to a sibling skill was never inspected,
        even though a local agent session loads it as skill `zzz`. Two aliases
        of one stale directory also reported only whichever the walk reached
        first, so the verdict depended on directory iteration order. Loop
        protection needs the inodes on the path from the root to HERE, not
        every inode seen anywhere.
        """
        try:
            entries = sorted(os.scandir(directory), key=lambda e: e.name)
        except OSError as exc:
            report.fail(f"could not read {show(os.fsencode(str(directory)))}: {exc}")
            return
        subdirs: list[pathlib.Path] = []
        for entry in entries:
            try:
                if entry.is_file() and entry.name == SKILL.decode():
                    inspect(pathlib.Path(entry.path))
                elif entry.is_dir():
                    subdirs.append(pathlib.Path(entry.path))
            except OSError as exc:
                report.fail(f"could not stat {entry.path}: {exc}")
        for sub in subdirs:
            try:
                st = sub.stat()
            except OSError as exc:
                report.fail(f"could not stat {sub}: {exc}")
                continue
            key = (st.st_dev, st.st_ino)
            if key in ancestry:
                continue  # a genuine loop: this directory contains itself
            walk(sub, ancestry | {key})

    def inspect(abs_path: pathlib.Path) -> None:
        rel = os.fsencode(str(abs_path.relative_to(root)))
        if rel in tracked_skills:
            return
        name = skill_dir_name(rel)
        if name in sources or has_version_marker(root, rel, None, report):
            report.fail(f"{show(rel)} is an untracked skill copy under the copy root")

    try:
        base_key = (base.stat().st_dev, base.stat().st_ino)
    except OSError as exc:
        report.fail(f"could not stat the copy root: {exc}")
        return
    walk(base, frozenset({base_key}))


def verdict(report: Report, source_count: int) -> int:
    print(
        f"agent-skills parity: {source_count} source(s), "
        f"{report.checked} of {report.expected} copy/copies compared, "
        f"{report.missing} missing"
    )
    if report.failures:
        print("agent-skills parity: FAIL")
        return 1
    print("agent-skills parity: PASS")
    return 0


def resolve_root() -> pathlib.Path:
    """The repository being audited is the one the caller is standing in.

    Deriving it from __file__ meant the gate audited the repository the SCRIPT
    lives in, whichever tree it was pointed at. The matching trap in the other
    direction: an extracted release tarball sitting inside another checkout
    resolves `--show-toplevel` to the OUTER repository, and the gate then
    measures the wrong tree and still prints a verdict. Requiring the toplevel
    to be the current directory catches both, loudly.
    """
    cwd = pathlib.Path.cwd().resolve()
    try:
        out = git_bytes(cwd, "rev-parse", "--show-toplevel")
    except GitError as exc:
        raise GitError(f"not inside a git repository: {exc}") from exc
    top = pathlib.Path(os.fsdecode(out.strip())).resolve()
    if top != cwd:
        raise GitError(
            f"run this from the repository root. The git toplevel is {top} "
            f"but the working directory is {cwd}; measuring the wrong tree is "
            "how an extracted tarball once got audited as its parent repo"
        )
    return top


def main() -> int:
    try:
        return run(resolve_root())
    except GitError as exc:
        print(f"FAIL: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
