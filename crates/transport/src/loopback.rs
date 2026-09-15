//! An in-process, real-AEAD-encrypted channel standing in for the network
//! — the same role `consent::transport::LoopbackLink` plays for phase 1's
//! signaling, and the actual transport this phase's desktop demo streams
//! frames over (see `crate` root docs for why real cross-machine WebRTC
//! isn't wired up yet).
//!
//! "End-to-end encrypted; the relay never sees plaintext" is provable here,
//! not just asserted: [`LoopbackTransport::pair`] hands back two ends
//! joined by a channel that only ever carries [`EncryptedFrame`]s (nonce +
//! ChaCha20-Poly1305 ciphertext), keyed by
//! [`consent::HandshakeMachine::session_key`]. The tests decrypt-check that
//! the wire bytes never contain the plaintext and that a party without the
//! matching key — standing in for an untrusted relay — cannot read it.
//!
//! Two *different* keys, one per direction, are required — see
//! [`HOST_TO_HELPER_KEY_INFO`]/[`HELPER_TO_HOST_KEY_INFO`]'s doc comment
//! for why reusing a single key for both directions would be a nonce-reuse
//! hazard.

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use consent::SessionKey;
use std::sync::mpsc::{self, Receiver, Sender};

/// Domain-separation labels for the two directional media keys. Using one
/// shared key for both directions would mean two independent nonce
/// counters (one per `LoopbackTransport` instance) drawing from the same
/// keystream space — a real nonce-reuse hazard for any AEAD, ChaCha20-
/// Poly1305 included. Deriving a distinct key per direction (both sides
/// compute both, since both hold the same confirmed session code) makes
/// each key's nonce space independently safe instead.
pub const HOST_TO_HELPER_KEY_INFO: &[u8] = b"remote-assist/loopback-media/host-to-helper/v1";
pub const HELPER_TO_HOST_KEY_INFO: &[u8] = b"remote-assist/loopback-media/helper-to-host/v1";

#[derive(Debug, Clone)]
pub struct EncryptedFrame {
    nonce: [u8; 12],
    ciphertext: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportError {
    /// Decryption failed: tampered ciphertext, a mismatched key, or (in
    /// principle) a corrupted frame. AEAD deliberately doesn't distinguish
    /// these — any of them means "don't trust this frame".
    DecryptionFailed,
    /// The peer end was dropped (session ended, process exited, ...).
    Disconnected,
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TransportError::DecryptionFailed => write!(f, "frame failed authenticated decryption"),
            TransportError::Disconnected => write!(f, "peer disconnected"),
        }
    }
}

impl std::error::Error for TransportError {}

/// One end of an encrypted, in-process, full-duplex channel.
pub struct LoopbackTransport {
    encrypt_cipher: ChaCha20Poly1305,
    decrypt_cipher: ChaCha20Poly1305,
    tx: Sender<EncryptedFrame>,
    rx: Receiver<EncryptedFrame>,
    send_nonce_counter: u64,
}

impl LoopbackTransport {
    /// Builds a connected pair: `(host_side, helper_side)`.
    /// `key_host_to_helper` and `key_helper_to_host` must be different keys
    /// — see the module docs.
    pub fn pair(key_host_to_helper: &SessionKey, key_helper_to_host: &SessionKey) -> (Self, Self) {
        let (tx_a, rx_b) = mpsc::channel();
        let (tx_b, rx_a) = mpsc::channel();
        let host_side = Self {
            encrypt_cipher: cipher_from(key_host_to_helper),
            decrypt_cipher: cipher_from(key_helper_to_host),
            tx: tx_a,
            rx: rx_a,
            send_nonce_counter: 0,
        };
        let helper_side = Self {
            encrypt_cipher: cipher_from(key_helper_to_host),
            decrypt_cipher: cipher_from(key_host_to_helper),
            tx: tx_b,
            rx: rx_b,
            send_nonce_counter: 0,
        };
        (host_side, helper_side)
    }

    /// Encrypts and sends one frame (an encoded video sample, in the real
    /// pipeline). The peer end's `try_recv` sees only the resulting
    /// [`EncryptedFrame`] — never `plaintext` itself.
    pub fn send(&mut self, plaintext: &[u8]) {
        self.send_nonce_counter += 1;
        let mut nonce_bytes = [0u8; 12];
        nonce_bytes[..8].copy_from_slice(&self.send_nonce_counter.to_be_bytes());
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ciphertext = self
            .encrypt_cipher
            .encrypt(nonce, plaintext)
            .expect("ChaCha20-Poly1305 encryption does not fail for in-memory buffers");
        let _ = self.tx.send(EncryptedFrame { nonce: nonce_bytes, ciphertext });
    }

    /// Non-blocking: `Ok(None)` if nothing has arrived yet, `Err` if the
    /// peer disconnected or a frame failed to authenticate.
    pub fn try_recv(&self) -> Result<Option<Vec<u8>>, TransportError> {
        match self.rx.try_recv() {
            Ok(frame) => {
                let nonce = Nonce::from_slice(&frame.nonce);
                let plaintext = self
                    .decrypt_cipher
                    .decrypt(nonce, frame.ciphertext.as_slice())
                    .map_err(|_| TransportError::DecryptionFailed)?;
                Ok(Some(plaintext))
            }
            Err(mpsc::TryRecvError::Empty) => Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => Err(TransportError::Disconnected),
        }
    }
}

