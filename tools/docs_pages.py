#!/usr/bin/env python3
"""docs_pages.py -- the explanations "Erklärung öffnen" links: PAGES in src/workspace/docs.rs from the website

    python tools/docs_pages.py            writes PAGES (the newest version of every page)
    python tools/docs_pages.py --check    exit code 1 when PAGES differs from the website

The website (JFLibrary/jfreundWebsite, beside this repository as ../jfreundWebsite, or the folder
JFREUND_WEBSITE names) keeps every explanation as web/content/docs/networker/<devices|cables>/<id>/
with page.json, its published versions. A published version never changes, so this build links
the newest version there is when it is made; an older build keeps its older links, which go on
showing what they showed. Every id must be a model of the catalogue or a cable kind (the website's
catalog.json comes from `jfn-sim catalog`).
"""
import json
import os
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DOCS_RS = ROOT / "src" / "workspace" / "docs.rs"


def website():
    return Path(os.environ.get("JFREUND_WEBSITE", ROOT.parent / "jfreundWebsite")).resolve()


def pages(web):
    docs = web / "web" / "content" / "docs" / "networker"
    cat = json.loads((docs / "catalog.json").read_text(encoding="utf-8"))
    known = {"devices": {m["id"] for m in cat["models"]}, "cables": {c["id"] for c in cat["cables"]}}
    out = []
    for kind in ("devices", "cables"):
        for f in sorted((docs / kind).glob("*/page.json")):
            pid = f.parent.name
            if pid not in known[kind]:
                sys.exit(f"docs_pages: {kind}/{pid} is no {'model' if kind == 'devices' else 'cable kind'} of the catalogue")
            vs = json.loads(f.read_text(encoding="utf-8"))["versions"]
            out.append((kind, pid, vs[-1]["v"]))
    return out


def render(rows):
    body = "".join(f'    ("{k}", "{i}", "{v}"),\n' for k, i, v in rows)
    return ("/// the pages this version of the app links: (devices or cables, the model's or cable's id, the\n"
            "/// version) -- written by tools/docs_pages.py from the website's page.json files\n"
            f"pub(crate) const PAGES: &[(&str, &str, &str)] = &[\n{body}];\n")


def main():
    rows = pages(website())
    src = DOCS_RS.read_text(encoding="utf-8")
    pat = re.compile(r"/// the pages this version of the app links:.*?\];\n", re.S)
    if not pat.search(src):
        sys.exit("docs_pages: PAGES not found in src/workspace/docs.rs")
    new = pat.sub(lambda _: render(rows), src, count=1)
    if "--check" in sys.argv:
        if new != src:
            sys.exit("docs_pages: PAGES differs from the website (python tools/docs_pages.py)")
        print(f"docs_pages: up to date ({len(rows)} pages)")
        return 0
    DOCS_RS.write_text(new, encoding="utf-8", newline="\n")
    print(f"docs_pages: {len(rows)} pages ({sum(r[0] == 'devices' for r in rows)} devices, {sum(r[0] == 'cables' for r in rows)} cables)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
