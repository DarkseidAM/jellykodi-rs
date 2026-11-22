pub mod schema;

use crate::config::Config;
use crate::error::{JellyError, Result};
use crate::jellyfin::types::JellyfinItem;
use crate::jellyfin::JellyfinClient;
use crate::state::AppState;
use crate::widgets;
use chrono::Utc;
use futures_util::stream::{self, StreamExt, TryStreamExt};
use schema::{
    apply_playback, bulk_insert_items, delete_mapped, finalize_staging_db, insert_live_items,
    open_fresh_db, swap_into_place, FkCache, MapDb,
};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;
use tracing::{info, warn};

/// Full metadata for roots (movies/series/boxsets) including MediaSources so multi-versions are populated.
const SYNC_FIELDS_ROOTS: &str = "DateCreated,PremiereDate,CommunityRating,OfficialRating,Genres,Studios,People,Overview,ProviderIds,RunTimeTicks,UserData,PrimaryVersionId,ParentId,SeriesId,SeasonId,ParentIndexNumber,IndexNumber,ProductionYear,MediaStreams,MediaSources,MediaSourceCount,Tagline,Tags,ProductionLocations,RemoteTrailers,SortName,Status,ImageTags,BackdropImageTags,VoteCount";

/// Full metadata for movies / series / boxsets (People joins are expensive on episodes).
const SYNC_FIELDS_FULL: &str = "DateCreated,PremiereDate,CommunityRating,OfficialRating,Genres,Studios,People,Overview,ProviderIds,RunTimeTicks,UserData,PrimaryVersionId,ParentId,SeriesId,SeasonId,ParentIndexNumber,IndexNumber,ProductionYear,MediaStreams,MediaSourceCount,Tagline,Tags,ProductionLocations,RemoteTrailers,SortName,Status,ImageTags,BackdropImageTags,VoteCount";

/// Lean fields for Season/Episode — no People/Studios/Genres (huge JF JOIN cost).
const SYNC_FIELDS_LEAN: &str = "DateCreated,PremiereDate,CommunityRating,OfficialRating,Overview,ProviderIds,RunTimeTicks,UserData,PrimaryVersionId,ParentId,SeriesId,SeasonId,ParentIndexNumber,IndexNumber,ProductionYear,MediaStreams";

/// Commit SQLite every N items inside a pass (fewer fsyncs on eMMC).
const SQLITE_COMMIT_EVERY: usize = 4500;

