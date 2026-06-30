use crate::{config::AppConfig, organizer, parser, plex, tmdb};
use anyhow::{Context, Result};
use serde::Serialize;
use std::collections::HashMap;
use std::{
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};
use tracing::{debug, info, warn};

#[derive(Debug, Serialize)]
pub struct RescanResult {
    pub found_files: usize,
    pub processed_files: usize,
    pub changed_files: usize,
    pub failed_files: usize,
    pub plex_notified: bool,
}

pub fn create_link(src: &Path, link: &Path) -> Result<bool> {
    if let Some(parent) = link.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create_dir_all {}", parent.display()))?;
    }

    if link.exists() {
        if link.metadata()?.ino() == src.metadata()?.ino() {
            debug!("Link already correct: {}", link.display());
            return Ok(false);
        }
        std::fs::remove_file(link)
            .with_context(|| format!("remove stale link {}", link.display()))?;
        info!("Removed stale link: {}", link.display());
    }

    match std::fs::hard_link(src, link) {
        Ok(()) => {
            info!("Hardlink created: {} <- {}", link.display(), src.display());
            Ok(true)
        }
        Err(e) if e.raw_os_error() == Some(18) => {
            info!(
                "Hardlink not supported (cross-device), using copy instead: {}",
                link.display()
            );
            std::fs::copy(src, link)
                .with_context(|| format!("copy {} to {}", src.display(), link.display()))?;
            Ok(true)
        }
        Err(e) => {
            Err(e).with_context(|| format!("create link {} <- {}", link.display(), src.display()))
        }
    }
}

pub async fn process_file(src: &Path, cfg: &AppConfig, http: &reqwest::Client) -> Result<bool> {
    // Parse filename to infer title/season/year before metadata lookup.
    let parsed = parser::parse(src);
    info!(
        "Parsed '{}' -> title='{}' year={:?} tmdb_id={:?} season={:?} episodes={:?}",
        src.file_name().unwrap_or_default().to_string_lossy(),
        parsed.title,
        parsed.year,
        parsed.tmdb_id,
        parsed.season,
        parsed.episodes,
    );

    let media_info = tmdb::lookup(
        http,
        &cfg.tmdb_api_key,
        &parsed.title,
        parsed.year,
        parsed.tmdb_id,
        parsed.season,
    )
    .await
    .with_context(|| format!("TMDB lookup for '{}'", parsed.title))?;

    let link_path = organizer::build_plex_path(&cfg.plex_dir, &media_info, src);
    info!("Plex path: {}", link_path.display());

    create_link(src, &link_path)
}

pub async fn process_folder(
    files: &[PathBuf],
    cfg: &AppConfig,
    http: &reqwest::Client,
) -> Result<bool> {
    if files.is_empty() {
        debug!(
            "Ignoring empty folder {}",
            files[0].parent().unwrap().display()
        );
        return Ok(false);
    }

    let mut parsed_files = Vec::with_capacity(files.len());

    for src in files {
        let relative = src.strip_prefix(&cfg.watch_dir)?;

        info!(
            "Processing file: {} from file {}",
            relative.display(),
            src.display()
        );

        let parsed = parser::parse(relative);

        info!(
            "Parsed '{}' -> title='{}' year={:?} tmdb_id={:?} season={:?} episodes={:?}",
            relative.display(),
            parsed.title,
            parsed.year,
            parsed.tmdb_id,
            parsed.season,
            parsed.episodes,
        );

        parsed_files.push((src, parsed));
    }

    let first = &parsed_files[0].1;

    let media_info = tmdb::lookup(
        http,
        &cfg.tmdb_api_key,
        &first.title,
        first.year,
        first.tmdb_id,
        first.season,
    )
    .await
    .with_context(|| {
        let folder = files[0]
            .parent()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "<unknown>".to_string());
        format!("TMDB lookup for folder '{}'", folder)
    })?;

    info!(
        "Folder-level TMDB lookup complete: folder='{}' files={}",
        files[0]
            .parent()
            .unwrap_or_else(|| Path::new("/"))
            .display(),
        files.len()
    );

    let mut changed = false;
    for (src, _) in parsed_files {
        let link_path = organizer::build_plex_path(&cfg.plex_dir, &media_info, src);
        info!("Plex path: {}", link_path.display());
        if create_link(src, &link_path)? {
            changed = true;
        }
    }

    Ok(changed)
}

