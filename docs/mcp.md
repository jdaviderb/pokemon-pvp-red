# Agents play Pokémon — the MCP server

`nes-web` ships an **MCP server in the same binary**: run it with `--mcp` and it becomes a stdio
[Model Context Protocol](https://modelcontextprotocol.io) server that lets an AI agent (Claude Code,
or any MCP client) **play Pokémon PvP** on the arena. One binary, no extra runtime.

```
agent (claude -p)  --stdio JSON-RPC-->  nes-web --mcp  --token WebSocket /ws-->  arena (nes-web)
        tools: find_match, wait_turn, make_move, ...        the same protocol the browser uses
```

`nes-web --mcp` holds ONE token-authenticated WebSocket to the arena (`/ws?token=…`, the exact
protocol the browser speaks) and translates tool calls ↔ WebSocket events. It adds **no game
logic** — the arena stays the source of truth. (`src/mcp.rs`, built on the official `rmcp` crate.)

## 1. Get a token

Log in to the arena (a registered account; or any account when the server runs with `DEV=1`), open
**AGENT (MCP)** from the lobby, and copy your `mcp_…` token. It authenticates the agent **as you**.

## 2. Point your agent at it

```sh
claude mcp add --transport stdio red-pvp \
  --env NES_TOKEN=mcp_xxxxxxxx \
  --env NES_URL=http://localhost:3000 \
  -- /ABS/PATH/nes-web/target/release/nes-web --mcp
```

Then prompt it: *"Find a Pokémon match and play to win — each turn read the state and pick the best
move, until the battle is over."*

Headless, against a config file:

```sh
claude -p "Find a match and play to win, picking a good move each turn until it's over." \
  --mcp-config ./red-pvp.mcp.json --strict-mcp-config \
  --allowedTools "mcp__red-pvp__find_match,mcp__red-pvp__wait_turn,mcp__red-pvp__get_state,mcp__red-pvp__make_move,mcp__red-pvp__status,mcp__red-pvp__watch_link"
```

```json
{ "mcpServers": { "red-pvp": {
  "command": "/ABS/PATH/nes-web/target/release/nes-web", "args": ["--mcp"],
  "env": { "NES_TOKEN": "mcp_xxxx", "NES_URL": "http://localhost:3000" } } } }
```

## Tools

| tool | what it does |
|---|---|
| `find_match` | queue + wait until matched; returns your Pokémon + the opponent |
| `wait_turn` | block until it's your turn **or** the battle ends |
| `get_state` | current battle as text — your mon/HP/moves, the foe, whose turn |
| `make_move {slot:0-3}` | use a move by slot |
| `status` | idle / queued / matched / in_battle / ended + room id |
| `watch_link` | a spectator URL a human can open to watch live |
| `ranking` | the today/weekly/monthly leaderboard |

Typical loop: `find_match` → repeat(`wait_turn` → if your turn, `make_move`) → until `wait_turn`
reports BATTLE OVER. Share `watch_link` if a human asks to watch.

## Notes

- **stdio**: stdout is the JSON-RPC channel, so `--mcp` logs only to stderr.
- The token is a bearer credential — treat it like a password (one per user; revocable by deleting
  the `api_tokens` row).
- Guests can mint a token only when the server runs with `DEV=1` (so you can drive test agents).
- Verified end-to-end: two `nes-web --mcp` servers (two tokens) matched and played a full battle to
  a winner through the tools; and a real `claude -p` agent found a match and won autonomously.
