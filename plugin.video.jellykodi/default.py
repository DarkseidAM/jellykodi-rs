#!/usr/bin/env python3
"""Thin warm shim: Unix-socket IPC to jellykodi-rs (Jellyfin 12)."""

from __future__ import annotations

import json
import os
import socket
import sys
import time
from urllib.parse import parse_qsl

import xbmc
import xbmcaddon
import xbmcgui
import xbmcplugin
import xbmcvfs

ADDON = xbmcaddon.Addon()
HANDLE = int(sys.argv[1]) if len(sys.argv) > 1 else -1

# Safe VideoInfoTag media types (Python 3.14 / Arctic home widgets).
_SAFE_MEDIA = {
    "movie": "movie",
    "episode": "episode",
    "tvshow": "tvshow",
    "season": "season",
    "musicvideo": "musicvideo",
    "video": "video",
    "set": "movie",
}


def addon_data_path() -> str:
    return xbmcvfs.translatePath(ADDON.getAddonInfo("profile"))


def sock_path() -> str:
    return os.path.join(addon_data_path(), "jellykodi.sock")


def ipc_call(command: str, timeout: float = 25.0, wait_daemon: float = 45.0):
    """Talk to jellykodi-rs. Wait for the socket on boot — Arctic home widgets
    fire GetDirectory before the service has spawned the daemon; failing fast
    leaves NumItems=0 and the right-side rows stay permanently hidden.
    """
    path = sock_path()
    deadline = time.time() + max(0.0, wait_daemon)
    last_err: Exception | None = None
    while True:
        try:
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
        except OSError as exc:
            last_err = exc
            if time.time() >= deadline:
                break
            # Keep the plugin "updating" while we wait so Arctic visibility
            # (NumItems>0 | IsUpdating) does not hide the row forever.
            time.sleep(0.25)
    raise OSError(str(last_err) if last_err else f"daemon socket missing: {path}")


def _fail_dir() -> None:
    # Never Dialog() from widget/plugin directory context — SIGSEGV on Back.
    try:
        xbmcplugin.endOfDirectory(HANDLE, succeeded=False, updateListing=False)
    except Exception:
        pass


def render_items(items, content: str = "videos") -> None:
    if isinstance(items, dict) and items.get("error"):
        _fail_dir()
        return

    try:
        xbmcplugin.setContent(
            HANDLE,
            content
            if content in ("movies", "episodes", "tvshows", "seasons", "videos", "sets")
            else "videos",
        )
    except Exception:
        pass

    dir_items = []
    for it in items or []:
        try:
            url = it.get("plugin_url") or ""
            if not url:
                continue
            is_folder = bool(it.get("is_folder"))
            is_playable = bool(it.get("is_playable")) and not is_folder
            title = it.get("title") or ""
            # Do NOT pass path= into ListItem() — duplicate path with
            # addDirectoryItems has crashed Kodi 22 / Py3.14 on PreviousWindow.
            li = xbmcgui.ListItem(label=title, offscreen=True)
            art = it.get("art") or {}
            if isinstance(art, dict) and art:
                safe_art = {
                    k: v
                    for k, v in art.items()
                    if isinstance(k, str) and isinstance(v, str) and v
                }
                if safe_art:
                    li.setArt(safe_art)
            media = _SAFE_MEDIA.get(str(it.get("media_type") or "video"), "video")
            try:
                tag = li.getVideoInfoTag()
                tag.setMediaType(media)
                tag.setTitle(title)
                plot = it.get("plot") or ""
                if plot:
                    tag.setPlot(plot)
                year = int(it.get("year") or 0)
                if year > 0:
                    tag.setYear(year)
                duration = int(it.get("duration") or 0)
                if duration > 0:
                    tag.setDuration(duration)
                resume = int(it.get("resume_pos") or 0)
                if is_playable and resume > 0 and duration > resume:
                    tag.setResumePoint(float(resume), float(duration))
            except Exception:
                pass
            if is_playable:
                li.setProperty("IsPlayable", "true")
            dir_items.append((url, li, is_folder))
        except Exception:
            continue

    try:
        if dir_items:
            xbmcplugin.addDirectoryItems(HANDLE, dir_items, len(dir_items))
        # Never cacheToDisc — empty boot listings stick forever.
        xbmcplugin.endOfDirectory(
            HANDLE,
            cacheToDisc=False,
            succeeded=bool(dir_items),
            updateListing=False,
        )
    except Exception:
        _fail_dir()


