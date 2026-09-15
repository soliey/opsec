//! The consent gate for input injection.
//!
//! This is the one place in this crate that decides whether a given mouse
//! or keyboard event is actually allowed to reach [`crate::backend::
//! InputBackend`]. Read this file to audit the injection-side guarantee;
//! nothing outside it can shortcut the result — `motion` and `timing` only
//! ever produce *plans*, never touch a backend themselves.
//!
//! # The invariant
//!
//! [`NaturalInput`] never caches "the session was active when this call
//! started". Every single atomic step — one mouse move, one button
//! press/release, one key press/release — calls [`SessionGate::is_active`]
//! immediately beforehand. A long mouse move is many small steps for
//! exactly this reason: the gate is checked between every one of them, so
//! there is no unit of work bigger than "one step" between a hotkey firing
//! and injection actually stopping. That is what "stops within one event
//! loop tick" means here — see `crates/desktop`'s wiring of the global
//! hotkey to `consent::HandshakeMachine`, and this module's own tests,
//! which drive a real `HandshakeMachine` mid-motion.
//!
//! If the gate closes while a key or mouse button this instance pressed is
//! still down, it is released immediately as a safety fallback (see
//! [`NaturalInput::release_all_held`]) — a session ending must never leave
//! a button/key stuck down on the host.

use crate::backend::{InputBackend, KeyCode, MouseButton};
use crate::motion;
use crate::timing;
use settings::InputFeel;
use std::collections::HashSet;
use std::thread;

/// Live session-state oracle. `NaturalInput` is generic over this instead
/// of depending on `consent` directly, so this crate stays decoupled from
/// exactly how a caller stores its `HandshakeMachine` (a `Mutex`, a pair of
/// linked machines for a same-process demo, ...) — see the crate docs.
///
/// Implemented for any `Fn() -> bool`, so a caller can typically just pass
/// a closure.
pub trait SessionGate {
    /// Must reflect the session's *current* state — see the module docs.
    /// True only while the session is `consent::SessionState::Active`.
    fn is_active(&self) -> bool;
}

