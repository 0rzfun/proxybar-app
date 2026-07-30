import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import "./style.css";

type SettingsState = {
  subscription_url: string;
  proxy_port: number;
  title: string;
  description: string;
  subscription_label: string;
  subscription_placeholder: string;
  proxy_port_label: string;
  confirm: string;
  cancel: string;
};

const form = document.querySelector<HTMLFormElement>("#settings-form")!;
const subscriptionInput = document.querySelector<HTMLInputElement>("#subscription-url")!;
const portInput = document.querySelector<HTMLInputElement>("#proxy-port")!;
const error = document.querySelector<HTMLParagraphElement>("#error")!;
const save = document.querySelector<HTMLButtonElement>("#save")!;
const cancel = document.querySelector<HTMLButtonElement>("#cancel")!;

async function load(): Promise<void> {
  const state = await invoke<SettingsState>("get_settings_state");
  document.documentElement.lang = navigator.language;
  document.querySelector<HTMLHeadingElement>("#title")!.textContent = state.title;
  document.querySelector<HTMLParagraphElement>("#description")!.textContent = state.description;
  document.querySelector<HTMLLabelElement>("#subscription-label")!.textContent = state.subscription_label;
  document.querySelector<HTMLLabelElement>("#proxy-port-label")!.textContent = state.proxy_port_label;
  subscriptionInput.value = state.subscription_url;
  subscriptionInput.placeholder = state.subscription_placeholder;
  portInput.value = String(state.proxy_port);
  save.textContent = state.confirm;
  cancel.textContent = state.cancel;
  await getCurrentWindow().setTitle(state.title);
  window.setTimeout(() => subscriptionInput.focus(), 50);
}

form.addEventListener("submit", async (event) => {
  event.preventDefault();
  error.textContent = "";
  save.disabled = true;
  try {
    await invoke("save_settings", {
      subscriptionUrl: subscriptionInput.value.trim(),
      proxyPort: Number(portInput.value),
    });
  } catch (reason) {
    error.textContent = String(reason);
  } finally {
    save.disabled = false;
  }
});

async function hideSettings(): Promise<void> {
  error.textContent = "";
  try {
    await invoke("hide_settings");
  } catch (reason) {
    error.textContent = String(reason);
  }
}

cancel.addEventListener("click", () => void hideSettings());
window.addEventListener("keydown", (event) => {
  if (event.key === "Escape") {
    event.preventDefault();
    void hideSettings();
  }
});
void listen("settings-opened", () => void load());
window.addEventListener("DOMContentLoaded", () => void load());
