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
#[serde(rename_all = "snake_case")]
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

/// How the `input` crate's `natural_input` module paces mouse movement and
/// click timing when *this* machine is sending input as the helper. Purely
/// a feel/comfort preference: every value goes through the exact same
/// consent gate (`NaturalInput` re-checks live session state before every
/// injected event, in every profile below), so this can never widen when or
/// whether input is allowed to be sent — only how it's shaped once it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputFeel {
    /// Direct, immediate cursor placement and key timing. No curvature, no
    /// jitter, no pauses — a deliberate choice for users who prefer
    /// precision over human-likeness, not an attempt to look robotic by
    /// accident.
    Instant,
    /// Eased, curved mouse movement with light click-timing jitter, natural
    /// pacing. The default.
    #[default]
    Smooth,
    /// Adds organic low-frequency tremor and more variable cadence on top
    /// of `Smooth`, for users who prefer movement to read as fully organic.
    VeryNatural,
}

/// The streaming bitrate ceiling for the current and future sessions. The
/// adaptive bitrate controller in `crates/transport` (`bitrate::
/// AdaptiveBitrateController`) never targets above this profile's
/// [`max_bps`](BandwidthProfile::max_bps), regardless of how clean the link
/// looks — it can only ever ask for less, never more, than what the host
/// chose here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BandwidthProfile {
    /// Up to 2 Mbps.
    #[default]
    Standard,
    /// Up to 0.5 Mbps, for constrained/metered links.
    Low,
}

impl BandwidthProfile {
    /// The hard ceiling in bits per second.
    pub const fn max_bps(self) -> u32 {
        match self {
            BandwidthProfile::Standard => 2_000_000,
            BandwidthProfile::Low => 500_000,
        }
    }
}

/// Host preferences for the current and future sessions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    pub overlay_visibility: OverlayVisibility,
    /// In-session sounds and notification cues. On by default; a "quiet
    /// session" is `sounds_enabled: false`.
    pub sounds_enabled: bool,
    pub input_feel: InputFeel,
    pub bandwidth_profile: BandwidthProfile,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            overlay_visibility: OverlayVisibility::Full,
            sounds_enabled: true,
            input_feel: InputFeel::Smooth,
            bandwidth_profile: BandwidthProfile::Standard,
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
        assert_eq!(s.input_feel, InputFeel::Smooth);
        assert_eq!(s.bandwidth_profile, BandwidthProfile::Standard);
    }

    #[test]
    fn bandwidth_profile_ceilings_match_the_phase_4_spec() {
        assert_eq!(BandwidthProfile::Standard.max_bps(), 2_000_000);
        assert_eq!(BandwidthProfile::Low.max_bps(), 500_000);
        assert!(BandwidthProfile::Low.max_bps() < BandwidthProfile::Standard.max_bps());
    }

    #[test]
    fn enum_wire_format_matches_the_lowercase_snake_case_values_the_ui_sends() {
        // crates/ui/main.js sets these fields directly from HTML radio
        // `value`s ("minimal_indicator", "very_natural", ...) — without
        // `rename_all = "snake_case"` those wouldn't deserialize against
        // the default PascalCase variant names at all, silently failing
        // every `update_settings` call from the settings panel.
        assert_eq!(serde_json::to_string(&OverlayVisibility::Full).unwrap(), "\"full\"");
        assert_eq!(
            serde_json::to_string(&OverlayVisibility::MinimalIndicator).unwrap(),
            "\"minimal_indicator\""
        );
        assert_eq!(serde_json::to_string(&OverlayVisibility::Presenter).unwrap(), "\"presenter\"");

        assert_eq!(serde_json::to_string(&InputFeel::Instant).unwrap(), "\"instant\"");
        assert_eq!(serde_json::to_string(&InputFeel::Smooth).unwrap(), "\"smooth\"");
        assert_eq!(serde_json::to_string(&InputFeel::VeryNatural).unwrap(), "\"very_natural\"");

        assert_eq!(serde_json::to_string(&BandwidthProfile::Standard).unwrap(), "\"standard\"");
        assert_eq!(serde_json::to_string(&BandwidthProfile::Low).unwrap(), "\"low\"");
    }

    #[test]
    fn round_trips_through_json() {
        let s = Settings {
            overlay_visibility: OverlayVisibility::Presenter,
            sounds_enabled: false,
            input_feel: InputFeel::VeryNatural,
            bandwidth_profile: BandwidthProfile::Low,
        };
        let json = serde_json::to_string(&s).expect("serialize");
        let back: Settings = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(s, back);
    }
}
