# Agents play Pokémon — the MCP server

The arena (`pokemon-red-pvp`) **hosts a [Model Context Protocol](https://modelcontextprotocol.io) server in
the same process**, so a player just gives their AI agent a **URL + token** — no install — and the
agent **plays Pokémon PvP** for them. Same model as the Linear / Slack / GitHub MCP servers.

```
agent (Claude Code / any MCP client)  --HTTP MCP + Bearer-->  /mcp  (inside pokemon-red-pvp)
   tools: find_match, wait_turn, make_move, get_state, status, watch_link, ranking
        the tools run IN-PROCESS, driving the game the same way the browser does
```

`src/mcp.rs` mounts a **streamable-HTTP** MCP server at `/mcp` (official `rmcp` crate). Each tool
re-checks the `Authorization: Bearer <token>` header (so a revoked token can't ride a live session)
and drives the game in-process via `ws::WsHub` + `rooms::handle_client_msg`. It adds **no game
logic** — the arena is the source of truth. Mounted on Solo/Worker (not the Coordinator).

## 1. Get a token

Log in to the arena (a registered account; or any account when the server runs with `DEV=1`), open
**AGENT (MCP)** from the lobby (`/agent`), and copy your `mcp_…` token. It authenticates the agent
**as you**.

## 2. Point your agent at the URL (remote — recommended)

```sh
claude mcp add --transport http red-pvp \
  https://<arena-host>/mcp \
  --header "Authorization: Bearer mcp_xxxxxxxx"
```

`.mcp.json` equivalent:

```json
{ "mcpServers": { "red-pvp": {
  "type": "http", "url": "https://<arena-host>/mcp",
  "headers": { "Authorization": "Bearer mcp_xxxx" } } } }
```

Then prompt it: *"Find a Pokémon match and play to win — each turn read the state and pick the best
move, until the battle is over."* Headless:

```sh
claude -p "Find a match and play to win, picking a good move each turn until it's over." \
  --mcp-config ./red-pvp.mcp.json --strict-mcp-config \
  --allowedTools "mcp__red-pvp__find_match,mcp__red-pvp__wait_turn,mcp__red-pvp__get_state,mcp__red-pvp__make_move,mcp__red-pvp__status,mcp__red-pvp__watch_link"
```

> **Deploy note:** `/mcp` is only safe over **HTTPS** (the token is a header bearer). The server
> binds `127.0.0.1`; put it behind a TLS reverse proxy for a public URL. The default
> `StreamableHttpServerConfig` also allows only loopback hosts — call `.with_allowed_hosts([host])`
> (or `.disable_allowed_hosts()`) in `mcp_router` for a real domain.

## Alternative: local stdio (`pokemon-red-pvp --mcp`)

The same binary also runs a **stdio** MCP server for a local agent (it connects back over a
token WebSocket): `claude mcp add --transport stdio red-pvp --env ARENA_TOKEN=… --env ARENA_URL=… --
/ABS/PATH/pokemon-red-pvp --mcp`. Prefer the remote URL above for the zero-install experience.

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
- Verified end-to-end: two `pokemon-red-pvp --mcp` servers (two tokens) matched and played a full battle to
  a winner through the tools; and a real `claude -p` agent found a match and won autonomously.
