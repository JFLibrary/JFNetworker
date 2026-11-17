#!/usr/bin/env python3
"""release.py -- the version, the changelog and the release notes of JFNetworker (docs/RELEASE.md)

    python tools/release/release.py prepare X.Y.Z [--date YYYY-MM-DD]
        [Unreleased] of CHANGELOG.md becomes ## [X.Y.Z] - DATE (a new empty [Unreleased] above it),
        version= in PACKAGE and the program's version in Cargo.toml and Cargo.lock become X.Y.Z
    python tools/release/release.py check vX.Y.Z
        exit code 1 unless the tag, PACKAGE, Cargo.toml and a section ## [X.Y.Z] of CHANGELOG.md agree
    python tools/release/release.py notes X.Y.Z [--out FILE] [--gpg] [--notarized]
        the section of X.Y.Z as the text of the GitHub release: the downloads, the note about the
        unsigned setup (--gpg: SHA256SUMS.asc is there, --notarized: so is the dmg)

Only the program's own version changes: the crates in crates/ and sim/ keep theirs.
"""
import argparse
import datetime
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
VERSION = re.compile(r"^\d+\.\d+\.\d+$")

DOWNLOADS = """### Downloads

| File | System |
|---|---|
| `JFNetworker-{v}-universal.dmg` | macOS 13 or newer, Apple silicon and Intel{mac} |
| `jfnetworker_{v}_amd64.deb` | Debian, Ubuntu and their relatives, x86-64 |
| `JFNetworker-{v}-netlab-x86_64.AppImage` | other Linux systems, x86-64 |
| `JFNetworker_Setup.exe` | Windows 10 and 11, x86-64 |
| `jfn-sim-{v}-*` | the command-line runner alone |

The Windows setup is **not signed** yet, so Windows SmartScreen warns about an unknown publisher
("More info", then "Run anyway"). To make sure a file is the one built here, compare its SHA-256
with `SHA256SUMS`{gpg} or check the build's attestation:
`gh attestation verify JFNetworker_Setup.exe --repo JFLibrary/JFNetworker`.
"""


def read(p):
    return (ROOT / p).read_bytes().decode("utf-8")


def write(p, s):
    (ROOT / p).write_bytes(s.encode("utf-8"))


def nl_of(s):
    return "\r\n" if "\r\n" in s else "\n"


def package_version():
    m = re.search(r"^version=(.+?)\s*$", read("PACKAGE"), re.M)
    return m.group(1) if m else None


def cargo_version():
    s = read("Cargo.toml")
    m = re.search(r"^\[package\]\s*$(.*?)^\[", s, re.M | re.S)
    v = re.search(r'^version\s*=\s*"([^"]+)"', m.group(1), re.M) if m else None
    return v.group(1) if v else None


def section(v):
    """the text of ## [v] in CHANGELOG.md (None: there is none)"""
    s = read("CHANGELOG.md").replace("\r\n", "\n")
    m = re.search(rf"^## \[{re.escape(v)}\][^\n]*\n(.*?)(?=^## \[|\Z)", s, re.M | re.S)
    return m.group(1).strip() if m else None


def prepare(v, date):
    if not VERSION.match(v):
        sys.exit(f"release: {v} is no version X.Y.Z")
    if section(v) is not None:
        sys.exit(f"release: CHANGELOG.md has a section {v} already")
    s = read("CHANGELOG.md")
    nl = nl_of(s)
    head = "## [Unreleased]"
    if s.count(head + nl) != 1:
        sys.exit("release: CHANGELOG.md has no single ## [Unreleased]")
    body = s.split(head + nl, 1)[1]
    nxt = re.search(r"^## \[", body, re.M)
    if not (body[: nxt.start()] if nxt else body).strip():
        sys.exit("release: [Unreleased] is empty: nothing to release")
    write("CHANGELOG.md", s.replace(head + nl, f"{head}{nl}{nl}## [{v}] - {date}{nl}", 1))
    p = read("PACKAGE")
    write("PACKAGE", re.sub(r"^version=.*$", f"version={v}", p, count=1, flags=re.M))
    c = read("Cargo.toml")
    m = re.search(r"^\[package\]\s*$.*?^version\s*=\s*\"[^\"]+\"", c, re.M | re.S)
    if not m:
        sys.exit("release: no version in [package] of Cargo.toml")
    old = m.group(0)
    write("Cargo.toml", c.replace(old, re.sub(r'version\s*=\s*"[^"]+"', f'version = "{v}"', old), 1))
    lock = read("Cargo.lock")
    lock2, n = re.subn(r'(\[\[package\]\]\r?\nname = "JFNetworker"\r?\nversion = )"[^"]+"', rf'\1"{v}"', lock, count=1)
    if n != 1:
        sys.exit("release: JFNetworker is not in Cargo.lock")
    write("Cargo.lock", lock2)
    print(f"release: {v} of {date} in CHANGELOG.md, PACKAGE, Cargo.toml and Cargo.lock")


def check(tag):
    v = tag[1:] if tag.startswith("v") else tag
    problems = []
    if not VERSION.match(v):
        problems.append(f"the tag {tag} is no vX.Y.Z")
    if package_version() != v:
        problems.append(f"PACKAGE says version={package_version()}")
    if cargo_version() != v:
        problems.append(f"Cargo.toml says version = \"{cargo_version()}\"")
    if not section(v):
        problems.append(f"CHANGELOG.md has no section ## [{v}] (or it is empty)")
    if problems:
        sys.exit(f"release: {tag} does not fit:\n  " + "\n  ".join(problems))
    print(f"release: {tag} fits PACKAGE, Cargo.toml and CHANGELOG.md")


def notes(v, out, gpg, notarized):
    text = section(v)
    if not text:
        sys.exit(f"release: CHANGELOG.md has no section {v}")
    body = text + "\n\n" + DOWNLOADS.format(
        v=v, mac=" (signed and notarized)" if notarized else " (not notarized yet: System Settings › Privacy & Security › Open Anyway)",
        gpg=" (signed in `SHA256SUMS.asc`; the key is on networker.jfreund.org)" if gpg else "")
    if out:
        Path(out).write_text(body, encoding="utf-8")
    else:
        sys.stdout.write(body)


def main():
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("prepare")
    p.add_argument("version")
    p.add_argument("--date", default=datetime.date.today().isoformat())
    c = sub.add_parser("check")
    c.add_argument("tag")
    n = sub.add_parser("notes")
    n.add_argument("version")
    n.add_argument("--out")
    n.add_argument("--gpg", action="store_true", help="SHA256SUMS.asc is there")
    n.add_argument("--notarized", action="store_true", help="the dmg is signed and notarized")
    a = ap.parse_args()
    if a.cmd == "prepare":
        prepare(a.version, a.date)
    elif a.cmd == "check":
        check(a.tag)
    else:
        notes(a.version, a.out, a.gpg, a.notarized)


if __name__ == "__main__":
    main()
