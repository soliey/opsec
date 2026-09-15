// Host/Helper consent handshake UI, screen capture exclusion, and host
// settings. `main.rs` is a one-line shim that calls [`run`]; everything
// lives here so tests can reuse the same pure logic (`overlay::geometry_for`,
// the settings load/save helpers) that the running app uses.
//
// This file only ever asks the `consent` crate for decisions — it never
// sets a session to Active itself. Host and Helper windows are excluded
// from screen capture unconditionally, at creation, independent of any
// setting (see CLAUDE.md, "Capture exclusion vs. unobtrusiveness").

pub mod overlay;

use base64::Engine as _;
use consent::transport::{LoopbackLink, PeerLink};
use consent::{HandshakeMachine, LocalEvent, PeerMessage, Role, SessionCode, SessionState};
use input::{InputBackend, InputError, KeyCode, MouseButton, NaturalInput, SessionGate};
use serde::Serialize;
use settings::{OverlayVisibility, Settings};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::Manager;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};
use transport::bitrate::AdaptiveBitrateController;
use transport::encoder::{DecodedFrame, SoftwareH264Decoder, SoftwareH264Encoder, VideoEncoder};
use transport::frame_prep::{changed_fraction, downscale_for_profile};
use transport::loopback::{LoopbackTransport, HELPER_TO_HOST_KEY_INFO, HOST_TO_HELPER_KEY_INFO};
use transport::still_screen::{SendDecision, StillScreenPacer};

struct Session {
    host: HandshakeMachine,
    helper: HandshakeMachine,
    host_link: LoopbackLink,
    helper_link: LoopbackLink,
}

impl Session {
    fn new() -> Self {
        let (host_link, helper_link) = LoopbackLink::pair();
        Self {
            host: HandshakeMachine::new(Role::Host),
            helper: HandshakeMachine::new(Role::Helper),
            host_link,
            helper_link,
        }
    }

    /// Applies any peer messages queued for the host.
    fn pump_host(&mut self) {
        while let Some(msg) = self.host_link.try_recv() {
            let _ = self.host.apply_peer(msg);
        }
    }

    /// Applies any peer messages queued for the helper.
    fn pump_helper(&mut self) {
        while let Some(msg) = self.helper_link.try_recv() {
            let _ = self.helper.apply_peer(msg);
        }
    }

    /// Ends both sides instantly, as the global hotkey does. Safe to call
    /// even if one or both sides already ended.
    fn hotkey_end_both(&mut self) {
        let _ = self.host.apply_local(LocalEvent::HotkeyEnd);
        self.host_link.send(PeerMessage::HotkeyEnd);
        let _ = self.helper.apply_local(LocalEvent::HotkeyEnd);
        self.helper_link.send(PeerMessage::HotkeyEnd);
        self.pump_host();
        self.pump_helper();
    }
}

/// Live gate for [`NaturalInput`]: reads the *current* state of the host's
/// side of the handshake on every check, never a cached snapshot — see
/// `input::natural_input`'s module docs for why that matters. Injection
/// always executes on the host machine, so it's gated on `session.host`,
/// regardless of which side (host's own hotkey, or the helper's, relayed
/// as a `PeerMessage`) caused the session to end.
struct HostGate(Arc<Mutex<Session>>);

impl SessionGate for HostGate {
    fn is_active(&self) -> bool {
        self.0.lock().map(|s| s.host.is_active()).unwrap_or(false)
    }
}

#[cfg(windows)]
fn make_backend() -> Box<dyn InputBackend + Send> {
    Box::new(input::backend::windows::SendInputBackend::new())
}

#[cfg(target_os = "macos")]
fn make_backend() -> Box<dyn InputBackend + Send> {
    Box::new(input::backend::macos::CgEventBackend::new(0.0, 0.0))
}

#[cfg(not(any(windows, target_os = "macos")))]
fn make_backend() -> Box<dyn InputBackend + Send> {
    Box::new(input::backend::NullBackend)
}

