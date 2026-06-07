# Scaling nes-web

The arena is built to scale **horizontally by worker process**: one libretro emulator = one process =
one battle (libretro globals are per-process), so concurrency is just the worker count. The default
mode is the **coordinator** (`cargo run --release`): it owns auth/lobby/matchmaking + a pool of
ephemeral worker processes (one per battle, spawned on demand, reaped on end), capped by
`MAX_WORKERS`/`--workers` (unbounded by default). `--solo` is the single-process fallback.

This doc tracks what makes it scale and what's left.

## Done

Per-machine bottlenecks fixed (the things that fail under load are in the coordinator's HTTP/state
layer, not the emulator):

- **Ephemeral worker pool** — N concurrent battles; zombie-reaped; concurrent reaper; crash → players
  notified. (`coordinator.rs`)
- **WebRTC lifecycle** — PeerConnections closed on disconnect/timeout (no leak), `SPECTATOR_MAX`
  admission, `STUN_URLS` hook, video broadcast buffer 240 + keyframe-on-lag. (`webrtc.rs`)
- **DB** — per-process small pools, migrate + WAL only on coordinator/solo, plus **indexes** on
  `matches(ended_at)` and `matches(p1_user,winner_seat)` / `(p2_user,winner_seat)` so the ranking job
  and `/api/collection` use indexes instead of full scans. (`db.rs`, `ranking.rs`, migration 007)
- **Hot HTTP paths off the request path:**
  - `/api/online` — O(1) insert + `len()`; the O(N) eviction runs in a background sweeper
    (`rooms::spawn_presence_sweeper`). Keyed by account (logged-in) or browser cookie (anon), so tabs/
    refreshes don't inflate it.
  - `/api/live` — a pre-serialized snapshot (`WorkerPool.live_cache`, refreshed on battle start/end),
    read lock-free; no contention with the worker-lifecycle lock that every TV poll used to hit.
  - `/api/ranking` — cached in memory + file by a background job (`ranking.rs`).
  - `/api/config` — flags cached in memory (`GameState.flags_cache`, refreshed ~30s); zero DB reads
    per page load.

Rough ceiling after these: a single coordinator handles ~thousands of concurrent battles and
hundreds of thousands of pollers; the emulators are the only heavy CPU and they're separate
processes you add machines for.

## Quick code wins still open (in-repo, no infra)

- **WorkerPool `Vec` → `HashMap<port, Worker>`** (+ a `public_id → port` index): `worker_for` /
  `set_battle` / `kill` are O(N) scans today; O(1) at thousands of workers. (`coordinator.rs`)
- **Event-driven matchmaker** — replace the 250ms poll loop with a `tokio::sync::Notify` fired by
  `find_match`, removing the 0–250ms pairing-latency floor. (`coordinator.rs` + `rooms.rs`)
- **Prewarmed worker pool** — keep K idle booted workers so match-start skips the ~3–5s emulator boot;
  assign from the warm set, top it back up in the background. Biggest match-latency win at scale.

## Infra (deploy / architecture — out of repo)

- **Postgres + a pooler (PgBouncer)** — set `DATABASE_URL`; the code already targets Postgres (SeaORM,
  no change). sqlite is single-writer = local/dev only.
- **Multi-coordinator HA** — the coordinator holds the queue/presence/WsHub/worker-pool in memory, so
  it's one process / SPOF. To run several behind a load balancer, move the **matchmaking queue and
  presence to Redis** and make worker ownership shared (or shard players by coordinator). Sticky
  sessions for the WS.
- **SFU for spectators** — the per-battle broadcast fan-out is fine for moderate audiences; thousands
  of viewers on one battle need an SFU (e.g. relay the VP8/Opus to a media server) instead of one
  PeerConnection per viewer on the worker.
- **TURN + TLS** — the server binds `127.0.0.1`. A public deployment needs a TLS reverse proxy (the
  `/mcp` bearer token and WebRTC signaling must be HTTPS) and a TURN server for players behind
  symmetric NAT (`STUN_URLS` already accepts TURN entries). Also set the streamable-HTTP MCP
  `allowed_hosts` for the real domain.
