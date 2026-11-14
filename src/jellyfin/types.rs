use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ItemsEnvelope {
    #[serde(rename = "Items", default)]
    pub items: Vec<JellyfinItem>,
    #[serde(rename = "TotalRecordCount", default)]
    pub total_record_count: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct JellyfinItem {
    #[serde(rename = "Id")]
    pub id: String,
    #[serde(rename = "Name", default)]
    pub name: String,
    #[serde(rename = "Type", default)]
    pub item_type: String,
    #[serde(rename = "Overview")]
    pub overview: Option<String>,
    #[serde(rename = "ProductionYear")]
    pub year: Option<i32>,
    #[serde(rename = "PremiereDate")]
    pub premiered: Option<String>,
    #[serde(rename = "DateCreated")]
    pub date_created: Option<String>,
    #[serde(rename = "RunTimeTicks")]
    pub runtime_ticks: Option<i64>,
    #[serde(rename = "OfficialRating")]
    pub mpaa: Option<String>,
    #[serde(rename = "CommunityRating")]
    pub community_rating: Option<f64>,
    #[serde(rename = "Genres")]
    pub genres: Option<Vec<String>>,
    #[serde(rename = "Studios")]
    pub studios: Option<Vec<Named>>,
    #[serde(rename = "People")]
    pub people: Option<Vec<Person>>,
    #[serde(rename = "UserData")]
    pub user_data: Option<UserData>,
    #[serde(rename = "SeriesId")]
    pub series_id: Option<String>,
    #[serde(rename = "SeriesName")]
    pub series_name: Option<String>,
    #[serde(rename = "ParentId")]
    pub parent_id: Option<String>,
    #[serde(rename = "SeasonId")]
    pub season_id: Option<String>,
    #[serde(rename = "ParentIndexNumber")]
    pub parent_index: Option<i32>,
    #[serde(rename = "IndexNumber")]
    pub index_number: Option<i32>,
    #[serde(rename = "ProviderIds")]
    pub provider_ids: Option<serde_json::Value>,
    #[serde(rename = "MediaSources")]
    pub media_sources: Option<Vec<MediaSource>>,
    #[serde(rename = "MediaStreams")]
    pub media_streams: Option<Vec<MediaStream>>,
    /// Primary version id when this item is an alternate (Merge Versions).
    #[serde(rename = "PrimaryVersionId")]
    pub primary_version_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Named {
    #[serde(rename = "Name")]
    pub name: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Person {
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "Role")]
    pub role: Option<String>,
    #[serde(rename = "Type")]
    pub person_type: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct UserData {
    #[serde(rename = "PlayCount")]
    pub play_count: Option<i32>,
    #[serde(rename = "Played", default)]
    pub played: bool,
    #[serde(rename = "PlaybackPositionTicks")]
    pub position_ticks: Option<i64>,
    #[serde(rename = "LastPlayedDate")]
    pub last_played: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MediaSource {
    #[serde(rename = "Id")]
    pub id: String,
    #[serde(rename = "Path")]
    pub path: Option<String>,
    #[serde(rename = "Container")]
    pub container: Option<String>,
    #[serde(rename = "Size")]
    pub size: Option<i64>,
    #[serde(rename = "Bitrate")]
    pub bitrate: Option<i64>,
    #[serde(rename = "SupportsDirectPlay", default)]
    pub supports_direct_play: bool,
    #[serde(rename = "SupportsDirectStream", default)]
    pub supports_direct_stream: bool,
    #[serde(rename = "DirectStreamUrl")]
    pub direct_stream_url: Option<String>,
    #[serde(rename = "TranscodingUrl")]
    pub transcoding_url: Option<String>,
    #[serde(rename = "MediaStreams")]
    pub media_streams: Option<Vec<MediaStream>>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MediaStream {
    #[serde(rename = "Type")]
    pub stream_type: String,
    #[serde(rename = "Index", default)]
    pub index: i32,
    #[serde(rename = "Language")]
    pub language: Option<String>,
    #[serde(rename = "Codec")]
    pub codec: Option<String>,
    #[serde(rename = "IsExternal", default)]
    pub is_external: bool,
    #[serde(rename = "IsTextSubtitleStream", default)]
    pub is_text_subtitle: bool,
    #[serde(rename = "SupportsExternalStream", default)]
    pub supports_external_stream: bool,
    #[serde(rename = "DeliveryUrl")]
    pub delivery_url: Option<String>,
    #[serde(rename = "IsDefault", default)]
    pub is_default: bool,
    #[serde(rename = "IsForced", default)]
    pub is_forced: bool,
    #[serde(rename = "Width")]
    pub width: Option<i32>,
    #[serde(rename = "Height")]
    pub height: Option<i32>,
    #[serde(rename = "Channels")]
    pub channels: Option<i32>,
    #[serde(rename = "AspectRatio")]
    pub aspect_ratio: Option<String>,
    #[serde(rename = "Profile")]
    pub profile: Option<String>,
    #[serde(rename = "VideoRangeType")]
    pub video_range_type: Option<String>,
    #[serde(rename = "DvProfile")]
    pub dv_profile: Option<serde_json::Value>,
}

impl MediaStream {
    pub fn normalized_video_codec(&self) -> String {
        let codec = self.codec.as_deref().unwrap_or("").to_ascii_lowercase();
        let profile = self.profile.as_deref().unwrap_or("").to_ascii_lowercase();
        if codec.contains("msmpeg4") {
            "divx".to_string()
        } else if codec.contains("mpeg4") && (profile.contains("simple") || profile.is_empty()) {
            "xvid".to_string()
        } else {
            codec
        }
    }

    pub fn normalized_audio_codec(&self) -> String {
        let codec = self.codec.as_deref().unwrap_or("").to_ascii_lowercase();
        let profile = self.profile.as_deref().unwrap_or("").to_ascii_lowercase();
        if profile.contains("dts-hd ma") {
            "dtshd_ma".to_string()
        } else if profile.contains("dts-hd hra") {
            "dtshd_hra".to_string()
        } else if profile.contains("dts:x") {
            "dts_x".to_string()
        } else {
            codec
        }
    }

    pub fn parsed_aspect_ratio(&self) -> f64 {
        if let Some((w, h)) = self.aspect_ratio.as_deref().and_then(|s| s.split_once(':'))
            && let (Ok(wf), Ok(hf)) = (w.trim().parse::<f64>(), h.trim().parse::<f64>())
            && hf > 0.0
        {
            return (wf / hf * 1000.0).round() / 1000.0;
        }
        if let (Some(w), Some(h)) = (self.width, self.height)
            && h > 0
        {
            return ((w as f64) / (h as f64) * 1000.0).round() / 1000.0;
        }
        1.778
    }

    pub fn hdr_type(&self) -> String {
        if self.dv_profile.is_some() {
            return "dolbyvision".to_string();
        }
        if let Some(ref vrt) = self.video_range_type {
            let u = vrt.to_ascii_uppercase();
            if u.contains("DOVI") {
                return "dolbyvision".to_string();
            }
            if u.contains("HDR10") {
                return "hdr10".to_string();
            }
            if u.contains("HLG") {
                return "hlg".to_string();
            }
        }
        if let Some(p) = self.profile.as_deref()
            && p.to_ascii_lowercase().contains("dolby vision")
        {
            return "dolbyvision".to_string();
        }
        String::new()
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct PlaybackInfoResponse {
    #[serde(rename = "MediaSources", default)]
    pub media_sources: Vec<MediaSource>,
    #[serde(rename = "PlaySessionId")]
    pub play_session_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct KodiSyncQueueResponse {
    #[serde(rename = "Items", default)]
    pub items: Vec<KodiSyncQueueItem>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct KodiSyncQueueItem {
    #[serde(rename = "ItemId")]
    pub item_id: Option<String>,
    #[serde(rename = "ItemType")]
    pub item_type: Option<String>,
    /// Added / Removed / Updated depending on plugin version.
    #[serde(rename = "Type")]
    pub change_type: Option<String>,
}
