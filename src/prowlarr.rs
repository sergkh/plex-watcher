use crate::config::ProwlarrConfig;
use anyhow::{bail, Context, Result};
use reqwest::header::{HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use tracing::info;

const MAX_RESULTS: usize = 20;
const API_KEY_HEADER: HeaderName = HeaderName::from_static("x-api-key");

#[derive(Debug, Clone, Serialize)]
pub struct SearchResult {
    pub title: String,
    pub indexer: String,
    pub seeders: u64,
    pub leechers: u64,
    pub size: String,
    pub magnet_uri: String,
}

#[derive(Deserialize)]
struct RawSearchResult {
    title: Option<String>,
    indexer: Option<String>,
    seeders: Option<u64>,
    leechers: Option<u64>,
    size: Option<u64>,
    #[serde(rename = "magnetUrl")]
    magnet_url: Option<String>,
    #[serde(rename = "downloadUrl")]
    download_url: Option<String>,
    guid: Option<String>,
}

pub struct ProwlarrClient {
    config: ProwlarrConfig,
    http: reqwest::Client,
}

impl ProwlarrClient {
    pub fn new(config: ProwlarrConfig, http: reqwest::Client) -> Self {
        Self { config, http }
    }

    pub async fn validate_credentials(&self) -> Result<()> {
        let response = self
            .http
            .get(self.url("/api/v1/system/status"))
            .header(API_KEY_HEADER, self.api_key_header()?)
            .send()
            .await
            .context("send Prowlarr status request")?;

        let status = response.status();
        if !status.is_success() {
            let details = response.text().await.unwrap_or_default();
            bail!("Prowlarr status failed ({status}): {details}");
        }

        Ok(())
    }

    pub async fn search(&self, query: &str) -> Result<Vec<SearchResult>> {
        let started_at = std::time::Instant::now();
        let mut url = format!(
            "{}?query={}&type=search",
            self.url("/api/v1/search"),
            urlencoding(query)
        );
        for id in &self.config.indexer_ids {
            url.push_str("&indexerIds=");
            url.push_str(&id.to_string());
        }

        let response = self
            .http
            .get(url)
            .header(API_KEY_HEADER, self.api_key_header()?)
            .send()
            .await
            .context("send Prowlarr search request")?;

        let status = response.status();
        if !status.is_success() {
            let details = response.text().await.unwrap_or_default();
            bail!("Prowlarr search failed ({status}): {details}");
        }

        let payload: Vec<RawSearchResult> = response
            .json()
            .await
            .context("parse Prowlarr search response")?;

        let mut results: Vec<SearchResult> = payload
            .into_iter()
            .filter_map(|item| {
                let source = item.magnet_url.or(item.download_url).or(item.guid)?;
                Some(SearchResult {
                    title: item
                        .title
                        .map(|title| title.trim().to_string())
                        .filter(|title| !title.is_empty())
                        .unwrap_or_else(|| "Untitled result".into()),
                    indexer: item
                        .indexer
                        .map(|indexer| indexer.trim().to_string())
                        .filter(|indexer| !indexer.is_empty())
                        .unwrap_or_else(|| "unknown-indexer".into()),
                    seeders: item.seeders.unwrap_or(0),
                    leechers: item.leechers.unwrap_or(0),
                    size: format_bytes(item.size),
                    magnet_uri: source,
                })
            })
            .collect();

        results.sort_by(|a, b| b.seeders.cmp(&a.seeders));
        results.truncate(MAX_RESULTS);

        info!(
            "Prowlarr search took {} ms. Results: {}",
            started_at.elapsed().as_millis(),
            results
                .iter()
                .map(|result| result.title.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );

        Ok(results)
    }

    fn api_key_header(&self) -> Result<HeaderValue> {
        HeaderValue::from_str(&self.config.api_key).context("build Prowlarr API key header")
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.config.base_url, path)
    }
}

fn format_bytes(size_in_bytes: Option<u64>) -> String {
    let Some(size) = size_in_bytes.filter(|size| *size > 0) else {
        return "Unknown".into();
    };

    let units = ["B", "KB", "MB", "GB", "TB"];
    let index = ((size as f64).log(1024.0).floor() as usize).min(units.len() - 1);
    let value = size as f64 / 1024_f64.powi(index as i32);
    let decimals = if value >= 10.0 || index == 0 { 0 } else { 1 };

    format!("{value:.decimals$} {}", units[index])
}

fn urlencoding(s: &str) -> String {
    let mut encoded = String::new();
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}
