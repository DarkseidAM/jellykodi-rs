use thiserror::Error;

#[derive(Debug, Error)]
pub enum JellyError {
    #[error("config: {0}")]
    Config(String),
    #[error("jellyfin http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("jellyfin api: {0}")]
    Api(String),
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("offline: jellyfin unreachable")]
    Offline,
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, JellyError>;
