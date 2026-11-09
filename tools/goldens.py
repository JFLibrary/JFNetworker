#!/usr/bin/env python3
"""goldens.py -- the golden scenarios through jfn-sim, without the window program

    python tools/goldens.py [--sim PATH]

Every tests/scen_*.uis starts the window program with "PROJECT --scenario S --golden R"; jfn-sim
takes the same after `run`.  This runs all of them with jfn-sim (default: the newest one in
target/), the same reports byte for byte on every system.  CI runs it on Windows and Linux; the
window program's run of the same scripts is part of `cargo jflib test`.
"""
import os
import shlex
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def find_sim():
    if "--sim" in sys.argv:
        return Path(sys.argv[sys.argv.index("--sim") + 1])
    exe = "jfn-sim.exe" if os.name == "nt" else "jfn-sim"
    found = sorted((ROOT / "target").glob(f"**/{exe}"), key=lambda p: p.stat().st_mtime)
    if not found:
        sys.exit("goldens: no jfn-sim (cargo build --workspace, or --sim PATH)")
    return found[-1]


def main():
    sim = find_sim()
    failed = 0
    scripts = sorted((ROOT / "tests").glob("scen_*.uis"))
    for s in scripts:
        first = s.read_text(encoding="utf-8").splitlines()[0]
        args = shlex.split(first[len("#!args"):])
        p = subprocess.run([str(sim), "run", *args], cwd=ROOT, capture_output=True, text=True, encoding="utf-8", errors="replace")
        if p.returncode == 0:
            print(f"ok   {s.name}")
            continue
        failed += 1
        print(f"FAIL {s.name} (exit {p.returncode})")
        print("\n".join("     " + l for l in (p.stdout + p.stderr).strip().splitlines()[-15:]))
    print(f"{len(scripts) - failed} of {len(scripts)} golden scenarios passed ({sim.name})")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
