#!/usr/bin/env python3
"""Context actions for jellykodi list items."""

from __future__ import annotations

import json
import os
import socket
import sys
from urllib.parse import parse_qsl

import xbmc
import xbmcgui
import xbmcvfs


def _item_id(path: str) -> str:
    if "?" not in path:
        return ""
    params = dict(parse_qsl(path.split("?", 1)[1]))
    return params.get("id", "")


def _ipc(command: str) -> dict:
    addon = xbmcaddon_addon()
    profile = xbmcvfs.translatePath(addon.getAddonInfo("profile"))
    sock_path = os.path.join(profile, "jellykodi.sock")
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as s:
        s.settimeout(8.0)
        s.connect(sock_path)
        s.sendall(command.encode("utf-8") + b"\n")
        raw = s.recv(65536)
    payload = json.loads(raw.decode("utf-8").strip() or "{}")
    if payload.get("error"):
        raise RuntimeError(payload["error"])
    return payload


def xbmcaddon_addon():
    import xbmcaddon

    return xbmcaddon.Addon()


def main() -> None:
    item = getattr(sys, "listitem", None)
    path = ""
    if item is not None:
        try:
            path = item.getPath() or ""
        except Exception:
            path = ""
    if "plugin.video.jellykodi" not in path:
        return
    item_id = _item_id(path)
    if not item_id:
        return
    choice = xbmcgui.Dialog().select("JellyKodi", ["Play version...", "Mark watched", "Add favorite"])
    try:
        if choice == 0:
            play = _ipc(f"PLAY {item_id}")
            avail = play.get("available_sources") or []
            if len(avail) <= 1:
                xbmcgui.Dialog().notification("JellyKodi", "Only 1 version available", xbmcgui.NOTIFICATION_INFO, 2500)
                return
            labels = [s.get("name") or s.get("id") or "Version" for s in avail]
            v_choice = xbmcgui.Dialog().select("Choose Version", labels)
            if v_choice < 0:
                return
            chosen_source = avail[v_choice].get("id")
            xbmc.Player().play(f"plugin://plugin.video.jellykodi/?mode=play&id={item_id}&source={chosen_source}")
        elif choice == 1:
            _ipc(f"PLAYED {item_id}")
            xbmcgui.Dialog().notification("JellyKodi", "Marked watched", xbmcgui.NOTIFICATION_INFO, 2500)
        elif choice == 2:
            _ipc(f"FAVORITE {item_id}")
            xbmcgui.Dialog().notification("JellyKodi", "Favorite updated", xbmcgui.NOTIFICATION_INFO, 2500)
    except Exception as exc:
        xbmcgui.Dialog().notification("JellyKodi", str(exc), xbmcgui.NOTIFICATION_ERROR, 3500)


if __name__ == "__main__":
    main()
