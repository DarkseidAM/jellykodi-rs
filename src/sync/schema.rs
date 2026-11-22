use crate::error::Result;
use crate::jellyfin::types::JellyfinItem;
use crate::jellyfin::JellyfinClient;
use rusqlite::{params, Connection};
use rustc_hash::FxHashMap;
use std::path::Path;

/// Real CoreELEC / Kodi 22 `MyVideos149` schema (tables + indexes + version row).
/// Slimmer hand-rolled schemas lack `version` and cause Kodi to error on open.
const SCHEMA: &str = include_str!("myvideos149.sql");

/// Kodi system "Standard" video version type (MyVideos ≥121 / Omega+).
const VIDEO_VERSION_ID_DEFAULT: i64 = 40400;
/// MyVideos150 (Piers): VideoAssetType::VERSION == 1 (was 0 on Omega).
const VIDEO_ASSET_TYPE_VERSION: i64 = 1;

#[derive(Debug, Clone)]
pub struct MapEntry {
    pub jellyfin_id: String,
    pub kind: String,
    pub kodi_id: i64,
    pub file_id: i64,
}

pub struct FkCache {
    actors: FxHashMap<String, i64>,
    genres: FxHashMap<String, i64>,
    studios: FxHashMap<String, i64>,
    tags: FxHashMap<String, i64>,
    countries: FxHashMap<String, i64>,
    shows: FxHashMap<String, i64>, // jellyfin series id -> idShow
    shows_by_title: FxHashMap<String, i64>, // lowercased series title -> idShow (merges alternate series IDs)
    seasons: FxHashMap<String, i64>,
    seasons_by_show_and_num: FxHashMap<(i64, i32), i64>, // (idShow, season_num) -> idSeason
    movies: FxHashMap<String, i64>,
    boxsets: FxHashMap<String, i64>,
    map_rows: Vec<MapEntry>,
    pending_versions: Vec<JellyfinItem>,
    next_actor: i64,
    next_genre: i64,
    next_studio: i64,
    next_tag: i64,
    next_country: i64,
    next_file: i64,
    next_movie: i64,
    next_show: i64,
    next_season: i64,
    next_episode: i64,
    next_set: i64,
    next_rating: i64,
    next_uniqueid: i64,
    movie_path_id: i64,
    tv_path_id: i64,
}

pub fn open_fresh_db(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)?;
    // Staging-only: maximize eMMC write throughput. Restored in finalize_staging_db
    // before Kodi opens the swapped DB.
    conn.execute_batch(
        "PRAGMA journal_mode = OFF;
         PRAGMA synchronous = OFF;
         PRAGMA cache_size = -65536;
         PRAGMA temp_store = MEMORY;
         PRAGMA mmap_size = 268435456;",
    )?;
    conn.execute_batch(SCHEMA)?;
    Ok(conn)
}

/// Return staging DB to Kodi-safe journal/sync settings before swap.
pub fn finalize_staging_db(conn: &mut Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_movie_c00 ON movie (c00);
         CREATE INDEX IF NOT EXISTS idx_tvshow_c00 ON tvshow (c00);
         CREATE INDEX IF NOT EXISTS idx_episode_c00 ON episode (c00);
         PRAGMA journal_mode = DELETE;
         PRAGMA synchronous = NORMAL;
         PRAGMA optimize;",
    )?;
    Ok(())
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
        // Required for movie_view INNER JOIN on MyVideos150 — without this, movies are invisible.
        conn.execute(
            "INSERT OR IGNORE INTO videoversiontype (id, name, owner, itemType) VALUES (?1, 'Standard', 0, ?2)",
            params![VIDEO_VERSION_ID_DEFAULT, VIDEO_ASSET_TYPE_VERSION],
        )?;
        Ok(Self {
            actors: FxHashMap::default(),
            genres: FxHashMap::default(),
            studios: FxHashMap::default(),
            tags: FxHashMap::default(),
            countries: FxHashMap::default(),
            shows: FxHashMap::default(),
            shows_by_title: FxHashMap::default(),
            seasons: FxHashMap::default(),
            seasons_by_show_and_num: FxHashMap::default(),
            movies: FxHashMap::default(),
            boxsets: FxHashMap::default(),
            map_rows: Vec::new(),
            pending_versions: Vec::new(),
            next_actor: 1,
            next_genre: 1,
            next_studio: 1,
            next_tag: 1,
            next_country: 1,
            next_file: 1,
            next_movie: 1,
            next_show: 1,
            next_season: 1,
            next_episode: 1,
            next_set: 1,
            next_rating: 1,
            next_uniqueid: 1,
            movie_path_id: 1,
            tv_path_id: 2,
        })
    }

    pub fn movie_id(&self, jellyfin_id: &str) -> Option<i64> {
        self.movies.get(jellyfin_id).copied()
    }

    pub fn boxset_pairs(&self) -> Vec<(String, i64)> {
        self.boxsets.iter().map(|(k, v)| (k.clone(), *v)).collect()
    }

    pub fn take_map(&mut self) -> Vec<MapEntry> {
        std::mem::take(&mut self.map_rows)
    }

    /// Continue numbering and name tables on a database Kodi already has open.
    pub fn resume(conn: &Connection) -> Result<Self> {
        let mut cache = Self::init(conn)?;
        cache.next_file = max_plus(conn, "SELECT COALESCE(MAX(idFile),0) FROM files")?;
        cache.next_movie = max_plus(conn, "SELECT COALESCE(MAX(idMovie),0) FROM movie")?;
        cache.next_show = max_plus(conn, "SELECT COALESCE(MAX(idShow),0) FROM tvshow")?;
        cache.next_season = max_plus(conn, "SELECT COALESCE(MAX(idSeason),0) FROM seasons")?;
        cache.next_episode = max_plus(conn, "SELECT COALESCE(MAX(idEpisode),0) FROM episode")?;
        cache.next_set = max_plus(conn, "SELECT COALESCE(MAX(idSet),0) FROM sets")?;
        cache.next_rating = max_plus(conn, "SELECT COALESCE(MAX(rating_id),0) FROM rating")?;
        cache.next_uniqueid = max_plus(conn, "SELECT COALESCE(MAX(uniqueid_id),0) FROM uniqueid")?;
        load_name_map(conn, "SELECT genre_id, name FROM genre", &mut cache.genres, &mut cache.next_genre)?;
        load_name_map(conn, "SELECT studio_id, name FROM studio", &mut cache.studios, &mut cache.next_studio)?;
        load_name_map(conn, "SELECT tag_id, name FROM tag", &mut cache.tags, &mut cache.next_tag)?;
        load_name_map(conn, "SELECT country_id, name FROM country", &mut cache.countries, &mut cache.next_country)?;
        load_name_map(conn, "SELECT actor_id, name FROM actor", &mut cache.actors, &mut cache.next_actor)?;
        Ok(cache)
    }
}

fn max_plus(conn: &Connection, sql: &str) -> Result<i64> {
    let n: i64 = conn.query_row(sql, [], |r| r.get(0))?;
    Ok(n + 1)
}

fn load_name_map(
    conn: &Connection,
    sql: &str,
    map: &mut FxHashMap<String, i64>,
    next: &mut i64,
) -> Result<()> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
    let mut max_id = 0i64;
    for row in rows {
        let (id, name) = row?;
        max_id = max_id.max(id);
        map.insert(name, id);
    }
    *next = max_id + 1;
    Ok(())
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
                "Series" => insert_series(&tx, cache, client, item, cast_limit)?,
                "Season" => insert_season(&tx, cache, client, item)?,
                "Episode" => insert_episode(&tx, cache, client, item, cast_limit)?,
                "BoxSet" => insert_boxset(&tx, cache, client, item)?,
                _ => {}
            }
        }
        let pending = std::mem::take(&mut cache.pending_versions);
        for item in pending {
            let primary = item.primary_version_id.clone().unwrap_or_default();
            if let Some(movie_id) = cache.movies.get(&primary).copied() {
                insert_alternate_file(&tx, cache, client, &item, movie_id)?;
            } else {
                insert_movie(&tx, cache, client, &item, cast_limit)?;
            }
        }
    }
    tx.commit()?;
    Ok(())
}

