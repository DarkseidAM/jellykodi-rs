#!/usr/bin/env python3
"""Start jellykodi-rs and own a single xbmc.Player for JF12 progress writeback."""

from __future__ import annotations

import json
import os
import signal
import socket
import subprocess
import time

import xbmc
import xbmcaddon
import xbmcgui
import xbmcvfs

ADDON = xbmcaddon.Addon()
xbmc.log("[jellykodi] service module loaded", xbmc.LOGINFO)


def profile() -> str:
    return xbmcvfs.translatePath(ADDON.getAddonInfo("profile"))


def sock_path() -> str:
    return os.path.join(profile(), "jellykodi.sock")


def config_path() -> str:
    return os.path.join(profile(), "config.json")


def binary_path() -> str:
    base = xbmcvfs.translatePath(ADDON.getAddonInfo("path"))
    return os.path.join(base, "bin", "jellykodi-rs")


def daemon_log_path() -> str:
    return os.path.join(profile(), "daemon.log")


def ensure_config() -> str:
    os.makedirs(profile(), exist_ok=True)
    path = config_path()
    if os.path.exists(path):
        return path

    server = ADDON.getSetting("server_url") or "https://jellyfin.darkseid.in"
    user_id = ADDON.getSetting("user_id") or ""
    token = ADDON.getSetting("access_token") or ""
    device_id = ADDON.getSetting("device_id") or "jellykodi-coreelec"
    cfg = {
        "server_url": server.rstrip("/"),
        "user_id": user_id,
        "access_token": token,
        "device_id": device_id,
        "device_name": "CoreELEC",
        "socket_path": sock_path(),
        "kodi_db_dir": xbmcvfs.translatePath("special://database"),
        "staging_dir": xbmcvfs.translatePath("special://temp"),
        "map_db_path": os.path.join(profile(), "jellykodi_map.db"),
        "playback_timeout_secs": 20,
        "http_timeout_secs": 60,
        "sync_page_size": 1000,
        "sync_concurrency": 8,
        "sync_concurrency_playing": 1,
        "cast_limit": 15,
        "kodisync_poll_secs": 300,
    }
    with open(path, "w", encoding="utf-8") as fh:
        json.dump(cfg, fh, indent=2)
    return path


def daemon_healthy() -> bool:
    """True if an existing daemon answers STATUS on the Unix socket."""
    try:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as s:
            s.settimeout(1.0)
            s.connect(sock_path())
            s.sendall(b"STATUS\n")
            data = s.recv(4096)
            return bool(data) and b"online" in data
    except OSError:
        return False


def stop_stale_daemon() -> None:
    """Kill leftover jellykodi-rs only if it is unhealthy (never interrupt a live sync)."""
    if daemon_healthy():
        xbmc.log("[jellykodi] existing daemon healthy — not restarting", xbmc.LOGINFO)
        return
    bin_path = binary_path()
    try:
        out = subprocess.check_output(["pgrep", "-f", bin_path], text=True)
    except (subprocess.CalledProcessError, FileNotFoundError):
        out = ""
    for pid_s in out.split():
        try:
            pid = int(pid_s.strip())
            if pid == os.getpid():
                continue
            os.kill(pid, signal.SIGTERM)
            xbmc.log(f"[jellykodi] stopped stale daemon pid={pid}", xbmc.LOGINFO)
        except (ValueError, ProcessLookupError, PermissionError):
            pass
    time.sleep(0.5)
    sock = sock_path()
    if os.path.exists(sock):
        try:
            os.remove(sock)
        except OSError:
            pass


def _setting_on(name: str) -> bool:
    return ADDON.getSetting(name) == "true"


def _play_next(item_id: str, series_id: str) -> None:
    try:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as s:
            s.settimeout(8.0)
            s.connect(sock_path())
            s.sendall(f"NEXT {item_id} {series_id}\n".encode("utf-8"))
            raw = s.recv(4096)
        payload = json.loads(raw.decode("utf-8").strip() or "{}")
        nxt = payload.get("id")
        if nxt:
            xbmc.executebuiltin(
                f"PlayMedia(plugin://plugin.video.jellykodi/?mode=play&id={nxt})"
            )
    except Exception as exc:
        xbmc.log(f"[jellykodi] play next skipped: {exc}", xbmc.LOGINFO)


