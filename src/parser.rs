//! Parses media metadata out of raw filenames.
//!
//! Handles the most common scene/release naming conventions:
//!   Show.Name.S01E03.Episode.Title.1080p.BluRay.mkv
//!   Show Name - 1x03 - Episode Title.mkv
//!   Movie.Name.2023.2160p.UHD.BluRay.mkv
//!   Movie Name (2023).mkv
//!
//! Also extracts show name and year from folder structure:
//!   Show Name (2019)/Season 03/S03E08.Title.mkv

use once_cell::sync::Lazy;
use regex::Regex;
use tracing::debug;
use std::path::Path;

/// Everything the parser could extract from a filename.
#[derive(Debug, Clone)]
pub struct ParsedName {
    /// Best-guess title (spaces, proper casing applied)
    pub title: String,
    /// Four-digit year, if found
    pub year: Option<u16>,
    /// Season number — `Some` means this looks like a TV episode
    pub season: Option<u16>,
    /// Episode number(s), e.g. `vec![3]` or `vec![3, 4]` for multi-episode files
    pub episodes: Vec<u16>,
}

impl ParsedName {
    pub fn is_episode(&self) -> bool {
        self.season.is_some()
    }
}

// ── compiled regexes ─────────────────────────────────────────────────────────

/// SnnEnn or SnnEnnEnn (multi-episode), optionally at start of string
static RE_SXX_EXX: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)(?:^|[. _-])S(\d{1,2})E(\d{1,2})(?:E(\d{1,2}))*").unwrap()
});

/// 1x03 style, optionally at start of string
static RE_1X03: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)(?:^|[. _-])(\d{1,2})x(\d{2})").unwrap()
});

/// Four-digit year, usually surrounded by dots/spaces/parens
static RE_YEAR: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?:^|[. _(])((19|20)\d{2})(?:[. _)]|$)").unwrap()
});

/// Common release junk that marks the end of the meaningful title
static RE_JUNK: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?i)[. _-](1080p|2160p|720p|480p|4k|uhd|bluray|blu-ray|bdrip|brrip|dvdrip|web-?dl|webrip|hdtv|proper|repack|extended|theatrical|directors\.cut|YIFY|RARBG|x264|x265|h\.?264|h\.?265|avc|hevc|xvid|divx|aac|ac3|dts|truehd|atmos|10bit|hdr|dovi|remux|atvp).*,?",
    )
    .unwrap()
});

/// Bracketed metadata tags like [tmdbid-338], [UKR_ENG], [Hurtom]
static RE_BRACKET_TAG: Lazy<Regex> = Lazy::new(|| Regex::new(r"\[[^\]]+\]").unwrap());

/// Standalone season markers in titles/folders, e.g. .S01. or - S2 -
static RE_STANDALONE_SEASON: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)(?:^|[. _-])S\d{1,2}(?:$|[. _-])").unwrap());

// ── public API ────────────────────────────────────────────────────────────────