fn play_url(item_id: &str, source_id: Option<&str>) -> String {
    match source_id {
        Some(source) if !source.is_empty() => {
            format!("plugin://plugin.video.jellykodi/?mode=play&id={item_id}&source={source}")
        }
        _ => format!("plugin://plugin.video.jellykodi/?mode=play&id={item_id}"),
    }
}

fn version_type_id_for_label(label: &str) -> i64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in label.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    100_000 + (hash % 800_000_000) as i64
}

fn ensure_version_type(
    tx: &rusqlite::Transaction<'_>,
    source: &crate::jellyfin::types::MediaSource,
) -> Result<i64> {
    let label = source.version_label();
    let id = version_type_id_for_label(&label);
    tx.execute(
        "INSERT OR IGNORE INTO videoversiontype (id, name, owner, itemType) VALUES (?1, ?2, 0, ?3)",
        params![id, label, VIDEO_ASSET_TYPE_VERSION],
    )?;
    Ok(id)
}

fn ensure_custom_version_type(
    tx: &rusqlite::Transaction<'_>,
    label: &str,
) -> Result<i64> {
    let id = version_type_id_for_label(label);
    tx.execute(
        "INSERT OR IGNORE INTO videoversiontype (id, name, owner, itemType) VALUES (?1, ?2, 0, ?3)",
        params![id, label, VIDEO_ASSET_TYPE_VERSION],
    )?;
    Ok(id)
}

fn insert_movie(
    tx: &rusqlite::Transaction<'_>,
    cache: &mut FkCache,
    client: &JellyfinClient,
    item: &JellyfinItem,
    cast_limit: usize,
) -> Result<()> {
    if let Some(pid) = &item.primary_version_id
        && pid != &item.id
    {
        if let Some(movie_id) = cache.movies.get(pid).copied() {
            insert_alternate_file(tx, cache, client, item, movie_id)?;
            return Ok(());
        }
        cache.pending_versions.push(item.clone());
        return Ok(());
    }
    let file_id = cache.next_file;
    cache.next_file += 1;
    let movie_id = cache.next_movie;
    cache.next_movie += 1;
    let sources = item.media_sources.clone().unwrap_or_default();
    let default_source = sources.iter().max_by_key(|s| (s.source_height(), s.bitrate.unwrap_or(0)));
    let filename = play_url(&item.id, default_source.filter(|_| sources.len() > 1).map(|s| s.id.as_str()));
    let (play_count, last_played, pos) = userdata(item);
    let duration = item.runtime_ticks.unwrap_or(0) / 10_000_000;
    {
        let mut stmt = tx.prepare_cached(
            "INSERT INTO files (idFile, idPath, strFilename, playCount, lastPlayed, dateAdded) VALUES (?1,?2,?3,?4,?5,?6)",
        )?;
        stmt.execute(params![
            file_id,
            cache.movie_path_id,
            filename,
            play_count,
            last_played,
            item.premiered.as_deref().unwrap_or("")
        ])?;
    }
    let genres = join_list(item.genres.as_deref());
    let studios = join_named(item.studios.as_deref());
    let writers = people_named(item, "Writer");
    let directors = people_named(item, "Director");
    let countries = join_list(item.production_locations.as_deref());
    let trailer = trailer_url(item);
    let sort = item.sort_name.clone().unwrap_or_else(|| item.name.clone());
    let short_plot: String = item
        .overview
        .as_deref()
        .unwrap_or("")
        .chars()
        .take(255)
        .collect();
    let rating_id = insert_rating(tx, cache, movie_id, "movie", item)?;
    let uniqueid_id = insert_uniqueid(tx, cache, movie_id, "movie", item)?;
    {
        let mut stmt = tx.prepare_cached(
            "INSERT INTO movie (idMovie, idFile, c00, c01, c02, c03, c04, c05, c06, c07, c09, c10, c11, c12, c14, c15, c16, c18, c19, c21, premiered) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21)",
        )?;
        stmt.execute(params![
            movie_id,
            file_id,
            item.name,
            item.overview.as_deref().unwrap_or(""),
            short_plot,
            item.tagline.as_deref().unwrap_or(""),
            item.vote_count.unwrap_or(0).to_string(),
            rating_id.map(|id| id.to_string()).unwrap_or_default(),
            writers,
            item.year.unwrap_or(0).to_string(),
            uniqueid_id.map(|id| id.to_string()).unwrap_or_default(),
            sort,
            duration.to_string(),
            item.mpaa.as_deref().unwrap_or(""),
            genres,
            directors,
            item.name,
            studios,
            trailer,
            countries,
            item.premiered.as_deref().unwrap_or("")
        ])?;
    }
    let default_type = if sources.len() > 1 {
        default_source
            .map(|s| ensure_version_type(tx, s))
            .transpose()?
            .unwrap_or(VIDEO_VERSION_ID_DEFAULT)
    } else {
        VIDEO_VERSION_ID_DEFAULT
    };
    {
        let mut stmt = tx.prepare_cached(
            "INSERT INTO videoversion (idFile, idMedia, media_type, itemType, idType) VALUES (?1,?2,'movie',?3,?4)",
        )?;
        stmt.execute(params![
            file_id,
            movie_id,
            VIDEO_ASSET_TYPE_VERSION,
            default_type
        ])?;
    }
    insert_art(tx, movie_id, "movie", client, item)?;
    let default_streams = default_source
        .and_then(|s| s.media_streams.clone())
        .or_else(|| item.media_streams.clone());
    if let Some(ref streams) = default_streams {
        insert_streamdetails(tx, file_id, streams, duration)?;
    }
    if sources.len() > 1 {
        let default_id = default_source.map(|s| s.id.as_str()).unwrap_or("");
        for source in sources.iter().filter(|s| !s.id.eq_ignore_ascii_case(default_id)) {
            insert_source_file(tx, cache, &item.id, movie_id, source, duration)?;
        }
    }
    if pos > 0 && duration > 0 {
        let mut stmt = tx.prepare_cached(
            "INSERT INTO bookmark (idFile, timeInSeconds, totalTimeInSeconds, player, type) VALUES (?1,?2,?3,'VideoPlayer',1)",
        )?;
        stmt.execute(params![file_id, pos as f64, duration as f64])?;
    }
    insert_cast(tx, cache, item, movie_id, "movie", cast_limit)?;
    link_crew(tx, cache, item, movie_id, "movie")?;
    link_names(tx, cache, item.genres.as_deref().unwrap_or(&[]), "genre", "genre_id", "genre_link", movie_id, "movie")?;
    link_named_rows(tx, cache, item.studios.as_deref(), movie_id, "movie")?;
    link_names(tx, cache, item.tags.as_deref().unwrap_or(&[]), "tag", "tag_id", "tag_link", movie_id, "movie")?;
    link_names(tx, cache, item.production_locations.as_deref().unwrap_or(&[]), "country", "country_id", "country_link", movie_id, "movie")?;
    cache.movies.insert(item.id.clone(), movie_id);
    remember(cache, &item.id, "movie", movie_id, file_id);
    Ok(())
}