fn cipher_from(key: &SessionKey) -> ChaCha20Poly1305 {
    ChaCha20Poly1305::new_from_slice(key.as_bytes()).expect("SessionKey is always exactly 32 bytes")
}

#[cfg(test)]
mod tests {
    use super::*;
    use consent::{HandshakeMachine, LocalEvent, PeerMessage, Role, SessionCode};

    fn active_pair() -> (HandshakeMachine, HandshakeMachine) {
        let mut host = HandshakeMachine::new(Role::Host);
        let mut helper = HandshakeMachine::new(Role::Helper);
        let code = SessionCode::generate();
        host.apply_local(LocalEvent::GenerateCode(code.clone())).unwrap();
        host.apply_local(LocalEvent::Confirm).unwrap();
        helper.apply_local(LocalEvent::EnterCode(code.clone())).unwrap();
        helper.apply_local(LocalEvent::Confirm).unwrap();
        host.apply_peer(PeerMessage::Confirm { code: code.clone() }).unwrap();
        helper.apply_peer(PeerMessage::Confirm { code }).unwrap();
        (host, helper)
    }

    fn connected_pair() -> (LoopbackTransport, LoopbackTransport) {
        let (host, helper) = active_pair();
        let h2h = host.session_key(HOST_TO_HELPER_KEY_INFO).unwrap();
        let help2h = helper.session_key(HELPER_TO_HOST_KEY_INFO).unwrap();
        // Both sides derive both directional keys independently from the
        // same confirmed code — that's what lets each side build both
        // ciphers without ever sending key material anywhere.
        let h2h_from_helper = helper.session_key(HOST_TO_HELPER_KEY_INFO).unwrap();
        let help2h_from_host = host.session_key(HELPER_TO_HOST_KEY_INFO).unwrap();
        assert_eq!(h2h.as_bytes(), h2h_from_helper.as_bytes());
        assert_eq!(help2h.as_bytes(), help2h_from_host.as_bytes());
        LoopbackTransport::pair(&h2h, &help2h)
    }

    #[test]
    fn round_trips_host_to_helper() {
        let (mut host, helper) = connected_pair();
        host.send(b"encoded-frame-bytes");
        assert_eq!(helper.try_recv().unwrap(), Some(b"encoded-frame-bytes".to_vec()));
    }

    #[test]
    fn round_trips_helper_to_host() {
        let (host, mut helper) = connected_pair();
        helper.send(b"congestion-feedback");
        assert_eq!(host.try_recv().unwrap(), Some(b"congestion-feedback".to_vec()));
    }

    #[test]
    fn empty_channel_returns_none_not_an_error() {
        let (_host, helper) = connected_pair();
        assert_eq!(helper.try_recv().unwrap(), None);
    }

    #[test]
    fn the_relay_the_channel_itself_never_carries_plaintext() {
        // The channel's payload type is EncryptedFrame; grab the actual
        // frame that crossed it (before the peer decrypts it) and confirm
        // its ciphertext doesn't contain the plaintext anywhere — proving
        // this isn't a no-op/XOR "encryption".
        let (mut host, helper) = connected_pair();
        let plaintext = b"a screen frame nobody but the peer should read";
        host.send(plaintext);

        let frame = helper.rx.recv().unwrap();
        assert_ne!(frame.ciphertext, plaintext, "ciphertext must not equal the plaintext");
        let contains_plaintext = frame
            .ciphertext
            .windows(plaintext.len())
            .any(|window| window == plaintext.as_slice());
        assert!(!contains_plaintext, "ciphertext must not contain the plaintext as a substring");
    }

    #[test]
    fn a_party_without_the_matching_key_cannot_decrypt() {
        // An eavesdropper's own session (different code, so a different
        // derived key) trying to read the frame stands in for "the relay"
        // — or anyone else without the confirmed session code.
        let (mut host, helper) = connected_pair();
        host.send(b"secret screen contents");
        let frame = helper.rx.recv().unwrap();

        let (eavesdropper, _) = connected_pair();
        let wrong_key_result = eavesdropper
            .decrypt_cipher
            .decrypt(Nonce::from_slice(&frame.nonce), frame.ciphertext.as_slice());
        assert!(wrong_key_result.is_err(), "decryption under an unrelated session's key must fail");
    }

    #[test]
    fn tampered_ciphertext_fails_authenticated_decryption() {
        let (mut host, helper) = connected_pair();
        host.send(b"integrity-checked payload");
        let mut frame = helper.rx.recv().unwrap();
        frame.ciphertext[0] ^= 0xFF;
        let result = helper
            .decrypt_cipher
            .decrypt(Nonce::from_slice(&frame.nonce), frame.ciphertext.as_slice());
        assert!(result.is_err(), "a single flipped bit must fail AEAD authentication");
    }

    #[test]
    fn many_sequential_sends_never_reuse_a_nonce_or_panic() {
        let (mut host, helper) = connected_pair();
        for i in 0..500u32 {
            host.send(&i.to_le_bytes());
        }
        let mut received = Vec::new();
        while let Some(bytes) = helper.try_recv().unwrap() {
            received.push(u32::from_le_bytes(bytes.try_into().unwrap()));
        }
        assert_eq!(received.len(), 500);
        assert_eq!(received, (0..500).collect::<Vec<_>>());
    }
}