struct AppState {
    session: Arc<Mutex<Session>>,
    settings: Arc<Mutex<Settings>>,
    settings_path: PathBuf,
    /// The one instance through which every helper-initiated input event
    /// flows, on this (host) machine. `Mutex`-guarded because Tauri
    /// commands run on whatever thread the frontend's call lands on, but
    /// there is only ever one `NaturalInput` for the whole app — matching
    /// the hotkey's "stop *all* input" scope.
    input: Mutex<NaturalInput<HostGate, Box<dyn InputBackend + Send>>>,
    /// The most recently decoded frame the helper hasn't been sent yet —
    /// written by `run_streaming_loop`, read (and consumed) by
    /// `next_stream_frame`.
    latest_stream_frame: Arc<Mutex<Option<DecodedFrame>>>,
}

fn load_settings(path: &Path) -> Settings {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_settings(path: &Path, settings: &Settings) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(settings) {
        let _ = std::fs::write(path, json);
    }
}

/// Moves/resizes/shows/hides the host window to match `mode`, unless the
/// session isn't active — the pre-connection and post-session screens
/// always show in full, regardless of settings. This is the only place
/// overlay visibility is applied; it never touches whether the window is
/// excluded from capture (that's permanent, set once at window creation).
///
/// Also toggles whether the window can be focused at all
/// (`set_focusable`, `WS_EX_NOACTIVATE` on Windows / a nonactivating
/// `NSPanel` on macOS under Tauri's hood) — see CLAUDE.md, "Session
/// behavior": while a session is Active, our window must never steal focus
/// from whatever the host is doing (up to and including a fullscreen game),
/// in every overlay mode, not just the reduced ones. Before/after Active
/// the host needs real keyboard focus to type a code or click buttons, so
/// it's focusable there as normal.
fn apply_host_overlay(app: &tauri::AppHandle, is_active: bool, mode: OverlayVisibility) {
    let Some(window) = app.get_webview_window("host") else {
        return;
    };
    let _ = window.set_focusable(!is_active);
    let mode = if is_active { mode } else { OverlayVisibility::Full };
    match overlay::geometry_for(mode) {
        Some(rect) => {
            let _ = window.set_size(tauri::Size::Physical(tauri::PhysicalSize {
                width: rect.width as u32,
                height: rect.height as u32,
            }));
            let _ = window.set_position(tauri::Position::Physical(tauri::PhysicalPosition {
                x: rect.x,
                y: rect.y,
            }));
            let _ = window.show();
        }
        None => {
            let _ = window.hide();
        }
    }
}

fn current_overlay_mode(state: &AppState) -> OverlayVisibility {
    state.settings.lock().map(|s| s.overlay_visibility).unwrap_or_default()
}

#[derive(Serialize, Clone)]
struct StateDto {
    role: &'static str,
    phase: &'static str,
    code: Option<String>,
    end_reason: Option<String>,
}

fn to_dto(role: Role, state: &SessionState) -> StateDto {
    let role = match role {
        Role::Host => "host",
        Role::Helper => "helper",
    };
    match state {
        SessionState::AwaitingCode => StateDto {
            role,
            phase: "awaiting_code",
            code: None,
            end_reason: None,
        },
        SessionState::AwaitingLocalConfirmation { code } => StateDto {
            role,
            phase: "awaiting_local_confirmation",
            code: Some(code.to_string()),
            end_reason: None,
        },
        SessionState::AwaitingRemoteConfirmation { code } => StateDto {
            role,
            phase: "awaiting_remote_confirmation",
            code: Some(code.to_string()),
            end_reason: None,
        },
        SessionState::Active { code } => StateDto {
            role,
            phase: "active",
            code: Some(code.to_string()),
            end_reason: None,
        },
        SessionState::Ended { reason } => StateDto {
            role,
            phase: "ended",
            code: None,
            end_reason: Some(format!("{reason:?}")),
        },
    }
}

#[tauri::command]
fn disclosure_text() -> &'static str {
    consent::DISCLOSURE_TEXT
}

#[tauri::command]
fn get_state(state: tauri::State<AppState>, role: String) -> Result<StateDto, String> {
    let session = state.session.lock().map_err(|_| "session lock poisoned".to_string())?;
    match role.as_str() {
        "host" => Ok(to_dto(Role::Host, session.host.state())),
        "helper" => Ok(to_dto(Role::Helper, session.helper.state())),
        _ => Err(format!("unknown role: {role}")),
    }
}

#[tauri::command]
fn get_settings(state: tauri::State<AppState>) -> Result<Settings, String> {
    state.settings.lock().map(|s| *s).map_err(|_| "settings lock poisoned".to_string())
}