fn insert_series(
    tx: &rusqlite::Transaction<'_>,
    cache: &mut FkCache,
    client: &JellyfinClient,
    item: &JellyfinItem,
    cast_limit: usize,
) -> Result<()> {
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

    // Check if this show was already registered by ID or title (e.g. from an alternate library version or early season)
    let existing_show_id = cache
        .shows
        .get(&item.id)
        .copied()
        .or_else(|| cache.shows_by_title.get(&item.name.trim().to_lowercase()).copied());

    if let Some(show_id) = existing_show_id {
        cache.shows.insert(item.id.clone(), show_id);
        cache.shows_by_title.insert(item.name.trim().to_lowercase(), show_id);
        let rating_id = insert_rating(tx, cache, show_id, "tvshow", item)?;
        let uniqueid_id = insert_uniqueid(tx, cache, show_id, "tvshow", item)?;
        {
            let mut stmt = tx.prepare_cached(
                "UPDATE tvshow SET c00=?1, c01=?2, c04=?3, c05=?4, c08=?5, c12=?6, c13=?7, c14=?8 WHERE idShow=?9",
            )?;
            stmt.execute(params![
                item.name,
                item.overview.as_deref().unwrap_or(""),
                rating_id.map(|id| id.to_string()).unwrap_or_default(),
                item.premiered.as_deref().unwrap_or(""),
                genres,
                uniqueid_id.map(|id| id.to_string()).unwrap_or_default(),
                item.mpaa.as_deref().unwrap_or(""),
                studios,
                show_id
            ])?;
        }
        {
            let mut stmt = tx.prepare_cached(
                "INSERT OR IGNORE INTO tvshowlinkpath (idShow, idPath) VALUES (?1,?2)",
            )?;
            stmt.execute(params![show_id, cache.tv_path_id])?;
        }
        {
            let mut stmt =
                tx.prepare_cached("DELETE FROM art WHERE media_id=?1 AND media_type='tvshow'")?;
            stmt.execute(params![show_id])?;
        }
        insert_art(tx, show_id, "tvshow", client, item)?;
        clear_links(tx, show_id, "tvshow")?;
        link_show_meta(tx, cache, item, show_id, cast_limit)?;
        remember(cache, &item.id, "tvshow", show_id, 0);
        return Ok(());
    }

    let show_id = cache.next_show;
    cache.next_show += 1;
    cache.shows.insert(item.id.clone(), show_id);
    cache.shows_by_title.insert(item.name.trim().to_lowercase(), show_id);
    let rating_id = insert_rating(tx, cache, show_id, "tvshow", item)?;
    let uniqueid_id = insert_uniqueid(tx, cache, show_id, "tvshow", item)?;
    {
        let mut stmt = tx.prepare_cached(
            "INSERT INTO tvshow (idShow, c00, c01, c02, c04, c05, c08, c09, c10, c12, c13, c14, c15, c16) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
        )?;
        stmt.execute(params![
            show_id,
            item.name,
            item.overview.as_deref().unwrap_or(""),
            item.status.as_deref().unwrap_or(""),
            rating_id.map(|id| id.to_string()).unwrap_or_default(),
            item.premiered.as_deref().unwrap_or(""),
            genres,
            item.name,
            "disintegrate browse bug",
            uniqueid_id.map(|id| id.to_string()).unwrap_or_default(),
            item.mpaa.as_deref().unwrap_or(""),
            studios,
            item.sort_name.as_deref().unwrap_or(&item.name),
            trailer_url(item),
        ])?;
    }
    {
        let mut stmt = tx.prepare_cached(
            "INSERT OR IGNORE INTO tvshowlinkpath (idShow, idPath) VALUES (?1,?2)",
        )?;
        stmt.execute(params![show_id, cache.tv_path_id])?;
    }
    insert_art(tx, show_id, "tvshow", client, item)?;
    link_show_meta(tx, cache, item, show_id, cast_limit)?;
    remember(cache, &item.id, "tvshow", show_id, 0);
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
    let show_id = match cache.shows.get(series_id).copied() {
        Some(id) => id,
        None => {
            // Try matching by series_name before minting a duplicate show row
            let matched = item.series_name.as_ref().and_then(|name| {
                cache.shows_by_title.get(&name.trim().to_lowercase()).copied()
            });
            if let Some(existing_id) = matched {
                cache.shows.insert(series_id.clone(), existing_id);
                existing_id
            } else {
                // Placeholder show row if series page not yet seen.
                let id = cache.next_show;
                cache.next_show += 1;
                cache.shows.insert(series_id.clone(), id);
                if let Some(name) = &item.series_name {
                    cache.shows_by_title.insert(name.trim().to_lowercase(), id);
                }
                {
                    let mut stmt = tx.prepare_cached(
                        "INSERT INTO tvshow (idShow, c00, c01, c05, c08, c14) VALUES (?1,?2,'','','','')",
                    )?;
                    stmt.execute(params![id, item.series_name.as_deref().unwrap_or("Unknown")])?;
                }
                {
                    let mut stmt = tx.prepare_cached(
                        "INSERT OR IGNORE INTO tvshowlinkpath (idShow, idPath) VALUES (?1,?2)",
                    )?;
                    stmt.execute(params![id, cache.tv_path_id])?;
                }
                id
            }
        }
    };

    // Jellyfin Season: IndexNumber is the season number; ParentIndexNumber is usually None.
    let num = item.index_number.or(item.parent_index).unwrap_or(0);

    // If season already exists for this show (e.g. from an alternate library version), reuse it
    if let Some(&existing_season_id) = cache.seasons_by_show_and_num.get(&(show_id, num)) {
        cache.seasons.insert(item.id.clone(), existing_season_id);
        return Ok(());
    }

    let season_id = cache.next_season;
    cache.next_season += 1;
    cache.seasons.insert(item.id.clone(), season_id);
    cache.seasons_by_show_and_num.insert((show_id, num), season_id);
    {
        let mut stmt = tx.prepare_cached(
            "INSERT INTO seasons (idSeason, idShow, season, name) VALUES (?1,?2,?3,?4)",
        )?;
        stmt.execute(params![season_id, show_id, num, item.name])?;
    }
    insert_art(tx, season_id, "season", client, item)?;
    remember(cache, &item.id, "season", season_id, 0);
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
    {
        let mut stmt = tx.prepare_cached(
            "INSERT INTO files (idFile, idPath, strFilename, playCount, lastPlayed, dateAdded) VALUES (?1,?2,?3,?4,?5,?6)",
        )?;
        stmt.execute(params![
            file_id,
            cache.tv_path_id,
            filename,
            play_count,
            last_played,
            item.premiered.as_deref().unwrap_or("")
        ])?;
    }
    let show_id = item
        .series_id
        .as_ref()
        .and_then(|s| {
            cache.shows.get(s).copied().or_else(|| {
                item.series_name.as_ref().and_then(|name| {
                    cache.shows_by_title.get(&name.trim().to_lowercase()).copied()
                })
            })
        })
        .unwrap_or(0);
    let season_id = item
        .season_id
        .as_ref()
        .and_then(|s| cache.seasons.get(s).copied())
        .or_else(|| {
            let snum = item.parent_index.unwrap_or(0);
            cache.seasons_by_show_and_num.get(&(show_id, snum)).copied()
        })
        .unwrap_or(0);
    let duration_str = if duration > 0 { duration.to_string() } else { String::new() };
    let premiered = item.premiered.as_deref().unwrap_or("");
    let rating_id = insert_rating(tx, cache, ep_id, "episode", item)?;
    let uniqueid_id = insert_uniqueid(tx, cache, ep_id, "episode", item)?;
    {
        let mut stmt = tx.prepare_cached(
            "INSERT INTO episode (idEpisode, idFile, c00, c01, c03, c04, c05, c09, c10, c12, c13, c20, idShow, idSeason) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
        )?;
        stmt.execute(params![
            ep_id,
            file_id,
            item.name,
            item.overview.as_deref().unwrap_or(""),
            rating_id.map(|id| id.to_string()).unwrap_or_default(),
            people_named(item, "Writer"),
            premiered,
            duration_str,
            people_named(item, "Director"),
            item.parent_index.unwrap_or(0).to_string(),
            item.index_number.unwrap_or(0).to_string(),
            uniqueid_id.map(|id| id.to_string()).unwrap_or_default(),
            show_id,
            season_id
        ])?;
    }
    insert_art(tx, ep_id, "episode", client, item)?;
    if let Some(ref streams) = item.media_streams {
        insert_streamdetails(tx, file_id, streams, duration)?;
    }
    if pos > 0 && duration > 0 {
        let mut stmt = tx.prepare_cached(
            "INSERT INTO bookmark (idFile, timeInSeconds, totalTimeInSeconds, player, type) VALUES (?1,?2,?3,'VideoPlayer',1)",
        )?;
        stmt.execute(params![file_id, pos as f64, duration as f64])?;
    }
    if cast_limit > 0 {
        insert_cast(tx, cache, item, ep_id, "episode", cast_limit)?;
    }
    link_crew(tx, cache, item, ep_id, "episode")?;
    remember(cache, &item.id, "episode", ep_id, file_id);
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
    {
        let mut stmt =
            tx.prepare_cached("INSERT INTO sets (idSet, strSet, strOverview) VALUES (?1,?2,?3)")?;
        stmt.execute(params![
            set_id,
            item.name,
            item.overview.as_deref().unwrap_or("")
        ])?;
    }
    insert_art(tx, set_id, "set", client, item)?;
    cache.boxsets.insert(item.id.clone(), set_id);
    remember(cache, &item.id, "set", set_id, 0);
    Ok(())
}

