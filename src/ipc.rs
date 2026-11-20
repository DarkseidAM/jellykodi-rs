use crate::error::{JellyError, Result};
use crate::jellyfin::JellyfinClient;
use crate::playback;
use crate::state::AppState;
use crate::widgets;
use serde_json::json;
use std::path::Path;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tracing::{info, warn};

pub async fn serve_socket(
    path: &Path,
    client: JellyfinClient,
    state: Arc<AppState>,
) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path)?;
    // Allow Kodi (often different uid on some builds) to connect — CoreELEC is root.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o666));
    }
    info!("ipc listening on {}", path.display());

    loop {
        let (stream, _) = listener.accept().await?;
        let client = client.clone();
        let state = Arc::clone(&state);
        tokio::spawn(async move {
            if let Err(e) = handle_client(stream, client, state).await {
                warn!("ipc client error: {e}");
            }
        });
    }
}

async fn handle_client(
    stream: UnixStream,
    client: JellyfinClient,
    state: Arc<AppState>,
) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    let Some(line) = lines.next_line().await? else {
        return Ok(());
    };
    let line = line.trim().to_string();
    let response = dispatch(&line, &client, &state).await;
    let body = match response {
        Ok(v) => v.to_string(),
        Err(e) => json!({"error": e.to_string()}).to_string(),
    };
    writer.write_all(body.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    Ok(())
}

async fn dispatch(
    line: &str,
    client: &JellyfinClient,
    state: &Arc<AppState>,
) -> Result<serde_json::Value> {
    if let Some(name) = line.strip_prefix("WIDGET ") {
        return Ok(widgets::widget_payload(state, name.trim()).await);
    }
    if let Some(term) = line.strip_prefix("SEARCH ") {
        let items = widgets::search_items(client, term.trim()).await?;
        return Ok(serde_json::to_value(items)?);
    }
    if let Some(id) = line.strip_prefix("BROWSE ") {
        let items = widgets::browse_items(client, id.trim()).await?;
        return Ok(serde_json::to_value(items)?);
    }
    if let Some(id) = line.strip_prefix("PLAY ") {
        let play = playback::resolve_play(client, id.trim()).await?;
        return Ok(serde_json::to_value(play)?);
    }
    if let Some(raw) = line.strip_prefix("EVENT ") {
        return handle_event(raw, client, state).await;
    }
    if line == "STATUS" {
        let snap = state.status_snapshot().await;
        return Ok(json!({
            "online": snap.online,
            "syncing": snap.syncing,
            "last_error": snap.last_error,
            "last_full_sync": snap.last_full_sync,
            "library_items": snap.library_items,
            "server": client.base_url(),
            "jellyfin_api": "12.x",
        }));
    }
    if line == "RESYNC" {
        state.resync_notify.notify_one();
        return Ok(json!({"queued": true}));
    }
    if line == "REFRESH_WIDGETS" {
        widgets::refresh_all_widgets(client, state).await?;
        return Ok(json!({"ok": true}));
    }
    Err(JellyError::Other(format!("unknown command: {line}")))
}

async fn handle_event(
    raw: &str,
    client: &JellyfinClient,
    state: &Arc<AppState>,
) -> Result<serde_json::Value> {
    let v: serde_json::Value = serde_json::from_str(raw)?;
    let event = v.get("event").and_then(|x| x.as_str()).unwrap_or("");
    let item_id = v
        .get("item_id")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let media_source_id = v
        .get("media_source_id")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let play_session_id = v
        .get("play_session_id")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let pos = v.get("pos").and_then(|x| x.as_f64()).unwrap_or(0.0);
    let duration = v.get("duration").and_then(|x| x.as_f64()).unwrap_or(0.0);
    let position_ticks = (pos * 10_000_000.0) as i64;

    match event {
        "started" => {
            state.set_playing(true);
            if !item_id.is_empty() {
                let _ = client
                    .reporting_started(&item_id, &media_source_id, &play_session_id, position_ticks)
                    .await;
            }
        }
        "paused" | "progress" => {
            let paused = event == "paused";
            if !item_id.is_empty() {
                let _ = client
                    .reporting_progress(
                        &item_id,
                        &media_source_id,
                        &play_session_id,
                        position_ticks,
                        paused,
                    )
                    .await;
            }
        }
        "stopped" | "ended" => {
            state.set_playing(false);
            if !item_id.is_empty() {
                // 1. Instant optimistic in-memory update (<0.1ms) - Plezy/Findroid architecture
                state.optimistic_update_cw(&item_id, pos, duration).await;

                // 2. Background authoritative reporting and widget refresh
                let c = client.clone();
                let s = Arc::clone(state);
                let iid = item_id.clone();
                let msid = media_source_id.clone();
                let psid = play_session_id.clone();
                tokio::spawn(async move {
                    let _ = c
                        .reporting_stopped(&iid, &msid, &psid, position_ticks)
                        .await;
                    let _ = widgets::refresh_all_widgets(&c, &s).await;
                });
            }
        }
        _ => {}
    }
    Ok(json!({"ok": true}))
}
