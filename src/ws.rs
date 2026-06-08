//! Per-client WebSocket: lobby/queue/room/battle realtime events. Auth'd from the session cookie
//! on the upgrade GET. Media (video/audio) stays on the separate WebRTC `/offer` path; this WS
//! carries only JSON game events. Messages are free-form `serde_json::Value` tagged with `type`.

use std::collections::HashMap;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum_extra::extract::cookie::PrivateCookieJar;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::{mpsc, Mutex};

use crate::signaling::AppState;

/// Registry of live WS senders, keyed by user id (a user may have several tabs).
#[derive(Default)]
pub struct WsHub {
    conns: Mutex<HashMap<i32, Vec<mpsc::UnboundedSender<serde_json::Value>>>>,
}

impl WsHub {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a sender for `uid` (a browser tab, or an in-process MCP agent session).
    pub async fn add(&self, uid: i32, tx: mpsc::UnboundedSender<serde_json::Value>) {
        self.conns.lock().await.entry(uid).or_default().push(tx);
    }

    /// Fan a message out to every live tab of `uid`; prune senders whose receiver is gone.
    pub async fn send_to(&self, uid: i32, msg: serde_json::Value) {
        let mut c = self.conns.lock().await;
        if let Some(v) = c.get_mut(&uid) {
            v.retain(|t| t.send(msg.clone()).is_ok());
            if v.is_empty() {
                c.remove(&uid);
            }
        }
    }

    async fn prune(&self, uid: i32) {
        let mut c = self.conns.lock().await;
        if let Some(v) = c.get_mut(&uid) {
            v.retain(|t| !t.is_closed());
            if v.is_empty() {
                c.remove(&uid);
            }
        }
    }
}

#[derive(serde::Deserialize)]
pub struct WsQuery {
    /// Agent/MCP auth: connect with `?token=mcp_...` instead of a browser session cookie.
    pub token: Option<String>,
    /// Coordinator only: `?room=<uuid>` marks this as a BATTLE connection — the coordinator bridges
    /// it to the worker process running that room. Lobby/matchmaking connections omit it.
    pub room: Option<String>,
}

/// GET /ws — authenticated WebSocket upgrade. A browser authenticates via the session cookie; an
/// AI agent (the MCP server) authenticates via `?token=` (so it needs no cookie). Same protocol.
pub async fn ws_upgrade(
    State(st): State<AppState>,
    jar: PrivateCookieJar,
    Query(q): Query<WsQuery>,
    headers: axum::http::HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let user = match q.token.as_deref() {
        Some(t) => crate::auth::user_from_token(&st, t).await,
        None => crate::auth::user_from_jar(&st, &jar).await,
    };
    let Some(u) = user else {
        return (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
    };

    // Coordinator + ?room=<uuid>: a BATTLE connection. The battle runs on a worker process on an
    // internal port (not exposed behind TLS), so the coordinator transparently bridges this socket
    // to that worker's /ws — every battle flows through the one public origin. Auth (token and/or
    // session cookie) is forwarded; workers share the cookie key, so a forwarded cookie authenticates.
    if st.role == crate::signaling::Role::Coordinator {
        if let Some(room) = q.room.as_deref() {
            if let Some(port) = st.pool.as_ref().and_then(|p| p.worker_for(room)) {
                let token = q.token.clone();
                let cookie =
                    headers.get("cookie").and_then(|v| v.to_str().ok()).map(|s| s.to_string());
                return ws.on_upgrade(move |socket| proxy_ws(socket, port, token, cookie));
            }
        }
    }

    let (uid, uname) = (u.id, u.username.clone());
    ws.on_upgrade(move |socket| handle_socket(socket, st, uid, uname))
}

/// Bridge a client battle WebSocket to the worker process running its room (coordinator only). The
/// client only ever talks to the public origin; we pump frames both ways to the worker's `/ws`,
/// forwarding the caller's auth (token query and/or session cookie).
async fn proxy_ws(client: WebSocket, port: u16, token: Option<String>, cookie: Option<String>) {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::Message as TMsg;

    let mut url = format!("ws://127.0.0.1:{port}/ws");
    if let Some(t) = token.as_deref() {
        url.push_str("?token=");
        url.push_str(t);
    }
    let mut req = match url.into_client_request() {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("proxy_ws: bad worker url: {e}");
            return;
        }
    };
    if let Some(c) = cookie {
        if let Ok(v) = c.parse() {
            req.headers_mut().insert("cookie", v);
        }
    }
    let worker = match tokio_tungstenite::connect_async(req).await {
        Ok((ws, _)) => ws,
        Err(e) => {
            tracing::warn!("proxy_ws: connect worker :{port} failed: {e}");
            return;
        }
    };
    let (mut csink, mut cstream) = client.split();
    let (mut wsink, mut wstream) = worker.split();

    let c2w = async {
        while let Some(Ok(msg)) = cstream.next().await {
            let out = match msg {
                Message::Text(t) => TMsg::Text(t.as_str().to_string().into()),
                Message::Binary(b) => TMsg::Binary(b.to_vec().into()),
                Message::Ping(p) => TMsg::Ping(p.to_vec().into()),
                Message::Pong(p) => TMsg::Pong(p.to_vec().into()),
                Message::Close(_) => break,
            };
            if wsink.send(out).await.is_err() {
                break;
            }
        }
        let _ = wsink.send(TMsg::Close(None)).await;
    };
    let w2c = async {
        while let Some(Ok(msg)) = wstream.next().await {
            let out = match msg {
                TMsg::Text(t) => Message::Text(t.as_str().to_string().into()),
                TMsg::Binary(b) => Message::Binary(b.to_vec().into()),
                TMsg::Ping(p) => Message::Ping(p.to_vec().into()),
                TMsg::Pong(p) => Message::Pong(p.to_vec().into()),
                TMsg::Close(_) => break,
                TMsg::Frame(_) => continue,
            };
            if csink.send(out).await.is_err() {
                break;
            }
        }
    };
    tokio::select! { _ = c2w => {}, _ = w2c => {} }
}

async fn handle_socket(socket: WebSocket, st: AppState, uid: i32, uname: String) {
    let (mut sink, mut stream) = socket.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<serde_json::Value>();
    st.game.ws.add(uid, tx).await;

    // Outbound pump: hub -> this socket.
    let send_task = tokio::spawn(async move {
        while let Some(v) = rx.recv().await {
            if sink.send(Message::Text(v.to_string().into())).await.is_err() {
                break;
            }
        }
    });

    // Initial paint (hello + lobby/room state).
    crate::rooms::on_connect(&st.game, uid, &uname).await;

    // Inbound: client intents.
    while let Some(Ok(msg)) = stream.next().await {
        match msg {
            Message::Text(t) => {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(t.as_str()) {
                    crate::rooms::handle_client_msg(&st.game, uid, &uname, v).await;
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }

    send_task.abort();
    st.game.ws.prune(uid).await;
    // Drop a disconnecting player from the matchmaking queue so they aren't matched as a "ghost"
    // (which would spawn a worker for someone who already left).
    crate::rooms::cancel_queue(&st.game, uid).await;
}
