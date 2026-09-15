//! The mutual-consent state machine.
//!
//! This is the one place in the codebase that is allowed to decide whether a
//! session is [`SessionState::Active`]. Read this file to audit the whole
//! consent guarantee; nothing outside it can shortcut the result.
//!
//! # The invariant
//!
//! `Active` is reachable **only** by a [`HandshakeMachine`] that has seen
//! both:
//!   1. a [`LocalEvent::Confirm`] from the human sitting at *this* machine, and
//!   2. a [`PeerMessage::Confirm`] carrying the *same* [`SessionCode`],
//!      which by construction only exists on a peer that itself received an
//!      equivalent local Confirm.
//!
//! Those two facts can arrive in either order (the network gives no
//! ordering guarantee), so the machine tracks a pending peer confirmation
//! when it arrives early, but it never activates on one alone. See
//! `tests/no_session_without_both_confirmations.rs` for exhaustive proof.
//!
//! A single global hotkey event ([`LocalEvent::HotkeyEnd`] /
//! [`PeerMessage::HotkeyEnd`]) ends the session from *any* state, including
//! `Active`, and the resulting [`SessionState::Ended`] is terminal — a new
//! `HandshakeMachine` (i.e. a brand new code and a brand new pair of
//! confirmations) is required to connect again.

use crate::code::{SessionCode, SessionKey};
use std::fmt;

/// Shown on the pre-connection screen on both machines, verbatim, before the
/// Confirm button is enabled. Kept as one constant so product/legal copy
/// changes happen in exactly one place and stay in sync between host and
/// helper builds.
pub const DISCLOSURE_TEXT: &str = "\
Starting this session will let the other person see your screen and control \
your mouse and keyboard. Nothing is shared until you both enter the same \
session code and press Confirm. You can end the session instantly, at any \
time, by pressing the global hotkey on either computer.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Host,
    Helper,
}

/// Where a single machine's side of the handshake currently stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionState {
    /// Host: no code generated yet. Helper: no code typed in yet.
    AwaitingCode,
    /// A code is known locally (shown, or typed) but this human has not yet
    /// pressed Confirm on the disclosure screen.
    AwaitingLocalConfirmation { code: SessionCode },
    /// This human pressed Confirm; waiting on the peer to do the same with
    /// a matching code.
    AwaitingRemoteConfirmation { code: SessionCode },
    /// Both sides confirmed with matching codes. The only state in which
    /// later phases (capture/input/streaming) are permitted to run.
    Active { code: SessionCode },
    /// Terminal for this machine instance. Reconnecting means constructing
    /// a new `HandshakeMachine` with a new code.
    Ended { reason: EndReason },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndReason {
    LocalCanceled,
    RemoteCanceled,
    CodeMismatch,
    HotkeyDuringHandshake,
    HotkeyDuringSession,
    PeerDisconnected,
}

/// Actions the human at *this* machine takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalEvent {
    /// Host generates (or regenerates, before confirming) a code to display.
    GenerateCode(SessionCode),
    /// Helper types in the code read to them out-of-band.
    EnterCode(SessionCode),
    /// The Confirm button on the pre-connection disclosure screen.
    Confirm,
    /// The Cancel button, or closing the pre-connection window.
    Cancel,
    /// The global end-session hotkey. Valid from any state, including
    /// `Active`, and even after `Ended` (idempotent no-op).
    HotkeyEnd,
}

/// Messages arriving from the peer over whatever transport is wired up.
/// This is the *only* way remote intent enters the machine — there is no
/// "trust the transport" shortcut into `Active`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerMessage {
    Confirm { code: SessionCode },
    Cancel,
    HotkeyEnd,
    Disconnected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandshakeError {
    /// Confirm was pressed before any code was generated/entered.
    NoCodeYet,
    /// An event that only makes sense before local confirmation arrived
    /// after this side already confirmed (e.g. trying to edit the code).
    AlreadyConfirmedLocally,
    /// The session already ended; construct a new `HandshakeMachine`.
    SessionEnded,
}

impl fmt::Display for HandshakeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HandshakeError::NoCodeYet => write!(f, "no session code yet"),
            HandshakeError::AlreadyConfirmedLocally => {
                write!(f, "this side already confirmed; can't change the code now")
            }
            HandshakeError::SessionEnded => write!(f, "session already ended"),
        }
    }
}

