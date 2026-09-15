//! Still-screen optimization: while the host's screen barely changes
//! (reading, writing, a static document), drop the send rate toward
//! near-zero rather than re-encoding and sending a stream of
//! nearly-identical frames — the single biggest lever for the "no
//! noticeable fan noise, CPU, or GPU load" resource target during the
//! large fraction of a typical session spent looking at a mostly-static
//! screen.
//!
//! Pure pacing decision — no capture, no encoding. The caller feeds it a
//! cheap per-frame change score (e.g. the fraction of sampled pixels that
//! differ from the previous frame by more than a noise threshold — a
//! capture-crate concern, not this one's) and asks
//! [`StillScreenPacer::should_send`] before doing any encode work at all.

use std::time::{Duration, Instant};

/// Below this fraction-changed, a frame counts as "still".
const STILL_THRESHOLD: f32 = 0.01;
/// Consecutive still frames required before pacing actually drops — a
/// couple of still frames could just be a blink between keystrokes, not a
/// truly static screen.
const STILL_STREAK_TO_PACE: u32 = 5;
/// Normal cadence while the screen is active.
const ACTIVE_INTERVAL: Duration = Duration::from_millis(33); // ~30 fps
/// Near-zero cadence once confirmed still. Not literally 0 — an occasional
/// frame still goes out so a slowly-drifting difference (a blinking
/// cursor, a clock) eventually catches up, and so the link doesn't look
/// dead.
const STILL_INTERVAL: Duration = Duration::from_secs(3);

/// Whether to actually encode+send this frame, and if not, how long until
/// the next one is worth trying.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendDecision {
    /// Encode and send this frame; the next candidate frame should be
    /// considered again in `next_interval`.
    Send { next_interval: Duration },
    /// Skip this frame entirely (no encode, no send) — try again after
    /// `retry_after`.
    Skip { retry_after: Duration },
}

pub struct StillScreenPacer {
    still_streak: u32,
    is_pacing: bool,
    last_sent_at: Option<Instant>,
}

impl Default for StillScreenPacer {
    fn default() -> Self {
        Self::new()
    }
}

impl StillScreenPacer {
    pub fn new() -> Self {
        Self { still_streak: 0, is_pacing: false, last_sent_at: None }
    }

    /// `changed_fraction` is the fraction of sampled pixels that changed
    /// since the last frame, in `[0.0, 1.0]`. `now` is injected (rather
    /// than read internally) so this stays a pure, deterministic function
    /// of its inputs and is trivially testable without real sleeps.
    pub fn decide(&mut self, changed_fraction: f32, now: Instant) -> SendDecision {
        let is_still_frame = changed_fraction <= STILL_THRESHOLD;

        if is_still_frame {
            self.still_streak = self.still_streak.saturating_add(1);
        } else {
            // Any real activity snaps pacing off immediately — going from
            // still to active must be responsive even though going the
            // other way is deliberately gradual (the streak requirement).
            self.still_streak = 0;
            self.is_pacing = false;
        }

        if !self.is_pacing && self.still_streak >= STILL_STREAK_TO_PACE {
            self.is_pacing = true;
        }

        let min_interval = if self.is_pacing { STILL_INTERVAL } else { ACTIVE_INTERVAL };

        let due = match self.last_sent_at {
            Some(last) => now.saturating_duration_since(last) >= min_interval,
            None => true,
        };

        if due {
            self.last_sent_at = Some(now);
            SendDecision::Send { next_interval: min_interval }
        } else {
            let elapsed = self.last_sent_at.map(|l| now.saturating_duration_since(l)).unwrap_or(Duration::ZERO);
            SendDecision::Skip { retry_after: min_interval.saturating_sub(elapsed) }
        }
    }

