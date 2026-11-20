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

    pub fn base_url(&self) -> &str {
        &self.base
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
        match self.get_json::<serde_json::Value>("/System/Info/Public").await {
            Ok(_) => Ok(()),
            Err(JellyError::Http(e)) if e.is_connect() || e.is_timeout() => {
                Err(JellyError::Offline)
            }
            Err(e) => Err(e),
        }
    }

    /// Paginated library items (Movie/Series/Season/Episode/BoxSet).
    /// Only page 0 requests `TotalRecordCount` (avoids a full-table COUNT on every page).
    pub async fn get_item(&self, item_id: &str) -> Result<JellyfinItem> {
        let q = format!("/Users/{}/Items/{}", self.user_id, item_id);
        self.get_json(&q).await
    }

    pub async fn items_page(
        &self,
        include_types: &str,
        start_index: u32,
        limit: u32,
        fields: &str,
    ) -> Result<ItemsEnvelope> {
        let enable_total = if start_index == 0 { "true" } else { "false" };
        let q = format!(
            "/Users/{}/Items?Recursive=true&IncludeItemTypes={}&StartIndex={}&Limit={}&Fields={}&EnableTotalRecordCount={}&ExcludeItemTypes=Person,Studio,Genre,LiveTvChannel",
            self.user_id,
            urlencoding(include_types),
            start_index,
            limit,
            urlencoding(fields),
            enable_total,
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
        // GroupItems=false: Episode Latest otherwise collapses to Series folders.
        let q = format!(
            "/Users/{}/Items/Latest?Limit={}&IncludeItemTypes={}&Fields={}&GroupItems=false",
            self.user_id,
            limit,
            urlencoding(include_types),
            urlencoding(fields),
        );
        // Latest returns a bare array in JF 12.
        self.get_json(&q).await
    }

    /// Filtered library query (IsResumable / IsFavorite / BoxSet lists for widgets).
    pub async fn items_filtered(
        &self,
        include_types: &str,
        filters: &str,
        sort_by: &str,
        sort_order: &str,
        limit: u32,
        fields: &str,
    ) -> Result<ItemsEnvelope> {
        let mut q = format!(
            "/Users/{}/Items?Recursive=true&IncludeItemTypes={}&Limit={}&Fields={}&EnableTotalRecordCount=false&IsMissing=false",
            self.user_id,
            urlencoding(include_types),
            limit,
            urlencoding(fields),
        );
        if !filters.is_empty() {
            q.push_str(&format!("&Filters={}", urlencoding(filters)));
        }
        if !sort_by.is_empty() {
            q.push_str(&format!(
                "&SortBy={}&SortOrder={}",
                urlencoding(sort_by),
                urlencoding(if sort_order.is_empty() {
                    "Descending"
                } else {
                    sort_order
                })
            ));
        }
        self.get_json(&q).await
    }

    pub async fn resume_typed(
        &self,
        include_types: &str,
        limit: u32,
        fields: &str,
    ) -> Result<ItemsEnvelope> {
        let q = format!(
            "/UserItems/Resume?UserId={}&Limit={}&Fields={}&MediaTypes=Video&IncludeItemTypes={}&EnableTotalRecordCount=false",
            self.user_id,
            limit,
            urlencoding(fields),
            urlencoding(include_types),
        );
        self.get_json(&q).await
    }

    pub async fn search(
        &self,
        term: &str,
        include_types: &str,
        limit: u32,
        fields: &str,
    ) -> Result<ItemsEnvelope> {
        let q = format!(
            "/Users/{}/Items?Recursive=true&SearchTerm={}&IncludeItemTypes={}&Limit={}&Fields={}&EnableTotalRecordCount=false",
            self.user_id,
            urlencoding(term),
            urlencoding(include_types),
            limit,
            urlencoding(fields),
        );
        self.get_json(&q).await
    }

    /// Children of a BoxSet / Series / Season (widget folder browse).
    pub async fn items_by_parent(
        &self,
        parent_id: &str,
        include_types: &str,
        limit: u32,
        fields: &str,
    ) -> Result<ItemsEnvelope> {
        let q = format!(
            "/Users/{}/Items?ParentId={}&Recursive=true&IncludeItemTypes={}&Limit={}&Fields={}&EnableTotalRecordCount=false&SortBy=SortName&SortOrder=Ascending&IsMissing=false",
            self.user_id,
            urlencoding(parent_id),
            urlencoding(include_types),
            limit,
            urlencoding(fields),
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
        let mut body = serde_json::json!({
            "ItemId": item_id,
            "CanSeek": true,
            "IsPaused": false,
            "IsMuted": false,
            "PlayMethod": "DirectPlay",
            "QueueableMediaTypes": "Video",
        });
        if position_ticks > 0 {
            body["PositionTicks"] = serde_json::json!(position_ticks);
        }
        if !media_source_id.is_empty() {
            body["MediaSourceId"] = serde_json::json!(media_source_id);
        }
        if !play_session_id.is_empty() {
            body["PlaySessionId"] = serde_json::json!(play_session_id);
        }
        self.post_empty("/Sessions/Playing", &body).await
    }

    pub async fn reporting_progress(
        &self,
        item_id: &str,
        media_source_id: &str,
        play_session_id: &str,
        position_ticks: i64,
        runtime_ticks: i64,
        is_paused: bool,
    ) -> Result<()> {
        let mut body = serde_json::json!({
            "ItemId": item_id,
            "PositionTicks": position_ticks,
            "IsPaused": is_paused,
            "CanSeek": true,
            "IsMuted": false,
            "PlayMethod": "DirectPlay",
            "EventName": if is_paused { "Pause" } else { "TimeUpdate" },
        });
        if !media_source_id.is_empty() {
            body["MediaSourceId"] = serde_json::json!(media_source_id);
        }
        if !play_session_id.is_empty() {
            body["PlaySessionId"] = serde_json::json!(play_session_id);
        }
        if runtime_ticks > 0 {
            body["RunTimeTicks"] = serde_json::json!(runtime_ticks);
        }
        self.post_empty("/Sessions/Playing/Progress", &body).await
    }

    pub async fn reporting_stopped(
        &self,
        item_id: &str,
        media_source_id: &str,
        play_session_id: &str,
        position_ticks: i64,
        runtime_ticks: i64,
    ) -> Result<()> {
        let mut body = serde_json::json!({
            "ItemId": item_id,
            "PositionTicks": position_ticks,
        });
        if !media_source_id.is_empty() {
            body["MediaSourceId"] = serde_json::json!(media_source_id);
        }
        if !play_session_id.is_empty() {
            body["PlaySessionId"] = serde_json::json!(play_session_id);
        }
        if runtime_ticks > 0 {
            body["RunTimeTicks"] = serde_json::json!(runtime_ticks);
        }
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
            { "Type": "Video", "Container": "mkv,mp4,mov,ts,m2ts,webm,avi,wmv,m4v", "VideoCodec": "h264,hevc,av1,vp9,mpeg2video,vc1,mpeg4", "AudioCodec": "aac,ac3,eac3,mp3,flac,truehd,dts,dca,opus,pcm,vorbis" },
            { "Type": "Audio" },
            { "Type": "Photo" }
        ],
        "TranscodingProfiles": [
            { "Type": "Video", "Container": "ts", "VideoCodec": "h264", "AudioCodec": "aac,ac3", "Protocol": "hls", "Context": "Streaming" }
        ],
        "ContainerProfiles": [],
        "CodecProfiles": [],
        "SubtitleProfiles": [
            { "Format": "srt", "Method": "External" },
            { "Format": "srt", "Method": "Embed" },
            { "Format": "subrip", "Method": "Embed" },
            { "Format": "subrip", "Method": "External" },
            { "Format": "ass", "Method": "Embed" },
            { "Format": "ass", "Method": "External" },
            { "Format": "ssa", "Method": "Embed" },
            { "Format": "ssa", "Method": "External" },
            { "Format": "pgssub", "Method": "Embed" },
            { "Format": "pgssub", "Method": "External" },
            { "Format": "pgs", "Method": "Embed" },
            { "Format": "pgs", "Method": "External" },
            { "Format": "dvdsub", "Method": "Embed" },
            { "Format": "dvdsub", "Method": "External" },
            { "Format": "vtt", "Method": "Embed" },
            { "Format": "vtt", "Method": "External" },
            { "Format": "sub", "Method": "Embed" },
            { "Format": "sub", "Method": "External" }
        ]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_profile_includes_embedded_subtitles() {
        let p = device_profile_coreelec();
        let subs = p["SubtitleProfiles"].as_array().expect("subtitle profiles");
        let formats: Vec<(String, String)> = subs
            .iter()
            .map(|s| {
                (
                    s["Format"].as_str().unwrap().to_string(),
                    s["Method"].as_str().unwrap().to_string(),
                )
            })
            .collect();
        assert!(formats.contains(&("ass".into(), "Embed".into())));
        assert!(formats.contains(&("pgssub".into(), "Embed".into())));
        assert!(formats.contains(&("subrip".into(), "Embed".into())));
        assert!(formats.contains(&("dvdsub".into(), "Embed".into())));
    }

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

    #[test]
    fn stream_details_normalization() {
        let v_hdr = types::MediaStream {
            stream_type: "Video".into(),
            index: 0,
            language: Some("eng".into()),
            codec: Some("hevc".into()),
            is_external: false,
            is_text_subtitle: false,
            supports_external_stream: false,
            delivery_url: None,
            is_default: true,
            is_forced: false,
            width: Some(3840),
            height: Some(2160),
            channels: None,
            aspect_ratio: Some("16:9".into()),
            profile: Some("Main 10".into()),
            video_range_type: Some("DOVIWithEL".into()),
            dv_profile: Some(serde_json::json!(7)),
        };
        assert_eq!(v_hdr.normalized_video_codec(), "hevc");
        assert_eq!(v_hdr.hdr_type(), "dolbyvision");
        assert_eq!(v_hdr.parsed_aspect_ratio(), 1.778);

        let a_atmos = types::MediaStream {
            stream_type: "Audio".into(),
            index: 1,
            language: Some("eng".into()),
            codec: Some("truehd".into()),
            is_external: false,
            is_text_subtitle: false,
            supports_external_stream: false,
            delivery_url: None,
            is_default: true,
            is_forced: false,
            width: None,
            height: None,
            channels: Some(8),
            aspect_ratio: None,
            profile: Some("Dolby TrueHD + Dolby Atmos".into()),
            video_range_type: None,
            dv_profile: None,
        };
        assert_eq!(a_atmos.normalized_audio_codec(), "truehd");
        assert_eq!(a_atmos.channels, Some(8));
    }
}
