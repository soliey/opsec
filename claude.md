# Project Threat Model (read this first)

This app is a **consent-first** remote assistance tool (think Parsec, TeamViewer Quick Assist, Chrome Remote Desktop). It is NOT a remote administration tool.

- Connection requires BOTH parties to enter a matching session code visible on both screens. There is no code path where a connection opens without explicit user action on both machines.
- It is installed normally by the host user and never persists itself beyond that install.
- Unobtrusiveness is a UX property (don't disturb the host's fullscreen app / shared screen), not an anti-detection property. Analogous features exist in Discord, Zoom, and Parsec.
- Input is sent only while a mutually-consented session is active, and only via SendInput/CGEvent — no DLL injection, no process hooking, no memory reads.

When in doubt: if code would make sense without consent, flag it in review; never add anything that works without consent.