fn insert_rating(
    tx: &rusqlite::Transaction<'_>,
    cache: &mut FkCache,
    media_id: i64,
    media_type: &str,
    item: &JellyfinItem,
) -> Result<Option<i64>> {
    let Some(rating) = item.community_rating else {
        return Ok(None);
    };
    let id = cache.next_rating;
    cache.next_rating += 1;
    let mut stmt = tx.prepare_cached(
        "INSERT INTO rating (rating_id, media_id, media_type, rating_type, rating, votes) VALUES (?1,?2,?3,'default',?4,?5)",
    )?;
    stmt.execute(params![
        id,
        media_id,
        media_type,
        rating,
        item.vote_count.unwrap_or(0)
    ])?;
    Ok(Some(id))
}

fn insert_uniqueid(
    tx: &rusqlite::Transaction<'_>,
    cache: &mut FkCache,
    media_id: i64,
    media_type: &str,
    item: &JellyfinItem,
) -> Result<Option<i64>> {
    let Some(providers) = item.provider_ids.as_ref().and_then(|v| v.as_object()) else {
        return Ok(None);
    };
    let (ptype, value) = if let Some(v) = providers.get("Imdb").and_then(|x| x.as_str()) {
        ("imdb", v)
    } else if let Some(v) = providers.get("Tmdb").and_then(|x| x.as_str()) {
        ("tmdb", v)
    } else if let Some(v) = providers.get("Tvdb").and_then(|x| x.as_str()) {
        ("tvdb", v)
    } else {
        return Ok(None);
    };
    let id = cache.next_uniqueid;
    cache.next_uniqueid += 1;
    let mut stmt = tx.prepare_cached(
        "INSERT INTO uniqueid (uniqueid_id, media_id, media_type, value, type) VALUES (?1,?2,?3,?4,?5)",
    )?;
    stmt.execute(params![id, media_id, media_type, value, ptype])?;
    Ok(Some(id))
}

fn insert_art(
    tx: &rusqlite::Transaction<'_>,
    media_id: i64,
    media_type: &str,
    client: &JellyfinClient,
    item: &JellyfinItem,
) -> Result<()> {
    let poster = client.image_url(&item.id, "Primary", 720);
    let fanart = client.image_url(&item.id, "Backdrop", 1920);
    write_art(tx, media_id, media_type, "poster", &poster)?;
    write_art(tx, media_id, media_type, "thumb", &poster)?;
    write_art(tx, media_id, media_type, "fanart", &fanart)?;
    for (tag, kind) in [
        ("Logo", "clearlogo"),
        ("Banner", "banner"),
        ("Disc", "discart"),
        ("Thumb", "landscape"),
        ("Art", "clearart"),
    ] {
        if has_image(item, tag) {
            let url = client.image_url(&item.id, tag, 800);
            write_art(tx, media_id, media_type, kind, &url)?;
        }
    }
    Ok(())
}

fn write_art(
    tx: &rusqlite::Transaction<'_>,
    media_id: i64,
    media_type: &str,
    kind: &str,
    url: &str,
) -> Result<()> {
    let mut stmt = tx.prepare_cached(
        "INSERT INTO art (media_id, media_type, type, url) VALUES (?1,?2,?3,?4)",
    )?;
    stmt.execute(params![media_id, media_type, kind, url])?;
    Ok(())
}

fn has_image(item: &JellyfinItem, key: &str) -> bool {
    item.image_tags
        .as_ref()
        .and_then(|m| m.get(key))
        .map(|v| !v.is_null())
        .unwrap_or(false)
}

fn join_list(values: Option<&[String]>) -> String {
    values
        .map(|v| v.join(" / "))
        .unwrap_or_default()
}

fn join_named(values: Option<&[crate::jellyfin::types::Named]>) -> String {
    values
        .map(|v| {
            v.iter()
                .map(|x| x.name.as_str())
                .collect::<Vec<_>>()
                .join(" / ")
        })
        .unwrap_or_default()
}

fn people_named(item: &JellyfinItem, kind: &str) -> String {
    item.people
        .as_ref()
        .map(|people| {
            people
                .iter()
                .filter(|p| p.person_type.as_deref() == Some(kind))
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>()
                .join(" / ")
        })
        .unwrap_or_default()
}

fn trailer_url(item: &JellyfinItem) -> String {
    item.remote_trailers
        .as_ref()
        .and_then(|t| t.first())
        .and_then(|t| t.url.clone())
        .unwrap_or_default()
}

fn remember(cache: &mut FkCache, jellyfin_id: &str, kind: &str, kodi_id: i64, file_id: i64) {
    cache.map_rows.push(MapEntry {
        jellyfin_id: jellyfin_id.to_string(),
        kind: kind.to_string(),
        kodi_id,
        file_id,
    });
}

fn actor_id(tx: &rusqlite::Transaction<'_>, cache: &mut FkCache, name: &str) -> Result<i64> {
    if let Some(&id) = cache.actors.get(name) {
        return Ok(id);
    }
    let id = cache.next_actor;
    cache.next_actor += 1;
    let changed = tx.execute(
        "INSERT OR IGNORE INTO actor (actor_id, name) VALUES (?1,?2)",
        params![id, name],
    )?;
    let id = if changed == 0 {
        tx.query_row("SELECT actor_id FROM actor WHERE name=?1", params![name], |r| r.get(0))?
    } else {
        id
    };
    cache.actors.insert(name.to_string(), id);
    Ok(id)
}

