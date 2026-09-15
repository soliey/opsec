const { invoke } = window.__TAURI__.core;

const role = new URLSearchParams(window.location.search).get("role") === "helper" ? "helper" : "host";
const isHost = role === "host";

const el = (id) => document.getElementById(id);

const heading = el("heading");
const subheading = el("subheading");
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

heading.textContent = "Remote Assist";
subheading.textContent = isHost ? "You are the HOST (being helped)" : "You are the HELPER (assisting)";

const END_REASON_TEXT = {
  LocalCanceled: "You canceled before the session started.",
  RemoteCanceled: "The other person canceled before the session started.",
  CodeMismatch: "The codes entered on each side didn't match, so no connection was made.",
  HotkeyDuringHandshake: "The session was ended with the hotkey before it started.",
  HotkeyDuringSession: "The session was ended with the hotkey.",
  PeerDisconnected: "The other person's connection was lost.",
};

let currentSettings = { overlay_visibility: "full", sounds_enabled: true, input_feel: "smooth" };
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
}

async function saveSettings() {
  await invoke("update_settings", { newSettings: currentSettings });
}

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
}

// The helper's "try it" control surface: box-local coordinates are scaled
// up into a small, predictable region near the top-left of the host's
// real screen, so the demo moves the host's actual cursor somewhere safe
// and visible rather than wherever the virtual desktop's origin lands.
// Every event below is refused server-side the instant the session isn't
// Active (see NaturalInput/HostGate in the desktop crate) — this is only
// about where a *permitted* event lands, never about permission itself.
const REMOTE_SURFACE_SCALE = 5;

function remoteSurfaceToScreenXY(evt) {
  const rect = remoteSurface.getBoundingClientRect();
  const bx = Math.max(0, Math.min(rect.width, evt.clientX - rect.left));
  const by = Math.max(0, Math.min(rect.height, evt.clientY - rect.top));
  return [Math.round(bx * REMOTE_SURFACE_SCALE), Math.round(by * REMOTE_SURFACE_SCALE)];
}

if (!isHost) {
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

async function refresh(dto) {
  if (!dto) {
    dto = await invoke("get_state", { role });
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
        btnPrimary.onclick = async () => refresh(await invoke("host_generate_code"));
      } else {
        show(disclosurePanel, true);
        disclosureText.textContent = await invoke("disclosure_text");
        show(codePanel, true);
        show(helperCodeEntry, true);
        btnPrimary.textContent = "Confirm";
        btnPrimary.onclick = async () => {
          try {
            const next = await invoke("helper_enter_code", { code: codeInput.value });
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
      btnPrimary.onclick = async () => {
        const cmd = isHost ? "host_confirm" : "helper_confirm";
        await refresh(await invoke(cmd));
      };
      btnCancel.onclick = async () => {
        const cmd = isHost ? "host_cancel" : "helper_cancel";
        await refresh(await invoke(cmd));
      };
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
      btnCancel.onclick = async () => {
        const cmd = isHost ? "host_cancel" : "helper_cancel";
        await refresh(await invoke(cmd));
      };
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
      // The control surface is helper-only: the host is the one being
      // controlled, not the one controlling.
      show(remoteSurfacePanel, !isHost);
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

btnEnd.onclick = async () => refresh(await invoke("end_session", { role }));
btnEndMinimal.onclick = async () => refresh(await invoke("end_session", { role }));
btnRestart.onclick = async () => {
  await invoke("reset_session");
  await refresh();
};

(async () => {
  await loadSettings();
  await refresh();
})();

// Light polling so this window picks up state changes driven by the other
// window's actions (e.g. the peer confirming, or the global hotkey).
setInterval(() => refresh(), 400);
