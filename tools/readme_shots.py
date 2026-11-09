#!/usr/bin/env python3
"""readme_shots.py -- the pictures of README.md: docs/shots/*.uis against the program, into docs/images/

    python tools/readme_shots.py                 every docs/shots/*.uis
    python tools/readme_shots.py editor academy  only these (docs/shots/NAME.uis)

The scripts are window scripts like tests/*.uis (a first line "#!args ..." adds arguments, `shot FILE`
writes a picture) but no tests: `cargo jflib test` does not run them.  They run in this repository's
folder against target/debug/JFNetworker(.exe) (cargo build first), without a real window.
"""
import os
import shlex
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def main():
    exe = ROOT / "target" / "debug" / ("JFNetworker.exe" if os.name == "nt" else "JFNetworker")
    if not exe.exists():
        sys.exit(f"readme_shots: {exe} is missing (cargo build)")
    names = sys.argv[1:]
    scripts = sorted((ROOT / "docs" / "shots").glob("*.uis"))
    if names:
        scripts = [s for s in scripts if s.stem in names]
    (ROOT / "docs" / "images").mkdir(parents=True, exist_ok=True)
    failed = 0
    for s in scripts:
        first = s.read_text(encoding="utf-8").splitlines()[:1]
        extra = shlex.split(first[0][len("#!args"):]) if first and first[0].startswith("#!args") else []
        started = time.monotonic()
        p = subprocess.run([str(exe), *extra, "--script", s.relative_to(ROOT).as_posix()], cwd=ROOT,
                           capture_output=True, text=True, encoding="utf-8", errors="replace")
        ok = p.returncode == 0
        failed += not ok
        print(f"{'ok  ' if ok else 'FAIL'} {s.name} ({time.monotonic() - started:.1f} s)")
        if not ok:
            print("\n".join("     " + l for l in (p.stdout + p.stderr).strip().splitlines()[-20:]))
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
