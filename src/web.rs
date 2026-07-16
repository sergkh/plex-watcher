use crate::{config::AppConfig, logs, processor, prowlarr, qbittorrent, tmdb};
use anyhow::{Context, Result, anyhow};
use axum::{
    Json, Router,
    extract::{Query, State},
    http::StatusCode,
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use std::{fs, net::SocketAddr, path::Path, sync::Arc, time::UNIX_EPOCH};

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
    tmdb_title: Option<String>,
    tmdb_id: Option<u64>,
    tmdb_year: Option<u16>,
    autoupdate: Option<bool>,
}

#[derive(Deserialize)]
struct RenameWatchInput {
    path: String,
    new_name: String,
}

#[derive(Serialize)]
struct WatchEntry {
    path: String,
    name: String,
    kind: String,
    size_bytes: Option<u64>,
    modified_ms: Option<u128>,
}

pub async fn serve(cfg: Arc<AppConfig>, http: reqwest::Client) -> Result<()> {
    let addr: SocketAddr = cfg
        .web_addr
        .parse()
        .with_context(|| format!("parse WEB_ADDR '{}'", cfg.web_addr))?;

    let qbittorrent = if let Some(config) = cfg.qbittorrent.clone() {
        let client = Arc::new(qbittorrent::QBittorrentClient::new(config, http.clone()));
        if let Err(e) = client.validate_credentials().await {
            tracing::warn!(
                "qBittorrent validation failed at startup; integration remains enabled: {e:#}"
            );
        }
        Some(client)
    } else {
        None
    };
    let prowlarr = if let Some(config) = cfg.prowlarr.clone() {
        let client = Arc::new(prowlarr::ProwlarrClient::new(config, http.clone()));
        if let Err(e) = client.validate_credentials().await {
            tracing::warn!(
                "Prowlarr validation failed at startup; integration remains enabled: {e:#}"
            );
        }
        Some(client)
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
        .route("/api/watch", get(list_watch_dir))
        .route("/api/watch/rescan", post(rescan_watch_dir))
        .route("/api/watch/rename", post(rename_watch_entry))
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
    let client = state
        .qbittorrent
        .as_ref()
        .context("qBittorrent is not configured")?;
    let results = client.list(params.id.as_deref()).await?;
    Ok(Json(results))
}

async fn search_torrents(
    State(state): State<WebState>,
    Query(params): Query<ProwlarrSearchParams>,
) -> Result<Json<Vec<prowlarr::SearchResult>>, WebError> {
    let client = state
        .prowlarr
        .as_ref()
        .context("Prowlarr is not configured")?;
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
    let client = state
        .qbittorrent
        .as_ref()
        .context("qBittorrent is not configured")?;
    let source = input.magnet_uri.trim();
    if source.is_empty() {
        return Err(anyhow!("magnet_uri is required").into());
    }

    let download_name = tmdb_download_name(&input);
    let save_path = download_name.as_ref().map(|name| {
        state
            .cfg
            .watch_dir
            .join(name)
            .to_string_lossy()
            .into_owned()
    });
    let tags = if input.autoupdate.unwrap_or(false) {
        vec!["autoupdate"]
    } else {
        Vec::new()
    };
    let results = client
        .add_magnet_with_name(
            source,
            download_name.as_deref(),
            save_path.as_deref(),
            &tags,
        )
        .await?;
    Ok(Json(results))
}

async fn list_logs() -> Json<Vec<logs::LogEntry>> {
    Json(logs::entries())
}

async fn list_watch_dir(State(state): State<WebState>) -> Result<Json<Vec<WatchEntry>>, WebError> {
    let mut entries = top_level_watch_entries(&state.cfg.watch_dir)?;
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(Json(entries))
}

async fn rescan_watch_dir(
    State(state): State<WebState>,
) -> Result<Json<processor::RescanResult>, WebError> {
    Ok(Json(
        processor::rescan_watch_dir(&state.cfg, &state.http).await?,
    ))
}

async fn rename_watch_entry(
    State(state): State<WebState>,
    Json(input): Json<RenameWatchInput>,
) -> Result<Json<Vec<WatchEntry>>, WebError> {
    rename_top_level_watch_entry(&state.cfg.watch_dir, &input)?;
    let mut entries = top_level_watch_entries(&state.cfg.watch_dir)?;
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(Json(entries))
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

        (status, self.0.to_string()).into_response()
    }
}

const INDEX_HTML: &str = include_str!("web/index.html");

fn tmdb_download_name(input: &AddTorrentInput) -> Option<String> {
    let title = input.tmdb_title.as_deref()?.trim();
    let tmdb_id = input.tmdb_id?;
    if title.is_empty() {
        return None;
    }

    let mut name = sanitize_path_segment(title);
    if let Some(year) = input.tmdb_year {
        name.push_str(&format!(" ({year})"));
    }
    name.push_str(&format!(" [tmdb-{tmdb_id}]"));
    Some(name)
}

fn sanitize_path_segment(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|ch| match ch {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => ' ',
            ch if ch.is_control() => ' ',
            ch => ch,
        })
        .collect::<String>();

    sanitized.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn top_level_watch_entries(root: &Path) -> Result<Vec<WatchEntry>> {
    const MAX_ENTRIES: usize = 2_000;
    let mut entries = Vec::new();

    let read_dir = fs::read_dir(root).with_context(|| format!("read {}", root.display()))?;
    for entry in read_dir {
        if entries.len() >= MAX_ENTRIES {
            break;
        }

        let entry = entry.with_context(|| format!("read entry in {}", root.display()))?;
        let path = entry.path();
        let metadata = entry
            .metadata()
            .with_context(|| format!("read metadata for {}", path.display()))?;
        let is_dir = metadata.is_dir();
        let relative = path.strip_prefix(root).unwrap_or(&path);

        entries.push(WatchEntry {
            path: relative.display().to_string(),
            name: entry.file_name().to_string_lossy().to_string(),
            kind: if is_dir { "folder" } else { "file" }.to_string(),
            size_bytes: metadata.is_file().then_some(metadata.len()),
            modified_ms: metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map(|duration| duration.as_millis()),
        });
    }

    Ok(entries)
}

fn rename_top_level_watch_entry(root: &Path, input: &RenameWatchInput) -> Result<()> {
    let current_name = input.path.trim();
    let new_name = input.new_name.trim();

    validate_top_level_name(current_name).context("invalid current path")?;
    validate_top_level_name(new_name).context("invalid new name")?;

    if current_name == new_name {
        return Ok(());
    }

    let source = root.join(current_name);
    let destination = root.join(new_name);

    if !source.exists() {
        return Err(anyhow!("watched item does not exist: {current_name}"));
    }
    if destination.exists() {
        return Err(anyhow!("destination already exists: {new_name}"));
    }

    fs::rename(&source, &destination)
        .with_context(|| format!("rename {} to {}", source.display(), destination.display()))
}

fn validate_top_level_name(value: &str) -> Result<()> {
    if value.is_empty() {
        return Err(anyhow!("name is required"));
    }

    let path = Path::new(value);
    if path.is_absolute()
        || path.components().count() != 1
        || value == "."
        || value == ".."
        || value.contains('/')
        || value.contains('\\')
        || value.chars().any(char::is_control)
    {
        return Err(anyhow!("name must be a single watched-folder item"));
    }

    Ok(())
}