#[tauri::command]
fn update_settings(
    app: tauri::AppHandle,
    state: tauri::State<AppState>,
    new_settings: Settings,
) -> Result<(), String> {
    {
        let mut settings = state.settings.lock().map_err(|_| "settings lock poisoned".to_string())?;
        *settings = new_settings;
    }
    if let Ok(mut input) = state.input.lock() {
        input.set_feel(new_settings.input_feel);
    }
    save_settings(&state.settings_path, &new_settings);
    let is_active = state
        .session
        .lock()
        .map(|s| s.host.is_active())
        .map_err(|_| "session lock poisoned".to_string())?;
    apply_host_overlay(&app, is_active, new_settings.overlay_visibility);
    Ok(())
}

#[tauri::command]
fn host_generate_code(state: tauri::State<AppState>) -> Result<StateDto, String> {
    let mut session = state.session.lock().map_err(|_| "session lock poisoned".to_string())?;
    let code = SessionCode::generate();
    session
        .host
        .apply_local(LocalEvent::GenerateCode(code))
        .map_err(|e| e.to_string())?;
    Ok(to_dto(Role::Host, session.host.state()))
}

#[tauri::command]
fn helper_enter_code(state: tauri::State<AppState>, code: String) -> Result<StateDto, String> {
    let mut session = state.session.lock().map_err(|_| "session lock poisoned".to_string())?;
    let code = SessionCode::parse(&code).map_err(|e| e.to_string())?;
    session
        .helper
        .apply_local(LocalEvent::EnterCode(code))
        .map_err(|e| e.to_string())?;
    Ok(to_dto(Role::Helper, session.helper.state()))
}

#[tauri::command]
fn host_confirm(app: tauri::AppHandle, state: tauri::State<AppState>) -> Result<StateDto, String> {
    let (dto, is_active);
    {
        let mut session = state.session.lock().map_err(|_| "session lock poisoned".to_string())?;
        let code_to_relay = match session.host.state() {
            SessionState::AwaitingLocalConfirmation { code } => Some(code.clone()),
            _ => None,
        };
        session.host.apply_local(LocalEvent::Confirm).map_err(|e| e.to_string())?;
        if let Some(code) = code_to_relay {
            session.host_link.send(PeerMessage::Confirm { code });
        }
        session.pump_helper();
        is_active = session.host.is_active();
        dto = to_dto(Role::Host, session.host.state());
    }
    apply_host_overlay(&app, is_active, current_overlay_mode(&state));
    Ok(dto)
}

#[tauri::command]
fn helper_confirm(app: tauri::AppHandle, state: tauri::State<AppState>) -> Result<StateDto, String> {
    let (dto, host_is_active);
    {
        let mut session = state.session.lock().map_err(|_| "session lock poisoned".to_string())?;
        let code_to_relay = match session.helper.state() {
            SessionState::AwaitingLocalConfirmation { code } => Some(code.clone()),
            _ => None,
        };
        session.helper.apply_local(LocalEvent::Confirm).map_err(|e| e.to_string())?;
        if let Some(code) = code_to_relay {
            session.helper_link.send(PeerMessage::Confirm { code });
        }
        session.pump_host();
        host_is_active = session.host.is_active();
        dto = to_dto(Role::Helper, session.helper.state());
    }
    apply_host_overlay(&app, host_is_active, current_overlay_mode(&state));
    Ok(dto)
}

#[tauri::command]
fn host_cancel(app: tauri::AppHandle, state: tauri::State<AppState>) -> Result<StateDto, String> {
    let dto;
    {
        let mut session = state.session.lock().map_err(|_| "session lock poisoned".to_string())?;
        session.host.apply_local(LocalEvent::Cancel).map_err(|e| e.to_string())?;
        session.host_link.send(PeerMessage::Cancel);
        session.pump_helper();
        dto = to_dto(Role::Host, session.host.state());
    }
    apply_host_overlay(&app, false, current_overlay_mode(&state));
    Ok(dto)
}