fn link_crew(
    tx: &rusqlite::Transaction<'_>,
    cache: &mut FkCache,
    item: &JellyfinItem,
    media_id: i64,
    media_type: &str,
) -> Result<()> {
    let Some(people) = &item.people else {
        return Ok(());
    };
    for person in people {
        let table = match person.person_type.as_deref() {
            Some("Director") => "director_link",
            Some("Writer") => "writer_link",
            _ => continue,
        };
        let id = actor_id(tx, cache, &person.name)?;
        let sql = format!(
            "INSERT OR IGNORE INTO {table} (actor_id, media_id, media_type) VALUES (?1,?2,?3)"
        );
        let mut stmt = tx.prepare_cached(&sql)?;
        stmt.execute(params![id, media_id, media_type])?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn link_names(
    tx: &rusqlite::Transaction<'_>,
    cache: &mut FkCache,
    names: &[String],
    table: &str,
    id_col: &str,
    link_table: &str,
    media_id: i64,
    media_type: &str,
) -> Result<()> {
    for raw in names {
        let name = raw.trim();
        if name.is_empty() {
            continue;
        }
        let id = named_id(tx, cache, table, id_col, name)?;
        let sql = format!(
            "INSERT OR IGNORE INTO {link_table} ({id_col}, media_id, media_type) VALUES (?1,?2,?3)"
        );
        let mut stmt = tx.prepare_cached(&sql)?;
        stmt.execute(params![id, media_id, media_type])?;
    }
    Ok(())
}

fn named_id(
    tx: &rusqlite::Transaction<'_>,
    cache: &mut FkCache,
    table: &str,
    id_col: &str,
    name: &str,
) -> Result<i64> {
    let existing = match table {
        "genre" => cache.genres.get(name).copied(),
        "tag" => cache.tags.get(name).copied(),
        "country" => cache.countries.get(name).copied(),
        _ => None,
    };
    if let Some(id) = existing {
        return Ok(id);
    }
    let id = match table {
        "genre" => {
            let id = cache.next_genre;
            cache.next_genre += 1;
            id
        }
        "tag" => {
            let id = cache.next_tag;
            cache.next_tag += 1;
            id
        }
        "country" => {
            let id = cache.next_country;
            cache.next_country += 1;
            id
        }
        _ => return Ok(0),
    };
    let sql = format!("INSERT OR IGNORE INTO {table} ({id_col}, name) VALUES (?1,?2)");
    let changed = tx.execute(&sql, params![id, name])?;
    let id = if changed == 0 {
        let lookup = format!("SELECT {id_col} FROM {table} WHERE name=?1");
        tx.query_row(&lookup, params![name], |r| r.get(0))?
    } else {
        id
    };
    match table {
        "genre" => {
            cache.genres.insert(name.to_string(), id);
        }
        "tag" => {
            cache.tags.insert(name.to_string(), id);
        }
        "country" => {
            cache.countries.insert(name.to_string(), id);
        }
        _ => {}
    }
    Ok(id)
}

fn link_named_rows(
    tx: &rusqlite::Transaction<'_>,
    cache: &mut FkCache,
    studios: Option<&[crate::jellyfin::types::Named]>,
    media_id: i64,
    media_type: &str,
) -> Result<()> {
    let Some(studios) = studios else {
        return Ok(());
    };
    for studio in studios {
        let name = studio.name.trim();
        if name.is_empty() {
            continue;
        }
        let id = if let Some(&id) = cache.studios.get(name) {
            id
        } else {
            let id = cache.next_studio;
            cache.next_studio += 1;
            let changed = tx.execute(
                "INSERT OR IGNORE INTO studio (studio_id, name) VALUES (?1,?2)",
                params![id, name],
            )?;
            let id = if changed == 0 {
                tx.query_row(
                    "SELECT studio_id FROM studio WHERE name=?1",
                    params![name],
                    |r| r.get(0),
                )?
            } else {
                id
            };
            cache.studios.insert(name.to_string(), id);
            id
        };
        let mut stmt = tx.prepare_cached(
            "INSERT OR IGNORE INTO studio_link (studio_id, media_id, media_type) VALUES (?1,?2,?3)",
        )?;
        stmt.execute(params![id, media_id, media_type])?;
    }
    Ok(())
}

fn clear_links(tx: &rusqlite::Transaction<'_>, media_id: i64, media_type: &str) -> Result<()> {
    for table in [
        "actor_link",
        "director_link",
        "writer_link",
        "genre_link",
        "studio_link",
        "tag_link",
        "country_link",
    ] {
        let sql = format!("DELETE FROM {table} WHERE media_id=?1 AND media_type=?2");
        tx.execute(&sql, params![media_id, media_type])?;
    }
    Ok(())
}

fn link_show_meta(
    tx: &rusqlite::Transaction<'_>,
    cache: &mut FkCache,
    item: &JellyfinItem,
    show_id: i64,
    cast_limit: usize,
) -> Result<()> {
    insert_cast(tx, cache, item, show_id, "tvshow", cast_limit)?;
    link_crew(tx, cache, item, show_id, "tvshow")?;
    link_names(tx, cache, item.genres.as_deref().unwrap_or(&[]), "genre", "genre_id", "genre_link", show_id, "tvshow")?;
    link_named_rows(tx, cache, item.studios.as_deref(), show_id, "tvshow")?;
    link_names(tx, cache, item.tags.as_deref().unwrap_or(&[]), "tag", "tag_id", "tag_link", show_id, "tvshow")?;
    Ok(())
}

fn insert_source_file(
    tx: &rusqlite::Transaction<'_>,
    cache: &mut FkCache,
    item_id: &str,
    movie_id: i64,
    source: &crate::jellyfin::types::MediaSource,
    duration: i64,
) -> Result<()> {
    let file_id = cache.next_file;
    cache.next_file += 1;
    let filename = play_url(item_id, Some(&source.id));
    let mut stmt = tx.prepare_cached(
        "INSERT INTO files (idFile, idPath, strFilename, playCount, lastPlayed, dateAdded) VALUES (?1,?2,?3,0,NULL,?4)",
    )?;
    stmt.execute(params![file_id, cache.movie_path_id, filename, ""])?;
    let type_id = ensure_version_type(tx, source)?;
    let mut stmt = tx.prepare_cached(
        "INSERT INTO videoversion (idFile, idMedia, media_type, itemType, idType) VALUES (?1,?2,'movie',?3,?4)",
    )?;
    stmt.execute(params![file_id, movie_id, VIDEO_ASSET_TYPE_VERSION, type_id])?;
    if let Some(streams) = source.media_streams.as_ref() {
        insert_streamdetails(tx, file_id, streams, duration)?;
    }
    Ok(())
}

fn insert_alternate_file(
    tx: &rusqlite::Transaction<'_>,
    cache: &mut FkCache,
    client: &JellyfinClient,
    item: &JellyfinItem,
    movie_id: i64,
) -> Result<()> {
    let file_id = cache.next_file;
    cache.next_file += 1;
    let filename = format!("plugin://plugin.video.jellykodi/?mode=play&id={}", item.id);
    let (play_count, last_played, pos) = userdata(item);
    let duration = item.runtime_ticks.unwrap_or(0) / 10_000_000;
    let mut stmt = tx.prepare_cached(
        "INSERT INTO files (idFile, idPath, strFilename, playCount, lastPlayed, dateAdded) VALUES (?1,?2,?3,?4,?5,?6)",
    )?;
    stmt.execute(params![
        file_id,
        cache.movie_path_id,
        filename,
        play_count,
        last_played,
        item.premiered.as_deref().unwrap_or("")
    ])?;
    let type_id = if let Some(sources) = &item.media_sources
        && let Some(first) = sources.first()
    {
        ensure_version_type(tx, first)?
    } else {
        ensure_custom_version_type(tx, &item.name)?
    };
    let mut stmt = tx.prepare_cached(
        "INSERT INTO videoversion (idFile, idMedia, media_type, itemType, idType) VALUES (?1,?2,'movie',?3,?4)",
    )?;
    stmt.execute(params![file_id, movie_id, VIDEO_ASSET_TYPE_VERSION, type_id])?;
    if let Some(ref streams) = item.media_streams {
        insert_streamdetails(tx, file_id, streams, duration)?;
    }
    if pos > 0 && duration > 0 {
        let mut stmt = tx.prepare_cached(
            "INSERT INTO bookmark (idFile, timeInSeconds, totalTimeInSeconds, player, type) VALUES (?1,?2,?3,'VideoPlayer',1)",
        )?;
        stmt.execute(params![file_id, pos as f64, duration as f64])?;
    }
    let _ = client;
    remember(cache, &item.id, "movieversion", movie_id, file_id);
    Ok(())
}

pub fn set_movie_collection(conn: &Connection, movie_id: i64, set_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE movie SET idSet=?1 WHERE idMovie=?2",
        params![set_id, movie_id],
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
            let changed = tx.execute(
                "INSERT OR IGNORE INTO actor (actor_id, name) VALUES (?1,?2)",
                params![id, &person.name],
            )?;
            let id = if changed == 0 {
                tx.query_row(
                    "SELECT actor_id FROM actor WHERE name=?1",
                    params![&person.name],
                    |r| r.get(0),
                )?
            } else {
                id
            };
            cache.actors.insert(person.name.clone(), id);
            id
        };
        let mut stmt = tx.prepare_cached(
            "INSERT OR IGNORE INTO actor_link (actor_id, media_id, media_type, role, cast_order) VALUES (?1,?2,?3,?4,?5)",
        )?;
        stmt.execute(params![
            actor_id,
            media_id,
            media_type,
            person.role.as_deref().unwrap_or(""),
            idx as i64
        ])?;
    }
    Ok(())
}

