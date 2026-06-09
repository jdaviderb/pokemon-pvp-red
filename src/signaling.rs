//! axum router: serve static/index.html and handle the POST /offer SDP exchange.
//! Same-origin, so no CORS. Wire JSON is exactly {"type","sdp"} (browser-identical).

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{FromRef, Path, Query, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::http::{HeaderValue, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use axum_extra::extract::cookie::{Cookie, CookieJar, Key, PrivateCookieJar, SameSite};
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};
use serde::{Deserialize, Serialize};
use tower_http::services::{ServeDir, ServeFile};

use ::webrtc::api::API;

use crate::pipeline::AppInner;

/// How this process participates in the (optionally) multi-process arena.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    /// Single process: emulator + matchmaking + everything (the default, backwards-compatible).
    Solo,
    /// Emulator worker: runs one battle assigned by the coordinator (no own matchmaking).
    Worker,
    /// No emulator: auth/lobby/matchmaking + a pool of worker processes; redirects players to them.
    Coordinator,
}

#[derive(Clone)]
pub struct AppState {
    pub api: Arc<API>,
    pub inner: Arc<AppInner>,
    pub db: DatabaseConnection,
    pub cookie_key: Key,
    pub game: Arc<crate::rooms::GameState>,
    pub role: Role,
    /// Coordinator only: the worker-process pool (for the /room redirect + matchmaking).
    pub pool: Option<Arc<crate::coordinator::WorkerPool>>,
    /// DEV mode (env `DEV`): when off, the unauthenticated /battle/* dev endpoints and the dev
    /// console are NOT mounted at all (security: no anonymous control of the shared emulator).
    pub dev: bool,
    /// Configured OAuth providers (google, ...). Empty if none are set up.
    pub oauth: Arc<crate::oauth::Registry>,
    /// Mark session/oauth cookies Secure (HTTPS-only). On in prod, off in DEV (localhost HTTP).
    pub cookie_secure: bool,
    /// Cached leaderboard (Today/Weekly/Monthly), refreshed by a background job; /api/ranking reads it.
    pub ranking: crate::ranking::RankingCache,
}

impl FromRef<AppState> for Key {
    fn from_ref(s: &AppState) -> Self {
        s.cookie_key.clone()
    }
}

#[derive(Deserialize)]
pub struct OfferRequest {
    pub sdp: String,
    #[serde(rename = "type")]
    pub kind: String, // "offer"
}

#[derive(Serialize)]
pub struct AnswerResponse {
    pub sdp: String,
    #[serde(rename = "type")]
    pub kind: String, // "answer"
}

/// Address to bind: `BIND_ADDR` env (e.g. `0.0.0.0` in Docker) else loopback for local safety.
pub fn bind_addr(port: u16) -> std::net::SocketAddr {
    let ip = std::env::var("BIND_ADDR")
        .ok()
        .and_then(|s| s.parse::<std::net::IpAddr>().ok())
        .unwrap_or_else(|| std::net::IpAddr::from([127, 0, 0, 1]));
    std::net::SocketAddr::new(ip, port)
}