pub async fn rescan_watch_dir(cfg: &AppConfig, http: &reqwest::Client) -> Result<RescanResult> {
    let files = scan_video_files(&cfg.watch_dir, &cfg.ignored_dirs)?;
    info!("Manual rescan found {} video files", files.len());

    let mut result = RescanResult {
        found_files: files.len(),
        processed_files: 0,
        changed_files: 0,
        failed_files: 0,
        plex_notified: false,
    };
    let mut root_files = Vec::new();
    let mut files_by_folder: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();

    for src in files {
        let parent = src.parent().map(Path::to_path_buf);
        if parent.as_deref() == Some(cfg.watch_dir.as_path()) {
            root_files.push(src);
            continue;
        }

        let folder = parent.unwrap_or_else(|| cfg.watch_dir.clone());
        files_by_folder.entry(folder).or_default().push(src);
    }

    for src in root_files {
        result.processed_files += 1;
        match process_file(&src, cfg, http).await {
            Ok(changed) => {
                if changed {
                    result.changed_files += 1;
                }
            }
            Err(e) => {
                warn!("Identification failed for file {}: {e:#}", src.display());
                result.failed_files += 1;
                if create_unsorted_link(&src, cfg)? {
                    result.changed_files += 1;
                }
            }
        }
    }

    for files in files_by_folder.into_values() {
        result.processed_files += files.len();
        match process_folder(&files, cfg, http).await {
            Ok(changed) => {
                if changed {
                    result.changed_files += files.len();
                }
            }
            Err(e) => {
                let folder_label = files[0]
                    .parent()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "<unknown>".to_string());
                warn!("Identification failed for folder {}: {e:#}", folder_label);
                result.failed_files += files.len();
                for src in files {
                    if create_unsorted_link(&src, cfg)? {
                        result.changed_files += 1;
                    }
                }
            }
        }
    }

    if result.changed_files > 0 {
        plex::notify_plex(&cfg.plex_url, &cfg.plex_token, &cfg.plex_library_ids, http).await;
        result.plex_notified = !cfg.plex_token.is_empty();
    }

    Ok(result)
}

pub fn remove_hardlinks_pointing_to(plex_dir: &Path, src: &Path) -> Result<()> {
    let src_ino = src.metadata().map(|m| m.ino()).ok();

    fn remove_link(link: &Path) -> Result<()> {
        match std::fs::remove_file(link) {
            Ok(()) => {
                info!("Link removed: {}", link.display());
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).with_context(|| format!("remove link {}", link.display())),
        }
    }

    fn walk(dir: &Path, src_ino: Option<u64>, src: &Path) -> Result<()> {
        for entry in std::fs::read_dir(dir)?.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, src_ino, src)?;
            }
            if let Ok(meta) = path.metadata() {
                if !meta.is_dir() && src_ino == Some(meta.ino()) {
                    remove_link(&path)?;
                }
            }
        }
        Ok(())
    }

    walk(plex_dir, src_ino, src)
}

fn create_unsorted_link(src: &Path, cfg: &AppConfig) -> Result<bool> {
    let fallback = cfg
        .plex_dir
        .join("Unsorted")
        .join(src.file_name().unwrap_or_default());
    create_link(src, &fallback)
}

fn scan_video_files(watch_dir: &Path, ignored_dirs: &[String]) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    scan_video_files_inner(watch_dir, watch_dir, ignored_dirs, &mut files)?;
    Ok(files)
}

fn scan_video_files_inner(
    dir: &Path,
    watch_dir: &Path,
    ignored_dirs: &[String],
    files: &mut Vec<PathBuf>,
) -> Result<()> {
    for entry in std::fs::read_dir(dir)?.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if !should_ignore(&path, watch_dir, ignored_dirs) {
                scan_video_files_inner(&path, watch_dir, ignored_dirs, files)?;
            }
        } else if is_video(&path) && !should_ignore(&path, watch_dir, ignored_dirs) {
            files.push(path);
        }
    }
    Ok(())
}

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
