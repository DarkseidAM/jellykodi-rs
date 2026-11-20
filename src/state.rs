use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::{Notify, RwLock};

#[derive(Debug, Default, Clone)]
pub struct StatusSnapshot {
    pub online: bool,
    pub syncing: bool,
    pub last_error: Option<String>,
    pub last_full_sync: Option<String>,
    pub library_items: u64,
}

pub struct AppState {
    pub online: AtomicBool,
    pub playing: AtomicBool,
    pub syncing: AtomicBool,
    pub last_error: RwLock<Option<String>>,
    pub last_full_sync: RwLock<Option<String>>,
    pub library_items: RwLock<u64>,
    /// Pre-serialized JSON arrays keyed by widget name.
    pub widgets: RwLock<HashMap<String, Value>>,
    pub resync_notify: Notify,
    pub delta_notify: Notify,
}

impl AppState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            online: AtomicBool::new(false),
            playing: AtomicBool::new(false),
            syncing: AtomicBool::new(false),
            last_error: RwLock::new(None),
            last_full_sync: RwLock::new(None),
            library_items: RwLock::new(0),
            widgets: RwLock::new(HashMap::new()),
            resync_notify: Notify::new(),
            delta_notify: Notify::new(),
        })
    }

    pub fn set_online(&self, v: bool) {
        self.online.store(v, Ordering::SeqCst);
    }

    pub fn is_online(&self) -> bool {
        self.online.load(Ordering::SeqCst)
    }

    pub fn set_playing(&self, v: bool) {
        self.playing.store(v, Ordering::SeqCst);
    }

    pub fn is_playing(&self) -> bool {
        self.playing.load(Ordering::SeqCst)
    }

    pub async fn status_snapshot(&self) -> StatusSnapshot {
        StatusSnapshot {
            online: self.is_online(),
            syncing: self.syncing.load(Ordering::SeqCst),
            last_error: self.last_error.read().await.clone(),
            last_full_sync: self.last_full_sync.read().await.clone(),
            library_items: *self.library_items.read().await,
        }
    }

    pub async fn get_cw_resume_pos(&self, item_id: &str) -> Option<i64> {
        let map = self.widgets.read().await;
        for key in &["continue_watching", "resume"] {
            if let Some(cw_val) = map.get(*key)
                && let Some(arr) = cw_val.as_array()
            {
                for it in arr {
                    if it
                        .get("plugin_url")
                        .and_then(|u| u.as_str())
                        .map(|u| u.contains(item_id))
                        .unwrap_or(false)
                        && let Some(pos) = it.get("resume_pos").and_then(|p| p.as_i64())
                    {
                        return Some(pos);
                    }
                }
            }
        }
        None
    }

    /// Optimistic in-memory update for continue_watching & resume widgets on playback stop.
    /// Eliminates network delay so home widgets are already refreshed when the user returns.
    pub async fn optimistic_update_cw(&self, item_id: &str, pos_secs: f64, duration_secs: f64) {
        if item_id.is_empty() {
            return;
        }
        let mut map = self.widgets.write().await;
        for key in &["continue_watching", "resume"] {
            if let Some(cw_val) = map.get_mut(*key)
                && let Some(arr) = cw_val.as_array_mut()
                && let Some(idx) = arr.iter().position(|it| {
                    it.get("plugin_url")
                        .and_then(|u| u.as_str())
                        .map(|u| u.contains(item_id))
                        .unwrap_or(false)
                })
            {
                let mut item = arr.remove(idx);
                let played = duration_secs > 0.0 && (pos_secs / duration_secs) >= 0.90;
                if !played && pos_secs > 10.0 {
                    if let Some(obj) = item.as_object_mut() {
                        obj.insert("resume_pos".into(), serde_json::json!(pos_secs as i64));
                    }
                    arr.insert(0, item);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn optimistic_update_cw_updates_and_moves_to_front() {
        let state = AppState::new();
        let items = serde_json::json!([
            { "title": "Show A", "plugin_url": "plugin://plugin.video.jellykodi/?mode=play&id=id1", "resume_pos": 100 },
            { "title": "Show B", "plugin_url": "plugin://plugin.video.jellykodi/?mode=play&id=id2", "resume_pos": 50 }
        ]);
        state.widgets.write().await.insert("continue_watching".into(), items);

        state.optimistic_update_cw("id2", 500.0, 1800.0).await;

        let map = state.widgets.read().await;
        let cw = map.get("continue_watching").unwrap().as_array().unwrap();
        assert_eq!(cw[0]["title"], "Show B");
        assert_eq!(cw[0]["resume_pos"], 500);
        assert_eq!(cw[1]["title"], "Show A");
    }

    #[tokio::test]
    async fn optimistic_update_cw_removes_played() {
        let state = AppState::new();
        let items = serde_json::json!([
            { "title": "Show A", "plugin_url": "plugin://plugin.video.jellykodi/?mode=play&id=id1", "resume_pos": 100 }
        ]);
        state.widgets.write().await.insert("continue_watching".into(), items);

        state.optimistic_update_cw("id1", 1750.0, 1800.0).await;

        let map = state.widgets.read().await;
        let cw = map.get("continue_watching").unwrap().as_array().unwrap();
        assert_eq!(cw.len(), 0);
    }
}
