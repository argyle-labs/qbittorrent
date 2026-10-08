<p align="center">
  <img src="assets/icon-256.png" width="120" alt="qbittorrent" />
</p>

# qbittorrent

qBittorrent is a lightweight, ad-free BitTorrent client with a web UI.

A first-party [orca](https://github.com/argyle-labs/orca) plugin (service-backend).

This repo is **self-contained** — the steps below run qbittorrent **by hand, without orca**. orca automates exactly this (same image, ports, and data) through one generic surface.

---

## Run it without orca

### Docker Compose

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

### Other runtimes

**Podman** — the compose above works with `podman compose up -d`, or run it directly:

```sh
podman run -d --name qbittorrent --restart unless-stopped \
    -p 8080:8080/tcp \
    -p 6881:6881/tcp \
    -p 6881:6881/udp \
    -v ./config:/config \
    -v /path/to/downloads:/downloads \
    lscr.io/linuxserver/qbittorrent:latest
```

**LXC** — on a container-capable LXC (e.g. a Proxmox LXC with nesting enabled) run the same image via Docker/Podman as above, or install qbittorrent from upstream directly on the guest: <https://www.qbittorrent.org/>.

**VM** — install qbittorrent from upstream (<https://www.qbittorrent.org/>) or run the same container image inside the VM; expose port `8080`.

**Unraid** — add via *Community Applications*, or *Docker → Add Container* with image `lscr.io/linuxserver/qbittorrent:latest`, port `8080`, and the volume paths above.

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

orca drives this plugin through the generic `service.*` surface:

```sh
orca service.deploy qbittorrent      # render + launch on any supported runtime
orca service.status qbittorrent      # health + rich diagnostics (typed payload)
orca service.backup qbittorrent      # location-agnostic backup (tar; PBS on Proxmox)
orca service.configure qbittorrent   # apply config via the upstream API
```

plus WebUI tools against a registered endpoint (`qbittorrent.create`):

```sh
orca opnsense.pia.forwarded_port --name gw                           # -> {"port": 51234, ...}
orca qbittorrent.listen_port.status --name dl --expected-port 51234
orca qbittorrent.listen_port.sync --name dl --port 51234            # dry run: reports drift
orca qbittorrent.listen_port.sync --name dl --port 51234 --execute
orca qbittorrent.listen_port.sync --name dl --mode listen --port-file /gluetun/forwarded_port --execute
```

`listen_port.sync` has two modes. It writes only on drift and treats `random_port`
as drift in both.

- `announce` (default): behind a fixed NAT mapping (`PIA-PF-port -> host:6881`)
  qBittorrent keeps listening on `6881` and the PIA port goes to `announce_port`,
  the port reported to trackers. Needs qBittorrent 5.1+ built against libtorrent
  2.0.11+ (older builds save the value but ignore it); it refuses otherwise.
  It only changes what trackers and DHT are told, not the actual listener or
  local service discovery.
- `listen`: when inbound traffic reaches the client unmapped, the PIA port
  becomes the listen port, `random_port` is turned off and `announce_port` is
  reset to `0`. Use it with gluetun's `forwarded_port` file: gluetun does not
  remap the port.

## Layout

- `src/` — the plugin (pure Rust): the `ServiceBackend` descriptor + `configure` / `status`.
- `docs/` — standalone operator notes.
- [CAPABILITIES.md](CAPABILITIES.md) — the service-backend contract checklist.
- `assets/` — plugin icon.
