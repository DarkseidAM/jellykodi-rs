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


def notify_daemon(payload: dict) -> None:
    try:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as s:
            s.settimeout(2.0)
            s.connect(sock_path())
            s.sendall(b"EVENT " + json.dumps(payload).encode("utf-8") + b"\n")
            s.recv(4096)
    except OSError:
        pass


class UnifiedJellyPlayer(xbmc.Player):
    def __init__(self) -> None:
        super().__init__()
        self.current_pos = 0.0
        self.item_id = ""
        self.media_source_id = ""
        self.play_session_id = ""

    def _capture_ids(self) -> None:
        try:
            pass
        except Exception:
            pass

    def onAVStarted(self) -> None:
        if self.isPlayingVideo():
            self._capture_ids()
            notify_daemon(
                {
                    "event": "started",
                    "file": self.getPlayingFile(),
                    "item_id": self.item_id,
                    "media_source_id": self.media_source_id,
                    "play_session_id": self.play_session_id,
                    "pos": self.current_pos,
                }
            )

    def onPlayBackPaused(self) -> None:
        notify_daemon(
            {
                "event": "paused",
                "item_id": self.item_id,
                "media_source_id": self.media_source_id,
                "play_session_id": self.play_session_id,
                "pos": self.current_pos,
            }
        )

    def onPlayBackStopped(self) -> None:
        notify_daemon(
            {
                "event": "stopped",
                "item_id": self.item_id,
                "media_source_id": self.media_source_id,
                "play_session_id": self.play_session_id,
                "pos": self.current_pos,
            }
        )
        self.item_id = ""

    def onPlayBackEnded(self) -> None:
        notify_daemon(
            {
                "event": "ended",
                "item_id": self.item_id,
                "media_source_id": self.media_source_id,
                "play_session_id": self.play_session_id,
                "pos": self.current_pos,
            }
        )
        self.item_id = ""


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
    try:
        while not monitor.abortRequested():
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
                try:
                    player.current_pos = player.getTime()
                    if int(time.time()) % 10 == 0 and player.item_id:
                        notify_daemon(
                            {
                                "event": "progress",
                                "item_id": player.item_id,
                                "media_source_id": player.media_source_id,
                                "play_session_id": player.play_session_id,
                                "pos": player.current_pos,
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
