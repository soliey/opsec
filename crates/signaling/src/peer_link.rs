//! [`SupabaseRealtimeLink`]: the real, cross-machine implementation of
//! [`consent::transport::PeerLink`] — see that trait's module doc for the
//! one accepted gap (`PeerMessage`s before `Active` cross the relay
//! unauthenticated, secured only by the topic hash plus the handshake
//! state machine's own code matching).
//!
//! Also exposes the raw `"sdp"` event on the same joined channel
//! (`send_sdp`/`try_recv_sdp`) for `transport::webrtc_media` to carry the
//! HMAC-sealed SDP offer/answer and ICE candidates once the session is
//! `Active` — the plan calls for one topic join per session, not two, to
//! keep RLS/connection overhead minimal.

use crate::client::RealtimeChannel;
use consent::transport::PeerLink;
use consent::PeerMessage;
use serde_json::Value;

pub struct SupabaseRealtimeLink {
    channel: RealtimeChannel,
}

impl SupabaseRealtimeLink {
    /// Joins the Broadcast channel for `topic` (see `crate::topic::
    /// topic_for_code`). Construction never blocks — the actual WebSocket
    /// connect/join happens on a background task on `handle`.
    pub fn connect(handle: &tokio::runtime::Handle, project_url: &str, publishable_key: &str, topic: &str) -> Self {
        Self { channel: RealtimeChannel::join(handle, project_url, publishable_key, topic) }
    }

    /// Sends a sealed SDP/ICE envelope (as JSON) on the `"sdp"` event of
    /// this same channel. See `transport::webrtc_media`, the intended
    /// caller.
    pub fn send_sdp(&self, payload: Value) {
        self.channel.send_sdp(payload);
    }

    /// Non-blocking: `None` if nothing has arrived on `"sdp"` yet.
    pub fn try_recv_sdp(&self) -> Option<Value> {
        self.channel.try_recv_sdp()
    }
}

impl transport::webrtc_media::SdpChannel for SupabaseRealtimeLink {
    fn send_sdp(&self, payload: Value) {
        SupabaseRealtimeLink::send_sdp(self, payload);
    }

    fn try_recv_sdp(&self) -> Option<Value> {
        SupabaseRealtimeLink::try_recv_sdp(self)
    }
}

impl PeerLink for SupabaseRealtimeLink {
    fn send(&self, message: PeerMessage) {
        let Ok(payload) = serde_json::to_value(&message) else { return };
        self.channel.send_handshake(payload);
    }

    fn try_recv(&self) -> Option<PeerMessage> {
        let payload = self.channel.try_recv_handshake()?;
        // A malformed/tampered message here is simply dropped — there's no
        // way to distinguish "not for us" from "corrupted" at this layer,
        // and either way it's not a valid `PeerMessage` to hand the state
        // machine. The caller's next poll just tries again.
        serde_json::from_value(payload).ok()
    }
}
