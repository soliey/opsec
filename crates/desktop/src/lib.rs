// Host/Helper consent handshake UI, screen capture exclusion, real
// cross-machine signaling/media, and host settings. `main.rs` is a one-line
// shim that calls [`run`]; everything lives here so tests can reuse the
// same pure logic (`overlay::geometry_for`, the settings load/save
// helpers) that the running app uses.
//
// This file only ever asks the `consent` crate for decisions — it never
// sets a session to Active itself. This window is excluded from screen
// capture unconditionally, at creation, independent of any setting (see
// CLAUDE.md, "Capture exclusion vs. unobtrusiveness").
//
// One process = one machine = one role, chosen once (and persisted) on
// first run — see `choose_role`/`start_role`. Handshake signaling and
// media/input both use real, cross-machine transports:
// `signaling::SupabaseRealtimeLink` (a `consent::transport::PeerLink`) for
// the handshake, and `transport::webrtc_media` (real WebRTC data channels)
// for video and input once the session is Active.

pub mod overlay;

use base64::Engine as _;
use consent::transport::PeerLink;
use consent::{HandshakeMachine, LocalEvent, PeerMessage, Role, SessionCode, SessionState};
use input::{InputBackend, KeyCode, MouseButton, NaturalInput, SessionGate};
use serde::{Deserialize, Serialize};
use settings::{OverlayVisibility, Settings};
use signaling::SupabaseRealtimeLink;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::Manager;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};
use transport::bitrate::AdaptiveBitrateController;
use transport::encoder::{DecodedFrame, SoftwareH264Decoder, SoftwareH264Encoder, VideoEncoder};
use transport::frame_prep::{changed_fraction, downscale_for_profile};
use transport::ice::IceConfig;
use transport::loopback::MediaLink;
use transport::signaling::SIGNALING_KEY_INFO;
use transport::still_screen::{SendDecision, StillScreenPacer};
use transport::webrtc_media;

/// One machine's side of a pairing: this process's role, its handshake
/// state, and (once a code exists — see `Session::ensure_link`) the real
/// signaling link to the peer. Unlike the same-process demo this replaced,
/// there is exactly one `HandshakeMachine` here, because there is exactly
/// one role per process now.
struct Session {
    role: Role,
    handshake: HandshakeMachine,
    link: Option<Arc<SupabaseRealtimeLink>>,
}

impl Session {
    fn new(role: Role) -> Self {
        Self { role, handshake: HandshakeMachine::new(role), link: None }
    }

    /// Real signaling can't start until a code exists locally — the
    /// Supabase Realtime Broadcast topic is derived from it (see
    /// `signaling::topic::topic_for_code`) — so the link is created lazily
    /// here rather than at `Session::new`.
    fn ensure_link(&mut self, runtime: &tokio::runtime::Handle, code: &SessionCode) {
        if self.link.is_none() {
            let topic = signaling::topic::topic_for_code(code);
            self.link = Some(Arc::new(SupabaseRealtimeLink::connect(
                runtime,
                signaling::SUPABASE_URL,
                signaling::SUPABASE_PUBLISHABLE_KEY,
                &topic,
            )));
        }
    }

    /// Applies any peer messages queued on the real link.
    fn pump(&mut self) {
        if let Some(link) = &self.link {
            while let Some(msg) = link.try_recv() {
                let _ = self.handshake.apply_peer(msg);
            }
        }
    }

    fn send_peer(&self, message: PeerMessage) {
        if let Some(link) = &self.link {
            link.send(message);
        }
    }

    /// Ends this side instantly, as the global hotkey does, and notifies
    /// the peer over the real link. Safe to call even if already ended or
    /// no link exists yet.
    fn hotkey_end(&mut self) {
        let _ = self.handshake.apply_local(LocalEvent::HotkeyEnd);
        self.send_peer(PeerMessage::HotkeyEnd);
        self.pump();
    }
}

/// Live gate for [`NaturalInput`]: reads the *current* state of this
/// machine's handshake on every check, never a cached snapshot — see
/// `input::natural_input`'s module docs for why that matters. Only ever
/// constructed when this process is the Host (input injection always
/// happens on the host machine); `None` (role not chosen yet, or this
/// process is the Helper) reads as inactive.
struct HostGate(Arc<Mutex<Option<Session>>>);

