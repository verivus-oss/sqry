#!/usr/bin/env python3
"""Behavioural gate for scripts/install.sh.

The installer shipped a glibc binary to musl hosts, checksummed it, installed it
and printed "Installation complete" over a binary that could not execute. Nothing
here would have noticed, because the suite asserted what the installer SAID and
never that the thing it left on disk could run. Every arm below is a behaviour
somebody can lose, and several exist because a reviewer deleted the behaviour
and watched the old suite stay green.

WHY PYTHON. The fakes must model flag ARITY, not just flag presence: a fake curl
that swallowed `-w '%{url_effective}'` as a bare flag would consume the URL as
its argument and answer a different request than the installer made, so the arm
would pass while production failed. Modelling that in shell means hand-rolled
`case` arms and `shift 2`, which is exactly the kind of code that produced the
whitespace and quoting defects this repository has been chasing. Here the fakes
parse a real argument vector and log it as JSON before answering, so an arity
mismatch is visible in the log rather than silently producing a plausible
result.

The installer itself stays shell and is exercised as a black box: this file
never reads its source except where an arm traces one function deliberately.
"""

from __future__ import annotations

import hashlib
import json
import os
import pathlib
import re
import shutil
import stat
import subprocess
import sys
import tempfile

HERE = pathlib.Path(__file__).resolve().parent
ROOT = HERE.parent.parent
INSTALL = ROOT / "scripts" / "install.sh"

VERSION_TAG = "v9.9.9"
REPO = "test-owner/test-repo"
COMPONENTS = ("sqry", "sqry-mcp", "sqry-lsp", "sqryd")
SUFFIXES = (
    "linux-x86_64", "linux-x86_64-musl", "linux-arm64",
    "linux-arm64-musl", "macos-x86_64",
)

# Bump deliberately. A suite that quietly runs fewer arms than yesterday is the
# failure this number exists to catch.
EXPECTED_ARMS = 50
EXPECTED_SKIPS = 3

FAKE_CURL = r'''#!/usr/bin/env python3
# Die on SIGPIPE like a real tool: without this a consumer that closes
# early turns into a BrokenPipeError traceback on stderr, which reads as
# a failure of the thing under test.
import signal
signal.signal(signal.SIGPIPE, signal.SIG_DFL)
"""Model the installer's real curl invocations, including flag ARITY.

argv is logged before anything is answered, so a mismatch is visible in the log
rather than producing a wrong-but-plausible result.
"""
import json, os, pathlib, sys

argv = sys.argv[1:]
with open(os.environ["CURL_LOG"], "a") as log:
    log.write(json.dumps(argv) + "\n")

out = None
url = None
write_format = None
head_only = False
i = 0
while i < len(argv):
    a = argv[i]
    if a in ("-o", "--output"):
        out = argv[i + 1]; i += 2; continue
    if a in ("-w", "--write-out"):
        write_format = argv[i + 1]; i += 2; continue
    if a in ("-K", "--config"):
        i += 2; continue
    if a in ("-H", "--header"):
        i += 2; continue
    if a in ("-I", "--head"):
        head_only = True; i += 1; continue
    if a.startswith("-"):
        # Bundled short flags. -I may ride along inside them (-fsSLI).
        if not a.startswith("--") and "I" in a:
            head_only = True
        i += 1; continue
    url = a; i += 1

def emit(text):
    if out:
        pathlib.Path(out).write_text(text)
    else:
        sys.stdout.write(text)

# The releases/latest redirect: the installer reads the resolved URL out of
# -w '%{url_effective}' with the body discarded, so answer on stdout.
if head_only and write_format:
    if url and url.endswith("/releases/latest"):
        emit(url[: -len("/latest")] + "/tag/" + os.environ.get("FAKE_LATEST_TAG", ""))
    else:
        emit(url or "")
    sys.exit(0)

if url and "api.github.com" in url and url.endswith("releases/latest"):
    emit(json.dumps({"tag_name": os.environ.get("FAKE_LATEST_TAG", "")}))
    sys.exit(0)

if url and "/releases/download/" in url:
    name = url.rsplit("/", 1)[-1]
    asset = pathlib.Path(os.environ["ASSET_DIR"]) / name
    if not asset.is_file():
        sys.exit(22)
    data = asset.read_bytes()
    if out:
        pathlib.Path(out).write_bytes(data)
    else:
        sys.stdout.buffer.write(data)
    sys.exit(0)

sys.exit(22)
'''

