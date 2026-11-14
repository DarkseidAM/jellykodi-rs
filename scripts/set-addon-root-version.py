#!/usr/bin/env python3
"""Set only the root <addon version="..."> attribute (never <?xml> or <import>)."""

from __future__ import annotations

import re
import sys
from pathlib import Path


def main() -> int:
    if len(sys.argv) != 3:
        print(f"usage: {sys.argv[0]} <addon.xml> <version>", file=sys.stderr)
        return 2
    path = Path(sys.argv[1])
    ver = sys.argv[2]
    text = path.read_text(encoding="utf-8")
    new, n = re.subn(
        r'(<addon\b[^>]*?\bversion=")[^"]+(")',
        rf"\g<1>{ver}\2",
        text,
        count=1,
    )
    if n != 1:
        print(f"expected 1 root addon version bump, got {n}", file=sys.stderr)
        return 1
    path.write_text(new, encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
