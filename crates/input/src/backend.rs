//! Where injected events actually reach the OS.
//!
//! [`InputBackend`] is the only seam between [`crate::natural_input`] and a
//! real operating system: the trait itself carries no policy (no gating, no
//! pacing — that's `natural_input`'s job), just "make this one event
//! happen". Per CLAUDE.md, the only implementations that may exist are ones
//! backed by `SendInput` (Windows) or `CGEvent` (macOS) — no DLL injection,
//! no `SetWindowsHookEx`, no memory reads of other processes.

/// A mouse button, as understood by both platform backends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

/// A raw platform key code, passed through unmodified: Windows virtual-key
/// codes on Windows, `CGKeyCode`s on macOS. Kept as a thin newtype (no
/// cross-platform keymap) since translating human key names to codes is a
/// UI-layer concern, not this crate's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyCode(pub u16);

/// One platform's real input-injection primitive. Every method is a single
/// atomic OS event; `natural_input` is what turns a sequence of these into
/// a human-paced, consent-gated action.
pub trait InputBackend {
    /// Moves the cursor to an absolute screen position.
    fn move_cursor(&mut self, x: i32, y: i32);
    /// Presses (`pressed: true`) or releases (`pressed: false`) a mouse
    /// button at the cursor's current position.
    fn mouse_button(&mut self, button: MouseButton, pressed: bool);
    /// Presses or releases a key.
    fn key(&mut self, code: KeyCode, pressed: bool);
}

