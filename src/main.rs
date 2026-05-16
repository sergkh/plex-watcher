mod organizer;
mod parser;
mod plex;
mod tmdb;

use dotenvy;
use anyhow::{Context, Result};
use notify::{Config as NotifyConfig, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::{
    collections::HashSet,
    env,
    os::unix::fs as unix_fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{
    sync::mpsc,
    time::{sleep, Instant},
};
use tracing::{debug, error, info, warn};

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct AppConfig {
    watch_dir: PathBuf,
    plex_dir: PathBuf,
    plex_url: String,
    plex_token: String,
    plex_library_ids: Vec<String>,
    tmdb_api_key: String,
    debounce_ms: u64,
}

impl AppConfig {
    fn from_env() -> Result<Self> {
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
        })
    }
}

// ---------------------------------------------------------------------------
// Video extension check
// ---------------------------------------------------------------------------

fn is_video(path: &Path) -> bool {
    const VIDEO_EXTS: &[&str] = &[
        "mp4", "mkv", "avi", "mov", "wmv", "m4v", "ts", "flv", "webm", "mpg", "mpeg",
    ];
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| VIDEO_EXTS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Symlink helpers
// ---------------------------------------------------------------------------

fn create_symlink(src: &Path, link: &Path) -> Result<bool> {
    if let Some(parent) = link.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create_dir_all {}", parent.display()))?;
    }

    if link.exists() || link.symlink_metadata().is_ok() {
        if let Ok(existing) = std::fs::read_link(link) {
            if existing == src {
                debug!("Symlink already correct: {}", link.display());
                return Ok(false);
            }
        }
        std::fs::remove_file(link)
            .with_context(|| format!("remove stale symlink {}", link.display()))?;
        info!("Removed stale symlink: {}", link.display());
    }

    unix_fs::symlink(src, link)
        .with_context(|| format!("symlink {} -> {}", link.display(), src.display()))?;
    info!("Symlink created: {} -> {}", link.display(), src.display());
    Ok(true)
}

fn remove_symlink(link: &Path) -> Result<()> {
    match std::fs::remove_file(link) {
        Ok(()) => { info!("Symlink removed: {}", link.display()); Ok(()) }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("remove symlink {}", link.display())),
    }
}

// ---------------------------------------------------------------------------
// Startup validation
// ---------------------------------------------------------------------------

fn validate_symlink_permissions(plex_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(plex_dir)
        .with_context(|| format!("create plex directory {}", plex_dir.display()))?;

    for subdir in &["Movies", "TV Shows", "Unsorted"] {
        let dir = plex_dir.join(subdir);
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("create {} directory", dir.display()))?;

        let test_src = env::temp_dir().join("media_sync_symlink_test_src");
        let test_link = dir.join(".media_sync_test");

        std::fs::write(&test_src, "test")
            .context("create test source file")?;

        let result = unix_fs::symlink(&test_src, &test_link);
        let _ = std::fs::remove_file(&test_src);
        let _ = std::fs::remove_file(&test_link);

        result.with_context(|| format!(
            "cannot create symlinks in {} - check permissions or filesystem support",
            dir.display()
        ))?;
    }

    Ok(())
}


// ---------------------------------------------------------------------------
// Core: identify file and place it in the Plex tree
// ---------------------------------------------------------------------------

async fn process_file(src: &Path, cfg: &AppConfig, http: &reqwest::Client) -> Result<bool> {
    // 1. Parse the raw filename
    let parsed = parser::parse(src);
    info!(
        "Parsed '{}' -> title='{}' year={:?} season={:?} episodes={:?}",
        src.file_name().unwrap_or_default().to_string_lossy(),
        parsed.title, parsed.year, parsed.season, parsed.episodes,
    );

    // 2. TMDB lookup
    let media_info = tmdb::lookup(
        http, &cfg.tmdb_api_key,
        &parsed.title, parsed.year, parsed.season, &parsed.episodes,
    )
    .await
    .with_context(|| format!("TMDB lookup for '{}'", parsed.title))?;

    // 3. Build Plex path
    let link_path = organizer::build_plex_path(&cfg.plex_dir, &media_info, src);
    info!("Plex path: {}", link_path.display());

    // 4. Create symlink
    create_symlink(src, &link_path)
}

// ---------------------------------------------------------------------------
// File event messages
// ---------------------------------------------------------------------------

