//! Parses media metadata out of raw filenames.
//!
//! Handles the most common scene/release naming conventions:
//!   Show.Name.S01E03.Episode.Title.1080p.BluRay.mkv
//!   Show Name - 1x03 - Episode Title.mkv
//!   Movie.Name.2023.2160p.UHD.BluRay.mkv
//!   Movie Name (2023).mkv

use once_cell::sync::Lazy;
use regex::Regex;
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
    Regex::new(r"(?i)(?:^|[. _-])S(\d{1,2})E(\d{2})(?:E(\d{2}))*").unwrap()
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
        r"(?i)[. _-](1080p|2160p|720p|480p|4k|uhd|bluray|blu-ray|bdrip|brrip|dvdrip|web-?dl|webrip|hdtv|proper|repack|extended|theatrical|directors\.cut|YIFY|RARBG|x264|x265|h\.?264|h\.?265|avc|hevc|xvid|divx|aac|ac3|dts|truehd|atmos|10bit|hdr|dovi|remux).*",
    )
    .unwrap()
});

// ── public API ────────────────────────────────────────────────────────────────

pub fn parse(path: &Path) -> ParsedName {
    // Work on the filename stem only
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default();

    // 1. Look for SxxExx
    if let Some(caps) = RE_SXX_EXX.captures(stem) {
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
            let after_match = &stem[caps.get(0).unwrap().end()..];
            after_match.trim_start_matches(|c: char| c == '.' || c == ' ' || c == '_' || c == '-')
        } else {
            // Match in middle: The.Boys.S03E08.Title -> extract before S03E08
            &stem[..caps.get(0).unwrap().start()]
        };

        // Remove junk from the extracted title
        let without_junk = RE_JUNK.replace(title_raw, "");
        let year = extract_year(stem);

        // Also remove year from title if found
        let title_without_year = if let Some(y) = year {
            without_junk
                .replace(&format!("({})", y), "")
                .replace(&format!(".{}", y), "")
                .replace(&format!(" {}", y), "")
        } else {
            without_junk.into_owned()
        };

        return ParsedName {
            title: clean_title(&title_without_year),
            year,
            season: Some(season),
            episodes,
        };
    }

    // 2. Look for 1x03
    if let Some(caps) = RE_1X03.captures(stem) {
        let season: u16 = caps[1].parse().unwrap_or(1);
        let episode: u16 = caps[2].parse().unwrap_or(1);

        // Extract title: either before the match or after (if at start)
        let title_raw = if caps.get(0).unwrap().start() == 0 {
            // Match at beginning: 1x03.Show.Name -> extract "Show Name"
            let after_match = &stem[caps.get(0).unwrap().end()..];
            after_match.trim_start_matches(|c: char| c == '.' || c == ' ' || c == '_' || c == '-')
        } else {
            // Match in middle: Show.Name.1x03.Title -> extract before 1x03
            &stem[..caps.get(0).unwrap().start()]
        };

        // Remove junk from the extracted title
        let without_junk = RE_JUNK.replace(title_raw, "");
        let year = extract_year(stem);

        // Also remove year from title if found
        let title_without_year = if let Some(y) = year {
            without_junk
                .replace(&format!("({})", y), "")
                .replace(&format!(".{}", y), "")
                .replace(&format!(" {}", y), "")
        } else {
            without_junk.into_owned()
        };

        return ParsedName {
            title: clean_title(&title_without_year),
            year,
            season: Some(season),
            episodes: vec![episode],
        };
    }

    // 3. Movie: strip junk tokens, then strip year
    let without_junk = RE_JUNK.replace(stem, "");
    let year = extract_year(&without_junk);

    // Remove the year from the title string too
    let title_raw = if let Some(y) = year {
        without_junk
            .replace(&format!("({})", y), "")
            .replace(&format!(".{}", y), "")
            .replace(&format!(" {}", y), "")
    } else {
        without_junk.into_owned()
    };

    ParsedName {
        title: clean_title(&title_raw),
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

/// Replace dots/underscores with spaces and trim.
fn clean_title(raw: &str) -> String {
    raw.replace(['.', '_'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string()
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
    fn sxx_exx() {
        let r = p("Breaking.Bad.S03E07.One.Minute.1080p.BluRay.mkv");
        assert_eq!(r.title, "Breaking Bad");
        assert_eq!(r.season, Some(3));
        assert_eq!(r.episodes, vec![7]);
    }

    #[test]
    fn multi_episode() {
        let r = p("The.Office.S02E01E02.720p.mkv");
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
