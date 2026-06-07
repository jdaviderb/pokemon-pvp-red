#!/usr/bin/env node
// Red PVP Arena — MCP server. Lets an AI agent (Claude Code, etc.) play Pokemon PvP on the arena.
//
// It holds ONE persistent token-authenticated WebSocket to the arena (the same /ws protocol the
// browser uses) and exposes the gameplay as MCP tools. Configure your agent with:
//   claude mcp add --transport stdio red-pvp --env NES_TOKEN=mcp_... --env NES_URL=http://host -- node index.js
//
// IMPORTANT: stdout is the JSON-RPC channel — all logging goes to stderr.
import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { z } from "zod";

const NES_URL = (process.env.NES_URL || "http://localhost:3000").replace(/\/+$/, "");
const NES_TOKEN = process.env.NES_TOKEN || "";
const log = (...a) => console.error("[nes-mcp]", ...a);
if (!NES_TOKEN) log("WARNING: NES_TOKEN is empty — the arena will reject the WebSocket.");

// ---- live arena state, fed by the WebSocket event stream ----
const S = {
  phase: "idle", // idle | queued | matched | in_battle | ended
  room: null, seat: null, opponent: null,
  youDex: null, oppDex: null,
  you: null, opp: null, // {species,hp,max_hp,status}
  moves: null, // [{slot,id,pp}] for the current turn
  myTurn: false,
  result: null, // 'won' | 'lost' | 'ended'
  connected: false,
};
let dexName = {}; // internal species index -> NAME (loaded from /api/species)

const waiters = [];
function wake() {
  for (let i = waiters.length - 1; i >= 0; i--) {
    if (waiters[i].pred()) { waiters[i].resolve(true); waiters.splice(i, 1); }
  }
}
function waitFor(pred, ms) {
  return new Promise((resolve) => {
    if (pred()) return resolve(true);
    const w = { pred, resolve };
    waiters.push(w);
    setTimeout(() => { const i = waiters.indexOf(w); if (i >= 0) { waiters.splice(i, 1); resolve(false); } }, ms);
  });
}

let ws = null;
const outbox = [];
function send(obj) {
  if (ws && ws.readyState === 1) ws.send(JSON.stringify(obj));
  else outbox.push(obj);
}
function connect() {
  const wsUrl = NES_URL.replace(/^http/, "ws") + "/ws?token=" + encodeURIComponent(NES_TOKEN);
  ws = new WebSocket(wsUrl);
  ws.addEventListener("open", () => { S.connected = true; log("ws connected"); while (outbox.length) ws.send(JSON.stringify(outbox.shift())); });
  ws.addEventListener("message", (e) => onMsg(JSON.parse(typeof e.data === "string" ? e.data : e.data.toString())));
  ws.addEventListener("close", () => { S.connected = false; log("ws closed; reconnecting in 1s"); setTimeout(connect, 1000); });
  ws.addEventListener("error", () => log("ws error"));
}
function onMsg(m) {
  switch (m.type) {
    case "queued": S.phase = "queued"; break;
    case "matched": S.phase = "matched"; S.room = m.room_id; S.seat = m.seat; S.opponent = m.opponent; S.result = null; break;
    case "slot_result": S.youDex = m.you_dex; S.oppDex = m.opp_dex; break;
    case "battle_state": S.phase = "in_battle"; S.you = m.you; S.opp = m.opp; break;
    case "your_turn": S.myTurn = true; S.moves = m.moves || []; S.phase = "in_battle"; break;
    case "winner": S.phase = "ended"; S.result = m.you_won ? "won" : "lost"; S.myTurn = false; break;
    case "room_closed": if (S.phase !== "ended") { S.phase = "ended"; S.result = S.result || "ended"; } S.myTurn = false; break;
  }
  wake();
}

