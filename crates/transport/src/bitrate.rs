//! Adaptive bitrate control: AIMD (additive-increase/multiplicative-decrease)
//! congestion control, the same family as TCP Reno and the core of WebRTC's
//! own GCC — cut hard and immediately on congestion, climb back slowly and
//! in small steps once the link looks clean. "Never spikes" per the phase 4
//! spec means exactly that asymmetry: a bad `report()` can drop the target
//! by a third in one call, but a good one can only ever nudge it up by a
//! small, bounded fraction, and always inside [`settings::BandwidthProfile`]'s
//! ceiling.
//!
//! Pure state machine — no sockets, no timers, no encoder calls. The
//! caller (`crates/desktop`) feeds it periodic [`CongestionReport`]s from
//! whatever the real transport's feedback is (WebRTC RTCP receiver
//! reports, in a future real integration; `crates/transport::loopback`'s
//! own throughput bookkeeping today) and reads `target_bps()` before
//! encoding each frame.

use settings::BandwidthProfile;
use std::time::Duration;

/// One period's observed link health. `loss_fraction` is packets lost /
/// packets sent in `[0.0, 1.0]`; `rtt` is the current round-trip estimate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CongestionReport {
    pub loss_fraction: f32,
    pub rtt: Duration,
}

/// Above this fraction lost, the link is congested: cut the bitrate.
const LOSS_CONGESTION_THRESHOLD: f32 = 0.10;
/// Below this fraction lost, the link is clean enough to probe upward.
const LOSS_CLEAN_THRESHOLD: f32 = 0.02;
/// A jump in RTT this large (relative to the trailing estimate) also
/// counts as congestion, even with acceptable loss — bufferbloat shows up
/// as delay before it shows up as drops.
const RTT_SPIKE_RATIO: f32 = 1.5;

/// Multiplicative cut applied on congestion (matches classic AIMD's ~0.5,
/// slightly gentler since video quality degrades visibly at hard cuts).
const DECREASE_FACTOR: f32 = 0.7;
/// Additive climb applied per clean report, as a fraction of the profile
/// ceiling — bounded so recovery is always gradual, never a jump straight
/// back to the ceiling.
const INCREASE_STEP_FRACTION: f32 = 0.05;
/// Never target below this floor — encoders (and viewers) stop being
/// useful well before this, so there's nothing to gain by chasing zero.
const MIN_BPS: u32 = 100_000;

pub struct AdaptiveBitrateController {
    profile: BandwidthProfile,
    target_bps: u32,
    trailing_rtt: Option<Duration>,
}

impl AdaptiveBitrateController {
    /// Starts at the profile's ceiling — optimistic by default, the same
    /// way `settings::Settings::default()` is the most transparent option
    /// by default; the first sign of congestion pulls it down from there.
    pub fn new(profile: BandwidthProfile) -> Self {
        Self {
            profile,
            target_bps: profile.max_bps(),
            trailing_rtt: None,
        }
    }

    pub fn target_bps(&self) -> u32 {
        self.target_bps
    }

    /// Applies a new setting, immediately re-clamping the current target
    /// to the new ceiling (e.g. switching Standard -> Low mid-session must
    /// never leave the target above the new profile's cap).
    pub fn set_profile(&mut self, profile: BandwidthProfile) {
        self.profile = profile;
        self.target_bps = self.target_bps.min(profile.max_bps());
    }

    /// Feeds one period's congestion signal and updates the target.
    pub fn report(&mut self, report: CongestionReport) {
        let rtt_spiked = match self.trailing_rtt {
            Some(trailing) if trailing > Duration::ZERO => {
                report.rtt.as_secs_f32() > trailing.as_secs_f32() * RTT_SPIKE_RATIO
            }
            _ => false,
        };
        self.trailing_rtt = Some(report.rtt);

        if report.loss_fraction >= LOSS_CONGESTION_THRESHOLD || rtt_spiked {
            let cut = (self.target_bps as f32 * DECREASE_FACTOR) as u32;
            self.target_bps = cut.max(MIN_BPS);
        } else if report.loss_fraction <= LOSS_CLEAN_THRESHOLD {
            let ceiling = self.profile.max_bps();
            let step = (ceiling as f32 * INCREASE_STEP_FRACTION) as u32;
            self.target_bps = self.target_bps.saturating_add(step).min(ceiling);
        }
        // Between the clean and congested thresholds: hold steady. Chasing
        // every small fluctuation is itself a source of visible quality
        // jitter, which is exactly what "never spikes" is guarding against.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clean_report() -> CongestionReport {
        CongestionReport { loss_fraction: 0.0, rtt: Duration::from_millis(40) }
    }

