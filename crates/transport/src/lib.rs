//! Phase 4: encrypted transport and screen streaming.
//!
//! # What's real here
//!
//! - [`signaling`] — authenticates session-setup messages (SDP-equivalent)
//!   with an HMAC keyed by [`consent::HandshakeMachine::session_key`], so a
//!   signaling relay that isn't the confirmed peer can't forge or tamper
//!   with connection setup. Pure, fully tested.
//! - [`bitrate`] — [`bitrate::AdaptiveBitrateController`]: AIMD congestion
//!   control (cut hard on loss, climb back slowly, never spike), capped by
//!   `settings::BandwidthProfile`. Pure, fully tested.
//! - [`still_screen`] — [`still_screen::StillScreenPacer`]: drops send rate
//!   toward near-zero while the screen barely changes, snaps back
//!   immediately once it does. Pure, fully tested.
//! - [`loopback`] — [`loopback::LoopbackTransport`]: an in-process,
//!   real-AEAD-encrypted channel standing in for the network, the same way
//!   `consent::transport::LoopbackLink` stands in for real signaling
//!   transport in phase 1. Its "relay" literally cannot decrypt frames
//!   without the session key — see its tests — which is what proves "the
//!   relay never sees plaintext" for real rather than by assertion.
//! - [`encoder`] — [`encoder::VideoEncoder`] trait, a real software H.264
//!   backend via `openh264`, and a real (Windows) hardware-encoder
//!   *availability probe* via Media Foundation's `MFTEnumEx`.
//! - [`webrtc_media`] — [`webrtc_media::WebRtcMediaLink`]: a real, P2P
//!   `RTCPeerConnection` with two `RTCDataChannel`s (`"media"`, `"input"`),
//!   STUN-only ICE (no TURN configured — symmetric-NAT pairs won't connect,
//!   an accepted limitation), negotiated over whatever [`webrtc_media::
//!   SdpChannel`] the caller supplies (in practice, `signaling::
//!   SupabaseRealtimeLink`). Video still rides the data channel as opaque
//!   chunked bytes rather than a proper RTP media track with jitter
//!   buffer/FEC — intentional, since it keeps the existing application-level
//!   encode/pace/decode pipeline unchanged rather than a real limitation to
//!   fix here.
//!
//! # What's honestly not wired up yet
//!
//! The task names `libdatachannel` for P2P DTLS-SRTP + TURN fallback.
//! `libdatachannel`'s Rust bindings (the `datachannel` crate) require
//! building the C++ library via a CMake build script; this sandbox has no
//! `cmake` installed at all, so that path was a dead end — confirmed by
//! actually trying it, not assumed. [`webrtc_media`] uses the pure-Rust
//! `webrtc` crate (webrtc-rs 0.20) instead, which builds cleanly here.
//!
//! Real two-peer ICE/DTLS connectivity across distinct NATs/networks still
//! can't be verified end-to-end from a single sandboxed machine — the same
//! unverifiable-without-real-hardware situation `capture::exclusion::macos`
//! and `input::backend::macos` are already honest about. What *is*
//! verified here: two local processes on one machine, both negotiating over
//! the real (live, internet-facing) Supabase Realtime relay, exercise the
//! full offer/answer/ICE-candidate/data-channel-open path for real —
//! localhost ICE candidates only, not real cross-NAT server-reflexive
//! negotiation.
//!
//! The actual hardware encode *pipeline* (feeding `IMFTransform` samples on
//! Windows, `VTCompressionSession` on macOS) is a separate, still-open gap:
//! [`encoder`]'s Windows backend really probes for hardware H.264 support
//! (`MFTEnumEx`), but routes bytes through the software encoder either way
//! — see that module's doc comment.

pub mod bitrate;
pub mod encoder;
pub mod frame_prep;
pub mod ice;
pub mod loopback;
pub mod signaling;
pub mod still_screen;
pub mod webrtc_media;
