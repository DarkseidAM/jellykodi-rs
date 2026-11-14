use crate::error::{JellyError, Result};
use crate::jellyfin::types::MediaSource;
use crate::jellyfin::JellyfinClient;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct PlayResolve {
    pub stream_url: String,
    pub subtitles: Vec<String>,
    pub item_id: String,
    pub media_source_id: String,
    pub play_session_id: String,
    pub play_method: String,
}

pub async fn resolve_play(client: &JellyfinClient, item_id: &str) -> Result<PlayResolve> {
    let info = client.playback_info(item_id).await?;
    let play_session_id = info.play_session_id.unwrap_or_default();

    let source = pick_best_source(&info.media_sources)
        .ok_or_else(|| JellyError::Api("PlaybackInfo returned no MediaSources".into()))?;

    let (stream_url, play_method) = stream_url_for(client, item_id, source)?;
    let subtitles = external_subs(client, source);

    Ok(PlayResolve {
        stream_url,
        subtitles,
        item_id: item_id.to_string(),
        media_source_id: source.id.clone(),
        play_session_id,
        play_method,
    })
}

/// Prefer highest-resolution DirectPlay/DirectStream source (Merge Versions / UHD).
fn pick_best_source(sources: &[MediaSource]) -> Option<&MediaSource> {
    sources.iter().max_by_key(|s| {
        let h = s
            .media_streams
            .as_ref()
            .and_then(|ms| {
                ms.iter()
                    .filter(|m| m.stream_type == "Video")
                    .filter_map(|m| m.height)
                    .max()
            })
            .unwrap_or(0);
        let bitrate = s.bitrate.unwrap_or(0);
        let direct = if s.supports_direct_play || s.supports_direct_stream {
            1_i64
        } else {
            0
        };
        (direct, h as i64, bitrate)
    })
}

fn stream_url_for(
    client: &JellyfinClient,
    item_id: &str,
    source: &MediaSource,
) -> Result<(String, String)> {
    if source.supports_direct_play || source.supports_direct_stream {
        if let Some(u) = &source.direct_stream_url {
            return Ok((absolutize(client.base_url(), u), "DirectStream".into()));
        }
        // Universal static stream with ApiKey for JF 12 when header can't be used by Kodi ffmpeg.
        let url = format!(
            "{}/Videos/{}/stream?Static=true&MediaSourceId={}&ApiKey={}",
            client.base_url(),
            item_id,
            urlencoding(&source.id),
            urlencoding(client.token()),
        );
        return Ok((url, "DirectPlay".into()));
    }
    if let Some(u) = &source.transcoding_url {
        return Ok((absolutize(client.base_url(), u), "Transcode".into()));
    }
    Err(JellyError::Api(
        "no DirectPlay/DirectStream/Transcode URL available".into(),
    ))
}

fn external_subs(client: &JellyfinClient, source: &MediaSource) -> Vec<String> {
    let Some(streams) = &source.media_streams else {
        return vec![];
    };
    streams
        .iter()
        .filter(|s| {
            s.stream_type == "Subtitle"
                && s.is_external
                && s.is_text_subtitle
                && s.supports_external_stream
        })
        .filter_map(|s| s.delivery_url.as_ref())
        .map(|u| absolutize(client.base_url(), u))
        .collect()
}

fn absolutize(base: &str, path: &str) -> String {
    if path.starts_with("http://") || path.starts_with("https://") {
        path.to_string()
    } else if path.starts_with('/') {
        format!("{}{}", base.trim_end_matches('/'), path)
    } else {
        format!("{}/{}", base.trim_end_matches('/'), path)
    }
}

fn urlencoding(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}
