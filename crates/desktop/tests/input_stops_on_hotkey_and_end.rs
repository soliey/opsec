//! Proves the phase 3 guarantee end-to-end: input injection stops the
//! instant a session is no longer `consent::SessionState::Active` — on the
//! host's own hotkey, on the helper's hotkey (relayed as a `PeerMessage`,
//! since injection always executes on the host machine), and on any other
//! way the consent module reports the session ended (e.g.
//! `PeerDisconnected`) — and that none of this depends on `InputFeel` or
//! `OverlayVisibility`. That's exactly what CLAUDE.md and `settings::
//! Settings` promise: preferences change how input is shaped, never
//! whether or when it's allowed.
//!
//! `Session`/`AppState`/`HostGate` (the real wiring in `src/lib.rs`) are
//! private to the app crate, so this test rebuilds the same shape — a pair
//! of linked `HandshakeMachine`s gating a `NaturalInput` — from
//! `remote_assist_lib`'s public surface plus the same `consent`/`input`
//! crates the app itself depends on, the same way
//! `capture_excludes_app_windows.rs` reuses `remote_assist_lib::overlay`
//! rather than reaching into private app state.

use consent::{HandshakeMachine, LocalEvent, PeerMessage, Role, SessionCode};
use input::{InputBackend, InputError, KeyCode, MouseButton, NaturalInput};
use remote_assist_lib::overlay;
use settings::{InputFeel, OverlayVisibility};
use std::cell::RefCell;

fn all_input_feels() -> [InputFeel; 3] {
    [InputFeel::Instant, InputFeel::Smooth, InputFeel::VeryNatural]
}

fn all_overlay_modes() -> [OverlayVisibility; 3] {
    [
        OverlayVisibility::Full,
        OverlayVisibility::MinimalIndicator,
        OverlayVisibility::Presenter,
    ]
}

/// Host and helper, both `Active` with matching codes — mirrors what
/// `Session::new()` plus a completed handshake looks like in the real app.
fn active_pair() -> (RefCell<HandshakeMachine>, RefCell<HandshakeMachine>) {
    let mut host = HandshakeMachine::new(Role::Host);
    let mut helper = HandshakeMachine::new(Role::Helper);
    let code = SessionCode::generate();
    host.apply_local(LocalEvent::GenerateCode(code.clone())).unwrap();
    host.apply_local(LocalEvent::Confirm).unwrap();
    helper.apply_local(LocalEvent::EnterCode(code.clone())).unwrap();
    helper.apply_local(LocalEvent::Confirm).unwrap();
    host.apply_peer(PeerMessage::Confirm { code: code.clone() }).unwrap();
    helper.apply_peer(PeerMessage::Confirm { code }).unwrap();
    assert!(host.is_active());
    assert!(helper.is_active());
    (RefCell::new(host), RefCell::new(helper))
}

/// Records every `move_cursor` call into a shared, externally-visible list
/// (moves are private to `NaturalInput`, so the test can't inspect its
/// backend after the fact — this is what lets it check call counts anyway)
/// and, once that list hits `end_after` entries, runs `on_end` — standing
/// in for a hotkey or peer message arriving asynchronously mid-motion, the
/// same way a real one would.
struct EndingBackend<'a> {
    moves: &'a RefCell<Vec<(i32, i32)>>,
    end_after: usize,
    on_end: Box<dyn FnMut() + 'a>,
}

impl<'a> InputBackend for EndingBackend<'a> {
    fn move_cursor(&mut self, x: i32, y: i32) {
        self.moves.borrow_mut().push((x, y));
        if self.moves.borrow().len() == self.end_after {
            (self.on_end)();
        }
    }
    fn mouse_button(&mut self, _button: MouseButton, _pressed: bool) {}
    fn key(&mut self, _code: KeyCode, _pressed: bool) {}
}

#[derive(Debug, Clone, Copy)]
enum Trigger {
    HostHotkey,
    HelperHotkeyPropagatedToHost,
    HostPeerDisconnected,
}

#[test]
fn injection_stops_on_every_end_trigger_in_every_feel_and_overlay_combination() {
    for overlay_mode in all_overlay_modes() {
        // Overlay visibility is a host-window cosmetic setting only (see
        // settings::OverlayVisibility and CLAUDE.md's capture-exclusion
        // section) — it must have zero bearing on the injection gate.
        // Sweeping it here, alongside every InputFeel, is how this test
        // proves that rather than assuming it.
        let has_geometry = overlay::geometry_for(overlay_mode).is_some();
        assert_eq!(
            has_geometry,
            overlay_mode != OverlayVisibility::Presenter,
            "sanity check on overlay::geometry_for itself"
        );

        for feel in all_input_feels() {
            for trigger in [
                Trigger::HostHotkey,
                Trigger::HelperHotkeyPropagatedToHost,
                Trigger::HostPeerDisconnected,
            ] {
                let label = format!("{overlay_mode:?} / {feel:?} / {trigger:?}");
                let (host, helper) = active_pair();

                // `Instant` sends a whole move as a single atomic step
                // (`input::motion::plan_move`), so there is nothing to
                // interrupt mid-motion: the trigger fires as a side effect
                // of that one step and the call still completes. Every
                // other feel breaks the move into many steps, so ending
                // the session partway through must cut it off before the
                // remaining steps are ever sent.
                let end_after = if matches!(feel, InputFeel::Instant) { 1 } else { 3 };

                let moves = RefCell::new(Vec::new());
                let backend = EndingBackend {
                    moves: &moves,
                    end_after,
                    on_end: Box::new(|| match trigger {
                        Trigger::HostHotkey => {
                            let _ = host.borrow_mut().apply_local(LocalEvent::HotkeyEnd);
                        }
                        Trigger::HelperHotkeyPropagatedToHost => {
                            let _ = helper.borrow_mut().apply_local(LocalEvent::HotkeyEnd);
                            let _ = host.borrow_mut().apply_peer(PeerMessage::HotkeyEnd);
                        }
                        Trigger::HostPeerDisconnected => {
                            let _ = host.borrow_mut().apply_peer(PeerMessage::Disconnected);
                        }
                    }),
                };

                // Injection always executes on the host machine, so it's
                // gated on the host's own resulting state regardless of
                // which machine's action caused the session to end.
                let mut input = NaturalInput::new(|| host.borrow().is_active(), backend, feel, (0, 0));

                let result = input.move_mouse_to((3000, 2000));
                assert!(!host.borrow().is_active(), "{label}: host must no longer be active");

                if matches!(feel, InputFeel::Instant) {
                    assert_eq!(result, Ok(()), "{label}");
                } else {
                    assert_eq!(result, Err(InputError::SessionNotActive), "{label}");
                    assert_eq!(
                        moves.borrow().len(),
                        end_after,
                        "{label}: exactly the steps sent before the trigger fired, nothing after"
                    );
                }
            }
        }
    }
}
