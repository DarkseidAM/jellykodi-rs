mod config;
mod error;
mod ipc;
mod jellyfin;
mod playback;
mod state;
mod sync;
mod widgets;

use clap::Parser;
use config::Config;
use jellyfin::JellyfinClient;
use state::AppState;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(name = "jellykodi-rs", version, about = "Jellyfin 12 hybrid sidecar for Kodi")]
struct Args {
    /// Path to JSON config (server URL, token, paths).
    #[arg(long, env = "JELLYKODI_CONFIG")]
    config: PathBuf,

    /// Skip initial background sync workers (IPC + widgets only).
    #[arg(long, default_value_t = false)]
    ipc_only: bool,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("info".parse().unwrap()))
        .init();

    let args = Args::parse();
    let cfg = match Config::load(&args.config) {
        Ok(c) => c,
        Err(e) => {
            error!("config error: {e}");
            std::process::exit(1);
        }
    };

    info!(
        "jellykodi-rs {} starting; server={} (Jellyfin 12.x API)",
        env!("CARGO_PKG_VERSION"),
        cfg.server_url
    );

    let client = match JellyfinClient::new(&cfg) {
        Ok(c) => c,
        Err(e) => {
            error!("http client: {e}");
            std::process::exit(1);
        }
    };

    match client.ping().await {
        Ok(()) => {
            info!("jellyfin reachable");
        }
        Err(e) => {
            error!("jellyfin ping failed: {e}");
            // Continue — offline browse still useful once DB exists.
        }
    }

    let state = AppState::new();
    state.set_online(true);
    *state.library_paths.write().await = Some(crate::state::LibraryPaths {
        kodi_db_dir: cfg.kodi_db_dir.clone(),
        map_db_path: cfg.map_db_path.clone(),
    });

    let sock = cfg.socket_path.clone();
    let ipc_client = client.clone();
    let ipc_state = Arc::clone(&state);
    let ipc_task = tokio::spawn(async move {
        if let Err(e) = ipc::serve_socket(&sock, ipc_client, ipc_state).await {
            error!("ipc server exited: {e}");
        }
    });

    let ws_client = client.clone();
    let ws_state = Arc::clone(&state);
    let ws_task = tokio::spawn(async move {
        jellyfin::ws::run_websocket_loop(ws_client, ws_state).await;
    });

    let sync_task = if args.ipc_only {
        tokio::spawn(async {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
            }
        })
    } else {
        let sync_client = client.clone();
        let sync_cfg = cfg.clone();
        let sync_state = Arc::clone(&state);
        tokio::spawn(async move {
            sync::run_sync_workers(sync_client, sync_cfg, sync_state).await;
        })
    };

    tokio::select! {
        _ = ipc_task => error!("ipc task ended"),
        _ = ws_task => error!("ws task ended"),
        _ = sync_task => error!("sync task ended"),
        _ = tokio::signal::ctrl_c() => info!("shutdown"),
    }
}
