use crate::config::QBittorrentConfig;
use anyhow::{Context, Result, bail};
use once_cell::sync::Lazy;
use regex::Regex;
use reqwest::{
    StatusCode,
    header::{CONTENT_TYPE, COOKIE, SET_COOKIE},
};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::{info, warn};

static SID_COOKIE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(QBT_)?SID(_[0-9]{2,6})?=[^;]+").unwrap());

#[derive(Debug, Clone, Serialize)]
pub struct TorrentTask {
    pub id: String,
    pub name: String,
    pub progress_percent: f64,
    pub state: String,
    pub save_path: String,
}

#[derive(Deserialize)]
struct RawTorrent {
    hash: String,
    name: String,
    progress: f64,
    state: String,
    save_path: Option<String>,
}

pub struct QBittorrentClient {
    config: QBittorrentConfig,
    http: reqwest::Client,
    sid_cookie: Mutex<Option<String>>,
}

impl QBittorrentClient {
    pub fn new(config: QBittorrentConfig, http: reqwest::Client) -> Self {
        Self {
            config,
            http,
            sid_cookie: Mutex::new(None),
        }
    }

    pub async fn validate_credentials(&self) -> Result<()> {
        self.login().await
    }

    pub async fn add_magnet_with_name(
        &self,
        magnet_uri: &str,
        download_name: Option<&str>,
    ) -> Result<Vec<TorrentTask>> {
        self.add_magnet_once(magnet_uri, download_name, true).await
    }

    pub async fn list(&self, id: Option<&str>) -> Result<Vec<TorrentTask>> {
        self.list_once(id, true).await
    }

    async fn login(&self) -> Result<()> {
        let url = self.url("/api/v2/auth/login");
        let response = self
            .http
            .post(url)
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(format!(
                "username={}&password={}",
                urlencoding(&self.config.username),
                urlencoding(&self.config.password)
            ))
            .send()
            .await
            .context("send qBittorrent login request")?;

        let status = response.status();
        let sid_cookie = sid_cookie_from_response(&response);
        let body = response.text().await.unwrap_or_default();

        info!(
            "qBittorrent login cookie: {}",
            sid_cookie.as_deref().unwrap_or("none")
        );

        if !status.is_success() || sid_cookie.is_none() {
            warn!(
                "Failed to log in to qBittorrent. Status: {}, Result: {}, sid match: {}",
                status,
                body.trim(),
                sid_cookie.is_some()
            );
            bail!(
                "qBittorrent login failed. Check URL and credentials: {}, user: {}",
                self.config.base_url,
                self.config.username
            );
        }

        *self.sid_cookie.lock().await = sid_cookie;
        Ok(())
    }

    async fn add_magnet_once(
        &self,
        magnet_uri: &str,
        download_name: Option<&str>,
        can_retry: bool,
    ) -> Result<Vec<TorrentTask>> {
        let sid_cookie = self.sid_cookie().await?;
        let mut form = reqwest::multipart::Form::new().text("urls", magnet_uri.to_string());
        if let Some(name) = download_name.map(str::trim).filter(|name| !name.is_empty()) {
            form = form
                .text("rename", name.to_string())
                .text("contentLayout", "Subfolder");
        }

        let response = self
            .http
            .post(self.url("/api/v2/torrents/add"))
            .header(COOKIE, sid_cookie)
            .multipart(form)
            .send()
            .await
            .context("send qBittorrent add request")?;

        if response.status() == StatusCode::FORBIDDEN && can_retry {
            self.clear_sid().await;
            self.login().await?;
            return Box::pin(self.add_magnet_once(magnet_uri, download_name, false)).await;
        }

        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            bail!("qBittorrent add failed ({status}): {}", body.trim());
        }

        info!(
            "Downloading magnet link: {magnet_uri}{}",
            download_name
                .map(|name| format!(" as {name}"))
                .unwrap_or_default()
        );
        self.list(None).await
    }

    async fn list_once(&self, id: Option<&str>, can_retry: bool) -> Result<Vec<TorrentTask>> {
        let sid_cookie = self.sid_cookie().await?;
        let local_sid = sid_cookie.clone();
        let mut url = self.url("/api/v2/torrents/info");
        if let Some(id) = id.map(str::trim).filter(|id| !id.is_empty()) {
            url.push_str("?hashes=");
            url.push_str(&urlencoding(id));
        }

        let response = self
            .http
            .get(url)
            .header(COOKIE, sid_cookie)
            .send()
            .await
            .context("send qBittorrent list request")?;

        if response.status() == StatusCode::FORBIDDEN && can_retry {
            self.clear_sid().await;
            self.login().await?;
            return Box::pin(self.list_once(id, false)).await;
        }

        let status = response.status();
        if !status.is_success() {
            let details = response.text().await.unwrap_or_default();
            bail!("qBittorrent list failed ({status}): {details}. SID: {local_sid}");
        }

        let payload: Vec<RawTorrent> = response
            .json()
            .await
            .context("parse qBittorrent list response")?;

        Ok(payload
            .into_iter()
            .map(|item| TorrentTask {
                id: item.hash,
                name: item.name,
                progress_percent: (item.progress * 1000.0).round() / 10.0,
                state: item.state,
                save_path: item.save_path.unwrap_or_default(),
            })
            .collect())
    }

    async fn sid_cookie(&self) -> Result<String> {
        if let Some(cookie) = self.sid_cookie.lock().await.clone() {
            return Ok(cookie);
        }

        self.login().await?;
        self.sid_cookie
            .lock()
            .await
            .clone()
            .context("qBittorrent login did not return SID cookie")
    }

    async fn clear_sid(&self) {
        *self.sid_cookie.lock().await = None;
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.config.base_url, path)
    }
}

fn sid_cookie_from_response(response: &reqwest::Response) -> Option<String> {
    response
        .headers()
        .get_all(SET_COOKIE)
        .iter()
        .find_map(|value| {
            let value = value.to_str().ok()?;
            SID_COOKIE_RE.find(value).map(|m| m.as_str().to_string())
        })
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
