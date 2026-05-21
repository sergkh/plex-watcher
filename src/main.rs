mod config;
mod organizer;
mod parser;
mod plex;
mod processor;
mod tmdb;

use anyhow::{Context, Result};
use notify::{Config as NotifyConfig, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{
    sync::mpsc,
    time::{sleep, Instant},
};
use tracing::{debug, error, info, warn};

use config::AppConfig;
use processor::{create_link, process_file, process_folder, remove_hardlinks_pointing_to};

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

fn should_ignore(path: &Path, watch_dir: &Path, ignored_dirs: &[String]) -> bool {
    if let Ok(rel_path) = path.strip_prefix(watch_dir) {
        if let Some(first_component) = rel_path.components().next() {
            if let Some(dir_name) = first_component.as_os_str().to_str() {
                return ignored_dirs.iter().any(|ignored| ignored == dir_name);
            }
        }
    }
    false
}


// ---------------------------------------------------------------------------
// Startup validation
// ---------------------------------------------------------------------------

fn validate_hardlink_permissions(watch_dir: &Path, plex_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(plex_dir)
        .with_context(|| format!("create plex directory {}", plex_dir.display()))?;

    let test_src = watch_dir.join(".media_sync_test_src");
    std::fs::write(&test_src, "test")
        .context("create test source file in watch directory")?;

    let mut use_copy = false;

    for subdir in &["Movies", "TV Shows", "Unsorted"] {
        let dir = plex_dir.join(subdir);
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("create {} directory", dir.display()))?;

        let test_link = dir.join(".media_sync_test");

        match std::fs::hard_link(&test_src, &test_link) {
            Ok(()) => debug!("Hardlink test succeeded in {}", dir.display()),
            Err(e) if e.raw_os_error() == Some(18) => {
                info!("Hardlinks not supported across {} and {} - will use copy instead",
                      watch_dir.display(), dir.display());
                use_copy = true;
                let _ = std::fs::copy(&test_src, &test_link);
            }
            Err(e) => {
                let _ = std::fs::remove_file(&test_src);
                let _ = std::fs::remove_file(&test_link);
                return Err(e).with_context(|| format!(
                    "cannot create links in {} - check permissions",
                    dir.display()
                ));
            }
        }
        let _ = std::fs::remove_file(&test_link);
    }

    std::fs::remove_file(&test_src).ok();

    if use_copy {
        info!("Link method: copy (hardlinks not supported)");
    } else {
        info!("Link method: hardlink");
    }

    Ok(())
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
    info!("Polling:   {}", if cfg.enable_polling { "enabled" } else { "disabled" });
    info!("Ignored:   {:?}", cfg.ignored_dirs);

    validate_hardlink_permissions(&cfg.watch_dir, &cfg.plex_dir).context("validate hardlink permissions")?;
    info!("Hardlink permissions validated");

    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;

    let (tx, mut rx) = mpsc::unbounded_channel::<FileEvent>();

    let watch_dir = cfg.watch_dir.clone();
    let _watcher = {
        let tx = tx.clone();
        let watch_dir_clone = watch_dir.clone();
        let ignored_dirs = cfg.ignored_dirs.clone();
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
                                for p in paths {
                                    debug!("Event: Create/Modify: {}", p.display());
                                    let is_vid = is_video(&p);
                                    let is_ign = should_ignore(&p, &watch_dir_clone, &ignored_dirs);
                                    debug!("  is_video={} should_ignore={}", is_vid, is_ign);
                                    if is_vid && !is_ign {
                                        let _ = tx.send(FileEvent::Added(p));
                                    }
                                }
                            }
                            EventKind::Remove(_) => {
                                for p in paths {
                                    debug!("Event: Remove: {}", p.display());
                                    let is_vid = is_video(&p);
                                    let is_ign = should_ignore(&p, &watch_dir_clone, &ignored_dirs);
                                    debug!("  is_video={} should_ignore={}", is_vid, is_ign);
                                    if is_vid && !is_ign {
                                        let _ = tx.send(FileEvent::Removed(p));
                                    }
                                }
                            }
                            kind => debug!("Event (ignored): {:?}", kind),
                        }
                    }
                    Err(e) => error!("Notify error: {e}"),
                }
            }
        });
        watcher
    };

    // Scan for any existing video files that may have been added before watcher started
    let tx_initial = tx.clone();
    match scan_initial_files(&watch_dir, &cfg.ignored_dirs) {
        Ok(files) => {
            info!("Found {} existing video files", files.len());
            for file in files {
                let _ = tx_initial.send(FileEvent::Added(file));
            }
        }
        Err(e) => warn!("Initial directory scan failed: {e:#}"),
    }

    let debounce = Duration::from_millis(cfg.debounce_ms);
    let mut pending_added: HashSet<PathBuf> = HashSet::new();
    let mut pending_removed: HashSet<PathBuf> = HashSet::new();
    let mut deadline: Option<Instant> = None;
    let mut last_scan = Instant::now();
    let mut last_seen_files: HashSet<PathBuf> = HashSet::new();
    let poll_interval = Duration::from_secs(5);

    loop {
        let timeout = if cfg.enable_polling {
            deadline
                .map(|d| d.saturating_duration_since(Instant::now()))
                .unwrap_or(poll_interval)
        } else {
            deadline
                .map(|d| d.saturating_duration_since(Instant::now()))
                .unwrap_or(Duration::from_secs(3600))
        };

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

                let mut added_by_folder: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
                let mut root_files: Vec<PathBuf> = Vec::new();
                for src in pending_added.drain() {
                    let parent = src.parent().map(Path::to_path_buf);
                    if parent.as_deref() == Some(watch_dir.as_path()) {
                        root_files.push(src);
                        continue;
                    }

                    let folder = parent.unwrap_or_else(|| watch_dir.clone());
                    added_by_folder.entry(folder).or_default().push(src);
                }

                for src in root_files {
                    match process_file(&src, &cfg, &http).await {
                        Ok(true) => needs_refresh = true,
                        Ok(false) => {}
                        Err(e) => {
                            warn!("Identification failed for file {}: {e:#}", src.display());
                            let fallback = cfg
                                .plex_dir
                                .join("Unsorted")
                                .join(src.file_name().unwrap_or_default());
                            match create_link(&src, &fallback) {
                                Ok(true) => needs_refresh = true,
                                Ok(false) => {}
                                Err(e2) => error!("{e2:#}"),
                            }
                        }
                    }
                }

                for files in added_by_folder.into_values() {
                    match process_folder(&files, &cfg, &http).await {
                        Ok(true) => needs_refresh = true,
                        Ok(false) => {}
                        Err(e) => {
                            // Metadata lookup failed: put files in Unsorted so nothing is dropped
                            let folder_label = files[0]
                                .parent()
                                .map(|p| p.display().to_string())
                                .unwrap_or_else(|| "<unknown>".to_string());
                            warn!("Identification failed for folder {}: {e:#}", folder_label);
                            for src in files {
                                let fallback = cfg.plex_dir
                                    .join("Unsorted")
                                    .join(src.file_name().unwrap_or_default());
                                match create_link(&src, &fallback) {
                                    Ok(true)  => needs_refresh = true,
                                    Ok(false) => {}
                                    Err(e2)   => error!("{e2:#}"),
                                }
                            }
                        }
                    }
                }

                for src in pending_removed.drain() {
                    if let Err(e) = remove_hardlinks_pointing_to(&cfg.plex_dir, &src) {
                        error!("Cleanup error: {e:#}");
                    } else {
                        needs_refresh = true;
                    }
                }

                if needs_refresh {
                    plex::notify_plex(&cfg.plex_url, &cfg.plex_token, &cfg.plex_library_ids, &http).await;
                }
            }

            _ = sleep(poll_interval), if deadline.is_none() && cfg.enable_polling => {
                // Periodic scan to catch files that appeared without triggering events (e.g., copies)
                if last_scan.elapsed() >= poll_interval {
                    last_scan = Instant::now();
                    if let Ok(current_files) = scan_initial_files(&watch_dir, &cfg.ignored_dirs) {
                        let current_set: HashSet<_> = current_files.into_iter().collect();
                        let new_files: Vec<_> = current_set.difference(&last_seen_files).cloned().collect();

                        if !new_files.is_empty() {
                            debug!("Polling found {} new files", new_files.len());
                            for file in new_files {
                                info!("Detected via polling: {}", file.display());
                                pending_added.insert(file);
                            }
                            deadline = Some(Instant::now() + debounce);
                        }
                        last_seen_files = current_set;
                    }
                }
            }
        }
    }
    Ok(())
}

/// Scan for existing video files in the watch directory on startup
fn scan_initial_files(watch_dir: &Path, ignored_dirs: &[String]) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();

    fn walk(dir: &Path, watch_dir: &Path, ignored_dirs: &[String], files: &mut Vec<PathBuf>) -> Result<()> {
        for entry in std::fs::read_dir(dir)?.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if !should_ignore(&path, watch_dir, ignored_dirs) {
                    walk(&path, watch_dir, ignored_dirs, files)?;
                }
            } else if is_video(&path) && !should_ignore(&path, watch_dir, ignored_dirs) {
                files.push(path);
            }
        }
        Ok(())
    }

    walk(watch_dir, watch_dir, ignored_dirs, &mut files)?;
    Ok(files)
}
