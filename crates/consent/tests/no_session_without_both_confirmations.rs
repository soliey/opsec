//! End-to-end proof, over the loopback transport, that a session between
//! two independent `HandshakeMachine`s can only ever become `Active` on
//! both sides at once, and only after both humans pressed Confirm with
//! matching codes. Unit tests in `handshake.rs` cover a single machine's
//! internal logic; this file drives two machines wired together the way
//! the real app will, standing in for host and helper on separate
//! computers.

use consent::transport::{LoopbackLink, PeerLink};
use consent::{HandshakeMachine, LocalEvent, PeerMessage, Role, SessionCode, SessionState};

/// Pumps any messages currently queued on `link` into `machine`.
fn pump(machine: &mut HandshakeMachine, link: &LoopbackLink) {
    while let Some(msg) = link.try_recv() {
        machine.apply_peer(msg).unwrap();
    }
}

#[test]
fn full_pairing_both_sides_reach_active_together() {
    let (host_link, helper_link) = LoopbackLink::pair();
    let mut host = HandshakeMachine::new(Role::Host);
    let mut helper = HandshakeMachine::new(Role::Helper);

    let code = SessionCode::generate();
    host.apply_local(LocalEvent::GenerateCode(code.clone())).unwrap();
    // Out-of-band: host reads the code aloud, helper types it in.
    helper.apply_local(LocalEvent::EnterCode(code.clone())).unwrap();

    assert!(!host.is_active());
    assert!(!helper.is_active());

    // Host confirms first and tells the peer.
    host.apply_local(LocalEvent::Confirm).unwrap();
    host_link.send(PeerMessage::Confirm { code: code.clone() });
    pump(&mut helper, &helper_link);

    // Helper hasn't confirmed locally yet, so still not active even though
    // the peer's confirmation already arrived.
    assert!(!helper.is_active());
    assert!(!host.is_active());

    // Helper confirms too.
    helper.apply_local(LocalEvent::Confirm).unwrap();
    helper_link.send(PeerMessage::Confirm { code: code.clone() });
    pump(&mut host, &host_link);

    assert!(host.is_active());
    assert!(helper.is_active());
    assert_eq!(*host.state(), SessionState::Active { code: code.clone() });
    assert_eq!(*helper.state(), SessionState::Active { code });
}

#[test]
fn mismatched_codes_never_produce_active_on_either_side() {
    let (host_link, helper_link) = LoopbackLink::pair();
    let mut host = HandshakeMachine::new(Role::Host);
    let mut helper = HandshakeMachine::new(Role::Helper);

    host.apply_local(LocalEvent::GenerateCode(SessionCode::parse("042817").unwrap()))
        .unwrap();
    // Helper mistypes / uses a stale code from a previous session.
    helper
        .apply_local(LocalEvent::EnterCode(SessionCode::parse("999999").unwrap()))
        .unwrap();

    host.apply_local(LocalEvent::Confirm).unwrap();
    host_link.send(PeerMessage::Confirm {
        code: SessionCode::parse("042817").unwrap(),
    });
    helper.apply_local(LocalEvent::Confirm).unwrap();
    helper_link.send(PeerMessage::Confirm {
        code: SessionCode::parse("999999").unwrap(),
    });

    pump(&mut host, &host_link);
    pump(&mut helper, &helper_link);

    assert!(!host.is_active());
    assert!(!helper.is_active());
    assert!(host.is_ended());
    assert!(helper.is_ended());
}

#[test]
fn helper_confirming_alone_never_activates_host() {
    let (host_link, helper_link) = LoopbackLink::pair();
    let mut host = HandshakeMachine::new(Role::Host);
    let mut helper = HandshakeMachine::new(Role::Helper);

    let code = SessionCode::generate();
    host.apply_local(LocalEvent::GenerateCode(code.clone())).unwrap();
    helper.apply_local(LocalEvent::EnterCode(code.clone())).unwrap();

    // Only the helper confirms; host never presses Confirm.
    helper.apply_local(LocalEvent::Confirm).unwrap();
    helper_link.send(PeerMessage::Confirm { code });
    pump(&mut host, &host_link);

    assert!(!host.is_active());
    assert!(!helper.is_active());
    assert!(matches!(host.state(), SessionState::AwaitingLocalConfirmation { .. }));
}