FAKE_UNAME = r'''#!/usr/bin/env python3
# Die on SIGPIPE like a real tool: without this a consumer that closes
# early turns into a BrokenPipeError traceback on stderr, which reads as
# a failure of the thing under test.
import signal
signal.signal(signal.SIGPIPE, signal.SIG_DFL)
import os, subprocess, sys
a = sys.argv[1:]
if a[:1] == ["-s"]:
    print(os.environ.get("FAKE_OS", "Linux"))
elif a[:1] == ["-m"]:
    print(os.environ.get("FAKE_ARCH", "x86_64"))
else:
    sys.exit(subprocess.run(["/usr/bin/uname", *a]).returncode)
'''

# Two real musl shapes, both exiting non-zero, because a detector that depends
# on ldd's exit status is wrong on every musl host.
#   musl        the classic banner, on stderr, exit 1.
#   musl_alpine Alpine >= 3.10 replaced /usr/bin/ldd with a shell script that
#               execs the loader, so --version is treated as a file to load and
#               the error names the loader path. GNU config.guess dropped
#               ldd-based musl detection over that second shape.
FAKE_LDD = r'''#!/usr/bin/env python3
# Die on SIGPIPE like a real tool: without this a consumer that closes
# early turns into a BrokenPipeError traceback on stderr, which reads as
# a failure of the thing under test.
import signal
signal.signal(signal.SIGPIPE, signal.SIG_DFL)
import os, sys
mode = os.environ.get("FAKE_LIBC", "gnu")
if mode == "musl":
    print("musl libc (x86_64)", file=sys.stderr)
    print("Version 1.2.5", file=sys.stderr)
    print("Dynamic Program Loader", file=sys.stderr)
    print("Usage: ldd [options] [--] pathname", file=sys.stderr)
    sys.exit(1)
if mode == "musl_alpine":
    print("/lib/ld-musl-x86_64.so.1: cannot load --version: No such file or directory", file=sys.stderr)
    sys.exit(1)
if mode == "none":
    print("ldd: not found", file=sys.stderr)
    sys.exit(127)
print("ldd (GNU libc) 2.40")
'''

# cosign exists only so --verify-signatures clears its tool preflight the same
# way on every host: that preflight accepts gh OR cosign, and whether gh is
# installed differs between this runner and a contributor's laptop.
FAKE_COSIGN = r'''#!/usr/bin/env python3
# Die on SIGPIPE like a real tool: without this a consumer that closes
# early turns into a BrokenPipeError traceback on stderr, which reads as
# a failure of the thing under test.
import signal
signal.signal(signal.SIGPIPE, signal.SIG_DFL)
import os, sys
sys.exit(int(os.environ.get("FAKE_COSIGN_RC", "0")))
'''