def notify_daemon(payload: dict) -> None:
    try:
        xbmc.log(
            f"[jellykodi] notify_daemon event={payload.get('event')} item={payload.get('item_id')} pos={payload.get('pos')}",
            xbmc.LOGINFO,
        )
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as s:
            s.settimeout(2.0)
            s.connect(sock_path())
            s.sendall(b"EVENT " + json.dumps(payload).encode("utf-8") + b"\n")
            s.recv(4096)
    except Exception as exc:
        xbmc.log(f"[jellykodi] notify_daemon error: {exc}", xbmc.LOGWARNING)


class UnifiedJellyPlayer(xbmc.Player):
    def __init__(self) -> None:
        super().__init__()
        self.current_pos = 0.0
        self.duration = 0.0
        self.item_id = ""
        self.media_source_id = ""
        self.play_session_id = ""
        self.intro_skipped = False

    def _capture_ids(self) -> None:
        try:
            win = xbmcgui.Window(10000)
            self.item_id = win.getProperty("jellykodi.item_id")
            self.media_source_id = win.getProperty("jellykodi.media_source_id")
            self.play_session_id = win.getProperty("jellykodi.play_session_id")
            self.duration = float(win.getProperty("jellykodi.duration") or 0.0)
        except Exception:
            pass
        if not self.item_id:
            try:
                f = self.getPlayingFile()
                if "id=" in f:
                    self.item_id = f.split("id=")[1].split("&")[0]
                elif "/Videos/" in f:
                    parts = f.split("/Videos/")[1].split("/")
                    if parts:
                        self.item_id = parts[0]
            except Exception:
                pass
        xbmc.log(
            f"[jellykodi] captured ids: item_id={self.item_id} session={self.play_session_id} dur={self.duration}",
            xbmc.LOGINFO,
        )

    def onAVStarted(self) -> None:
        xbmc.log("[jellykodi] onAVStarted", xbmc.LOGINFO)
        if self.isPlayingVideo():
            self._capture_ids()
            if self.item_id:
                try:
                    self.current_pos = self.getTime()
                except RuntimeError:
                    self.current_pos = 0.0
                try:
                    win = xbmcgui.Window(10000)
                    req_offset = float(win.getProperty("jellykodi.start_offset") or 0.0)
                    if req_offset > 5.0 and self.current_pos < 3.0:
                        xbmc.log(f"[jellykodi] onAVStarted seeking to start_offset={req_offset}", xbmc.LOGINFO)
                        self.seekTime(req_offset)
                        self.current_pos = req_offset
                except Exception as exc:
                    xbmc.log(f"[jellykodi] seekTime error: {exc}", xbmc.LOGWARNING)
                notify_daemon(
                    {
                        "event": "started",
                        "file": self.getPlayingFile(),
                        "item_id": self.item_id,
                        "media_source_id": self.media_source_id,
                        "play_session_id": self.play_session_id,
                        "pos": self.current_pos,
                        "duration": self.duration,
                    }
                )

    def onPlayBackSeek(self, time: int, seek_offset: int) -> None:
        xbmc.log(f"[jellykodi] onPlayBackSeek time={time} seek_offset={seek_offset}", xbmc.LOGINFO)
        if not self.item_id:
            self._capture_ids()
        if time:
            self.current_pos = float(time) / 1000.0
        else:
            try:
                self.current_pos = self.getTime()
            except RuntimeError:
                pass
        try:
            if self.duration <= 0.0:
                self.duration = self.getTotalTime()
        except Exception:
            pass
        xbmc.log(
            f"[jellykodi] onPlayBackSeek resolved pos={self.current_pos:.1f}s dur={self.duration:.1f}s",
            xbmc.LOGINFO,
        )
        if self.item_id:
            notify_daemon(
                {
                    "event": "progress",
                    "item_id": self.item_id,
                    "media_source_id": self.media_source_id,
                    "play_session_id": self.play_session_id,
                    "pos": self.current_pos,
                    "duration": self.duration,
                }
            )

    def onPlayBackResumed(self) -> None:
        xbmc.log("[jellykodi] onPlayBackResumed", xbmc.LOGINFO)
        if not self.item_id:
            self._capture_ids()
        try:
            self.current_pos = self.getTime()
        except RuntimeError:
            pass
        if self.item_id:
            notify_daemon(
                {
                    "event": "progress",
                    "item_id": self.item_id,
                    "media_source_id": self.media_source_id,
                    "play_session_id": self.play_session_id,
                    "pos": self.current_pos,
                    "duration": self.duration,
                }
            )

    def onPlayBackPaused(self) -> None:
        xbmc.log("[jellykodi] onPlayBackPaused", xbmc.LOGINFO)
        if not self.item_id:
            self._capture_ids()
        try:
            self.current_pos = self.getTime()
        except RuntimeError:
            pass
        if self.item_id:
            notify_daemon(
                {
                    "event": "paused",
                    "item_id": self.item_id,
                    "media_source_id": self.media_source_id,
                    "play_session_id": self.play_session_id,
                    "pos": self.current_pos,
                    "duration": self.duration,
                }
            )

    def onPlayBackStopped(self) -> None:
        xbmc.log(f"[jellykodi] onPlayBackStopped item_id={self.item_id} pos={self.current_pos}", xbmc.LOGINFO)
        if not self.item_id:
            self._capture_ids()
        if self.item_id:
            notify_daemon(
                {
                    "event": "stopped",
                    "item_id": self.item_id,
                    "media_source_id": self.media_source_id,
                    "play_session_id": self.play_session_id,
                    "pos": self.current_pos,
                    "duration": self.duration,
                }
            )
            # Plezy/Findroid architecture: trigger immediate container refresh
            try:
                xbmc.executebuiltin("Container.Refresh")
            except Exception:
                pass
            self._clear_session()

    def onPlayBackEnded(self) -> None:
        xbmc.log(f"[jellykodi] onPlayBackEnded item_id={self.item_id}", xbmc.LOGINFO)
        if not self.item_id:
            self._capture_ids()
        if self.item_id:
            notify_daemon(
                {
                    "event": "ended",
                    "item_id": self.item_id,
                    "media_source_id": self.media_source_id,
                    "play_session_id": self.play_session_id,
                    "pos": self.duration if self.duration > 0.0 else self.current_pos,
                    "duration": self.duration,
                }
            )
            try:
                xbmc.executebuiltin("Container.Refresh")
            except Exception:
                pass
            item_id = self.item_id
            series_id = ""
            try:
                series_id = xbmcgui.Window(10000).getProperty("jellykodi.series_id")
            except Exception:
                pass
            self._clear_session()
            if item_id and series_id and _setting_on("play_next"):
                _play_next(item_id, series_id)

    def onPlayBackError(self) -> None:
        xbmc.log(f"[jellykodi] onPlayBackError item_id={self.item_id}", xbmc.LOGINFO)
        if not self.item_id:
            self._capture_ids()
        if self.item_id:
            notify_daemon(
                {
                    "event": "stopped",
                    "item_id": self.item_id,
                    "media_source_id": self.media_source_id,
                    "play_session_id": self.play_session_id,
                    "pos": self.current_pos,
                    "duration": self.duration,
                }
            )
            try:
                xbmc.executebuiltin("Container.Refresh")
            except Exception:
                pass
            self._clear_session()

    def _clear_session(self) -> None:
        self.intro_skipped = False
        self.item_id = ""
        self.media_source_id = ""
        self.play_session_id = ""
        self.duration = 0.0
        try:
            win = xbmcgui.Window(10000)
            win.clearProperty("jellykodi.item_id")
            win.clearProperty("jellykodi.media_source_id")
            win.clearProperty("jellykodi.play_session_id")
            win.clearProperty("jellykodi.duration")
            win.clearProperty("jellykodi.start_offset")
            win.clearProperty("jellykodi.stream_url")
            win.clearProperty("jellykodi.intro_start")
            win.clearProperty("jellykodi.intro_end")
            win.clearProperty("jellykodi.series_id")
        except Exception:
            pass


