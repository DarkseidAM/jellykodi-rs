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
}
