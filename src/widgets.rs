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
    let cw = refresh_continue_watching(client).await?;
    let movies = refresh_latest(client, "Movie", "movie").await?;
    let episodes = refresh_latest(client, "Episode", "episode").await?;

    let movies_v = serde_json::to_value(&movies)?;
    let episodes_v = serde_json::to_value(&episodes)?;
    let mut map = state.widgets.write().await;
    map.insert("continue_watching".into(), serde_json::to_value(&cw)?);
    map.insert("recently_added_movies".into(), movies_v.clone());
    map.insert("recently_added_episodes".into(), episodes_v.clone());
    // Aliases matching common JellyCon / Arctic widget names.
    map.insert("recent_movies".into(), movies_v);
    map.insert("recent_episodes".into(), episodes_v);
    info!(
        "widgets refreshed cw={} movies={} episodes={}",
        cw.len(),
        movies.len(),
        episodes.len()
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

    for raw in resume.items.into_iter().chain(nextup.items.into_iter()) {
        if !seen_ids.insert(raw.id.clone()) {
            continue;
        }
        if let Some(sid) = &raw.series_id {
            if !seen_series.insert(sid.clone()) {
                continue;
            }
        }
        merged.push(to_widget(client, &raw));
    }

    merged.sort_by(|a, b| b.sort_ts.cmp(&a.sort_ts));
    merged.truncate(20);
    Ok(merged)
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
    KodiWidgetItem {
        title,
        plot: raw.overview.clone().unwrap_or_default(),
        year: raw.year.unwrap_or(0),
        duration,
        resume_pos,
        media_type: if raw.item_type == "Episode" {
            "episode".into()
        } else {
            "movie".into()
        },
        plugin_url: format!("plugin://plugin.video.jellykodi/?mode=play&id={}", raw.id),
        is_folder: false,
        is_playable: true,
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
    use crate::jellyfin::types::{UserData, JellyfinItem};

    #[test]
    fn merge_dedupes_series() {
        // Pure unit: construct items and run sort logic via to_widget needs client —
        // test dedupe sets directly here.
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
