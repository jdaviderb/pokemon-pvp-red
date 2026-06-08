# Pokémon PVP Red

### The original Pokémon Red — online, in your browser, battling REAL people in real time. No download. ▶️

<!-- GIF: full hero shot — click PLAY (guest), hit FIND MATCH, the slot machine spins and rolls a random Gen-1 team. End on the battle starting. This is THE money shot. -->
![Pokémon PVP Red — click PLAY, find a match, slot machine rolls your team](docs/media/hero.gif)

Remember battling your friends with a link cable? **Pokémon PVP Red** is that — except your opponent is
anyone in the world, it's live, and it runs in your browser. It's the real Pokémon Red, in color, with
**no app to install, no emulator to set up, no ROM to find.** Open the page, hit **PLAY**, and you're in.

> ## 🧪 Alpha — this is an early experiment
> It's **live and playable**, but it's a **work in progress**: expect rough edges, weird bugs, and things
> that change (or break) without warning. Saves, accounts, and features may reset. Come play, come break
> it, and don't be shocked when things move around. You've been warned 😄

## ▶ Play now — **[pokemonpvp.red](https://pokemonpvp.red)**

Free. Instant. Play as a guest or sign in with Google. **Smash PLAY.** 🔴

---

## How it works (about 10 seconds)

1. **Go to [pokemonpvp.red](https://pokemonpvp.red)** and hit **PLAY** — jump in as a guest, no signup.
2. Press **FIND MATCH**. A slot machine rolls you a **random Gen-1 team**.
3. You're matched with a **real person** — battle live, **15 seconds per turn**.
4. **Win** to climb the leaderboards and earn Pokémon for your collection.

That's it. No tutorials, no grind to get started — just battles.

<!-- GIF: a single live battle turn — the FIGHT menu, picking a move, the attack animation landing, HP bar draining, and the 15-second turn timer ticking. Show the real game responding to a real player. -->
![A live Pokémon PVP Red battle turn — pick a move, 15s on the clock](docs/media/battle-turn.gif)

## What makes it fun

- **⚔️ Real opponents, live.** You're matched against an actual person, not a bot. Real battles, real
  pressure, 15 seconds a turn.
- **🎰 A random team every time.** The slot machine deals you a fresh Gen-1 squad each match. No two games
  are the same — adapt or get swept.
- **🏆 Climb the leaderboards.** Win to rise on the **Today**, **Weekly**, and **Monthly** boards. Top the
  day, grind the week, own the month.
- **📦 Collect your favorites.** Win **5 times** with a Pokémon and it's **yours** — added to your
  collection forever.
- **📺 Watch every battle at once.** The live TV wall streams every match happening right now. Spectate
  the whole arena and scout your next rival.
- **🚀 Zero friction.** One click and you're battling. Sign in with Google whenever you want to save your
  wins and your collection.

## Every battle, live — the TV wall

Why watch one battle when you can watch them all? Every live match in the arena, on a single screen. It's
Pokémon as a spectator sport.

<!-- GIF: the live TV wall — a paginated grid of multiple simultaneous battle streams all playing live at once. Pan across the wall so it's obviously many real games. -->
![The Pokémon PVP Red TV wall — every live battle at once](docs/media/tv-wall.gif)

## Or let an AI play for you 🤖

Don't feel like holding the controller? You can connect an **AI agent** that finds matches and battles
real opponents for you — completely hands-off. (For the curious, it's powered by a built-in MCP server.)

<!-- GIF: an AI agent playing — show the agent finding a match, choosing moves, and the live game reacting/winning. Make it clear the agent is driving. -->
![An AI agent playing Pokémon PVP Red](docs/media/mcp-agent.gif)

---

### ▶ [Play now — pokemonpvp.red](https://pokemonpvp.red)

Free, instant, in your browser. See you in the arena. 🔴

<br>

<details>
<summary><b>For developers — run it yourself</b></summary>

<br>

Pokémon PVP Red runs the real Pokémon Red **fully server-side** ([gambatte](https://github.com/libretro/gambatte-libretro)
libretro core, written in **Rust**) and streams it live to the browser over **WebRTC** (VP8 video + Opus
audio); input goes back over a data channel. The browser never touches a ROM. AI agents plug in via an
**MCP server**.

```sh
cargo run --release          # default = coordinator (one emulator per battle → N concurrent rooms)
# open http://localhost:3000  →  PLAY AS GUEST  →  FIND MATCH
```

`cargo run --release -- --solo` runs the single-process arena (one emulator, one battle). Needs homebrew
`libvpx` + `libopus`; the gambatte core lives in `cores/` (see `cores/fetch.sh`).

**Docker (production, linux/amd64):**

```sh
./build-docker-production.sh                                   # → pokemon-red-pvp:prod
docker run --rm --network host -e DEV=1 pokemon-red-pvp:prod   # open http://localhost:3000
```

Use `--network host`, not `-p` — WebRTC's ICE candidates are unreachable across Docker's bridge network.

**Controls (Game Boy):** Arrows = D-pad · `X` = A · `Z` = B · `Enter` = Start · `⇧Right` / `⌫` = Select.

Full docs: **[DOCS.md](DOCS.md)** · architecture & internals: **[CLAUDE.md](CLAUDE.md)** · AI agents:
**[docs/mcp.md](docs/mcp.md)** and **[docs/connect-agents.md](docs/connect-agents.md)**.

</details>
