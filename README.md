# Remote Assist — Phase 1: Mutual Consent Handshake

A consent-first remote assistance tool (think Parsec / Windows Quick
Assist), not a remote administration tool. See `CLAUDE.md` for the full
threat model. This phase implements only the pairing handshake — no screen
capture, input injection, or real network transport yet.

## Layout

- `crates/consent` — the auditable core. A pure state machine
  (`HandshakeMachine`) that decides whether a session may become `Active`.
  No UI, no networking beyond the `PeerLink` trait it defines for later
  phases to implement over WebRTC. Start here to audit the consent
  guarantee; `src/handshake.rs`'s doc comment explains the invariant, and
  `tests/no_session_without_both_confirmations.rs` proves it end-to-end.
- `crates/desktop` — a Tauri app that opens two windows, Host and Helper,
  each driving its own `HandshakeMachine`. Today the two machines are
  connected by an in-process `LoopbackLink`, standing in for the WebRTC
  data channel a later phase will use for real cross-machine signaling.
- `crates/ui` — the plain HTML/CSS/JS frontend shared by both windows.

## Running

```
cargo test --workspace   # 28 tests, all in the consent crate
cargo run -p desktop     # opens the Host and Helper windows
```

Requires the Rust MSVC toolchain (Visual Studio Build Tools, "Desktop
development with C++" workload) on Windows.

Try it: click **Generate Code** in the Host window, type that code into the
Helper window, press **Confirm** on both sides, then press `Ctrl+Alt+End`
(or the in-app **End Session** button) to see the instant, mutual
disconnect.

## What's next (not built yet)

- Real signaling/transport (WebRTC) implementing `consent::transport::PeerLink`
  between two separate machines, replacing `LoopbackLink`.
- Screen capture and streaming, gated on `HandshakeMachine::is_active()`.
- Input injection (SendInput/CGEvent) via the global hotkey's existing
  end-session path, gated the same way.