pub async fn run_sync_workers(client: JellyfinClient, cfg: Config, state: Arc<AppState>) {
    if let Err(e) = widgets::refresh_all_widgets(&client, &state).await {
        warn!("initial widget refresh failed: {e}");
        *state.last_error.write().await = Some(e.to_string());
    }

    let map_empty =
        !cfg.map_db_path.exists() || MapDb::count(&cfg.map_db_path).unwrap_or(0) == 0;
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
                if state.syncing.load(Ordering::Relaxed) {
                    continue;
                }
                if let Err(e) = delta_sync(&client, &cfg, &state, &last_delta).await {
                    warn!("delta sync failed: {e}");
                } else {
                    last_delta = Utc::now().to_rfc3339();
                }
                let _ = widgets::refresh_all_widgets(&client, &state).await;
            }
            _ = tokio::time::sleep(poll) => {
                if state.syncing.load(Ordering::Relaxed) {
                    continue;
                }
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

pub async fn full_sync(client: &JellyfinClient, cfg: &Config, state: &Arc<AppState>) -> Result<()> {
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
    info!("full sync starting (ordered passes + fetch/db pipeline)");
    std::fs::create_dir_all(&cfg.staging_dir)?;
    std::fs::create_dir_all(&cfg.kodi_db_dir)?;

    let staging = cfg.staging_dir.join("MyVideos131_jellykodi_staging.db");
    let _ = std::fs::remove_file(&staging);
    let conn = open_fresh_db(&staging)?;
    let cache = FkCache::init(&conn)?;

    let concurrency = if state.is_playing() {
        cfg.sync_concurrency_playing
    } else {
        cfg.sync_concurrency
    };

    // Pass 1: library roots with full People/Studios/Genres and MediaSources.
    let (conn, cache, n1) = sync_pass(SyncPass {
        client,
        cfg,
        concurrency,
        include_types: "Movie,Series,BoxSet",
        fields: SYNC_FIELDS_ROOTS,
        cast_limit: cfg.cast_limit,
        pass_name: "roots",
        conn,
        cache,
    })
    .await?;
    // Pass 2: seasons (lean fields) — cache.shows populated from pass 1.
    let (conn, cache, n2) = sync_pass(SyncPass {
        client,
        cfg,
        concurrency,
        include_types: "Season",
        fields: SYNC_FIELDS_LEAN,
        cast_limit: 0,
        pass_name: "seasons",
        conn,
        cache,
    })
    .await?;
    // Pass 3: episodes with the same fields and cast budget as movies.
    let (mut conn, cache, n3) = sync_pass(SyncPass {
        client,
        cfg,
        concurrency,
        include_types: "Episode",
        fields: SYNC_FIELDS_FULL,
        cast_limit: cfg.cast_limit,
        pass_name: "episodes",
        conn,
        cache,
    })
    .await?;

    link_boxset_membership(client, &mut conn, &cache).await?;
    let map_rows = {
        let mut cache = cache;
        cache.take_map()
    };
    let total_seen = n1 + n2 + n3;
    finalize_staging_db(&mut conn)?;
    drop(conn);

    let target = find_or_create_myvideos_path(&cfg.kodi_db_dir)?;
    swap_into_place(&staging, &target)?;
    MapDb::rebuild_from_pass(&cfg.map_db_path, total_seen, &map_rows)?;
    *state.library_items.write().await = total_seen;
    *state.last_full_sync.write().await = Some(Utc::now().to_rfc3339());
    *state.last_error.write().await = None;
    info!(
        "full sync complete items≈{total_seen} (roots={n1} seasons={n2} episodes={n3}) -> {}",
        target.display()
    );
    let _ = widgets::refresh_all_widgets(client, state).await;
    Ok(())
}

struct SyncPass<'a> {
    client: &'a JellyfinClient,
    cfg: &'a Config,
    concurrency: usize,
    include_types: &'a str,
    fields: &'a str,
    cast_limit: usize,
    pass_name: &'a str,
    conn: rusqlite::Connection,
    cache: FkCache,
}

