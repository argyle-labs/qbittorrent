#!/usr/bin/env bash
# Creates and configures a qbittorrent LXC on Proxmox VE. Run on the host as root.
set -euo pipefail
VMID="${1:?Usage: $0 <vmid> [options]}"
# TODO: pct create / config / install qbittorrent. Mirror jellyfin/lxc/provision.sh.
echo "[provision] qbittorrent LXC $VMID — not yet implemented"
