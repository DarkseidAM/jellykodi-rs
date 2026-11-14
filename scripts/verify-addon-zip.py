#!/usr/bin/env python3
"""Fail if packaging clobbered import / XML declaration versions."""

from __future__ import annotations

import sys
import zipfile
import xml.etree.ElementTree as ET


def main() -> int:
    if len(sys.argv) != 2:
        print(f"usage: {sys.argv[0]} <plugin.zip>", file=sys.stderr)
        return 2
    with zipfile.ZipFile(sys.argv[1]) as zf:
        raw = zf.read("plugin.video.jellykodi/addon.xml")
    if raw.startswith(b"<?xml") and b'version="1.0"' not in raw.split(b"?>", 1)[0]:
        print("xml declaration version was clobbered", file=sys.stderr)
        return 1
    root = ET.fromstring(raw)
    addon_ver = root.attrib["version"]
    requires = root.find("requires")
    if requires is None:
        print("missing <requires>", file=sys.stderr)
        return 1
    for imp in requires:
        aid = imp.attrib.get("addon", "")
        ver = imp.attrib.get("version", "")
        if aid == "xbmc.python":
            if not ver.startswith("3."):
                print(f"xbmc.python version invalid: {ver}", file=sys.stderr)
                return 1
            if ver == addon_ver:
                print("xbmc.python version clobbered to addon version", file=sys.stderr)
                return 1
        if aid == "repository.jellykodi" and ver == addon_ver:
            print("repository.jellykodi version clobbered to addon version", file=sys.stderr)
            return 1
    print(f"addon.xml versions ok: {addon_ver}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
