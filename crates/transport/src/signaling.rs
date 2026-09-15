//! Authenticating session-setup messages (SDP offer/answer, ICE candidates)
//! with the phase-1 session code — this is the "extend the consent module,
//! don't duplicate the logic" seam named in the phase 4 spec.
//!
//! WebRTC's own DTLS handshake authenticates the *media* channel via
//! certificate fingerprints — but those fingerprints are only as trustworthy
//! as the signaling channel that carried them. If signaling itself runs
//! through a relay or rendezvous service that isn't fully trusted, an
//! attacker astride it could swap in their own fingerprint and sit in the
//! middle of a connection that otherwise "looks" end-to-end encrypted. Both
//! machines already independently confirmed they hold the *same* session
//! code (that's the whole guarantee `consent::HandshakeMachine` provides),
//! so this module uses a key derived from that code
//! ([`consent::HandshakeMachine::session_key`]) to HMAC-authenticate every
//! signaling message — closing that gap without inventing any new trust
//! source.

use consent::SessionKey;
use hmac::{Hmac, Mac};
use sha2::Sha256;

/// Domain-separation label for [`SessionKey`]s used to authenticate
/// signaling — see `SessionCode::derive_key`'s doc comment. Never reused
/// for another purpose (e.g. [`crate::loopback`] uses its own label).
pub const SIGNALING_KEY_INFO: &[u8] = b"remote-assist/signaling-auth/v1";

const MAC_LEN: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignalingEnvelope {
    pub payload: Vec<u8>,
    mac: [u8; MAC_LEN],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthError {
    /// The envelope's MAC didn't verify against this key — either it was
    /// tampered with in transit, or it was never sealed with the same
    /// session key (e.g. a relay trying to inject its own SDP).
    InvalidMac,
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "signaling envelope failed authentication")
    }
}

impl std::error::Error for AuthError {}

/// Authenticates `payload` (e.g. a serialized SDP offer/answer or ICE
/// candidate) with `key`, producing an envelope safe to hand to an
/// untrusted signaling relay: the relay can see and forward the payload
/// (this is *authentication*, not secrecy — SDP has to be visible in
/// transit to reach a signaling server at all) but cannot forge one that
/// [`open`] will accept without the same session key.
pub fn seal(key: &SessionKey, payload: Vec<u8>) -> SignalingEnvelope {
    let mut mac = Hmac::<Sha256>::new_from_slice(key.as_bytes()).expect("HMAC accepts any key length");
    mac.update(&payload);
    let tag = mac.finalize().into_bytes();
    let mut mac_bytes = [0u8; MAC_LEN];
    mac_bytes.copy_from_slice(&tag);
    SignalingEnvelope { payload, mac: mac_bytes }
}

/// Verifies `envelope` against `key` (constant-time), returning the payload
/// only if the MAC checks out.
pub fn open(key: &SessionKey, envelope: &SignalingEnvelope) -> Result<Vec<u8>, AuthError> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key.as_bytes()).expect("HMAC accepts any key length");
    mac.update(&envelope.payload);
    mac.verify_slice(&envelope.mac).map_err(|_| AuthError::InvalidMac)?;
    Ok(envelope.payload.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use consent::{HandshakeMachine, LocalEvent, PeerMessage, Role, SessionCode};

    fn matching_keys() -> (SessionKey, SessionKey) {
        let mut host = HandshakeMachine::new(Role::Host);
        let mut helper = HandshakeMachine::new(Role::Helper);
        let code = SessionCode::generate();
        host.apply_local(LocalEvent::GenerateCode(code.clone())).unwrap();
        host.apply_local(LocalEvent::Confirm).unwrap();
        helper.apply_local(LocalEvent::EnterCode(code.clone())).unwrap();
        helper.apply_local(LocalEvent::Confirm).unwrap();
        host.apply_peer(PeerMessage::Confirm { code: code.clone() }).unwrap();
        helper.apply_peer(PeerMessage::Confirm { code }).unwrap();
        (
            host.session_key(SIGNALING_KEY_INFO).unwrap(),
            helper.session_key(SIGNALING_KEY_INFO).unwrap(),
        )
    }

    #[test]
    fn sealed_by_one_side_opens_with_the_others_matching_key() {
        let (host_key, helper_key) = matching_keys();
        let envelope = seal(&host_key, b"fake-sdp-offer".to_vec());
        assert_eq!(open(&helper_key, &envelope).unwrap(), b"fake-sdp-offer");
    }

    #[test]
    fn tampered_payload_fails_authentication() {
        let (host_key, helper_key) = matching_keys();
        let mut envelope = seal(&host_key, b"fake-sdp-offer".to_vec());
        envelope.payload[0] ^= 0xFF;
        assert_eq!(open(&helper_key, &envelope), Err(AuthError::InvalidMac));
    }

    #[test]
    fn wrong_key_fails_authentication() {
        let (host_key, _helper_key) = matching_keys();
        let other_session_key = {
            let mut m = HandshakeMachine::new(Role::Host);
            let code = SessionCode::generate();
            m.apply_local(LocalEvent::GenerateCode(code.clone())).unwrap();
            m.apply_local(LocalEvent::Confirm).unwrap();
            m.apply_peer(PeerMessage::Confirm { code }).unwrap();
            m.session_key(SIGNALING_KEY_INFO).unwrap()
        };
        let envelope = seal(&host_key, b"fake-sdp-offer".to_vec());
        assert_eq!(open(&other_session_key, &envelope), Err(AuthError::InvalidMac));
    }

    #[test]
    fn a_relay_that_only_sees_the_envelope_cannot_forge_a_valid_one() {
        // Simulates an untrusted relay: it has the envelope bytes (payload
        // + mac) but not the session key, and tries to substitute its own
        // payload while keeping the original mac.
        let (host_key, helper_key) = matching_keys();
        let legit = seal(&host_key, b"legit-offer".to_vec());
        let forged = SignalingEnvelope {
            payload: b"attacker-controlled-offer".to_vec(),
            mac: legit.mac,
        };
        assert_eq!(open(&helper_key, &forged), Err(AuthError::InvalidMac));
    }
}
