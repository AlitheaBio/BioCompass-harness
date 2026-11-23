// Only unfinished requests live here. A completed prepare frees its key so a new run is intentional.
const pending = new Map<string, string>();
const prefix = "bc.prepare.v1.";
let principal: string | null = null;

export function setPrepareCaller(caller: {org_id: string; actor: string; user_id?: string | null; auth_method?: string} | null) {
  principal = caller && JSON.stringify([caller.org_id, caller.auth_method === "api_key" ? caller.actor : caller.user_id || caller.actor]);
}

function randomId(): string {
  if (typeof crypto.randomUUID === "function") return crypto.randomUUID();
  const b = crypto.getRandomValues(new Uint8Array(16));
  b[6] = (b[6]! & 15) | 64;
  b[8] = (b[8]! & 63) | 128;
  const hex = Array.from(b, v => v.toString(16).padStart(2, "0")).join("");
  return [hex.slice(0,8), hex.slice(8,12), hex.slice(12,16), hex.slice(16,20), hex.slice(20)].join("-");
}

export async function beginPrepareRequest(payload: unknown) {
  if (!principal) return null;
  const canonical = JSON.stringify([principal, payload], (_key, value: unknown) => {
    if (value && typeof value === "object" && !Array.isArray(value)) {
      return Object.fromEntries(Object.entries(value).sort(([a], [b]) => a < b ? -1 : a > b ? 1 : 0));
    }
    return value;
  });
  // LAN HTTP may lack SubtleCrypto. Keep the request in memory there rather than persisting input data.
  const hash = crypto.subtle && await crypto.subtle.digest("SHA-256", new TextEncoder().encode(canonical));
  const slot = hash ? prefix + Array.from(new Uint8Array(hash), b => b.toString(16).padStart(2, "0")).join("") : canonical;
  let storage: Storage | undefined;
  let previous = pending.get(slot);
  if (hash) {
    try {
      storage = window.sessionStorage;
      previous ??= storage.getItem(slot) ?? undefined;
    } catch { /* A tab with storage disabled still supports retry in memory. */ }
  }
  const key = previous ?? randomId();
  pending.set(slot, key);
  try { storage?.setItem(slot, key); } catch { /* Keep the in-memory copy. */ }
  return { key, finish() {
    if (pending.get(slot) === key) pending.delete(slot);
    try { if (storage?.getItem(slot) === key) storage.removeItem(slot); } catch { /* No stored copy. */ }
  }};
}
