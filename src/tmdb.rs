//! TMDB (The Movie Database) API client.
//!
//! Requires a free API key from https://www.themoviedb.org/settings/api
//! Set via the TMDB_API_KEY environment variable.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use tracing::{debug, info};

const BASE: &str = "https://api.themoviedb.org/3";

// ── public types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum MediaInfo {
    Movie {
        title: String,
        year: u16,
        tmdb_id: u64,
    },
    Episode {
        show_title: String,
        show_year: u16,
        season: u16,
        episodes: Vec<u16>,
        tmdb_id: u64,
    },
}

// ── TMDB response shapes ──────────────────────────────────────────────────────

#[derive(Deserialize)]
struct MovieResult {
    id: u64,
    title: String,
    release_date: Option<String>, // "YYYY-MM-DD"
}

#[derive(Deserialize)]
struct TvResult {
    id: u64,
    name: String,
    first_air_date: Option<String>,
}

#[derive(Deserialize)]
struct SearchMovieResp {
    results: Vec<MovieResult>,
}

#[derive(Deserialize)]
struct SearchTvResp {
    results: Vec<TvResult>,
}

// ── public API ────────────────────────────────────────────────────────────────

/// Look up a media file using parsed metadata from the filename.
///
/// For episodes  → searches TV first, falls back to movie.
/// For movies    → searches movies first, falls back to TV.
pub async fn lookup(
    client: &reqwest::Client,
    api_key: &str,
    title: &str,
    year: Option<u16>,
    season: Option<u16>,
    episodes: &[u16],
) -> Result<MediaInfo> {
    if season.is_some() {
        // Looks like a TV episode — search TV first
        if let Ok(info) = search_tv(client, api_key, title, year, season.unwrap(), episodes).await {
            return Ok(info);
        }
    }

    // Try movie
    if let Ok(info) = search_movie(client, api_key, title, year).await {
        return Ok(info);
    }

    // Fallback: if it had season info, try TV even without a year
    if let Some(s) = season {
        return search_tv(client, api_key, title, None, s, episodes).await;
    }

    bail!("No TMDB match found for '{title}'");
}

// ── private helpers ───────────────────────────────────────────────────────────

async fn search_movie(
    client: &reqwest::Client,
    api_key: &str,
    title: &str,
    year: Option<u16>,
) -> Result<MediaInfo> {
    let mut url = format!(
        "{BASE}/search/movie?api_key={api_key}&query={}&language=en-US&page=1",
        urlencoding(title)
    );
    if let Some(y) = year {
        url.push_str(&format!("&year={y}"));
    }

    debug!("TMDB movie search: {title} ({year:?})");
    let resp: SearchMovieResp = client.get(&url).send().await?.json().await
        .context("parse TMDB movie response")?;

    let hit = resp.results.into_iter().next()
        .context("no movie results")?;

    let release_year = hit.release_date
        .as_deref()
        .and_then(|d| d.get(..4))
        .and_then(|y| y.parse().ok())
        .unwrap_or(year.unwrap_or(0));

    info!("TMDB movie match: '{}' ({})", hit.title, release_year);
    Ok(MediaInfo::Movie {
        title: hit.title,
        year: release_year,
        tmdb_id: hit.id,
    })
}

async fn search_tv(
    client: &reqwest::Client,
    api_key: &str,
    title: &str,
    year: Option<u16>,
    season: u16,
    episodes: &[u16],
) -> Result<MediaInfo> {
    let mut url = format!(
        "{BASE}/search/tv?api_key={api_key}&query={}&language=en-US&page=1",
        urlencoding(title)
    );
    if let Some(y) = year {
        url.push_str(&format!("&first_air_date_year={y}"));
    }

    debug!("TMDB TV search: {title} ({year:?})");
    let resp: SearchTvResp = client.get(&url).send().await?.json().await
        .context("parse TMDB TV response")?;

    let hit = resp.results.into_iter().next()
        .context("no TV results")?;

    let air_year = hit.first_air_date
        .as_deref()
        .and_then(|d| d.get(..4))
        .and_then(|y| y.parse().ok())
        .unwrap_or(year.unwrap_or(0));

    info!("TMDB TV match: '{}' ({}) S{:02}", hit.name, air_year, season);
    Ok(MediaInfo::Episode {
        show_title: hit.name,
        show_year: air_year,
        season,
        episodes: episodes.to_vec(),
        tmdb_id: hit.id,
    })
}

fn urlencoding(s: &str) -> String {
    // Minimal percent-encoding for query values (spaces → %20, etc.)
    s.chars()
        .flat_map(|c| match c {
            ' ' => "%20".chars().collect::<Vec<_>>(),
            '&' => "%26".chars().collect(),
            '+' => "%2B".chars().collect(),
            '#' => "%23".chars().collect(),
            c => vec![c],
        })
        .collect()
}
