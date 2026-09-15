const { invoke } = window.__TAURI__.core;

// Which side of a session this installation is acts as. Unlike the old
// same-process demo (role picked from a `?role=` URL query param, since
// both roles lived in one process), this is now unknown until the backend
// says so: either a role was already persisted from a previous run, or the
// human picks one on the role-selection screen below.
let role = null;
let isHost = false;
let wiredForRole = false;

const el = (id) => document.getElementById(id);

const heading = el("heading");
const subheading = el("subheading");
const rolePanel = el("role-panel");
const btnRoleHost = el("btn-role-host");
const btnRoleHelper = el("btn-role-helper");
const disclosurePanel = el("disclosure-panel");
const disclosureText = el("disclosure-text");
const codePanel = el("code-panel");
const hostCodeView = el("host-code-view");
const codeDisplay = el("code-display");
const helperCodeEntry = el("helper-code-entry");
const codeInput = el("code-input");
const btnPrimary = el("btn-primary");
const btnCancel = el("btn-cancel");
const statusText = el("status-text");
const activePanel = el("active-panel");
const activePanelMinimal = el("active-panel-minimal");
const btnEnd = el("btn-end");
const btnEndMinimal = el("btn-end-minimal");
const endedPanel = el("ended-panel");
const endedText = el("ended-text");
const btnRestart = el("btn-restart");
const settingsToggleRow = el("settings-toggle-row");
const settingsPanel = el("settings-panel");
const btnSettingsToggle = el("btn-settings-toggle");
const quietSessionCheckbox = el("quiet-session-checkbox");
const remoteSurfacePanel = el("remote-surface-panel");
const remoteSurface = el("remote-surface");
const streamViewPanel = el("stream-view-panel");
const streamCanvas = el("stream-canvas");

heading.textContent = "Remote Assist";

const END_REASON_TEXT = {
  LocalCanceled: "You canceled before the session started.",
  RemoteCanceled: "The other person canceled before the session started.",
  CodeMismatch: "The codes entered on each side didn't match, so no connection was made.",
  HotkeyDuringHandshake: "The session was ended with the hotkey before it started.",
  HotkeyDuringSession: "The session was ended with the hotkey.",
  PeerDisconnected: "The other person's connection was lost.",
};

let currentSettings = {
  overlay_visibility: "full",
  sounds_enabled: true,
  input_feel: "smooth",
  bandwidth_profile: "standard",
  role: null,
};
let lastPhase = null;

function show(node, visible) {
  node.hidden = !visible;
}

// A short, synthesized cue — no embedded audio asset needed. Muted
// entirely by the "quiet session" setting.
function playCue(kind) {
  if (!currentSettings.sounds_enabled) return;
  try {
    const ctx = new (window.AudioContext || window.webkitAudioContext)();
    const osc = ctx.createOscillator();
    const gain = ctx.createGain();
    osc.frequency.value = kind === "ended" ? 320 : 520;
    gain.gain.setValueAtTime(0.15, ctx.currentTime);
    gain.gain.exponentialRampToValueAtTime(0.001, ctx.currentTime + 0.18);
    osc.connect(gain).connect(ctx.destination);
    osc.start();
    osc.stop(ctx.currentTime + 0.2);
  } catch (e) {
    // Audio isn't available in this context; the visual cue still shows.
  }
}

async function loadSettings() {
  if (!isHost) return;
  currentSettings = await invoke("get_settings");
  const radios = document.querySelectorAll('input[name="overlay"]');
  radios.forEach((r) => {
    r.checked = r.value === currentSettings.overlay_visibility;
  });
  quietSessionCheckbox.checked = !currentSettings.sounds_enabled;
  document.querySelectorAll('input[name="input-feel"]').forEach((r) => {
    r.checked = r.value === currentSettings.input_feel;
  });
  document.querySelectorAll('input[name="bandwidth"]').forEach((r) => {
    r.checked = r.value === currentSettings.bandwidth_profile;
  });
}

async function saveSettings() {
  await invoke("update_settings", { newSettings: currentSettings });
}

// The helper's "try it" control surface: box-local coordinates are scaled
// up into a small, predictable region near the top-left of the host's
// real screen, so the demo moves the host's actual cursor somewhere safe
// and visible rather than wherever the virtual desktop's origin lands.
// Every event below is refused on the host machine the instant the session
// isn't Active (see NaturalInput/HostGate in the desktop crate) — this is
// only about where a *permitted* event lands, never about permission
// itself.
const REMOTE_SURFACE_SCALE = 5;

