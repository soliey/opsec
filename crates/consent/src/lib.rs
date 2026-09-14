//! Consent handshake — the auditable core of the mutual-consent gate.
//!
//! This crate has exactly one job: decide, from explicit user actions on
//! both machines, whether a session is allowed to become
//! [`SessionState::Active`]. It knows nothing about screen capture, input
//! injection, or streaming — those are later phases, gated on `is_active()`
//! from outside this crate.
//!
//! Start reading at [`handshake`] — that module and its tests are the
//! entire consent guarantee.

mod code;
mod handshake;
pub mod transport;

pub use code::{CodeError, SessionCode, CODE_LEN};
pub use handshake::{
    EndReason, HandshakeError, HandshakeMachine, LocalEvent, PeerMessage, Role, SessionState,
    DISCLOSURE_TEXT,
};