pub fn router(state: AppState) -> Router {
    // Serve ./static, index.html as the directory index for "/".
    let static_service = ServeDir::new("static").append_index_html_on_directories(true);
    let mut app = Router::new()
        .route("/offer", post(offer_handler))
        // --- auth + session ---
        .route("/auth/register", post(crate::auth::register))
        .route("/auth/login", post(crate::auth::login))
        .route("/auth/logout", post(crate::auth::logout))
        .route("/auth/guest", post(crate::auth::guest))
        .route("/auth/set-name", post(crate::auth::set_name))
        .route("/api/mcp/token", post(crate::auth::mcp_token))
        .route("/api/me", get(crate::auth::me))
        .route("/api/species", get(species_list_handler))
        .route("/api/online", get(online_handler))
        .route("/api/name-available", get(crate::auth::name_available))
        .route("/api/config", get(config_handler))
        .route("/api/room/{id}", get(room_info_handler))
        .route("/api/live", get(live_handler))
        .route("/api/ranking", get(ranking_handler))
        .route("/api/collection", get(collection_handler))
        // --- social login (provider-agnostic: /auth/oauth/{provider}[/callback]) ---
        .route("/auth/oauth/{provider}", get(crate::oauth::start))
        .route("/auth/oauth/{provider}/callback", get(crate::oauth::callback))
        // --- clean (extensionless) page URLs — more professional than *.html ---
        .route_service("/login", ServeFile::new("static/login.html"))
        .route_service("/lobby", ServeFile::new("static/lobby.html"))
        .route_service("/tv", ServeFile::new("static/tv.html"))
        .route_service("/ranking", ServeFile::new("static/ranking.html"))
        .route_service("/collection", ServeFile::new("static/collection.html"))
        .route_service("/agent", ServeFile::new("static/mcp.html"))
        // /room: worker/solo serve the page; the coordinator redirects to the worker running it.
        .route("/room", get(room_page_handler))
        // --- realtime ---
        .route("/ws", get(crate::ws::ws_upgrade));

    // Worker only: the coordinator drives battles through these internal (secret-gated) endpoints.
    if state.role == Role::Worker {
        app = app
            .route("/internal/assign", post(crate::coordinator::assign_handler))
            .route("/internal/status", get(crate::coordinator::status_handler));
    }

    // DEV-only: the dev console + the unauthenticated emulator-control endpoints. Off by default so
    // they don't exist in production (no anonymous /battle/* control of the shared emulator).
    if state.dev {
        tracing::warn!("DEV mode ON — /console + /battle/* are mounted (unauthenticated)");
        app = app
            .route("/debug/frame", get(debug_frame_handler))
            .route("/battle/state", get(battle_state_handler))
            .route("/battle/action", post(battle_action_handler))
            .route("/battle/save", post(battle_save_handler))
            .route("/battle/load", post(battle_load_handler))
            .route("/battle/setup", post(battle_setup_handler))
            .route("/battle/species", get(battle_species_handler))
            .route("/battle/enemy", post(battle_enemy_handler))
            .route("/battle/player", post(battle_player_handler))
            .route_service("/console", ServeFile::new("dev/console.html"));
    }

    // Remote MCP server (streamable HTTP) at /mcp so an AI agent plays via a URL + bearer token.
    // Mounted on Solo (in-process battle) and Coordinator (matchmake here, then route the session to
    // the spawned worker). NOT on ephemeral Workers — agents always connect to the front door.
    if state.role != Role::Worker {
        app = app.merge(crate::mcp::mcp_router(state.clone()));
    }

    app.fallback_service(static_service)
        // Never let the browser serve a STALE cached HTML page (routing/onboarding logic lives in
        // it). Assets keep their own caching; only text/html is marked no-cache.
        .layer(axum::middleware::map_response(no_cache_html))
        .with_state(state)
}

/// Mark HTML responses no-cache so page logic is always fresh (assets are untouched).
async fn no_cache_html(mut res: Response) -> Response {
    let is_html = res
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("text/html"));
    if is_html {
        res.headers_mut().insert(CACHE_CONTROL, HeaderValue::from_static("no-cache, must-revalidate"));
    }
    res
}

/// GET /api/species -> [{dex, index, name, types, moves}] (dex = position+1; index = internal
/// Gen-1 byte). The slot machine keys sprites by national dex; the engine speaks internal index.
/// `types` is a display string (e.g. "ICE/FLYING"); `moves` are the 4 base move ids.
async fn species_list_handler() -> Json<Vec<serde_json::Value>> {
    Json(
        crate::battle::SPECIES
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let moves: Vec<u8> = s.moves.iter().map(|(id, _pp)| *id).collect();
                serde_json::json!({
                    "dex": i + 1,
                    "index": s.species,
                    "name": s.name,
                    "types": type_label(s.type1, s.type2),
                    "moves": moves,
                })
            })
            .collect(),
    )
}

