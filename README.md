# Remote Assist — Phase 3: Input Injection, Gated on Consent

A consent-first remote assistance tool (think Parsec / Windows Quick
Assist), not a remote administration tool. See `CLAUDE.md` for the full
threat model. Phase 1 built the mutual-consent handshake; phase 2 added
screen capture and window exclusion; phase 3 adds human-paced mouse/
keyboard injection (`natural_input`), hard-gated on live consent state at
every atomic event. Sending captured frames to a helper (transport/
streaming) is still a later phase.

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
  corner indicator / Presenter), in-session sounds (on by default, or a
  "quiet session"), and input feel (Instant / Smooth / Very natural). Pure
  data and defaults only — no file I/O, no window code — so the defaults
  and their rationale are auditable on their own.
  `tests/no_setting_bypasses_consent.rs` proves no setting, including
  Presenter (fully hidden) and every input feel, can affect the handshake.
- `crates/input` — human-paced mouse/keyboard injection. `motion` and
  `timing` are pure generators (curved, eased mouse paths; click-timing
  jitter; seeded per-call, never fixed offsets — see their doc comments);
  `backend` is the only place that calls the OS (`SendInput` on Windows,
  `CGEvent` on macOS — unverified, no macOS machine in this dev loop — no
  DLL injection, no `SetWindowsHookEx`, no memory reads of other
  processes). `natural_input::NaturalInput` is the gate: it re-checks a
  live `SessionGate` immediately before *every* atomic step, so a session
  ending mid-motion stops injection within one step, always, and releases
  any key/button it was holding down as a safety fallback. Its own tests
  drive a real `consent::HandshakeMachine` through host-hotkey,
  helper-hotkey (propagated as a `PeerMessage`), and peer-disconnect
  endings to prove this rather than trusting a mock.
- `crates/desktop` (package `remote-assist`) — the Tauri app: Host and
  Helper windows, each driving its own `HandshakeMachine`; settings
  load/save and the overlay-mode window logic (`src/overlay.rs`); capture
  exclusion applied to both windows at creation
  (`exclude_window_from_capture` in `src/lib.rs`); a single
  `NaturalInput`, gated on the host's own handshake state via `HostGate`,
  behind the `helper_move_mouse` / `helper_click` / `helper_key_press`
  commands. The global end-session hotkey and the in-app End Session
  control both force-release any held key/button on top of the gate
  closing on its own. `tests/capture_excludes_app_windows.rs` proves
  excluded windows are actually absent from a real capture, in every
  overlay mode. `tests/input_stops_on_hotkey_and_end.rs` proves injection
  stops on a host hotkey, a helper hotkey, and a peer disconnect, in every
  combination of input feel and overlay visibility.
  `tests/identity_hygiene.rs` checks the process name and window titles
  against the naming rules in `CLAUDE.md`.
- `crates/ui` — the plain HTML/CSS/JS frontend shared by both windows,
  including the settings panel (host window only) and the helper's "try
  it" control surface (a small box that sends mouse move/click events to
  the host while a session is active).

## Running

```
cargo test --workspace     # 62 tests
cargo run -p remote-assist # opens the Host and Helper windows
```

Requires the Rust MSVC toolchain (Visual Studio Build Tools, "Desktop
development with C++" workload) on Windows.

Try it: click **Generate Code** in the Host window, type that code into the
Helper window, press **Confirm** on both sides. In the Host window's
Settings panel, switch overlay visibility between Full, Minimal, and
Presenter while a session is active to see the window resize/hide; toggle
"quiet session" to mute the start/end cue; switch input feel between
Instant, Smooth, and Very natural. In the Helper window, once active, move
the mouse and click inside the "try it" box — that's real `SendInput`
moving your own cursor near the top-left of your screen, paced per the
current input feel. Press `Ctrl+Alt+End` anywhere (or the in-app **End
Session** control) to end the session instantly in every overlay mode,
including Presenter — injection stops within the same tick, even mid-move.

Note: once the app is running, screenshotting its own windows — by us, by
you, or by any other capture tool — will show whatever is behind them
instead. That's `WDA_EXCLUDEFROMCAPTURE` working as intended, not a bug.

## What's next (not built yet)

- Real signaling/transport (WebRTC) implementing `consent::transport::PeerLink`
  between two separate machines, replacing `LoopbackLink`. Until that
  lands, "helper" and "host" are two windows of the same local process, so
  the input-injection demo moves this machine's own cursor rather than a
  remote one.
- Actually streaming captured frames to a helper, gated on
  `HandshakeMachine::is_active()`.
- macOS capture-session window exclusion, once `scap` ships a working
  release with `excluded_targets` (see `crates/capture/Cargo.toml`).
- macOS input injection (`input::backend::macos::CgEventBackend`) is
  written but unverified — no macOS machine in this dev loop.
