//! Excluding our own windows from screen capture.
//!
//! Windows and macOS solve this at different layers — there is no single
//! cross-platform function, so each platform gets its own module.

/// `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)` makes a window
/// invisible to *any* capture (ours or anyone else's — DXGI Desktop
/// Duplication, GDI BitBlt, screenshot tools) for as long as the window
/// exists. This is the strongest, capturer-agnostic guarantee available on
/// Windows, which is why phase 2 doesn't rely on scap's `excluded_targets`
/// option here: scap's Windows capture engine doesn't look at that option
/// at all (its DXGI engine captures the whole monitor) — verified against
/// its source at `src/capturer/engine/win/mod.rs`.
#[cfg(windows)]
pub mod windows {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{SetWindowDisplayAffinity, WDA_EXCLUDEFROMCAPTURE, WDA_NONE};

    #[derive(Debug)]
    pub struct AffinityError(pub windows::core::Error);

    impl std::fmt::Display for AffinityError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "SetWindowDisplayAffinity failed: {}", self.0)
        }
    }

    impl std::error::Error for AffinityError {}

    /// Excludes `hwnd` from every capture of the screen from this point on.
    pub fn exclude_from_capture(hwnd: HWND) -> Result<(), AffinityError> {
        unsafe { SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE) }.map_err(AffinityError)
    }

    /// Reverses [`exclude_from_capture`]. Phase 2 never calls this in the
    /// running app (our windows stay excluded for their whole lifetime) —
    /// it exists so the exclusion test can create a *non*-excluded control
    /// window and prove the capture pipeline actually sees normal windows.
    pub fn include_in_capture(hwnd: HWND) -> Result<(), AffinityError> {
        unsafe { SetWindowDisplayAffinity(hwnd, WDA_NONE) }.map_err(AffinityError)
    }
}

/// On macOS there is no per-window "invisible to all capture" flag reachable
/// without AppKit (`NSWindow.sharingType`). Instead, scap's macOS engine
/// builds an `SCContentFilter` that excludes specific `SCWindow`s from one
/// capture session at a time (`Options.excluded_targets`). So exclusion here
/// is a property of *the capture*, applied by whoever starts one, not of the
/// window itself — it protects the helper's view but not an unrelated
/// screen recorder the way the Windows flag does.
///
/// Unverified on real hardware: there is no macOS machine in this dev loop.
/// Written against scap's `main` branch source
/// (`src/capturer/engine/mac/mod.rs`), which does honor `excluded_targets`.
#[cfg(target_os = "macos")]
pub mod macos {
    use scap::Target;

    /// Finds the `scap::Target` for one of our own windows by exact title,
    /// to pass into `capturer::Options.excluded_targets`.
    pub fn target_for_window_title(title: &str) -> Option<Target> {
        scap::get_all_targets().into_iter().find(|t| match t {
            Target::Window(w) => w.title == title,
            Target::Display(_) => false,
        })
    }
}
