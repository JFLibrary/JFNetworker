#!/usr/bin/env python3
"""linux_test.py -- the test scripts on the Linux program, from Windows through WSL

    python tools/linux_test.py                 every tests/*.uis
    python tools/linux_test.py file catalog    only these (tests/NAME.uis)

`cargo jflib test` runs the program of the machine it runs on.  On Windows this script builds the
Linux programs (cargo build --workspace --target x86_64-unknown-linux-gnu), then runs every script as
`cargo jflib test` does, in the folder of the repository, through the WSL distribution
JFLIB_WSL_DISTRO (default Ubuntu-24.04; WSLg gives the program its display, the scripts open no
real window).  On Linux, `cargo jflib test --target x86_64-unknown-linux-gnu` does the same directly.
"""
import os
import shlex
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
TRIPLE = "x86_64-unknown-linux-gnu"
DISTRO = os.environ.get("JFLIB_WSL_DISTRO", "Ubuntu-24.04")


def wsl_path(p):
    p = str(Path(p).resolve())
    return "/mnt/" + p[0].lower() + p[2:].replace("\\", "/")


def main():
    if os.name != "nt":
        sys.exit("linux_test.py runs on Windows; on Linux: cargo jflib test --target " + TRIPLE)
    names = sys.argv[1:]
    r = subprocess.run(["cargo", "build", "--workspace", "--target", TRIPLE], cwd=ROOT)
    if r.returncode:
        sys.exit(r.returncode)
    exe = wsl_path(ROOT / "target" / TRIPLE / "debug" / "JFNetworker")
    scripts = sorted((ROOT / "tests").glob("*.uis"))
    if names:
        scripts = [s for s in scripts if s.stem in names]
    failed = 0
    for s in scripts:
        text = s.read_text(encoding="utf-8")
        first = text.splitlines()[:1]
        extra = shlex.split(first[0][len("#!args"):]) if first and first[0].startswith("#!args") else []
        for line in text.splitlines():
            if line.startswith(("shot ", "dump ")) and len(line.split()) > 1:
                (ROOT / line.split()[1]).parent.mkdir(parents=True, exist_ok=True)
        rel = s.relative_to(ROOT).as_posix()
        started = time.monotonic()
        p = subprocess.run(["wsl.exe", "-d", DISTRO, "--cd", wsl_path(ROOT), "-e", exe, *extra, "--script", rel],
                           stdin=subprocess.DEVNULL, capture_output=True, text=True, encoding="utf-8", errors="replace")
        if p.returncode == 0:
            print(f"ok   {rel} ({time.monotonic() - started:.1f} s)", flush=True)
            continue
        failed += 1
        print(f"FAIL {rel} (exit {p.returncode}, {time.monotonic() - started:.0f} s)", flush=True)
        for line in (p.stdout + p.stderr).splitlines():
            if line.lstrip().startswith(("FAIL", "error", "panic", "UIWARN")):
                print("     " + line.strip())
    print(f"{len(scripts) - failed} of {len(scripts)} scripts passed on Linux ({DISTRO})")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