pub fn parse(path: &Path) -> ParsedName {
    
    debug!("Parsing path: {}", path.display());

    // Prefer metadata from the first folder in the incoming relative path.
    let folder_hint = extract_from_first_folder(path);

    // Work on the filename stem only
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default();

    let cleaned_stem = pre_clean(stem);

    debug!("Parsing filename: {}", cleaned_stem);

    // 1. Look for SxxExx
    if let Some(caps) = RE_SXX_EXX.captures(&cleaned_stem) {
        let season: u16 = caps[1].parse().unwrap_or(1);
        let ep1: u16 = caps[2].parse().unwrap_or(1);
        let mut episodes = vec![ep1];
        if let Some(ep2) = caps.get(3) {
            if let Ok(n) = ep2.as_str().parse() {
                episodes.push(n);
            }
        }

        // Extract title: either before the match (if not at start) or after (if at start)
        let title_raw = if caps.get(0).unwrap().start() == 0 {
            // Match at beginning: S03E08. The Boys -> extract "The Boys"
            let after_match = &cleaned_stem[caps.get(0).unwrap().end()..];
            after_match.trim_start_matches(|c: char| c == '.' || c == ' ' || c == '_' || c == '-')
        } else {
            // Match in middle: The.Boys.S03E08.Title -> extract before S03E08
            &cleaned_stem[..caps.get(0).unwrap().start()]
        };

        let mut year = extract_year(&cleaned_stem);

        // Also remove year from title if found
        let title_without_year = if let Some(y) = year {
            title_raw
                .replace(&format!("({})", y), "")
                .replace(&format!(".{}", y), "")
                .replace(&format!(" {}", y), "")
        } else {
            title_raw.to_string()
        };

        let mut title = clean_title(&title_without_year);
        if let Some((folder_title, folder_year)) = folder_hint.clone() {
            title = folder_title;
            year = folder_year.or(year);
        }

        return ParsedName {
            title,
            year,
            season: Some(season),
            episodes,
        };
    }

    // 2. Look for 1x03
    if let Some(caps) = RE_1X03.captures(&cleaned_stem) {
        let season: u16 = caps[1].parse().unwrap_or(1);
        let episode: u16 = caps[2].parse().unwrap_or(1);

        // Extract title: either before the match or after (if at start)
        let title_raw = if caps.get(0).unwrap().start() == 0 {
            // Match at beginning: 1x03.Show.Name -> extract "Show Name"
            let after_match = &cleaned_stem[caps.get(0).unwrap().end()..];
            after_match.trim_start_matches(|c: char| c == '.' || c == ' ' || c == '_' || c == '-')
        } else {
            // Match in middle: Show.Name.1x03.Title -> extract before 1x03
            &cleaned_stem[..caps.get(0).unwrap().start()]
        };

        let mut year = extract_year(&cleaned_stem);

        // Also remove year from title if found
        let title_without_year = if let Some(y) = year {
            title_raw
                .replace(&format!("({})", y), "")
                .replace(&format!(".{}", y), "")
                .replace(&format!(" {}", y), "")
        } else {
            title_raw.to_string()
        };

        let mut title = clean_title(&title_without_year);
        if let Some((folder_title, folder_year)) = folder_hint.clone() {
            title = folder_title;
            year = folder_year.or(year);
        }

        return ParsedName {
            title,
            year,
            season: Some(season),
            episodes: vec![episode],
        };
    }

    // 3. Movie: cleanup first, then parse year/title
    let mut year = extract_year(&cleaned_stem);

    // Remove the year from the title string too
    let title_raw = if let Some(y) = year {
        cleaned_stem
            .replace(&format!("({})", y), "")
            .replace(&format!(".{}", y), "")
            .replace(&format!(" {}", y), "")
    } else {
        cleaned_stem
    };

    let mut title = clean_title(&title_raw);
    if let Some((folder_title, folder_year)) = folder_hint {
        title = folder_title;
        year = folder_year.or(year);
    }

    ParsedName {
        title,
        year,
        season: None,
        episodes: vec![],
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn extract_year(s: &str) -> Option<u16> {
    RE_YEAR
        .captures(s)
        .and_then(|c| c.get(1))
        .and_then(|m| m.as_str().parse().ok())
}

fn pre_clean(s: &str) -> String {
    let without_junk = RE_JUNK.replace(s, "").into_owned();
    let without_tags = RE_BRACKET_TAG.replace_all(&without_junk, " ").into_owned();
    let without_season = RE_STANDALONE_SEASON
        .replace_all(&without_tags, " ")
        .into_owned();

    without_season
        .trim_matches(|c: char| c == '\'' || c == '"' || c.is_whitespace())
        .to_string()
}

/// Replace dots/underscores with spaces and trim.
fn clean_title(raw: &str) -> String {
    raw.replace(['.', '_'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_matches(|c: char| c == '\'' || c == '"')
        .trim()
        .to_string()
}

/// Extract title/year from the first folder component in the path.
/// For "Show Name (2019)/Season 03/S03E08.Title.mkv" -> Some(("Show Name", Some(2019))).
fn extract_from_first_folder(path: &Path) -> Option<(String, Option<u16>)> {
    let parent = path.parent()?;
    let first_folder = parent.components().next()?.as_os_str().to_str()?;

    let cleaned = pre_clean(first_folder);
    let year = extract_year(&cleaned).or_else(|| extract_year(first_folder));
    let title_without_year = if let Some(y) = year {
        cleaned
            .replace(&format!("({})", y), "")
            .replace(&format!(".{}", y), "")
            .replace(&format!(" {}", y), "")
    } else {
        cleaned
    };

    let title = clean_title(&title_without_year);
    if title.is_empty() {
        None
    } else {
        Some((title, year))
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn p(s: &str) -> ParsedName {
        parse(Path::new(s))
    }

    #[test]
    fn should_parse_the_boys_s1_episode() {
        let r = p("The Boys (2019) BDRip-AVC [UKR_ENG] [Hurtom]/Season 01/S01E01. The Boys (2019) BDRip-AVC [2xUKR_ENG] [Hurtom].mkv");
        assert_eq!(r.title, "The Boys");
        assert_eq!(r.year, Some(2019));
        assert_eq!(r.season, Some(1));        
        assert_eq!(r.episodes, vec![1]);
    }


    #[test]
    fn should_parse_the_boys_s2_episode() {
        let r = p("The Boys (2019) BDRip-AVC [UKR_ENG] [Hurtom]/Season 02/S02E02. The Boys (2020) BDRip-AVC [2xUKR_ENG] [Hurtom].mkv");
        assert_eq!(r.title, "The Boys");
        assert_eq!(r.year, Some(2019));
        assert_eq!(r.season, Some(2));        
        assert_eq!(r.episodes, vec![2]);
    }

    #[test]
    fn should_parse_margo_episode() {
        let r = p("Margos.Got.Money.Troubles.S01.2026.ATVP.WEB-DL.1080p/Margos.Got.Money.Troubles.S01E01.2026.ATVP.WEB-DL.1080p.mkv");
        assert_eq!(r.title, "Margos Got Money Troubles");
        assert_eq!(r.year, Some(2026));
        assert_eq!(r.season, Some(1));
        assert_eq!(r.episodes, vec![1]);
    }

    #[test]
    fn should_parse_a_movie_in_the_root() {
        let r = p("Dust Bunny (2025)/Dust Bunny (2025) WEB-DLRip-AVC Ukr Eng.mkv");
        assert_eq!(r.title, "Dust Bunny");
        assert_eq!(r.year, Some(2025));
        assert_eq!(r.season, None);
    }


    #[test]
    fn should_parse_a_movie_in_the_root_with_tmdbid() {
        let r = p("'Good Bye, Lenin! (2003) [tmdbid-338].mkv");
        assert_eq!(r.title, "Good Bye, Lenin!");
        assert_eq!(r.year, Some(2003));
        assert_eq!(r.season, None);
    }

    #[test]
    fn sxx_exx() {
        let r = p("Breaking.Bad/Season 3/Breaking.Bad.S03E07.One.Minute.1080p.BluRay.mkv");
        assert_eq!(r.title, "Breaking Bad");
        assert_eq!(r.season, Some(3));
        assert_eq!(r.episodes, vec![7]);
    }

    #[test]
    fn multi_episode() {
        let r = p("The.Office/Season 02/The.Office.S02E01E02.720p.mkv");
        assert_eq!(r.title, "The Office");
        assert_eq!(r.season, Some(2));
        assert_eq!(r.episodes, vec![1, 2]);
    }

    #[test]
    fn one_x_style() {
        let r = p("Firefly.1x03.Triangle.mkv");
        assert_eq!(r.title, "Firefly");
        assert_eq!(r.season, Some(1));
        assert_eq!(r.episodes, vec![3]);
    }

    #[test]
    fn movie_with_year() {
        let r = p("Dune.Part.Two.2024.2160p.UHD.BluRay.mkv");
        assert_eq!(r.title, "Dune Part Two");
        assert_eq!(r.year, Some(2024));
        assert!(!r.is_episode());
    }

    #[test]
    fn movie_parens_year() {
        let r = p("The.Godfather.(1972).mkv");
        assert_eq!(r.title, "The Godfather");
        assert_eq!(r.year, Some(1972));
    }

    #[test]
    fn episode_at_start() {
        let r = p("S03E08. The Boys (2022) BDRip-AVC [4xUKR_ENG] [Hurtom].mkv");
        assert_eq!(r.title, "The Boys");
        assert_eq!(r.year, Some(2022));
        assert_eq!(r.season, Some(3));
        assert_eq!(r.episodes, vec![8]);
    }
}
