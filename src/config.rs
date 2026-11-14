use crate::error::{JellyError, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub server_url: String,
    pub user_id: String,
    pub access_token: String,
    pub device_id: String,
    pub device_name: String,
    /// Unix socket for IPC with the Kodi Python shim.
    pub socket_path: PathBuf,
    /// Kodi Database directory (contains MyVideos*.db).
    pub kodi_db_dir: PathBuf,
    /// Temp dir for staging MyVideos builds.
    pub staging_dir: PathBuf,
    /// Path to jellykodi_map.db (jellyfin_id ↔ kodi ids).
    pub map_db_path: PathBuf,
    pub playback_timeout_secs: u64,
    pub http_timeout_secs: u64,
    pub sync_page_size: u32,
    pub sync_concurrency: usize,
    pub sync_concurrency_playing: usize,
    pub cast_limit: usize,
    pub kodisync_poll_secs: u64,
}

impl Default for Config {
    fn default() -> Self {
        let addon_data = PathBuf::from("/storage/.kodi/userdata/addon_data/plugin.video.jellykodi");
        Self {
            server_url: "https://jellyfin.darkseid.in".into(),
            user_id: String::new(),
            access_token: String::new(),
            device_id: uuid::Uuid::new_v4().to_string(),
            device_name: "CoreELEC".into(),
            socket_path: addon_data.join("jellykodi.sock"),
            kodi_db_dir: PathBuf::from("/storage/.kodi/userdata/Database"),
            staging_dir: PathBuf::from("/storage/.kodi/temp"),
            map_db_path: addon_data.join("jellykodi_map.db"),
            playback_timeout_secs: 20,
            http_timeout_secs: 60,
            sync_page_size: 500,
            sync_concurrency: 4,
            sync_concurrency_playing: 1,
            cast_limit: 15,
            kodisync_poll_secs: 300,
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| JellyError::Config(format!("read {}: {e}", path.display())))?;
        let mut cfg: Config = serde_json::from_str(&raw)
            .map_err(|e| JellyError::Config(format!("parse {}: {e}", path.display())))?;
        cfg.server_url = cfg.server_url.trim_end_matches('/').to_string();
        if cfg.access_token.is_empty() || cfg.user_id.is_empty() {
            return Err(JellyError::Config(
                "user_id and access_token are required".into(),
            ));
        }
        Ok(cfg)
    }

    pub fn base_url(&self) -> &str {
        &self.server_url
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_strips_trailing_slash() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let cfg = Config {
            user_id: "user".into(),
            access_token: "token".into(),
            server_url: "https://example.test/".into(),
            ..Config::default()
        };
        std::fs::write(&path, serde_json::to_string_pretty(&cfg).unwrap()).unwrap();
        let loaded = Config::load(&path).unwrap();
        assert_eq!(loaded.user_id, "user");
        assert_eq!(loaded.access_token, "token");
        assert_eq!(loaded.server_url, "https://example.test");
    }
}
