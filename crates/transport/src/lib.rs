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
//!
//! # What's honestly not wired up yet
//!
//! The task names `libdatachannel` for P2P DTLS-SRTP + TURN fallback.
//! `libdatachannel`'s Rust bindings (the `datachannel` crate) require
//! building the C++ library via a CMake build script; this sandbox has no
//! `cmake` installed at all, so that path is a dead end here — confirmed by
//! actually trying it, not assumed (`cargo check` fails immediately with
//! "program not found: cmake").
//!
//! The pure-Rust alternative, `webrtc` (webrtc-rs 0.20), *does* build
//! cleanly here (verified) and covers the same ICE/DTLS/SRTP/SCTP/TURN
//! surface. It was not wired into a working `PeerConnection` in this phase:
//! its 0.20 API is a substantial, unfamiliar, builder/trait-based surface
//! (custom pluggable async runtime, `PeerConnectionEventHandler` callbacks)
//! that would take real trial-and-error to get right, and — more
//! fundamentally — this is a single sandboxed machine, so even a
//! byte-perfect implementation of real two-peer ICE/DTLS connectivity
//! couldn't be verified end-to-end here, the same unverifiable-without-
//! real-hardware situation `capture::exclusion::macos` and
//! `input::backend::macos` are already honest about. [`ice`] builds the
//! one piece of that integration that *is* real, pure, and testable without
//! a live peer: the ICE server list (STUN default + TURN relay fallback,
//! per `settings`), shaped to drop straight into `webrtc::peer_connection::
//! transport::RTCIceServer` when a future phase wires up a real
//! `PeerConnectionBuilder` — see that module's doc comment for the mapping.
//!
//! The actual hardware encode *pipeline* (feeding `IMFTransform` samples on
//! Windows, `VTCompressionSession` on macOS) is the same kind of gap:
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
