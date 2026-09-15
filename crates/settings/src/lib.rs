//! Host preferences for an in-progress session.
//!
//! This crate is pure data plus defaults — no file I/O, no window
//! manipulation, no Tauri. The desktop crate loads/saves a [`Settings`]
//! value and is the only place that acts on it (resizing/hiding the host's
//! own window, deciding whether to play a cue). Keeping it separate makes
//! the default values and their rationale auditable on their own, the same
//! way `consent` keeps the handshake auditable on its own.
//!
//! Defaults are the most transparent option in every case: the host sees
//! the full overlay and hears normal sounds/notifications unless they
//! explicitly choose otherwise. Nothing here can affect the pre-connection
//! consent screen — see `consent::HandshakeMachine`, which has no
//! `Settings` parameter anywhere in its API, so there is no code path for a
//! preference to suppress or skip it.

use serde::{Deserialize, Serialize};

/// How much of our own UI shows on the host's screen once a session is
/// [`consent::SessionState::Active`](../consent/enum.SessionState.html) —
/// never before. Purely a host comfort preference: our windows are always
/// excluded from the capture/stream regardless of this setting (see the
/// `capture` crate), so this only changes what the *host* sees locally.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum OverlayVisibility {
    /// The full status window: banner, hotkey hint, End Session button.
    #[default]
    Full,
    /// A small corner indicator: just a status dot and an End Session
    /// control, for hosts who want less on-screen clutter.
    MinimalIndicator,
    /// Fully hidden. For hosts who are separately recording or sharing
    /// their own screen (e.g. over a projector, or a capture method that
    /// isn't covered by the OS-level exclusion) and want zero footprint of
    /// our UI in what they show. The end-session hotkey still works.
    Presenter,
}

/// Host preferences for the current and future sessions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    pub overlay_visibility: OverlayVisibility,
    /// In-session sounds and notification cues. On by default; a "quiet
    /// session" is `sounds_enabled: false`.
    pub sounds_enabled: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            overlay_visibility: OverlayVisibility::Full,
            sounds_enabled: true,
        }
    }
}

impl Settings {
    /// Convenience reader matching the product-facing "quiet session" term.
    pub fn is_quiet_session(&self) -> bool {
        !self.sounds_enabled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_the_most_transparent_option() {
        let s = Settings::default();
        assert_eq!(s.overlay_visibility, OverlayVisibility::Full);
        assert!(s.sounds_enabled);
        assert!(!s.is_quiet_session());
    }

    #[test]
    fn round_trips_through_json() {
        let s = Settings {
            overlay_visibility: OverlayVisibility::Presenter,
            sounds_enabled: false,
        };
        let json = serde_json::to_string(&s).expect("serialize");
        let back: Settings = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(s, back);
    }
}
