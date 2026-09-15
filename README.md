# Remote Assist — Phase 2: Capture, Exclusion, Presenter Mode, Settings

A consent-first remote assistance tool (think Parsec / Windows Quick
Assist), not a remote administration tool. See `CLAUDE.md` for the full
threat model. Phase 1 built the mutual-consent handshake; phase 2 adds
screen capture, excludes our own windows from it, a host-side overlay
visibility/sounds settings module, and application identity hygiene.
Sending captured frames to a helper (transport/streaming) is still a later
phase.

## Layout

- `crates/consent` — the auditable core. A pure state machine
  (`HandshakeMachine`) that decides whether a session may become `Active`.
  Start here to audit the consent guarantee; `src/handshake.rs`'s doc
  comment explains the invariant, and
  `tests/no_session_without_both_confirmations.rs` proves it end-to-end.
- `crates/capture` — screen capture via `scap` (DXGI Desktop Duplication on
  Windows, ScreenCaptureKit on macOS), and `exclusion`, which keeps our own
  windows out of any capture. Windows: `SetWindowDisplayAffinity
  (WDA_EXCLUDEFROMCAPTURE)`, applied once per window, independent of any
  setting. macOS: exclusion is a capture-session property
  (`SCContentFilter`) rather than a window property, and isn't wired up yet
  because the `scap` release with `excluded_targets` support is currently
  unbuildable upstream — see the comment in `crates/capture/Cargo.toml`.
- `crates/settings` — host preferences: overlay visibility (Full / Minimal
  corner indicator / Presenter) and in-session sounds (on by default, or a
  "quiet session"). Pure data and defaults only — no file I/O, no window
  code — so the defaults and their rationale are auditable on their own.
  `tests/no_setting_bypasses_consent.rs` proves no setting, including
  Presenter (fully hidden), can affect the handshake.
- `crates/desktop` (package `remote-assist`) — the Tauri app: Host and
  Helper windows, each driving its own `HandshakeMachine`; settings
  load/save and the overlay-mode window logic (`src/overlay.rs`); capture
  exclusion applied to both windows at creation
  (`exclude_window_from_capture` in `src/lib.rs`).
  `tests/capture_excludes_app_windows.rs` proves excluded windows are
  actually absent from a real capture, in every overlay mode.
  `tests/identity_hygiene.rs` checks the process name and window titles
  against the naming rules in `CLAUDE.md`.
- `crates/ui` — the plain HTML/CSS/JS frontend shared by both windows,
  including the settings panel (host window only).

## Running

```
cargo test --workspace     # 40 tests
cargo run -p remote-assist # opens the Host and Helper windows
```

Requires the Rust MSVC toolchain (Visual Studio Build Tools, "Desktop
development with C++" workload) on Windows.

Try it: click **Generate Code** in the Host window, type that code into the
Helper window, press **Confirm** on both sides. In the Host window's
Settings panel, switch overlay visibility between Full, Minimal, and
Presenter while a session is active to see the window resize/hide; toggle
"quiet session" to mute the start/end cue. Press `Ctrl+Alt+End` anywhere
(or the in-app **End Session** control) to end the session instantly in
every overlay mode, including Presenter.

Note: once the app is running, screenshotting its own windows — by us, by
you, or by any other capture tool — will show whatever is behind them
instead. That's `WDA_EXCLUDEFROMCAPTURE` working as intended, not a bug.

## What's next (not built yet)

- Real signaling/transport (WebRTC) implementing `consent::transport::PeerLink`
  between two separate machines, replacing `LoopbackLink`.
- Actually streaming captured frames to a helper, gated on
  `HandshakeMachine::is_active()`.
- macOS capture-session window exclusion, once `scap` ships a working
  release with `excluded_targets` (see `crates/capture/Cargo.toml`).
- Input injection (SendInput/CGEvent) via the global hotkey's existing
  end-session path, gated the same way.
