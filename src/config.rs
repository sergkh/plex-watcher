use anyhow::{bail, Context, Result};
use std::{env, path::PathBuf};

pub const CONFIG_HELP: &str = r#"plex-watcher

Usage:
  plex-watcher
  plex-watcher help
  plex-watcher --help
  plex-watcher -h

Configuration is read from environment variables. A local .env file is loaded
when present.

Required:
  TMDB_API_KEY
      TMDb API key used for media lookup and web search.

Core paths:
  WATCH_DIR
      Folder to watch for incoming media.
      Default: /watch
  PLEX_DIR
      Folder where Plex-ready hardlinks/copies are created.
      Default: /plex
  IGNORE_DIRS
      Comma-separated direct subfolders under WATCH_DIR to ignore.
      Default: incomplete

Plex:
  PLEX_URL
      Plex server URL.
      Default: http://plex:32400
  PLEX_TOKEN
      Plex auth token. Empty disables Plex refresh notifications.
      Default: empty
  PLEX_LIBRARY_IDS
      Comma-separated Plex library section IDs to refresh. Empty refreshes all.
      Default: empty
  PLEX_NOTIFY_DEBOUNCE_MS
      Quiet period after file events before processing/refreshing Plex.
      Default: 10000

Web UI:
  WEB_ADDR
      Address for the built-in web UI/API to bind.
      Default: 0.0.0.0:8000

qBittorrent integration (optional):
  QBITTORRENT_URL
      qBittorrent Web API base URL.
      Example: http://qbittorrent:8080
  QBITTORRENT_USERNAME
      qBittorrent username.
  QBITTORRENT_PASSWORD
      qBittorrent password.

Prowlarr integration (optional):
  PROWLARR_URL
      Prowlarr API base URL.
      Example: http://prowlarr:9696
  PROWLARR_API_KEY
      Prowlarr API key.
  PROWLARR_INDEXER_IDS
      Optional comma-separated indexer IDs. Empty searches all enabled indexers.
      Default: empty

Watcher behavior:
  ENABLE_POLLING
      Enables periodic polling in addition to filesystem events.
      Default: false

Logging:
  RUST_LOG
      Rust tracing filter.
      Default: plex_watcher=info
"#;

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub watch_dir: PathBuf,
    pub plex_dir: PathBuf,
    pub plex_url: String,
    pub plex_token: String,
    pub plex_library_ids: Vec<String>,
    pub tmdb_api_key: String,
    pub web_addr: String,
    pub qbittorrent: Option<QBittorrentConfig>,
    pub prowlarr: Option<ProwlarrConfig>,
    pub debounce_ms: u64,
    pub ignored_dirs: Vec<String>,
    pub enable_polling: bool,
}

#[derive(Debug, Clone)]
pub struct QBittorrentConfig {
    pub base_url: String,
    pub username: String,
    pub password: String,
}

#[derive(Debug, Clone)]
pub struct ProwlarrConfig {
    pub base_url: String,
    pub api_key: String,
    pub indexer_ids: Vec<u64>,
}

impl AppConfig {
    pub fn from_env() -> Result<Self> {
        dotenvy::dotenv().ok();
        let tmdb_api_key = env::var("TMDB_API_KEY")
            .context("TMDB_API_KEY environment variable is required")?;
        if tmdb_api_key.trim().is_empty() {
            bail!("TMDB_API_KEY environment variable is required");
        }

        Ok(Self {
            watch_dir: PathBuf::from(env::var("WATCH_DIR").unwrap_or_else(|_| "/watch".into())),
            plex_dir: PathBuf::from(env::var("PLEX_DIR").unwrap_or_else(|_| "/plex".into())),
            plex_url: env::var("PLEX_URL").unwrap_or_else(|_| "http://plex:32400".into()),
            plex_token: env::var("PLEX_TOKEN").unwrap_or_default(),
            plex_library_ids: env::var("PLEX_LIBRARY_IDS")
                .unwrap_or_default()
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect(),
            tmdb_api_key,
            web_addr: env::var("WEB_ADDR").unwrap_or_else(|_| "0.0.0.0:8000".into()),
            qbittorrent: qbittorrent_config_from_env(),
            prowlarr: prowlarr_config_from_env(),
            debounce_ms: env::var("PLEX_NOTIFY_DEBOUNCE_MS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(10_000),
            ignored_dirs: env::var("IGNORE_DIRS")
                .unwrap_or_else(|_| "incomplete".into())
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect(),
            enable_polling: env::var("ENABLE_POLLING")
                .ok()
                .and_then(|v| v.to_lowercase().parse::<bool>().ok())
                .unwrap_or(false),
        })
    }
}

fn prowlarr_config_from_env() -> Option<ProwlarrConfig> {
    let base_url = env::var("PROWLARR_URL").ok()?;
    let api_key = env::var("PROWLARR_API_KEY").ok()?;
    let indexer_ids = env::var("PROWLARR_INDEXER_IDS")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse().ok())
        .collect();

    if base_url.trim().is_empty() || api_key.trim().is_empty() {
        return None;
    }

    Some(ProwlarrConfig {
        base_url: base_url.trim_end_matches('/').to_string(),
        api_key,
        indexer_ids,
    })
}

fn qbittorrent_config_from_env() -> Option<QBittorrentConfig> {
    let base_url = env::var("QBITTORRENT_URL").ok()?;
    let username = env::var("QBITTORRENT_USERNAME").ok()?;
    let password = env::var("QBITTORRENT_PASSWORD").ok()?;

    if base_url.trim().is_empty() || username.trim().is_empty() {
        return None;
    }

    Some(QBittorrentConfig {
        base_url: base_url.trim_end_matches('/').to_string(),
        username,
        password,
    })
}
