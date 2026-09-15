//! Pure, OS-independent generation of human-paced mouse paths.
//!
//! No window handles, no syscalls, no gating — just turning a start point,
//! an end point, and an [`InputFeel`] into the sequence of intermediate
//! points [`crate::natural_input::NaturalInput`] will actually send. Kept
//! separate from `natural_input` so the shape of the motion (curvature,
//! easing, jitter) is testable on its own, the same way `consent::code`'s
//! format is testable independent of the handshake that uses it.
//!
//! Every jitter/curvature value here is sampled fresh from `rand::
//! thread_rng()` on each call — seeded randomness, never a fixed table of
//! offsets — so two calls with identical `from`/`to`/`feel` produce
//! different intermediate paths (see the `never_repeats_exactly` test).

use crate::InputFeel;
use rand::Rng;
use std::f64::consts::TAU;
use std::time::Duration;

/// One point along a planned mouse path, plus how long to pause after
/// reaching it before sending the next one (zero for most steps — the
/// occasional nonzero pause is what gives "small random pauses").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MotionStep {
    pub x: i32,
    pub y: i32,
    pub pause_after: Duration,
}

/// Target spacing, in pixels, between consecutive `Smooth`/`VeryNatural`
/// steps. Small enough that no single step reads as a teleport, large
/// enough that a long move doesn't need hundreds of `SendInput` calls.
const STEP_SPACING_PX: f64 = 12.0;
const MIN_STEPS: usize = 6;
const MAX_STEPS: usize = 80;

/// Plans the path from `from` to `to` for the given feel. Returns an empty
/// vec if `from == to` (nothing to move). `Instant` is a single step — a
/// deliberate, user-chosen precision mode, not a bug: see [`InputFeel`].
/// `Smooth` and `VeryNatural` always return at least [`MIN_STEPS`] points
/// for any nonzero move, so the cursor is never sent in one jump — "never
/// teleport the cursor" per CLAUDE.md/the phase 3 spec.
pub fn plan_move(from: (i32, i32), to: (i32, i32), feel: InputFeel) -> Vec<MotionStep> {
    if from == to {
        return Vec::new();
    }
    match feel {
        InputFeel::Instant => vec![MotionStep {
            x: to.0,
            y: to.1,
            pause_after: Duration::ZERO,
        }],
        InputFeel::Smooth | InputFeel::VeryNatural => plan_curved_move(from, to, feel),
    }
}

