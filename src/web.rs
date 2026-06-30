use crate::{config::AppConfig, logs, prowlarr, qbittorrent, tmdb};
use anyhow::{anyhow, Context, Result};
use axum::{
    Json, Router,
    extract::{Query, State},
    http::StatusCode,
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use std::{net::SocketAddr, sync::Arc};

#[derive(Clone)]
struct WebState {
    cfg: Arc<AppConfig>,
    http: reqwest::Client,
    qbittorrent: Option<Arc<qbittorrent::QBittorrentClient>>,
    prowlarr: Option<Arc<prowlarr::ProwlarrClient>>,
}

#[derive(Deserialize)]
struct SearchParams {
    q: String,
}

#[derive(Deserialize)]
struct TorrentParams {
    id: Option<String>,
}

#[derive(Deserialize)]
struct ProwlarrSearchParams {
    q: String,
}

#[derive(Deserialize)]
struct AddTorrentInput {
    magnet_uri: String,
}

pub async fn serve(cfg: Arc<AppConfig>, http: reqwest::Client) -> Result<()> {
    let addr: SocketAddr = cfg
        .web_addr
        .parse()
        .with_context(|| format!("parse WEB_ADDR '{}'", cfg.web_addr))?;

    let qbittorrent = if let Some(config) = cfg.qbittorrent.clone() {
        let client = Arc::new(qbittorrent::QBittorrentClient::new(config, http.clone()));
        match client.validate_credentials().await {
            Ok(()) => Some(client),
            Err(e) => {
                tracing::warn!("Disabling qBittorrent integration: {e:#}");
                None
            }
        }
    } else {
        None
    };
    let prowlarr = if let Some(config) = cfg.prowlarr.clone() {
        let client = Arc::new(prowlarr::ProwlarrClient::new(config, http.clone()));
        match client.validate_credentials().await {
            Ok(()) => Some(client),
            Err(e) => {
                tracing::warn!("Disabling Prowlarr integration: {e:#}");
                None
            }
        }
    } else {
        None
    };

    let app = Router::new()
        .route("/", get(index))
        .route("/api/search", get(search))
        .route("/api/prowlarr/search", get(search_torrents))
        .route("/api/torrents", get(list_torrents))
        .route("/api/torrents/add", post(add_torrent))
        .route("/api/logs", get(list_logs))
        .with_state(WebState {
            cfg,
            http,
            qbittorrent,
            prowlarr,
        });

    tracing::info!("Web UI listening on http://{addr}");
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("bind web UI to {addr}"))?;

    axum::serve(listener, app)
        .await
        .context("run web UI server")
}

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn search(
    State(state): State<WebState>,
    Query(params): Query<SearchParams>,
) -> Result<Json<Vec<tmdb::MediaSearchResult>>, WebError> {
    let query = params.q.trim();
    if query.is_empty() {
        return Ok(Json(Vec::new()));
    }

    let results = tmdb::search_media(&state.http, &state.cfg.tmdb_api_key, query).await?;
    Ok(Json(results))
}

async fn list_torrents(
    State(state): State<WebState>,
    Query(params): Query<TorrentParams>,
) -> Result<Json<Vec<qbittorrent::TorrentTask>>, WebError> {
    let client = state.qbittorrent.as_ref().context("qBittorrent is not configured")?;
    let results = client.list(params.id.as_deref()).await?;
    Ok(Json(results))
}

async fn search_torrents(
    State(state): State<WebState>,
    Query(params): Query<ProwlarrSearchParams>,
) -> Result<Json<Vec<prowlarr::SearchResult>>, WebError> {
    let client = state.prowlarr.as_ref().context("Prowlarr is not configured")?;
    let query = params.q.trim();
    if query.is_empty() {
        return Ok(Json(Vec::new()));
    }

    let results = client.search(query).await?;
    Ok(Json(results))
}

async fn add_torrent(
    State(state): State<WebState>,
    Json(input): Json<AddTorrentInput>,
) -> Result<Json<Vec<qbittorrent::TorrentTask>>, WebError> {
    let client = state.qbittorrent.as_ref().context("qBittorrent is not configured")?;
    let source = input.magnet_uri.trim();
    if source.is_empty() {
        return Err(anyhow!("magnet_uri is required").into());
    }

    let results = client.add_magnet(source).await?;
    Ok(Json(results))
}

async fn list_logs() -> Json<Vec<logs::LogEntry>> {
    Json(logs::entries())
}

struct WebError(anyhow::Error);

impl<E> From<E> for WebError
where
    E: Into<anyhow::Error>,
{
    fn from(err: E) -> Self {
        Self(err.into())
    }
}

impl IntoResponse for WebError {
    fn into_response(self) -> Response {
        tracing::warn!("Web request failed: {:#}", self.0);
        let status = if self.0.to_string().contains("qBittorrent is not configured") {
            StatusCode::SERVICE_UNAVAILABLE
        } else if self.0.to_string().contains("Prowlarr is not configured") {
            StatusCode::SERVICE_UNAVAILABLE
        } else {
            StatusCode::INTERNAL_SERVER_ERROR
        };

        (status, self.0.to_string())
            .into_response()
    }
}

const INDEX_HTML: &str = include_str!("web/index.html");