fn insert_streamdetails(
    tx: &rusqlite::Transaction<'_>,
    file_id: i64,
    streams: &[crate::jellyfin::types::MediaStream],
    duration: i64,
) -> Result<()> {
    for s in streams {
        match s.stream_type.as_str() {
            "Video" => {
                let codec = s.normalized_video_codec();
                let aspect = s.parsed_aspect_ratio();
                let width = s.width.unwrap_or(0);
                let height = s.height.unwrap_or(0);
                let hdr = s.hdr_type();
                let mut stmt = tx.prepare_cached(
                    "INSERT INTO streamdetails (idFile, iStreamType, strVideoCodec, fVideoAspect, iVideoWidth, iVideoHeight, iVideoDuration, strStereoMode, strHdrType) VALUES (?1, 0, ?2, ?3, ?4, ?5, ?6, '', ?7)",
                )?;
                stmt.execute(params![file_id, codec, aspect, width, height, duration, hdr])?;
            }
            "Audio" => {
                let codec = s.normalized_audio_codec();
                let channels = s.channels.unwrap_or(2);
                let lang = s.language.as_deref().unwrap_or("");
                let mut stmt = tx.prepare_cached(
                    "INSERT INTO streamdetails (idFile, iStreamType, strAudioCodec, iAudioChannels, strAudioLanguage) VALUES (?1, 1, ?2, ?3, ?4)",
                )?;
                stmt.execute(params![file_id, codec, channels, lang])?;
            }
            "Subtitle" => {
                let lang = s.language.as_deref().unwrap_or("");
                let mut stmt = tx.prepare_cached(
                    "INSERT INTO streamdetails (idFile, iStreamType, strSubtitleLanguage) VALUES (?1, 2, ?2)",
                )?;
                stmt.execute(params![file_id, lang])?;
            }
            _ => {}
        }
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

#[derive(Debug, Clone)]
pub struct MappedRow {
    pub kind: String,
    pub kodi_id: i64,
    pub file_id: i64,
}

#[derive(Debug, Clone)]
pub struct LocalHit {
    pub jellyfin_id: String,
    pub kind: String,
    pub title: String,
    pub year: i32,
    pub plot: String,
}

pub fn hydrate_parents(cache: &mut FkCache, map_path: &Path) -> Result<()> {
    if !map_path.exists() {
        return Ok(());
    }
    let conn = Connection::open(map_path)?;
    let mut stmt = conn.prepare(
        "SELECT jellyfin_id, kind, kodi_id FROM map WHERE kind IN ('movie','tvshow','season')",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, i64>(2)?,
        ))
    })?;
    for row in rows {
        let (id, kind, kodi_id) = row?;
        match kind.as_str() {
            "movie" => {
                cache.movies.insert(id, kodi_id);
            }
            "tvshow" => {
                cache.shows.insert(id, kodi_id);
            }
            "season" => {
                cache.seasons.insert(id, kodi_id);
            }
            _ => {}
        }
    }
    Ok(())
}

pub fn insert_live_items(
    videos: &Path,
    map_path: &Path,
    client: &JellyfinClient,
    items: &[JellyfinItem],
    cast_limit: usize,
) -> Result<()> {
    if items.is_empty() || !videos.exists() {
        return Ok(());
    }
    let mut conn = Connection::open(videos)?;
    conn.busy_timeout(std::time::Duration::from_secs(8))?;
    let mut cache = FkCache::resume(&conn)?;
    hydrate_parents(&mut cache, map_path)?;
    bulk_insert_items(&mut conn, &mut cache, client, items, cast_limit)?;
    MapDb::upsert_rows(map_path, &cache.take_map())?;
    Ok(())
}

pub fn delete_mapped(videos: &Path, map_path: &Path, jellyfin_id: &str) -> Result<()> {
    let Some(row) = MapDb::lookup(map_path, jellyfin_id)? else {
        return Ok(());
    };
    if videos.exists() {
        let conn = Connection::open(videos)?;
        conn.busy_timeout(std::time::Duration::from_secs(8))?;
        match row.kind.as_str() {
            "movie" => {
                let mut alt_files = Vec::new();
                if let Ok(mut stmt) = conn.prepare("SELECT idFile FROM videoversion WHERE idMedia=?1 AND media_type='movie'")
                    && let Ok(rows) = stmt.query_map(params![row.kodi_id], |r| r.get::<_, i64>(0))
                {
                    alt_files.extend(rows.filter_map(|r| r.ok()));
                }
                for fid in alt_files {
                    let _ = conn.execute("DELETE FROM files WHERE idFile=?1", params![fid]);
                    let _ = conn.execute("DELETE FROM videoversion WHERE idFile=?1", params![fid]);
                    let _ = conn.execute("DELETE FROM streamdetails WHERE idFile=?1", params![fid]);
                    let _ = conn.execute("DELETE FROM bookmark WHERE idFile=?1", params![fid]);
                }
                conn.execute("DELETE FROM movie WHERE idMovie=?1", params![row.kodi_id])?;
            }
            "episode" => {
                conn.execute("DELETE FROM episode WHERE idEpisode=?1", params![row.kodi_id])?;
            }
            "tvshow" => {
                conn.execute("DELETE FROM tvshow WHERE idShow=?1", params![row.kodi_id])?;
            }
            "season" => {
                conn.execute("DELETE FROM seasons WHERE idSeason=?1", params![row.kodi_id])?;
            }
            "set" => {
                conn.execute("DELETE FROM sets WHERE idSet=?1", params![row.kodi_id])?;
            }
            _ => {}
        }
        if row.file_id > 0 {
            conn.execute("DELETE FROM files WHERE idFile=?1", params![row.file_id])?;
            conn.execute("DELETE FROM videoversion WHERE idFile=?1", params![row.file_id])?;
            conn.execute("DELETE FROM streamdetails WHERE idFile=?1", params![row.file_id])?;
            conn.execute("DELETE FROM bookmark WHERE idFile=?1", params![row.file_id])?;
        }
    }
    MapDb::remove(map_path, jellyfin_id)?;
    Ok(())
}

pub fn apply_playback(
    videos: &Path,
    map_path: &Path,
    jellyfin_id: &str,
    pos_secs: f64,
    duration_secs: f64,
) -> Result<()> {
    let Some(row) = MapDb::lookup(map_path, jellyfin_id)? else {
        return Ok(());
    };
    if row.file_id <= 0 || !videos.exists() {
        return Ok(());
    }
    let conn = Connection::open(videos)?;
    conn.busy_timeout(std::time::Duration::from_secs(8))?;
    let played = duration_secs > 0.0 && pos_secs / duration_secs >= 0.90;
    if played {
        conn.execute(
            "UPDATE files SET playCount = COALESCE(playCount,0)+1, lastPlayed = datetime('now') WHERE idFile=?1",
            params![row.file_id],
        )?;
        conn.execute(
            "DELETE FROM bookmark WHERE idFile=?1 AND type=1",
            params![row.file_id],
        )?;
    } else if pos_secs > 0.0 {
        conn.execute(
            "UPDATE files SET lastPlayed = datetime('now') WHERE idFile=?1",
            params![row.file_id],
        )?;
        conn.execute(
            "DELETE FROM bookmark WHERE idFile=?1 AND type=1",
            params![row.file_id],
        )?;
        conn.execute(
            "INSERT INTO bookmark (idFile, timeInSeconds, totalTimeInSeconds, player, type) VALUES (?1,?2,?3,'VideoPlayer',1)",
            params![row.file_id, pos_secs, duration_secs],
        )?;
    }
    Ok(())
}

