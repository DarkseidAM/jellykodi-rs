#!/usr/bin/env python3
"""Assemble a Kodi repository tree for GitHub Pages + release assets."""

from __future__ import annotations

import argparse
import hashlib
import shutil
import sys
import xml.etree.ElementTree as ET
import zipfile
from pathlib import Path


def addon_id_version(addon_xml: Path) -> tuple[str, str]:
    root = ET.parse(addon_xml).getroot()
    return root.attrib["id"], root.attrib["version"]


def addon_xml_fragment(addon_xml: Path) -> str:
    root = ET.parse(addon_xml).getroot()
    # Pretty-ish single addon element for addons.xml
    return ET.tostring(root, encoding="unicode")


def write_zip(src_dir: Path, zip_path: Path) -> None:
    zip_path.parent.mkdir(parents=True, exist_ok=True)
    if zip_path.exists():
        zip_path.unlink()
    with zipfile.ZipFile(zip_path, "w", compression=zipfile.ZIP_DEFLATED) as zf:
        for path in sorted(src_dir.rglob("*")):
            if path.is_file():
                zf.write(path, path.relative_to(src_dir.parent).as_posix())


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument(
        "--plugin-zip",
        type=Path,
        required=True,
        help="Already-built plugin.video.jellykodi-VERSION.zip (contains bin/jellykodi-rs)",
    )
    ap.add_argument("--repo-src", type=Path, default=Path("repository.jellykodi"))
    ap.add_argument("--out", type=Path, default=Path("repo-dist"))
    args = ap.parse_args()

    if not args.plugin_zip.is_file():
        print(f"missing plugin zip: {args.plugin_zip}", file=sys.stderr)
        return 1
    if not (args.repo_src / "addon.xml").is_file():
        print(f"missing {args.repo_src}/addon.xml", file=sys.stderr)
        return 1

    out = args.out
    if out.exists():
        shutil.rmtree(out)
    out.mkdir(parents=True)

    # Unpack plugin zip to read addon.xml + copy zip into datadir layout
    stage = out / "_stage"
    stage.mkdir()
    with zipfile.ZipFile(args.plugin_zip) as zf:
        zf.extractall(stage)
    plugin_dirs = [p for p in stage.iterdir() if p.is_dir()]
    if len(plugin_dirs) != 1:
        print(f"expected one top-level addon dir in {args.plugin_zip}", file=sys.stderr)
        return 1
    plugin_dir = plugin_dirs[0]
    plugin_id, plugin_ver = addon_id_version(plugin_dir / "addon.xml")
    plugin_dest = out / plugin_id / f"{plugin_id}-{plugin_ver}.zip"
    plugin_dest.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(args.plugin_zip, plugin_dest)

    repo_id, repo_ver = addon_id_version(args.repo_src / "addon.xml")
    repo_zip = out / repo_id / f"{repo_id}-{repo_ver}.zip"
    write_zip(args.repo_src, repo_zip)

    # Also emit flat copies for GitHub Release assets
    shutil.copy2(plugin_dest, out / f"{plugin_id}-{plugin_ver}.zip")
    shutil.copy2(repo_zip, out / f"{repo_id}-{repo_ver}.zip")

    fragments = [
        addon_xml_fragment(plugin_dir / "addon.xml"),
        addon_xml_fragment(args.repo_src / "addon.xml"),
    ]
    addons_xml = '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n<addons>\n'
    for frag in fragments:
        addons_xml += frag + "\n"
    addons_xml += "</addons>\n"
    (out / "addons.xml").write_text(addons_xml, encoding="utf-8")
    md5 = hashlib.md5(addons_xml.encode("utf-8")).hexdigest()
    (out / "addons.xml.md5").write_text(md5 + "\n", encoding="utf-8")

    (out / "index.html").write_text(
        f"""<!DOCTYPE html>
<html lang="en">
<head><meta charset="utf-8"><title>JellyKodi Repository</title></head>
<body>
<h1>JellyKodi Repository</h1>
<p>Install <code>{repo_id}-{repo_ver}.zip</code> once in Kodi
(<strong>Add-ons → Install from zip file</strong>), then install/update
<code>{plugin_id}</code> from that repository. Auto-updates use this Pages feed.</p>
<ul>
  <li><a href="{repo_id}/{repo_id}-{repo_ver}.zip">{repo_id}-{repo_ver}.zip</a></li>
  <li><a href="{plugin_id}/{plugin_id}-{plugin_ver}.zip">{plugin_id}-{plugin_ver}.zip</a></li>
  <li><a href="addons.xml">addons.xml</a> (<a href="addons.xml.md5">md5</a>)</li>
</ul>
<p>Source: <a href="https://github.com/DarkseidAM/jellykodi-rs">DarkseidAM/jellykodi-rs</a></p>
</body>
</html>
""",
        encoding="utf-8",
    )

    shutil.rmtree(stage)
    print(f"repo tree ready under {out} ({plugin_id} {plugin_ver}, {repo_id} {repo_ver})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
