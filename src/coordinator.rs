//! Scalable mode: a COORDINATOR process that owns auth/lobby/matchmaking + a POOL of emulator
//! WORKER processes (each = this same binary with `--worker`, its own emulator, sharing the DB).
//!
//! Flow: players connect their lobby WS to the coordinator and `find_match`. When two are paired,
//! the coordinator picks a free worker, tells it to run the battle (`POST /internal/assign`), and
//! sends both players a `matched` event. The browser navigates to `/room?id=<uuid>` on the
//! coordinator, which **redirects** to the worker running it — WebRTC video + the battle WS then go
//! browser↔worker directly (cookies are shared across localhost ports, so the session still
//! authenticates against the shared DB). One worker = one concurrent battle, so N workers = N
//! battles at once. Backwards-compatible: with no `--coordinator`, the binary is the solo arena.

use std::process::{Child, Command};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::rooms::{self, UserId};
use crate::signaling::AppState;

struct Worker {
    port: u16,
    child: Child,
    busy: bool,
    room: Option<String>, // public_id of the battle it's currently running
}

pub struct WorkerPool {
    http: reqwest::Client,
    secret: String,
    workers: Mutex<Vec<Worker>>,
}

impl WorkerPool {
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap_or_default(),
            secret: rooms::gen_uuid(),
            workers: Mutex::new(Vec::new()),
        }
    }

    /// Port of the worker currently running `public_id` (used by the /room redirect).
    pub fn worker_for(&self, public_id: &str) -> Option<u16> {
        self.workers
            .lock()
            .unwrap()
            .iter()
            .find(|w| w.room.as_deref() == Some(public_id))
            .map(|w| w.port)
    }

    /// Claim a free worker (marks it busy). None if all are running a battle.
    fn take_free(&self) -> Option<u16> {
        let mut ws = self.workers.lock().unwrap();
        let w = ws.iter_mut().find(|w| !w.busy)?;
        w.busy = true;
        Some(w.port)
    }

    fn set(&self, port: u16, room: Option<String>, busy: bool) {
        if let Some(w) = self.workers.lock().unwrap().iter_mut().find(|w| w.port == port) {
            w.room = room;
            w.busy = busy;
        }
    }
}

impl Default for WorkerPool {
    fn default() -> Self {
        Self::new()
    }
}

/// Spawn the worker pool, start the coordinator matchmaker + status poller, then serve.
pub async fn run_coordinator(
    state: AppState,
    n: usize,
    rom: String,
    core: String,
    port: u16,
    db_url: String,
) -> anyhow::Result<()> {
    let pool = state.pool.clone().expect("coordinator requires a pool");
    let coord_origin = format!("http://localhost:{port}");
    let exe = std::env::current_exe()?;

    for i in 0..n {
        let wport = port + 1 + i as u16;
        match Command::new(&exe)
            .arg(&rom)
            .arg(&core)
            .arg("--worker")
            .arg("--port")
            .arg(wport.to_string())
            .env("DATABASE_URL", &db_url)
            .env("INTERNAL_SECRET", &pool.secret)
            .env("COORDINATOR_ORIGIN", &coord_origin)
            .env("RUST_LOG", "nes_web=warn,webrtc=off")
            .spawn()
        {
            Ok(child) => {
                pool.workers.lock().unwrap().push(Worker { port: wport, child, busy: false, room: None });
                tracing::info!("coordinator: spawned worker on :{wport}");
            }
            Err(e) => tracing::error!("coordinator: spawn worker :{wport} failed: {e}"),
        }
    }

    // Wait until every worker answers /internal/status (emulator booted + ROM loaded).
    let ports: Vec<u16> = pool.workers.lock().unwrap().iter().map(|w| w.port).collect();
    for p in &ports {
        let url = format!("http://127.0.0.1:{p}/internal/status?secret={}", pool.secret);
        let mut ready = false;
        for _ in 0..80 {
            tokio::time::sleep(Duration::from_millis(400)).await;
            if let Ok(r) = pool.http.get(&url).send().await {
                if r.status().is_success() {
                    ready = true;
                    break;
                }
            }
        }
        tracing::info!("coordinator: worker :{p} ready={ready}");
    }

    tokio::spawn(status_poller(pool.clone()));
    tokio::spawn(matchmaker(state.clone(), pool.clone()));

    // Best-effort: kill workers when the coordinator is Ctrl-C'd.
    {
        let pool = pool.clone();
        tokio::spawn(async move {
            let _ = tokio::signal::ctrl_c().await;
            for w in pool.workers.lock().unwrap().iter_mut() {
                let _ = w.child.kill();
            }
            std::process::exit(0);
        });
    }

    let app = crate::signaling::router(state);
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("▶  COORDINATOR on http://localhost:{port}  ({n} emulator workers)");
    axum::serve(listener, app).await?;
    Ok(())
}

