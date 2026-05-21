use anyhow::{Context, Result};
use std::{env, path::PathBuf};

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub watch_dir: PathBuf,
    pub plex_dir: PathBuf,
    pub plex_url: String,
    pub plex_token: String,
    pub plex_library_ids: Vec<String>,
    pub tmdb_api_key: String,
    pub debounce_ms: u64,
    pub ignored_dirs: Vec<String>,
    pub enable_polling: bool,
}

impl AppConfig {
    pub fn from_env() -> Result<Self> {
        dotenvy::dotenv().ok();
        let tmdb_api_key = env::var("TMDB_API_KEY")
            .context("TMDB_API_KEY environment variable is required")?;

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
