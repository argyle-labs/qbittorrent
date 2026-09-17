# qBittorrent queue deadlock and torrent-liveness traps (operational KB)

Two operational traps that silently wedge downloads. Complements
[qbittorrent.md](qbittorrent.md).

## Queue-limit deadlock — global download speed stuck at 0

Symptom: global download speed is 0 with many torrents in `queuedDL`, but disk and
VPN are fine.

Root cause: the defaults `max_active_downloads = 3`, `max_active_torrents = 5`,
and `dont_count_slow_torrents = false`. Stalled torrents that make no progress
still occupy the active-download slots and never release them, so healthy queued
torrents — even large, well-seeded swarms — never start.

**Fix (live via the WebUI API, no restart needed):**

```
POST /api/v2/app/setPreferences
body: {"dont_count_slow_torrents": true, "max_active_downloads": 8, "max_active_torrents": 20}
```

In one incident this took global speed from 0 to ~19 MB/s instantly and started
several torrents. This is the durable config — keep it.

## Tracker-announce lag looks like "dead torrents" — do NOT cull on it

While a torrent is **queued** it has not announced yet, so `num_complete` and
`num_seeds` read 0. This looks dead but is not — once active it announces and
finds the swarm. In one sweep 17 torrents were flagged "dead"; after the
queue-limit fix above, most were actively downloading.

**Rules:**

- Never cull based on a 0-seed reading while a torrent is in a queued state.
- A torrent is *truly* dead only when `num_seeds == 0` **AND** `num_complete == 0`
  **AND** `num_leechs == 0` **while active/announced**.
- Give torrents with leechers-but-no-seeder 24–48h before culling.
- Only auto-cull exact-duplicate 0-seed twins of a copy that is actively
  downloading.

## VPN port-forward caveat (seeding only)

If the VPN provides no port forward (no forwarded inbound port, UPnP off), inbound
peers cannot reach you. This hurts **seeding/ratio**, not downloading. Fix it
later for ratio; it is never the cause of a download deadlock.