    pub fn is_pacing(&self) -> bool {
        self.is_pacing
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(offset: Duration) -> Instant {
        // A fixed base plus an offset gives deterministic, comparable
        // instants without depending on wall-clock timing.
        Instant::now().checked_sub(Duration::from_secs(3600)).unwrap() + offset
    }

    #[test]
    fn active_screen_sends_at_full_cadence() {
        let mut pacer = StillScreenPacer::new();
        let mut now = t(Duration::ZERO);
        let mut sends = 0;
        for _ in 0..30 {
            if matches!(pacer.decide(0.5, now), SendDecision::Send { .. }) {
                sends += 1;
            }
            now += ACTIVE_INTERVAL;
        }
        assert_eq!(sends, 30, "every frame should send while content is actively changing");
        assert!(!pacer.is_pacing());
    }

    #[test]
    fn sustained_still_content_drops_into_near_zero_pacing() {
        let mut pacer = StillScreenPacer::new();
        let mut now = t(Duration::ZERO);
        for _ in 0..(STILL_STREAK_TO_PACE + 2) {
            pacer.decide(0.0, now);
            now += ACTIVE_INTERVAL;
        }
        assert!(pacer.is_pacing(), "a sustained streak of still frames must engage pacing");
    }

    #[test]
    fn pacing_sends_far_fewer_frames_than_active_cadence_would() {
        let mut pacer = StillScreenPacer::new();
        let mut now = t(Duration::ZERO);
        // Drive it into pacing first.
        for _ in 0..(STILL_STREAK_TO_PACE + 1) {
            pacer.decide(0.0, now);
            now += ACTIVE_INTERVAL;
        }
        assert!(pacer.is_pacing());

        // Simulate 30 seconds of continued stillness at the *active*
        // cadence being offered (i.e. the caller keeps asking every
        // ~33ms, as it would for a live capture loop) and count sends.
        let window = Duration::from_secs(30);
        let end = now + window;
        let mut sends_during_pacing = 0;
        while now < end {
            if matches!(pacer.decide(0.0, now), SendDecision::Send { .. }) {
                sends_during_pacing += 1;
            }
            now += ACTIVE_INTERVAL;
        }
        let active_cadence_would_have_sent = (window.as_secs_f64() / ACTIVE_INTERVAL.as_secs_f64()) as u32;
        assert!(
            sends_during_pacing < active_cadence_would_have_sent / 10,
            "pacing should send far less than 10% of the active-cadence frame count \
             (got {sends_during_pacing} vs active {active_cadence_would_have_sent})"
        );
        // Still not literally zero — near-zero, not dead.
        assert!(sends_during_pacing > 0);
    }

    #[test]
    fn a_single_change_after_stillness_snaps_back_to_active_immediately() {
        let mut pacer = StillScreenPacer::new();
        let mut now = t(Duration::ZERO);
        for _ in 0..(STILL_STREAK_TO_PACE + 1) {
            pacer.decide(0.0, now);
            now += ACTIVE_INTERVAL;
        }
        assert!(pacer.is_pacing());

        // Real activity resumes.
        let decision = pacer.decide(0.8, now);
        assert!(!pacer.is_pacing(), "activity must clear pacing immediately, not gradually");
        assert!(matches!(decision, SendDecision::Send { .. }));

        // And cadence is back to active-rate immediately on the next frame.
        now += ACTIVE_INTERVAL;
        let decision = pacer.decide(0.8, now);
        assert!(matches!(decision, SendDecision::Send { .. }));
    }

    #[test]
    fn brief_flicker_of_stillness_does_not_engage_pacing() {
        let mut pacer = StillScreenPacer::new();
        let mut now = t(Duration::ZERO);
        // Fewer still frames than the streak requirement, interleaved with
        // activity — should never engage pacing (e.g. a blink mid-typing).
        for _ in 0..(STILL_STREAK_TO_PACE - 1) {
            pacer.decide(0.0, now);
            now += ACTIVE_INTERVAL;
        }
        pacer.decide(0.5, now);
        assert!(!pacer.is_pacing());
    }
}
