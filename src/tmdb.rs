//! TMDB (The Movie Database) API client.
//!
//! Requires a free API key from https://www.themoviedb.org/settings/api
//! Set via the TMDB_API_KEY environment variable.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
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
        tmdb_id: u64,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct MediaSearchResult {
    pub media_type: String,
    pub title: String,
    pub year: Option<u16>,
    pub tmdb_id: u64,
    pub tmdb_url: String,
    pub imdb_id: Option<String>,
    pub imdb_url: Option<String>,
    pub overview: Option<String>,
    pub poster_url: Option<String>,
    pub vote_average: Option<f32>,
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

#[derive(Deserialize)]
struct SearchMultiResp {
    results: Vec<MultiResult>,
}

#[derive(Deserialize)]
struct MultiResult {
    id: u64,
    media_type: String,
    title: Option<String>,
    name: Option<String>,
    release_date: Option<String>,
    first_air_date: Option<String>,
    overview: Option<String>,
    poster_path: Option<String>,
    vote_average: Option<f32>,
}

#[derive(Deserialize)]
struct ExternalIdsResp {
    imdb_id: Option<String>,
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
) -> Result<MediaInfo> {
    if season.is_some() {
        // Looks like a TV episode — search TV first
        if let Ok(info) = search_tv(client, api_key, title, year, season.unwrap()).await {
            return Ok(info);
        }
    }

    // Try movie
    if let Ok(info) = search_movie(client, api_key, title, year).await {
        return Ok(info);
    }

    // Fallback: if it had season info, try TV even without a year
    if let Some(s) = season {
        return search_tv(client, api_key, title, None, s).await;
    }

    bail!("No TMDB match found for '{title}'");
}

pub async fn search_media(
    client: &reqwest::Client,
    api_key: &str,
    query: &str,
) -> Result<Vec<MediaSearchResult>> {
    let url = format!(
        "{BASE}/search/multi?api_key={api_key}&query={}&language=en-US&page=1&include_adult=false",
        urlencoding(query)
    );

    debug!("TMDB web media search: {query}");
    let resp: SearchMultiResp = client
        .get(&url)
        .send()
        .await?
        .json()
        .await
        .context("parse TMDB multi search response")?;

    let mut media = Vec::new();
    for hit in resp
        .results
        .into_iter()
        .filter(|hit| hit.media_type == "movie" || hit.media_type == "tv")
        .take(10)
    {
        let title = hit.title.or(hit.name).unwrap_or_else(|| "Untitled".into());
        let date = hit.release_date.or(hit.first_air_date);
        let year = date
            .as_deref()
            .and_then(|d| d.get(..4))
            .and_then(|y| y.parse().ok());
        let poster_url = hit
            .poster_path
            .as_ref()
            .map(|path| format!("https://image.tmdb.org/t/p/w342{path}"));
        let imdb_id = imdb_id_for_media(client, api_key, &hit.media_type, hit.id)
            .await
            .ok()
            .flatten();
        let imdb_url = imdb_id
            .as_ref()
            .map(|id| format!("https://www.imdb.com/title/{id}/"));
        let tmdb_url = format!(
            "https://www.themoviedb.org/{}/{}",
            if hit.media_type == "tv" {
                "tv"
            } else {
                "movie"
            },
            hit.id
        );

        media.push(MediaSearchResult {
            media_type: hit.media_type,
            title,
            year,
            tmdb_id: hit.id,
            tmdb_url,
            imdb_id,
            imdb_url,
            overview: hit.overview.filter(|s| !s.is_empty()),
            poster_url,
            vote_average: hit.vote_average,
        });
    }

    Ok(media)
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
    let resp: SearchMovieResp = client
        .get(&url)
        .send()
        .await?
        .json()
        .await
        .context("parse TMDB movie response")?;

    let hit = resp
        .results
        .into_iter()
        .next()
        .context("no movie results")?;

    let release_year = hit
        .release_date
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
) -> Result<MediaInfo> {
    let mut url = format!(
        "{BASE}/search/tv?api_key={api_key}&query={}&language=en-US&page=1",
        urlencoding(title)
    );
    if let Some(y) = year {
        url.push_str(&format!("&first_air_date_year={y}"));
    }

    debug!("TMDB TV search: {title} ({year:?})");
    let resp: SearchTvResp = client
        .get(&url)
        .send()
        .await?
        .json()
        .await
        .context("parse TMDB TV response")?;

    let hit = resp.results.into_iter().next().context("no TV results")?;

    let air_year = hit
        .first_air_date
        .as_deref()
        .and_then(|d| d.get(..4))
        .and_then(|y| y.parse().ok())
        .unwrap_or(year.unwrap_or(0));

    info!(
        "TMDB TV match: '{}' ({}) S{:02}",
        hit.name, air_year, season
    );

    Ok(MediaInfo::Episode {
        show_title: hit.name,
        show_year: air_year,
        season,
        tmdb_id: hit.id,
    })
}

async fn imdb_id_for_media(
    client: &reqwest::Client,
    api_key: &str,
    media_type: &str,
    media_id: u64,
) -> Result<Option<String>> {
    let url = format!("{BASE}/{media_type}/{media_id}/external_ids?api_key={api_key}");
    let resp: ExternalIdsResp = client
        .get(&url)
        .send()
        .await?
        .json()
        .await
        .context("parse TMDB movie external IDs response")?;

    Ok(resp.imdb_id.filter(|id| !id.is_empty()))
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