/// GET /api/config -> {providers:["google",...], username_login:bool, guest:bool}. Public; the login
/// page uses it to decide which options to render. Deliberately does NOT expose the raw DEV flag.
async fn config_handler(State(state): State<AppState>) -> Json<serde_json::Value> {
    let mut providers: Vec<&str> = state.oauth.keys().map(|s| s.as_str()).collect();
    providers.sort_unstable();
    Json(serde_json::json!({
        "providers": providers,
        "username_login": crate::auth::username_login_on(&state),
        "guest": crate::flags::cached(&state.game, crate::flags::GUEST_MODE),
        "mode_box": crate::flags::cached(&state.game, crate::flags::TITLE_MODE_BOX),
        // On a worker this points back at the coordinator so "RETURN HOME" leaves the worker port.
        "coordinator_origin": std::env::var("COORDINATOR_ORIGIN").unwrap_or_default(),
        // Fighting-game arena (PSX): the room page captures the keyboard as a 2-player controller
        // instead of running the Pokémon battle UI.
        "fight": crate::rooms::fight_mode(),
    }))
}

/// GET /room: serve the battle page (same origin everywhere). In coordinator mode the page's battle
/// WS (`/ws?room=`) and video (`/offer?room=`) are bridged to the worker running this room, so the
/// browser only ever talks to the public origin — no redirect to an internal worker port. The page
/// shows the recorded result or "BATTLE NOT FOUND" (via /api/room) for an ended/unknown room.
async fn room_page_handler() -> Response {
    match tokio::fs::read_to_string("static/room.html").await {
        Ok(html) => Html(html).into_response(),
        Err(_) => (StatusCode::NOT_FOUND, "room page missing").into_response(),
    }
}

/// GET /api/room/{id} -> spectator info for a room UUID ({found, live, phase, p1, p2}). Public so a
/// not-logged-in viewer can watch. {found:false} for an unknown / ended room.
async fn room_info_handler(State(state): State<AppState>, Path(id): Path<String>) -> Json<serde_json::Value> {
    match crate::rooms::spectate_info(&state, &id).await {
        Some(v) => Json(v),
        None => Json(serde_json::json!({ "found": false })),
    }
}

/// GET /api/live -> {battles:[{id,p1,p2}]} for the TV page. The coordinator aggregates its whole
/// worker pool; solo/worker report their own active battle. Public (guests can watch).
async fn live_handler(State(st): State<AppState>) -> Json<serde_json::Value> {
    if st.role == Role::Coordinator {
        // lock-free cached snapshot — refreshed off the request path on battle start/end
        let snap = st.pool.as_ref().map(|p| p.live_snapshot()).unwrap_or_default();
        return Json(serde_json::json!({ "battles": &*snap }));
    }
    let battles = crate::rooms::live_battles(&st.game).await;
    Json(serde_json::json!({ "battles": battles }))
}

/// GET /api/ranking -> the cached leaderboard {today, weekly, monthly, updated}. In-memory read
/// (a background job refreshes it every RANKING_REFRESH_SECS); never hits the DB on the request path.
async fn ranking_handler(State(st): State<AppState>) -> Json<serde_json::Value> {
    Json(st.ranking.lock().unwrap().clone())
}