def _widget_content(widget_type: str) -> str:
    t = (widget_type or "").lower()
    if t in ("continue_watching", "resume"):
        return "videos"
    if "episode" in t or t in ("next_up", "tv_next_up"):
        return "episodes"
    if "show" in t or "series" in t:
        return "tvshows"
    if "collection" in t:
        return "sets"
    if "movie" in t:
        return "movies"
    return "videos"


def render_widget(widget_type: str) -> None:
    try:
        items = ipc_call(f"WIDGET {widget_type}")
    except (OSError, TimeoutError, json.JSONDecodeError, ValueError):
        _fail_dir()
        return
    render_items(items, content=_widget_content(widget_type))


def render_browse(item_id: str) -> None:
    if not item_id:
        _fail_dir()
        return
    try:
        items = ipc_call(f"BROWSE {item_id}", timeout=30.0)
    except (OSError, TimeoutError, json.JSONDecodeError, ValueError):
        _fail_dir()
        return
    content = "videos"
    if isinstance(items, list) and items:
        mt = str(items[0].get("media_type") or "")
        content = {
            "movie": "movies",
            "episode": "episodes",
            "season": "seasons",
            "tvshow": "tvshows",
            "set": "sets",
        }.get(mt, "videos")
    render_items(items, content=content)


def resolve_play(item_id: str) -> None:
    if not item_id:
        xbmcplugin.setResolvedUrl(HANDLE, False, xbmcgui.ListItem())
        return
    try:
        play = ipc_call(f"PLAY {item_id}", timeout=30.0, wait_daemon=10.0)
    except (OSError, TimeoutError, json.JSONDecodeError, ValueError) as exc:
        try:
            xbmcgui.Dialog().notification(
                "JellyKodi", f"Play failed: {exc}", xbmcgui.NOTIFICATION_ERROR, 4000
            )
        except Exception:
            pass
        xbmcplugin.setResolvedUrl(HANDLE, False, xbmcgui.ListItem())
        return

    if not isinstance(play, dict) or play.get("error") or not play.get("stream_url"):
        err = play.get("error") if isinstance(play, dict) else "bad play response"
        try:
            xbmcgui.Dialog().notification(
                "JellyKodi", str(err), xbmcgui.NOTIFICATION_ERROR, 4000
            )
        except Exception:
            pass
        xbmcplugin.setResolvedUrl(HANDLE, False, xbmcgui.ListItem())
        return

    li = xbmcgui.ListItem(path=play["stream_url"], offscreen=True)
    subs = play.get("subtitles") or []
    if subs:
        try:
            li.setSubtitles(subs)
        except Exception:
            pass
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
            _fail_dir()
            return
        term = (kb.getText() or "").strip()
    if not term:
        _fail_dir()
        return
    try:
        items = ipc_call(f"SEARCH {term}", timeout=30.0)
    except (OSError, TimeoutError, json.JSONDecodeError, ValueError):
        _fail_dir()
        return
    render_items(items, content="videos")


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
            st = ipc_call("STATUS", timeout=2.0, wait_daemon=5.0)
            xbmcgui.Dialog().ok("JellyKodi status", json.dumps(st, indent=2)[:3500])
        except Exception as exc:
            try:
                xbmcgui.Dialog().ok("JellyKodi status", f"offline: {exc}")
            except Exception:
                pass
        try:
            xbmcplugin.endOfDirectory(HANDLE)
        except Exception:
            pass
    elif mode == "resync":
        try:
            ipc_call("RESYNC", timeout=2.0, wait_daemon=5.0)
            xbmcgui.Dialog().notification(
                "JellyKodi", "Full resync queued", xbmcgui.NOTIFICATION_INFO, 3000
            )
        except Exception as exc:
            try:
                xbmcgui.Dialog().notification(
                    "JellyKodi", str(exc), xbmcgui.NOTIFICATION_ERROR, 4000
                )
            except Exception:
                pass
        try:
            xbmcplugin.endOfDirectory(HANDLE)
        except Exception:
            pass
    else:
        render_widget(params.get("type", "continue_watching"))


if __name__ == "__main__":
    try:
        main()
    except Exception:
        _fail_dir()
