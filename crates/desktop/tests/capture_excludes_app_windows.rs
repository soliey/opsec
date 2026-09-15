//! Proves that a window excluded from capture (`SetWindowDisplayAffinity
//! (WDA_EXCLUDEFROMCAPTURE)`) really is absent from a real screen capture —
//! taken the same way the future streaming pipeline will (`capture::
//! capture_one_frame`) — in every overlay visibility mode the app supports.
//!
//! Method: paint two throwaway windows a distinctive, essentially-impossible
//! background color each — one excluded, one not — at the app's real Full
//! and MinimalIndicator geometry (`remote_assist_lib::overlay::geometry_for`,
//! the same function `apply_host_overlay` uses), plus a third, hidden
//! excluded window standing in for Presenter mode. Capture the primary
//! display and scan pixels for each marker color.
//!
//! The non-excluded control window is the sanity check: if its marker isn't
//! found either, the capture pipeline itself is broken (wrong display, no
//! permission, composited from a stale buffer, ...) and an absent excluded
//! marker would prove nothing.
//!
//! Requires a real, unlocked, interactive display with GPU access — skips
//! itself (rather than failing) when `capture::capture_one_frame` reports
//! capture isn't available, e.g. over some remote sessions or CI runners.

#![cfg(windows)]

use capture::exclusion::windows::exclude_from_capture;
use capture::CaptureError;
use remote_assist_lib::overlay;
use settings::OverlayVisibility;
use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{CreateSolidBrush, UpdateWindow};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassW, ShowWindow, UnregisterClassW,
    SW_HIDE, SW_SHOW, WNDCLASSW, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

const fn rgb(r: u8, g: u8, b: u8) -> u32 {
    (r as u32) | ((g as u32) << 8) | ((b as u32) << 16)
}

// Deliberately unlikely colors for ordinary desktop content to collide with.
const EXCLUDED_MARKER: u32 = rgb(17, 222, 47);
const CONTROL_MARKER: u32 = rgb(241, 3, 214);

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

unsafe fn register_marker_class(name: windows::core::PCWSTR, color: u32, hinstance: HINSTANCE) {
    let brush = unsafe { CreateSolidBrush(COLORREF(color)) };
    let wc = WNDCLASSW {
        lpfnWndProc: Some(wndproc),
        hInstance: hinstance,
        lpszClassName: name,
        hbrBackground: brush,
        ..Default::default()
    };
    unsafe {
        RegisterClassW(&wc);
    }
}

unsafe fn create_marker_window(
    class_name: windows::core::PCWSTR,
    hinstance: HINSTANCE,
    rect: overlay::Rect,
    visible: bool,
) -> HWND {
    let hwnd = unsafe {
        CreateWindowExW(
            // Topmost so ambient desktop windows (browser, editor, system
            // Settings, ...) can't cover our test windows and produce a
            // false "marker not found" — this test needs deterministic
            // z-order, not real app behavior.
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            class_name,
            w!(""),
            WS_POPUP,
            rect.x,
            rect.y,
            rect.width,
            rect.height,
            None,
            None,
            Some(hinstance),
            None,
        )
        .expect("CreateWindowExW failed")
    };
    unsafe {
        let _ = ShowWindow(hwnd, if visible { SW_SHOW } else { SW_HIDE });
        let _ = UpdateWindow(hwnd);
    }
    hwnd
}

fn pixel_matches(pixel: &[u8], colorref: u32) -> bool {
    let r = (colorref & 0xFF) as u8;
    let g = ((colorref >> 8) & 0xFF) as u8;
    let b = ((colorref >> 16) & 0xFF) as u8;
    // BGRA8888, as scap::frame::BGRAFrame lays it out.
    pixel[0] == b && pixel[1] == g && pixel[2] == r
}

#[test]
fn excluded_windows_never_appear_in_capture_in_any_overlay_mode() {
    let full = overlay::geometry_for(OverlayVisibility::Full).expect("Full has geometry");
    let minimal = overlay::geometry_for(OverlayVisibility::MinimalIndicator).expect("MinimalIndicator has geometry");
    // Presenter has no geometry (window is hidden), so any small rect works.
    let presenter_rect = overlay::Rect {
        x: 0,
        y: 0,
        width: 300,
        height: 100,
    };

    let (control, full_win, minimal_win, presenter_win, hinstance);
    unsafe {
        hinstance = HINSTANCE(GetModuleHandleW(None).expect("GetModuleHandleW").0);
        register_marker_class(w!("RemoteAssistTestExcluded"), EXCLUDED_MARKER, hinstance);
        register_marker_class(w!("RemoteAssistTestControl"), CONTROL_MARKER, hinstance);

        // Not excluded: proves the capture pipeline sees ordinary windows.
        control = create_marker_window(
            w!("RemoteAssistTestControl"),
            hinstance,
            overlay::Rect {
                x: 700,
                y: 700,
                width: 220,
                height: 160,
            },
            true,
        );

        full_win = create_marker_window(w!("RemoteAssistTestExcluded"), hinstance, full, true);
        exclude_from_capture(full_win).expect("exclude full-mode window");

        minimal_win = create_marker_window(w!("RemoteAssistTestExcluded"), hinstance, minimal, true);
        exclude_from_capture(minimal_win).expect("exclude minimal-indicator window");

        presenter_win = create_marker_window(w!("RemoteAssistTestExcluded"), hinstance, presenter_rect, false);
        exclude_from_capture(presenter_win).expect("exclude presenter-mode window");

        std::thread::sleep(std::time::Duration::from_millis(500));
    }

    let capture_result = capture::capture_one_frame();

    unsafe {
        let _ = DestroyWindow(control);
        let _ = DestroyWindow(full_win);
        let _ = DestroyWindow(minimal_win);
        let _ = DestroyWindow(presenter_win);
        let _ = UnregisterClassW(w!("RemoteAssistTestExcluded"), Some(hinstance));
        let _ = UnregisterClassW(w!("RemoteAssistTestControl"), Some(hinstance));
    }

    let frame = match capture_result {
        Ok(frame) => frame,
        Err(CaptureError::Unsupported) | Err(CaptureError::NoPermission) => {
            eprintln!("skipping: screen capture is not available in this environment");
            return;
        }
        Err(e) => panic!("capture_one_frame failed: {e}"),
    };

    let mut has_excluded_marker = false;
    let mut has_control_marker = false;
    for pixel in frame.data.as_chunks::<4>().0 {
        if pixel_matches(pixel, EXCLUDED_MARKER) {
            has_excluded_marker = true;
        }
        if pixel_matches(pixel, CONTROL_MARKER) {
            has_control_marker = true;
        }
    }

    assert!(
        has_control_marker,
        "sanity check failed: the non-excluded control window's marker color was not found anywhere \
         in the captured frame, so the capture pipeline itself may be broken (wrong display, no \
         permission, stale buffer) — an absent excluded marker would prove nothing in that case"
    );
    assert!(
        !has_excluded_marker,
        "an excluded window's marker color was found in the captured frame — \
         SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE) did not keep it out of the capture \
         in at least one overlay mode (Full, MinimalIndicator, or Presenter)"
    );
}
