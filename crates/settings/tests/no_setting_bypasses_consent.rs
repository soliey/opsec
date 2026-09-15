//! Proves that host preferences — specifically Presenter mode, which fully
//! hides the overlay — cannot affect whether a session may become Active.
//!
//! The real guarantee is structural: `consent::HandshakeMachine`'s API has
//! no `Settings` parameter anywhere, so there is no code path through which
//! a preference could reach it. This test is the executable version of that
//! claim: it runs the ordinary handshake with every `OverlayVisibility`
//! configured and confirms the outcome never changes.

use consent::{HandshakeMachine, LocalEvent, PeerMessage, Role, SessionCode};
use settings::{OverlayVisibility, Settings};

fn all_visibilities() -> [OverlayVisibility; 3] {
    [
        OverlayVisibility::Full,
        OverlayVisibility::MinimalIndicator,
        OverlayVisibility::Presenter,
    ]
}

#[test]
fn no_visibility_setting_activates_without_both_confirmations() {
    for visibility in all_visibilities() {
        let settings = Settings {
            overlay_visibility: visibility,
            sounds_enabled: true,
        };

        let mut m = HandshakeMachine::new(Role::Host);
        let code = SessionCode::generate();
        m.apply_local(LocalEvent::GenerateCode(code)).unwrap();
        m.apply_local(LocalEvent::Confirm).unwrap();

        assert!(
            !m.is_active(),
            "{settings:?} must not activate a session on local confirm alone"
        );
    }
}

#[test]
fn every_visibility_setting_still_requires_the_peers_matching_confirm() {
    for visibility in all_visibilities() {
        let settings = Settings {
            overlay_visibility: visibility,
            sounds_enabled: false,
        };

        let mut m = HandshakeMachine::new(Role::Host);
        let code = SessionCode::generate();
        m.apply_local(LocalEvent::GenerateCode(code.clone())).unwrap();
        m.apply_local(LocalEvent::Confirm).unwrap();
        assert!(!m.is_active(), "{settings:?}: still waiting on the peer");

        m.apply_peer(PeerMessage::Confirm { code }).unwrap();
        assert!(
            m.is_active(),
            "{settings:?}: both sides confirmed with matching codes, this must activate"
        );
    }
}