/// GET /api/collection (authed) -> {threshold, mons:[{index,dex,name,wins,owned}]}. A trainer OWNS a
/// Pokemon once they've WON with it `COLLECTION_WINS` times (env, default 5) — "gotta catch 'em all".
async fn collection_handler(
    State(st): State<AppState>,
    crate::auth::AuthUser(u): crate::auth::AuthUser,
) -> Json<serde_json::Value> {
    let threshold: i64 =
        std::env::var("COLLECTION_WINS").ok().and_then(|s| s.parse().ok()).unwrap_or(5);
    let backend = st.db.get_database_backend();
    let (p1, p2) = if matches!(backend, sea_orm::DatabaseBackend::Postgres) {
        ("$1", "$2")
    } else {
        ("?", "?")
    };
    // Count finished wins grouped by the species the user WON with (their own side).
    let sql = format!(
        "SELECT species, COUNT(*) AS wins FROM (\
            SELECT (CASE WHEN winner_seat = 1 THEN p1_species ELSE p2_species END) AS species \
            FROM matches WHERE (winner_seat = 1 AND p1_user = {p1}) OR (winner_seat = 2 AND p2_user = {p2})\
         ) t GROUP BY species ORDER BY wins DESC"
    );
    let rows = st
        .db
        .query_all(Statement::from_sql_and_values(backend, &sql, [u.id.into(), u.id.into()]))
        .await
        .unwrap_or_default();
    let mons: Vec<serde_json::Value> = rows
        .iter()
        .filter_map(|r| {
            let idx: i32 = r.try_get("", "species").ok()?;
            let wins: i64 = r.try_get("", "wins").ok()?;
            let (dex, name) = crate::battle::dex_name_by_index(idx as u8)?;
            Some(serde_json::json!({
                "index": idx, "dex": dex, "name": name, "wins": wins, "owned": wins >= threshold,
            }))
        })
        .collect();
    Json(serde_json::json!({ "threshold": threshold, "mons": mons }))
}

pub(crate) const ONLINE_WINDOW_SECS: u64 = 16;
const CID_COOKIE: &str = "pvp_cid";

/// GET /api/online -> {online: N}. A presence heartbeat keyed by a STABLE identity so refreshes,
/// extra tabs, and login->lobby->room navigation don't double-count: a logged-in user counts ONCE
/// (any tabs/devices) via their session, and an anonymous visitor counts once per browser via a
/// long-lived `nes_cid` cookie. Counts distinct identities seen in the last ONLINE_WINDOW_SECS.
async fn online_handler(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    cookies: CookieJar,
) -> impl IntoResponse {
    let (key, cookies) = match crate::auth::user_from_jar(&state, &jar).await {
        Some(u) => (format!("u:{}", u.id), cookies),
        None => match cookies.get(CID_COOKIE) {
            Some(c) => (format!("c:{}", c.value()), cookies),
            None => {
                let cid = crate::rooms::gen_uuid();
                let cookie = Cookie::build((CID_COOKIE, cid.clone()))
                    .http_only(true)
                    .same_site(SameSite::Lax)
                    .secure(state.cookie_secure)
                    .path("/")
                    .max_age(time::Duration::days(365))
                    .build();
                (format!("c:{cid}"), cookies.add(cookie))
            }
        },
    };
    // O(1) on the request path: just record the heartbeat + read the size. Stale entries are evicted
    // by a background sweeper (rooms::spawn_presence_sweeper), so this hot path never does the O(N)
    // retain under the lock — critical when every online client polls every few seconds.
    let total = {
        let mut m = state.game.online.lock().unwrap();
        m.insert(key, std::time::Instant::now());
        m.len()
    };
    (cookies, Json(serde_json::json!({ "online": total })))
}

fn gen1_type_name(t: u8) -> &'static str {
    match t {
        0 => "NORMAL", 1 => "FIGHTING", 2 => "FLYING", 3 => "POISON", 4 => "GROUND", 5 => "ROCK",
        7 => "BUG", 8 => "GHOST", 0x14 => "FIRE", 0x15 => "WATER", 0x16 => "GRASS",
        0x17 => "ELECTRIC", 0x18 => "PSYCHIC", 0x19 => "ICE", 0x1A => "DRAGON", _ => "???",
    }
}
fn type_label(t1: u8, t2: u8) -> String {
    if t1 == t2 {
        gen1_type_name(t1).to_string()
    } else {
        format!("{}/{}", gen1_type_name(t1), gen1_type_name(t2))
    }
}

#[derive(Deserialize)]
struct OfferRoomQ {
    room: Option<String>,
}

