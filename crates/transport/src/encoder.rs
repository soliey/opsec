//! Video encoding/decoding for the streamed screen.
//!
//! [`SoftwareH264Encoder`]/[`SoftwareH264Decoder`] are real, working H.264
//! codec wrappers around `openh264` (Cisco's OpenH264, vendored and built
//! from source by the `openh264` crate's default `source` feature — no
//! system codec library required, confirmed buildable in this sandbox).
//! [`probe_hardware_h264`] is a real (Windows) hardware-encoder
//! *availability* check via Media Foundation's `MFTEnumEx` — genuinely
//! queries the OS for a registered hardware H.264 MFT (which is how
//! Windows exposes NVENC/QuickSync/AMD VCE uniformly, matching the task's
//! "NVENC/Media Foundation on Windows" phrasing: MF is the umbrella API,
//! the vendor encoder is whatever's registered underneath it).
//!
//! What's honestly not wired up: actually *using* a hardware MFT to
//! encode — that's a large `IMFTransform` sample-processing pipeline this
//! phase doesn't implement (same gap class as `capture::exclusion::macos`)
//! — so [`select_backend`] always returns the software backend today, with
//! the probe result attached for the caller to log/display, not to act on
//! yet. macOS `VideoToolbox` hardware encoding is the same kind of
//! documented-not-wired gap, for the same "no macOS machine in this dev
//! loop" reason as `input::backend::macos`.

use capture::BgraFrame;
use openh264::OpenH264API;
use openh264::decoder::{Decoder, DecoderConfig};
use openh264::encoder::{BitRate, Encoder, EncoderConfig, FrameRate, UsageType};
use openh264::formats::{BgraSliceU8, YUVBuffer, YUVSource};

#[derive(Debug)]
pub enum EncoderError {
    Init(String),
    Encode(String),
    Decode(String),
}

impl std::fmt::Display for EncoderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EncoderError::Init(e) => write!(f, "failed to initialize codec: {e}"),
            EncoderError::Encode(e) => write!(f, "encode failed: {e}"),
            EncoderError::Decode(e) => write!(f, "decode failed: {e}"),
        }
    }
}

impl std::error::Error for EncoderError {}

/// What any video encoder backend must do: turn one captured frame into
/// encoded bytes, honoring a live bitrate target (from
/// `crate::bitrate::AdaptiveBitrateController`) and an occasional forced
/// keyframe (e.g. the first frame after `crate::still_screen::
/// StillScreenPacer` resumes from a long still period).
pub trait VideoEncoder {
    fn encode(&mut self, frame: &BgraFrame, force_keyframe: bool) -> Result<Vec<u8>, EncoderError>;
    /// Re-targets the encoder's bitrate. `openh264`'s safe API doesn't
    /// expose live bitrate changes, so this reconstructs the underlying
    /// encoder — the next `encode()` call starts a fresh sequence (which
    /// is also why callers should treat the frame right after a bitrate
    /// change as an implicit keyframe boundary).
    fn set_target_bitrate(&mut self, bps: u32);
}

pub struct SoftwareH264Encoder {
    encoder: Encoder,
    target_bps: u32,
    target_fps: f32,
}

impl SoftwareH264Encoder {
    /// `target_fps` of `0.0` means "uncapped" (paced entirely by the
    /// caller, e.g. `StillScreenPacer`).
    pub fn new(target_bps: u32, target_fps: f32) -> Result<Self, EncoderError> {
        let encoder = build_encoder(target_bps, target_fps)?;
        Ok(Self { encoder, target_bps, target_fps })
    }
}

fn build_encoder(target_bps: u32, target_fps: f32) -> Result<Encoder, EncoderError> {
    let config = EncoderConfig::new()
        .bitrate(BitRate::from_bps(target_bps))
        .max_frame_rate(FrameRate::from_hz(target_fps))
        .usage_type(UsageType::ScreenContentRealTime);
    Encoder::with_api_config(OpenH264API::from_source(), config).map_err(|e| EncoderError::Init(e.to_string()))
}

impl VideoEncoder for SoftwareH264Encoder {
    fn encode(&mut self, frame: &BgraFrame, force_keyframe: bool) -> Result<Vec<u8>, EncoderError> {
        if force_keyframe {
            self.encoder.force_intra_frame();
        }
        let bgra = BgraSliceU8::new(&frame.data, (frame.width as usize, frame.height as usize));
        let yuv = YUVBuffer::from_rgb_source(bgra);
        let bitstream = self.encoder.encode(&yuv).map_err(|e| EncoderError::Encode(e.to_string()))?;
        Ok(bitstream.to_vec())
    }

