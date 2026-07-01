<p align="center">
  <img src="assets/icon-256.png" width="120" alt="qbittorrent" />
</p>

# qbittorrent

qBittorrent is a lightweight, ad-free BitTorrent client with a web UI.

A first-party [orca](https://github.com/argyle-labs/orca) plugin (service-backend).

This repo is **self-contained** — the steps below run qbittorrent **by hand, without orca**. orca automates exactly this (same image, ports, and data) through one generic surface.

---

## Run it without orca

### Docker / Podman

```yaml
# compose.yml
services:
  qbittorrent:
    image: lscr.io/linuxserver/qbittorrent:latest
    container_name: qbittorrent
    restart: unless-stopped
    ports:
      - "8080:8080/tcp"   # web UI
      - "6881:6881/tcp"   # torrent
      - "6881:6881/udp"   # torrent
    volumes:
      - ./config:/config
      - /path/to/downloads:/downloads
```

```sh
docker compose up -d
```

Podman: the same file with `podman-compose up -d`.

### Ports & data

| | |
|---|---|
| Default port | `8080` |
| Upstream | <https://www.qbittorrent.org/> |
| Operator notes | [qbittorrent.md](docs/qbittorrent.md) |


### Backup & restore

Back up the config/data volume(s) above — that's the whole service state (stop the container first for a clean copy). Restore by putting them back and starting it.

> With orca this is **`service.backup` / `service.restore`** — location-agnostic (docker / podman / lxc / vm), one command regardless of where qbittorrent runs. No per-service backup script.

## With orca

orca drives this plugin through the single generic `service.*` surface — no per-plugin tools:

```sh
orca service.deploy qbittorrent      # render + launch on any supported runtime
orca service.status qbittorrent      # health + rich diagnostics (typed payload)
orca service.backup qbittorrent      # location-agnostic backup (tar; PBS on Proxmox)
orca service.configure qbittorrent   # apply config via the upstream API
```

## Layout

- `src/` — the plugin (pure Rust): the `ServiceBackend` descriptor + `configure` / `status`.
- `docs/` — standalone operator notes.
- [CAPABILITIES.md](CAPABILITIES.md) — the service-backend contract checklist.
- `assets/` — plugin icon.