#[tauri::command]
fn helper_cancel(app: tauri::AppHandle, state: tauri::State<AppState>) -> Result<StateDto, String> {
    let dto;
    {
        let mut session = state.session.lock().map_err(|_| "session lock poisoned".to_string())?;
        session.helper.apply_local(LocalEvent::Cancel).map_err(|e| e.to_string())?;
        session.helper_link.send(PeerMessage::Cancel);
        session.pump_host();
        dto = to_dto(Role::Helper, session.helper.state());
    }
    apply_host_overlay(&app, false, current_overlay_mode(&state));
    Ok(dto)
}

/// Either window's in-app "End Session" button — behaves like the global
/// hotkey but scoped to one side's local state, exactly as a real remote
/// hotkey press on that one physical machine would.
#[tauri::command]
fn end_session(app: tauri::AppHandle, state: tauri::State<AppState>, role: String) -> Result<StateDto, String> {
    let dto = {
        let mut session = state.session.lock().map_err(|_| "session lock poisoned".to_string())?;
        match role.as_str() {
            "host" => {
                let _ = session.host.apply_local(LocalEvent::HotkeyEnd);
                session.host_link.send(PeerMessage::HotkeyEnd);
                session.pump_helper();
                to_dto(Role::Host, session.host.state())
            }
            "helper" => {
                let _ = session.helper.apply_local(LocalEvent::HotkeyEnd);
                session.helper_link.send(PeerMessage::HotkeyEnd);
                session.pump_host();
                to_dto(Role::Helper, session.helper.state())
            }
            _ => return Err(format!("unknown role: {role}")),
        }
    };
    // The end-session control (like the global hotkey) always fully ends
    // the session, so the host is never left active here.
    if let Ok(mut input) = state.input.lock() {
        input.release_all_held();
    }
    apply_host_overlay(&app, false, current_overlay_mode(&state));
    Ok(dto)
}

#[tauri::command]
fn reset_session(app: tauri::AppHandle, state: tauri::State<AppState>) -> Result<(), String> {
    {
        let mut session = state.session.lock().map_err(|_| "session lock poisoned".to_string())?;
        *session = Session::new();
    }
    if let Ok(mut input) = state.input.lock() {
        input.release_all_held();
    }
    apply_host_overlay(&app, false, current_overlay_mode(&state));
    Ok(())
}

fn mouse_button_from_str(button: &str) -> Result<MouseButton, String> {
    match button {
        "left" => Ok(MouseButton::Left),
        "right" => Ok(MouseButton::Right),
        "middle" => Ok(MouseButton::Middle),
        other => Err(format!("unknown mouse button: {other}")),
    }
}

/// Moves the host's cursor to `(x, y)` along a human-paced path (per the
/// current `input_feel` setting), sent by the helper's control surface.
/// Hard-gated on live host session state inside `NaturalInput` — see
/// `HostGate` and `input::natural_input`. A refusal (session not active)
/// is not surfaced as a command error: it's the expected outcome once a
/// session has ended, and the frontend's own state polling already reflects
/// that.
#[tauri::command]
fn helper_move_mouse(state: tauri::State<AppState>, x: i32, y: i32) -> Result<(), String> {
    let mut input = state.input.lock().map_err(|_| "input lock poisoned".to_string())?;
    match input.move_mouse_to((x, y)) {
        Ok(()) | Err(InputError::SessionNotActive) => Ok(()),
    }
}

/// Moves to `(x, y)` and clicks `button` there, with human-paced timing.
#[tauri::command]
fn helper_click(state: tauri::State<AppState>, button: String, x: i32, y: i32) -> Result<(), String> {
    let button = mouse_button_from_str(&button)?;
    let mut input = state.input.lock().map_err(|_| "input lock poisoned".to_string())?;
    match input.click(button, (x, y)) {
        Ok(()) | Err(InputError::SessionNotActive) => Ok(()),
    }
}

/// A single human-paced key tap (down, brief hold, up). `code` is a raw
/// platform virtual-key code (Windows) — see `input::backend::KeyCode`.
#[tauri::command]
fn helper_key_press(state: tauri::State<AppState>, code: u16) -> Result<(), String> {
    let mut input = state.input.lock().map_err(|_| "input lock poisoned".to_string())?;
    match input.key_press(KeyCode(code)) {
        Ok(()) | Err(InputError::SessionNotActive) => Ok(()),
    }
}