function remoteSurfaceToScreenXY(evt) {
  const rect = remoteSurface.getBoundingClientRect();
  const bx = Math.max(0, Math.min(rect.width, evt.clientX - rect.left));
  const by = Math.max(0, Math.min(rect.height, evt.clientY - rect.top));
  return [Math.round(bx * REMOTE_SURFACE_SCALE), Math.round(by * REMOTE_SURFACE_SCALE)];
}

// Role-specific wiring (settings controls for the host, the remote control
// surface for the helper) runs exactly once, the first time this process's
// role becomes known — either from a role persisted on a previous run, or
// right after `choose_role` resolves on this one.
function wireForRole() {
  if (wiredForRole) return;
  wiredForRole = true;

  subheading.textContent = isHost ? "You are the HOST (being helped)" : "You are the HELPER (assisting)";

  if (isHost) {
    show(settingsToggleRow, true);
    btnSettingsToggle.onclick = () => {
      show(settingsPanel, settingsPanel.hidden);
    };
    document.querySelectorAll('input[name="overlay"]').forEach((radio) => {
      radio.onchange = async () => {
        currentSettings.overlay_visibility = radio.value;
        await saveSettings();
      };
    });
    quietSessionCheckbox.onchange = async () => {
      currentSettings.sounds_enabled = !quietSessionCheckbox.checked;
      await saveSettings();
    };
    document.querySelectorAll('input[name="input-feel"]').forEach((radio) => {
      radio.onchange = async () => {
        currentSettings.input_feel = radio.value;
        await saveSettings();
      };
    });
    document.querySelectorAll('input[name="bandwidth"]').forEach((radio) => {
      radio.onchange = async () => {
        currentSettings.bandwidth_profile = radio.value;
        await saveSettings();
      };
    });
  } else {
    let lastMoveSentAt = 0;
    remoteSurface.addEventListener("mousemove", (evt) => {
      const now = performance.now();
      if (now - lastMoveSentAt < 30) return;
      lastMoveSentAt = now;
      const [x, y] = remoteSurfaceToScreenXY(evt);
      invoke("helper_move_mouse", { x, y }).catch(() => {});
    });
    remoteSurface.addEventListener("click", (evt) => {
      const [x, y] = remoteSurfaceToScreenXY(evt);
      invoke("helper_click", { button: "left", x, y }).catch(() => {});
    });
  }
}

// Draws whatever the host->helper streaming pipeline (capture -> still-
// screen pacing -> H.264 encode -> real WebRTC data channel -> decode, all
// gated on the session being Active) has most recently produced. A `null`
// result just means nothing new arrived since the last poll — normal and
// frequent, especially while the host's screen is mostly still.
async function pollStreamFrame() {
  if (isHost || lastPhase !== "active") return;
  let frame;
  try {
    frame = await invoke("next_stream_frame");
  } catch (e) {
    return;
  }
  if (!frame) return;

  const raw = atob(frame.rgba_base64);
  const rgba = new Uint8ClampedArray(raw.length);
  for (let i = 0; i < raw.length; i++) rgba[i] = raw.charCodeAt(i);

  if (streamCanvas.width !== frame.width || streamCanvas.height !== frame.height) {
    streamCanvas.width = frame.width;
    streamCanvas.height = frame.height;
  }
  streamCanvas.getContext("2d").putImageData(new ImageData(rgba, frame.width, frame.height), 0, 0);
}