pub fn search_local(videos: &Path, map_path: &Path, term: &str, limit: i64) -> Result<Vec<LocalHit>> {
    if term.trim().is_empty() || !videos.exists() || !map_path.exists() || !map_has_kodi_id(map_path)? {
        return Ok(Vec::new());
    }
    let like = format!("%{}%", escape_like(term.trim()));
    let conn = Connection::open(videos)?;
    conn.execute("ATTACH DATABASE ?1 AS jkmap", params![map_path.to_string_lossy()])?;
    let mut stmt = conn.prepare(
        "SELECT jellyfin_id, kind, title, year, plot FROM (
            SELECT m.jellyfin_id AS jellyfin_id, 'movie' AS kind, mo.c00 AS title,
                   CAST(mo.c07 AS INTEGER) AS year, mo.c01 AS plot
            FROM movie mo
            JOIN jkmap.map m ON m.kind='movie' AND m.kodi_id = mo.idMovie
            WHERE mo.c00 LIKE ?1 ESCAPE '\\'
            UNION ALL
            SELECT m.jellyfin_id, 'tvshow', tv.c00, 0, tv.c01
            FROM tvshow tv
            JOIN jkmap.map m ON m.kind='tvshow' AND m.kodi_id = tv.idShow
            WHERE tv.c00 LIKE ?1 ESCAPE '\\'
            UNION ALL
            SELECT m.jellyfin_id, 'episode', ep.c00, 0, ep.c01
            FROM episode ep
            JOIN jkmap.map m ON m.kind='episode' AND m.kodi_id = ep.idEpisode
            WHERE ep.c00 LIKE ?1 ESCAPE '\\'
         ) LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![like, limit], |r| {
        Ok(LocalHit {
            jellyfin_id: r.get(0)?,
            kind: r.get(1)?,
            title: r.get(2)?,
            year: r.get(3)?,
            plot: r.get(4)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

fn map_has_kodi_id(path: &Path) -> Result<bool> {
    let conn = Connection::open(path)?;
    let mut stmt = conn.prepare("PRAGMA table_info(map)")?;
    let cols = stmt.query_map([], |r| r.get::<_, String>(1))?;
    for col in cols {
        if col? == "kodi_id" {
            return Ok(true);
        }
    }
    Ok(false)
}

fn escape_like(term: &str) -> String {
    term.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
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

    pub fn rebuild_from_pass(path: &Path, count: u64, rows: &[MapEntry]) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut conn = Connection::open(path)?;
        conn.execute_batch(
            "DROP TABLE IF EXISTS map;
             CREATE TABLE map (
               jellyfin_id TEXT PRIMARY KEY,
               kind TEXT NOT NULL,
               kodi_id INTEGER NOT NULL,
               file_id INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS meta (k TEXT PRIMARY KEY, v TEXT);",
        )?;
        let tx = conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT OR REPLACE INTO map (jellyfin_id, kind, kodi_id, file_id) VALUES (?1,?2,?3,?4)",
            )?;
            for row in rows {
                stmt.execute(params![row.jellyfin_id, row.kind, row.kodi_id, row.file_id])?;
            }
        }
        tx.execute(
            "INSERT OR REPLACE INTO meta (k,v) VALUES ('last_count', ?1)",
            params![count.to_string()],
        )?;
        tx.execute(
            "INSERT OR REPLACE INTO meta (k,v) VALUES ('last_full_sync', ?1)",
            params![chrono::Utc::now().to_rfc3339()],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn lookup(path: &Path, jellyfin_id: &str) -> Result<Option<MappedRow>> {
        if !path.exists() {
            return Ok(None);
        }
        let conn = Connection::open(path)?;
        let mut stmt = conn.prepare(
            "SELECT kind, kodi_id, file_id FROM map WHERE jellyfin_id=?1",
        )?;
        let mut rows = stmt.query(params![jellyfin_id])?;
        if let Some(row) = rows.next()? {
            return Ok(Some(MappedRow {
                kind: row.get(0)?,
                kodi_id: row.get(1)?,
                file_id: row.get(2)?,
            }));
        }
        Ok(None)
    }

    pub fn upsert_rows(path: &Path, rows: &[MapEntry]) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut conn = Connection::open(path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS map (
               jellyfin_id TEXT PRIMARY KEY,
               kind TEXT NOT NULL,
               kodi_id INTEGER NOT NULL,
               file_id INTEGER NOT NULL
             );",
        )?;
        let tx = conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT OR REPLACE INTO map (jellyfin_id, kind, kodi_id, file_id) VALUES (?1,?2,?3,?4)",
            )?;
            for row in rows {
                stmt.execute(params![row.jellyfin_id, row.kind, row.kodi_id, row.file_id])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn remove(path: &Path, jellyfin_id: &str) -> Result<()> {
        if !path.exists() {
            return Ok(());
        }
        let conn = Connection::open(path)?;
        conn.execute("DELETE FROM map WHERE jellyfin_id=?1", params![jellyfin_id])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::jellyfin::types::{MediaSource, MediaStream};

    #[test]
    fn insert_movie_populates_streamdetails() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("test_schema.db");
        let mut conn = open_fresh_db(&db_path).unwrap();
        let mut cache = FkCache::init(&conn).unwrap();

        let cfg = Config::default();
        let client = JellyfinClient::new(&cfg).unwrap();

        let item = JellyfinItem {
            id: "m1".into(),
            name: "Test Movie 4K".into(),
            item_type: "Movie".into(),
            overview: Some("Plot".into()),
            year: Some(2024),
            premiered: Some("2024-01-01".into()),
            date_created: None,
            runtime_ticks: Some(7200 * 10_000_000),
            mpaa: Some("PG-13".into()),
            community_rating: Some(8.5),
            genres: Some(vec!["Action".into()]),
            studios: None,
            people: None,
            user_data: None,
            series_id: None,
            series_name: None,
            parent_id: None,
            season_id: None,
            parent_index: None,
            index_number: None,
            provider_ids: None,
            media_sources: None,
            media_source_count: None,
            media_streams: Some(vec![
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
                    video_range_type: Some("DOVI".into()),
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
                    profile: Some("Dolby TrueHD + Dolby Atmos".into()),
                    video_range_type: None,
                    dv_profile: None,
                },
            ]),
            primary_version_id: None,
            ..Default::default()
        };

        bulk_insert_items(&mut conn, &mut cache, &client, &[item], 15).unwrap();

        let count: i64 = conn
            .query_row("SELECT count(*) FROM streamdetails", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 2);

        let (v_codec, v_width, v_hdr): (String, i64, String) = conn
            .query_row(
                "SELECT strVideoCodec, iVideoWidth, strHdrType FROM streamdetails WHERE iStreamType = 0",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(v_codec, "hevc");
        assert_eq!(v_width, 3840);
        assert_eq!(v_hdr, "dolbyvision");

        let (a_codec, a_channels): (String, i64) = conn
            .query_row(
                "SELECT strAudioCodec, iAudioChannels FROM streamdetails WHERE iStreamType = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(a_codec, "truehd");
        assert_eq!(a_channels, 8);

        let genre: String = conn
            .query_row(
                "SELECT g.name FROM genre g JOIN genre_link l ON l.genre_id = g.genre_id WHERE l.media_type='movie'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(genre, "Action");
        let outline: String = conn
            .query_row("SELECT c02 FROM movie", [], |r| r.get(0))
            .unwrap();
        assert_eq!(outline, "Plot");

        let map_path = temp.path().join("map.db");
        let rows = cache.take_map();
        MapDb::rebuild_from_pass(&map_path, 1, &rows).unwrap();
        finalize_staging_db(&mut conn).unwrap();
        drop(conn);
        let hits = search_local(&db_path, &map_path, "Test Movie", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].jellyfin_id, "m1");
        assert_eq!(hits[0].kind, "movie");

        let old_map = temp.path().join("old_map.db");
        let old = Connection::open(&old_map).unwrap();
        old.execute_batch(
            "CREATE TABLE map (jellyfin_id TEXT PRIMARY KEY, kind TEXT);
             INSERT INTO map (jellyfin_id, kind) VALUES ('m1','movie');",
        )
        .unwrap();
        drop(old);
        let legacy = search_local(&db_path, &old_map, "Test Movie", 10).unwrap();
        assert!(legacy.is_empty());
    }

    #[test]
    fn insert_movie_writes_one_version_per_media_source() {
        use crate::jellyfin::types::{MediaSource, MediaStream};
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("versions.db");
        let mut conn = open_fresh_db(&db_path).unwrap();
        let mut cache = FkCache::init(&conn).unwrap();
        let client = JellyfinClient::new(&Config::default()).unwrap();
        let video = |height: i32| MediaStream {
            stream_type: "Video".into(),
            height: Some(height),
            codec: Some("hevc".into()),
            ..Default::default()
        };
        let source = |id: &str, height: i32, name: &str| MediaSource {
            id: id.into(),
            name: Some(name.into()),
            bitrate: Some(i64::from(height)),
            media_streams: Some(vec![video(height)]),
            ..Default::default()
        };
        let item = JellyfinItem {
            id: "movie".into(),
            name: "GOAT".into(),
            item_type: "Movie".into(),
            media_sources: Some(vec![source("hd", 1080, "GOAT.1080p.mkv"), source("uhd", 2160, "GOAT.2160p.mkv")]),
            ..Default::default()
        };
        bulk_insert_items(&mut conn, &mut cache, &client, &[item], 0).unwrap();
        let versions: i64 = conn
            .query_row("SELECT COUNT(*) FROM videoversion", [], |r| r.get(0))
            .unwrap();
        assert_eq!(versions, 2);
        let has: i64 = conn
            .query_row("SELECT hasVideoVersions FROM movie_view LIMIT 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(has, 1);
        let default_file: String = conn
            .query_row(
                "SELECT f.strFilename FROM movie m JOIN files f ON f.idFile = m.idFile",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(default_file.contains("source=uhd"), "{default_file}");
        let files: Vec<String> = conn
            .prepare("SELECT strFilename FROM files")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert!(files.iter().any(|f| f.contains("source=hd")));
        let names: Vec<String> = conn
            .prepare("SELECT name FROM videoversiontype WHERE id != ?1")
            .unwrap()
            .query_map(params![VIDEO_VERSION_ID_DEFAULT], |r| r.get(0))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert!(names.iter().any(|n| n.contains("2160p")));
        assert!(names.iter().any(|n| n.contains("1080p")));
    }

    #[test]
    fn delete_mapped_removes_movie_and_all_alternate_versions() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("test_delete_versions.db");
        let map_path = temp.path().join("test_map.db");
        let mut conn = open_fresh_db(&db_path).unwrap();
        let mut cache = FkCache::init(&conn).unwrap();
        let cfg = Config::default();
        let client = JellyfinClient::new(&cfg).unwrap();

        let video = |height: i32| MediaStream {
            stream_type: "Video".into(),
            height: Some(height),
            codec: Some("hevc".into()),
            ..Default::default()
        };
        let source = |id: &str, height: i32, name: &str| MediaSource {
            id: id.into(),
            name: Some(name.into()),
            bitrate: Some(i64::from(height)),
            media_streams: Some(vec![video(height)]),
            ..Default::default()
        };
        let item = JellyfinItem {
            id: "m_del".into(),
            name: "Delete Me".into(),
            item_type: "Movie".into(),
            media_sources: Some(vec![source("hd", 1080, "Delete.1080p.mkv"), source("uhd", 2160, "Delete.2160p.mkv")]),
            ..Default::default()
        };
        bulk_insert_items(&mut conn, &mut cache, &client, &[item], 0).unwrap();
        MapDb::rebuild_from_pass(&map_path, 1, &cache.take_map()).unwrap();
        drop(conn);

        delete_mapped(&db_path, &map_path, "m_del").unwrap();

        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let movie_count: i64 = conn.query_row("SELECT COUNT(*) FROM movie", [], |r| r.get(0)).unwrap();
        assert_eq!(movie_count, 0);
        let file_count: i64 = conn.query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0)).unwrap();
        assert_eq!(file_count, 0);
        let vv_count: i64 = conn.query_row("SELECT COUNT(*) FROM videoversion", [], |r| r.get(0)).unwrap();
        assert_eq!(vv_count, 0);
    }

    #[test]
    fn series_merging_across_libraries_preserves_all_seasons_and_episodes() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("test_series_merge.db");
        let mut conn = open_fresh_db(&db_path).unwrap();
        let mut cache = FkCache::init(&conn).unwrap();

        let cfg = Config::default();
        let client = JellyfinClient::new(&cfg).unwrap();

        // 1. Pass 1: Series arrives with UHD series ID
        let series_uhd = JellyfinItem {
            id: "uhd_series_id".into(),
            name: "Blackadder".into(),
            item_type: "Series".into(),
            overview: Some("Cunning plan".into()),
            year: Some(1983),
            premiered: Some("1983-06-15".into()),
            date_created: None,
            runtime_ticks: None,
            mpaa: None,
            community_rating: None,
            genres: None,
            studios: None,
            people: None,
            user_data: None,
            series_id: None,
            series_name: None,
            parent_id: None,
            season_id: None,
            parent_index: None,
            index_number: None,
            provider_ids: None,
            media_sources: None,
            media_source_count: None,
            media_streams: None,
            primary_version_id: None,
            ..Default::default()
        };

        // 2. Pass 2: Season 1 arrives pointing to UHD series ID
        let season_1 = JellyfinItem {
            id: "season_1_id".into(),
            name: "The Black Adder".into(),
            item_type: "Season".into(),
            overview: None,
            year: Some(1983),
            premiered: None,
            date_created: None,
            runtime_ticks: None,
            mpaa: None,
            community_rating: None,
            genres: None,
            studios: None,
            people: None,
            user_data: None,
            series_id: Some("uhd_series_id".into()),
            series_name: Some("Blackadder".into()),
            parent_id: None,
            season_id: None,
            parent_index: None,
            index_number: Some(1),
            provider_ids: None,
            media_sources: None,
            media_source_count: None,
            media_streams: None,
            primary_version_id: None,
            ..Default::default()
        };

        // Season 4 arrives pointing to HD series ID (alternate library ID suppressed in Pass 1)
        let season_4 = JellyfinItem {
            id: "season_4_id".into(),
            name: "Season 4".into(),
            item_type: "Season".into(),
            overview: None,
            year: Some(1989),
            premiered: None,
            date_created: None,
            runtime_ticks: None,
            mpaa: None,
            community_rating: None,
            genres: None,
            studios: None,
            people: None,
            user_data: None,
            series_id: Some("hd_series_id".into()),
            series_name: Some("Blackadder".into()),
            parent_id: None,
            season_id: None,
            parent_index: None,
            index_number: Some(4),
            provider_ids: None,
            media_sources: None,
            media_source_count: None,
            media_streams: None,
            primary_version_id: None,
            ..Default::default()
        };

        // 3. Pass 3: Season 4 episode arrives pointing to HD series ID and season_4_id
        let ep_s4e1 = JellyfinItem {
            id: "ep_s4e1_id".into(),
            name: "Captain Cook".into(),
            item_type: "Episode".into(),
            overview: Some("Episode plot".into()),
            year: Some(1989),
            premiered: Some("1989-09-28".into()),
            date_created: None,
            runtime_ticks: Some(1800 * 10_000_000),
            mpaa: None,
            community_rating: None,
            genres: None,
            studios: None,
            people: None,
            user_data: None,
            series_id: Some("hd_series_id".into()),
            series_name: Some("Blackadder".into()),
            parent_id: None,
            season_id: Some("season_4_id".into()),
            parent_index: Some(4),
            index_number: Some(1),
            provider_ids: None,
            media_sources: None,
            media_source_count: None,
            media_streams: None,
            primary_version_id: None,
            ..Default::default()
        };

        bulk_insert_items(
            &mut conn,
            &mut cache,
            &client,
            &[series_uhd, season_1, season_4, ep_s4e1],
            15,
        )
        .unwrap();

        // Must create EXACTLY ONE tvshow in Kodi library
        let show_count: i64 = conn
            .query_row("SELECT count(*) FROM tvshow", [], |r| r.get(0))
            .unwrap();
        assert_eq!(show_count, 1, "Must merge into a single tvshow row");

        // Must have BOTH seasons under that single show
        let season_count: i64 = conn
            .query_row("SELECT count(*) FROM seasons", [], |r| r.get(0))
            .unwrap();
        assert_eq!(season_count, 2, "Must contain both Season 1 and Season 4");

        // Episode must be linked to the single show and season 4
        let (ep_show, ep_season): (i64, i64) = conn
            .query_row(
                "SELECT idShow, idSeason FROM episode WHERE c00 = 'Captain Cook'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();

        let (expected_show, expected_season4): (i64, i64) = conn
            .query_row(
                "SELECT idShow, idSeason FROM seasons WHERE season = 4",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();

        assert_eq!(ep_show, expected_show);
        assert_eq!(ep_season, expected_season4);
    }
}
