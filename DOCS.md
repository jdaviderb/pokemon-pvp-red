# Pokémon Red PVP — documentation index

All docs for the project, in one place. (Pokémon Red on Game Boy, streamed over WebRTC, with a PvP
arena + AI agents.)

## Start here

| Doc | What it covers |
|---|---|
| [README.md](README.md) | What the project is, quick start, Docker, features, controls |
| [CLAUDE.md](CLAUDE.md) | Working in this repo: build & run, architecture, file map, non-obvious gotchas |

## Guides (`docs/`)

| Doc | What it covers |
|---|---|
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | Full project guide: components, the emulator pipeline, cores, HTTP API, build, extending |
| [docs/battle-arena.md](docs/battle-arena.md) | The Pokémon Red battle arena: reading WRAM, injecting party/HP, the input macro |
| [docs/multiplayer.md](docs/multiplayer.md) | The 2-player online arena: DB / auth / rooms / WebSocket, the match flow |
| [docs/mcp.md](docs/mcp.md) | The MCP server — let an AI agent play (remote HTTP + token, or stdio); tools + setup |
| [docs/connect-agents.md](docs/connect-agents.md) | Connect popular agents (Claude Code/Desktop, Cursor, Codex CLI, ChatGPT, VS Code) to the arena MCP |
| [docs/SCALING.md](docs/SCALING.md) | What's done for scale, the remaining code wins, and the infra path (Postgres / SFU / TURN); Docker + WebRTC notes |
| [docs/pokemon-red-ram-map.md](docs/pokemon-red-ram-map.md) | Gen-1 Pokémon Red WRAM map (battle state, party, HP — big-endian) |

## Design records (verified)

| Doc | What it covers |
|---|---|
| [DESIGN-GB.md](DESIGN-GB.md) | Game Boy / gambatte integration (RGB565, GBC colorization, savestates) |
| [DESIGN-BATTLE.md](DESIGN-BATTLE.md) | The AI battle arena: state model, the menu-driver `TapMachine`, move injection |
| [DESIGN-MULTIPLAYER.md](DESIGN-MULTIPLAYER.md) | The 2-player online game on top of the arena: rooms FSM, turn engine |
