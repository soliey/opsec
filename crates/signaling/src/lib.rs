//! Real cross-machine transport for handshake signaling and SDP/ICE
//! exchange — a hand-rolled Phoenix-channel-over-WebSocket client speaking
//! Supabase Realtime Broadcast (no official Rust SDK exists). This is the
//! one place session-related bytes actually leave the machine before a P2P
//! WebRTC connection exists; `consent` itself stays networking-free per its
//! own doc comments.
//!
//! Async only inside this crate: `client::RealtimeChannel` runs its
//! WebSocket connection on a caller-supplied `tokio::runtime::Handle` and
//! exposes a fully synchronous, non-blocking `send`/`try_recv_*` surface —
//! the same shape `consent::transport::PeerLink` already established — so
//! nothing outside `signaling` (and `transport::webrtc_media`, which reuses
//! the same `RealtimeChannel` for SDP/ICE) needs to touch `async fn`.
//!
//! [`SUPABASE_URL`]/[`SUPABASE_PUBLISHABLE_KEY`] point at a live project
//! (`roadto36`) reused for development/testing — **not** a dedicated
//! project for this app. Swap these for a dedicated project's URL/key
//! before shipping to real users; the publishable key is safe to embed (it
//! only grants what the Realtime Authorization RLS policies allow, scoped
//! to the `remote-assist:*` topic prefix) but sharing infrastructure with
//! an unrelated production app is a testing-only convenience, not a
//! long-term choice.

mod client;
mod peer_link;
pub mod topic;

pub use peer_link::SupabaseRealtimeLink;

/// TESTING ONLY — see this module's doc comment.
pub const SUPABASE_URL: &str = "https://ieempujvrzlrchvzdfgn.supabase.co";
/// TESTING ONLY — see this module's doc comment. A Supabase publishable
/// key, not a secret: it only grants what RLS allows.
pub const SUPABASE_PUBLISHABLE_KEY: &str = "sb_publishable_OB_K5t3SAF7w9kXAYXwPWA_29LL5bu4";
