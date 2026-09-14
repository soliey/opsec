//! How [`PeerMessage`]s reach the other machine.
//!
//! Phase 1 ships only [`LoopbackLink`]: an in-process, in-memory pair used
//! by tests and by same-device demos of the UI. A later phase adds a
//! WebRTC-data-channel-backed implementation of [`PeerLink`] for real
//! cross-machine signaling; [`HandshakeMachine`](crate::HandshakeMachine)
//! does not need to change when that happens — it only ever sees
//! [`PeerMessage`] values, never transport details.

use crate::handshake::PeerMessage;
use std::sync::mpsc::{self, Receiver, Sender};

/// A duplex channel carrying handshake messages to and from the peer.
pub trait PeerLink {
    fn send(&self, message: PeerMessage);
    /// Non-blocking: returns `None` if nothing has arrived yet.
    fn try_recv(&self) -> Option<PeerMessage>;
}

/// An in-process, in-memory pair of linked endpoints. Whatever is sent on
/// one side's `send` shows up in the other side's `try_recv`.
pub struct LoopbackLink {
    tx: Sender<PeerMessage>,
    rx: Receiver<PeerMessage>,
}

impl LoopbackLink {
    /// Builds two ends of the same link, already connected to each other.
    pub fn pair() -> (Self, Self) {
        let (tx_a, rx_b) = mpsc::channel();
        let (tx_b, rx_a) = mpsc::channel();
        (Self { tx: tx_a, rx: rx_a }, Self { tx: tx_b, rx: rx_b })
    }
}

impl PeerLink for LoopbackLink {
    fn send(&self, message: PeerMessage) {
        // The only way this fails is if the peer end was dropped, which is
        // itself a disconnect; there is nothing actionable to do with the
        // error here, and callers detect the disconnect via `try_recv`
        // returning `None` forever, same as a stalled network link would.
        let _ = self.tx.send(message);
    }

    fn try_recv(&self) -> Option<PeerMessage> {
        self.rx.try_recv().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_sent_on_one_end_arrive_on_the_other() {
        let (a, b) = LoopbackLink::pair();
        assert_eq!(a.try_recv(), None);

        b.send(PeerMessage::Cancel);
        assert_eq!(a.try_recv(), Some(PeerMessage::Cancel));
        assert_eq!(a.try_recv(), None);

        a.send(PeerMessage::HotkeyEnd);
        assert_eq!(b.try_recv(), Some(PeerMessage::HotkeyEnd));
    }
}
