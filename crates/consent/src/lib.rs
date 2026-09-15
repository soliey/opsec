//! Consent handshake — the auditable core of the mutual-consent gate.
//!
//! This crate has exactly one job: decide, from explicit user actions on
//! both machines, whether a session is allowed to become
//! [`SessionState::Active`]. It knows nothing about screen capture, input
//! injection, or streaming — those are later phases, gated on `is_active()`
//! from outside this crate. [`HandshakeMachine::session_key`] is the one
//! intentional extension of that authority into phase 4: it turns an
//! already-confirmed [`SessionCode`] into transport key material, so the
//! transport layer authenticates itself off the *same* mutually-confirmed
//! secret instead of re-deriving trust on its own — see its doc comment
//! and `SessionCode::derive_key`, which it's the only way to reach.
//!
//! Start reading at [`handshake`] — that module and its tests are the
//! entire consent guarantee.

mod code;
mod handshake;
pub mod transport;

pub use code::{CodeError, SessionCode, SessionKey, CODE_LEN, SESSION_KEY_LEN};
pub use handshake::{
    EndReason, HandshakeError, HandshakeMachine, LocalEvent, PeerMessage, Role, SessionState,
    DISCLOSURE_TEXT,
};
