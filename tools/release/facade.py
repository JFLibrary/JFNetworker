#!/usr/bin/env python3
"""facade.py -- for `cargo jflib package` only: the crate jflib as a dependency of the program

cargo-jflib 0.2.1 reads the JFLib version from the facade crate jflib (it builds the installer stub
and the AppImage runtime at that version) and refuses an app that does not depend on it:
"JFNetworker does not depend on the crate jflib".  JFNetworker uses the single crates (jflib-ui,
jflib-netpkt, ...), and the facade would pull all of JFLib into every build, so the release
workflow adds it in its own checkout just before packaging, at the version of jflib-rt.  Nothing
in the program uses it, so the binary does not change.  Gap 1 of docs/RELEASE.md: drop this once
cargo-jflib takes the version from jflib-rt too.

    python tools/release/facade.py      edits Cargo.toml in place (never commit the result)
"""
import re
import sys
from pathlib import Path

p = Path(__file__).resolve().parents[2] / "Cargo.toml"
s = p.read_bytes().decode("utf-8")
m = re.search(r'jflib-rt", version = "=([^"]+)"', s)
if not m:
    sys.exit("facade: no jflib-rt version in Cargo.toml")
if re.search(r"^jflib\s*=", s, re.M):
    print("facade: Cargo.toml has jflib already")
    sys.exit(0)
s2, n = re.subn(r"^(script\.workspace = true)(\r?\n)", rf'\1\2jflib = "={m.group(1)}"\2', s, count=1, flags=re.M)
if n != 1:
    sys.exit("facade: no line script.workspace = true in [dependencies]")
p.write_bytes(s2.encode("utf-8"))
print(f"facade: jflib = \"={m.group(1)}\" added for packaging")