#[derive(Serialize, Clone)]
struct StreamFrameDto {
    width: usize,
    height: usize,
    /// Standard base64, tightly-packed RGBA8 — one `putImageData` away
    /// from the helper's `<canvas>`.
    rgba_base64: String,
}

/// Pops the latest decoded frame, if a new one has arrived since the last
/// call. `None` is the normal, frequent case between production ticks —
/// not an error the frontend needs to react to beyond "nothing new yet".
#[tauri::command]
fn next_stream_frame(state: tauri::State<AppState>) -> Option<StreamFrameDto> {
    let frame = state.latest_stream_frame.lock().ok()?.take()?;
    Some(StreamFrameDto {
        width: frame.width,
        height: frame.height,
        rgba_base64: base64::engine::general_purpose::STANDARD.encode(&frame.rgba),
    })
}

/// Capture → still-screen pacing → encode → encrypt (loopback, standing in
/// for the real P2P transport — see `transport`'s crate docs) → decrypt →
/// decode → `latest_stream_frame`, for as long as the app runs.
///
/// A no-op whenever the host isn't `Active`: the outer loop re-checks
/// `session.host.is_active()` before doing anything, and the inner loop
/// re-checks it every iteration too — the same "gate checked before every
/// step, never a cached snapshot" discipline `input::NaturalInput` uses for
/// injection, applied here to capture/encode/send. Capture always goes
/// through `capture::capture_one_frame`, so the phase 2 exclusion
/// (`exclude_window_from_capture`, applied once at window creation) covers
/// this path automatically — there's no second capture entry point to keep
/// in sync.
///
/// Runs on a dedicated OS thread rather than Tauri's async runtime: this
/// app has no other async work, and a plain loop with `thread::sleep`
/// keeps the resource-target story simple to audit (every sleep in this
/// function is a deliberate cap on how often it captures/encodes — see the
/// resource-target comments below).
fn run_streaming_loop(session: Arc<Mutex<Session>>, settings: Arc<Mutex<Settings>>, latest_frame: Arc<Mutex<Option<DecodedFrame>>>) {
    /// Outer cadence cap even while actively streaming: `capture_one_frame`
    /// spins up and tears down a full capture session per call (phase 2's
    /// API is single-shot, not a persistent stream), so hammering it as
    /// fast as possible would itself be the "noticeable CPU/GPU load" the
    /// resource target warns against. 3-4Hz is enough for a
    /// mostly-static-content assistance session; `StillScreenPacer` pushes
    /// well below this during genuinely still periods.
    const CAPTURE_INTERVAL: Duration = Duration::from_millis(280);
    const IDLE_POLL_INTERVAL: Duration = Duration::from_millis(200);

    let is_active = || session.lock().map(|s| s.host.is_active()).unwrap_or(false);
    let current_profile = || settings.lock().map(|s| s.bandwidth_profile).unwrap_or_default();

    loop {
        if !is_active() {
            std::thread::sleep(IDLE_POLL_INTERVAL);
            continue;
        }

        let Some((key_host_to_helper, key_helper_to_host)) = session.lock().ok().and_then(|s| {
            Some((s.host.session_key(HOST_TO_HELPER_KEY_INFO)?, s.host.session_key(HELPER_TO_HOST_KEY_INFO)?))
        }) else {
            std::thread::sleep(IDLE_POLL_INTERVAL);
            continue;
        };
        let (mut sender, receiver) = LoopbackTransport::pair(&key_host_to_helper, &key_helper_to_host);

        let profile = current_profile();
        let mut bitrate = AdaptiveBitrateController::new(profile);
        let (Ok(mut encoder), Ok(mut decoder)) =
            (SoftwareH264Encoder::new(bitrate.target_bps(), 30.0), SoftwareH264Decoder::new())
        else {
            std::thread::sleep(IDLE_POLL_INTERVAL);
            continue;
        };
        let mut pacer = StillScreenPacer::new();
        let mut prev_frame: Option<capture::BgraFrame> = None;

        while is_active() {
            let profile = current_profile();
            bitrate.set_profile(profile);
            encoder.set_target_bitrate(bitrate.target_bps());

            let Ok(frame) = capture::capture_one_frame() else {
                std::thread::sleep(CAPTURE_INTERVAL);
                continue;
            };
            let scaled = downscale_for_profile(&frame, profile);
            let changed = changed_fraction(prev_frame.as_ref(), &scaled);

            let decision = pacer.decide(changed, std::time::Instant::now());
            prev_frame = Some(scaled);
            let SendDecision::Send { .. } = decision else {
                let SendDecision::Skip { retry_after } = decision else { unreachable!() };
                std::thread::sleep(retry_after.min(CAPTURE_INTERVAL * 4));
                continue;
            };

            if let Ok(encoded) = encoder.encode(prev_frame.as_ref().expect("just set"), false) {
                sender.send(&encoded);
            }

            if let Ok(Some(bytes)) = receiver.try_recv() {
                if let Ok(Some(decoded)) = decoder.decode(&bytes) {
                    if let Ok(mut slot) = latest_frame.lock() {
                        *slot = Some(decoded);
                    }
                }
            }

            std::thread::sleep(CAPTURE_INTERVAL);
        }

        // Session ended: don't leave a stale frame around for whatever
        // session connects next.
        if let Ok(mut slot) = latest_frame.lock() {
            *slot = None;
        }
    }
}

