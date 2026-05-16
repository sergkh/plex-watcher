use std::time::Duration;
use tokio::time::sleep;
use tracing::{info, warn, error};

pub async fn notify_plex(
    plex_url: &str,
    plex_token: &str,
    library_ids: &[String],
    client: &reqwest::Client,
) {
    if plex_token.is_empty() {
        warn!("PLEX_TOKEN not set — skipping Plex notification");
        return;
    }

    let result = if library_ids.is_empty() {
        refresh_all(plex_url, plex_token, client).await
    } else {
        refresh_ids(plex_url, plex_token, library_ids, client).await
    };

    if let Err(e) = result {
        error!("Plex notification failed: {e:#}");
    }
}

async fn refresh_all(url: &str, token: &str, client: &reqwest::Client) -> anyhow::Result<()> {
    #[derive(serde::Deserialize)]
    struct Dir { key: String, title: String }
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "PascalCase")]
    struct MC { directory: Vec<Dir> }
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "PascalCase")]
    struct Root { media_container: MC }

    let resp: Root = client
        .get(format!("{url}/library/sections"))
        .header("X-Plex-Token", token)
        .header("Accept", "application/json")
        .send().await?.json().await?;

    let ids: Vec<String> = resp.media_container.directory.iter()
        .inspect(|d| info!("Found section {} ({})", d.key, d.title))
        .map(|d| d.key.clone())
        .collect();

    refresh_ids(url, token, &ids, client).await
}

async fn refresh_ids(
    url: &str,
    token: &str,
    ids: &[String],
    client: &reqwest::Client,
) -> anyhow::Result<()> {
    for id in ids {
        fetch_with_retry(client, &format!("{url}/library/sections/{id}/refresh"), token, 3).await?;
        info!("Plex section {id} refresh triggered");
    }
    Ok(())
}

async fn fetch_with_retry(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    retries: u32,
) -> anyhow::Result<()> {
    let mut last_err = anyhow::anyhow!("no attempts");
    for attempt in 1..=retries {
        match client.get(url).header("X-Plex-Token", token).send().await {
            Ok(r) if r.status().is_success() => return Ok(()),
            Ok(r) => last_err = anyhow::anyhow!("HTTP {}", r.status()),
            Err(e) => last_err = e.into(),
        }
        if attempt < retries {
            warn!("Plex request failed ({attempt}/{retries}): {last_err}. Retrying in 2s...");
            sleep(Duration::from_secs(2)).await;
        }
    }
    Err(last_err)
}