impl<F: Fn() -> bool> SessionGate for F {
    fn is_active(&self) -> bool {
        self()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputError {
    /// The gate reported the session was not (or no longer) active before
    /// this step could be sent. Any steps already sent earlier in the same
    /// call did happen — this only means the call was cut short; any
    /// key/button that call had pressed down was released immediately.
    SessionNotActive,
}

/// Turns requested mouse/keyboard actions into real, human-paced OS input,
/// hard-gated on a [`SessionGate`] at every atomic step.
pub struct NaturalInput<G, B> {
    gate: G,
    backend: B,
    feel: InputFeel,
    position: (i32, i32),
    held_keys: HashSet<KeyCode>,
    held_buttons: HashSet<MouseButton>,
}

impl<G: SessionGate, B: InputBackend> NaturalInput<G, B> {
    pub fn new(gate: G, backend: B, feel: InputFeel, start_position: (i32, i32)) -> Self {
        Self {
            gate,
            backend,
            feel,
            position: start_position,
            held_keys: HashSet::new(),
            held_buttons: HashSet::new(),
        }
    }

    pub fn set_feel(&mut self, feel: InputFeel) {
        self.feel = feel;
    }

    pub fn position(&self) -> (i32, i32) {
        self.position
    }

    /// True only while the underlying gate currently reports the session
    /// as active — never cached, always a fresh check.
    pub fn is_session_active(&self) -> bool {
        self.gate.is_active()
    }

    /// The one gate check every public method funnels through immediately
    /// before touching the backend. On failure, force-releases anything
    /// this instance is currently holding down before returning the error.
    fn require_active(&mut self) -> Result<(), InputError> {
        if self.gate.is_active() {
            Ok(())
        } else {
            self.release_all_held();
            Err(InputError::SessionNotActive)
        }
    }

    /// Releases every key/button this instance believes it is currently
    /// holding, regardless of gate state. Safe to call any time, including
    /// when nothing is held (a no-op then). Exposed publicly so the
    /// hotkey/session-end handler can call it directly as a belt-and-braces
    /// safety net, on top of the automatic release inside [`Self::
    /// require_active`].
    pub fn release_all_held(&mut self) {
        for key in std::mem::take(&mut self.held_keys) {
            self.backend.key(key, false);
        }
        for button in std::mem::take(&mut self.held_buttons) {
            self.backend.mouse_button(button, false);
        }
    }

    /// Moves the cursor from its last known position to `target` along a
    /// human-paced path (see [`motion::plan_move`]), re-checking the gate
    /// before every intermediate step. Stops immediately — sending nothing
    /// further — the instant the gate closes.
    pub fn move_mouse_to(&mut self, target: (i32, i32)) -> Result<(), InputError> {
        let steps = motion::plan_move(self.position, target, self.feel);
        for step in steps {
            self.require_active()?;
            self.backend.move_cursor(step.x, step.y);
            self.position = (step.x, step.y);
            if !step.pause_after.is_zero() {
                thread::sleep(step.pause_after);
            }
        }
        Ok(())
    }

    /// Moves to `at` (if not already there), then presses and releases
    /// `button` with human-paced timing. Gated the same as every other
    /// step: if the session ends mid-click, the button is released
    /// immediately and the call returns `Err` without completing.
    pub fn click(&mut self, button: MouseButton, at: (i32, i32)) -> Result<(), InputError> {
        self.move_mouse_to(at)?;

        let pre = timing::inter_action_pause(self.feel);
        if !pre.is_zero() {
            thread::sleep(pre);
        }
        self.require_active()?;
        self.backend.mouse_button(button, true);
        self.held_buttons.insert(button);

        let hold = timing::click_hold_duration(self.feel);
        if !hold.is_zero() {
            thread::sleep(hold);
        }
        self.require_active()?;
        self.backend.mouse_button(button, false);
        self.held_buttons.remove(&button);
        Ok(())
    }

    pub fn key_down(&mut self, code: KeyCode) -> Result<(), InputError> {
        self.require_active()?;
        self.backend.key(code, true);
        self.held_keys.insert(code);
        Ok(())
    }

    pub fn key_up(&mut self, code: KeyCode) -> Result<(), InputError> {
        self.require_active()?;
        self.backend.key(code, false);
        self.held_keys.remove(&code);
        Ok(())
    }

    /// A single human-paced key tap: down, hold, up.
    pub fn key_press(&mut self, code: KeyCode) -> Result<(), InputError> {
        self.key_down(code)?;
        let hold = timing::click_hold_duration(self.feel);
        if !hold.is_zero() {
            thread::sleep(hold);
        }
        self.key_up(code)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{InputBackend, KeyCode, MouseButton};
    use consent::{HandshakeMachine, LocalEvent, PeerMessage, Role, SessionCode};
    use std::cell::RefCell;

    /// Records every backend call and, once `moves.len()` hits `end_after`,
    /// ends the session it's given — simulating an async hotkey/peer
    /// message arriving mid-motion, the same way a real global hotkey or
    /// network message would interrupt a real in-progress move.
    struct EndingBackend<'a> {
        moves: Vec<(i32, i32)>,
        buttons: Vec<(MouseButton, bool)>,
        keys: Vec<(KeyCode, bool)>,
        end_after_moves: Option<usize>,
        on_move: Box<dyn FnMut() + 'a>,
    }

    impl<'a> EndingBackend<'a> {
        fn new(end_after_moves: Option<usize>, on_move: impl FnMut() + 'a) -> Self {
            Self {
                moves: Vec::new(),
                buttons: Vec::new(),
                keys: Vec::new(),
                end_after_moves,
                on_move: Box::new(on_move),
            }
        }
    }

    impl<'a> InputBackend for EndingBackend<'a> {
        fn move_cursor(&mut self, x: i32, y: i32) {
            self.moves.push((x, y));
            if self.end_after_moves == Some(self.moves.len()) {
                (self.on_move)();
            }
        }
        fn mouse_button(&mut self, button: MouseButton, pressed: bool) {
            self.buttons.push((button, pressed));
        }
        fn key(&mut self, code: KeyCode, pressed: bool) {
            self.keys.push((code, pressed));
        }
    }

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

    #[test]
    fn refuses_to_send_anything_when_never_active() {
        let host = RefCell::new(HandshakeMachine::new(Role::Host));
        let backend = EndingBackend::new(None, || {});
        let mut input = NaturalInput::new(|| host.borrow().is_active(), backend, InputFeel::Smooth, (0, 0));

        let result = input.move_mouse_to((500, 500));
        assert_eq!(result, Err(InputError::SessionNotActive));
        assert!(input.backend.moves.is_empty(), "must not send a single event before the gate ever opens");
    }

    #[test]
    fn host_hotkey_mid_motion_stops_within_one_step() {
        for feel in [InputFeel::Smooth, InputFeel::VeryNatural] {
            let (host, _helper) = active_pair();
            let backend = EndingBackend::new(Some(3), || {
                let _ = host.borrow_mut().apply_local(LocalEvent::HotkeyEnd);
            });
            let mut input = NaturalInput::new(|| host.borrow().is_active(), backend, feel, (0, 0));

            let result = input.move_mouse_to((3000, 2000));
            assert_eq!(result, Err(InputError::SessionNotActive), "{feel:?}");
            assert_eq!(
                input.backend.moves.len(),
                3,
                "{feel:?}: exactly the 3 steps sent before the hotkey fired, nothing after"
            );
            assert!(!host.borrow().is_active(), "{feel:?}");
        }
    }

    #[test]
    fn helper_hotkey_propagated_to_host_stops_injection_too() {
        // The helper's hotkey ends the helper's own machine and relays
        // HotkeyEnd to the host, exactly as `Session::hotkey_end_both`
        // does in the desktop crate — injection runs on the host machine,
        // so it's the host's resulting state that must gate it.
        for feel in [InputFeel::Smooth, InputFeel::VeryNatural] {
            let (host, helper) = active_pair();
            let backend = EndingBackend::new(Some(2), || {
                let _ = helper.borrow_mut().apply_local(LocalEvent::HotkeyEnd);
                let _ = host.borrow_mut().apply_peer(PeerMessage::HotkeyEnd);
            });
            let mut input = NaturalInput::new(|| host.borrow().is_active(), backend, feel, (0, 0));

            let result = input.move_mouse_to((3000, 2000));
            assert_eq!(result, Err(InputError::SessionNotActive), "{feel:?}");
            assert_eq!(input.backend.moves.len(), 2, "{feel:?}");
            assert!(!host.borrow().is_active(), "{feel:?}");
            assert!(!helper.borrow().is_active(), "{feel:?}");
        }
    }

    #[test]
    fn peer_disconnect_stops_injection_the_same_way_as_a_hotkey() {
        // Instant is deliberately excluded: it's a single atomic step (see
        // `motion::plan_move`), so there is no "mid-motion" to interrupt —
        // covered instead by `instant_refuses_when_already_inactive` below.
        for feel in [InputFeel::Smooth, InputFeel::VeryNatural] {
            let (host, _helper) = active_pair();
            let backend = EndingBackend::new(Some(1), || {
                let _ = host.borrow_mut().apply_peer(PeerMessage::Disconnected);
            });
            let mut input = NaturalInput::new(|| host.borrow().is_active(), backend, feel, (0, 0));

            let result = input.move_mouse_to((3000, 2000));
            assert_eq!(result, Err(InputError::SessionNotActive), "{feel:?}");
            assert!(!host.borrow().is_active(), "{feel:?}");
        }
    }

    #[test]
    fn instant_refuses_when_already_inactive() {
        // Instant's single step still goes through the same gate check as
        // every other feel — it just has nothing to interrupt mid-motion.
        let host = RefCell::new(HandshakeMachine::new(Role::Host));
        let backend = EndingBackend::new(None, || {});
        let mut input = NaturalInput::new(|| host.borrow().is_active(), backend, InputFeel::Instant, (0, 0));

        let result = input.move_mouse_to((3000, 2000));
        assert_eq!(result, Err(InputError::SessionNotActive));
        assert!(input.backend.moves.is_empty());
    }

    #[test]
    fn click_releases_a_held_button_if_the_gate_closes_before_release() {
        let (host, _helper) = active_pair();
        // end_after_moves is never hit (a click's move to a point already
        // at (0,0) sends no move events); instead we end the session
        // directly after the button-down call by inspecting call order via
        // a second small helper backend.
        struct ClickEndingBackend<'a> {
            buttons: Vec<(MouseButton, bool)>,
            host: &'a RefCell<HandshakeMachine>,
        }
        impl<'a> InputBackend for ClickEndingBackend<'a> {
            fn move_cursor(&mut self, _x: i32, _y: i32) {}
            fn mouse_button(&mut self, button: MouseButton, pressed: bool) {
                self.buttons.push((button, pressed));
                if pressed {
                    // Simulate the session ending the instant after the
                    // button went down but before it comes back up.
                    let _ = self.host.borrow_mut().apply_local(LocalEvent::HotkeyEnd);
                }
            }
            fn key(&mut self, _c: KeyCode, _p: bool) {}
        }

        let backend = ClickEndingBackend { buttons: Vec::new(), host: &host };
        let mut input = NaturalInput::new(|| host.borrow().is_active(), backend, InputFeel::Instant, (0, 0));

        let result = input.click(MouseButton::Left, (0, 0));
        assert_eq!(result, Err(InputError::SessionNotActive));
        assert_eq!(
            input.backend.buttons,
            vec![(MouseButton::Left, true), (MouseButton::Left, false)],
            "the button natural_input pressed down must be released even though the session ended first"
        );
        assert!(input.held_buttons.is_empty());
    }

    #[test]
    fn stopped_key_press_releases_the_key() {
        let (host, _helper) = active_pair();
        struct KeyEndingBackend<'a> {
            keys: Vec<(KeyCode, bool)>,
            host: &'a RefCell<HandshakeMachine>,
        }
        impl<'a> InputBackend for KeyEndingBackend<'a> {
            fn move_cursor(&mut self, _x: i32, _y: i32) {}
            fn mouse_button(&mut self, _b: MouseButton, _p: bool) {}
            fn key(&mut self, code: KeyCode, pressed: bool) {
                self.keys.push((code, pressed));
                if pressed {
                    let _ = self.host.borrow_mut().apply_local(LocalEvent::HotkeyEnd);
                }
            }
        }

        let backend = KeyEndingBackend { keys: Vec::new(), host: &host };
        let mut input = NaturalInput::new(|| host.borrow().is_active(), backend, InputFeel::Instant, (0, 0));

        let result = input.key_press(KeyCode(0x41));
        assert_eq!(result, Err(InputError::SessionNotActive));
        assert_eq!(input.backend.keys, vec![(KeyCode(0x41), true), (KeyCode(0x41), false)]);
        assert!(input.held_keys.is_empty());
    }

    #[test]
    fn successful_click_when_session_stays_active_throughout() {
        let (host, _helper) = active_pair();
        let backend = EndingBackend::new(None, || {});
        let mut input = NaturalInput::new(|| host.borrow().is_active(), backend, InputFeel::Instant, (0, 0));

        let result = input.click(MouseButton::Left, (100, 100));
        assert_eq!(result, Ok(()));
        assert_eq!(
            input.backend.buttons,
            vec![(MouseButton::Left, true), (MouseButton::Left, false)]
        );
        assert_eq!(input.position(), (100, 100));
    }

    #[test]
    fn panic_release_works_even_with_nothing_held() {
        let (host, _helper) = active_pair();
        let backend = EndingBackend::new(None, || {});
        let mut input = NaturalInput::new(|| host.borrow().is_active(), backend, InputFeel::Smooth, (0, 0));
        input.release_all_held();
        assert!(input.backend.buttons.is_empty());
        assert!(input.backend.keys.is_empty());
    }
}
