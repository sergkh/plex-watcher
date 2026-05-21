use crate::{config::AppConfig, organizer, parser, tmdb};
use anyhow::{Context, Result};
use std::{
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};
use tracing::{debug, info, warn};

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
        Err(e) => Err(e).with_context(|| format!("create link {} <- {}", link.display(), src.display())),
    }
}

pub async fn process_file(src: &Path, cfg: &AppConfig, http: &reqwest::Client) -> Result<bool> {
    // Parse filename to infer title/season/year before metadata lookup.
    let parsed = parser::parse(src);
    info!(
        "Parsed '{}' -> title='{}' year={:?} season={:?} episodes={:?}",
        src.file_name().unwrap_or_default().to_string_lossy(),
        parsed.title,
        parsed.year,
        parsed.season,
        parsed.episodes,
    );

    let media_info = tmdb::lookup(
        http,
        &cfg.tmdb_api_key,
        &parsed.title,
        parsed.year,
        parsed.season
    )
    .await
    .with_context(|| format!("TMDB lookup for '{}'", parsed.title))?;

    let link_path = organizer::build_plex_path(&cfg.plex_dir, &media_info, src);
    info!("Plex path: {}", link_path.display());

    create_link(src, &link_path)
}

pub async fn process_folder(files: &[PathBuf], cfg: &AppConfig, http: &reqwest::Client) -> Result<bool> {
    if files.is_empty() {
        debug!("Ignoring empty folder {}", files[0].parent().unwrap().display());
        return Ok(false);
    }

    let mut parsed_files = Vec::with_capacity(files.len());    
    
    for src in files {

        let relative = src.strip_prefix(&cfg.watch_dir)?;
        
        info!("Processing file: {} from file {}", relative.display(), src.display());

        let parsed = parser::parse(relative);
        
        info!(
            "Parsed '{}' -> title='{}' year={:?} season={:?} episodes={:?}",
            relative.display(),
            parsed.title,
            parsed.year,
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
        files[0].parent().unwrap_or_else(|| Path::new("/")).display(),
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
