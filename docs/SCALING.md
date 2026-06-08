# Scaling Pokémon Red PVP

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

## Everything through one TLS origin (coordinator proxy)

Production runs the **coordinator** behind a single TLS domain, not `--solo`. The coordinator owns no
emulator, so for a matched player it **bridges the battle through itself** instead of redirecting the
browser to the worker's port (which isn't exposed behind TLS):

- **Battle WebSocket** — `GET /ws?room=<uuid>` on the coordinator is proxied to that room's worker
  (`ws.rs::proxy_ws`), forwarding the caller's `?token=` and/or session cookie (workers inherit
  `COOKIE_SECRET`, so a forwarded cookie authenticates). Lobby/matchmaking `/ws` (no `?room=`) is still
  handled locally.
- **WebRTC** — `/offer?room=<uuid>` is proxied to the worker (`WorkerPool::proxy_offer`); the SDP answer
  carries the worker's ICE candidates so media still flows browser↔worker directly (node public IP via
  `hostNetwork`). Only the signaling crosses the coordinator.
- The coordinator's `/api/me` and `/api/room/{id}` read live battles from the **pool** (the local rooms
  map is empty there) so the same-origin room page knows the player is in their match.

`/room` serves the page same-origin everywhere (no redirect). This is what lets many battles run behind
one cert. (`coordinator.rs`, `ws.rs`, `signaling.rs::{room_page_handler,offer_handler}`.)

## Per-battle CPU: encode only when watched + a realtime encoder

The emulator is cheap (a Game Boy at ~59.7 fps is trivial); the per-battle CPU was almost entirely the
**VP8 video path**. Two fixes took a *watched* battle from ~1.2 cores to ~0.15 and let dozens run on one
16-core node:

1. **Encode only while a battle has a viewer.** The RGB565→I420→VP8 pipeline (and Opus) is skipped
   entirely when `video_tx.receiver_count() == 0` (`pipeline.rs`, gated by `watching`). An unwatched
   battle costs just emulation; encoding resumes with a keyframe (via `keyframe_req`) the instant a
   viewer/TV connects. The TV wall paginates, so only the visible cells ever encode.

2. **A realtime VP8 preset.** The `vpx-encode 0.6.2` crate hard-codes, for VP8, `cpu_used = 0` (libvpx's
   SLOWEST/best-quality setting — roughly a core per 60 fps stream) and `g_threads = 8` (8 encoder
   threads for a tiny 160×144 frame — and that libvpx thread pool **spins even when idle**, so dozens of
   *unwatched* workers melted the box for nothing). We vendor the crate (`vendor/vpx-encode` +
   `[patch.crates-io]`) with `cpu_used = 12` (realtime) and `g_threads = 1`, and drop the bitrate to
   1000 kbps (3500 was for 640×240 N64).

**Measured on the 16-core prod node:** 12 *watched* battles 14 cores → ~1.9; ~25 concurrent battles
load ~56 → **~5** (those idle 8-thread encoder pools were the mysterious load that made the box choke
with the CPU otherwise near-idle). Practical knob: set **`MAX_WORKERS`** to the node's emulator budget —
budget ~1 core per *peak-watched* battle; unwatched battles are nearly free.

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
- **Docker + WebRTC** — run the container with **`docker run --network host`** (not `-p`). WebRTC
  advertises the server's ICE candidates, and under Docker's bridge network those are the container's
  internal `127.0.0.1`/`172.x` addresses, unreachable from the host browser → the UI loads but no
  video. Host networking makes the candidates reachable (verified on OrbStack: TV video flows). The
  stricter alternative is `set_nat_1to1_ips(<public IP>)` in the WebRTC SettingEngine + publishing a
  fixed UDP port range. (HTTP/agents work either way; only the WebRTC media needs this.)
- **TURN + TLS** — the server binds `127.0.0.1`. A public deployment needs a TLS reverse proxy (the
  `/mcp` bearer token and WebRTC signaling must be HTTPS) and a TURN server for players behind
  symmetric NAT (`STUN_URLS` already accepts TURN entries). Also set the streamable-HTTP MCP
  `allowed_hosts` for the real domain.
