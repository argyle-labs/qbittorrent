# qBittorrent

BitTorrent client.

- **Host**: <host> — see [Network Map](../network/network-map.md)
- **Port**: 8070 (Web UI), 6881 (torrent)
- **Image**: `lscr.io/linuxserver/qbittorrent`
- **Compose**: [compose/qbittorrent/docker-compose.yml](../../compose/qbittorrent/docker-compose.yml)
- **Network**: `media`
- **VPN**: All traffic routes through <vpn-provider> region-a via OPNsense. Killswitch blocks internet if VPN is down.

## Deploy

Deployed as a Portainer Git stack pointing at the `main` branch. Portainer polls GitHub every 5 minutes and redeploys on changes.

To manually redeploy: Portainer → Stacks → qbittorrent → Pull and redeploy.

## Volumes

| Host Path | Container Path | Description |
|-----------|---------------|-------------|
| `/opt/appdata/qbittorrent` | `/config` | qBittorrent config |
| `/mnt/<host>/downloads` | `/downloads` | Downloads root (NFS) — `incomplete/` and `completed/` are subdirs |

NFS mounts on <host> are defined in `/etc/fstab` and managed by the `netmount` OpenRC service. See [nfs-alpine.md](../host-setup/nfs-alpine.md).

## Environment Variables

| Variable                    | Default          | Description               |
|-----------------------------|------------------|---------------------------|
| `TZ`                        | `Etc/UTC` | Timezone                  |
| `PUID` / `PGID`             | `1000`           | User/group ID             |
| `QBITTORRENT_IMAGE_TAG`     | `latest`         | Image tag                 |
| `QBITTORRENT_CONFIG_PATH`   | `./config`       | Config directory          |
| `DOWNLOADS_PATH`            | *(required)*     | Downloads root path (must contain `incomplete/` and `completed/` subdirs) |

## Troubleshooting

```bash
docker logs qbittorrent

# Check NFS mounts are active
mount | grep <host>

# Test write access
touch /mnt/<host>/downloads/completed/test && rm /mnt/<host>/downloads/completed/test
```