enum FileEvent {
    Added(PathBuf),
    Removed(PathBuf),
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "plex_watcher=info".into()),
        )
        .init();

    let cfg = AppConfig::from_env().context("load config")?;
    let cfg = Arc::new(cfg);

    info!("Watching:  {}", cfg.watch_dir.display());
    info!("Plex dir:  {}", cfg.plex_dir.display());
    info!("Plex URL:  {}", cfg.plex_url);
    info!("Debounce:  {}ms", cfg.debounce_ms);

    validate_symlink_permissions(&cfg.plex_dir).context("validate symlink permissions")?;
    info!("Symlink permissions validated");

    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;

    let (tx, mut rx) = mpsc::unbounded_channel::<FileEvent>();

    let watch_dir = cfg.watch_dir.clone();
    let _watcher = {
        let tx = tx.clone();
        let (notify_tx, notify_rx) = std::sync::mpsc::channel::<Result<Event, notify::Error>>();
        let mut watcher = RecommendedWatcher::new(notify_tx, NotifyConfig::default())?;
        watcher.watch(&watch_dir, RecursiveMode::Recursive)?;
        info!("Watcher started. Waiting for files...");

        std::thread::spawn(move || {
            for res in notify_rx {
                match res {
                    Ok(event) => {
                        let paths = event.paths;
                        match event.kind {
                            EventKind::Create(_) | EventKind::Modify(_) => {
                                for p in paths { if is_video(&p) { let _ = tx.send(FileEvent::Added(p)); } }
                            }
                            EventKind::Remove(_) => {
                                for p in paths { if is_video(&p) { let _ = tx.send(FileEvent::Removed(p)); } }
                            }
                            _ => {}
                        }
                    }
                    Err(e) => error!("Notify error: {e}"),
                }
            }
        });
        watcher
    };

    let debounce = Duration::from_millis(cfg.debounce_ms);
    let mut pending_added: HashSet<PathBuf> = HashSet::new();
    let mut pending_removed: HashSet<PathBuf> = HashSet::new();
    let mut deadline: Option<Instant> = None;

    loop {
        let timeout = deadline
            .map(|d| d.saturating_duration_since(Instant::now()))
            .unwrap_or(Duration::from_secs(3600));

        tokio::select! {
            msg = rx.recv() => {
                match msg {
                    Some(FileEvent::Added(p))   => { pending_added.insert(p); }
                    Some(FileEvent::Removed(p)) => { pending_removed.insert(p); }
                    None => break,
                }
                deadline = Some(Instant::now() + debounce);
            }

            _ = sleep(timeout), if deadline.is_some() => {
                deadline = None;
                let mut needs_refresh = false;

                for src in pending_added.drain() {
                    match process_file(&src, &cfg, &http).await {
                        Ok(true)  => needs_refresh = true,
                        Ok(false) => {}
                        Err(e) => {
                            // TMDB failed: put it in Unsorted so nothing is silently dropped
                            warn!("Identification failed for {}: {e:#}", src.display());
                            let fallback = cfg.plex_dir
                                .join("Unsorted")
                                .join(src.file_name().unwrap_or_default());
                            match create_symlink(&src, &fallback) {
                                Ok(true)  => needs_refresh = true,
                                Ok(false) => {}
                                Err(e2)   => error!("{e2:#}"),
                            }
                        }
                    }
                }

                for src in pending_removed.drain() {
                    if let Err(e) = remove_symlinks_pointing_to(&cfg.plex_dir, &src) {
                        error!("Cleanup error: {e:#}");
                    } else {
                        needs_refresh = true;
                    }
                }

                if needs_refresh {
                    plex::notify_plex(&cfg.plex_url, &cfg.plex_token, &cfg.plex_library_ids, &http).await;
                }
            }
        }
    }
    Ok(())
}

/// Walk the plex tree and remove any symlink whose resolved target matches `src`.
fn remove_symlinks_pointing_to(plex_dir: &Path, src: &Path) -> Result<()> {
    fn walk(dir: &Path, src: &Path) -> Result<()> {
        for entry in std::fs::read_dir(dir)?.flatten() {
            let path = entry.path();
            if path.is_dir() { walk(&path, src)?; }
            if path.symlink_metadata().map(|m| m.file_type().is_symlink()).unwrap_or(false) {
                if let Ok(target) = std::fs::read_link(&path) {
                    if target == src { remove_symlink(&path)?; }
                }
            }
        }
        Ok(())
    }
    walk(plex_dir, src)
}
