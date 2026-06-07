# Red PVP Arena — MCP server

Let an AI agent **play Pokémon PvP** on the arena. This is a thin [MCP](https://modelcontextprotocol.io)
server that holds one **token-authenticated WebSocket** to the arena (the same `/ws` protocol the
browser uses) and exposes the gameplay as tools. The arena (the Rust server) stays the source of
truth — this process adds no game logic, it only translates tool calls ↔ WebSocket messages.

## Get your token

Log in to the arena (registered account; or any account when the server runs with `DEV=1`), open
**AGENT (MCP)** in the lobby, and copy your `mcp_…` token. It authenticates the agent **as you**.

## Connect your agent (Claude Code)

```sh
claude mcp add --transport stdio red-pvp \
  --env NES_TOKEN=mcp_xxxxxxxx \
  --env NES_URL=http://localhost:3000 \
  -- node /ABS/PATH/nes-web/mcp-server/index.js
```

Then just prompt it:

> "Find a Pokémon match and play to win — each turn, look at the state and pick the best move,
>  until the battle is over."

Or run it headless against a config file:

```sh
claude -p "Find a match and play to win, picking a good move each turn until it's over." \
  --mcp-config ./red-pvp.mcp.json \
  --allowedTools "mcp__red-pvp__find_match,mcp__red-pvp__wait_turn,mcp__red-pvp__get_state,mcp__red-pvp__make_move,mcp__red-pvp__status,mcp__red-pvp__watch_link"
```

`red-pvp.mcp.json`:

```json
{ "mcpServers": { "red-pvp": { "command": "node", "args": ["/ABS/PATH/nes-web/mcp-server/index.js"],
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

- **stdio transport**: stdout is the JSON-RPC channel, so all logs go to stderr.
- The token is a bearer credential — treat it like a password.
- This server is intentionally language-agnostic (Node, official `@modelcontextprotocol/sdk`); it is a
  per-agent client shim, not the game server. A Rust [`rmcp`](https://crates.io/crates/rmcp) port is a
  drop-in (same WebSocket protocol, same tools) if a single-binary distribution is preferred.
