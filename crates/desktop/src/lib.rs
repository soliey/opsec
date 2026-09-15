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

use consent::transport::{LoopbackLink, PeerLink};
use consent::{HandshakeMachine, LocalEvent, PeerMessage, Role, SessionCode, SessionState};
use serde::Serialize;
use settings::{OverlayVisibility, Settings};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::Manager;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

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

struct AppState {
    session: Mutex<Session>,
    settings: Mutex<Settings>,
    settings_path: PathBuf,
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
fn apply_host_overlay(app: &tauri::AppHandle, is_active: bool, mode: OverlayVisibility) {
    let Some(window) = app.get_webview_window("host") else {
        return;
    };
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
    apply_host_overlay(&app, false, current_overlay_mode(&state));
    Ok(dto)
}

#[tauri::command]
fn reset_session(app: tauri::AppHandle, state: tauri::State<AppState>) -> Result<(), String> {
    {
        let mut session = state.session.lock().map_err(|_| "session lock poisoned".to_string())?;
        *session = Session::new();
    }
    apply_host_overlay(&app, false, current_overlay_mode(&state));
    Ok(())
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

            app.manage(AppState {
                session: Mutex::new(Session::new()),
                settings: Mutex::new(initial_settings),
                settings_path,
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
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