/// Fetch one IncludeItemTypes set with overlapping HTTP + single-threaded SQLite writer.
async fn sync_pass(
    pass: SyncPass<'_>,
) -> Result<(rusqlite::Connection, FkCache, u64)> {
    let SyncPass {
        client,
        cfg,
        concurrency,
        include_types,
        fields,
        cast_limit,
        pass_name,
        conn,
        cache,
    } = pass;
    let page = cfg.sync_page_size;
    let t_pass = Instant::now();

    let first = client
        .items_page(include_types, 0, page, fields)
        .await?;
    let total = first
        .total_record_count
        .unwrap_or(first.items.len() as u64);
    info!(pass = pass_name, total, "sync pass begin");

    let (tx, rx) = mpsc::channel::<Vec<JellyfinItem>>(8);
    let client_db = client.clone();

    let writer = tokio::task::spawn_blocking(move || -> Result<(rusqlite::Connection, FkCache, u64)> {
        let mut conn = conn;
        let mut cache = cache;
        let mut buf: Vec<JellyfinItem> = Vec::with_capacity(SQLITE_COMMIT_EVERY);
        let mut written = 0u64;
        let mut rx = rx;

        let mut flush = |buf: &mut Vec<JellyfinItem>,
                         conn: &mut rusqlite::Connection,
                         cache: &mut FkCache|
         -> Result<()> {
            if buf.is_empty() {
                return Ok(());
            }
            let t0 = Instant::now();
            let n = buf.len();
            bulk_insert_items(conn, cache, &client_db, buf, cast_limit)?;
            written += n as u64;
            info!(
                db_ms = t0.elapsed().as_millis() as u64,
                n,
                written,
                "sqlite flush"
            );
            buf.clear();
            Ok(())
        };

        while let Some(batch) = rx.blocking_recv() {
            buf.extend(batch);
            if buf.len() >= SQLITE_COMMIT_EVERY {
                flush(&mut buf, &mut conn, &mut cache)?;
            }
        }
        flush(&mut buf, &mut conn, &mut cache)?;
        Ok((conn, cache, written))
    });

    // Page 0
    let first_items = attach_movie_versions(client, filter_primary_versions(first.items)).await;
    if !first_items.is_empty() {
        tx.send(first_items)
            .await
            .map_err(|e| JellyError::Other(format!("sync channel: {e}")))?;
    }

    let offsets: Vec<u32> = (1u32..)
        .map(|i| i.saturating_mul(page))
        .take_while(|&o| (o as u64) < total)
        .collect();

    let client_f = client.clone();
    let include = include_types.to_string();
    let fields_s = fields.to_string();

    let fetched = stream::iter(offsets)
        .map(|offset| {
            let c = client_f.clone();
            let inc = include.clone();
            let f = fields_s.clone();
            async move {
                let t0 = Instant::now();
                let env = c.items_page(&inc, offset, page, &f).await?;
                let fetch_ms = t0.elapsed().as_millis() as u64;
                let items = attach_movie_versions(&c, filter_primary_versions(env.items)).await;
                Ok::<_, JellyError>((offset, items, fetch_ms))
            }
        })
        .buffer_unordered(concurrency.max(1))
        .try_for_each(|(offset, items, fetch_ms)| {
            let tx = tx.clone();
            async move {
                info!(
                    pass = pass_name,
                    offset,
                    fetch_ms,
                    n = items.len(),
                    "fetched"
                );
                if !items.is_empty() {
                    tx.send(items)
                        .await
                        .map_err(|e| JellyError::Other(format!("sync channel: {e}")))?;
                }
                Ok(())
            }
        })
        .await;

    drop(tx);
    let joined = writer
        .await
        .map_err(|e| JellyError::Other(format!("writer join: {e}")))?;
    if let Err(write_err) = &joined {
        warn!(pass = pass_name, "sqlite writer failed: {write_err}");
    }
    fetched?;
    let (conn, cache, written) = joined?;

    info!(
        pass = pass_name,
        written,
        pass_ms = t_pass.elapsed().as_millis() as u64,
        "sync pass complete"
    );
    Ok((conn, cache, written))
}

/// Library queries hide alternate version items. Pull every source onto the primary movie.
async fn attach_movie_versions(client: &JellyfinClient, mut items: Vec<JellyfinItem>) -> Vec<JellyfinItem> {
    let ids: Vec<String> = items
        .iter()
        .filter(|it| {
            it.item_type == "Movie"
                && it.media_source_count.unwrap_or(0) > 1
                && it.media_sources.as_ref().map_or(0, |s| s.len()) <= 1
        })
        .map(|it| it.id.clone())
        .collect();
    if ids.is_empty() {
        return items;
    }
    let fetched: Vec<(String, Vec<crate::jellyfin::types::MediaSource>)> = stream::iter(ids)
        .map(|id| {
            let client = client.clone();
            async move {
                match client.item_media_sources(&id).await {
                    Ok(sources) => (id, sources),
                    Err(e) => {
                        warn!("media sources for {id} failed: {e}");
                        (id, Vec::new())
                    }
                }
            }
        })
        .buffer_unordered(8)
        .collect()
        .await;
    let by_id: std::collections::HashMap<String, Vec<crate::jellyfin::types::MediaSource>> =
        fetched.into_iter().collect();
    for item in &mut items {
        if let Some(sources) = by_id.get(&item.id)
            && sources.len() > 1
        {
            item.media_sources = Some(sources.clone());
        }
    }
    items
}

/// Keep alternate movie files (stored as extra videoversions). Drop alternate
/// episodes so a title does not appear twice in a season.
fn filter_primary_versions(items: Vec<JellyfinItem>) -> Vec<JellyfinItem> {
    items
        .into_iter()
        .filter(|it| {
            let alternate = matches!(&it.primary_version_id, Some(pid) if pid != &it.id);
            !(alternate && it.item_type == "Episode")
        })
        .collect()
}

