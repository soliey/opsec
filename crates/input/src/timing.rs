//! Pure generation of human-like click timing and inter-action pauses.
//!
//! Same spirit as [`crate::motion`]: no OS calls, just numbers, sampled
//! fresh from `rand::thread_rng()` on every call — never a fixed value
//! reused call to call (except for [`InputFeel::Instant`], which is
//! deliberately fixed at (near) zero, matching its "robotic-precise"
//! product description).

use crate::InputFeel;
use rand::Rng;
use std::time::Duration;

/// How long a mouse button (or key, for a tap-style press) stays down
/// between the press and release halves of a click/press action.
pub fn click_hold_duration(feel: InputFeel) -> Duration {
    match feel {
        InputFeel::Instant => Duration::from_millis(20),
        InputFeel::Smooth => Duration::from_millis(rand::thread_rng().gen_range(60..140)),
        InputFeel::VeryNatural => Duration::from_millis(rand::thread_rng().gen_range(70..190)),
    }
}

/// A small pause before an action begins (e.g. between arriving at a click
/// target and pressing the button) — the "human-like click-timing jitter"
/// called for in the phase 3 spec. `VeryNatural` occasionally inserts a
/// longer hesitation on top of its usual range, for "more variable
/// cadence".
pub fn inter_action_pause(feel: InputFeel) -> Duration {
    let mut rng = rand::thread_rng();
    match feel {
        InputFeel::Instant => Duration::ZERO,
        InputFeel::Smooth => Duration::from_millis(rng.gen_range(20..90)),
        InputFeel::VeryNatural => {
            if rng.gen_bool(0.12) {
                Duration::from_millis(rng.gen_range(150..400))
            } else {
                Duration::from_millis(rng.gen_range(40..160))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instant_is_fixed_and_minimal() {
        for _ in 0..20 {
            assert_eq!(click_hold_duration(InputFeel::Instant), Duration::from_millis(20));
            assert_eq!(inter_action_pause(InputFeel::Instant), Duration::ZERO);
        }
    }

    #[test]
    fn smooth_and_very_natural_vary_across_calls() {
        for feel in [InputFeel::Smooth, InputFeel::VeryNatural] {
            let holds: std::collections::HashSet<_> =
                (0..30).map(|_| click_hold_duration(feel)).collect();
            assert!(
                holds.len() > 1,
                "{feel:?}: expected click-hold duration to vary across calls, all {} samples were identical",
                holds.len()
            );

            let pauses: std::collections::HashSet<_> =
                (0..30).map(|_| inter_action_pause(feel)).collect();
            assert!(
                pauses.len() > 1,
                "{feel:?}: expected inter-action pause to vary across calls"
            );
        }
    }

    #[test]
    fn very_natural_has_wider_range_than_smooth() {
        let sample = |feel: InputFeel| -> (Duration, Duration) {
            let mut min = Duration::MAX;
            let mut max = Duration::ZERO;
            for _ in 0..200 {
                let d = click_hold_duration(feel);
                min = min.min(d);
                max = max.max(d);
            }
            (min, max)
        };
        let (smooth_min, smooth_max) = sample(InputFeel::Smooth);
        let (very_min, very_max) = sample(InputFeel::VeryNatural);
        assert!(smooth_min < smooth_max);
        assert!(very_min < very_max);
        assert!(
            very_max - very_min >= smooth_max - smooth_min,
            "expected VeryNatural's click-hold range to be at least as wide as Smooth's"
        );
    }
}