async fn offer_handler(
    State(state): State<AppState>,
    Query(q): Query<OfferRoomQ>,
    Json(offer): Json<OfferRequest>,
) -> Result<Json<AnswerResponse>, (StatusCode, String)> {
    let _ = &offer.kind; // expected "offer"
    // Coordinator owns no emulator: proxy the SDP to the worker running this room (the answer carries
    // the worker's ICE candidates, so media flows browser<->worker directly). Used by the TV wall to
    // play every live battle same-origin from :3000.
    if state.role == Role::Coordinator {
        let room = q.room.ok_or((StatusCode::BAD_REQUEST, "room query required".to_string()))?;
        let pool = state
            .pool
            .as_ref()
            .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "no pool".to_string()))?;
        let sdp = pool
            .proxy_offer(&room, &offer.sdp)
            .await
            .ok_or((StatusCode::BAD_GATEWAY, "worker offer failed".to_string()))?;
        return Ok(Json(AnswerResponse { sdp, kind: "answer".to_owned() }));
    }
    let answer_sdp = crate::webrtc::build_peer_and_answer(&state.api, &state.inner, offer.sdp)
        .await
        .map_err(|e| {
            tracing::error!("offer handling failed: {e:?}");
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        })?;
    Ok(Json(AnswerResponse {
        sdp: answer_sdp,
        kind: "answer".to_owned(),
    }))
}

// ---------- AI battle-arena API ----------

/// GET /debug/frame -> the latest framebuffer as a PPM (DEV only). Lets us eyeball the HUD overlay.
async fn debug_frame_handler() -> Response {
    let (w, h, pitch, fmt, bytes) = crate::libretro::snapshot_frame();
    if w == 0 || bytes.is_empty() {
        return (StatusCode::SERVICE_UNAVAILABLE, "no frame").into_response();
    }
    let (w, h) = (w as usize, h as usize);
    let mut out = format!("P6\n{w} {h}\n255\n").into_bytes();
    for y in 0..h {
        for x in 0..w {
            let off = y * pitch + x * 2;
            let (r, g, b) = if fmt == crate::video::PIXFMT_RGB565 && off + 1 < bytes.len() {
                let v = (bytes[off] as u16) | ((bytes[off + 1] as u16) << 8);
                let (r5, g6, b5) = (((v >> 11) & 0x1f) as u8, ((v >> 5) & 0x3f) as u8, (v & 0x1f) as u8);
                ((r5 << 3) | (r5 >> 2), (g6 << 2) | (g6 >> 4), (b5 << 3) | (b5 >> 2))
            } else {
                (0, 0, 0)
            };
            out.extend_from_slice(&[r, g, b]);
        }
    }
    ([(CONTENT_TYPE, "image/x-portable-pixmap")], out).into_response()
}

/// GET /battle/state -> the latest snapshot the emulator thread published.
async fn battle_state_handler(
    State(state): State<AppState>,
) -> Result<Json<crate::battle::BattleState>, (StatusCode, String)> {
    match state.inner.battle.lock().unwrap().clone() {
        Some(st) => Ok(Json(st)),
        None => Err((StatusCode::SERVICE_UNAVAILABLE, "no battle state yet".into())),
    }
}

/// POST /battle/action  body: {"type":"move","slot":0} (also switch/run/buttons). 202 = queued.
async fn battle_action_handler(
    State(state): State<AppState>,
    Json(action): Json<crate::battle::AgentAction>,
) -> StatusCode {
    if emu_busy(&state) {
        return StatusCode::CONFLICT;
    }
    let _ = state.inner.action_tx.send(action);
    StatusCode::ACCEPTED
}

/// True while a multiplayer match owns the single emulator — the dev console is locked out.
fn emu_busy(state: &AppState) -> bool {
    state.game.emu_busy.load(std::sync::atomic::Ordering::Relaxed)
}

/// POST /battle/save -> serialize on the emu thread, write states/battle.state, return the blob.
async fn battle_save_handler(
    State(state): State<AppState>,
) -> Result<Bytes, (StatusCode, String)> {
    if emu_busy(&state) {
        return Err((StatusCode::CONFLICT, "match in progress".into()));
    }
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ = state.inner.save_tx.send(tx);
    match rx.await {
        Ok(Some(buf)) => {
            let _ = std::fs::create_dir_all("states");
            if let Err(e) = std::fs::write("states/battle.state", &buf) {
                tracing::warn!("battle.state write failed: {e}");
            }
            Ok(Bytes::from(buf))
        }
        _ => Err((StatusCode::INTERNAL_SERVER_ERROR, "serialize failed".into())),
    }
}

