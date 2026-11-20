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

                streams = it.get("streams") or {}
                if isinstance(streams, dict) and streams:
                    video = streams.get("video")
                    if isinstance(video, dict):
                        v_codec = str(video.get("codec") or "")
                        v_w = int(video.get("width") or 0)
                        v_h = int(video.get("height") or 0)
                        v_aspect = float(video.get("aspect") or 0.0)
                        v_hdr = str(video.get("hdr_type") or "")
                        try:
                            tag.addVideoStream(
                                xbmc.VideoStreamDetail(
                                    width=v_w,
                                    height=v_h,
                                    codec=v_codec,
                                    aspect=v_aspect,
                                    duration=duration,
                                    hdrtype=v_hdr,
                                )
                            )
                        except Exception:
                            try:
                                tag.addVideoStream(
                                    xbmc.VideoStreamDetail(
                                        width=v_w,
                                        height=v_h,
                                        codec=v_codec,
                                        aspect=v_aspect,
                                        duration=duration,
                                    )
                                )
                            except Exception:
                                pass
                        try:
                            li.addStreamInfo(
                                "video",
                                {
                                    "codec": v_codec,
                                    "width": v_w,
                                    "height": v_h,
                                    "aspect": v_aspect,
                                    "duration": duration,
                                    "hdrtype": v_hdr,
                                },
                            )
                        except Exception:
                            pass
                        if v_hdr:
                            li.setProperty("video.hdrtype", v_hdr)
                    audio = streams.get("audio")
                    if isinstance(audio, dict):
                        a_codec = str(audio.get("codec") or "")
                        a_ch = int(audio.get("channels") or 0)
                        a_lang = str(audio.get("language") or "")
                        try:
                            tag.addAudioStream(
                                xbmc.AudioStreamDetail(
                                    codec=a_codec,
                                    channels=a_ch,
                                    language=a_lang,
                                )
                            )
                        except Exception:
                            pass
                        try:
                            li.addStreamInfo(
                                "audio",
                                {
                                    "codec": a_codec,
                                    "channels": a_ch,
                                    "language": a_lang,
                                },
                            )
                        except Exception:
                            pass
            except Exception:
                pass

            # Rich metadata (Rating, Genres, Studios, MPAA, Cast, UniqueIDs)
            try:
                tag = li.getVideoInfoTag()
                genres = it.get("genres") or []
                if genres:
                    tag.setGenres(genres)
                studios = it.get("studios") or []
                if studios:
                    tag.setStudios(studios)
                mpaa = it.get("mpaa") or ""
                if mpaa:
                    tag.setMpaa(mpaa)
                rating = float(it.get("community_rating") or 0.0)
                if rating > 0.0:
                    tag.setRating(rating, 0, "imdb", True)
                sort_title = it.get("sort_title") or ""
                if sort_title:
                    tag.setSortTitle(sort_title)
                tagline = it.get("tagline") or ""
                if tagline:
                    tag.setTagLine(tagline)

                cast_list = it.get("cast") or []
                if cast_list:
                    actors = [
                        xbmc.Actor(
                            name=c.get("name", ""),
                            role=c.get("role", ""),
                            order=int(c.get("order") or 0),
                            thumbnail=c.get("thumbnail", ""),
                        )
                        for c in cast_list
                        if c.get("name")
                    ]
                    tag.setCast(actors)
            except Exception:
                pass

            unique_ids = it.get("unique_ids") or {}
            if unique_ids and isinstance(unique_ids, dict):
                try:
                    li.setUniqueIDs(unique_ids, defaultrating="imdb")
                except Exception:
                    pass

            if media == "movie" and year > 0:
                li.setLabel2(str(year))
            elif media == "episode":
                s = int(it.get("season") or 0)
                e = int(it.get("episode") or 0)
                if s > 0 and e > 0:
                    li.setLabel2(f"S{s:02d}E{e:02d}")

            if is_playable:
                li.setProperty("IsPlayable", "true")
            elif is_folder:
                li.setProperty("IsPlayable", "false")
                li.setProperty("IsFolder", "true")

            dir_items.append((url, li, is_folder))
        except Exception:
            continue

    try:
        if dir_items:
            xbmcplugin.addDirectoryItems(HANDLE, dir_items, len(dir_items))
        try:
            xbmcplugin.addSortMethod(HANDLE, xbmcplugin.SORT_METHOD_UNSORTED)
        except Exception:
            pass
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

    resume_pos = int(play.get("resume_pos_secs") or 0)
    duration = int(play.get("duration_secs") or 0)
    start_offset = 0

    # C MODE: Native Kodi Library click (videodb://) - Kodi handles bookmark dialog and sets sys.argv[3]
    if len(sys.argv) > 3 and "resume:" in sys.argv[3]:
        is_resume = sys.argv[3].split("resume:")[1].lower() == "true"
        start_offset = resume_pos if is_resume else 0
    # A MODE: Direct Jellyfin widget click (plugin://) - Context Menu Dialog
    elif resume_pos > 0 and duration > resume_pos:
        mins, secs = divmod(resume_pos, 60)
        hours, mins = divmod(mins, 60)
        time_str = f"{hours}:{mins:02d}:{secs:02d}" if hours else f"{mins}:{secs:02d}"
        choice = xbmcgui.Dialog().contextmenu([f"Resume from {time_str}", "Play from beginning"])
        if choice == -1:  # Cancelled / Back
            xbmcplugin.setResolvedUrl(HANDLE, False, xbmcgui.ListItem())
            return
        start_offset = resume_pos if choice == 0 else 0

    li = xbmcgui.ListItem(path=play["stream_url"], offscreen=True)
    li.setProperty("IsPlayable", "true")
    li.setProperty("StartOffset", str(start_offset))

    # Video Info Tag on resolved ListItem for OSD display
    try:
        tag = li.getVideoInfoTag()
        tag.setTitle(play.get("title") or "")
        tag.setMediaType(play.get("media_type") or "video")
        if play.get("plot"):
            tag.setPlot(play["plot"])
        if play.get("year"):
            tag.setYear(int(play["year"]))
        if duration > 0:
            tag.setDuration(duration)
        if resume_pos > 0:
            tag.setResumePoint(float(resume_pos), float(duration))
    except Exception:
        pass

    # Add stream tracks for Player OSD Track Selector
    streams = play.get("streams") or {}
    if isinstance(streams, dict):
        for vt in streams.get("video") or []:
            try:
                li.addStreamInfo(
                    "video",
                    {
                        "codec": vt.get("codec", ""),
                        "width": int(vt.get("width") or 0),
                        "height": int(vt.get("height") or 0),
                        "aspect": float(vt.get("aspect") or 1.78),
                        "duration": duration,
                        "hdrtype": vt.get("hdr_type", ""),
                    },
                )
            except Exception:
                pass
        for at in streams.get("audio") or []:
            try:
                li.addStreamInfo(
                    "audio",
                    {
                        "codec": at.get("codec", ""),
                        "channels": int(at.get("channels") or 2),
                        "language": at.get("language", ""),
                    },
                )
            except Exception:
                pass
        for st in streams.get("subtitle") or []:
            try:
                li.addStreamInfo(
                    "subtitle",
                    {
                        "language": st.get("language", ""),
                    },
                )
            except Exception:
                pass

    # External Subtitles (Option 1: direct URL streaming)
    subs = []
    for s in play.get("subtitles") or []:
        if isinstance(s, dict) and s.get("url"):
            subs.append(s["url"])
        elif isinstance(s, str) and s:
            subs.append(s)
    if subs:
        try:
            li.setSubtitles(subs)
        except Exception:
            pass

    # Bridge session tracking to service.py via HomeWindow
    try:
        win = xbmcgui.Window(10000)
        win.setProperty("jellykodi.item_id", play.get("item_id") or item_id)
        win.setProperty("jellykodi.media_source_id", play.get("media_source_id") or "")
        win.setProperty("jellykodi.play_session_id", play.get("play_session_id") or "")
        win.setProperty("jellykodi.stream_url", play["stream_url"])
        win.setProperty("jellykodi.duration", str(duration))
    except Exception:
        pass

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