class Suite:
    def __init__(self, tmp: pathlib.Path) -> None:
        self.tmp = tmp
        self.assets = tmp / "assets"
        self.fake = tmp / "fake"
        self.passed = 0
        self.failed = 0
        self.skipped = 0
        self.rc = 0
        self.out = ""
        self.err = ""
        self.curl_log: list[list[str]] = []
        self.assets.mkdir(parents=True)
        self.fake.mkdir(parents=True)
        for name, body in (
            ("curl", FAKE_CURL), ("uname", FAKE_UNAME),
            ("ldd", FAKE_LDD), ("cosign", FAKE_COSIGN),
        ):
            p = self.fake / name
            p.write_text(body)
            p.chmod(p.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
        for suffix in SUFFIXES:
            for component in COMPONENTS:
                self.make_binary(component, suffix)
        self.make_binary("sqry", "linux-x86_64-broken", broken=True)
        self.regen_sums()

    # -- fixtures -----------------------------------------------------------
    def make_binary(self, component: str, suffix: str, broken: bool = False) -> None:
        path = self.assets / f"{component}-{suffix}"
        if broken:
            path.write_text(
                '#!/bin/sh\necho "cannot execute: wrong libc (simulated)" >&2\nexit 1\n'
            )
        else:
            path.write_text(f'#!/bin/sh\necho "{component} 9.9.9"\n')
        path.chmod(0o755)

    def regen_sums(self) -> None:
        lines = []
        for entry in sorted(self.assets.iterdir()):
            if entry.name == "SHA256SUMS.txt" or not entry.is_file():
                continue
            digest = hashlib.sha256(entry.read_bytes()).hexdigest()
            lines.append(f"{digest}  {entry.name}\n")
        (self.assets / "SHA256SUMS.txt").write_text("".join(lines))

    # -- driving ------------------------------------------------------------
    def run_install(self, dirname: str, *args: str, **env_overrides: str) -> pathlib.Path:
        install_dir = self.tmp / dirname
        log = self.tmp / f"curl.{dirname}.log"
        log.write_text("")
        env = {
            "PATH": f"{self.fake}:{os.environ.get('PATH', '/usr/bin:/bin')}",
            "HOME": str(self.tmp),
            "TMPDIR": str(self.tmp),
            "ASSET_DIR": str(self.assets),
            "CURL_LOG": str(log),
            "FAKE_LATEST_TAG": VERSION_TAG,
            "FAKE_OS": "Linux",
            "FAKE_ARCH": "x86_64",
            "FAKE_LIBC": "gnu",
            "FAKE_COSIGN_RC": "0",
            "SQRY_INSTALL_LOADER_DIR": str(self.tmp / "noloader"),
        }
        env.update(env_overrides)
        result = subprocess.run(
            ["bash", str(INSTALL), "--repo", REPO, "--install-dir", str(install_dir), *args],
            env=env, cwd=self.tmp, capture_output=True, text=True, check=False,
        )
        self.rc = result.returncode
        self.out = result.stdout
        self.err = result.stderr
        self.curl_log = [json.loads(l) for l in log.read_text().splitlines() if l.strip()]
        return install_dir

    def requested(self, asset: str) -> bool:
        """Was this exact asset name the final segment of a download URL?

        Matched on the segment so `sqry-linux-x86_64` does not also match
        `sqry-linux-x86_64-musl`, and so the `-o <path>` that follows the URL
        cannot be mistaken for it.
        """
        for argv in self.curl_log:
            for token in argv:
                if "/releases/download/" in token and token.rsplit("/", 1)[-1] == asset:
                    return True
        return False

    def curl_saw(self, needle: str) -> bool:
        return any(needle in token for argv in self.curl_log for token in argv)

    def assets_named(self) -> str:
        names = {
            t.rsplit("/", 1)[-1]
            for argv in self.curl_log for t in argv
            if "/releases/download/" in t and "sqry-" in t
        }
        return " ".join(sorted(names)) or "(nothing)"

    def last_err(self) -> str:
        lines = [l for l in self.err.splitlines() if l.strip()]
        return lines[-1] if lines else ""

    def half_done_line(self) -> str:
        for line in self.err.splitlines():
            idx = line.find("already installed and working:")
            if idx >= 0:
                return line[idx:].strip()
        return ""

    # -- assertions ---------------------------------------------------------
    def check(self, condition: bool, good: str, bad: str) -> None:
        if condition:
            print(f"  PASS  {good}")
            self.passed += 1
        else:
            print(f"  FAIL  {bad}")
            self.failed += 1

    def skip(self, why: str) -> None:
        print(f"  SKIP  {why}")
        self.skipped += 1


def arms(s: Suite) -> None:
    # (I1) glibc host installs the glibc asset and the binary is runnable.
    d = s.run_install("i1", "--version", VERSION_TAG, "--component", "sqry")
    s.check(s.rc == 0 and os.access(d / "sqry", os.X_OK),
            "(I1) glibc host: install succeeds and leaves an executable",
            f"(I1) glibc host: rc={s.rc}; {s.last_err()}")
    s.check(s.requested("sqry-linux-x86_64") and not s.requested("sqry-linux-x86_64-musl"),
            "(I1) glibc host: requested the glibc asset, not the musl one",
            f"(I1) glibc host: wrong asset; curl saw: {s.assets_named()}")

    # (I2) musl host gets the musl asset. Before libc detection existed this arm
    # downloaded the glibc build, checksummed it, installed it and said complete.
    d = s.run_install("i2", "--version", VERSION_TAG, "--component", "sqry", FAKE_LIBC="musl")
    s.check(s.requested("sqry-linux-x86_64-musl"),
            "(I2) musl host: requested the musl asset",
            f"(I2) musl host: installer asked for {s.assets_named()}")
    s.check(s.rc == 0 and os.access(d / "sqry", os.X_OK),
            "(I2) musl host: install succeeds", f"(I2) musl host: rc={s.rc}; {s.last_err()}")
    s.check("Detected C library: musl" in s.out,
            "(I2) musl host: the run says which C library it detected",
            "(I2) musl host: detection was silent, so a wrong guess is invisible")

    # (I2b) Alpine >= 3.10: ldd is a shell script exec'ing the loader, so
    # --version produces a load error rather than a banner.
    s.run_install("i2b", "--version", VERSION_TAG, "--component", "sqry", FAKE_LIBC="musl_alpine")
    s.check(s.requested("sqry-linux-x86_64-musl"),
            "(I2b) Alpine >= 3.10 ldd shape is still detected as musl",
            "(I2b) Alpine >= 3.10 ldd shape was read as glibc")

    # (I2c) A host with no ldd at all (WolfiOS) must not crash and must land on glibc.
    d = s.run_install("i2c", "--version", VERSION_TAG, "--component", "sqry", FAKE_LIBC="none")
    s.check(s.rc == 0 and s.requested("sqry-linux-x86_64"),
            "(I2c) a host whose ldd fails outright installs the glibc build",
            f"(I2c) missing/failing ldd broke the install: rc={s.rc}; {s.last_err()}")

    # (I2d) THE LOADER-FILE BRANCH, consulted FIRST, which had no coverage until
    # a reviewer deleted it and watched the suite stay green. ldd is forced to
    # report glibc, so a musl answer can only have come from the loader file.
    loader = s.tmp / "fakeloader"; loader.mkdir(exist_ok=True)
    (loader / "ld-musl-x86_64.so.1").touch()
    s.run_install("i2d", "--version", VERSION_TAG, "--component", "sqry",
                  SQRY_INSTALL_LOADER_DIR=str(loader), FAKE_LIBC="gnu")
    s.check(s.requested("sqry-linux-x86_64-musl"),
            "(I2d) the loader file alone identifies a musl host, with ldd reporting glibc",
            "(I2d) the loader-file branch did not fire; detection fell through to ldd")
    s.check("Detected C library: musl" in s.out,
            "(I2d) the loader-file result is reported, not silent",
            "(I2d) the loader-file branch produced no receipt line")

    # (I2e) Control for (I2d): without the loader file the same setup must NOT
    # say musl. Absent this, (I2d) passes for a detector that always says musl.
    s.run_install("i2e", "--version", VERSION_TAG, "--component", "sqry",
                  SQRY_INSTALL_LOADER_DIR=str(s.tmp / "definitely-empty"), FAKE_LIBC="gnu")
    s.check(s.requested("sqry-linux-x86_64") and not s.requested("sqry-linux-x86_64-musl"),
            "(I2e) with no loader file and glibc ldd, the glibc asset is chosen",
            "(I2e) detection answered musl with neither signal present")

    # (I2f) The /lib DEFAULT of SQRY_INSTALL_LOADER_DIR. (I2d) always sets the
    # override, so the value that runs in production was the one nothing
    # exercised. Trace the real function and assert the path it tests.
    fn = extract_function(INSTALL.read_text(encoding="utf-8"), "detect_linux_libc")
    if fn is None:
        s.check(False, "", "(I2f) could not extract detect_linux_libc from install.sh")
        s.check(False, "", "(I2f) could not extract detect_linux_libc from install.sh")
    else:
        default = traced_loader_path(fn, override=None)
        s.check(default == "[[ -e /lib/ld-musl-x86_64.so.1 ]]",
                "(I2f) with the override unset, the loader is sought at /lib",
                f"(I2f) default loader path is wrong: {default}")
        overridden = traced_loader_path(fn, override="/probe/dir")
        s.check(overridden == "[[ -e /probe/dir/ld-musl-x86_64.so.1 ]]",
                "(I2f) the override redirects that lookup, so the default is not hardcoded",
                f"(I2f) override did not redirect the lookup: {overridden}")

    # (I2g) The arm64 -> aarch64 mapping. musl names its loader with the GNU
    # architecture spelling. No arm set FAKE_ARCH, so mutating that mapping to
    # anything, x86_64 included, survived the whole suite.
    loader64 = s.tmp / "fakeloader-arm"; loader64.mkdir(exist_ok=True)
    (loader64 / "ld-musl-aarch64.so.1").touch()
    s.run_install("i2g", "--version", VERSION_TAG, "--component", "sqry",
                  FAKE_ARCH="aarch64", SQRY_INSTALL_LOADER_DIR=str(loader64), FAKE_LIBC="gnu")
    s.check(s.requested("sqry-linux-arm64-musl"),
            "(I2g) an arm64 host with only an aarch64 loader resolves to the musl asset",
            f"(I2g) arm64 did not map to the aarch64 loader name; asset was {s.assets_named()}")
    loaderx = s.tmp / "fakeloader-x86only"; loaderx.mkdir(exist_ok=True)
    (loaderx / "ld-musl-x86_64.so.1").touch()
    s.run_install("i2g2", "--version", VERSION_TAG, "--component", "sqry",
                  FAKE_ARCH="aarch64", SQRY_INSTALL_LOADER_DIR=str(loaderx), FAKE_LIBC="gnu")
    s.check(s.requested("sqry-linux-arm64") and not s.requested("sqry-linux-arm64-musl"),
            "(I2g) an x86_64 loader does not satisfy an arm64 host, so the name is per-arch",
            "(I2g) arm64 host accepted an x86_64 loader name")

    # (I3) an explicit --libc overrides detection in both directions.
    s.run_install("i3", "--version", VERSION_TAG, "--component", "sqry", "--libc", "gnu",
                  FAKE_LIBC="musl")
    s.check(s.requested("sqry-linux-x86_64") and not s.requested("sqry-linux-x86_64-musl"),
            "(I3) --libc gnu overrides a musl-reporting host",
            f"(I3) --libc gnu was ignored; curl saw: {s.assets_named()}")
    s.run_install("i3b", "--version", VERSION_TAG, "--component", "sqry", "--libc", "musl",
                  FAKE_LIBC="gnu")
    s.check(s.requested("sqry-linux-x86_64-musl"),
            "(I3) --libc musl overrides a glibc-reporting host",
            "(I3) --libc musl was ignored")

    # (I4) THE ARM THAT WOULD HAVE CAUGHT THE ORIGINAL BUG. The checksum passes:
    # the bytes are exactly what the fixture published, they just cannot run.
    shutil.copy(s.assets / "sqry-linux-x86_64-broken", s.assets / "sqry-linux-x86_64")
    s.regen_sums()
    s.run_install("i4", "--version", VERSION_TAG, "--component", "sqry")
    s.check(s.rc != 0,
            f"(I4) a binary that cannot report its version fails the install (rc={s.rc})",
            "(I4) the installer exited 0 for a binary it had just proved unrunnable")
    s.check("cannot report its version" in s.err.lower(),
            "(I4) the failure names what went wrong, on stderr",
            "(I4) the install failed without saying why")
    s.check("Installation complete" not in s.out,
            "(I4) a failed install never prints 'Installation complete'",
            "(I4) the run printed 'Installation complete' over a binary that cannot run")
    s.check("--libc musl" in s.err,
            "(I4) the failure points at the libc override that would fix it",
            "(I4) the failure gives the user no next step")
    s.make_binary("sqry", "linux-x86_64"); s.regen_sums()

    # (I11) --component all must install ALL FOUR. Every other arm touching
    # `all` exercises a FAILURE, so deleting a component went undetected.
    d = s.run_install("i11", "--version", VERSION_TAG, "--component", "all")
    s.check(s.rc == 0, "(I11) --component all succeeds when every binary is good",
            f"(I11) --component all failed: rc={s.rc}; {s.last_err()}")
    absent = [c for c in COMPONENTS if not os.access(d / c, os.X_OK)]
    s.check(not absent, "(I11) all four binaries are installed and executable",
            f"(I11) --component all did not install: {' '.join(absent)}")
    count = sum(1 for p in d.iterdir() if p.is_file() and os.access(p, os.X_OK)) if d.is_dir() else 0
    s.check(count == 4, "(I11) exactly 4 binaries installed, no more and no fewer",
            f"(I11) expected exactly 4 installed binaries, found {count}")

    # (I10) --component all failing partway must NAME what is already installed,
    # EXACTLY. A substring assertion detects omission and never over-inclusion,
    # so these match the WHOLE line and run two scenarios, because a list pinned
    # at one scenario is indistinguishable from a hardcoded string.
    shutil.copy(s.assets / "sqry-linux-x86_64-broken", s.assets / "sqry-lsp-linux-x86_64")
    s.regen_sums()
    d = s.run_install("i10a", "--version", VERSION_TAG, "--component", "all")
    s.check(s.rc != 0 and os.access(d / "sqry", os.X_OK) and os.access(d / "sqry-mcp", os.X_OK),
            "(I10a) --component all fails on the bad binary after installing the good ones",
            f"(I10a) rc={s.rc}, sqry {'present' if os.access(d / 'sqry', os.X_OK) else 'absent'}")
    s.check(s.half_done_line() == "already installed and working: sqry sqry-mcp",
            "(I10a) the half-done list is exactly the components that proved they run",
            f"(I10a) expected exactly 'sqry sqry-mcp', got: {s.half_done_line()}")
    s.check("sqry-lsp" not in s.half_done_line(),
            "(I10a) the component that failed its run-proof is NOT listed as working",
            "(I10a) the failing binary is named as already installed and working")
    s.check("re-running is safe" in s.err,
            "(I10a) the failure tells the user recovery is a re-run",
            "(I10a) no recovery guidance on a partial install")
    s.make_binary("sqry-lsp", "linux-x86_64")

    # (I10b) SECOND component fails: a different list, so a hardcoded one dies.
    shutil.copy(s.assets / "sqry-linux-x86_64-broken", s.assets / "sqry-mcp-linux-x86_64")
    s.regen_sums()
    s.run_install("i10b", "--version", VERSION_TAG, "--component", "all")
    s.check(s.half_done_line() == "already installed and working: sqry",
            "(I10b) a different failure point yields a different list, so it is derived",
            f"(I10b) expected exactly 'sqry', got: {s.half_done_line()}")
    s.make_binary("sqry-mcp", "linux-x86_64")

    # (I10c) FIRST component fails: nothing installed, so claim no partial state.
    shutil.copy(s.assets / "sqry-linux-x86_64-broken", s.assets / "sqry-linux-x86_64")
    s.regen_sums()
    s.run_install("i10c", "--version", VERSION_TAG, "--component", "all")
    s.check("already installed and working" not in s.err,
            "(I10c) a first-component failure claims no partial install",
            f"(I10c) claimed a partial install with nothing installed: {s.half_done_line()}")
    s.make_binary("sqry", "linux-x86_64"); s.regen_sums()

    # (I5) The receipt carries the version the binary actually reported.
    d = s.run_install("i5", "--version", VERSION_TAG, "--component", "sqry")
    s.check(f"Installed: {d}/sqry (sqry 9.9.9)" in s.out,
            "(I5) the receipt quotes the installed binary's own --version output",
            f"(I5) receipt line was: {next((l for l in s.out.splitlines() if l.startswith('Installed:')), '(none)')}")

    # (I6) `latest` resolution through the redirect. This is the arm the fake
    # curl's -w arity exists for: a fake that swallowed the format string would
    # pass this while the real installer failed.
    d = s.run_install("i6", "--component", "sqry")
    s.check(s.rc == 0 and os.access(d / "sqry", os.X_OK),
            "(I6) 'latest' resolves through the redirect and installs",
            f"(I6) latest-tag resolution failed: rc={s.rc}; {s.last_err()}")
    s.check(s.curl_saw("url_effective"),
            "(I6) the redirect path was the one exercised, not the API fallback",
            "(I6) the installer never used the redirect; the arm proved nothing")

    # (I7) A bad checksum still fails, and fails before anything is installed.
    sums = s.assets / "SHA256SUMS.txt"
    sums.write_text(re.sub(r"^[0-9a-f]{64}(  sqry-linux-x86_64)$", "0" * 64 + r"\1",
                           sums.read_text(), flags=re.M))
    d = s.run_install("i7", "--version", VERSION_TAG, "--component", "sqry")
    s.check(s.rc != 0 and not (d / "sqry").exists(),
            "(I7) a checksum mismatch fails and installs nothing",
            f"(I7) checksum mismatch: rc={s.rc}, binary "
            f"{'INSTALLED' if (d / 'sqry').exists() else 'absent'}")
    s.regen_sums()

    # (I8) macOS has no musl build, so asking for one is an argument error.
    s.run_install("i8", "--version", VERSION_TAG, "--component", "sqry", "--libc", "musl",
                  FAKE_OS="Darwin")
    s.check(s.rc != 0 and "linux only" in s.err.lower(),
            "(I8) --libc musl on macOS is rejected with a reason",
            f"(I8) macOS accepted --libc musl: rc={s.rc}; {s.last_err()}")

    # (I9) An invalid --libc value is rejected rather than treated as gnu.
    s.run_install("i9", "--version", VERSION_TAG, "--component", "sqry", "--libc", "glibc")
    s.check(s.rc != 0 and "must be auto, gnu, or musl" in s.err,
            "(I9) an unknown --libc value is rejected",
            f"(I9) --libc glibc was accepted: rc={s.rc}")

    # The arms below close mutation survivors reported in review. Each names the
    # behaviour it protects; every one of them had zero arms before.

    # (I12) The version tag is pasted straight into a download URL, so its shape
    # is an input-validation boundary and not cosmetics.
    s.run_install("i12", "--component", "sqry", "--version", "9.9.9")
    s.check(s.rc != 0 and "version tag must be" in s.err,
            "(I12) a version tag without the v prefix is rejected",
            f"(I12) --version 9.9.9 was accepted: rc={s.rc}; {s.last_err()}")
    s.run_install("i12b", "--component", "sqry", "--version", VERSION_TAG)
    s.check(s.rc == 0, "(I12) the control: a well-formed tag still installs",
            f"(I12) a valid tag was rejected: rc={s.rc}; {s.last_err()}")

    # (I13) --repo is pasted into the same URL. Same boundary, same silence.
    s.run_install("i13", "--component", "sqry", "--version", VERSION_TAG, "--repo", "not a repo")
    s.check(s.rc != 0 and "must match OWNER/REPO" in s.err,
            "(I13) a malformed --repo is rejected",
            f"(I13) --repo 'not a repo' was accepted: rc={s.rc}; {s.last_err()}")

    # (I14) An unsupported architecture must stop before any download.
    s.run_install("i14", "--component", "sqry", "--version", VERSION_TAG, FAKE_ARCH="ppc64le")
    s.check(s.rc != 0 and "unsupported architecture" in s.err,
            "(I14) an unsupported architecture is refused by name",
            f"(I14) ppc64le was accepted: rc={s.rc}; {s.last_err()}")

    # (I15) --no-checksum must actually stop verification, and the default must
    # actually do it. Both directions, because a flag that silently does nothing
    # and a default that silently does nothing look identical from one side.
    s.make_binary("sqry", "linux-x86_64"); s.regen_sums()
    s.run_install("i15", "--component", "sqry", "--version", VERSION_TAG, "--no-checksum")
    s.check(s.rc == 0 and not s.curl_saw("SHA256SUMS.txt"),
            "(I15) --no-checksum installs without fetching SHA256SUMS.txt",
            f"(I15) --no-checksum still fetched checksums (rc={s.rc})")
    s.run_install("i15b", "--component", "sqry", "--version", VERSION_TAG)
    s.check(s.rc == 0 and s.curl_saw("SHA256SUMS.txt"),
            "(I15) the control: without the flag, checksums ARE fetched",
            f"(I15) the default did not fetch checksums, so --no-checksum proves nothing (rc={s.rc})")

    # (I16) --verify-signatures must change what the installer DOES. Making the
    # flag assign false turned it into a no-op that printed nothing alarming,
    # which is the worst shape for a security flag: the user believes they
    # opted in.
    s.run_install("i16", "--component", "sqry", "--version", VERSION_TAG, "--verify-signatures")
    s.check(s.curl_saw("release-artifacts.attestation.json"),
            "(I16) --verify-signatures fetches the attestation bundle",
            "(I16) --verify-signatures never asked for an attestation; the flag did nothing")
    s.run_install("i16b", "--component", "sqry", "--version", VERSION_TAG)
    s.check(not s.curl_saw("release-artifacts.attestation.json"),
            "(I16) the control: without the flag, no attestation is fetched",
            "(I16) attestation fetched without the flag, so (I16) proves nothing")

    # (I17) macOS must get the macos asset. (I8) only proves --libc musl is
    # refused there, which happens before the suffix is ever built.
    s.make_binary("sqry", "macos-x86_64"); s.regen_sums()
    d = s.run_install("i17", "--component", "sqry", "--version", VERSION_TAG, FAKE_OS="Darwin")
    s.check(s.requested("sqry-macos-x86_64") and not s.requested("sqry-linux-x86_64"),
            "(I17) a Darwin host requests the macos asset",
            f"(I17) Darwin asked for: {s.assets_named()}")
    s.check(s.rc == 0 and os.access(d / "sqry", os.X_OK),
            "(I17) a Darwin host installs and leaves an executable",
            f"(I17) Darwin install failed: rc={s.rc}; {s.last_err()}")

    # (I18) --verify-signatures with nothing verifiable available must REFUSE.
    # (I16) only proves a URL was requested, so replacing the terminus of
    # verify_asset_provenance with a success message installed an unverified
    # binary while the suite stayed green.
    s.make_binary("sqry", "linux-x86_64"); s.regen_sums()
    for stale in ("sqry-linux-x86_64.bundle", "release-artifacts.attestation.json"):
        (s.assets / stale).unlink(missing_ok=True)
    s.regen_sums()
    d = s.run_install("i18", "--component", "sqry", "--version", VERSION_TAG, "--verify-signatures")
    s.check(s.rc != 0 and "no supported provenance verification succeeded" in s.err,
            "(I18) unverifiable provenance refuses the install by name",
            f"(I18) rc={s.rc} with no provenance available; {s.last_err()}")
    s.check(not (d / "sqry").exists(),
            "(I18) nothing was installed when provenance could not be verified",
            f"(I18) an unverified binary was installed at {d}/sqry")

    # (I19) When a bundle IS present, the verifier's verdict must decide the
    # install. A verifier whose failure is ignored is the same defect as no
    # verifier at all, so both directions are asserted.
    shutil.copy(s.assets / "sqry-linux-x86_64", s.assets / "sqry-linux-x86_64.bundle")
    s.regen_sums()
    d = s.run_install("i19", "--component", "sqry", "--version", VERSION_TAG,
                      "--verify-signatures", FAKE_COSIGN_RC="0")
    s.check(s.rc == 0 and os.access(d / "sqry", os.X_OK)
            and "Legacy Cosign bundle verified" in s.out,
            "(I19) a passing verifier lets the install through",
            f"(I19) verifier passed but install did not: rc={s.rc}; {s.last_err()}")
    d = s.run_install("i19b", "--component", "sqry", "--version", VERSION_TAG,
                      "--verify-signatures", FAKE_COSIGN_RC="1")
    s.check(s.rc != 0 and not (d / "sqry").exists(),
            "(I19) a failing verifier stops the install and leaves no binary",
            f"(I19) cosign failed but rc={s.rc} and binary "
            f"{'INSTALLED' if (d / 'sqry').exists() else 'absent'}")
    (s.assets / "sqry-linux-x86_64.bundle").unlink(missing_ok=True)
    s.regen_sums()

    # (I20) sha256_of_file's `shasum -a 256` fallback. A skip once declared this
    # unreachable on a host that has sha256sum. That was wrong: a PATH holding
    # exactly the tools install.sh calls, minus sha256sum, reaches it, and a
    # wrong digest algorithm there is a silent checksum bypass.
    shasum = shutil.which("shasum")
    if shasum is None:
        s.skip("(I20) this host has no shasum, so the sha256sum-absent fallback "
               "cannot be exercised here")
    else:
        sandbox = s.tmp / "nosha256"; sandbox.mkdir(exist_ok=True)
        # python3 is here for the FAKES' shebang, not for install.sh, which
        # calls none of it. sha256sum is the only thing deliberately withheld.
        for tool in ("bash", "sh", "env", "python3", "install", "grep", "awk", "sed",
                     "head", "cut", "tr", "sort", "find", "mktemp", "mkdir", "cp",
                     "rm", "chmod", "dirname", "basename", "cat", "shasum"):
            found = shutil.which(tool)
            if found:
                link = sandbox / tool
                if not link.exists():
                    link.symlink_to(found)
        s.make_binary("sqry", "linux-x86_64"); s.regen_sums()
        if shutil.which("sha256sum", path=f"{s.fake}:{sandbox}") is not None:
            s.check(False, "", "(I20) the sandbox still exposes sha256sum, so the "
                              "fallback was not the branch under test")
        else:
            d = s.run_install("i20", "--component", "sqry", "--version", VERSION_TAG,
                              PATH=f"{s.fake}:{sandbox}")
            if "command not found" in s.err:
                s.check(False, "", "(I20) the sandbox is missing a tool install.sh needs, "
                                   f"so this arm measured the sandbox: {s.last_err()}")
            else:
                s.check(s.rc == 0 and os.access(d / "sqry", os.X_OK),
                        "(I20) with no sha256sum on PATH, the shasum fallback verifies and installs",
                        f"(I20) shasum fallback failed: rc={s.rc}; {s.last_err()}")

    # Declared gaps. Named so the suite cannot be read as covering them.
    s.skip("(S1) no arm can plant /lib/ld-musl-<arch>.so.1 on a glibc host, so the "
           "branch firing against a REAL musl root is unexercised; (I2d) covers the "
           "logic and (I2f) the default path")
    s.skip("(S3) the GH_TOKEN and unauthenticated api.github.com tag-resolution "
           "stages are unexercised; (I6) covers only the redirect stage")
    s.skip("(S4) individual diagnostic lines in the failed-version-check path are "
           "not asserted, only the ones (I4)/(I10a)/(I10b) name")


def extract_function(text: str, name: str) -> str | None:
    """The body of a shell function, located by name and closed on `^}`."""
    lines = text.splitlines()
    body: list[str] = []
    capturing = False
    for line in lines:
        if not capturing:
            if line.startswith(f"{name}()"):
                capturing = True
                body.append(line)
            continue
        body.append(line)
        if line == "}":
            return "\n".join(body)
    return None


def traced_loader_path(fn: str, override: str | None) -> str:
    """Run the real function under `bash -x` and report the loader path it tests.

    Tracing rather than asserting on the source, because the source can be right
    and the resolved path still wrong.
    """
    env = {"PATH": os.environ.get("PATH", "/usr/bin:/bin")}
    if override is not None:
        env["SQRY_INSTALL_LOADER_DIR"] = override
    result = subprocess.run(
        ["bash", "-x", "-c", f"arch=x86_64\n{fn}\ndetect_linux_libc"],
        env=env, capture_output=True, text=True, check=False,
    )
    hits = re.findall(r"\[\[ -e [^ ]+ \]\]", result.stderr + result.stdout)
    return hits[0] if hits else "(no loader test traced)"


def main() -> int:
    if not INSTALL.is_file():
        print(f"FATAL: no installer at {INSTALL}", file=sys.stderr)
        return 1
    print("install.sh behavioural gate\n")
    tmp = pathlib.Path(tempfile.mkdtemp(prefix="install-gate-"))
    try:
        suite = Suite(tmp)
        arms(suite)
    finally:
        shutil.rmtree(tmp, ignore_errors=True)

    total = suite.passed + suite.failed
    print(f"\narms: {suite.passed} passed, {suite.failed} failed, {suite.skipped} skipped")
    if total != EXPECTED_ARMS or suite.skipped != EXPECTED_SKIPS:
        print(
            f"install.sh behavioural gate: FAIL ({total} arms and {suite.skipped} skips ran, "
            f"{EXPECTED_ARMS} and {EXPECTED_SKIPS} declared)"
        )
        return 1
    if suite.failed:
        print("install.sh behavioural gate: FAIL")
        return 1
    print("install.sh behavioural gate: PASS")
    return 0


if __name__ == "__main__":
    sys.exit(main())
