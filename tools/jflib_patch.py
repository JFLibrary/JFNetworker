#!/usr/bin/env python3
"""jflib_patch.py -- the table [patch.crates-io] in .cargo/config.toml: JFLib's crates from a checkout

    python tools/jflib_patch.py              ../JFLib, or the checkout JFLIB names
    python tools/jflib_patch.py --remove     the table away: JFLib from crates.io again

JFLib comes from crates.io (the style netlab too, since 2026-11-10).  To try changes of JFLib here,
this script points every JFLib crate this graph uses at a checkout: Cargo cannot link two copies of
jflib-rt (links = "jflib_rt"), so it cannot be only the changed one.  This script reads the crates and their folders from the checkout's
Cargo.toml, writes the table for all of them, asks `cargo metadata` which ones the graph uses and
writes the table again with only those (Cargo warns about unused entries).  The paths in the table
are relative to this repository's folder.
"""
import json
import os
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CONFIG = ROOT / ".cargo" / "config.toml"
MARK = "\n# JFLib from a checkout:"
OLD_MARK = "\n# Until the next JFLib release:"     # the table's heading before netlab was on crates.io
HEAD = """
# JFLib from a checkout: every JFLib crate of this program comes from a JFLib checkout beside this
# repository (../JFLib), not from crates.io, to try changes of JFLib here.  All of them, because two
# copies of jflib-rt cannot be linked (links = "jflib_rt").  python tools/jflib_patch.py --remove
# takes this table away again.
[patch.crates-io]
"""


def base():
    s = CONFIG.read_text(encoding="utf-8")
    return s.split(MARK)[0].split(OLD_MARK)[0].rstrip("\n") + "\n"


def main():
    if "--remove" in sys.argv:
        CONFIG.write_text(base(), encoding="utf-8", newline="\n")
        print("jflib_patch: the table is gone")
        return 0
    jflib = Path(os.environ.get("JFLIB", ROOT.parent / "JFLib")).resolve()
    manifest = (jflib / "Cargo.toml").read_text(encoding="utf-8")
    crates = {}
    for m in re.finditer(r'^\S+ = \{ package = "(jflib-[a-z0-9-]+)", path = "([^"]+)"', manifest, re.M):
        crates[m.group(1)] = m.group(2)
    for m in re.finditer(r'^(jflib-[a-z0-9-]+) = \{ path = "([^"]+)"', manifest, re.M):
        crates[m.group(1)] = m.group(2)
    if not crates:
        sys.exit(f"jflib_patch: no crates in {jflib / 'Cargo.toml'}")
    rel = Path(os.path.relpath(jflib, ROOT)).as_posix()

    def write(names):
        body = "".join(f'{n} = {{ path = "{rel}/{crates[n]}" }}\n' for n in sorted(names))
        CONFIG.write_text(base() + HEAD + body, encoding="utf-8", newline="\n")

    write(crates)
    r = subprocess.run(["cargo", "metadata", "--format-version", "1"], cwd=ROOT, capture_output=True, text=True, encoding="utf-8")
    if r.returncode:
        sys.exit(r.stderr[-3000:])
    used = {p["name"] for p in json.loads(r.stdout)["packages"] if p["name"] in crates}
    write(used)
    print(f"jflib_patch: {len(used)} of {len(crates)} JFLib crates from {jflib}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
