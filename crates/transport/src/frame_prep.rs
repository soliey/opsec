//! Pure per-frame preparation: how much a frame changed since the last one
//! (feeds `still_screen::StillScreenPacer`), and capping resolution to a
//! bandwidth profile's budget (part of the "no noticeable fan noise, CPU,
//! or GPU load" resource target — encoding a smaller frame is the single
//! biggest lever available without a hardware encode pipeline).
//!
//! Both are plain pixel-buffer math — no capture, no encoding, no OS
//! calls — so they're testable without a real display.

use capture::BgraFrame;
use settings::BandwidthProfile;

/// Per-channel difference (out of 255) above which a sampled pixel counts
/// as "changed" — small enough to catch real content changes, large enough
/// to ignore capture noise/dithering on an otherwise-static screen.
const PIXEL_NOISE_THRESHOLD: u8 = 12;
/// Sampling stride: compare every Nth pixel rather than the whole frame.
/// A full per-pixel diff on every captured frame would itself burn CPU —
/// exactly what still-screen detection exists to avoid.
const SAMPLE_STRIDE: usize = 7;

/// Fraction of sampled pixels that changed between `prev` and `curr`, in
/// `[0.0, 1.0]`. Returns `1.0` (treat as fully changed) if the frames
/// differ in size or `prev` is `None` — a resolution change or the very
/// first frame is never "still".
pub fn changed_fraction(prev: Option<&BgraFrame>, curr: &BgraFrame) -> f32 {
    let Some(prev) = prev else {
        return 1.0;
    };
    if prev.width != curr.width || prev.height != curr.height || prev.data.len() != curr.data.len() {
        return 1.0;
    }
    if curr.data.is_empty() {
        return 0.0;
    }

    let mut sampled = 0usize;
    let mut changed = 0usize;
    let pixel_count = curr.data.len() / 4;
    let mut pixel_index = 0usize;
    while pixel_index < pixel_count {
        let byte_index = pixel_index * 4;
        let a = &prev.data[byte_index..byte_index + 4];
        let b = &curr.data[byte_index..byte_index + 4];
        sampled += 1;
        let differs = a
            .iter()
            .zip(b.iter())
            .any(|(x, y)| x.abs_diff(*y) > PIXEL_NOISE_THRESHOLD);
        if differs {
            changed += 1;
        }
        pixel_index += SAMPLE_STRIDE;
    }

    if sampled == 0 {
        0.0
    } else {
        changed as f32 / sampled as f32
    }
}

/// The maximum encode width for a bandwidth profile — the resource-target
/// half of the "standard up to 2 Mbps / low up to 0.5 Mbps" setting.
/// Height follows proportionally in [`downscale_for_profile`].
const fn max_width_for(profile: BandwidthProfile) -> u32 {
    match profile {
        BandwidthProfile::Standard => 1280,
        BandwidthProfile::Low => 640,
    }
}