impl std::error::Error for HandshakeError {}

/// One machine's side of a pairing. Host and Helper both run this same
/// state machine; only which events they naturally trigger first differs.
pub struct HandshakeMachine {
    role: Role,
    state: SessionState,
    /// A peer `Confirm` that arrived before *this* side reached
    /// `AwaitingRemoteConfirmation`. Consumed (and checked against the
    /// local code) the moment the local human confirms. This is what makes
    /// activation order-independent without ever activating on one
    /// confirmation alone.
    pending_peer_confirm: Option<SessionCode>,
}

impl HandshakeMachine {
    pub fn new(role: Role) -> Self {
        Self {
            role,
            state: SessionState::AwaitingCode,
            pending_peer_confirm: None,
        }
    }

    pub fn role(&self) -> Role {
        self.role
    }

    pub fn state(&self) -> &SessionState {
        &self.state
    }

    pub fn is_active(&self) -> bool {
        matches!(self.state, SessionState::Active { .. })
    }

    /// Session key material for phase 4's transport, or `None` unless this
    /// machine is `Active`. This is the *only* sanctioned way to get a key
    /// out of a `SessionCode` — [`SessionCode::derive_key`] is
    /// crate-private specifically so routing it through here is not
    /// optional: a key can never be derived before both sides have
    /// actually confirmed the same code, tying transport authorization to
    /// the exact guarantee `apply_local`/`apply_peer` already prove. See
    /// `SessionCode::derive_key` for what `info` is for and what this key
    /// is (and isn't) good for.
    pub fn session_key(&self, info: &[u8]) -> Option<SessionKey> {
        match &self.state {
            SessionState::Active { code } => Some(code.derive_key(info)),
            _ => None,
        }
    }

    pub fn is_ended(&self) -> bool {
        matches!(self.state, SessionState::Ended { .. })
    }

    fn end(&mut self, reason: EndReason) {
        self.pending_peer_confirm = None;
        self.state = SessionState::Ended { reason };
    }

    /// Apply an action taken by the human at this machine.
    pub fn apply_local(&mut self, event: LocalEvent) -> Result<(), HandshakeError> {
        if let SessionState::Ended { .. } = self.state {
            return match event {
                // Pressing the panic hotkey after the session already ended
                // is always safe and never an error.
                LocalEvent::HotkeyEnd => Ok(()),
                _ => Err(HandshakeError::SessionEnded),
            };
        }

        match event {
            LocalEvent::HotkeyEnd => {
                let reason = if self.is_active() {
                    EndReason::HotkeyDuringSession
                } else {
                    EndReason::HotkeyDuringHandshake
                };
                self.end(reason);
                Ok(())
            }

            LocalEvent::Cancel => {
                self.end(EndReason::LocalCanceled);
                Ok(())
            }

            LocalEvent::GenerateCode(code) | LocalEvent::EnterCode(code) => match &self.state {
                SessionState::AwaitingCode | SessionState::AwaitingLocalConfirmation { .. } => {
                    self.state = SessionState::AwaitingLocalConfirmation { code };
                    Ok(())
                }
                SessionState::AwaitingRemoteConfirmation { .. } | SessionState::Active { .. } => {
                    Err(HandshakeError::AlreadyConfirmedLocally)
                }
                SessionState::Ended { .. } => unreachable!("handled above"),
            },

            LocalEvent::Confirm => match &self.state {
                SessionState::AwaitingCode => Err(HandshakeError::NoCodeYet),
                SessionState::AwaitingLocalConfirmation { code } => {
                    let code = code.clone();
                    match self.pending_peer_confirm.take() {
                        Some(peer_code) if peer_code == code => {
                            self.state = SessionState::Active { code };
                        }
                        Some(_mismatched) => {
                            self.end(EndReason::CodeMismatch);
                        }
                        None => {
                            self.state = SessionState::AwaitingRemoteConfirmation { code };
                        }
                    }
                    Ok(())
                }
                SessionState::AwaitingRemoteConfirmation { .. } | SessionState::Active { .. } => {
                    Err(HandshakeError::AlreadyConfirmedLocally)
                }
                SessionState::Ended { .. } => unreachable!("handled above"),
            },
        }
    }