const HOTKEY: &str = "CommandOrControl+Alt+End";

/// Excludes `label`'s window from any screen capture, unconditionally, for
/// as long as it exists. See CLAUDE.md, "Capture exclusion vs.
/// unobtrusiveness": this never depends on a setting.
#[cfg(windows)]
fn exclude_window_from_capture(app: &tauri::AppHandle, label: &str) {
    if let Some(window) = app.get_webview_window(label) {
        if let Ok(hwnd) = window.hwnd() {
            let _ = capture::exclusion::windows::exclude_from_capture(hwnd);
        }
    }
}

#[cfg(not(windows))]
fn exclude_window_from_capture(_app: &tauri::AppHandle, _label: &str) {
    // macOS exclusion is a property of a capture session (SCContentFilter),
    // not of the window itself, so there is nothing to apply at window
    // creation time — see capture::exclusion::macos and CLAUDE.md.
}

pub fn run() {
    tauri::Builder::default()
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    if event.state() == ShortcutState::Pressed {
                        let state = app.state::<AppState>();
                        {
                            let lock_result = state.session.lock();
                            if let Ok(mut session) = lock_result {
                                session.hotkey_end_both();
                            }
                        }
                        // Belt-and-braces: `NaturalInput` already refuses
                        // any further event the instant `HostGate` reports
                        // inactive (true the moment `hotkey_end_both`
                        // returns, above), but force-release whatever it
                        // may have been mid-way through holding down too,
                        // so a key/button never reads as stuck on the host.
                        if let Ok(mut input) = state.input.lock() {
                            input.release_all_held();
                        }
                        apply_host_overlay(app, false, current_overlay_mode(&state));
                    }
                })
                .build(),
        )
        .setup(|app| {
            // A single global hotkey ends whichever side(s) of this demo
            // are mid-handshake or active, in every overlay visibility mode
            // (it's a global OS hotkey, independent of window state).
            app.global_shortcut().register(HOTKEY)?;

            let settings_path = app.path().app_config_dir()?.join("settings.json");
            let initial_settings = load_settings(&settings_path);

            exclude_window_from_capture(app.handle(), "host");
            exclude_window_from_capture(app.handle(), "helper");

            let session = Arc::new(Mutex::new(Session::new()));
            let gate = HostGate(session.clone());
            let natural_input = NaturalInput::new(gate, make_backend(), initial_settings.input_feel, (0, 0));
            let settings = Arc::new(Mutex::new(initial_settings));
            let latest_stream_frame = Arc::new(Mutex::new(None));

            {
                let session = session.clone();
                let settings = settings.clone();
                let latest_stream_frame = latest_stream_frame.clone();
                std::thread::spawn(move || run_streaming_loop(session, settings, latest_stream_frame));
            }

            app.manage(AppState {
                session,
                settings,
                settings_path,
                input: Mutex::new(natural_input),
                latest_stream_frame,
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            disclosure_text,
            get_state,
            get_settings,
            update_settings,
            host_generate_code,
            helper_enter_code,
            host_confirm,
            helper_confirm,
            host_cancel,
            helper_cancel,
            end_session,
            reset_session,
            helper_move_mouse,
            helper_click,
            helper_key_press,
            next_stream_frame,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