/// `SendInput`-backed injection. This is the only permitted way this crate
/// touches input on Windows — see the module docs and CLAUDE.md.
#[cfg(windows)]
pub mod windows {
    use super::{InputBackend, KeyCode, MouseButton};
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYBD_EVENT_FLAGS,
        KEYEVENTF_KEYUP, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
        MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN,
        MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK, MOUSEINPUT, VIRTUAL_KEY,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
        SM_YVIRTUALSCREEN,
    };

    /// Injects via `SendInput` exclusively — one `INPUT` struct per call,
    /// nothing batched, nothing cached across calls. Holds no OS handles of
    /// its own, so it's cheap to construct per session.
    #[derive(Default)]
    pub struct SendInputBackend;

    impl SendInputBackend {
        pub fn new() -> Self {
            Self
        }

        fn send(&self, input: INPUT) {
            // SAFETY: `input` is a single, fully-initialized INPUT value;
            // SendInput only reads it for the duration of this call.
            unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32) };
        }

        /// `SendInput`'s absolute mouse coordinates are normalized to
        /// 0..=65535 over the *virtual* desktop (all monitors), not raw
        /// pixels — this maps a real screen coordinate into that space.
        fn to_absolute(x: i32, y: i32) -> (i32, i32) {
            unsafe {
                let vx = GetSystemMetrics(SM_XVIRTUALSCREEN);
                let vy = GetSystemMetrics(SM_YVIRTUALSCREEN);
                let vw = GetSystemMetrics(SM_CXVIRTUALSCREEN).max(1);
                let vh = GetSystemMetrics(SM_CYVIRTUALSCREEN).max(1);
                let nx = ((x - vx) as i64 * 65535 / (vw - 1).max(1) as i64) as i32;
                let ny = ((y - vy) as i64 * 65535 / (vh - 1).max(1) as i64) as i32;
                (nx, ny)
            }
        }
    }

    impl InputBackend for SendInputBackend {
        fn move_cursor(&mut self, x: i32, y: i32) {
            let (nx, ny) = Self::to_absolute(x, y);
            self.send(INPUT {
                r#type: INPUT_MOUSE,
                Anonymous: INPUT_0 {
                    mi: MOUSEINPUT {
                        dx: nx,
                        dy: ny,
                        mouseData: 0,
                        dwFlags: MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            });
        }

        fn mouse_button(&mut self, button: MouseButton, pressed: bool) {
            let flag = match (button, pressed) {
                (MouseButton::Left, true) => MOUSEEVENTF_LEFTDOWN,
                (MouseButton::Left, false) => MOUSEEVENTF_LEFTUP,
                (MouseButton::Right, true) => MOUSEEVENTF_RIGHTDOWN,
                (MouseButton::Right, false) => MOUSEEVENTF_RIGHTUP,
                (MouseButton::Middle, true) => MOUSEEVENTF_MIDDLEDOWN,
                (MouseButton::Middle, false) => MOUSEEVENTF_MIDDLEUP,
            };
            self.send(INPUT {
                r#type: INPUT_MOUSE,
                Anonymous: INPUT_0 {
                    mi: MOUSEINPUT {
                        dx: 0,
                        dy: 0,
                        mouseData: 0,
                        dwFlags: flag,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            });
        }

        fn key(&mut self, code: KeyCode, pressed: bool) {
            let flags = if pressed {
                KEYBD_EVENT_FLAGS(0)
            } else {
                KEYEVENTF_KEYUP
            };
            self.send(INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: VIRTUAL_KEY(code.0),
                        wScan: 0,
                        dwFlags: flags,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            });
        }
    }
}

/// `CGEvent`-backed injection. Unverified on real hardware — there is no
/// macOS machine in this dev loop — written against the `core-graphics`
/// crate's documented `CGEvent::new_mouse_event` / `new_keyboard_event`
/// APIs, the same honesty caveat `capture::exclusion::macos` carries.
#[cfg(target_os = "macos")]
pub mod macos {
    use super::{InputBackend, KeyCode, MouseButton};
    use core_graphics::event::{
        CGEvent, CGEventTapLocation, CGEventType, CGKeyCode, CGMouseButton,
    };
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
    use core_graphics::geometry::CGPoint;

    /// Injects via `CGEvent` exclusively, posted at `HIDEventTap` — the
    /// same tap level real hardware input arrives at, and the only level
    /// consistent with "no process hooking" (it does not target or attach
    /// to any other process).
    pub struct CgEventBackend {
        source: CGEventSource,
        position: CGPoint,
    }

    impl CgEventBackend {
        pub fn new(start_x: f64, start_y: f64) -> Self {
            Self {
                source: CGEventSource::new(CGEventSourceStateID::HIDSystemState)
                    .expect("CGEventSource::new"),
                position: CGPoint::new(start_x, start_y),
            }
        }
    }

    impl InputBackend for CgEventBackend {
        fn move_cursor(&mut self, x: i32, y: i32) {
            self.position = CGPoint::new(x as f64, y as f64);
            if let Ok(event) = CGEvent::new_mouse_event(
                self.source.clone(),
                CGEventType::MouseMoved,
                self.position,
                CGMouseButton::Left,
            ) {
                event.post(CGEventTapLocation::HID);
            }
        }

        fn mouse_button(&mut self, button: MouseButton, pressed: bool) {
            let cg_button = match button {
                MouseButton::Left => CGMouseButton::Left,
                MouseButton::Right => CGMouseButton::Right,
                MouseButton::Middle => CGMouseButton::Center,
            };
            let event_type = match (button, pressed) {
                (MouseButton::Left, true) => CGEventType::LeftMouseDown,
                (MouseButton::Left, false) => CGEventType::LeftMouseUp,
                (MouseButton::Right, true) => CGEventType::RightMouseDown,
                (MouseButton::Right, false) => CGEventType::RightMouseUp,
                (MouseButton::Middle, true) => CGEventType::OtherMouseDown,
                (MouseButton::Middle, false) => CGEventType::OtherMouseUp,
            };
            if let Ok(event) =
                CGEvent::new_mouse_event(self.source.clone(), event_type, self.position, cg_button)
            {
                event.post(CGEventTapLocation::HID);
            }
        }

        fn key(&mut self, code: KeyCode, pressed: bool) {
            if let Ok(event) =
                CGEvent::new_keyboard_event(self.source.clone(), code.0 as CGKeyCode, pressed)
            {
                event.post(CGEventTapLocation::HID);
            }
        }
    }
}

impl InputBackend for Box<dyn InputBackend + Send> {
    fn move_cursor(&mut self, x: i32, y: i32) {
        (**self).move_cursor(x, y)
    }
    fn mouse_button(&mut self, button: MouseButton, pressed: bool) {
        (**self).mouse_button(button, pressed)
    }
    fn key(&mut self, code: KeyCode, pressed: bool) {
        (**self).key(code, pressed)
    }
}

/// A backend that discards every event. Exists purely so the workspace
/// compiles on platforms with neither a Windows nor a macOS backend (e.g. a
/// Linux dev machine) — it is never a way to bypass the gate: `NaturalInput`
/// still checks [`crate::natural_input::SessionGate`] before every call
/// into this, there is simply nowhere for the event to go once it arrives.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullBackend;

impl InputBackend for NullBackend {
    fn move_cursor(&mut self, _x: i32, _y: i32) {}
    fn mouse_button(&mut self, _button: MouseButton, _pressed: bool) {}
    fn key(&mut self, _code: KeyCode, _pressed: bool) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct FakeBackend {
        moves: Vec<(i32, i32)>,
        buttons: Vec<(MouseButton, bool)>,
        keys: Vec<(KeyCode, bool)>,
    }

    impl InputBackend for FakeBackend {
        fn move_cursor(&mut self, x: i32, y: i32) {
            self.moves.push((x, y));
        }
        fn mouse_button(&mut self, button: MouseButton, pressed: bool) {
            self.buttons.push((button, pressed));
        }
        fn key(&mut self, code: KeyCode, pressed: bool) {
            self.keys.push((code, pressed));
        }
    }

    #[test]
    fn fake_backend_records_calls_in_order() {
        let mut backend = FakeBackend::default();
        backend.move_cursor(10, 20);
        backend.mouse_button(MouseButton::Left, true);
        backend.mouse_button(MouseButton::Left, false);
        backend.key(KeyCode(0x41), true);
        backend.key(KeyCode(0x41), false);

        assert_eq!(backend.moves, vec![(10, 20)]);
        assert_eq!(
            backend.buttons,
            vec![(MouseButton::Left, true), (MouseButton::Left, false)]
        );
        assert_eq!(
            backend.keys,
            vec![(KeyCode(0x41), true), (KeyCode(0x41), false)]
        );
    }
}
