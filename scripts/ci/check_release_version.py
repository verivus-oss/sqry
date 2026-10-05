#!/usr/bin/env python3
"""Assert that release-pinned documents name THIS release, everywhere.

The workspace version in Cargo.toml is the single source of truth. release-plz
bumps it, and everything else must follow it rather than restate it by hand.

WHAT THIS CATCHES THAT sync-versions.sh DOES NOT. That script checks one token
per file, via detect_old_version, which returns the FIRST version-looking string
it sees. In the shipped agent skills that is the `version:` frontmatter field,
so the other occurrences, in the prose, were never checked. They stayed correct
only as a side effect: the field went stale first, the file errored, and --fix
then rewrote every occurrence at once. The moment the field and the prose
diverge, the field reads current, the file gets a tick, and the prose is stale
for good. Measured: with the field at 32.0.0 and the prose left at v31.0.0, that
script printed a green tick for all eight skills while they carried thirteen
stale `v31.0.0` strings between them.

These documents are shipped to users as a plugin and they describe one release.
Every version token in them is a claim about it, so every token is checked.

A TOKEN NAMES THE RELEASE A DOCUMENT TARGETS, NEVER WHAT WAS MEASURED. Every
release PR runs `sync-versions.sh --fix`, which rewrites every token here to the
new version without measuring anything. A sentence that ties a token to a
measurement ("measured against vX", "executed with vX", "checked in the vX
tree") is therefore false from the next release on, so provenance in these
documents carries no version. This is a rule for authors, not a check: no text
pattern can tell a measurement claim from a target claim.

THE FILE SET IS DERIVED FROM THE INDEX, never listed, because a list goes stale
exactly when somebody adds the thing it was meant to cover.
"""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import re
import subprocess
import sys
import tomllib

# Anything that looks like a release. Bare and v-prefixed both appear in prose.
VERSION = re.compile(rb"v?\d+\.\d+\.\d+")

# The frontmatter field a shipped skill is pinned BY. Requiring merely that some
# version-shaped bytes appear moved the bar from zero bytes to six: a file whose
# entire content was "32.0.0" passed, and so did one whose `version:` field had
# been deleted while the prose still named the release. Both cleared this gate
# and the parity gate together, which is the pairing this file exists to close.
# `\r` is tolerated because a CRLF checkout is a checkout, not a defect: with
# core.autocrlf=true this rejected all eight shipped skills and said they carried
# no field. Quotes are tolerated because `version: "1.2.3"` is valid YAML and the
# two sibling readers of this same field both accept it.
VERSION_FIELD = re.compile(
    rb"""(?m)^version:[ \t]*["']?(v?\d+\.\d+\.\d+)["']?[ \t\r]*$""")

# The leading `---` block. A document's version is declared there, and nowhere
# else in the file counts, however version-shaped it looks.
FRONTMATTER = re.compile(
    rb"\A(?:\xef\xbb\xbf)?---[ \t]*\r?\n(.*?)\r?\n---[ \t]*\r?(?:\n|\Z)", re.S)

# Release-pinned document sets, as index pathspecs. Each entry is a shape, not a
# filename, so adding a skill is covered without touching this file.
PINNED_PATHSPECS = (
    "agent-skills/skills/*/SKILL.md",
    "agent-skills/.claude-plugin/plugin.json",
)


class Failure(RuntimeError):
    pass


def git(root: pathlib.Path, *args: str) -> bytes:
    r = subprocess.run(["git", *args], cwd=root, stdout=subprocess.PIPE,
                       stderr=subprocess.PIPE, check=False)
    if r.returncode != 0:
        raise Failure(f"git {' '.join(args)} exited {r.returncode}: "
                      f"{r.stderr.decode('utf-8', 'replace').strip()}")
    return r.stdout


def workspace_version(root: pathlib.Path) -> str:
    data = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))
    try:
        return data["workspace"]["package"]["version"]
    except KeyError as exc:
        raise Failure("Cargo.toml has no [workspace.package] version") from exc


def pinned_files(root: pathlib.Path) -> list[bytes]:
    """Tracked release-pinned documents, NUL-delimited so odd paths survive."""
    out = git(root, "ls-files", "-z", "--", *PINNED_PATHSPECS)
    files = [p for p in out.split(b"\0") if p]
    if not files:
        raise Failure(
            "0 release-pinned documents matched "
            f"{', '.join(PINNED_PATHSPECS)}; the derivation is broken or the "
            "tree moved. A set that comes back empty is not a clean tree."
        )
    return files


