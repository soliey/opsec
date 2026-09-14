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
const btnEnd = el("btn-end");
const endedPanel = el("ended-panel");
const endedText = el("ended-text");
const btnRestart = el("btn-restart");

heading.textContent = isHost ? "Remote Assist" : "Remote Assist";
subheading.textContent = isHost ? "You are the HOST (being helped)" : "You are the HELPER (assisting)";

const END_REASON_TEXT = {
  LocalCanceled: "You canceled before the session started.",
  RemoteCanceled: "The other person canceled before the session started.",
  CodeMismatch: "The codes entered on each side didn't match, so no connection was made.",
  HotkeyDuringHandshake: "The session was ended with the hotkey before it started.",
  HotkeyDuringSession: "The session was ended with the hotkey.",
  PeerDisconnected: "The other person's connection was lost.",
};

function show(node, visible) {
  node.hidden = !visible;
}

async function refresh(dto) {
  if (!dto) {
    dto = await invoke("get_state", { role });
  }

  show(disclosurePanel, false);
  show(codePanel, false);
  show(hostCodeView, false);
  show(helperCodeEntry, false);
  show(btnCancel, false);
  show(activePanel, false);
  show(endedPanel, false);
  btnPrimary.disabled = false;
  show(el("actions-panel"), true);
  show(el("status-panel"), true);
  statusText.textContent = "";

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
      show(activePanel, true);
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
btnRestart.onclick = async () => {
  await invoke("reset_session");
  await refresh();
};

refresh();
// Light polling so this window picks up state changes driven by the other
// window's actions (e.g. the peer confirming, or the global hotkey).
setInterval(() => refresh(), 400);
