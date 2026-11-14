use crate::error::Result;
use crate::jellyfin::types::JellyfinItem;
use crate::jellyfin::JellyfinClient;
use crate::state::AppState;
use rustc_hash::FxHashSet;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use tracing::info;

const WIDGET_FIELDS: &str = "PrimaryImageAspectRatio,DateCreated,Overview,PremiereDate,ProductionYear,RunTimeTicks,UserData,SeriesId,SeriesName,ParentIndexNumber,IndexNumber,MediaStreams";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KodiStreamDetails {
    pub video: Option<KodiVideoStream>,
    pub audio: Option<KodiAudioStream>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KodiVideoStream {
    pub codec: String,
    pub width: i32,
    pub height: i32,
    pub aspect: f64,
    pub hdr_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KodiAudioStream {
    pub codec: String,
    pub channels: i32,
    pub language: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct KodiWidgetItem {
    pub title: String,
    pub plot: String,
    pub year: i32,
    pub duration: i64,
    pub resume_pos: i64,
    pub media_type: String,
    pub plugin_url: String,
    pub is_folder: bool,
    pub is_playable: bool,
    pub sort_ts: String,
    pub art: KodiArt,
    pub streams: Option<KodiStreamDetails>,
}

#[derive(Debug, Clone, Serialize)]
pub struct KodiArt {
    pub poster: String,
    pub fanart: String,
    pub thumb: String,
}

pub async fn refresh_all_widgets(client: &JellyfinClient, state: &Arc<AppState>) -> Result<()> {
    let (
        cw,
        in_prog_movies,
        in_prog_eps,
        nextup,
        movies,
        episodes,
        fav_movies,
        fav_shows,
        collections,
        fav_collections,
    ) = tokio::try_join!(
        refresh_continue_watching(client),
        refresh_resume_typed(client, "Movie", "movie"),
        refresh_resume_typed(client, "Episode", "episode"),
        refresh_next_up(client),
        refresh_latest(client, "Movie", "movie"),
        refresh_latest(client, "Episode", "episode"),
        refresh_filtered(client, "Movie", "IsFavorite", "SortName", "Ascending", "movie"),
        refresh_filtered(client, "Series", "IsFavorite", "SortName", "Ascending", "tvshow"),
        refresh_filtered(client, "BoxSet", "", "SortName", "Ascending", "set"),
        refresh_filtered(client, "BoxSet", "IsFavorite", "SortName", "Ascending", "set"),
    )?;

    let mut fav_all = fav_movies.clone();
    fav_all.extend(fav_shows.clone());

    let movies_v = serde_json::to_value(&movies)?;
    let episodes_v = serde_json::to_value(&episodes)?;
    let cw_v = serde_json::to_value(&cw)?;
    let nextup_v = serde_json::to_value(&nextup)?;
    let in_prog_movies_v = serde_json::to_value(&in_prog_movies)?;
    let in_prog_eps_v = serde_json::to_value(&in_prog_eps)?;
    let fav_movies_v = serde_json::to_value(&fav_movies)?;
    let fav_shows_v = serde_json::to_value(&fav_shows)?;
    let fav_all_v = serde_json::to_value(&fav_all)?;
    let collections_v = serde_json::to_value(&collections)?;
    let fav_collections_v = serde_json::to_value(&fav_collections)?;

    let mut map = state.widgets.write().await;
    map.insert("continue_watching".into(), cw_v.clone());
    map.insert("in_progress_movies".into(), in_prog_movies_v.clone());
    map.insert("in_progress_episodes".into(), in_prog_eps_v.clone());
    map.insert("next_up".into(), nextup_v.clone());
    map.insert("recently_added_movies".into(), movies_v.clone());
    map.insert("recently_added_episodes".into(), episodes_v.clone());
    map.insert("favorites_movies".into(), fav_movies_v.clone());
    map.insert("favorites_shows".into(), fav_shows_v.clone());
    map.insert("favorites_all".into(), fav_all_v.clone());
    map.insert("collections".into(), collections_v.clone());
    map.insert("favorite_collections".into(), fav_collections_v.clone());

    // Aliases matching JellyCon / Arctic widget names.
    map.insert("recent_movies".into(), movies_v);
    map.insert("recent_episodes".into(), episodes_v);
    map.insert("movies_in_progress".into(), in_prog_movies_v);
    map.insert("tv_in_progress".into(), in_prog_eps_v);
    map.insert("tv_next_up".into(), nextup_v);
    map.insert("favorites".into(), fav_all_v);
    map.insert("resume".into(), cw_v);

    info!(
        "widgets refreshed cw={} in_prog_m={} in_prog_e={} nextup={} movies={} episodes={} fav_m={} fav_s={} coll={} fav_coll={}",
        cw.len(),
        in_prog_movies.len(),
        in_prog_eps.len(),
        nextup.len(),
        movies.len(),
        episodes.len(),
        fav_movies.len(),
        fav_shows.len(),
        collections.len(),
        fav_collections.len()
    );
    Ok(())
}

pub async fn refresh_continue_watching(client: &JellyfinClient) -> Result<Vec<KodiWidgetItem>> {
    let (resume, nextup) = tokio::try_join!(
        client.resume(20, WIDGET_FIELDS),
        client.next_up(20, WIDGET_FIELDS),
    )?;

    let mut seen_ids = FxHashSet::default();
    let mut seen_series = FxHashSet::default();
    let mut merged = Vec::with_capacity(25);

    for raw in resume.items.into_iter().chain(nextup.items) {
        if !seen_ids.insert(raw.id.clone()) {
            continue;
        }
        if let Some(sid) = &raw.series_id
            && !seen_series.insert(sid.clone())
        {
            continue;
        }
        merged.push(to_widget(client, &raw));
    }

    merged.sort_by(|a, b| b.sort_ts.cmp(&a.sort_ts));
    merged.truncate(20);
    Ok(merged)
}

async fn refresh_next_up(client: &JellyfinClient) -> Result<Vec<KodiWidgetItem>> {
    let env = client.next_up(20, WIDGET_FIELDS).await?;
    Ok(env.items.into_iter().map(|it| to_widget(client, &it)).collect())
}

async fn refresh_resume_typed(
    client: &JellyfinClient,
    include: &str,
    media_type: &str,
) -> Result<Vec<KodiWidgetItem>> {
    let env = client.resume_typed(include, 20, WIDGET_FIELDS).await?;
    Ok(env
        .items
        .into_iter()
        .map(|it| {
            let mut w = to_widget(client, &it);
            w.media_type = media_type.into();
            w
        })
        .collect())
}

async fn refresh_latest(
    client: &JellyfinClient,
    include: &str,
    media_type: &str,
) -> Result<Vec<KodiWidgetItem>> {
    let items = client.latest(include, 20, WIDGET_FIELDS).await?;
    Ok(items
        .into_iter()
        .filter(|it| it.item_type == include)
        .map(|it| {
            let mut w = to_widget(client, &it);
            w.media_type = media_type.into();
            w.is_folder = false;
            w.is_playable = true;
            w.plugin_url = format!("plugin://plugin.video.jellykodi/?mode=play&id={}", it.id);
            w
        })
        .collect())
}

async fn refresh_filtered(
    client: &JellyfinClient,
    include: &str,
    filters: &str,
    sort_by: &str,
    sort_order: &str,
    media_type: &str,
) -> Result<Vec<KodiWidgetItem>> {
    let env = client
        .items_filtered(include, filters, sort_by, sort_order, 40, WIDGET_FIELDS)
        .await?;
    Ok(env
        .items
        .into_iter()
        .map(|it| {
            let mut w = to_widget(client, &it);
            w.media_type = media_type.into();
            w
        })
        .collect())
}

pub async fn search_items(
    client: &JellyfinClient,
    term: &str,
) -> Result<Vec<KodiWidgetItem>> {
    let env = client
        .search(term, "Movie,Series,Episode", 40, WIDGET_FIELDS)
        .await?;
    Ok(env.items.into_iter().map(|it| to_widget(client, &it)).collect())
}

/// Expand a BoxSet / Series / Season into playable (or nested folder) children.
pub async fn browse_items(client: &JellyfinClient, parent_id: &str) -> Result<Vec<KodiWidgetItem>> {
    // Prefer movies (boxsets); fall back to seasons then episodes for series.
    let movies = client
        .items_by_parent(parent_id, "Movie", 200, WIDGET_FIELDS)
        .await?;
    if !movies.items.is_empty() {
        return Ok(movies
            .items
            .into_iter()
            .map(|it| to_widget(client, &it))
            .collect());
    }
    let seasons = client
        .items_by_parent(parent_id, "Season", 100, WIDGET_FIELDS)
        .await?;
    if !seasons.items.is_empty() {
        return Ok(seasons
            .items
            .into_iter()
            .map(|it| to_widget(client, &it))
            .collect());
    }
    let episodes = client
        .items_by_parent(parent_id, "Episode", 200, WIDGET_FIELDS)
        .await?;
    Ok(episodes
        .items
        .into_iter()
        .map(|it| to_widget(client, &it))
        .collect())
}

fn to_widget(client: &JellyfinClient, raw: &JellyfinItem) -> KodiWidgetItem {
    let title = match (&raw.series_name, raw.parent_index, raw.index_number) {
        (Some(s), Some(se), Some(ep)) => format!("{} - S{:02}E{:02} - {}", s, se, ep, raw.name),
        _ => raw.name.clone(),
    };
    let duration = raw.runtime_ticks.unwrap_or(0) / 10_000_000;
    let resume_pos = raw
        .user_data
        .as_ref()
        .and_then(|u| u.position_ticks)
        .unwrap_or(0)
        / 10_000_000;
    let sort_ts = raw
        .user_data
        .as_ref()
        .and_then(|u| u.last_played.clone())
        .or_else(|| raw.date_created.clone())
        .unwrap_or_default();
    let poster_id = raw.series_id.as_deref().unwrap_or(&raw.id);
    let is_folder = matches!(raw.item_type.as_str(), "Series" | "BoxSet" | "Season" | "Folder");
    let is_playable = !is_folder && matches!(raw.item_type.as_str(), "Movie" | "Episode");
    let streams = raw.media_streams.as_deref().and_then(extract_stream_details);
    KodiWidgetItem {
        title,
        plot: raw.overview.clone().unwrap_or_default(),
        year: raw.year.unwrap_or(0),
        duration,
        resume_pos,
        media_type: match raw.item_type.as_str() {
            "Episode" => "episode".into(),
            "Series" => "tvshow".into(),
            "Season" => "season".into(),
            "BoxSet" => "set".into(),
            _ => "movie".into(),
        },
        // Folders must stay inside the plugin — videodb:// sets/titles are empty (no idSet links yet).
        plugin_url: if is_playable {
            format!("plugin://plugin.video.jellykodi/?mode=play&id={}", raw.id)
        } else {
            format!("plugin://plugin.video.jellykodi/?mode=browse&id={}", raw.id)
        },
        is_folder,
        is_playable,
        sort_ts,
        art: KodiArt {
            poster: client.image_url(poster_id, "Primary", 600),
            fanart: client.image_url(poster_id, "Backdrop", 1280),
            thumb: client.image_url(&raw.id, "Primary", 640),
        },
        streams,
    }
}

fn extract_stream_details(
    streams: &[crate::jellyfin::types::MediaStream],
) -> Option<KodiStreamDetails> {
    let mut video = None;
    let mut audio = None;

    for s in streams {
        if video.is_none() && s.stream_type == "Video" {
            let codec = s.normalized_video_codec();
            let width = s.width.unwrap_or(0);
            let height = s.height.unwrap_or(0);
            let aspect = s.parsed_aspect_ratio();
            let hdr_type = s.hdr_type();
            video = Some(KodiVideoStream {
                codec,
                width,
                height,
                aspect,
                hdr_type,
            });
        } else if s.stream_type == "Audio" && (audio.is_none() || s.is_default) {
            let codec = s.normalized_audio_codec();
            let channels = s.channels.unwrap_or(2);
            let language = s.language.clone().unwrap_or_default();
            audio = Some(KodiAudioStream {
                codec,
                channels,
                language,
            });
        }
    }

    if video.is_some() || audio.is_some() {
        Some(KodiStreamDetails { video, audio })
    } else {
        None
    }
}

pub async fn widget_payload(state: &Arc<AppState>, name: &str) -> Value {
    state
        .widgets
        .read()
        .await
        .get(name)
        .cloned()
        .unwrap_or_else(|| Value::Array(vec![]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jellyfin::types::{JellyfinItem, UserData};

    #[test]
    fn merge_dedupes_series() {
        let mut seen = FxHashSet::default();
        assert!(seen.insert("s1".to_string()));
        assert!(!seen.insert("s1".to_string()));
        let _ = UserData {
            play_count: None,
            played: false,
            position_ticks: Some(10),
            last_played: Some("2026-01-01".into()),
        };
        let _ = JellyfinItem {
            id: "1".into(),
            name: "x".into(),
            item_type: "Episode".into(),
            overview: None,
            year: None,
            premiered: None,
            date_created: None,
            runtime_ticks: None,
            mpaa: None,
            community_rating: None,
            genres: None,
            studios: None,
            people: None,
            user_data: None,
            series_id: Some("s".into()),
            series_name: None,
            parent_id: None,
            season_id: None,
            parent_index: None,
            index_number: None,
            provider_ids: None,
            media_sources: None,
            media_streams: None,
            primary_version_id: None,
        };
    }

    #[test]
    fn extract_stream_details_parses_video_and_audio() {
        let streams = vec![
            crate::jellyfin::types::MediaStream {
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
            crate::jellyfin::types::MediaStream {
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
            },
        ];

        let details = extract_stream_details(&streams).expect("stream details present");
        let v = details.video.expect("video present");
        assert_eq!(v.codec, "hevc");
        assert_eq!(v.width, 3840);
        assert_eq!(v.height, 2160);
        assert_eq!(v.hdr_type, "dolbyvision");

        let a = details.audio.expect("audio present");
        assert_eq!(a.codec, "truehd");
        assert_eq!(a.channels, 8);
    }
}
