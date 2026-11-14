# jellykodi-rs

Hybrid **Jellyfin 12** client for **CoreELEC** (aarch64): a Rust sidecar syncs a native Kodi `MyVideos` library and pre-warms home widgets; a thin Python add-on (`plugin.video.jellykodi`) renders instantly over a Unix socket.

> Not a fork of [jellyfin-kodi](https://github.com/jellyfin/jellyfin-kodi) or [JellyCon](https://github.com/jellyfin/jellycon). Separate add-on id.

## Requirements

- Jellyfin **12.x** server (tested against **12.4.0**)
- CoreELEC / Kodi 21+ on aarch64 (e.g. Homatics R 4K Plus)
- Modern auth only: `Authorization: MediaBrowser …` (legacy `X-Emby-Token` / `api_key` are **not** used)

## Install (CoreELEC)

**Recommended (auto-updates):**

1. Download [`repository.jellykodi-1.0.0.zip`](https://github.com/DarkseidAM/jellykodi-rs/releases/latest) from the latest Release (or from [GitHub Pages](https://darkseidam.github.io/jellykodi-rs/)).
2. In Kodi: **Add-ons → Install from zip** → install the repository zip once.
3. **Add-ons → Install from repository → JellyKodi Repository → Video add-ons → JellyKodi Hybrid**.
4. Open add-on settings: set **Server URL**, **User ID**, **Access token** (reuse from JellyCon `auth.json` if migrating).
5. Restart Kodi (service starts `bin/jellykodi-rs`).

Kodi will pick up newer `plugin.video.jellykodi` versions from  
`https://darkseidam.github.io/jellykodi-rs/` on the next add-on update check.

**One-shot zip (no auto-update):** install `plugin.video.jellykodi-*.zip` from [Releases](https://github.com/DarkseidAM/jellykodi-rs/releases) instead of step 3.

Point Arctic / skinshortcuts widget paths at:

```text
plugin://plugin.video.jellykodi/?mode=widget&type=continue_watching
plugin://plugin.video.jellykodi/?mode=widget&type=recently_added_movies
plugin://plugin.video.jellykodi/?mode=widget&type=recently_added_episodes
```

Snapshot your skinshortcuts JSON before editing so you can roll back.

### Manual full resync

```text
plugin://plugin.video.jellykodi/?mode=resync
```

## Architecture (short)

- **Daemon** `jellykodi-rs`: HTTP/2 + WebSocket (`ApiKey` + KeepAlive) + KodiSyncQueue poll, widget RAM cache, staging MyVideos swap, PlaybackInfo resolve, session progress POSTs.
- **Shim** `default.py` / `service.py`: warm invoker ListItems; sole `xbmc.Player` owner.

Server path example: `https://jellyfin.example` (no `/emby` prefix).

## Develop

```bash
rustup default 1.100.0
cargo test
cargo build --release --target aarch64-unknown-linux-gnu
```

GitHub Actions builds the aarch64 add-on zip on every push/tag.

## License

MIT
