# Project Threat Model (read this first)

This app is a **consent-first** remote assistance tool (think Parsec, TeamViewer Quick Assist, Chrome Remote Desktop). It is NOT a remote administration tool.

- Connection requires BOTH parties to enter a matching session code visible on both screens. There is no code path where a connection opens without explicit user action on both machines.
- It is installed normally by the host user and never persists itself beyond that install.
- Unobtrusiveness is a UX property (don't disturb the host's fullscreen app / shared screen), not an anti-detection property. Analogous features exist in Discord, Zoom, and Parsec.
- Input is sent only while a mutually-consented session is active, and only via SendInput/CGEvent — no DLL injection, no process hooking, no memory reads.

When in doubt: if code would make sense without consent, flag it in review; never add anything that works without consent.

## Application identity

This is a consumer app, so it identifies itself plainly — the same way Zoom, Discord, and Parsec show their real name in title bars and task managers, never disguised as a system process or unrelated app. Concretely:

- Product name: **"Remote Assist"**. Used as-is, unabbreviated, in every window title.
- Process/binary name: **`remote-assist`** — the kebab-case form of the product name.
- Status text ("Session Active", "Waiting for confirmation", role labels) belongs in the window body, never the title bar. Every window's title is the product name alone, nothing appended — this keeps title bars stable regardless of session state, and is required for the minimal corner indicator in particular.

## Capture exclusion vs. unobtrusiveness

Our own windows (host and helper) are excluded from any screen capture (Windows: `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)`; macOS: ScreenCaptureKit content-filter exclusion) unconditionally, regardless of the host's overlay-visibility preference. This is what keeps our UI out of the video the helper actually sees, and it holds in every overlay mode — it is not something a setting can turn off. Overlay visibility (full / minimal / presenter) is a separate, host-only preference for what the host sees on their own physical screen; it never affects what capture excludes.
