# Pokémon PVP Red

### The original Pokémon Red — online, in your browser, battling REAL people in real time. Zero install. ▶️

<!-- GIF: full hero shot — click PLAY (guest), hit FIND MATCH, the slot machine spins and rolls a random Gen-1 team. End on the battle starting. This is THE money shot. -->
![Pokémon PVP Red — click PLAY, find a match, slot machine rolls your team](docs/media/hero.gif)

**Pokémon PVP Red** is **Pokémon Red online** the way 10-year-old you dreamed it: the actual Gen-1 cart,
emulated **server-side** and streamed live to your browser over WebRTC — in glorious color. No download.
No emulator setup. No ROM hunting. Click **PLAY** and you're in.

This is **pokemon red pvp** done right. Get matched against a **real human opponent**, get a random team
from the slot machine, and throw down in **online Pokémon battles** — 15 seconds a turn, winner takes the
ladder. Welcome to the **retro Pokémon arena**.

> **play pokemon red in browser** · **gen 1 pokemon pvp** · **pokemon multiplayer** · **pokemon battle online free**

## ▶ Play now — [pokemonpvp.red](https://pokemonpvp.red)

Free. Instant. As a guest or with Google. **Smash PLAY.** 🔴

---

## Why it rules

You grew up on this game. Now it fights back. Here's the kit:

- **⚔️ Real-time PvP** — matched against an actual human, live. **Online Pokémon battles**, 15 seconds per
  turn. No bots pretending to be people. Real opponents, real pressure.
- **🎰 Slot-machine random teams** — hit FIND MATCH and the slot machine rolls you a random Gen-1 squad.
  Every match is a fresh draw. Adapt or get swept.
- **🏆 Today / Weekly / Monthly leaderboards** — win to climb. Three boards, three windows, infinite
  bragging rights. Top the daily, grind the weekly, own the month.
- **📦 Build your collection** — **win 5 times with a Pokémon and it's YOURS.** Add it to your collection.
  Earn your team one victory at a time.
- **📺 The live TV wall** — every single battle happening right now, streaming at once. Spectate the
  whole arena. Find your next rival. Watch the ladder churn live.
- **🤖 AI agents via MCP** — don't want to hold the controller? Point an **AI agent** at the built-in
  **MCP server** and it finds matches and battles for you. Yes, really.
- **🚀 Instant guest play** — no signup wall. One click and you're battling. Sign in with Google when
  you're ready to keep your wins.
- **🌐 Runs in the browser, in color** — zero install. The real Pokémon Red, auto-colorized like a Game
  Boy Color rendered it, streamed straight to your tab.

<!-- GIF: a single live battle turn — the FIGHT menu, picking a move, the attack animation landing, HP bar draining, and the 15-second turn timer ticking. Show the real game responding to a real player. -->
![A live Pokémon PVP Red battle turn — pick a move, 15s on the clock](docs/media/battle-turn.gif)

## The TV wall

Why watch one battle when you can watch them ALL? Every live match in the arena, on one screen. This is
**pokemon red online** as a spectator sport.

<!-- GIF: the live TV wall — a paginated grid of multiple simultaneous WebRTC battle streams all playing live at once. Pan across the wall so it's obviously many real games. -->
![The Pokémon PVP Red TV wall — every live battle at once](docs/media/tv-wall.gif)

## Let an AI play for you

Connect an **AI agent** through the **MCP server** and watch it find a match and battle a real opponent —
hands off the keyboard. **Gen 1 pokemon pvp**, played by your agent. See [`docs/mcp.md`](docs/mcp.md) and
[`docs/connect-agents.md`](docs/connect-agents.md).

<!-- GIF: an AI agent connected via MCP — show the agent's tool calls / decisions on one side and the live game reacting (find match, choose moves, win) on the other. Make it clear the agent is driving. -->
![An AI agent playing Pokémon PVP Red via MCP](docs/media/mcp-agent.gif)

---

## Built different

A genuine flex, kept short:

- The **original Pokémon Red**, emulated **fully server-side** ([gambatte](https://github.com/libretro/gambatte-libretro)
  libretro core) — your browser never touches a ROM.
- Streamed live over **WebRTC**: **VP8 video + stereo Opus audio**, input sent back over a data channel.
- Written in **Rust**, one emulator per battle, fanned out to spectators in real time.
- **AI agents** plug in via a first-class **MCP server**.

The game is server-only; the browser just gets the stream. That's how **play pokemon red in browser**
with no install actually works.

---

## Run it yourself

```sh
cargo run --release          # default = coordinator (a worker/emulator per battle → N concurrent rooms)
# open http://localhost:3000  →  PLAY AS GUEST  →  FIND MATCH
```

`cargo run --release -- --solo` runs the single-process arena (one emulator, one battle at a time).
Needs homebrew `libvpx` + `libopus`; the gambatte core lives in `cores/` (see `cores/fetch.sh`).

**Docker (production, linux/amd64):**

```sh
./build-docker-production.sh                                   # → pokemon-red-pvp:prod
docker run --rm --network host -e DEV=1 pokemon-red-pvp:prod   # open http://localhost:3000
```

**Use `--network host`**, not `-p` — WebRTC's ICE candidates are unreachable across Docker's bridge
network (the UI loads but no video). See [`docs/SCALING.md`](docs/SCALING.md).

**Controls (Game Boy):** Arrows = D-pad · `X` = A · `Z` = B · `Enter` = Start · `⇧Right` / `⌫` = Select.

## Docs

Full documentation index: **[DOCS.md](DOCS.md)**. Deep build/architecture notes: **[CLAUDE.md](CLAUDE.md)**.

---

### ▶ [Play Pokémon PVP Red now — pokemonpvp.red](https://pokemonpvp.red) — free, instant, in your browser. Gotta battle 'em all. 🔴