#[test]
fn hotkey_on_either_machine_ends_an_active_session_on_that_machine_and_notifies_the_peer() {
    let (host_link, helper_link) = LoopbackLink::pair();
    let mut host = HandshakeMachine::new(Role::Host);
    let mut helper = HandshakeMachine::new(Role::Helper);

    let code = SessionCode::generate();
    host.apply_local(LocalEvent::GenerateCode(code.clone())).unwrap();
    helper.apply_local(LocalEvent::EnterCode(code.clone())).unwrap();
    host.apply_local(LocalEvent::Confirm).unwrap();
    host_link.send(PeerMessage::Confirm { code: code.clone() });
    pump(&mut helper, &helper_link);
    helper.apply_local(LocalEvent::Confirm).unwrap();
    helper_link.send(PeerMessage::Confirm { code });
    pump(&mut host, &host_link);
    assert!(host.is_active() && helper.is_active());

    // Helper hits the panic hotkey.
    helper.apply_local(LocalEvent::HotkeyEnd).unwrap();
    helper_link.send(PeerMessage::HotkeyEnd);
    assert!(!helper.is_active());

    // Host doesn't find out until the message is pumped — modeling real
    // network latency — but once it is, the host session ends too.
    assert!(host.is_active());
    pump(&mut host, &host_link);
    assert!(!host.is_active());
    assert!(host.is_ended());
}

/// Brute-force sanity check: no matter what order the four possible events
/// (local confirm, matching peer confirm, local cancel, peer cancel) are
/// fed to a fresh machine in, it only ever ends up `Active` on the runs
/// that contain *both* confirms and neither cancel before them.
#[test]
fn exhaustive_event_orderings_only_activate_with_both_confirms_and_no_cancel() {
    #[derive(Clone, Copy, Debug)]
    enum Ev {
        LocalConfirm,
        PeerConfirm,
        LocalCancel,
        PeerCancel,
    }

    fn permutations(events: &[Ev]) -> Vec<Vec<Ev>> {
        if events.len() <= 1 {
            return vec![events.to_vec()];
        }
        let mut out = Vec::new();
        for i in 0..events.len() {
            let mut rest = events.to_vec();
            let picked = rest.remove(i);
            for mut tail in permutations(&rest) {
                let mut seq = vec![picked];
                seq.append(&mut tail);
                out.push(seq);
            }
        }
        out
    }

    let all = [Ev::LocalConfirm, Ev::PeerConfirm, Ev::LocalCancel, Ev::PeerCancel];

    // Every ordering of just {LocalConfirm, PeerConfirm}: must always activate.
    for seq in permutations(&all[0..2]) {
        let mut m = HandshakeMachine::new(Role::Host);
        let code = SessionCode::parse("042817").unwrap();
        m.apply_local(LocalEvent::GenerateCode(code.clone())).unwrap();
        for ev in &seq {
            match ev {
                Ev::LocalConfirm => {
                    m.apply_local(LocalEvent::Confirm).unwrap();
                }
                Ev::PeerConfirm => {
                    m.apply_peer(PeerMessage::Confirm { code: code.clone() }).unwrap();
                }
                _ => unreachable!(),
            }
        }
        assert!(m.is_active(), "sequence {seq:?} should have activated");
    }

    // Any ordering that includes a cancel before both confirms land must
    // never activate, regardless of where the cancel falls.
    for seq in permutations(&all) {
        let mut m = HandshakeMachine::new(Role::Host);
        let code = SessionCode::parse("042817").unwrap();
        m.apply_local(LocalEvent::GenerateCode(code.clone())).unwrap();
        let mut cancel_happened = false;
        for ev in &seq {
            if m.is_ended() {
                break;
            }
            let _ = match ev {
                Ev::LocalConfirm => m.apply_local(LocalEvent::Confirm),
                Ev::PeerConfirm => m.apply_peer(PeerMessage::Confirm { code: code.clone() }),
                Ev::LocalCancel => {
                    cancel_happened = true;
                    m.apply_local(LocalEvent::Cancel)
                }
                Ev::PeerCancel => {
                    cancel_happened = true;
                    m.apply_peer(PeerMessage::Cancel)
                }
            };
        }
        if cancel_happened {
            assert!(!m.is_active(), "sequence {seq:?} canceled but ended up active");
        }
    }
}
