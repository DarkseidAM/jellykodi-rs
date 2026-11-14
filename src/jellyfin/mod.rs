//! Jellyfin **12.x** HTTP client.
//!
//! Auth follows the Jellyfin 12 scheme only:
//! `Authorization: MediaBrowser Client="…", Device="…", DeviceId="…", Version="…", Token="…"`
//! Legacy `X-Emby-Token` / `api_key` / `/emby` prefixes are intentionally unsupported.

pub mod types;
pub mod ws;

use crate::config::Config;
use crate::error::{JellyError, Result};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
use reqwest::Client;
use std::time::Duration;
use types::*;
use url::form_urlencoded;

pub const CLIENT_NAME: &str = "jellykodi-rs";
pub const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone)]
pub struct JellyfinClient {
    http: Client,
    base: String,
    user_id: String,
    token: String,
    device_id: String,
    device_name: String,
    playback_timeout: Duration,
}

impl JellyfinClient {
    pub fn new(cfg: &Config) -> Result<Self> {
        let http = Client::builder()
            .timeout(Duration::from_secs(cfg.http_timeout_secs))
            .http2_adaptive_window(true)
            .use_rustls_tls()
            .build()?;
        Ok(Self {
            http,
            base: cfg.base_url().to_string(),
            user_id: cfg.user_id.clone(),
            token: cfg.access_token.clone(),
            device_id: cfg.device_id.clone(),
            device_name: cfg.device_name.clone(),
            playback_timeout: Duration::from_secs(cfg.playback_timeout_secs),
        })
    }

    /// Jellyfin 12 MediaBrowser Authorization header (values URL-encoded).
    pub fn authorization_header(&self) -> String {
        let enc = |s: &str| form_urlencoded::byte_serialize(s.as_bytes()).collect::<String>();
        format!(
            "MediaBrowser Client=\"{}\", Device=\"{}\", DeviceId=\"{}\", Version=\"{}\", Token=\"{}\"",
            enc(CLIENT_NAME),
            enc(&self.device_name),
            enc(&self.device_id),
            enc(CLIENT_VERSION),
            enc(&self.token),
        )
    }