impl SessionGate for HostGate {
    fn is_active(&self) -> bool {
        self.0
            .lock()
            .ok()
            .and_then(|s| s.as_ref().map(|session| session.handshake.is_active()))
            .unwrap_or(false)
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

/// A helper-side input action, wire-serialized over the `"input"` WebRTC
/// data channel to the host. DTLS-SRTP secures the channel itself; there is
/// no separate application-level encryption layer here, unlike the
/// loopback-transport media path (see `transport::webrtc_media`'s doc
/// comment).
#[derive(Serialize, Deserialize)]
enum InputEvent {
    Move { x: i32, y: i32 },
    Click { button: String, x: i32, y: i32 },
    KeyPress { code: u16 },
}

struct AppState {
    session: Arc<Mutex<Option<Session>>>,
    settings: Arc<Mutex<Settings>>,
    settings_path: PathBuf,
    /// The one instance through which every helper-initiated input event
    /// flows, on this (host) machine. `None` on a Helper process, or before
    /// a role is chosen. `Mutex`-guarded because Tauri commands run on
    /// whatever thread the frontend's call lands on.
    input: Arc<Mutex<Option<NaturalInput<HostGate, Box<dyn InputBackend + Send>>>>>,
    /// This process's end of the `"input"` data channel — outbound on a
    /// Helper (Tauri input commands push onto it), inbound on a Host
    /// (`run_host_streaming_loop` drains it into `input`, above). `None`
    /// until a session is `Active` and WebRTC negotiation has completed.
    input_link: Arc<Mutex<Option<Box<dyn MediaLink + Send>>>>,
    /// The most recently decoded frame the helper hasn't been sent yet —
    /// written by `run_helper_receive_loop`, read (and consumed) by
    /// `next_stream_frame`. Always `None` on a Host process.
    latest_stream_frame: Arc<Mutex<Option<DecodedFrame>>>,
    /// Drives `signaling::SupabaseRealtimeLink` and `transport::
    /// webrtc_media` — the only async work in this app; everything else
    /// (Tauri commands, the streaming loops) stays synchronous. See
    /// `run()`'s doc comment on why this lives on its own dedicated thread.
    runtime: tokio::runtime::Handle,
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

/// Moves/resizes/shows/hides this window to match `mode`, unless the
/// session isn't active — the pre-connection and post-session screens
/// always show in full, regardless of settings. This is the only place
/// overlay visibility is applied; it never touches whether the window is
/// excluded from capture (that's permanent, set once at window creation).
/// A no-op on a Helper process: overlay visibility is a host-comfort
/// preference about the *host's own* window, and the helper isn't the one
/// being captured/watched.
///
/// Also toggles whether the window can be focused at all
/// (`set_focusable`, `WS_EX_NOACTIVATE` on Windows / a nonactivating
/// `NSPanel` on macOS under Tauri's hood) — see CLAUDE.md, "Session
/// behavior": while a session is Active, our window must never steal focus
/// from whatever the host is doing (up to and including a fullscreen game),
/// in every overlay mode, not just the reduced ones. Before/after Active
/// the host needs real keyboard focus to type a code or click buttons, so
/// it's focusable there as normal.
fn apply_overlay(app: &tauri::AppHandle, role: Role, is_active: bool, mode: OverlayVisibility) {
    if role != Role::Host {
        return;
    }
    let Some(window) = app.get_webview_window("main") else {
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
    role: Option<&'static str>,
    phase: &'static str,
    code: Option<String>,
    end_reason: Option<String>,
}

fn awaiting_role_dto() -> StateDto {
    StateDto { role: None, phase: "awaiting_role", code: None, end_reason: None }
}

fn to_dto(role: Role, state: &SessionState) -> StateDto {
    let role = Some(match role {
        Role::Host => "host",
        Role::Helper => "helper",
    });
    match state {
        SessionState::AwaitingCode => StateDto { role, phase: "awaiting_code", code: None, end_reason: None },
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
        SessionState::Active { code } => StateDto { role, phase: "active", code: Some(code.to_string()), end_reason: None },
        SessionState::Ended { reason } => {
            StateDto { role, phase: "ended", code: None, end_reason: Some(format!("{reason:?}")) }
        }
    }
}

fn parse_role(role: &str) -> Result<Role, String> {
    match role {
        "host" => Ok(Role::Host),
        "helper" => Ok(Role::Helper),
        other => Err(format!("unknown role: {other}")),
    }
}

#[tauri::command]
fn disclosure_text() -> &'static str {
    consent::DISCLOSURE_TEXT
}

/// First-run (or every-run, if a role was never chosen) role selection.
/// Persists into `settings::Settings` and starts this process's session +
/// streaming loop; a no-op returning the current state if a role was
/// already chosen (e.g. a stray double-click on the picker).
#[tauri::command]
fn choose_role(app: tauri::AppHandle, state: tauri::State<AppState>, role: String) -> Result<StateDto, String> {
    let role = parse_role(&role)?;
    {
        let session = state.session.lock().map_err(|_| "session lock poisoned".to_string())?;
        if let Some(session) = session.as_ref() {
            return Ok(to_dto(session.role, session.handshake.state()));
        }
    }

    {
        let mut settings = state.settings.lock().map_err(|_| "settings lock poisoned".to_string())?;
        settings.role = Some(role);
        save_settings(&state.settings_path, &settings);
    }

    start_role(
        role,
        state.session.clone(),
        state.input.clone(),
        state.input_link.clone(),
        state.latest_stream_frame.clone(),
        state.settings.clone(),
        state.runtime.clone(),
    );

    apply_overlay(&app, role, false, current_overlay_mode(&state));

    let session = state.session.lock().map_err(|_| "session lock poisoned".to_string())?;
    let session = session.as_ref().expect("start_role just set this");
    Ok(to_dto(session.role, session.handshake.state()))
}

#[tauri::command]
fn get_state(state: tauri::State<AppState>) -> Result<StateDto, String> {
    let session = state.session.lock().map_err(|_| "session lock poisoned".to_string())?;
    Ok(match session.as_ref() {
        Some(session) => to_dto(session.role, session.handshake.state()),
        None => awaiting_role_dto(),
    })
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
        if let Some(input) = input.as_mut() {
            input.set_feel(new_settings.input_feel);
        }
    }
    save_settings(&state.settings_path, &new_settings);
    let (role, is_active) = {
        let session = state.session.lock().map_err(|_| "session lock poisoned".to_string())?;
        match session.as_ref() {
            Some(session) => (session.role, session.handshake.is_active()),
            None => return Ok(()),
        }
    };
    apply_overlay(&app, role, is_active, new_settings.overlay_visibility);
    Ok(())
}

#[tauri::command]
fn generate_code(state: tauri::State<AppState>) -> Result<StateDto, String> {
    let mut session = state.session.lock().map_err(|_| "session lock poisoned".to_string())?;
    let session = session.as_mut().ok_or_else(|| "no role chosen yet".to_string())?;
    let code = SessionCode::generate();
    session.handshake.apply_local(LocalEvent::GenerateCode(code.clone())).map_err(|e| e.to_string())?;
    session.ensure_link(&state.runtime, &code);
    Ok(to_dto(session.role, session.handshake.state()))
}

#[tauri::command]
fn enter_code(state: tauri::State<AppState>, code: String) -> Result<StateDto, String> {
    let mut session = state.session.lock().map_err(|_| "session lock poisoned".to_string())?;
    let session = session.as_mut().ok_or_else(|| "no role chosen yet".to_string())?;
    let code = SessionCode::parse(&code).map_err(|e| e.to_string())?;
    session.handshake.apply_local(LocalEvent::EnterCode(code.clone())).map_err(|e| e.to_string())?;
    session.ensure_link(&state.runtime, &code);
    Ok(to_dto(session.role, session.handshake.state()))
}

#[tauri::command]
fn confirm(app: tauri::AppHandle, state: tauri::State<AppState>) -> Result<StateDto, String> {
    let (dto, role, is_active);
    {
        let mut session = state.session.lock().map_err(|_| "session lock poisoned".to_string())?;
        let session = session.as_mut().ok_or_else(|| "no role chosen yet".to_string())?;
        let code_to_relay = match session.handshake.state() {
            SessionState::AwaitingLocalConfirmation { code } => Some(code.clone()),
            _ => None,
        };
        session.handshake.apply_local(LocalEvent::Confirm).map_err(|e| e.to_string())?;
        if let Some(code) = code_to_relay {
            session.send_peer(PeerMessage::Confirm { code });
        }
        session.pump();
        role = session.role;
        is_active = session.handshake.is_active();
        dto = to_dto(session.role, session.handshake.state());
    }
    apply_overlay(&app, role, is_active, current_overlay_mode(&state));
    Ok(dto)
}

#[tauri::command]
fn cancel(app: tauri::AppHandle, state: tauri::State<AppState>) -> Result<StateDto, String> {
    let (dto, role);
    {
        let mut session = state.session.lock().map_err(|_| "session lock poisoned".to_string())?;
        let session = session.as_mut().ok_or_else(|| "no role chosen yet".to_string())?;
        session.handshake.apply_local(LocalEvent::Cancel).map_err(|e| e.to_string())?;
        session.send_peer(PeerMessage::Cancel);
        session.pump();
        role = session.role;
        dto = to_dto(session.role, session.handshake.state());
    }
    apply_overlay(&app, role, false, current_overlay_mode(&state));
    Ok(dto)
}

/// The in-app "End Session" button — behaves like the global hotkey but
/// scoped to this process, exactly as a real hotkey press on this one
/// physical machine would.
#[tauri::command]
fn end_session(app: tauri::AppHandle, state: tauri::State<AppState>) -> Result<StateDto, String> {
    let (dto, role) = {
        let mut session = state.session.lock().map_err(|_| "session lock poisoned".to_string())?;
        let session = session.as_mut().ok_or_else(|| "no role chosen yet".to_string())?;
        session.hotkey_end();
        (to_dto(session.role, session.handshake.state()), session.role)
    };
    // The end-session control (like the global hotkey) always fully ends
    // the session, so the host is never left active here.
    if let Ok(mut input) = state.input.lock() {
        if let Some(input) = input.as_mut() {
            input.release_all_held();
        }
    }
    apply_overlay(&app, role, false, current_overlay_mode(&state));
    Ok(dto)
}

#[tauri::command]
fn reset_session(app: tauri::AppHandle, state: tauri::State<AppState>) -> Result<(), String> {
    let role = {
        let mut session = state.session.lock().map_err(|_| "session lock poisoned".to_string())?;
        let role = session.as_ref().ok_or_else(|| "no role chosen yet".to_string())?.role;
        *session = Some(Session::new(role));
        role
    };
    if let Ok(mut link) = state.input_link.lock() {
        *link = None;
    }
    if let Ok(mut input) = state.input.lock() {
        if let Some(input) = input.as_mut() {
            input.release_all_held();
        }
    }
    apply_overlay(&app, role, false, current_overlay_mode(&state));
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

/// Pushes `event` onto this (helper) process's `"input"` data channel, if
/// one currently exists — silently doing nothing otherwise (no session
/// active yet, or WebRTC negotiation hasn't completed), matching the
/// "refusal isn't a command error" behavior the direct-injection path used
/// before the two roles became separate processes.
fn send_input_event(state: &AppState, event: &InputEvent) {
    if let Ok(mut link) = state.input_link.lock() {
        if let Some(link) = link.as_mut() {
            if let Ok(bytes) = serde_json::to_vec(event) {
                link.send(&bytes);
            }
        }
    }
}

#[tauri::command]
fn helper_move_mouse(state: tauri::State<AppState>, x: i32, y: i32) -> Result<(), String> {
    send_input_event(&state, &InputEvent::Move { x, y });
    Ok(())
}

#[tauri::command]
fn helper_click(state: tauri::State<AppState>, button: String, x: i32, y: i32) -> Result<(), String> {
    mouse_button_from_str(&button)?; // validate before sending — same as before
    send_input_event(&state, &InputEvent::Click { button, x, y });
    Ok(())
}

#[tauri::command]
fn helper_key_press(state: tauri::State<AppState>, code: u16) -> Result<(), String> {
    send_input_event(&state, &InputEvent::KeyPress { code });
    Ok(())
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

/// Applies every currently-queued input event from `input_link` to `input`
/// — called once per host-loop tick. A no-op whenever nothing has arrived,
/// the session isn't Active yet, or `input` hasn't been constructed (should
/// only happen transiently around role selection).
fn drain_input_events(
    input_link: &Mutex<Option<Box<dyn MediaLink + Send>>>,
    input: &Mutex<Option<NaturalInput<HostGate, Box<dyn InputBackend + Send>>>>,
) {
    let Ok(mut link_guard) = input_link.lock() else { return };
    let Some(link) = link_guard.as_mut() else { return };
    while let Ok(Some(bytes)) = link.try_recv() {
        let Ok(event) = serde_json::from_slice::<InputEvent>(&bytes) else { continue };
        let Ok(mut input_guard) = input.lock() else { continue };
        let Some(natural) = input_guard.as_mut() else { continue };
        match event {
            InputEvent::Move { x, y } => {
                let _ = natural.move_mouse_to((x, y));
            }
            InputEvent::Click { button, x, y } => {
                if let Ok(button) = mouse_button_from_str(&button) {
                    let _ = natural.click(button, (x, y));
                }
            }
            InputEvent::KeyPress { code } => {
                let _ = natural.key_press(KeyCode(code));
            }
        }
    }
}

/// Waits until this process's session is `Active` and a real signaling
/// link exists, then negotiates a real WebRTC connection over it. `None`
/// (rather than looping forever inside this function) whenever the session
/// isn't ready yet or negotiation fails/times out — callers re-poll after a
/// short sleep, matching the "gate checked every iteration, never a cached
/// snapshot" discipline used throughout this file.
fn try_connect_media(
    role: Role,
    session: &Mutex<Option<Session>>,
    runtime: &tokio::runtime::Handle,
) -> Option<(webrtc_media::WebRtcMediaLink, webrtc_media::WebRtcMediaLink)> {
    let (key, link) = session.lock().ok().and_then(|s| {
        let s = s.as_ref()?;
        if !s.handshake.is_active() {
            return None;
        }
        Some((s.handshake.session_key(SIGNALING_KEY_INFO)?, s.link.clone()?))
    })?;
    runtime.block_on(webrtc_media::negotiate(role, link, key, IceConfig::with_default_stun())).ok()
}

/// Capture → still-screen pacing → encode → real WebRTC data channel, for
/// as long as this (Host) process runs. Also drains the `"input"` channel
/// into `NaturalInput` each tick — see `drain_input_events`.
///
/// A no-op whenever the session isn't `Active`, or before WebRTC
/// negotiation with the peer completes: the outer loop re-checks before
/// doing anything, and the inner loop re-checks every iteration too — the
/// same discipline `input::NaturalInput` uses for injection, applied here
/// to capture/encode/send. Capture always goes through
/// `capture::capture_one_frame`, so the phase 2 exclusion
/// (`exclude_window_from_capture`, applied once at window creation) covers
/// this path automatically.
///
/// Runs on a dedicated OS thread: the only async work in this app
/// (signaling, WebRTC negotiation) is reached via `runtime.block_on`, kept
/// off Tauri's own dispatch threads and off the UI entirely.
fn run_host_streaming_loop(
    session: Arc<Mutex<Option<Session>>>,
    settings: Arc<Mutex<Settings>>,
    input: Arc<Mutex<Option<NaturalInput<HostGate, Box<dyn InputBackend + Send>>>>>,
    input_link: Arc<Mutex<Option<Box<dyn MediaLink + Send>>>>,
    runtime: tokio::runtime::Handle,
) {
    const CAPTURE_INTERVAL: Duration = Duration::from_millis(280);
    const IDLE_POLL_INTERVAL: Duration = Duration::from_millis(200);

    let is_active =
        || session.lock().map(|s| s.as_ref().is_some_and(|s| s.handshake.is_active())).unwrap_or(false);
    let current_profile = || settings.lock().map(|s| s.bandwidth_profile).unwrap_or_default();

    loop {
        if !is_active() {
            std::thread::sleep(IDLE_POLL_INTERVAL);
            continue;
        }

        let Some((mut media, input_channel)) = try_connect_media(Role::Host, &session, &runtime) else {
            std::thread::sleep(IDLE_POLL_INTERVAL);
            continue;
        };
        *input_link.lock().expect("input_link lock poisoned") = Some(Box::new(input_channel));

        let profile = current_profile();
        let mut bitrate = AdaptiveBitrateController::new(profile);
        let Ok(mut encoder) = SoftwareH264Encoder::new(bitrate.target_bps(), 30.0) else {
            *input_link.lock().expect("input_link lock poisoned") = None;
            std::thread::sleep(IDLE_POLL_INTERVAL);
            continue;
        };
        let mut pacer = StillScreenPacer::new();
        let mut prev_frame: Option<capture::BgraFrame> = None;

        while is_active() {
            let profile = current_profile();
            bitrate.set_profile(profile);
            encoder.set_target_bitrate(bitrate.target_bps());

            drain_input_events(&input_link, &input);

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
                media.send(&encoded);
            }

            std::thread::sleep(CAPTURE_INTERVAL);
        }

        *input_link.lock().expect("input_link lock poisoned") = None;
    }
}

/// Real WebRTC data channel → decode → `latest_stream_frame`, for as long
/// as this (Helper) process runs. Helper-initiated input goes out via the
/// Tauri commands writing directly to `input_link` (see
/// `send_input_event`), not this loop — it only ever reads the media
/// channel.
fn run_helper_receive_loop(
    session: Arc<Mutex<Option<Session>>>,
    latest_frame: Arc<Mutex<Option<DecodedFrame>>>,
    input_link: Arc<Mutex<Option<Box<dyn MediaLink + Send>>>>,
    runtime: tokio::runtime::Handle,
) {
    const IDLE_POLL_INTERVAL: Duration = Duration::from_millis(200);
    const RECEIVE_POLL_INTERVAL: Duration = Duration::from_millis(50);

    let is_active =
        || session.lock().map(|s| s.as_ref().is_some_and(|s| s.handshake.is_active())).unwrap_or(false);

    loop {
        if !is_active() {
            std::thread::sleep(IDLE_POLL_INTERVAL);
            continue;
        }

        let Some((media, input_channel)) = try_connect_media(Role::Helper, &session, &runtime) else {
            std::thread::sleep(IDLE_POLL_INTERVAL);
            continue;
        };
        *input_link.lock().expect("input_link lock poisoned") = Some(Box::new(input_channel));

        let Ok(mut decoder) = SoftwareH264Decoder::new() else {
            *input_link.lock().expect("input_link lock poisoned") = None;
            std::thread::sleep(IDLE_POLL_INTERVAL);
            continue;
        };

        while is_active() {
            if let Ok(Some(bytes)) = media.try_recv() {
                if let Ok(Some(decoded)) = decoder.decode(&bytes) {
                    if let Ok(mut slot) = latest_frame.lock() {
                        *slot = Some(decoded);
                    }
                }
            }
            std::thread::sleep(RECEIVE_POLL_INTERVAL);
        }

        if let Ok(mut slot) = latest_frame.lock() {
            *slot = None;
        }
        *input_link.lock().expect("input_link lock poisoned") = None;
    }
}

/// Constructs this process's `Session` (and, for a Host, its
/// `NaturalInput`), then spawns the role-appropriate streaming loop.
/// Called either from `setup()` (a role was already persisted from a
/// previous run) or from the `choose_role` command (first run).
fn start_role(
    role: Role,
    session_slot: Arc<Mutex<Option<Session>>>,
    input_slot: Arc<Mutex<Option<NaturalInput<HostGate, Box<dyn InputBackend + Send>>>>>,
    input_link: Arc<Mutex<Option<Box<dyn MediaLink + Send>>>>,
    latest_stream_frame: Arc<Mutex<Option<DecodedFrame>>>,
    settings: Arc<Mutex<Settings>>,
    runtime: tokio::runtime::Handle,
) {
    *session_slot.lock().expect("session lock poisoned") = Some(Session::new(role));

    if role == Role::Host {
        let gate = HostGate(session_slot.clone());
        let feel = settings.lock().map(|s| s.input_feel).unwrap_or_default();
        let natural_input = NaturalInput::new(gate, make_backend(), feel, (0, 0));
        *input_slot.lock().expect("input lock poisoned") = Some(natural_input);
    }

    std::thread::spawn(move || match role {
        Role::Host => run_host_streaming_loop(session_slot, settings, input_slot, input_link, runtime),
        Role::Helper => run_helper_receive_loop(session_slot, latest_stream_frame, input_link, runtime),
    });
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
                        let role = {
                            let lock_result = state.session.lock();
                            let mut role = None;
                            if let Ok(mut session) = lock_result {
                                if let Some(session) = session.as_mut() {
                                    session.hotkey_end();
                                    role = Some(session.role);
                                }
                            }
                            role
                        };
                        // Belt-and-braces: `NaturalInput` already refuses
                        // any further event the instant `HostGate` reports
                        // inactive (true the moment `hotkey_end` returns,
                        // above), but force-release whatever it may have
                        // been mid-way through holding down too, so a
                        // key/button never reads as stuck on the host.
                        if let Ok(mut input) = state.input.lock() {
                            if let Some(input) = input.as_mut() {
                                input.release_all_held();
                            }
                        }
                        if let Some(role) = role {
                            apply_overlay(app, role, false, current_overlay_mode(&state));
                        }
                    }
                })
                .build(),
        )
        .setup(|app| {
            // A single global hotkey ends this process's session, in every
            // overlay visibility mode (it's a global OS hotkey, independent
            // of window state).
            app.global_shortcut().register(HOTKEY)?;

            let settings_path = app.path().app_config_dir()?.join("settings.json");
            let initial_settings = load_settings(&settings_path);

            exclude_window_from_capture(app.handle(), "main");

            // The only async work in this app (real signaling + WebRTC).
            // Multi-threaded (not `new_current_thread`) specifically so the
            // streaming-loop threads can each call `Handle::block_on`
            // concurrently from outside the runtime — a current-thread
            // runtime only supports being driven by `block_on` from the one
            // thread that built it, which doesn't fit two independent
            // caller threads (host loop, helper loop) both needing to block
            // on WebRTC negotiation. A small worker pool is enough: this
            // app's async work is I/O-bound (one WebSocket, a couple of
            // WebRTC connections), never CPU-bound. Tauri's own dispatch
            // threads and the streaming loops never touch `async fn`
            // themselves — see `crates/signaling` and `transport::
            // webrtc_media`'s doc comments for why this couldn't stay fully
            // synchronous like the rest of the app.
            let tokio_runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("failed to build the tokio runtime");
            let runtime_handle = tokio_runtime.handle().clone();
            // Leaked deliberately: the runtime's worker threads must keep
            // driving `signaling`/`webrtc_media`'s spawned tasks for the
            // rest of the process's life, and this app never shuts them
            // down early (ending a session just idles the loops, it
            // doesn't tear down the runtime).
            Box::leak(Box::new(tokio_runtime));

            let session = Arc::new(Mutex::new(None));
            let input: Arc<Mutex<Option<NaturalInput<HostGate, Box<dyn InputBackend + Send>>>>> =
                Arc::new(Mutex::new(None));
            let input_link = Arc::new(Mutex::new(None));
            let settings = Arc::new(Mutex::new(initial_settings));
            let latest_stream_frame = Arc::new(Mutex::new(None));

            if let Some(role) = initial_settings.role {
                start_role(
                    role,
                    session.clone(),
                    input.clone(),
                    input_link.clone(),
                    latest_stream_frame.clone(),
                    settings.clone(),
                    runtime_handle.clone(),
                );
            }

            app.manage(AppState {
                session,
                settings,
                settings_path,
                input,
                input_link,
                latest_stream_frame,
                runtime: runtime_handle,
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            disclosure_text,
            choose_role,
            get_state,
            get_settings,
            update_settings,
            generate_code,
            enter_code,
            confirm,
            cancel,
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