def declared_version(label: str, blob: bytes) -> tuple[str | None, str]:
    """The version a pinned document DECLARES, by the field its format uses.

    Requiring merely that some version-shaped bytes appear moved the bar from
    zero bytes to six: a file whose entire content was "32.0.0" passed, and so
    did one whose version field had been deleted while the prose still named the
    release. Both cleared this gate and the parity gate together.

    Each format is read with its own parser rather than one regex over both,
    because a JSON `"version": "x"` and a markdown `version: x` are not the same
    shape and a pattern loose enough for both matches prose in either.
    """
    if label.endswith(".json"):
        try:
            data = json.loads(blob)
        except ValueError as exc:
            return None, f"is not parseable JSON ({exc})"
        value = data.get("version") if isinstance(data, dict) else None
        if not isinstance(value, str):
            return None, "has no top-level string \"version\" key"
        return value.lstrip("v"), ""
    # FRONTMATTER only. Searching the whole file accepted a `version:` line
    # inside a fenced code block, which is documentation of the format rather
    # than a declaration of this document's version.
    front = FRONTMATTER.match(blob)
    if front is None:
        return None, "has no frontmatter block to carry a `version:` field"
    match = VERSION_FIELD.search(front.group(1))
    if match is None:
        return None, "carries no `version:` frontmatter field"
    return match.group(1).decode().lstrip("v"), ""


def stale_tokens(blob: bytes, version: str) -> list[str]:
    want = {version.encode(), b"v" + version.encode()}
    return sorted({t.decode() for t in VERSION.findall(blob) if t not in want})


def run(root: pathlib.Path, fix: bool) -> int:
    version = workspace_version(root)
    files = pinned_files(root)
    print(f"release version (Cargo.toml [workspace.package]): {version}")
    errors = 0
    fixed = 0
    for rel in files:
        path = root / os.fsdecode(rel)
        label = os.fsdecode(rel)
        try:
            blob = path.read_bytes()
        except OSError as exc:
            print(f"FAIL: cannot read {label}: {exc}", file=sys.stderr)
            errors += 1
            continue
        total = len(VERSION.findall(blob))
        stale = stale_tokens(blob, version)
        declared, why = declared_version(label, blob)
        if declared is None:
            print(f"FAIL: {label} {why}; a release-pinned document is pinned BY "
                  f"its version field, and prose naming {version} elsewhere is "
                  f"not the same thing", file=sys.stderr)
            errors += 1
            continue
        if declared != version and not fix:
            print(f"FAIL: {label} declares version {declared}, this release is "
                  f"{version}", file=sys.stderr)
            errors += 1
            continue
        if not stale and declared == version:
            print(f"  ok    {label}  ({total} occurrence(s))")
            continue
        if fix:
            new = VERSION.sub(
                lambda m: (b"v" if m.group().startswith(b"v") else b"") + version.encode(),
                blob,
            )
            path.write_bytes(new)
            fixed += 1
            print(f"  fixed {label}  {' '.join(stale)} -> {version} ({total} occurrence(s))")
        else:
            print(f"FAIL: {label} names {' '.join(stale)}, this release is {version} "
                  f"({total} occurrence(s) scanned)", file=sys.stderr)
            errors += 1

    print(f"release-version consistency: {len(files)} pinned document(s), "
          f"{errors} rejected, {fixed} fixed")
    if errors:
        print("release-version consistency: FAIL")
        # --fix rewrites version TOKENS. It cannot add a field that is not
        # there, so saying "run --fix" for that class sends you round a loop.
        print("  --fix rewrites stale version tokens; a missing or wrong "
              "`version:` field must be corrected by hand", file=sys.stderr)
        return 1
    print("release-version consistency: PASS")
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--fix", action="store_true", help="rewrite every stale token")
    args = ap.parse_args()
    cwd = pathlib.Path.cwd().resolve()
    try:
        top = pathlib.Path(os.fsdecode(git(cwd, "rev-parse", "--show-toplevel").strip())).resolve()
        if top != cwd:
            raise Failure(f"run this from the repository root; git toplevel is {top}, cwd is {cwd}")
        return run(top, args.fix)
    except Failure as exc:
        print(f"FAIL: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