    fn headers(&self) -> Result<HeaderMap> {
        let mut h = HeaderMap::new();
        h.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&self.authorization_header())
                .map_err(|e| JellyError::Other(format!("auth header: {e}")))?,
        );
        Ok(h)
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    pub fn user_id(&self) -> &str {
        &self.user_id
    }

    pub fn base_url(&self) -> &str {
        &self.base
    }

    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, path_and_query: &str) -> Result<T> {
        let url = format!("{}{}", self.base, path_and_query);
        let resp = self
            .http
            .get(&url)
            .headers(self.headers()?)
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(JellyError::Api(format!("{status} {url}: {body}")));
        }
        Ok(resp.json().await?)
    }

    async fn post_json<T: serde::de::DeserializeOwned, B: serde::Serialize>(
        &self,
        path: &str,
        body: &B,
        timeout: Option<Duration>,
    ) -> Result<T> {
        let url = format!("{}{}", self.base, path);
        let mut req = self.http.post(&url).headers(self.headers()?).json(body);
        if let Some(t) = timeout {
            req = req.timeout(t);
        }
        let resp = req.send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(JellyError::Api(format!("{status} {url}: {body}")));
        }
        Ok(resp.json().await?)
    }

    async fn post_empty(&self, path: &str, body: &serde_json::Value) -> Result<()> {
        let url = format!("{}{}", self.base, path);
        let resp = self
            .http
            .post(&url)
            .headers(self.headers()?)
            .json(body)
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(JellyError::Api(format!("{status} {url}: {text}")));
        }
        Ok(())
    }

    pub async fn ping(&self) -> Result<()> {
        let _: serde_json::Value = self.get_json("/System/Info/Public").await?;
        Ok(())
    }

    /// Paginated library items (Movie/Series/Season/Episode/BoxSet).
    pub async fn items_page(
        &self,
        include_types: &str,
        start_index: u32,
        limit: u32,
        fields: &str,
    ) -> Result<ItemsEnvelope> {
        let q = format!(
            "/Users/{}/Items?Recursive=true&IncludeItemTypes={}&StartIndex={}&Limit={}&Fields={}&EnableTotalRecordCount=true&ExcludeItemTypes=Person,Studio,Genre,LiveTvChannel",
            self.user_id,
            urlencoding(include_types),
            start_index,
            limit,
            urlencoding(fields),
        );
        self.get_json(&q).await
    }

    pub async fn resume(&self, limit: u32, fields: &str) -> Result<ItemsEnvelope> {
        let q = format!(
            "/UserItems/Resume?UserId={}&Limit={}&Fields={}&MediaTypes=Video&EnableTotalRecordCount=false",
            self.user_id,
            limit,
            urlencoding(fields),
        );
        self.get_json(&q).await
    }

    pub async fn next_up(&self, limit: u32, fields: &str) -> Result<ItemsEnvelope> {
        let q = format!(
            "/Shows/NextUp?UserId={}&Limit={}&Fields={}&EnableResumable=false&EnableTotalRecordCount=false",
            self.user_id,
            limit,
            urlencoding(fields),
        );
        self.get_json(&q).await
    }

    pub async fn latest(
        &self,
        include_types: &str,
        limit: u32,
        fields: &str,
    ) -> Result<Vec<JellyfinItem>> {
        let q = format!(
            "/Users/{}/Items/Latest?Limit={}&IncludeItemTypes={}&Fields={}",
            self.user_id,
            limit,
            urlencoding(include_types),
            urlencoding(fields),
        );
        // Latest returns a bare array in JF 12.
        self.get_json(&q).await
    }

    pub async fn item(&self, id: &str, fields: &str) -> Result<JellyfinItem> {
        let q = format!(
            "/Users/{}/Items/{}?Fields={}",
            self.user_id,
            id,
            urlencoding(fields)
        );
        self.get_json(&q).await
    }

    pub async fn playback_info(&self, item_id: &str) -> Result<PlaybackInfoResponse> {
        // JF 12: prefer POST body; query params are obsolete.
        let body = serde_json::json!({
            "UserId": self.user_id,
            "EnableDirectPlay": true,
            "EnableDirectStream": true,
            "EnableTranscoding": true,
            "AllowVideoStreamCopy": true,
            "AllowAudioStreamCopy": true,
            "AutoOpenLiveStream": false,
            "DeviceProfile": device_profile_coreelec(),
        });
        self.post_json(
            &format!("/Items/{item_id}/PlaybackInfo"),
            &body,
            Some(self.playback_timeout),
        )
        .await
    }

    pub async fn reporting_started(
        &self,
        item_id: &str,
        media_source_id: &str,
        play_session_id: &str,
        position_ticks: i64,
    ) -> Result<()> {
        let body = serde_json::json!({
            "ItemId": item_id,
            "MediaSourceId": media_source_id,
            "PlaySessionId": play_session_id,
            "PositionTicks": position_ticks,
            "CanSeek": true,
            "IsPaused": false,
            "IsMuted": false,
            "PlayMethod": "DirectPlay",
        });
        self.post_empty("/Sessions/Playing", &body).await
    }

    pub async fn reporting_progress(
        &self,
        item_id: &str,
        media_source_id: &str,
        play_session_id: &str,
        position_ticks: i64,
        is_paused: bool,
    ) -> Result<()> {
        let body = serde_json::json!({
            "ItemId": item_id,
            "MediaSourceId": media_source_id,
            "PlaySessionId": play_session_id,
            "PositionTicks": position_ticks,
            "IsPaused": is_paused,
            "CanSeek": true,
            "IsMuted": false,
            "PlayMethod": "DirectPlay",
            "EventName": if is_paused { "Pause" } else { "TimeUpdate" },
        });
        self.post_empty("/Sessions/Playing/Progress", &body).await
    }

    pub async fn reporting_stopped(
        &self,
        item_id: &str,
        media_source_id: &str,
        play_session_id: &str,
        position_ticks: i64,
    ) -> Result<()> {
        let body = serde_json::json!({
            "ItemId": item_id,
            "MediaSourceId": media_source_id,
            "PlaySessionId": play_session_id,
            "PositionTicks": position_ticks,
        });
        self.post_empty("/Sessions/Playing/Stopped", &body).await
    }

    /// Kodi Sync Queue plugin delta (server already has plugin 16.x).
    pub async fn kodisync_items(&self, last_update_dt: &str) -> Result<KodiSyncQueueResponse> {
        let q = format!(
            "/Jellyfin.Plugin.KodiSyncQueue/{}/GetItems?LastUpdateDT={}",
            self.user_id,
            urlencoding(last_update_dt)
        );
        match self.get_json(&q).await {
            Ok(v) => Ok(v),
            Err(_) => {
                // Some builds expose lowercase plugin route variants.
                let q2 = format!(
                    "/Jellyfin.Plugin.KodiSyncQueue/{}/getItems?LastUpdateDT={}",
                    self.user_id,
                    urlencoding(last_update_dt)
                );
                self.get_json(&q2).await
            }
        }
    }

    pub fn image_url(&self, item_id: &str, image_type: &str, max_width: u32) -> String {
        format!(
            "{}/Items/{}/Images/{}?maxWidth={}&quality=90",
            self.base, item_id, image_type, max_width
        )
    }

    pub fn websocket_url(&self) -> String {
        let ws_base = if self.base.starts_with("https://") {
            self.base.replacen("https://", "wss://", 1)
        } else {
            self.base.replacen("http://", "ws://", 1)
        };
        // JF 12: capital-A ApiKey only (legacy api_key removed).
        format!(
            "{}/socket?ApiKey={}&DeviceId={}",
            ws_base.trim_end_matches('/'),
            urlencoding(&self.token),
            urlencoding(&self.device_id)
        )
    }
}

