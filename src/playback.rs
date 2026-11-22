use crate::error::{JellyError, Result};
use crate::jellyfin::types::{MediaSource, MediaStream};
use crate::jellyfin::JellyfinClient;
use crate::widgets::{KodiAudioStream, KodiVideoStream};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KodiSubtitleStream {
    pub language: String,
    pub is_default: bool,
    pub is_forced: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubtitleTrackInfo {
    pub url: String,
    pub language: String,
    pub codec: String,
    pub is_default: bool,
    pub is_forced: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaybackStreams {
    pub video: Vec<KodiVideoStream>,
    pub audio: Vec<KodiAudioStream>,
    pub subtitle: Vec<KodiSubtitleStream>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceOption {
    pub id: String,
    pub name: String,
    pub stream_url: String,
    pub subtitles: Vec<SubtitleTrackInfo>,
    pub streams: PlaybackStreams,
}

#[derive(Debug, Serialize)]
pub struct PlayResolve {
    pub stream_url: String,
    pub subtitles: Vec<SubtitleTrackInfo>,
    pub item_id: String,
    pub media_source_id: String,
    pub play_session_id: String,
    pub play_method: String,
    pub resume_pos_secs: i64,
    pub duration_secs: i64,
    pub title: String,
    pub year: i32,
    pub plot: String,
    pub media_type: String,
    pub streams: PlaybackStreams,
    pub intro_start_secs: i64,
    pub intro_end_secs: i64,
    pub series_id: String,
    pub available_sources: Vec<SourceOption>,
}

pub async fn resolve_play(
    client: &JellyfinClient,
    state: Option<&std::sync::Arc<crate::state::AppState>>,
    item_id: &str,
    source_id: Option<&str>,
) -> Result<PlayResolve> {
    let (info_res, item_res, intro_res) = tokio::join!(
        client.playback_info(item_id),
        client.get_item(item_id),
        client.media_intro(item_id),
    );

    let info = info_res?;
    let item = item_res.ok();
    let (intro_start_secs, intro_end_secs) = intro_res.unwrap_or((0, 0));

    let play_session_id = info.play_session_id.unwrap_or_default();

    let source = pick_source(&info.media_sources, source_id)
        .ok_or_else(|| JellyError::Api("PlaybackInfo returned no MediaSources".into()))?;

    let (stream_url, play_method) = stream_url_for(client, item_id, source)?;
    let subtitles = external_subs(client, item_id, source);
    let streams = extract_playback_streams(source.media_streams.as_deref().unwrap_or(&[]));

    let duration_secs = item
        .as_ref()
        .and_then(|i| i.runtime_ticks)
        .or_else(|| source.media_streams.as_ref().and_then(|ms| {
            ms.iter()
                .filter(|m| m.stream_type == "Video")
                .find_map(|m| m.aspect_ratio.as_ref())
                .map(|_| 0)
        }))
        .unwrap_or(0)
        / 10_000_000;

    let mut resume_pos_secs = item
        .as_ref()
        .and_then(|i| i.user_data.as_ref())
        .and_then(|u| u.position_ticks)
        .unwrap_or(0)
        / 10_000_000;

    if let Some(s) = state
        && let Some(opt_pos) = s.get_cw_resume_pos(item_id).await
    {
        resume_pos_secs = opt_pos;
    }

    let title = item.as_ref().map(|i| i.name.clone()).unwrap_or_default();
    let plot = item
        .as_ref()
        .and_then(|i| i.overview.clone())
        .unwrap_or_default();
    let year = item.as_ref().and_then(|i| i.year).unwrap_or(0);
    let media_type = item
        .as_ref()
        .map(|i| i.item_type.to_ascii_lowercase())
        .unwrap_or_else(|| "video".into());
    let series_id = item
        .as_ref()
        .and_then(|i| i.series_id.clone())
        .unwrap_or_default();

    let mut available_sources: Vec<SourceOption> = info
        .media_sources
        .iter()
        .filter_map(|s| {
            let (url, _) = stream_url_for(client, item_id, s).ok()?;
            let subs = external_subs(client, item_id, s);
            let strms = extract_playback_streams(s.media_streams.as_deref().unwrap_or(&[]));
            let name = s.version_label();
            Some(SourceOption {
                id: s.id.clone(),
                name,
                stream_url: url,
                subtitles: subs,
                streams: strms,
            })
        })
        .collect();

    available_sources.sort_by_key(|s| {
        let ms = info.media_sources.iter().find(|m| m.id == s.id);
        let h = ms.map(|m| m.source_height()).unwrap_or(0);
        let br = ms.and_then(|m| m.bitrate).unwrap_or(0);
        std::cmp::Reverse((h, br))
    });

    Ok(PlayResolve {
        stream_url,
        subtitles,
        item_id: item_id.to_string(),
        media_source_id: source.id.clone(),
        play_session_id,
        play_method,
        resume_pos_secs,
        duration_secs,
        title,
        year,
        plot,
        media_type,
        streams,
        intro_start_secs,
        intro_end_secs,
        series_id,
        available_sources,
    })
}

/// Use the version Kodi selected. Otherwise the highest-resolution direct source.
fn pick_source<'a>(sources: &'a [MediaSource], source_id: Option<&str>) -> Option<&'a MediaSource> {
    if let Some(id) = source_id.map(str::trim).filter(|id| !id.is_empty())
        && let Some(source) = sources.iter().find(|s| s.id.eq_ignore_ascii_case(id))
    {
        return Some(source);
    }
    pick_best_source(sources)
}

/// Prefer highest-resolution DirectPlay/DirectStream source (Merge Versions / UHD).
fn pick_best_source(sources: &[MediaSource]) -> Option<&MediaSource> {
    sources.iter().max_by_key(|s| {
        let h = s.source_height();
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

fn is_text_subtitle_codec(codec: &str) -> bool {
    matches!(
        codec.to_ascii_lowercase().as_str(),
        "subrip" | "srt" | "vtt" | "ass" | "ssa" | "sub" | "smi" | "sami"
    )
}

fn external_subs(
    client: &JellyfinClient,
    item_id: &str,
    source: &MediaSource,
) -> Vec<SubtitleTrackInfo> {
    let Some(streams) = &source.media_streams else {
        return vec![];
    };
    streams
        .iter()
        .filter(|s| {
            s.stream_type == "Subtitle"
                && s.is_external
                && (s.is_text_subtitle
                    || s.codec
                        .as_deref()
                        .map(is_text_subtitle_codec)
                        .unwrap_or(false))
        })
        .map(|s| {
            let url = if let Some(delivery_url) = &s.delivery_url {
                absolutize(client.base_url(), delivery_url)
            } else {
                let codec = s.codec.as_deref().unwrap_or("srt");
                format!(
                    "{}/Videos/{}/{}/Subtitles/{}/Stream.{}?ApiKey={}",
                    client.base_url().trim_end_matches('/'),
                    item_id,
                    source.id,
                    s.index,
                    codec,
                    urlencoding(client.token())
                )
            };
            SubtitleTrackInfo {
                url,
                language: s.language.clone().unwrap_or_default(),
                codec: s.codec.clone().unwrap_or_else(|| "srt".into()),
                is_default: s.is_default,
                is_forced: s.is_forced,
            }
        })
        .collect()
}

fn extract_playback_streams(streams: &[MediaStream]) -> PlaybackStreams {
    let mut video = Vec::new();
    let mut audio = Vec::new();
    let mut subtitle = Vec::new();

    for s in streams {
        if s.stream_type == "Video" {
            video.push(KodiVideoStream {
                codec: s.normalized_video_codec(),
                width: s.width.unwrap_or(0),
                height: s.height.unwrap_or(0),
                aspect: s.parsed_aspect_ratio(),
                hdr_type: s.hdr_type(),
            });
        } else if s.stream_type == "Audio" {
            audio.push(KodiAudioStream {
                codec: s.normalized_audio_codec(),
                channels: s.channels.unwrap_or(2),
                language: s.language.clone().unwrap_or_default(),
            });
        } else if s.stream_type == "Subtitle" {
            subtitle.push(KodiSubtitleStream {
                language: s.language.clone().unwrap_or_default(),
                is_default: s.is_default,
                is_forced: s.is_forced,
            });
        }
    }

    PlaybackStreams {
        video,
        audio,
        subtitle,
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_playback_streams_parses_all_streams() {
        let streams = vec![
            MediaStream {
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
            },
            MediaStream {
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
                profile: None,
                video_range_type: None,
                dv_profile: None,
            },
            MediaStream {
                stream_type: "Subtitle".into(),
                index: 2,
                language: Some("eng".into()),
                codec: Some("subrip".into()),
                is_external: true,
                is_text_subtitle: true,
                supports_external_stream: true,
                delivery_url: Some("/Videos/m1/Subtitles/2/Stream.srt".into()),
                is_default: false,
                is_forced: false,
                width: None,
                height: None,
                channels: None,
                aspect_ratio: None,
                profile: None,
                video_range_type: None,
                dv_profile: None,
            },
        ];

        let res = extract_playback_streams(&streams);
        assert_eq!(res.video.len(), 1);
        assert_eq!(res.audio.len(), 1);
        assert_eq!(res.subtitle.len(), 1);
        assert_eq!(res.video[0].hdr_type, "dolbyvision");
        assert_eq!(res.audio[0].channels, 8);
        assert_eq!(res.subtitle[0].language, "eng");
    }

    #[test]
    fn external_subs_only_includes_external_text_subtitles() {
        let cfg = crate::config::Config {
            server_url: "http://localhost:8096".into(),
            access_token: "token".into(),
            ..Default::default()
        };
        let client = JellyfinClient::new(&cfg).unwrap();
        let source = MediaSource {
            id: "source1".into(),
            name: None,
            container: Some("mkv".into()),
            path: None,
            size: None,
            bitrate: None,
            supports_direct_play: true,
            supports_direct_stream: true,
            direct_stream_url: None,
            transcoding_url: None,
            media_streams: Some(vec![
                // 1. Internal PGS subtitle - MUST be excluded to prevent "Please wait" hang
                MediaStream {
                    stream_type: "Subtitle".into(),
                    index: 1,
                    language: Some("eng".into()),
                    codec: Some("pgssub".into()),
                    is_external: false,
                    is_text_subtitle: false,
                    supports_external_stream: true,
                    delivery_url: None,
                    is_default: false,
                    is_forced: false,
                    width: None,
                    height: None,
                    channels: None,
                    aspect_ratio: None,
                    profile: None,
                    video_range_type: None,
                    dv_profile: None,
                },
                // 2. External PGS subtitle - Graphic format, MUST be excluded
                MediaStream {
                    stream_type: "Subtitle".into(),
                    index: 2,
                    language: Some("spa".into()),
                    codec: Some("pgs".into()),
                    is_external: true,
                    is_text_subtitle: false,
                    supports_external_stream: true,
                    delivery_url: Some("/Videos/item1/source1/Subtitles/2/Stream.pgs".into()),
                    is_default: false,
                    is_forced: false,
                    width: None,
                    height: None,
                    channels: None,
                    aspect_ratio: None,
                    profile: None,
                    video_range_type: None,
                    dv_profile: None,
                },
                // 3. External SRT subtitle - Text format, MUST be included
                MediaStream {
                    stream_type: "Subtitle".into(),
                    index: 3,
                    language: Some("fra".into()),
                    codec: Some("subrip".into()),
                    is_external: true,
                    is_text_subtitle: true,
                    supports_external_stream: true,
                    delivery_url: Some("/Videos/item1/source1/Subtitles/3/Stream.srt".into()),
                    is_default: true,
                    is_forced: false,
                    width: None,
                    height: None,
                    channels: None,
                    aspect_ratio: None,
                    profile: None,
                    video_range_type: None,
                    dv_profile: None,
                },
            ]),
        };

        let subs = external_subs(&client, "item1", &source);
        assert_eq!(subs.len(), 1, "Only external text subtitles must be included");
        assert_eq!(subs[0].codec, "subrip");
        assert_eq!(subs[0].language, "fra");
        assert_eq!(subs[0].url, "http://localhost:8096/Videos/item1/source1/Subtitles/3/Stream.srt");
    }

    #[test]
    fn pick_source_uses_the_selected_version() {
        let tall = MediaSource {
            id: "uhd".into(),
            media_streams: Some(vec![MediaStream {
                stream_type: "Video".into(),
                height: Some(2160),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let short = MediaSource {
            id: "hd".into(),
            media_streams: Some(vec![MediaStream {
                stream_type: "Video".into(),
                height: Some(1080),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let sources = [tall, short];
        assert_eq!(pick_source(&sources, Some("HD")).unwrap().id, "hd");
        assert_eq!(pick_source(&sources, None).unwrap().id, "uhd");
    }

    #[test]
    fn version_label_formats_and_cleans_properly() {
        let s1 = MediaSource {
            id: "s1".into(),
            name: Some("2160p HEVC TrueHD Atmos DV HDR10Plus]-CiNEPHiLES".into()),
            media_streams: Some(vec![MediaStream {
                stream_type: "Video".into(),
                height: Some(2160),
                codec: Some("hevc".into()),
                ..Default::default()
            }]),
            ..Default::default()
        };
        assert_eq!(s1.version_label(), "2160p HEVC TrueHD Atmos DV HDR10Plus");

        let s2 = MediaSource {
            id: "s2".into(),
            name: Some("1080p AVC TrueHD Atmos ]-CiNEPHiLES".into()),
            media_streams: Some(vec![MediaStream {
                stream_type: "Video".into(),
                height: Some(1080),
                codec: Some("h264".into()),
                ..Default::default()
            }]),
            ..Default::default()
        };
        assert_eq!(s2.version_label(), "1080p AVC TrueHD Atmos");

        let s3 = MediaSource {
            id: "s3".into(),
            name: Some("[WEBDL-2160p x264 AAC ]-AMORT".into()),
            media_streams: Some(vec![MediaStream {
                stream_type: "Video".into(),
                height: Some(2160),
                codec: Some("h264".into()),
                ..Default::default()
            }]),
            ..Default::default()
        };
        assert_eq!(s3.version_label(), "WEBDL-2160p x264 AAC");

        let s4 = MediaSource {
            id: "s4".into(),
            name: Some("Director's Cut".into()),
            media_streams: Some(vec![MediaStream {
                stream_type: "Video".into(),
                height: Some(1080),
                codec: Some("hevc".into()),
                ..Default::default()
            }]),
            ..Default::default()
        };
        assert_eq!(s4.version_label(), "1080p Director's Cut");
    }
}
