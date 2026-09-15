//! A minimal Phoenix-channel-over-WebSocket client speaking Supabase
//! Realtime's wire protocol (v1, object-framed) — enough to join exactly
//! one *private* Broadcast channel and exchange JSON on two known event
//! names, `"handshake"` and `"sdp"`. Not a general Phoenix/Supabase client:
//! no presence, no Postgres Changes, no reconnect-with-backoff — a dropped
//! connection here just means both `try_recv_*` methods go quiet, the same
//! way a stalled network link would for `consent::transport::LoopbackLink`.
//!
//! `config.private = true` on the join payload is load-bearing, not
//! decorative: Supabase Realtime Authorization (the RLS policies on
//! `realtime.messages`) is only enforced for channels joined as private —
//! a non-private join bypasses RLS entirely. See the migration applied
//! alongside this crate for the actual policies.

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc as std_mpsc;
use std::time::Duration;
use tokio::sync::mpsc as tokio_mpsc;
use tokio_tungstenite::tungstenite::Message;

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(25);
const HANDSHAKE_EVENT: &str = "handshake";
const SDP_EVENT: &str = "sdp";

/// One joined Realtime Broadcast channel, running its own background tokio
/// task. `send`/`try_recv_handshake`/`try_recv_sdp` are the whole surface —
/// deliberately synchronous and non-blocking so callers (the `PeerLink` and
/// `MediaLink` impls built on top of this) never touch async code.
///
/// The two receivers are `Mutex`-wrapped purely to make this type `Sync`:
/// `std::sync::mpsc::Receiver` is `Send` but not `Sync`, and this type is
/// shared behind a plain `Arc` (a `Session` clones it into concurrent
/// contexts — the handshake-pump thread, and `transport::webrtc_media`'s
/// background ICE-pump task — with no outer `Mutex` serializing every
/// caller), unlike `consent::transport::LoopbackLink`, which only ever
/// sits inside a single owning `Mutex`. Actual lock contention is
/// negligible: every call is a single non-blocking `try_recv`.
pub struct RealtimeChannel {
    outbound: tokio_mpsc::UnboundedSender<(&'static str, Value)>,
    handshake_rx: std::sync::Mutex<std_mpsc::Receiver<Value>>,
    sdp_rx: std::sync::Mutex<std_mpsc::Receiver<Value>>,
}

impl RealtimeChannel {
    /// Joins `topic` (already hashed from the session code — see
    /// `crate::topic`) on `project_url`'s Realtime endpoint, authenticated
    /// with `publishable_key`. Connects and joins on a background task
    /// spawned onto `handle`; construction itself never blocks.
    pub fn join(handle: &tokio::runtime::Handle, project_url: &str, publishable_key: &str, topic: &str) -> Self {
        let (outbound_tx, outbound_rx) = tokio_mpsc::unbounded_channel();
        let (handshake_tx, handshake_rx) = std_mpsc::channel();
        let (sdp_tx, sdp_rx) = std_mpsc::channel();

        let ws_url = websocket_url(project_url, publishable_key);
        let full_topic = format!("realtime:{topic}");

        handle.spawn(run_channel(ws_url, full_topic, outbound_rx, handshake_tx, sdp_tx));

        Self {
            outbound: outbound_tx,
            handshake_rx: std::sync::Mutex::new(handshake_rx),
            sdp_rx: std::sync::Mutex::new(sdp_rx),
        }
    }

    /// Broadcasts `payload` on `"handshake"` or `"sdp"`. Fire-and-forget,
    /// matching `PeerLink::send`'s semantics — nothing actionable to do if
    /// the background task has already given up.
    fn send(&self, event: &'static str, payload: Value) {
        let _ = self.outbound.send((event, payload));
    }

    pub fn send_handshake(&self, payload: Value) {
        self.send(HANDSHAKE_EVENT, payload);
    }

    pub fn send_sdp(&self, payload: Value) {
        self.send(SDP_EVENT, payload);
    }

    pub fn try_recv_handshake(&self) -> Option<Value> {
        self.handshake_rx.lock().ok()?.try_recv().ok()
    }

    pub fn try_recv_sdp(&self) -> Option<Value> {
        self.sdp_rx.lock().ok()?.try_recv().ok()
    }
}

fn websocket_url(project_url: &str, publishable_key: &str) -> String {
    let ws_base = project_url
        .trim_end_matches('/')
        .replacen("https://", "wss://", 1)
        .replacen("http://", "ws://", 1);
    format!("{ws_base}/realtime/v1/websocket?apikey={publishable_key}&vsn=1.0.0")
}

async fn run_channel(
    ws_url: String,
    topic: String,
    mut outbound_rx: tokio_mpsc::UnboundedReceiver<(&'static str, Value)>,
    handshake_tx: std_mpsc::Sender<Value>,
    sdp_tx: std_mpsc::Sender<Value>,
) {
    let Ok((ws_stream, _)) = tokio_tungstenite::connect_async(&ws_url).await else {
        return;
    };
    let (mut write, mut read) = ws_stream.split();
    let next_ref = AtomicU64::new(1);
    let mk_ref = || next_ref.fetch_add(1, Ordering::Relaxed).to_string();

    let join = json!({
        "topic": topic,
        "event": "phx_join",
        "payload": {
            "config": {
                "broadcast": { "self": false, "ack": false },
                "presence": { "key": "" },
                "private": true,
            }
        },
        "ref": mk_ref(),
    });
    if write.send(Message::Text(join.to_string())).await.is_err() {
        return;
    }

    let mut heartbeat = tokio::time::interval(HEARTBEAT_INTERVAL);
    heartbeat.tick().await; // first tick fires immediately; the join above already proved the socket's alive

    loop {
        tokio::select! {
            _ = heartbeat.tick() => {
                let hb = json!({ "topic": "phoenix", "event": "heartbeat", "payload": {}, "ref": mk_ref() });
                if write.send(Message::Text(hb.to_string())).await.is_err() {
                    return;
                }
            }
            outgoing = outbound_rx.recv() => {
                let Some((event, payload)) = outgoing else { return };
                let msg = json!({
                    "topic": topic,
                    "event": "broadcast",
                    "payload": { "type": "broadcast", "event": event, "payload": payload },
                    "ref": mk_ref(),
                });
                if write.send(Message::Text(msg.to_string())).await.is_err() {
                    return;
                }
            }
            incoming = read.next() => {
                let Some(Ok(Message::Text(text))) = incoming else {
                    if incoming.is_none() { return; } // socket closed
                    continue; // ping/pong/binary/close frame, or a read error — nothing to route
                };
                let Ok(frame) = serde_json::from_str::<Value>(&text) else { continue };
                if frame.get("event").and_then(Value::as_str) != Some("broadcast") {
                    continue; // phx_reply, presence_state, etc. — not something we act on
                }
                let Some(payload) = frame.get("payload") else { continue };
                let inner = payload.get("payload").cloned().unwrap_or(Value::Null);
                match payload.get("event").and_then(Value::as_str) {
                    Some(HANDSHAKE_EVENT) => { let _ = handshake_tx.send(inner); }
                    Some(SDP_EVENT) => { let _ = sdp_tx.send(inner); }
                    _ => {} // an event name from a future extension we don't know about yet
                }
            }
        }
    }
}
