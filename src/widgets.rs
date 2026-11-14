use crate::error::Result;
use crate::jellyfin::types::JellyfinItem;
use crate::jellyfin::JellyfinClient;
use crate::state::AppState;
use rustc_hash::FxHashSet;
use serde::Serialize;
use serde_json::Value;
use std::sync::Arc;
use tracing::info;

const WIDGET_FIELDS: &str = "PrimaryImageAspectRatio,DateCreated,Overview,PremiereDate,ProductionYear,RunTimeTicks,UserData,SeriesId,SeriesName,ParentIndexNumber,IndexNumber";

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
        .map(|it| {
            let mut w = to_widget(client, &it);
            w.media_type = media_type.into();
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
    KodiWidgetItem {
        title,
        plot: raw.overview.clone().unwrap_or_default(),
        year: raw.year.unwrap_or(0),
        duration,
        resume_pos,
        media_type: match raw.item_type.as_str() {
            "Episode" => "episode".into(),
            "Series" => "tvshow".into(),
            "BoxSet" => "set".into(),
            _ => "movie".into(),
        },
        plugin_url: if is_playable {
            format!("plugin://plugin.video.jellykodi/?mode=play&id={}", raw.id)
        } else if raw.item_type == "Series" {
            // Folder stub — open native TV library (series browse not yet in JK).
            "videodb://tvshows/titles/".into()
        } else if raw.item_type == "BoxSet" {
            "videodb://movies/sets/".into()
        } else {
            format!("plugin://plugin.video.jellykodi/?mode=play&id={}", raw.id)
        },
        is_folder,
        is_playable,
        sort_ts,
        art: KodiArt {
            poster: client.image_url(poster_id, "Primary", 600),
            fanart: client.image_url(poster_id, "Backdrop", 1280),
            thumb: client.image_url(&raw.id, "Primary", 640),
        },
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
            primary_version_id: None,
        };
    }
}
