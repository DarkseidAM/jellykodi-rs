//! Jellyfin 12 WebSocket client.
//! Uses `?ApiKey=` (capital A) and answers `ForceKeepAlive` with `KeepAlive`.

use super::JellyfinClient;
use crate::error::Result;
use crate::state::AppState;
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, info, warn};

pub async fn run_websocket_loop(client: JellyfinClient, state: Arc<AppState>) {
    let mut backoff = Duration::from_secs(1);
    loop {
        match connect_and_serve(&client, &state).await {
            Ok(()) => {
                backoff = Duration::from_secs(1);
            }
            Err(e) => {
                warn!("jellyfin websocket error: {e}");
                state.set_online(false);
            }
        }
        sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(60));
    }
}

async fn connect_and_serve(client: &JellyfinClient, state: &Arc<AppState>) -> Result<()> {
    let url = client.websocket_url();
    info!("connecting websocket {}", url.split('?').next().unwrap_or(&url));
    let (ws, _) = connect_async(&url)
        .await
        .map_err(|e| crate::error::JellyError::Other(format!("ws connect: {e}")))?;
    state.set_online(true);
    let (mut write, mut read) = ws.split();

    while let Some(msg) = read.next().await {
        let msg = msg.map_err(|e| crate::error::JellyError::Other(format!("ws read: {e}")))?;
        match msg {
            Message::Text(text) => {
                handle_text(&text, &mut write, state).await?;
            }
            Message::Ping(p) => {
                write
                    .send(Message::Pong(p))
                    .await
                    .map_err(|e| crate::error::JellyError::Other(format!("ws pong: {e}")))?;
            }
            Message::Close(_) => break,
            _ => {}
        }
    }
    Ok(())
}

async fn handle_text<S>(
    text: &str,
    write: &mut S,
    state: &Arc<AppState>,
) -> Result<()>
where
    S: SinkExt<Message> + Unpin,
    S::Error: std::fmt::Display,
{
    let v: Value = serde_json::from_str(text).unwrap_or(Value::Null);
    let msg_type = v
        .get("MessageType")
        .and_then(|x| x.as_str())
        .unwrap_or("");

    match msg_type {
        "ForceKeepAlive" | "KeepAlive" => {
            // JF 12: reply with KeepAlive JSON message.
            let reply = serde_json::json!({"MessageType": "KeepAlive"});
            write
                .send(Message::Text(reply.to_string().into()))
                .await
                .map_err(|e| crate::error::JellyError::Other(format!("ws keepalive: {e}")))?;
            debug!("ws keepalive answered");
        }
        "LibraryChanged" => {
            info!("LibraryChanged — queue delta sync + widget refresh");
            state.delta_notify.notify_one();
        }
        "UserDataChanged" => {
            info!("UserDataChanged — refresh widgets + userdata delta");
            state.delta_notify.notify_one();
        }
        other => {
            debug!("ws message ignored: {other}");
        }
    }
    Ok(())
}