fn urlencoding(s: &str) -> String {
    form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

/// Device profile tuned for CoreELEC / Amlogic S905X4 (Direct Play first).
fn device_profile_coreelec() -> serde_json::Value {
    serde_json::json!({
        "Name": "jellykodi-rs-coreelec",
        "MaxStaticBitrate": 200_000_000,
        "MaxStreamingBitrate": 200_000_000,
        "DirectPlayProfiles": [
            { "Type": "Video", "Container": "mkv,mp4,mov,ts,m2ts,webm", "VideoCodec": "h264,hevc,av1,vp9,mpeg2video,vc1", "AudioCodec": "aac,ac3,eac3,mp3,flac,truehd,dts,dca,opus,pcm,vorbis" }
        ],
        "TranscodingProfiles": [
            { "Type": "Video", "Container": "ts", "VideoCodec": "h264", "AudioCodec": "aac,ac3", "Protocol": "hls", "Context": "Streaming" }
        ],
        "ContainerProfiles": [],
        "CodecProfiles": [],
        "SubtitleProfiles": [
            { "Format": "srt", "Method": "External" },
            { "Format": "srt", "Method": "Embed" },
            { "Format": "ass", "Method": "External" },
            { "Format": "ssa", "Method": "External" },
            { "Format": "vtt", "Method": "External" },
            { "Format": "subrip", "Method": "External" }
        ]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_header_uses_mediabrowser_scheme() {
        let cfg = Config {
            access_token: "tok".into(),
            user_id: "uid".into(),
            device_id: "dev".into(),
            device_name: "CoreELEC".into(),
            ..Config::default()
        };
        let c = JellyfinClient::new(&cfg).unwrap();
        let h = c.authorization_header();
        assert!(h.starts_with("MediaBrowser "));
        assert!(h.contains("Token=\"tok\""));
        assert!(h.contains("Client=\"jellykodi-rs\""));
        assert!(!h.to_lowercase().contains("x-emby"));
    }

    #[test]
    fn websocket_url_uses_capital_apikey() {
        let cfg = Config {
            access_token: "abc".into(),
            user_id: "uid".into(),
            server_url: "https://jellyfin.example".into(),
            device_id: "d1".into(),
            ..Config::default()
        };
        let c = JellyfinClient::new(&cfg).unwrap();
        let u = c.websocket_url();
        assert!(u.starts_with("wss://"));
        assert!(u.contains("ApiKey=abc"));
        assert!(!u.contains("api_key="));
    }
}