/// Smootherstep: zero first and second derivative at both ends, giving a
/// natural ease-in/ease-out velocity curve (Perlin's improved variant of
/// smoothstep).
fn ease(t: f64) -> f64 {
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

fn plan_curved_move(from: (i32, i32), to: (i32, i32), feel: InputFeel) -> Vec<MotionStep> {
    let mut rng = rand::thread_rng();

    let (x0, y0) = (from.0 as f64, from.1 as f64);
    let (x1, y1) = (to.0 as f64, to.1 as f64);
    let dx = x1 - x0;
    let dy = y1 - y0;
    let distance = (dx * dx + dy * dy).sqrt();

    let steps = ((distance / STEP_SPACING_PX).round() as usize).clamp(MIN_STEPS, MAX_STEPS);

    // A quadratic Bezier control point, offset perpendicular to the
    // straight line by a random fraction of the distance (either side) —
    // this is the "slight curvature": real hand-guided mouse movement
    // rarely travels in a perfectly straight line.
    let (ux, uy) = (-dy / distance, dx / distance);
    let curvature_sign = if rng.gen_bool(0.5) { 1.0 } else { -1.0 };
    let curvature_frac: f64 = rng.gen_range(0.05..0.16) * curvature_sign;
    let control_offset = distance * curvature_frac;
    let (cx, cy) = (
        x0 + dx * 0.5 + ux * control_offset,
        y0 + dy * 0.5 + uy * control_offset,
    );

    // VeryNatural adds a low-frequency wobble (organic tremor) on top of
    // the base curve; Smooth doesn't.
    let tremor_amp: f64 = match feel {
        InputFeel::VeryNatural => rng.gen_range(0.8..2.2),
        _ => 0.0,
    };
    let tremor_freq: f64 = rng.gen_range(1.5..4.0);
    let tremor_phase: f64 = rng.gen_range(0.0..TAU);

    let jitter_amp: f64 = match feel {
        InputFeel::VeryNatural => 1.2,
        _ => 0.5,
    };
    let pause_probability: f64 = match feel {
        InputFeel::VeryNatural => 0.15,
        _ => 0.06,
    };
    let pause_range_ms: std::ops::Range<u64> = match feel {
        InputFeel::VeryNatural => 40..160,
        _ => 15..60,
    };

    let mut out = Vec::with_capacity(steps);
    for i in 1..=steps {
        let t_linear = i as f64 / steps as f64;
        let is_last = i == steps;
        let t = ease(t_linear);

        let one_minus_t = 1.0 - t;
        let bx = one_minus_t * one_minus_t * x0 + 2.0 * one_minus_t * t * cx + t * t * x1;
        let by = one_minus_t * one_minus_t * y0 + 2.0 * one_minus_t * t * cy + t * t * y1;

        let (jx, jy) = if is_last {
            // Always land exactly on the requested target.
            (0.0, 0.0)
        } else {
            let noise_x = rng.gen_range(-jitter_amp..jitter_amp);
            let noise_y = rng.gen_range(-jitter_amp..jitter_amp);
            let tremor = if tremor_amp > 0.0 {
                (t_linear * tremor_freq * TAU + tremor_phase).sin() * tremor_amp
            } else {
                0.0
            };
            (noise_x + ux * tremor, noise_y + uy * tremor)
        };

        let pause_after = if !is_last && rng.gen_bool(pause_probability) {
            Duration::from_millis(rng.gen_range(pause_range_ms.clone()))
        } else {
            Duration::ZERO
        };

        out.push(MotionStep {
            x: (bx + jx).round() as i32,
            y: (by + jy).round() as i32,
            pause_after,
        });
    }

    // Guard against float rounding at t=1 landing a pixel off target.
    if let Some(last) = out.last_mut() {
        last.x = to.0;
        last.y = to.1;
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_move_when_already_at_target() {
        assert!(plan_move((100, 100), (100, 100), InputFeel::Smooth).is_empty());
        assert!(plan_move((100, 100), (100, 100), InputFeel::Instant).is_empty());
        assert!(plan_move((100, 100), (100, 100), InputFeel::VeryNatural).is_empty());
    }

    #[test]
    fn instant_is_a_single_jump_to_target() {
        let steps = plan_move((0, 0), (500, 500), InputFeel::Instant);
        assert_eq!(steps.len(), 1);
        assert_eq!((steps[0].x, steps[0].y), (500, 500));
    }

    #[test]
    fn smooth_and_very_natural_never_teleport() {
        for feel in [InputFeel::Smooth, InputFeel::VeryNatural] {
            let steps = plan_move((0, 0), (800, 600), feel);
            assert!(
                steps.len() >= MIN_STEPS,
                "{feel:?}: expected at least {MIN_STEPS} steps, got {}",
                steps.len()
            );

            // No single step may cover a large fraction of the total
            // distance — that would be a teleport in disguise.
            let start: (f64, f64) = (0.0, 0.0);
            let end: (f64, f64) = (800.0, 600.0);
            let full_distance = (end.0 - start.0).hypot(end.1 - start.1);
            let mut prev = start;
            for step in &steps {
                let d = (step.x as f64 - prev.0).hypot(step.y as f64 - prev.1);
                assert!(
                    d <= full_distance * 0.4,
                    "{feel:?}: a single step moved {d:.1}px, more than 40% of the {full_distance:.1}px total move"
                );
                prev = (step.x as f64, step.y as f64);
            }
        }
    }

    #[test]
    fn smooth_and_very_natural_land_exactly_on_target() {
        for feel in [InputFeel::Smooth, InputFeel::VeryNatural] {
            let steps = plan_move((10, 10), (937, 412), feel);
            let last = steps.last().unwrap();
            assert_eq!((last.x, last.y), (937, 412));
        }
    }

    #[test]
    fn jitter_never_repeats_exactly() {
        // Same start/end/feel, called twice: seeded-per-call randomness
        // means the intermediate paths must differ (the final point is
        // always identical by design, so compare everything but that).
        let a = plan_move((0, 0), (900, 50), InputFeel::Smooth);
        let b = plan_move((0, 0), (900, 50), InputFeel::Smooth);
        let a_without_last = &a[..a.len() - 1];
        let b_without_last = &b[..b.len() - 1];
        assert_ne!(
            a_without_last, b_without_last,
            "two motion plans for the same start/end used identical (fixed) offsets"
        );
    }

    #[test]
    fn very_natural_has_more_pauses_than_smooth_on_average() {
        let count_pauses = |feel: InputFeel| -> usize {
            let mut total = 0;
            for _ in 0..40 {
                let steps = plan_move((0, 0), (1200, 900), feel);
                total += steps.iter().filter(|s| !s.pause_after.is_zero()).count();
            }
            total
        };

        let smooth_pauses = count_pauses(InputFeel::Smooth);
        let very_natural_pauses = count_pauses(InputFeel::VeryNatural);
        assert!(
            very_natural_pauses > smooth_pauses,
            "expected VeryNatural ({very_natural_pauses}) to pause more often than Smooth ({smooth_pauses}) over many samples"
        );
    }

    #[test]
    fn curvature_deviates_from_the_straight_line() {
        // Over many samples, at least some midpoints should be off the
        // straight line by a visible amount — proving the path isn't just
        // a jittered straight interpolation.
        let mut max_deviation = 0.0_f64;
        for _ in 0..20 {
            let steps = plan_move((0, 0), (1000, 0), InputFeel::Smooth);
            for step in &steps {
                max_deviation = max_deviation.max(step.y.unsigned_abs() as f64);
            }
        }
        assert!(
            max_deviation > 3.0,
            "expected visible perpendicular curvature over many samples, max deviation was {max_deviation}"
        );
    }
}
