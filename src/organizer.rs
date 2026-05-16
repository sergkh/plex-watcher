//! Builds the Plex-compatible destination path from TMDB metadata.
//!
//! Plex naming conventions used here:
//!   Movies:   {plex_dir}/Movies/{Title} ({Year})/{original_filename}
//!   Episodes: {plex_dir}/TV Shows/{Show} ({Year})/Season {N}/{original_filename}

use std::path::{Path, PathBuf};
use crate::tmdb::MediaInfo;

/// Given the TMDB lookup result and the original source path, return the
/// full symlink destination path inside `plex_root`.
pub fn build_plex_path(plex_root: &Path, info: &MediaInfo, src: &Path) -> PathBuf {
    let filename = src.file_name().unwrap_or_default();

    match info {
        MediaInfo::Movie { title, year, .. } => {
            // Movies/Dune Part Two (2024)/Dune.Part.Two.2024.2160p.mkv
            let folder = sanitize(&format!("{} ({})", title, year));
            plex_root
                .join("Movies")
                .join(&folder)
                .join(filename)
        }

        MediaInfo::Episode { show_title, show_year, season, .. } => {
            // TV Shows/Breaking Bad (2008)/Season 03/Breaking.Bad.S03E07.mkv
            let show_folder = sanitize(&format!("{} ({})", show_title, show_year));
            let season_folder = format!("Season {:02}", season);
            plex_root
                .join("TV Shows")
                .join(&show_folder)
                .join(&season_folder)
                .join(filename)
        }
    }
}

/// Strip characters that are illegal in most file systems / awkward for Plex.
fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c => c,
        })
        .collect::<String>()
        .trim()
        .to_string()
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tmdb::MediaInfo;
    use std::path::Path;

    #[test]
    fn movie_path() {
        let info = MediaInfo::Movie {
            title: "Dune: Part Two".into(),
            year: 2024,
            tmdb_id: 1,
        };
        let result = build_plex_path(
            Path::new("/plex"),
            &info,
            Path::new("/watch/Dune.Part.Two.2024.2160p.mkv"),
        );
        assert_eq!(
            result,
            Path::new("/plex/Movies/Dune_ Part Two (2024)/Dune.Part.Two.2024.2160p.mkv")
        );
    }

    #[test]
    fn episode_path() {
        let info = MediaInfo::Episode {
            show_title: "Breaking Bad".into(),
            show_year: 2008,
            season: 3,
            episodes: vec![7],
            tmdb_id: 1,
        };
        let result = build_plex_path(
            Path::new("/plex"),
            &info,
            Path::new("/watch/Breaking.Bad.S03E07.1080p.mkv"),
        );
        assert_eq!(
            result,
            Path::new("/plex/TV Shows/Breaking Bad (2008)/Season 03/Breaking.Bad.S03E07.1080p.mkv")
        );
    }
}
