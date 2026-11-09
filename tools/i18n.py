#!/usr/bin/env python3
"""i18n.py -- the window's texts against their catalogues: every German text has its English row

    python tools/i18n.py              src/, crates/ and sim/ against src/locales/*.tsv (exit code 1 on a gap)
    python tools/i18n.py --update     adds a row for every missing text (English left empty)
    python tools/i18n.py --unused     also lists rows no source uses any more

Runs JFLib's tools/i18n_check.py from a JFLib checkout (JFLIB, else ../JFLib beside this repository).
That tool expects its apps inside JFLib; this hands it a base folder above both repositories and
the library's own catalogue (LIB_TEXTS of lib/ui/i18n.rs) from the checkout.
"""
import importlib.util
import os
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def main():
    jflib = Path(os.environ.get("JFLIB", ROOT.parent / "JFLib")).resolve()
    tool = jflib / "tools" / "i18n_check.py"
    if not tool.exists():
        sys.exit(f"i18n: {tool} is missing (a JFLib checkout: JFLIB, else ../JFLib)")
    spec = importlib.util.spec_from_file_location("i18n_check", tool)
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    lib_catalogue = m.lib_catalogue

    def lib_from_jflib():
        base, m.ROOT = m.ROOT, jflib
        try:
            return lib_catalogue()
        finally:
            m.ROOT = base

    m.lib_catalogue = lib_from_jflib
    m.ROOT = Path(os.path.commonpath([jflib, ROOT]))
    return m.main([str(ROOT), "src", "crates", "sim", *sys.argv[1:]])


if __name__ == "__main__":
    sys.exit(main())
