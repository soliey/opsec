//! Where the host's own status window sits on screen, per
//! [`settings::OverlayVisibility`].
//!
//! This module is pure geometry — no Tauri, no window creation — so both
//! the running app and the capture-exclusion test can drive real windows
//! from the exact same numbers instead of the test guessing at duplicated
//! constants.
//!
//! Exclusion from capture (`capture::exclusion`) is independent of this and
//! is applied once, unconditionally, when a window is created — every mode
//! here still keeps the window excluded. What changes per mode is only
//! what the *host* sees locally.

use settings::OverlayVisibility;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// The host window's on-screen geometry for `mode`, or `None` if the
/// window should be hidden entirely (Presenter).
pub fn geometry_for(mode: OverlayVisibility) -> Option<Rect> {
    match mode {
        OverlayVisibility::Full => Some(Rect {
            x: 60,
            y: 80,
            width: 460,
            height: 640,
        }),
        OverlayVisibility::MinimalIndicator => Some(Rect {
            x: 40,
            y: 40,
            width: 260,
            height: 72,
        }),
        OverlayVisibility::Presenter => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_and_minimal_have_geometry_presenter_does_not() {
        assert!(geometry_for(OverlayVisibility::Full).is_some());
        assert!(geometry_for(OverlayVisibility::MinimalIndicator).is_some());
        assert!(geometry_for(OverlayVisibility::Presenter).is_none());
    }

    #[test]
    fn minimal_is_smaller_than_full() {
        let full = geometry_for(OverlayVisibility::Full).unwrap();
        let minimal = geometry_for(OverlayVisibility::MinimalIndicator).unwrap();
        assert!(minimal.width < full.width);
        assert!(minimal.height < full.height);
    }
}
