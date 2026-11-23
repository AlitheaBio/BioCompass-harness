// Browser session for the installed node's local release controls. The private key stays on the node.
const key = "bc.local-owner-session";
export const ownerLinkHelp = "Open the owner link printed by biocompass up or saved in owner-link.txt in the local data directory.";

function isLoopback() {
  const host = window.location.hostname;
  return host === "localhost" || host === "[::1]" || /^127(?:\.\d{1,3}){3}$/.test(host);
}

export function localOwnerSession(): string | null {
  try {
    return isLoopback() ? sessionStorage.getItem(key) : null;
  } catch {
    return null;
  }
}

export function localOwnerHeaders(): Record<string, string> {
  const session = localOwnerSession();
  return session ? { "X-BioCompass-Local-Owner": session } : {};
}

// Call only for known local endpoints. Never forward the header across a redirect.
export function localOwnerRequest() {
  const headers = localOwnerHeaders();
  return {
    mode: "same-origin" as const,
    redirect: "error" as const,
    credentials: "omit" as const,
    headers,
  };
}

export async function redeemLocalOwnerLink(): Promise<void> {
  const url = new URL(window.location.href);
  if (!url.hash.startsWith("#local-owner=")) return;
  const ticket = url.hash.slice("#local-owner=".length);
  url.hash = "";
  window.history.replaceState(window.history.state, "", url);
  if (!isLoopback() || !/^[a-f0-9]{64}$/i.test(ticket)) throw new Error("Invalid owner link.");
  const response = await fetch("/api/v1/local/owner-session", {
    method: "POST", mode: "same-origin", credentials: "omit", redirect: "error", cache: "no-store",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ ticket }),
  });
  if (!response.ok) throw new Error("Owner link unavailable.");
  const { owner_session } = await response.json();
  if (typeof owner_session !== "string" || !/^[a-f0-9]{64}$/i.test(owner_session)) throw new Error("Invalid owner session.");
  sessionStorage.setItem(key, owner_session);
}
