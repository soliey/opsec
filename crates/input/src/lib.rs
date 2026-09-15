//! Phase 3: remote input injection, hard-gated on live consent state.
//!
//! This crate has exactly one job on the injection side: turn a requested
//! mouse move/click or key press into real OS input — via `SendInput` on
//! Windows, `CGEvent` on macOS, nothing else — while never sending a single
//! event unless the session is, at that exact instant, `consent::
//! SessionState::Active`. See [`natural_input`] for the gate itself; that
//! module and its tests are the entire injection-side guarantee, the same
//! way `consent::handshake` is the entire activation guarantee.
//!
//! [`motion`] and [`timing`] are pure, OS-independent generators for
//! human-paced mouse paths and click timing — no window handles, no
//! syscalls, fully unit-testable. [`backend`] is the only place that talks
//! to the OS.
//!
//! This crate deliberately does not depend on `consent`: [`natural_input::
//! SessionGate`] is a trait any live boolean oracle can implement, so
//! `consent::HandshakeMachine` (or a `Mutex` wrapping one, as in
//! `crates/desktop`) is wired in by the caller, not baked in here. This
//! crate's own tests pull in `consent` as a dev-dependency to prove the
//! gate against the real state machine rather than a stand-in.

pub mod backend;
pub mod motion;
pub mod natural_input;
pub mod timing;

pub use backend::{InputBackend, KeyCode, MouseButton};
pub use natural_input::{InputError, NaturalInput, SessionGate};
pub use settings::InputFeel;