def run_service() -> None:
    xbmc.log("[jellykodi] run_service enter", xbmc.LOGINFO)
    cfg = ensure_config()
    bin_path = binary_path()
    stop_stale_daemon()
    proc = None
    log_fh = None
    adopted = daemon_healthy()
    if adopted:
        xbmc.log("[jellykodi] adopting already-running daemon", xbmc.LOGINFO)
    elif os.path.isfile(bin_path) and os.access(bin_path, os.X_OK):
        log_fh = open(daemon_log_path(), "ab", buffering=0)
        proc = subprocess.Popen(
            [bin_path, "--config", cfg],
            stdout=log_fh,
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )
        xbmc.log(f"[jellykodi] started daemon pid={proc.pid} bin={bin_path}", xbmc.LOGINFO)
    else:
        xbmc.log(f"[jellykodi] missing binary at {bin_path}", xbmc.LOGERROR)

    player = UnifiedJellyPlayer()
    monitor = xbmc.Monitor()
    widgets_primed = False
    _tick = 0
    try:
        while not monitor.abortRequested():
            _tick += 1
            # Skin builds home containers before this service starts the daemon.
            # ReloadSkin once per Kodi process after the widget cache is warm.
            # The flag is keyed by the Kodi pid so a skin reload does not loop,
            # and the next boot (new pid) refreshes again.
            if not widgets_primed and daemon_healthy():
                try:
                    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as s:
                        s.settimeout(90.0)
                        s.connect(sock_path())
                        s.sendall(b"REFRESH_WIDGETS\n")
                        s.recv(4096)
                    widgets_primed = True
                    skin_flag = f"/tmp/jellykodi-skin-{os.getppid()}"
                    if not os.path.exists(skin_flag):
                        open(skin_flag, "w").close()
                        xbmc.executebuiltin("ReloadSkin()")
                        xbmc.log("[jellykodi] primed widgets + ReloadSkin", xbmc.LOGINFO)
                except OSError as exc:
                    xbmc.log(f"[jellykodi] widget prime defer: {exc}", xbmc.LOGWARNING)
            restart_flag = os.path.join(profile(), ".restart_daemon")
            if os.path.exists(restart_flag):
                try:
                    os.remove(restart_flag)
                except OSError:
                    pass
                if proc is not None and proc.poll() is None:
                    proc.terminate()
                    xbmc.log("[jellykodi] restarting daemon after settings apply", xbmc.LOGINFO)
                else:
                    try:
                        out = subprocess.check_output(["pgrep", "-f", bin_path], text=True)
                    except (subprocess.CalledProcessError, FileNotFoundError):
                        out = ""
                    for pid_s in out.split():
                        try:
                            os.kill(int(pid_s.strip()), signal.SIGTERM)
                        except (ValueError, ProcessLookupError, PermissionError):
                            pass
            if proc is not None and proc.poll() is not None:
                xbmc.log(
                    f"[jellykodi] daemon exited code={proc.returncode}; restarting",
                    xbmc.LOGWARNING,
                )
                log_fh = open(daemon_log_path(), "ab", buffering=0)
                proc = subprocess.Popen(
                    [bin_path, "--config", cfg],
                    stdout=log_fh,
                    stderr=subprocess.STDOUT,
                    start_new_session=True,
                )
            elif proc is None and not daemon_healthy():
                # Adopted daemon died, or never started — bring it back.
                if os.path.isfile(bin_path) and os.access(bin_path, os.X_OK):
                    log_fh = open(daemon_log_path(), "ab", buffering=0)
                    proc = subprocess.Popen(
                        [bin_path, "--config", cfg],
                        stdout=log_fh,
                        stderr=subprocess.STDOUT,
                        start_new_session=True,
                    )
                    xbmc.log(
                        f"[jellykodi] restarted missing daemon pid={proc.pid}",
                        xbmc.LOGWARNING,
                    )
            if player.isPlayingVideo():
                if not player.item_id:
                    player._capture_ids()
                try:
                    player.current_pos = player.getTime()
                    if _setting_on("intro_skip") and not player.intro_skipped:
                        win = xbmcgui.Window(10000)
                        intro_end = float(win.getProperty("jellykodi.intro_end") or 0)
                        intro_start = float(win.getProperty("jellykodi.intro_start") or 0)
                        if (
                            intro_end > intro_start
                            and intro_start <= player.current_pos < intro_end - 1
                        ):
                            player.seekTime(intro_end)
                            player.intro_skipped = True
                    if player.duration <= 0.0:
                        player.duration = player.getTotalTime()
                    if _tick % 5 == 0 and player.item_id:
                        notify_daemon(
                            {
                                "event": "progress",
                                "item_id": player.item_id,
                                "media_source_id": player.media_source_id,
                                "play_session_id": player.play_session_id,
                                "pos": player.current_pos,
                                "duration": player.duration,
                            }
                        )
                except RuntimeError:
                    pass
            if monitor.waitForAbort(2.0):
                break
    finally:
        # Only stop a daemon we spawned. Adopted/external daemons keep running
        # across skin reloads so a mid-sync full import is not thrown away.
        if proc and proc.poll() is None:
            proc.terminate()
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()
        if log_fh is not None:
            try:
                log_fh.close()
            except Exception:
                pass
        xbmc.log("[jellykodi] run_service exit", xbmc.LOGINFO)


if __name__ == "__main__":
    run_service()
