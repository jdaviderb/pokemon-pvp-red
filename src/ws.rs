//! Per-client WebSocket: lobby/queue/room/battle realtime events. Auth'd from the session cookie
//! on the upgrade GET. Media (video/audio) stays on the separate WebRTC `/offer` path; this WS
//! carries only JSON game events. Messages are free-form `serde_json::Value` tagged with `type`.

use std::collections::HashMap;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Response;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::{mpsc, Mutex};

use crate::auth::AuthUser;
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

    async fn add(&self, uid: i32, tx: mpsc::UnboundedSender<serde_json::Value>) {
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

/// GET /ws — authenticated WebSocket upgrade.
pub async fn ws_upgrade(
    AuthUser(user): AuthUser,
    State(st): State<AppState>,
    ws: WebSocketUpgrade,
) -> Response {
    let uid = user.id;
    let uname = user.username.clone();
    ws.on_upgrade(move |socket| handle_socket(socket, st, uid, uname))
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
