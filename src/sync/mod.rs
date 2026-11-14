pub mod schema;

use crate::config::Config;
use crate::error::{JellyError, Result};
use crate::jellyfin::types::JellyfinItem;
use crate::jellyfin::JellyfinClient;
use crate::state::AppState;
use crate::widgets;
use chrono::Utc;
use schema::{bulk_insert_items, open_fresh_db, swap_into_place, FkCache, MapDb};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tracing::{info, warn};

const SYNC_FIELDS: &str =
    "DateCreated,PremiereDate,CommunityRating,OfficialRating,Genres,Studios,People,Overview,ProviderIds,RunTimeTicks,UserData,PrimaryVersionId,ParentId,SeriesId,SeasonId,ParentIndexNumber,IndexNumber,ProductionYear";

const INCLUDE_TYPES: &str = "Movie,Series,Season,Episode,BoxSet";

pub async fn run_sync_workers(client: JellyfinClient, cfg: Config, state: Arc<AppState>) {
    // Initial widget warm + optional full sync if map empty.
    if let Err(e) = widgets::refresh_all_widgets(&client, &state).await {
        warn!("initial widget refresh failed: {e}");
        *state.last_error.write().await = Some(e.to_string());
    }

    let map_empty = !cfg.map_db_path.exists()
        || MapDb::count(&cfg.map_db_path).unwrap_or(0) == 0;
    if map_empty {
        info!("map db empty — starting initial full sync");
        if let Err(e) = full_sync(&client, &cfg, &state).await {
            warn!("initial full sync failed: {e}");
            *state.last_error.write().await = Some(e.to_string());
        }
    }

    let mut last_delta = Utc::now().to_rfc3339();
    let poll = tokio::time::Duration::from_secs(cfg.kodisync_poll_secs);

    loop {
        tokio::select! {
            _ = state.resync_notify.notified() => {
                wait_until_idle(&state).await;
                if let Err(e) = full_sync(&client, &cfg, &state).await {
                    warn!("manual full sync failed: {e}");
                    *state.last_error.write().await = Some(e.to_string());
                } else {
                    last_delta = Utc::now().to_rfc3339();
                }
            }
            _ = state.delta_notify.notified() => {
                if let Err(e) = delta_sync(&client, &cfg, &state, &last_delta).await {
                    warn!("delta sync failed: {e}");
                } else {
                    last_delta = Utc::now().to_rfc3339();
                }
                let _ = widgets::refresh_all_widgets(&client, &state).await;
            }
            _ = tokio::time::sleep(poll) => {
                if let Err(e) = delta_sync(&client, &cfg, &state, &last_delta).await {
                    warn!("kodisync poll failed: {e}");
                } else {
                    last_delta = Utc::now().to_rfc3339();
                }
                let _ = widgets::refresh_all_widgets(&client, &state).await;
            }
        }
    }
}

async fn wait_until_idle(state: &Arc<AppState>) {
    while state.is_playing() {
        info!("full resync waiting for playback idle…");
        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
    }
}

pub async fn full_sync(
    client: &JellyfinClient,
    cfg: &Config,
    state: &Arc<AppState>,
) -> Result<()> {
    state.syncing.store(true, Ordering::SeqCst);
    let result = full_sync_inner(client, cfg, state).await;
    state.syncing.store(false, Ordering::SeqCst);
    result
}

