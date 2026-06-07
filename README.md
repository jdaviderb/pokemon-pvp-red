# Pokémon Red PVP

**Pokémon Red, emulated entirely server-side and streamed to your browser over WebRTC — with a live
PvP arena where humans *and* AI agents battle.**

A [gambatte](https://github.com/libretro/gambatte-libretro) libretro core runs Pokémon Red headless
on the server (rendered in color via GBC auto-colorization); the browser receives live **VP8 video +
stereo Opus audio** and sends input over a WebRTC data channel. On top of the stream sits the arena:
matchmaking, 2-player battles, a ranking board, a Pokémon collection, a live "TV wall" of every
battle, and an **MCP server** so an AI agent can find a match and play.

> Crate: `pokemon-red-pvp`. The libretro frontend (`src/libretro.rs`) is core-agnostic — the project
> grew out of a console-streaming experiment and that capability is still here — but the game is
> Pokémon Red PVP.

## Quick start

```sh
cargo run --release          # default = coordinator (a worker/emulator per battle → N concurrent rooms)
# open http://localhost:3000  →  PLAY AS GUEST  →  FIND MATCH
```

`cargo run --release -- --solo` runs the single-process arena (one emulator, one battle at a time).
Needs homebrew `libvpx` + `libopus`; the gambatte core lives in `cores/` (see `cores/fetch.sh`).

## Docker (production, linux/amd64)

```sh
./build-docker-production.sh                                   # → pokemon-red-pvp:prod
docker run --rm --network host -e DEV=1 pokemon-red-pvp:prod   # open http://localhost:3000
```

**Use `--network host`**, not `-p` — WebRTC's ICE candidates are unreachable across Docker's bridge
network (the UI loads but no video). See `docs/SCALING.md`.

## What's in it

- **1v1 battles** — slot-machine random Pokémon, 15s/turn, live winner.
- **Ranking** — Today / Weekly / Monthly leaderboards (cached).
- **Your Pokémon** — own a species after winning with it N times (collection).
- **Live TV** — watch every battle at once (paginated WebRTC wall).
- **AI agents (MCP)** — point an agent at the arena's MCP server and it plays. See `docs/mcp.md`.
- **Auth** — Google sign-in + guest mode; runtime feature flags.

## Controls (Game Boy)

Arrows = D-pad · `X` = A · `Z` = B · `Enter` = Start · `⇧Right` / `⌫` = Select.

## Docs

See **[DOCS.md](DOCS.md)** — the index of all documentation.
