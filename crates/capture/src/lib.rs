//! Screen capture (phase 2 groundwork) and window-capture exclusion.
//!
//! Two independent things live here:
//!
//! - [`exclusion`] keeps our own UI windows out of *any* capture of the
//!   host's screen — the same mechanism Discord's overlay and Zoom use.
//!   Per CLAUDE.md this is a UX property (don't clutter the shared view
//!   with our own chrome), never an anti-detection one: it never hides
//!   anything from the host, only from what gets captured/streamed.
//! - [`capture_one_frame`] grabs a frame from the host's screen via `scap`
//!   (DXGI Desktop Duplication on Windows, ScreenCaptureKit on macOS).
//!   Phase 2 only proves capture works and that excluded windows are
//!   actually absent from it; sending frames to a helper is a later phase.

pub mod exclusion;

use scap::capturer::{Capturer, Options};
use scap::frame::{Frame, FrameType};

#[derive(Debug)]
pub enum CaptureError {
    Unsupported,
    NoPermission,
    Build(String),
    Recv(std::sync::mpsc::RecvError),
    UnexpectedFrameFormat,
}

impl std::fmt::Display for CaptureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CaptureError::Unsupported => write!(f, "screen capture is not supported on this system"),
            CaptureError::NoPermission => write!(f, "screen capture permission was not granted"),
            CaptureError::Build(e) => write!(f, "failed to start capturer: {e}"),
            CaptureError::Recv(e) => write!(f, "failed to receive a frame: {e}"),
            CaptureError::UnexpectedFrameFormat => write!(f, "capturer returned an unexpected frame format"),
        }
    }
}

impl std::error::Error for CaptureError {}

/// One BGRA8888 frame captured from a display, rows top-to-bottom.
pub struct BgraFrame {
    pub width: i32,
    pub height: i32,
    pub data: Vec<u8>,
}

/// Captures a single frame from the primary display.
///
/// Exclusion of our own windows is *not* a parameter here: on Windows it's
/// a property of the window itself (see
/// [`exclusion::windows::exclude_from_capture`]), applied once, well before
/// any capture starts. The stable `scap` release this crate depends on
/// doesn't yet expose per-capture window exclusion (`Options.excluded_targets`
/// exists only in scap's still-unbuildable 0.1 beta — see this crate's
/// Cargo.toml) so macOS exclusion isn't wired through here yet either; see
/// `exclusion::macos` for what that will plug into once it is.
pub fn capture_one_frame() -> Result<BgraFrame, CaptureError> {
    if !scap::is_supported() {
        return Err(CaptureError::Unsupported);
    }
    if !scap::has_permission() && !scap::request_permission() {
        return Err(CaptureError::NoPermission);
    }

    let options = Options {
        fps: 5,
        target: None, // primary display
        show_cursor: false,
        show_highlight: false,
        output_type: FrameType::BGRAFrame,
        ..Default::default()
    };

    let mut capturer = Capturer::build(options).map_err(|e| CaptureError::Build(format!("{e:?}")))?;
    capturer.start_capture();
    let frame = capturer.get_next_frame().map_err(CaptureError::Recv);
    capturer.stop_capture();

    match frame? {
        Frame::BGRA(f) => Ok(BgraFrame {
            width: f.width,
            height: f.height,
            data: f.data,
        }),
        _ => Err(CaptureError::UnexpectedFrameFormat),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn is_supported_does_not_panic() {
        let _ = scap::is_supported();
    }
}