const nm = (idx) => dexName[idx] || ("#" + idx);
function monLine(label, mon, dex) {
  if (!mon) return `${label}: (unknown)`;
  const name = nm(mon.species) + (dex ? ` (dex ${dex})` : "");
  const st = mon.status ? ` [status ${mon.status}]` : "";
  return `${label}: ${name}  HP ${mon.hp}/${mon.max_hp}${st}`;
}
function stateText() {
  const lines = [`phase: ${S.phase}`];
  if (S.room) lines.push(`room: ${S.room} (seat ${S.seat}, vs ${S.opponent})`);
  if (S.phase === "in_battle" || S.you) {
    lines.push(monLine("YOU", S.you, S.youDex));
    lines.push(monLine("FOE", S.opp, S.oppDex));
    lines.push(S.myTurn ? "It is YOUR turn — pick a move with make_move." : "Waiting for the turn...");
    if (S.moves && S.moves.length) lines.push("moves: " + S.moves.map((x) => `slot ${x.slot} (move id ${x.id}, pp ${x.pp})`).join("; "));
  }
  if (S.phase === "ended") lines.push(`result: ${S.result === "won" ? "YOU WON" : S.result === "lost" ? "you lost" : "battle ended"}`);
  return lines.join("\n");
}
const txt = (s) => ({ content: [{ type: "text", text: s }] });

// ---- MCP server ----
const server = new McpServer({ name: "red-pvp-arena", version: "1.0.0" });

server.tool("find_match", "Queue for a Pokemon PvP match and wait until matched. Returns your assigned Pokemon and the opponent.", async () => {
  if (S.phase === "in_battle" || S.phase === "matched") return txt("Already in a match.\n" + stateText());
  send({ type: "find_match" });
  const ok = await waitFor(() => S.phase === "matched" || S.phase === "in_battle", 45000);
  return txt(ok ? "Matched!\n" + stateText() : "Still searching (no opponent yet). Call status or find_match again.");
});

server.tool("wait_turn", "Block until it's your turn to move OR the battle ends. Returns the current battle state + your move options, or the final result.", async () => {
  const ok = await waitFor(() => (S.myTurn && S.phase === "in_battle") || S.phase === "ended", 60000);
  if (!ok) return txt("Timed out waiting for your turn.\n" + stateText());
  if (S.phase === "ended") return txt("BATTLE OVER.\n" + stateText());
  return txt("YOUR TURN.\n" + stateText());
});

server.tool("make_move", "Use one of your Pokemon's moves by slot (0-3). Call wait_turn first to see your options.",
  { slot: z.number().int().min(0).max(3).describe("move slot 0-3 from the moves list") },
  async ({ slot }) => {
    if (S.phase === "ended") return txt("The battle is over.\n" + stateText());
    S.myTurn = false;
    send({ type: "commit_move", slot });
    return txt(`Move (slot ${slot}) submitted. Call wait_turn for the next turn or the result.`);
  });

server.tool("get_state", "Get the current battle state as text (your Pokemon + HP + moves, the foe, whose turn).", async () => txt(stateText()));

server.tool("status", "Quick status: idle / queued / matched / in_battle / ended, plus the room id.", async () =>
  txt(JSON.stringify({ phase: S.phase, room: S.room, seat: S.seat, opponent: S.opponent, result: S.result, connected: S.connected })));

server.tool("watch_link", "Get a spectator URL a human can open to watch your current battle live.", async () =>
  txt(S.room ? `${NES_URL}/room?id=${S.room}` : "No active battle yet — call find_match first."));

server.tool("ranking", "Get the arena leaderboard (top trainers by wins) for today/weekly/monthly.", async () => {
  try {
    const d = await (await fetch(`${NES_URL}/api/ranking`)).json();
    const fmt = (a) => (a || []).slice(0, 5).map((e, i) => `${i + 1}. ${e.name} (${e.wins})`).join("  ") || "(none)";
    return txt(`TODAY: ${fmt(d.today)}\nWEEKLY: ${fmt(d.weekly)}\nMONTHLY: ${fmt(d.monthly)}`);
  } catch (e) { return txt("Could not fetch ranking."); }
});

async function main() {
  // species index -> name (for readable battle text)
  try {
    const list = await (await fetch(`${NES_URL}/api/species`)).json();
    for (const s of list) dexName[s.index] = s.name;
    log(`loaded ${Object.keys(dexName).length} species names`);
  } catch (e) { log("could not load species names (non-fatal)"); }
  connect();
  const transport = new StdioServerTransport();
  await server.connect(transport);
  log("MCP server ready on stdio; arena =", NES_URL);
}
main().catch((e) => { log("fatal", e); process.exit(1); });
