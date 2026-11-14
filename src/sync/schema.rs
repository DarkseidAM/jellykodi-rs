use crate::error::Result;
use crate::jellyfin::types::JellyfinItem;
use crate::jellyfin::JellyfinClient;
use rusqlite::{params, Connection};
use rustc_hash::FxHashMap;
use std::path::Path;

/// Real CoreELEC / Kodi 22 `MyVideos149` schema (tables + indexes + version row).
/// Slimmer hand-rolled schemas lack `version` and cause Kodi to error on open.
const SCHEMA: &str = include_str!("myvideos149.sql");

pub struct FkCache {
    actors: FxHashMap<String, i64>,
    shows: FxHashMap<String, i64>, // jellyfin series id -> idShow
    seasons: FxHashMap<String, i64>,
    next_actor: i64,
    next_file: i64,
    next_movie: i64,
    next_show: i64,
    next_season: i64,
    next_episode: i64,
    next_set: i64,
    movie_path_id: i64,
    tv_path_id: i64,
}

pub fn open_fresh_db(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)?;
    conn.execute_batch(SCHEMA)?;
    Ok(conn)
}

impl FkCache {
    pub fn init(conn: &Connection) -> Result<Self> {
        conn.execute(
            "INSERT OR IGNORE INTO path (idPath, strPath, strContent, strScraper) VALUES (1, 'plugin://plugin.video.jellykodi/movies/', 'movies', 'metadata.local')",
            [],
        )?;
        conn.execute(
            "INSERT OR IGNORE INTO path (idPath, strPath, strContent, strScraper) VALUES (2, 'plugin://plugin.video.jellykodi/tvshows/', 'tvshows', 'metadata.local')",
            [],
        )?;
        Ok(Self {
            actors: FxHashMap::default(),
            shows: FxHashMap::default(),
            seasons: FxHashMap::default(),
            next_actor: 1,
            next_file: 1,
            next_movie: 1,
            next_show: 1,
            next_season: 1,
            next_episode: 1,
            next_set: 1,
            movie_path_id: 1,
            tv_path_id: 2,
        })
    }
}

pub fn bulk_insert_items(
    conn: &mut Connection,
    cache: &mut FkCache,
    client: &JellyfinClient,
    items: &[JellyfinItem],
    cast_limit: usize,
) -> Result<()> {
    let tx = conn.transaction()?;
    {
        for item in items {
            match item.item_type.as_str() {
                "Movie" => insert_movie(&tx, cache, client, item, cast_limit)?,
                "Series" => insert_series(&tx, cache, client, item)?,
                "Season" => insert_season(&tx, cache, client, item)?,
                "Episode" => insert_episode(&tx, cache, client, item, cast_limit)?,
                "BoxSet" => insert_boxset(&tx, cache, client, item)?,
                _ => {}
            }
        }
    }
    tx.commit()?;
    Ok(())
}

fn insert_movie(
    tx: &rusqlite::Transaction<'_>,
    cache: &mut FkCache,
    client: &JellyfinClient,
    item: &JellyfinItem,
    cast_limit: usize,
) -> Result<()> {
    let file_id = cache.next_file;
    cache.next_file += 1;
    let movie_id = cache.next_movie;
    cache.next_movie += 1;
    let filename = format!("plugin://plugin.video.jellykodi/?mode=play&id={}", item.id);
    let (play_count, last_played, pos) = userdata(item);
    let duration = item.runtime_ticks.unwrap_or(0) / 10_000_000;
    tx.execute(
        "INSERT INTO files (idFile, idPath, strFilename, playCount, lastPlayed, dateAdded) VALUES (?1,?2,?3,?4,?5,?6)",
        params![
            file_id,
            cache.movie_path_id,
            filename,
            play_count,
            last_played,
            item.premiered.as_deref().unwrap_or("")
        ],
    )?;
    let genres = item
        .genres
        .as_ref()
        .map(|g| g.join(" / "))
        .unwrap_or_default();
    let studios = item
        .studios
        .as_ref()
        .map(|s| s.iter().map(|x| x.name.as_str()).collect::<Vec<_>>().join(" / "))
        .unwrap_or_default();
    tx.execute(
        "INSERT INTO movie (idMovie, idFile, c00, c01, c07, c11, c12, c14, c18, premiered) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        params![
            movie_id,
            file_id,
            item.name,
            item.overview.as_deref().unwrap_or(""),
            item.year.unwrap_or(0).to_string(),
            duration.to_string(),
            item.mpaa.as_deref().unwrap_or(""),
            genres,
            studios,
            item.premiered.as_deref().unwrap_or("")
        ],
    )?;
    insert_art(tx, movie_id, "movie", client, &item.id)?;
    if pos > 0 && duration > 0 {
        tx.execute(
            "INSERT INTO bookmark (idFile, timeInSeconds, totalTimeInSeconds, player, type) VALUES (?1,?2,?3,'VideoPlayer',1)",
            params![file_id, pos as f64, duration as f64],
        )?;
    }
    insert_cast(tx, cache, item, movie_id, "movie", cast_limit)?;
    Ok(())
}

