// Phase 1 UI shell: two windows (Host, Helper) that each drive their own
// `consent::HandshakeMachine`, connected today by an in-process
// `LoopbackLink`. This file only ever asks the `consent` crate for
// decisions — it never sets a session to Active itself. When phase 2 adds
// real WebRTC signaling, only `Session`'s link type changes; the commands
// below stay the same.

use consent::transport::{LoopbackLink, PeerLink};
use consent::{HandshakeMachine, LocalEvent, PeerMessage, Role, SessionCode, SessionState};
use serde::Serialize;
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

struct AppState(Mutex<Session>);

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
    let session = state.0.lock().map_err(|_| "session lock poisoned".to_string())?;
    match role.as_str() {
        "host" => Ok(to_dto(Role::Host, session.host.state())),
        "helper" => Ok(to_dto(Role::Helper, session.helper.state())),
        _ => Err(format!("unknown role: {role}")),
    }
}

#[tauri::command]
fn host_generate_code(state: tauri::State<AppState>) -> Result<StateDto, String> {
    let mut session = state.0.lock().map_err(|_| "session lock poisoned".to_string())?;
    let code = SessionCode::generate();
    session
        .host
        .apply_local(LocalEvent::GenerateCode(code))
        .map_err(|e| e.to_string())?;
    Ok(to_dto(Role::Host, session.host.state()))
}

#[tauri::command]
fn helper_enter_code(state: tauri::State<AppState>, code: String) -> Result<StateDto, String> {
    let mut session = state.0.lock().map_err(|_| "session lock poisoned".to_string())?;
    let code = SessionCode::parse(&code).map_err(|e| e.to_string())?;
    session
        .helper
        .apply_local(LocalEvent::EnterCode(code))
        .map_err(|e| e.to_string())?;
    Ok(to_dto(Role::Helper, session.helper.state()))
}

#[tauri::command]
fn host_confirm(state: tauri::State<AppState>) -> Result<StateDto, String> {
    let mut session = state.0.lock().map_err(|_| "session lock poisoned".to_string())?;
    let code_to_relay = match session.host.state() {
        SessionState::AwaitingLocalConfirmation { code } => Some(code.clone()),
        _ => None,
    };
    session.host.apply_local(LocalEvent::Confirm).map_err(|e| e.to_string())?;
    if let Some(code) = code_to_relay {
        session.host_link.send(PeerMessage::Confirm { code });
    }
    session.pump_helper();
    Ok(to_dto(Role::Host, session.host.state()))
}

#[tauri::command]
fn helper_confirm(state: tauri::State<AppState>) -> Result<StateDto, String> {
    let mut session = state.0.lock().map_err(|_| "session lock poisoned".to_string())?;
    let code_to_relay = match session.helper.state() {
        SessionState::AwaitingLocalConfirmation { code } => Some(code.clone()),
        _ => None,
    };
    session.helper.apply_local(LocalEvent::Confirm).map_err(|e| e.to_string())?;
    if let Some(code) = code_to_relay {
        session.helper_link.send(PeerMessage::Confirm { code });
    }
    session.pump_host();
    Ok(to_dto(Role::Helper, session.helper.state()))
}

#[tauri::command]
fn host_cancel(state: tauri::State<AppState>) -> Result<StateDto, String> {
    let mut session = state.0.lock().map_err(|_| "session lock poisoned".to_string())?;
    session.host.apply_local(LocalEvent::Cancel).map_err(|e| e.to_string())?;
    session.host_link.send(PeerMessage::Cancel);
    session.pump_helper();
    Ok(to_dto(Role::Host, session.host.state()))
}

#[tauri::command]
fn helper_cancel(state: tauri::State<AppState>) -> Result<StateDto, String> {
    let mut session = state.0.lock().map_err(|_| "session lock poisoned".to_string())?;
    session.helper.apply_local(LocalEvent::Cancel).map_err(|e| e.to_string())?;
    session.helper_link.send(PeerMessage::Cancel);
    session.pump_host();
    Ok(to_dto(Role::Helper, session.helper.state()))
}

/// Either window's in-app "End Session" button — behaves like the global
/// hotkey but scoped to one side's local state, exactly as a real remote
/// hotkey press on that one physical machine would.
#[tauri::command]
fn end_session(state: tauri::State<AppState>, role: String) -> Result<StateDto, String> {
    let mut session = state.0.lock().map_err(|_| "session lock poisoned".to_string())?;
    match role.as_str() {
        "host" => {
            let _ = session.host.apply_local(LocalEvent::HotkeyEnd);
            session.host_link.send(PeerMessage::HotkeyEnd);
            session.pump_helper();
            Ok(to_dto(Role::Host, session.host.state()))
        }
        "helper" => {
            let _ = session.helper.apply_local(LocalEvent::HotkeyEnd);
            session.helper_link.send(PeerMessage::HotkeyEnd);
            session.pump_host();
            Ok(to_dto(Role::Helper, session.helper.state()))
        }
        _ => Err(format!("unknown role: {role}")),
    }
}

#[tauri::command]
fn reset_session(state: tauri::State<AppState>) -> Result<(), String> {
    let mut session = state.0.lock().map_err(|_| "session lock poisoned".to_string())?;
    *session = Session::new();
    Ok(())
}

const HOTKEY: &str = "CommandOrControl+Alt+End";

fn main() {
    tauri::Builder::default()
        .manage(AppState(Mutex::new(Session::new())))
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    if event.state() == ShortcutState::Pressed {
                        let state = app.state::<AppState>();
                        if let Ok(mut session) = state.0.lock() {
                            session.hotkey_end_both();
                        }
                    }
                })
                .build(),
        )
        .setup(|app| {
            // A single global hotkey ends whichever side(s) of this demo
            // are mid-handshake or active. On a real deployment each
            // machine runs one role, so this is "the" panic key for that
            // machine; here it stands for both simulated machines at once.
            app.global_shortcut().register(HOTKEY)?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            disclosure_text,
            get_state,
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