    /// Apply a message that arrived from the peer over the transport.
    pub fn apply_peer(&mut self, message: PeerMessage) -> Result<(), HandshakeError> {
        if let SessionState::Ended { .. } = self.state {
            return Err(HandshakeError::SessionEnded);
        }

        match message {
            PeerMessage::HotkeyEnd => {
                let reason = if self.is_active() {
                    EndReason::HotkeyDuringSession
                } else {
                    EndReason::HotkeyDuringHandshake
                };
                self.end(reason);
            }
            PeerMessage::Cancel => self.end(EndReason::RemoteCanceled),
            PeerMessage::Disconnected => self.end(EndReason::PeerDisconnected),
            PeerMessage::Confirm { code: peer_code } => match &self.state {
                SessionState::AwaitingRemoteConfirmation { code } => {
                    if *code == peer_code {
                        self.state = SessionState::Active { code: code.clone() };
                    } else {
                        self.end(EndReason::CodeMismatch);
                    }
                }
                SessionState::Active { .. } => {
                    // Already active; a duplicate/late confirm is a no-op.
                }
                SessionState::AwaitingCode | SessionState::AwaitingLocalConfirmation { .. } => {
                    // We haven't confirmed locally yet — remember it, but
                    // this alone must never activate the session.
                    self.pending_peer_confirm = Some(peer_code);
                }
                SessionState::Ended { .. } => unreachable!("handled above"),
            },
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(s: &str) -> SessionCode {
        SessionCode::parse(s).unwrap()
    }

    #[test]
    fn fresh_machine_is_awaiting_code() {
        let m = HandshakeMachine::new(Role::Host);
        assert_eq!(*m.state(), SessionState::AwaitingCode);
        assert!(!m.is_active());
    }

    #[test]
    fn confirm_without_code_is_an_error_and_does_not_activate() {
        let mut m = HandshakeMachine::new(Role::Host);
        assert_eq!(m.apply_local(LocalEvent::Confirm), Err(HandshakeError::NoCodeYet));
        assert!(!m.is_active());
    }

    #[test]
    fn local_confirm_alone_never_activates() {
        let mut m = HandshakeMachine::new(Role::Host);
        m.apply_local(LocalEvent::GenerateCode(code("042817"))).unwrap();
        m.apply_local(LocalEvent::Confirm).unwrap();
        assert_eq!(
            *m.state(),
            SessionState::AwaitingRemoteConfirmation { code: code("042817") }
        );
        assert!(!m.is_active());
    }

    #[test]
    fn remote_confirm_alone_never_activates() {
        let mut m = HandshakeMachine::new(Role::Helper);
        m.apply_local(LocalEvent::EnterCode(code("042817"))).unwrap();
        // Peer confirms before we do.
        m.apply_peer(PeerMessage::Confirm { code: code("042817") }).unwrap();
        assert!(!m.is_active());
        assert_eq!(
            *m.state(),
            SessionState::AwaitingLocalConfirmation { code: code("042817") }
        );
    }

    #[test]
    fn matching_confirms_activate_local_then_remote() {
        let mut m = HandshakeMachine::new(Role::Host);
        m.apply_local(LocalEvent::GenerateCode(code("042817"))).unwrap();
        m.apply_local(LocalEvent::Confirm).unwrap();
        assert!(!m.is_active());
        m.apply_peer(PeerMessage::Confirm { code: code("042817") }).unwrap();
        assert!(m.is_active());
        assert_eq!(*m.state(), SessionState::Active { code: code("042817") });
    }

    #[test]
    fn matching_confirms_activate_remote_then_local() {
        let mut m = HandshakeMachine::new(Role::Helper);
        m.apply_local(LocalEvent::EnterCode(code("042817"))).unwrap();
        m.apply_peer(PeerMessage::Confirm { code: code("042817") }).unwrap();
        assert!(!m.is_active());
        m.apply_local(LocalEvent::Confirm).unwrap();
        assert!(m.is_active());
        assert_eq!(*m.state(), SessionState::Active { code: code("042817") });
    }

    #[test]
    fn mismatched_codes_never_activate_remote_then_local() {
        let mut m = HandshakeMachine::new(Role::Helper);
        m.apply_local(LocalEvent::EnterCode(code("042817"))).unwrap();
        m.apply_peer(PeerMessage::Confirm { code: code("999999") }).unwrap();
        m.apply_local(LocalEvent::Confirm).unwrap();
        assert!(!m.is_active());
        assert_eq!(*m.state(), SessionState::Ended { reason: EndReason::CodeMismatch });
    }

    #[test]
    fn mismatched_codes_never_activate_local_then_remote() {
        let mut m = HandshakeMachine::new(Role::Host);
        m.apply_local(LocalEvent::GenerateCode(code("042817"))).unwrap();
        m.apply_local(LocalEvent::Confirm).unwrap();
        m.apply_peer(PeerMessage::Confirm { code: code("999999") }).unwrap();
        assert!(!m.is_active());
        assert_eq!(*m.state(), SessionState::Ended { reason: EndReason::CodeMismatch });
    }

    #[test]
    fn local_cancel_before_confirmation_prevents_activation() {
        let mut m = HandshakeMachine::new(Role::Host);
        m.apply_local(LocalEvent::GenerateCode(code("042817"))).unwrap();
        m.apply_local(LocalEvent::Cancel).unwrap();
        assert_eq!(*m.state(), SessionState::Ended { reason: EndReason::LocalCanceled });
        // Even a later matching peer confirm cannot resurrect it.
        assert_eq!(
            m.apply_peer(PeerMessage::Confirm { code: code("042817") }),
            Err(HandshakeError::SessionEnded)
        );
        assert!(!m.is_active());
    }

    #[test]
    fn remote_cancel_prevents_activation() {
        let mut m = HandshakeMachine::new(Role::Host);
        m.apply_local(LocalEvent::GenerateCode(code("042817"))).unwrap();
        m.apply_local(LocalEvent::Confirm).unwrap();
        m.apply_peer(PeerMessage::Cancel).unwrap();
        assert_eq!(*m.state(), SessionState::Ended { reason: EndReason::RemoteCanceled });
        assert!(!m.is_active());
    }

    #[test]
    fn hotkey_ends_active_session_immediately_and_irreversibly() {
        let mut m = HandshakeMachine::new(Role::Host);
        m.apply_local(LocalEvent::GenerateCode(code("042817"))).unwrap();
        m.apply_local(LocalEvent::Confirm).unwrap();
        m.apply_peer(PeerMessage::Confirm { code: code("042817") }).unwrap();
        assert!(m.is_active());

        m.apply_local(LocalEvent::HotkeyEnd).unwrap();
        assert!(!m.is_active());
        assert_eq!(
            *m.state(),
            SessionState::Ended { reason: EndReason::HotkeyDuringSession }
        );

        // Nothing can resurrect Active on an ended machine.
        assert_eq!(
            m.apply_peer(PeerMessage::Confirm { code: code("042817") }),
            Err(HandshakeError::SessionEnded)
        );
        assert_eq!(m.apply_local(LocalEvent::Confirm), Err(HandshakeError::SessionEnded));
        assert!(!m.is_active());

        // The hotkey itself stays safe to press again (idempotent no-op).
        assert_eq!(m.apply_local(LocalEvent::HotkeyEnd), Ok(()));
        assert!(m.is_ended());
    }

    #[test]
    fn peer_hotkey_ends_session_from_remote_side_too() {
        let mut m = HandshakeMachine::new(Role::Helper);
        m.apply_local(LocalEvent::EnterCode(code("042817"))).unwrap();
        m.apply_local(LocalEvent::Confirm).unwrap();
        m.apply_peer(PeerMessage::Confirm { code: code("042817") }).unwrap();
        assert!(m.is_active());

        m.apply_peer(PeerMessage::HotkeyEnd).unwrap();
        assert!(!m.is_active());
        assert_eq!(
            *m.state(),
            SessionState::Ended { reason: EndReason::HotkeyDuringSession }
        );
    }

    #[test]
    fn hotkey_during_handshake_is_labeled_distinctly_from_hotkey_during_session() {
        let mut m = HandshakeMachine::new(Role::Host);
        m.apply_local(LocalEvent::GenerateCode(code("042817"))).unwrap();
        m.apply_local(LocalEvent::HotkeyEnd).unwrap();
        assert_eq!(
            *m.state(),
            SessionState::Ended { reason: EndReason::HotkeyDuringHandshake }
        );
    }

    #[test]
    fn cannot_change_code_after_confirming_locally() {
        let mut m = HandshakeMachine::new(Role::Host);
        m.apply_local(LocalEvent::GenerateCode(code("042817"))).unwrap();
        m.apply_local(LocalEvent::Confirm).unwrap();
        assert_eq!(
            m.apply_local(LocalEvent::GenerateCode(code("111111"))),
            Err(HandshakeError::AlreadyConfirmedLocally)
        );
        assert_eq!(
            m.apply_local(LocalEvent::Confirm),
            Err(HandshakeError::AlreadyConfirmedLocally)
        );
        assert!(!m.is_active());
    }

    #[test]
    fn helper_can_correct_a_mistyped_code_before_confirming() {
        let mut m = HandshakeMachine::new(Role::Helper);
        m.apply_local(LocalEvent::EnterCode(code("000000"))).unwrap();
        m.apply_local(LocalEvent::EnterCode(code("042817"))).unwrap();
        assert_eq!(
            *m.state(),
            SessionState::AwaitingLocalConfirmation { code: code("042817") }
        );
    }

    #[test]
    fn disconnected_peer_ends_session() {
        let mut m = HandshakeMachine::new(Role::Host);
        m.apply_local(LocalEvent::GenerateCode(code("042817"))).unwrap();
        m.apply_local(LocalEvent::Confirm).unwrap();
        m.apply_peer(PeerMessage::Disconnected).unwrap();
        assert_eq!(*m.state(), SessionState::Ended { reason: EndReason::PeerDisconnected });
        assert!(!m.is_active());
    }

    #[test]
    fn session_key_is_none_before_active_and_some_once_active() {
        let mut m = HandshakeMachine::new(Role::Host);
        assert!(m.session_key(b"purpose-a").is_none());

        m.apply_local(LocalEvent::GenerateCode(code("042817"))).unwrap();
        assert!(m.session_key(b"purpose-a").is_none());

        m.apply_local(LocalEvent::Confirm).unwrap();
        assert!(m.session_key(b"purpose-a").is_none(), "local confirm alone must not unlock a key");

        m.apply_peer(PeerMessage::Confirm { code: code("042817") }).unwrap();
        assert!(m.is_active());
        assert!(m.session_key(b"purpose-a").is_some());
    }

    #[test]
    fn session_key_matches_between_both_sides_of_the_same_confirmed_code() {
        let mut host = HandshakeMachine::new(Role::Host);
        let mut helper = HandshakeMachine::new(Role::Helper);
        host.apply_local(LocalEvent::GenerateCode(code("042817"))).unwrap();
        host.apply_local(LocalEvent::Confirm).unwrap();
        helper.apply_local(LocalEvent::EnterCode(code("042817"))).unwrap();
        helper.apply_local(LocalEvent::Confirm).unwrap();
        host.apply_peer(PeerMessage::Confirm { code: code("042817") }).unwrap();
        helper.apply_peer(PeerMessage::Confirm { code: code("042817") }).unwrap();

        let host_key = host.session_key(b"remote-assist/sdp-auth/v1").unwrap();
        let helper_key = helper.session_key(b"remote-assist/sdp-auth/v1").unwrap();
        assert_eq!(host_key.as_bytes(), helper_key.as_bytes());
    }

    #[test]
    fn session_key_is_none_again_once_ended() {
        let mut m = HandshakeMachine::new(Role::Host);
        m.apply_local(LocalEvent::GenerateCode(code("042817"))).unwrap();
        m.apply_local(LocalEvent::Confirm).unwrap();
        m.apply_peer(PeerMessage::Confirm { code: code("042817") }).unwrap();
        assert!(m.session_key(b"purpose-a").is_some());

        m.apply_local(LocalEvent::HotkeyEnd).unwrap();
        assert!(m.session_key(b"purpose-a").is_none());
    }
}