fn insert_series(
    tx: &rusqlite::Transaction<'_>,
    cache: &mut FkCache,
    client: &JellyfinClient,
    item: &JellyfinItem,
) -> Result<()> {
    if cache.shows.contains_key(&item.id) {
        return Ok(());
    }
    let show_id = cache.next_show;
    cache.next_show += 1;
    cache.shows.insert(item.id.clone(), show_id);
    let genres = item
        .genres
        .as_ref()
        .map(|g| g.join(" / "))
        .unwrap_or_default();
    tx.execute(
        "INSERT INTO tvshow (idShow, c00, c01, c05, c08, c14) VALUES (?1,?2,?3,?4,?5,?6)",
        params![
            show_id,
            item.name,
            item.overview.as_deref().unwrap_or(""),
            item.premiered.as_deref().unwrap_or(""),
            genres,
            item.mpaa.as_deref().unwrap_or("")
        ],
    )?;
    insert_art(tx, show_id, "tvshow", client, &item.id)?;
    Ok(())
}

fn insert_season(
    tx: &rusqlite::Transaction<'_>,
    cache: &mut FkCache,
    client: &JellyfinClient,
    item: &JellyfinItem,
) -> Result<()> {
    let Some(series_id) = &item.series_id else {
        return Ok(());
    };
    let show_id = match cache.shows.get(series_id) {
        Some(&id) => id,
        None => {
            // Placeholder show row if series page not yet seen.
            let id = cache.next_show;
            cache.next_show += 1;
            cache.shows.insert(series_id.clone(), id);
            tx.execute(
                "INSERT INTO tvshow (idShow, c00, c01, c05, c08, c14) VALUES (?1,?2,'','','','')",
                params![id, item.series_name.as_deref().unwrap_or("Unknown")],
            )?;
            id
        }
    };
    let season_id = cache.next_season;
    cache.next_season += 1;
    cache.seasons.insert(item.id.clone(), season_id);
    let num = item.parent_index.unwrap_or(0);
    tx.execute(
        "INSERT INTO seasons (idSeason, idShow, season, name) VALUES (?1,?2,?3,?4)",
        params![season_id, show_id, num, item.name],
    )?;
    insert_art(tx, season_id, "season", client, &item.id)?;
    Ok(())
}

fn insert_episode(
    tx: &rusqlite::Transaction<'_>,
    cache: &mut FkCache,
    client: &JellyfinClient,
    item: &JellyfinItem,
    cast_limit: usize,
) -> Result<()> {
    let file_id = cache.next_file;
    cache.next_file += 1;
    let ep_id = cache.next_episode;
    cache.next_episode += 1;
    let filename = format!("plugin://plugin.video.jellykodi/?mode=play&id={}", item.id);
    let (play_count, last_played, pos) = userdata(item);
    let duration = item.runtime_ticks.unwrap_or(0) / 10_000_000;
    tx.execute(
        "INSERT INTO files (idFile, idPath, strFilename, playCount, lastPlayed, dateAdded) VALUES (?1,?2,?3,?4,?5,?6)",
        params![
            file_id,
            cache.tv_path_id,
            filename,
            play_count,
            last_played,
            item.premiered.as_deref().unwrap_or("")
        ],
    )?;
    let show_id = item
        .series_id
        .as_ref()
        .and_then(|s| cache.shows.get(s).copied())
        .unwrap_or(0);
    let season_id = item
        .season_id
        .as_ref()
        .and_then(|s| cache.seasons.get(s).copied())
        .unwrap_or(0);
    tx.execute(
        "INSERT INTO episode (idEpisode, idFile, c00, c01, c12, c13, idShow, idSeason) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
        params![
            ep_id,
            file_id,
            item.name,
            item.overview.as_deref().unwrap_or(""),
            item.parent_index.unwrap_or(0).to_string(),
            item.index_number.unwrap_or(0).to_string(),
            show_id,
            season_id
        ],
    )?;
    insert_art(tx, ep_id, "episode", client, &item.id)?;
    if pos > 0 && duration > 0 {
        tx.execute(
            "INSERT INTO bookmark (idFile, timeInSeconds, totalTimeInSeconds, player, type) VALUES (?1,?2,?3,'VideoPlayer',1)",
            params![file_id, pos as f64, duration as f64],
        )?;
    }
    insert_cast(tx, cache, item, ep_id, "episode", cast_limit)?;
    Ok(())
}

