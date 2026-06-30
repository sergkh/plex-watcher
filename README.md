Plex watcher
---

A simple service for watching a downloads folder and linking media files into an appropriate media folder in Plex.

It also includes a small web UI at `WEB_ADDR` (default `0.0.0.0:8000`) for searching movies and TV shows with the configured TMDb API key and opening matched IMDb pages.

When `QBITTORRENT_URL`, `QBITTORRENT_USERNAME`, and `QBITTORRENT_PASSWORD` are set, the web UI can add magnet links and show current qBittorrent download status.

When `PROWLARR_URL` and `PROWLARR_API_KEY` are set, the web UI can search torrents through Prowlarr. Set `PROWLARR_INDEXER_IDS` to a comma-separated list to limit searches to specific indexers.

# Configuration

For configuration parameters see [docker-compose.yml](./docker-compose.yml).