/// Downsamples `frame` (nearest-neighbor — cheap, and encoder-visible
/// quality loss from resampling is negligible next to what H.264
/// compression itself discards) so its width never exceeds the profile's
/// cap. Returns `frame` unchanged (no copy) if it's already within budget.
pub fn downscale_for_profile(frame: &BgraFrame, profile: BandwidthProfile) -> BgraFrame {
    let max_width = max_width_for(profile);
    if frame.width <= 0 || frame.height <= 0 || frame.width as u32 <= max_width {
        return BgraFrame { width: frame.width, height: frame.height, data: frame.data.clone() };
    }

    let scale = max_width as f64 / frame.width as f64;
    // H.264 (4:2:0 chroma subsampling) needs even dimensions.
    let new_width = ((frame.width as f64 * scale) as u32 & !1).max(2);
    let new_height = (((frame.height as f64 * scale) as u32) & !1).max(2);

    let mut data = vec![0u8; (new_width * new_height * 4) as usize];
    for y in 0..new_height {
        let src_y = ((y as f64 / scale) as u32).min(frame.height as u32 - 1);
        for x in 0..new_width {
            let src_x = ((x as f64 / scale) as u32).min(frame.width as u32 - 1);
            let src_idx = ((src_y * frame.width as u32 + src_x) * 4) as usize;
            let dst_idx = ((y * new_width + x) * 4) as usize;
            data[dst_idx..dst_idx + 4].copy_from_slice(&frame.data[src_idx..src_idx + 4]);
        }
    }

    BgraFrame { width: new_width as i32, height: new_height as i32, data }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid_frame(width: i32, height: i32, color: [u8; 4]) -> BgraFrame {
        let mut data = vec![0u8; (width * height * 4) as usize];
        for chunk in data.chunks_mut(4) {
            chunk.copy_from_slice(&color);
        }
        BgraFrame { width, height, data }
    }

    #[test]
    fn identical_frames_have_zero_changed_fraction() {
        let frame = solid_frame(64, 64, [10, 20, 30, 255]);
        assert_eq!(changed_fraction(Some(&frame), &frame), 0.0);
    }

    #[test]
    fn completely_different_frames_have_full_changed_fraction() {
        let a = solid_frame(64, 64, [0, 0, 0, 255]);
        let b = solid_frame(64, 64, [255, 255, 255, 255]);
        assert_eq!(changed_fraction(Some(&a), &b), 1.0);
    }

    #[test]
    fn no_previous_frame_counts_as_fully_changed() {
        let frame = solid_frame(64, 64, [1, 2, 3, 255]);
        assert_eq!(changed_fraction(None, &frame), 1.0);
    }

    #[test]
    fn a_resolution_change_counts_as_fully_changed() {
        let a = solid_frame(64, 64, [1, 2, 3, 255]);
        let b = solid_frame(32, 32, [1, 2, 3, 255]);
        assert_eq!(changed_fraction(Some(&a), &b), 1.0);
    }

    #[test]
    fn small_noise_below_threshold_does_not_count_as_changed() {
        let a = solid_frame(64, 64, [100, 100, 100, 255]);
        let b = solid_frame(64, 64, [104, 100, 100, 255]); // within PIXEL_NOISE_THRESHOLD
        assert_eq!(changed_fraction(Some(&a), &b), 0.0);
    }

    #[test]
    fn a_real_change_above_threshold_counts() {
        let a = solid_frame(64, 64, [100, 100, 100, 255]);
        let b = solid_frame(64, 64, [200, 100, 100, 255]);
        assert_eq!(changed_fraction(Some(&a), &b), 1.0);
    }

    #[test]
    fn downscale_leaves_small_frames_untouched() {
        let frame = solid_frame(320, 240, [1, 2, 3, 255]);
        let scaled = downscale_for_profile(&frame, BandwidthProfile::Standard);
        assert_eq!((scaled.width, scaled.height), (320, 240));
    }

    #[test]
    fn downscale_caps_width_to_the_profile_budget() {
        let frame = solid_frame(1920, 1080, [1, 2, 3, 255]);
        let standard = downscale_for_profile(&frame, BandwidthProfile::Standard);
        assert!(standard.width as u32 <= max_width_for(BandwidthProfile::Standard));
        let low = downscale_for_profile(&frame, BandwidthProfile::Low);
        assert!(low.width as u32 <= max_width_for(BandwidthProfile::Low));
        assert!(low.width < standard.width, "Low profile must encode at a smaller resolution than Standard");
    }

    #[test]
    fn downscale_preserves_aspect_ratio_and_even_dimensions() {
        let frame = solid_frame(1920, 1080, [1, 2, 3, 255]);
        let scaled = downscale_for_profile(&frame, BandwidthProfile::Low);
        assert_eq!(scaled.width % 2, 0);
        assert_eq!(scaled.height % 2, 0);
        let original_ratio = 1920.0 / 1080.0;
        let scaled_ratio = scaled.width as f64 / scaled.height as f64;
        assert!((original_ratio - scaled_ratio).abs() < 0.05);
    }

    #[test]
    fn downscale_output_buffer_matches_its_own_dimensions() {
        let frame = solid_frame(1920, 1080, [1, 2, 3, 255]);
        let scaled = downscale_for_profile(&frame, BandwidthProfile::Low);
        assert_eq!(scaled.data.len(), (scaled.width * scaled.height * 4) as usize);
    }
}