/// POST /battle/load  body: raw savestate bytes; if empty, load states/battle.state from disk.
async fn battle_load_handler(State(state): State<AppState>, body: Bytes) -> StatusCode {
    if emu_busy(&state) {
        return StatusCode::CONFLICT;
    }
    let data = if body.is_empty() {
        match std::fs::read("states/battle.state") {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("no battle.state on disk: {e}");
                return StatusCode::NOT_FOUND;
            }
        }
    } else {
        body.to_vec()
    };
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ = state.inner.load_tx.send((data, tx));
    match rx.await {
        Ok(true) => StatusCode::OK,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

#[derive(Deserialize)]
pub struct SetupRequest {
    pub player: u8, // internal species index
    pub enemy: u8,  // internal species index
    #[serde(default = "default_level")]
    pub level: u8,
    #[serde(default)]
    pub player_name: String, // custom nickname ("" = species name)
    #[serde(default)]
    pub enemy_name: String,
}
fn default_level() -> u8 {
    50
}

/// GET /battle/species -> the selectable species table for the dropdowns: [[index,"NAME"], ...].
async fn battle_species_handler() -> Json<Vec<(u8, &'static str)>> {
    Json(crate::battle::species_menu())
}

/// POST /battle/setup  body: {"player":74,"enemy":75,"level":50}
/// Loads the intro savestate, injects both party slots, drives the send-out. 200 = matchup live.
async fn battle_setup_handler(
    State(state): State<AppState>,
    Json(req): Json<SetupRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    if emu_busy(&state) {
        return Err((StatusCode::CONFLICT, "match in progress".into()));
    }
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ = state.inner.setup_tx.send(crate::pipeline::SetupReq {
        player: req.player,
        enemy: req.enemy,
        level: req.level,
        player_name: req.player_name,
        enemy_name: req.enemy_name,
        reply: tx,
    });
    match rx.await {
        Ok(Ok(())) => Ok(StatusCode::OK),
        Ok(Err(e)) => Err((StatusCode::BAD_REQUEST, e)),
        Err(_) => Err((StatusCode::INTERNAL_SERVER_ERROR, "emu thread gone".into())),
    }
}

#[derive(Deserialize)]
pub struct EnemyRequest {
    /// Enemy move slot to force every turn (0..3), or 255 = let the game AI decide.
    #[serde(default = "ai_slot")]
    pub slot: u8,
}
fn ai_slot() -> u8 {
    0xFF
}

/// POST /battle/enemy {"slot":0..3}  -> YOU pick the opponent's move (forces wEnemySelectedMove
/// each turn). {"slot":255} or empty -> hand control back to the game AI.
async fn battle_enemy_handler(State(state): State<AppState>, Json(req): Json<EnemyRequest>) -> StatusCode {
    if emu_busy(&state) {
        return StatusCode::CONFLICT;
    }
    state
        .inner
        .enemy_force
        .store(req.slot, std::sync::atomic::Ordering::Relaxed);
    StatusCode::ACCEPTED
}

/// POST /battle/player {"slot":0..3} -> force the PLAYER's selected move into wPlayerSelectedMove
/// (CCDC) each turn, so the executed move is exactly that slot even if the menu macro mis-navigates.
/// {"slot":255} or empty -> trust the menu pick.
async fn battle_player_handler(State(state): State<AppState>, Json(req): Json<EnemyRequest>) -> StatusCode {
    if emu_busy(&state) {
        return StatusCode::CONFLICT;
    }
    state
        .inner
        .player_force
        .store(req.slot, std::sync::atomic::Ordering::Relaxed);
    StatusCode::ACCEPTED
}