/// Pair queued players and hand each match to a free worker; notify both players.
async fn matchmaker(state: AppState, pool: Arc<WorkerPool>) {
    let game = state.game.clone();
    loop {
        tokio::time::sleep(Duration::from_millis(250)).await;
        loop {
            // Need both a waiting pair AND a free worker; otherwise wait.
            if game.queue.lock().await.len() < 2 {
                break;
            }
            let wport = match pool.take_free() {
                Some(p) => p,
                None => break, // all workers busy — leave players queued
            };
            let pair = {
                let mut q = game.queue.lock().await;
                if q.len() >= 2 {
                    Some((q.pop_front().unwrap(), q.pop_front().unwrap()))
                } else {
                    None
                }
            };
            let (a, b) = match pair {
                Some((a, b)) if a != b => (a, b),
                _ => {
                    pool.set(wport, None, false); // release the worker we claimed
                    break;
                }
            };
            match assign_to_worker(&pool, wport, a, b).await {
                Some(public_id) => {
                    pool.set(wport, Some(public_id.clone()), true);
                    let ua = rooms::username_of(&game.db, a).await;
                    let ub = rooms::username_of(&game.db, b).await;
                    game.ws.send_to(a, json!({"type":"matched","room_id":public_id,"seat":1,"opponent":ub})).await;
                    game.ws.send_to(b, json!({"type":"matched","room_id":public_id,"seat":2,"opponent":ua})).await;
                    tracing::info!("coordinator: {a} vs {b} -> worker :{wport} room {public_id}");
                }
                None => {
                    pool.set(wport, None, false);
                    let mut q = game.queue.lock().await;
                    q.push_front(b);
                    q.push_front(a);
                    tracing::warn!("coordinator: assign to :{wport} failed; requeued {a},{b}");
                    break;
                }
            }
        }
    }
}

async fn assign_to_worker(pool: &WorkerPool, port: u16, a: UserId, b: UserId) -> Option<String> {
    let url = format!("http://127.0.0.1:{port}/internal/assign");
    let resp = pool
        .http
        .post(&url)
        .json(&json!({ "p1": a, "p2": b, "secret": pool.secret }))
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let v: serde_json::Value = resp.json().await.ok()?;
    v.get("public_id").and_then(|x| x.as_str()).map(|s| s.to_string())
}

/// Poll each worker's status; when a busy worker is no longer running a battle, free it (so it can
/// take the next match) and forget its room (so /room?id=that falls back to the result page).
async fn status_poller(pool: Arc<WorkerPool>) {
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let ports: Vec<u16> = pool.workers.lock().unwrap().iter().map(|w| w.port).collect();
        for p in ports {
            let url = format!("http://127.0.0.1:{p}/internal/status?secret={}", pool.secret);
            if let Ok(r) = pool.http.get(&url).send().await {
                if let Ok(v) = r.json::<serde_json::Value>().await {
                    let busy = v.get("busy").and_then(|x| x.as_bool()).unwrap_or(false);
                    if !busy {
                        pool.set(p, None, false);
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Worker-side internal endpoints (secret-gated; mounted only in --worker mode)
// ---------------------------------------------------------------------------

fn internal_ok(secret: &str) -> bool {
    std::env::var("INTERNAL_SECRET").map(|s| !s.is_empty() && s == secret).unwrap_or(false)
}

#[derive(Deserialize)]
pub struct AssignReq {
    pub p1: UserId,
    pub p2: UserId,
    #[serde(default)]
    pub secret: String,
}

/// POST /internal/assign {p1, p2, secret} -> create + start the battle here; return its UUID.
pub async fn assign_handler(State(st): State<AppState>, Json(req): Json<AssignReq>) -> Response {
    if !internal_ok(&req.secret) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match rooms::assign_battle(&st.game, req.p1, req.p2).await {
        Some(public_id) => Json(json!({ "public_id": public_id })).into_response(),
        None => (StatusCode::INTERNAL_SERVER_ERROR, "assign failed").into_response(),
    }
}

#[derive(Deserialize)]
pub struct SecretQ {
    #[serde(default)]
    pub secret: String,
}

/// GET /internal/status?secret=... -> {busy} (true while a battle is running on this worker).
pub async fn status_handler(State(st): State<AppState>, Query(q): Query<SecretQ>) -> Response {
    if !internal_ok(&q.secret) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let busy = st.game.active_room.lock().await.is_some();
    Json(json!({ "busy": busy })).into_response()
}