    fn congested_report() -> CongestionReport {
        CongestionReport { loss_fraction: 0.25, rtt: Duration::from_millis(40) }
    }

    #[test]
    fn starts_at_the_profile_ceiling() {
        assert_eq!(
            AdaptiveBitrateController::new(BandwidthProfile::Standard).target_bps(),
            BandwidthProfile::Standard.max_bps()
        );
        assert_eq!(
            AdaptiveBitrateController::new(BandwidthProfile::Low).target_bps(),
            BandwidthProfile::Low.max_bps()
        );
    }

    #[test]
    fn never_exceeds_the_profile_ceiling_no_matter_how_many_clean_reports() {
        for profile in [BandwidthProfile::Standard, BandwidthProfile::Low] {
            let mut c = AdaptiveBitrateController::new(profile);
            for _ in 0..1000 {
                c.report(clean_report());
            }
            assert!(c.target_bps() <= profile.max_bps());
        }
    }

    #[test]
    fn congestion_cuts_the_target_immediately_and_substantially() {
        let mut c = AdaptiveBitrateController::new(BandwidthProfile::Standard);
        let before = c.target_bps();
        c.report(congested_report());
        assert!(
            c.target_bps() < before,
            "a single congested report must cut the target, not wait for more evidence"
        );
        assert!(
            (c.target_bps() as f32) <= before as f32 * 0.8,
            "the cut should be substantial (AIMD multiplicative decrease), not a token nudge"
        );
    }

    #[test]
    fn recovery_after_congestion_is_gradual_never_a_spike_back_to_ceiling() {
        let mut c = AdaptiveBitrateController::new(BandwidthProfile::Standard);
        c.report(congested_report());
        let after_cut = c.target_bps();
        assert!(after_cut < BandwidthProfile::Standard.max_bps());

        c.report(clean_report());
        let after_one_clean_report = c.target_bps();
        assert!(
            after_one_clean_report > after_cut,
            "a clean report should start recovering the target"
        );
        assert!(
            after_one_clean_report < BandwidthProfile::Standard.max_bps(),
            "one clean report must never jump straight back to the ceiling — that's the 'never spikes' guarantee"
        );
    }

    #[test]
    fn repeated_congestion_never_drops_below_the_floor() {
        let mut c = AdaptiveBitrateController::new(BandwidthProfile::Low);
        for _ in 0..1000 {
            c.report(congested_report());
        }
        assert!(c.target_bps() >= MIN_BPS);
    }

    #[test]
    fn an_rtt_spike_alone_counts_as_congestion_even_with_low_loss() {
        let mut c = AdaptiveBitrateController::new(BandwidthProfile::Standard);
        // Establish a trailing RTT baseline with a clean report first.
        c.report(CongestionReport { loss_fraction: 0.0, rtt: Duration::from_millis(30) });
        let before = c.target_bps();
        // Loss is well under the congestion threshold, but RTT more than
        // doubled — bufferbloat, cut anyway.
        c.report(CongestionReport { loss_fraction: 0.0, rtt: Duration::from_millis(120) });
        assert!(c.target_bps() < before);
    }

    #[test]
    fn switching_to_a_lower_profile_mid_session_reclamps_immediately() {
        let mut c = AdaptiveBitrateController::new(BandwidthProfile::Standard);
        assert_eq!(c.target_bps(), BandwidthProfile::Standard.max_bps());
        c.set_profile(BandwidthProfile::Low);
        assert!(c.target_bps() <= BandwidthProfile::Low.max_bps());
    }

    #[test]
    fn moderate_loss_between_thresholds_holds_steady() {
        let mut c = AdaptiveBitrateController::new(BandwidthProfile::Standard);
        let before = c.target_bps();
        c.report(CongestionReport { loss_fraction: 0.05, rtt: Duration::from_millis(40) });
        assert_eq!(c.target_bps(), before, "loss between clean/congested thresholds should not move the target");
    }
}