async function refresh(dto) {
  if (!dto) {
    dto = await invoke("get_state");
  }

  if (dto.phase === "awaiting_role") {
    show(rolePanel, true);
    show(disclosurePanel, false);
    show(codePanel, false);
    show(el("actions-panel"), false);
    show(el("status-panel"), false);
    show(activePanel, false);
    show(activePanelMinimal, false);
    show(endedPanel, false);
    show(remoteSurfacePanel, false);
    show(streamViewPanel, false);
    return;
  }
  show(rolePanel, false);

  if (role !== dto.role) {
    role = dto.role;
    isHost = role === "host";
    wireForRole();
    await loadSettings();
  }

  if (lastPhase !== dto.phase) {
    if (dto.phase === "active") playCue("active");
    if (dto.phase === "ended") playCue("ended");
    lastPhase = dto.phase;
  }

  show(disclosurePanel, false);
  show(codePanel, false);
  show(hostCodeView, false);
  show(helperCodeEntry, false);
  show(btnCancel, false);
  show(activePanel, false);
  show(activePanelMinimal, false);
  show(endedPanel, false);
  show(remoteSurfacePanel, false);
  show(streamViewPanel, false);
  btnPrimary.disabled = false;
  show(el("actions-panel"), true);
  show(el("status-panel"), true);
  statusText.textContent = "";

  // The pre-connection disclosure screen is unconditional: it is never
  // skipped or altered by a setting, regardless of overlay_visibility.
  switch (dto.phase) {
    case "awaiting_code": {
      if (isHost) {
        btnPrimary.textContent = "Generate Code";
        btnPrimary.onclick = async () => refresh(await invoke("generate_code"));
      } else {
        show(disclosurePanel, true);
        disclosureText.textContent = await invoke("disclosure_text");
        show(codePanel, true);
        show(helperCodeEntry, true);
        btnPrimary.textContent = "Confirm";
        btnPrimary.onclick = async () => {
          try {
            const next = await invoke("enter_code", { code: codeInput.value });
            await refresh(next);
          } catch (e) {
            statusText.textContent = String(e);
          }
        };
      }
      break;
    }

    case "awaiting_local_confirmation": {
      show(disclosurePanel, true);
      disclosureText.textContent = await invoke("disclosure_text");
      show(codePanel, true);
      if (isHost) {
        show(hostCodeView, true);
        codeDisplay.textContent = dto.code;
      } else {
        show(helperCodeEntry, true);
        codeInput.value = dto.code;
      }
      btnPrimary.textContent = "Confirm";
      show(btnCancel, true);
      btnPrimary.onclick = async () => refresh(await invoke("confirm"));
      btnCancel.onclick = async () => refresh(await invoke("cancel"));
      break;
    }

    case "awaiting_remote_confirmation": {
      show(codePanel, true);
      if (isHost) {
        show(hostCodeView, true);
      } else {
        show(helperCodeEntry, true);
      }
      codeDisplay.textContent = dto.code;
      codeInput.value = dto.code;
      btnPrimary.textContent = "Waiting for confirmation...";
      btnPrimary.disabled = true;
      show(btnCancel, true);
      btnCancel.onclick = async () => refresh(await invoke("cancel"));
      statusText.textContent = "Waiting for the other person to press Confirm.";
      break;
    }

    case "active": {
      show(el("actions-panel"), false);
      show(el("status-panel"), false);
      // Only the host's own overlay visibility setting changes anything
      // here; the helper always sees the full active panel. The window
      // itself (shown/hidden/resized) is handled entirely on the Rust
      // side — this only decides which in-window panel to render.
      if (isHost && currentSettings.overlay_visibility === "minimal_indicator") {
        show(activePanelMinimal, true);
      } else if (isHost && currentSettings.overlay_visibility === "presenter") {
        // Window is hidden by the backend; nothing to render.
      } else {
        show(activePanel, true);
      }
      // The control surface and stream view are helper-only: the host is
      // the one being controlled/watched, not the one controlling/watching.
      show(remoteSurfacePanel, !isHost);
      show(streamViewPanel, !isHost);
      break;
    }

    case "ended": {
      show(endedPanel, true);
      endedText.textContent = END_REASON_TEXT[dto.end_reason] || `Session ended (${dto.end_reason}).`;
      show(el("actions-panel"), false);
      break;
    }
  }
}

btnRoleHost.onclick = async () => refresh(await invoke("choose_role", { role: "host" }));
btnRoleHelper.onclick = async () => refresh(await invoke("choose_role", { role: "helper" }));

btnEnd.onclick = async () => refresh(await invoke("end_session"));
btnEndMinimal.onclick = async () => refresh(await invoke("end_session"));
btnRestart.onclick = async () => {
  await invoke("reset_session");
  await refresh();
};

refresh();

// Light polling so this window picks up state changes driven by the peer
// (e.g. the peer confirming, or the global hotkey).
setInterval(() => refresh(), 400);

// Independent, faster poll for stream frames — decoupled from the state
// poll above so the picture updates promptly without needing a full state
// round-trip each time. `pollStreamFrame` itself is a no-op whenever it's
// not the helper's active session, so this is idle almost all the time.
setInterval(() => pollStreamFrame(), 200);