fn insert_boxset(
    tx: &rusqlite::Transaction<'_>,
    cache: &mut FkCache,
    client: &JellyfinClient,
    item: &JellyfinItem,
) -> Result<()> {
    let set_id = cache.next_set;
    cache.next_set += 1;
    tx.execute(
        "INSERT INTO sets (idSet, strSet, strOverview) VALUES (?1,?2,?3)",
        params![
            set_id,
            item.name,
            item.overview.as_deref().unwrap_or("")
        ],
    )?;
    insert_art(tx, set_id, "set", client, &item.id)?;
    Ok(())
}

fn insert_art(
    tx: &rusqlite::Transaction<'_>,
    media_id: i64,
    media_type: &str,
    client: &JellyfinClient,
    item_id: &str,
) -> Result<()> {
    tx.execute(
        "INSERT INTO art (media_id, media_type, type, url) VALUES (?1,?2,'poster',?3)",
        params![media_id, media_type, client.image_url(item_id, "Primary", 720)],
    )?;
    tx.execute(
        "INSERT INTO art (media_id, media_type, type, url) VALUES (?1,?2,'fanart',?3)",
        params![media_id, media_type, client.image_url(item_id, "Backdrop", 1920)],
    )?;
    Ok(())
}

fn insert_cast(
    tx: &rusqlite::Transaction<'_>,
    cache: &mut FkCache,
    item: &JellyfinItem,
    media_id: i64,
    media_type: &str,
    cast_limit: usize,
) -> Result<()> {
    let Some(people) = &item.people else {
        return Ok(());
    };
    for (idx, person) in people
        .iter()
        .filter(|p| p.person_type.as_deref() == Some("Actor"))
        .take(cast_limit)
        .enumerate()
    {
        let actor_id = if let Some(&id) = cache.actors.get(&person.name) {
            id
        } else {
            let id = cache.next_actor;
            cache.next_actor += 1;
            tx.execute(
                "INSERT INTO actor (actor_id, name) VALUES (?1,?2)",
                params![id, &person.name],
            )?;
            cache.actors.insert(person.name.clone(), id);
            id
        };
        tx.execute(
            "INSERT INTO actor_link (actor_id, media_id, media_type, role, cast_order) VALUES (?1,?2,?3,?4,?5)",
            params![
                actor_id,
                media_id,
                media_type,
                person.role.as_deref().unwrap_or(""),
                idx as i64
            ],
        )?;
    }
    Ok(())
}

fn userdata(item: &JellyfinItem) -> (Option<i32>, Option<String>, i64) {
    match &item.user_data {
        Some(ud) => (
            if ud.played {
                ud.play_count.or(Some(1))
            } else {
                None
            },
            ud.last_played.clone(),
            ud.position_ticks.unwrap_or(0) / 10_000_000,
        ),
        None => (None, None, 0),
    }
}

pub fn swap_into_place(staging: &Path, target: &Path) -> Result<()> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Backup existing
    if target.exists() {
        let bak = target.with_extension("db.jellykodi.bak");
        let _ = std::fs::rename(target, &bak);
    }
    std::fs::rename(staging, target)?;
    // Clean WAL/SHM leftovers for old file names
    let _ = std::fs::remove_file(format!("{}-wal", target.display()));
    let _ = std::fs::remove_file(format!("{}-shm", target.display()));
    Ok(())
}

pub struct MapDb;

impl MapDb {
    pub fn count(path: &Path) -> Result<u64> {
        if !path.exists() {
            return Ok(0);
        }
        let conn = Connection::open(path)?;
        // Full sync currently stores the item count in meta — the per-id `map` table is
        // filled by later delta work. Without reading meta, every restart re-runs full sync.
        if let Ok(v) = conn.query_row(
            "SELECT v FROM meta WHERE k = 'last_count'",
            [],
            |r| r.get::<_, String>(0),
        ) && let Ok(n) = v.parse::<u64>()
        {
            return Ok(n);
        }
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM map", [], |r| r.get(0))
            .unwrap_or(0);
        Ok(n as u64)
    }

    pub fn rebuild_from_pass(path: &Path, count: u64) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS map (jellyfin_id TEXT PRIMARY KEY, kind TEXT);
             CREATE TABLE IF NOT EXISTS meta (k TEXT PRIMARY KEY, v TEXT);",
        )?;
        conn.execute(
            "INSERT OR REPLACE INTO meta (k,v) VALUES ('last_count', ?1)",
            params![count.to_string()],
        )?;
        conn.execute(
            "INSERT OR REPLACE INTO meta (k,v) VALUES ('last_full_sync', ?1)",
            params![chrono::Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }
}