async fn full_sync_inner(
    client: &JellyfinClient,
    cfg: &Config,
    state: &Arc<AppState>,
) -> Result<()> {
    info!("full sync starting (JF 12 Items pagination)");
    std::fs::create_dir_all(&cfg.staging_dir)?;
    std::fs::create_dir_all(&cfg.kodi_db_dir)?;

    let staging = cfg.staging_dir.join("MyVideos131_jellykodi_staging.db");
    let _ = std::fs::remove_file(&staging);
    let mut conn = open_fresh_db(&staging)?;
    let mut cache = FkCache::init(&conn)?;

    let concurrency = if state.is_playing() {
        cfg.sync_concurrency_playing
    } else {
        cfg.sync_concurrency
    };

    let mut total_seen = 0u64;
    let page = cfg.sync_page_size;

    // First page to learn total.
    let first = client
        .items_page(INCLUDE_TYPES, 0, page, SYNC_FIELDS)
        .await?;
    let total = first.total_record_count.unwrap_or(first.items.len() as u64);
    let mut batch = filter_primary_versions(first.items);
    total_seen += batch.len() as u64;
    bulk_insert_items(&mut conn, &mut cache, client, &batch, cfg.cast_limit)?;
    let mut start = page;

    while (start as u64) < total {
        let mut handles = Vec::new();
        for i in 0..concurrency {
            let idx = start + (i as u32) * page;
            if (idx as u64) >= total {
                break;
            }
            let c = client.clone();
            handles.push(tokio::spawn(async move {
                c.items_page(INCLUDE_TYPES, idx, page, SYNC_FIELDS).await
            }));
        }
        if handles.is_empty() {
            break;
        }
        let n = handles.len() as u32;
        for h in handles {
            let env = h
                .await
                .map_err(|e| JellyError::Other(format!("join: {e}")))??;
            batch = filter_primary_versions(env.items);
            total_seen += batch.len() as u64;
            bulk_insert_items(&mut conn, &mut cache, client, &batch, cfg.cast_limit)?;
        }
        start += n * page;
        info!("full sync progress start={start} total≈{total} written≈{total_seen}");
    }

    drop(cache);
    drop(conn);
    let target = find_or_create_myvideos_path(&cfg.kodi_db_dir)?;
    swap_into_place(&staging, &target)?;
    MapDb::rebuild_from_pass(&cfg.map_db_path, total_seen)?;
    *state.library_items.write().await = total_seen;
    *state.last_full_sync.write().await = Some(Utc::now().to_rfc3339());
    *state.last_error.write().await = None;
    info!("full sync complete items≈{total_seen} -> {}", target.display());
    let _ = widgets::refresh_all_widgets(client, state).await;
    Ok(())
}

/// Skip alternate versions that point at a PrimaryVersionId (one card per title).
fn filter_primary_versions(items: Vec<JellyfinItem>) -> Vec<JellyfinItem> {
    items
        .into_iter()
        .filter(|it| {
            // Keep primary/main; drop linked alternates that declare a different primary.
            !matches!(&it.primary_version_id, Some(pid) if pid != &it.id)
        })
        .collect()
}

async fn delta_sync(
    client: &JellyfinClient,
    cfg: &Config,
    state: &Arc<AppState>,
    last_update: &str,
) -> Result<()> {
    match client.kodisync_items(last_update).await {
        Ok(resp) => {
            let mut added = 0u32;
            let mut removed = 0u32;
            let mut updated = 0u32;
            for it in &resp.items {
                match it.change_type.as_deref() {
                    Some("Removed") => removed += 1,
                    Some("Added") => added += 1,
                    _ => updated += 1,
                }
                tracing::debug!(
                    item_id = ?it.item_id,
                    item_type = ?it.item_type,
                    change = ?it.change_type,
                    "kodisync item"
                );
            }
            info!(
                "kodisync delta items={} added={added} updated={updated} removed={removed}",
                resp.items.len()
            );
            // For now refresh widgets always; per-item MyVideos patch lands in later hardening.
            if !resp.items.is_empty() {
                let _ = widgets::refresh_all_widgets(client, state).await;
            }
            let _ = cfg; // map path reserved for incremental writer
            Ok(())
        }
        Err(e) => {
            // Plugin route quirks shouldn't kill the loop — fall back to widget refresh.
            warn!("kodisync unavailable ({e}); widget refresh only");
            widgets::refresh_all_widgets(client, state).await
        }
    }
}

fn find_or_create_myvideos_path(db_dir: &PathBuf) -> Result<PathBuf> {
    std::fs::create_dir_all(db_dir)?;
    let mut candidates: Vec<_> = std::fs::read_dir(db_dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with("MyVideos") && n.ends_with(".db"))
                .unwrap_or(false)
        })
        .collect();
    candidates.sort();
    if let Some(p) = candidates.pop() {
        return Ok(p);
    }
    Ok(db_dir.join("MyVideos131.db"))
}