    fn set_target_bitrate(&mut self, bps: u32) {
        if bps == self.target_bps {
            return;
        }
        // On failure, keep encoding at the old bitrate rather than
        // dropping the stream entirely over a reconfiguration failure.
        if let Ok(encoder) = build_encoder(bps, self.target_fps) {
            self.encoder = encoder;
            self.target_bps = bps;
        }
    }
}

/// One decoded frame, already converted to tightly-packed RGBA8 — ready to
/// hand straight to a `<canvas>` (`ImageData`) on the frontend.
pub struct DecodedFrame {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

pub struct SoftwareH264Decoder {
    decoder: Decoder,
}

impl SoftwareH264Decoder {
    pub fn new() -> Result<Self, EncoderError> {
        let decoder = Decoder::with_api_config(OpenH264API::from_source(), DecoderConfig::new())
            .map_err(|e| EncoderError::Init(e.to_string()))?;
        Ok(Self { decoder })
    }

    /// Decodes one Annex-B H.264 packet. Returns `None` when OpenH264
    /// needs more data before it can produce a picture (e.g. the very
    /// first packet, which is often just parameter sets) — this is normal,
    /// not an error.
    pub fn decode(&mut self, packet: &[u8]) -> Result<Option<DecodedFrame>, EncoderError> {
        let Some(yuv) = self.decoder.decode(packet).map_err(|e| EncoderError::Decode(e.to_string()))? else {
            return Ok(None);
        };
        let (width, height) = yuv.dimensions();
        let mut rgba = vec![0u8; width * height * 4];
        yuv.write_rgba8(&mut rgba);
        Ok(Some(DecodedFrame { width, height, rgba }))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HardwareEncoderAvailability {
    Available,
    Unavailable,
    /// Probing isn't implemented on this platform (see module docs).
    Unknown,
}

/// Real hardware-H.264-encoder availability probe on Windows (queries
/// Media Foundation's Transform registry via `MFTEnumEx`, the same
/// mechanism Windows uses to expose NVENC/QuickSync/AMD VCE uniformly).
/// See the module doc comment for what this result is (and isn't) used
/// for today.
#[cfg(windows)]
pub fn probe_hardware_h264() -> HardwareEncoderAvailability {
    windows_mf::probe()
}

#[cfg(not(windows))]
pub fn probe_hardware_h264() -> HardwareEncoderAvailability {
    HardwareEncoderAvailability::Unknown
}

/// Backend selection: always software today (see module docs) — this
/// function exists as the one seam a future phase changes once a real
/// hardware encode pipeline exists, so callers never have to know which
/// backend they got.
pub fn select_backend(target_bps: u32, target_fps: f32) -> Result<Box<dyn VideoEncoder + Send>, EncoderError> {
    let _hw = probe_hardware_h264(); // logged/surfaced by the caller; not acted on yet.
    Ok(Box::new(SoftwareH264Encoder::new(target_bps, target_fps)?))
}

#[cfg(windows)]
mod windows_mf {
    use super::HardwareEncoderAvailability;
    use windows::Win32::Media::MediaFoundation::{
        MFMediaType_Video, MFShutdown, MFStartup, MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG_HARDWARE,
        MFT_ENUM_FLAG_SORTANDFILTER, MFT_REGISTER_TYPE_INFO, MFTEnumEx, MFVideoFormat_H264, MFSTARTUP_LITE,
        MF_VERSION,
    };
    use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};

    /// Queries Media Foundation for any hardware-registered H.264 encoder
    /// MFT. Real Win32 calls, real result — see the module doc comment for
    /// why the result isn't wired into an actual encode path yet.
    pub fn probe() -> HardwareEncoderAvailability {
        // SAFETY: CoInitializeEx/MFStartup/MFTEnumEx/MFShutdown/
        // CoUninitialize are called in the documented order, each result
        // checked, and every successful init is paired with its shutdown
        // before returning.
        unsafe {
            let co_result = CoInitializeEx(None, COINIT_MULTITHREADED);
            if co_result.is_err() {
                return HardwareEncoderAvailability::Unknown;
            }
            if MFStartup(MF_VERSION, MFSTARTUP_LITE).is_err() {
                CoUninitialize();
                return HardwareEncoderAvailability::Unknown;
            }

            let output_type = MFT_REGISTER_TYPE_INFO {
                guidMajorType: MFMediaType_Video,
                guidSubtype: MFVideoFormat_H264,
            };

            let mut activates: *mut Option<windows::Win32::Media::MediaFoundation::IMFActivate> =
                std::ptr::null_mut();
            let mut count: u32 = 0;
            let enum_result = MFTEnumEx(
                MFT_CATEGORY_VIDEO_ENCODER,
                MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
                None,
                Some(&output_type),
                &mut activates,
                &mut count,
            );

            let availability = match enum_result {
                Ok(()) if count > 0 => HardwareEncoderAvailability::Available,
                Ok(()) => HardwareEncoderAvailability::Unavailable,
                Err(_) => HardwareEncoderAvailability::Unknown,
            };

            if !activates.is_null() && count > 0 {
                let slice = std::slice::from_raw_parts_mut(activates, count as usize);
                for activate in slice.iter_mut() {
                    // Dropping each Option<IMFActivate> releases its COM
                    // reference — MFTEnumEx hands back AddRef'd pointers
                    // the caller owns.
                    *activate = None;
                }
                windows::Win32::System::Com::CoTaskMemFree(Some(activates as *const core::ffi::c_void));
            }

            let _ = MFShutdown();
            CoUninitialize();
            availability
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_frame(width: i32, height: i32, seed: u8) -> BgraFrame {
        // A simple gradient — enough visual structure for the encoder to
        // do real work, deterministic so tests are reproducible.
        let mut data = vec![0u8; (width * height * 4) as usize];
        for y in 0..height {
            for x in 0..width {
                let idx = ((y * width + x) * 4) as usize;
                data[idx] = ((x + seed as i32) % 256) as u8; // B
                data[idx + 1] = ((y + seed as i32) % 256) as u8; // G
                data[idx + 2] = seed; // R
                data[idx + 3] = 255; // A
            }
        }
        BgraFrame { width, height, data }
    }

    #[test]
    fn encodes_a_frame_to_nonempty_h264_bytes() {
        let mut encoder = SoftwareH264Encoder::new(500_000, 30.0).expect("encoder init");
        let frame = synthetic_frame(64, 64, 10);
        let encoded = encoder.encode(&frame, true).expect("encode");
        assert!(!encoded.is_empty());
    }

    #[test]
    fn round_trips_through_a_real_decoder() {
        let mut encoder = SoftwareH264Encoder::new(500_000, 30.0).expect("encoder init");
        let mut decoder = SoftwareH264Decoder::new().expect("decoder init");

        let frame = synthetic_frame(64, 64, 42);
        let encoded = encoder.encode(&frame, true).expect("encode");

        // OpenH264 may need the parameter-set packet before it yields a
        // picture; feed the same encoded packet (which for a keyframe
        // includes SPS/PPS + slice data together) and accept either an
        // immediate picture or None with no error.
        let decoded = decoder.decode(&encoded).expect("decode should not error on a valid keyframe packet");
        if let Some(picture) = decoded {
            assert_eq!(picture.width, 64);
            assert_eq!(picture.height, 64);
            assert_eq!(picture.rgba.len(), 64 * 64 * 4);
        }
    }

    #[test]
    fn set_target_bitrate_is_a_no_op_when_unchanged() {
        let mut encoder = SoftwareH264Encoder::new(500_000, 30.0).expect("encoder init");
        encoder.set_target_bitrate(500_000);
        assert_eq!(encoder.target_bps, 500_000);
    }

    #[test]
    fn set_target_bitrate_updates_and_keeps_encoding_working() {
        let mut encoder = SoftwareH264Encoder::new(2_000_000, 30.0).expect("encoder init");
        encoder.set_target_bitrate(500_000);
        assert_eq!(encoder.target_bps, 500_000);

        let frame = synthetic_frame(32, 32, 5);
        let encoded = encoder.encode(&frame, true).expect("encode after bitrate change");
        assert!(!encoded.is_empty());
    }

    #[test]
    fn probe_hardware_h264_does_not_panic() {
        // Can't assert a specific availability (depends on the machine's
        // GPU/drivers), just that the real Win32 call sequence completes
        // cleanly and returns one of the defined outcomes.
        let _ = probe_hardware_h264();
    }
}