async fn link_boxset_membership(
    client: &JellyfinClient,
    conn: &mut rusqlite::Connection,
    cache: &FkCache,
) -> Result<()> {
    use crate::sync::schema::set_movie_collection;
    for (boxset_id, set_id) in cache.boxset_pairs() {
        let page = client
            .items_by_parent(&boxset_id, "Movie", 300, "Name")
            .await?;
        for movie in page.items {
            if let Some(movie_id) = cache.movie_id(&movie.id) {
                set_movie_collection(conn, movie_id, set_id)?;
            }
        }
    }
    Ok(())
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
            if !resp.items.is_empty() {
                apply_kodisync(client, cfg, &resp.items).await?;
                let _ = widgets::refresh_all_widgets(client, state).await;
            }
            Ok(())
        }
        Err(e) => {
            warn!("kodisync unavailable ({e}); widget refresh only");
            widgets::refresh_all_widgets(client, state).await
        }
    }
}

async fn apply_kodisync(
    client: &JellyfinClient,
    cfg: &Config,
    items: &[crate::jellyfin::types::KodiSyncQueueItem],
) -> Result<()> {
    let videos = find_or_create_myvideos_path(&cfg.kodi_db_dir)?;
    let map_path = cfg.map_db_path.clone();
    let mut to_insert = Vec::new();
    for it in items {
        let Some(id) = it.item_id.as_deref() else {
            continue;
        };
        if id.is_empty() {
            continue;
        }
        match it.change_type.as_deref() {
            Some("Removed") => {
                let videos = videos.clone();
                let map_path = map_path.clone();
                let id = id.to_string();
                tokio::task::spawn_blocking(move || delete_mapped(&videos, &map_path, &id))
                    .await
                    .map_err(|e| JellyError::Other(format!("delete join: {e}")))??;
            }
            _ => {
                let item = match client.get_item(id).await {
                    Ok(item) => item,
                    Err(e) => {
                        warn!("delta get_item {id} failed: {e}");
                        continue;
                    }
                };
                let videos_pb = videos.clone();
                let map_pb = map_path.clone();
                let id_owned = id.to_string();
                let (pos, dur) = playback_secs(&item);
                let known = MapDb::lookup(&map_path, id).unwrap_or(None).is_some();
                tokio::task::spawn_blocking(move || {
                    apply_playback(&videos_pb, &map_pb, &id_owned, pos, dur)
                })
                .await
                .map_err(|e| JellyError::Other(format!("userdata join: {e}")))??;
                let kind = item.item_type.as_str();
                let keep = matches!(kind, "Movie" | "Series" | "Season" | "Episode" | "BoxSet");
                if !known && keep && it.change_type.as_deref() != Some("Removed") {
                    to_insert.push(item);
                }
            }
        }
    }
    if !to_insert.is_empty() {
        let client_db = client.clone();
        let cast_limit = cfg.cast_limit;
        tokio::task::spawn_blocking(move || {
            insert_live_items(&videos, &map_path, &client_db, &to_insert, cast_limit)
        })
        .await
        .map_err(|e| JellyError::Other(format!("insert join: {e}")))??;
    }
    Ok(())
}

fn playback_secs(item: &JellyfinItem) -> (f64, f64) {
    let pos = item
        .user_data
        .as_ref()
        .and_then(|u| u.position_ticks)
        .unwrap_or(0) as f64
        / 10_000_000.0;
    let dur = item.runtime_ticks.unwrap_or(0) as f64 / 10_000_000.0;
    (pos, dur)
}

pub fn find_or_create_myvideos_path(db_dir: &PathBuf) -> Result<PathBuf> {
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
    for p in candidates.iter().rev() {
        if let Ok(conn) = rusqlite::Connection::open(p) {
            let ok: bool = conn
                .query_row(
                    "SELECT 1 FROM sqlite_master WHERE type='table' AND name='version'",
                    [],
                    |_| Ok(true),
                )
                .unwrap_or(false);
            if ok {
                return Ok(p.clone());
            }
        }
    }
    Ok(db_dir.join("MyVideos149.db"))
}
