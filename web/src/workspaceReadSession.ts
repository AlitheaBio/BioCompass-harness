import { useSyncExternalStore } from "react";

// This handle can open saved files on this installation. The cloud bearer stays in memory.
const storageKey = "bc.workspace.saved-read.v1";
const changedEvent = "biocompass-workspace-read-changed";
let generation = 0;

export function workspaceReadHandle(): string | null {
  try {
    const value = localStorage.getItem(storageKey);
    return value && /^[a-f0-9]{64}$/.test(value) ? value : null;
  } catch {
    return null;
  }
}

export function workspaceReadStorageChanged(event: StorageEvent): boolean {
  return event.storageArea === window.localStorage && (event.key === storageKey || event.key === null);
}

window.addEventListener("storage", (event) => {
  if (workspaceReadStorageChanged(event)) generation++;
});

export function workspaceReadGeneration(): number {
  return generation;
}

export function setWorkspaceReadHandle(token: string | null) {
  if (token && workspaceReadHandle() === token) return;
  generation++;
  if (!token && workspaceReadHandle() === null) return;
  try {
    if (token === null) localStorage.removeItem(storageKey);
    else if (/^[a-f0-9]{64}$/.test(token)) localStorage.setItem(storageKey, token);
    else return;
  } catch {
    return;
  }
  window.dispatchEvent(new Event(changedEvent));
}

function subscribe(notify: () => void) {
  const onStorage = (event: StorageEvent) => {
    if (workspaceReadStorageChanged(event)) notify();
  };
  window.addEventListener("storage", onStorage);
  window.addEventListener(changedEvent, notify);
  return () => {
    window.removeEventListener("storage", onStorage);
    window.removeEventListener(changedEvent, notify);
  };
}

export function useWorkspaceReadHandle(): string | null {
  return useSyncExternalStore(subscribe, workspaceReadHandle);
}
