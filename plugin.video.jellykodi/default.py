#!/usr/bin/env python3
"""Thin warm shim: Unix-socket IPC to jellykodi-rs (Jellyfin 12)."""

from __future__ import annotations

import json
import os
import socket
import sys
from urllib.parse import parse_qsl

import xbmc
import xbmcaddon
import xbmcgui
import xbmcplugin
import xbmcvfs

ADDON = xbmcaddon.Addon()
HANDLE = int(sys.argv[1]) if len(sys.argv) > 1 else -1


def addon_data_path() -> str:
    return xbmcvfs.translatePath(ADDON.getAddonInfo("profile"))


def sock_path() -> str:
    return os.path.join(addon_data_path(), "jellykodi.sock")


def ipc_call(command: str, timeout: float = 25.0):
    path = sock_path()
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as s:
        s.settimeout(timeout)
        s.connect(path)
        s.sendall(command.encode("utf-8") + b"\n")
        chunks = []
        while True:
            chunk = s.recv(65536)
            if not chunk:
                break
            chunks.append(chunk)
            if b"\n" in chunk:
                break
    raw = b"".join(chunks).strip()
    return json.loads(raw.decode("utf-8"))


def render_items(items) -> None:
    if isinstance(items, dict) and items.get("error"):
        xbmcgui.Dialog().notification("JellyKodi", items["error"], xbmcgui.NOTIFICATION_ERROR, 4000)
        xbmcplugin.endOfDirectory(HANDLE, succeeded=False)
        return

    xbmcplugin.setContent(HANDLE, "videos")
    dir_items = []
    for it in items or []:
        url = it.get("plugin_url") or ""
        is_folder = bool(it.get("is_folder"))
        is_playable = bool(it.get("is_playable")) and not is_folder
        li = xbmcgui.ListItem(label=it.get("title", ""), path=url, offscreen=True)
        li.setArt(it.get("art") or {})
        tag = li.getVideoInfoTag()
        media = it.get("media_type") or "video"
        if media == "set":
            media = "set"
        tag.setMediaType(media)
        tag.setTitle(it.get("title") or "")
        tag.setPlot(it.get("plot") or "")
        tag.setYear(int(it.get("year") or 0))
        tag.setDuration(int(it.get("duration") or 0))
        resume = int(it.get("resume_pos") or 0)
        duration = int(it.get("duration") or 0)
        if resume > 0 and duration > 0:
            tag.setResumePoint(float(resume), float(duration))
        if is_playable:
            li.setProperty("IsPlayable", "true")
            li.setProperty("IsFolder", "false")
        dir_items.append((url, li, is_folder))

    xbmcplugin.addDirectoryItems(HANDLE, dir_items, len(dir_items))
    xbmcplugin.endOfDirectory(HANDLE, cacheToDisc=False)


def render_widget(widget_type: str) -> None:
    try:
        items = ipc_call(f"WIDGET {widget_type}")
    except OSError as exc:
        xbmcgui.Dialog().notification(
            "JellyKodi",
            f"Daemon offline: {exc}",
            xbmcgui.NOTIFICATION_ERROR,
            4000,
        )
        xbmcplugin.endOfDirectory(HANDLE, succeeded=False)
        return
    render_items(items)


def render_browse(item_id: str) -> None:
    try:
        items = ipc_call(f"BROWSE {item_id}", timeout=30.0)
    except OSError as exc:
        xbmcgui.Dialog().notification(
            "JellyKodi",
            f"Browse failed: {exc}",
            xbmcgui.NOTIFICATION_ERROR,
            4000,
        )
        xbmcplugin.endOfDirectory(HANDLE, succeeded=False)
        return
    render_items(items)


def resolve_play(item_id: str) -> None:
    try:
        play = ipc_call(f"PLAY {item_id}", timeout=30.0)
    except OSError as exc:
        xbmcgui.Dialog().notification("JellyKodi", f"Play failed: {exc}", xbmcgui.NOTIFICATION_ERROR, 5000)
        xbmcplugin.setResolvedUrl(HANDLE, False, xbmcgui.ListItem())
        return

    if play.get("error"):
        xbmcgui.Dialog().notification("JellyKodi", play["error"], xbmcgui.NOTIFICATION_ERROR, 5000)
        xbmcplugin.setResolvedUrl(HANDLE, False, xbmcgui.ListItem())
        return

    li = xbmcgui.ListItem(path=play["stream_url"], offscreen=True)
    subs = play.get("subtitles") or []
    if subs:
        li.setSubtitles(subs)
    # Stash ids for service.py progress reporting.
    li.setProperty("jellykodi.item_id", play.get("item_id") or item_id)
    li.setProperty("jellykodi.media_source_id", play.get("media_source_id") or "")
    li.setProperty("jellykodi.play_session_id", play.get("play_session_id") or "")
    xbmcplugin.setResolvedUrl(HANDLE, True, li)


def run_search(query: str = "") -> None:
    term = (query or "").strip()
    if not term:
        kb = xbmc.Keyboard("", "JellyKodi Search")
        kb.doModal()
        if not kb.isConfirmed():
            xbmcplugin.endOfDirectory(HANDLE, succeeded=False)
            return
        term = (kb.getText() or "").strip()
    if not term:
        xbmcplugin.endOfDirectory(HANDLE, succeeded=False)
        return
    try:
        items = ipc_call(f"SEARCH {term}", timeout=30.0)
    except OSError as exc:
        xbmcgui.Dialog().notification(
            "JellyKodi", f"Search failed: {exc}", xbmcgui.NOTIFICATION_ERROR, 4000
        )
        xbmcplugin.endOfDirectory(HANDLE, succeeded=False)
        return
    render_items(items)


def main() -> None:
    params = dict(parse_qsl(sys.argv[2].lstrip("?"))) if len(sys.argv) > 2 else {}
    mode = params.get("mode", "widget")
    if mode == "play":
        resolve_play(params.get("id", ""))
    elif mode == "browse":
        render_browse(params.get("id", ""))
    elif mode == "search":
        run_search(params.get("q", ""))
    elif mode == "status":
        try:
            st = ipc_call("STATUS", timeout=2.0)
            xbmcgui.Dialog().ok("JellyKodi status", json.dumps(st, indent=2)[:3500])
        except OSError as exc:
            xbmcgui.Dialog().ok("JellyKodi status", f"offline: {exc}")
        xbmcplugin.endOfDirectory(HANDLE)
    elif mode == "resync":
        try:
            ipc_call("RESYNC", timeout=2.0)
            xbmcgui.Dialog().notification("JellyKodi", "Full resync queued", xbmcgui.NOTIFICATION_INFO, 3000)
        except OSError as exc:
            xbmcgui.Dialog().notification("JellyKodi", str(exc), xbmcgui.NOTIFICATION_ERROR, 4000)
        xbmcplugin.endOfDirectory(HANDLE)
    else:
        render_widget(params.get("type", "continue_watching"))


if __name__ == "__main__":
    main()
