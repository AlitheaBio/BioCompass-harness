import React, { useEffect, useState } from "react";
import {
  fetchWorkspaceHub,
  formatErrorInline,
  loginWorkspaceHubClient,
  logoutWorkspaceHubClient,
  updateWorkspaceHub,
} from "../api";
import { ownerLinkHelp } from "../localOwner";
import type { WorkspaceHubMode, WorkspaceHubStatus } from "../types";

export interface WorkspaceHubChooserProps {
  compact?: boolean;
  initialStatus?: WorkspaceHubStatus | null;
  onHubChanged?: (status: WorkspaceHubStatus) => Promise<void> | void;
}

/**
 * Track U3 — In-app workspace/hub chooser for the local client:
 * switch between Alithea Cloud, a custom Customer Cloud URL, or Offline without restarting.
 */
export function WorkspaceHubChooser({
  compact = false,
  initialStatus = null,
  onHubChanged,
}: WorkspaceHubChooserProps) {
  const [status, setStatus] = useState<WorkspaceHubStatus | null>(initialStatus);
  const [mode, setMode] = useState<WorkspaceHubMode>(initialStatus?.mode ?? "offline");
  const [customUrl, setCustomUrl] = useState<string>(
    initialStatus?.mode === "custom_cloud" ? (initialStatus.hub_url ?? "") : "",
  );
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [showSignIn, setShowSignIn] = useState(false);
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [orgId, setOrgId] = useState("");
  const [totpCode, setTotpCode] = useState("");
  const [hubKeyInput, setHubKeyInput] = useState("");

  useEffect(() => {
    if (initialStatus) {
      setStatus(initialStatus);
      setMode(initialStatus.mode);
      if (initialStatus.mode === "custom_cloud" && initialStatus.hub_url) {
        setCustomUrl(initialStatus.hub_url);
      }
    }
  }, [initialStatus]);

  useEffect(() => {
    if (status) return;
    let active = true;
    void fetchWorkspaceHub()
      .then((res) => {
        if (!active) return;
        setStatus(res);
        setMode(res.mode);
        if (res.mode === "custom_cloud" && res.hub_url) {
          setCustomUrl(res.hub_url);
        }
      })
      .catch(() => {
        // Non-fatal on initial probe
      });
    return () => {
      active = false;
    };
  }, [status]);

  const formatErr = (err: unknown): string => {
    const msg = formatErrorInline(err);
    return msg.toLowerCase().includes("owner link") || msg.toLowerCase().includes("unauthenticated")
      ? `${msg} ${ownerLinkHelp}`
      : msg;
  };

  const applyHubChoice = async (nextMode: WorkspaceHubMode, nextUrl?: string, hubKey?: string) => {
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      const updated = await updateWorkspaceHub({
        mode: nextMode,
        ...(nextMode === "custom_cloud" ? { hub_url: (nextUrl ?? customUrl).trim() } : {}),
        ...(hubKey ? { hub_key: hubKey.trim().toLowerCase() } : {}),
      });
      if (hubKey) setHubKeyInput("");
      setStatus(updated);
      setMode(updated.mode);
      if (updated.mode === "custom_cloud" && updated.hub_url) {
        setCustomUrl(updated.hub_url);
      }
      setNotice(
        updated.mode === "offline"
          ? "Switched to Offline mode (local workspace only)."
          : `Connected to ${updated.hub_url}.`,
      );
      await onHubChanged?.(updated);
    } catch (err: unknown) {
      setError(formatErr(err));
    } finally {
      setBusy(false);
    }
  };

  const handleSignIn = async (event: React.FormEvent) => {
    event.preventDefault();
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      const updated = await loginWorkspaceHubClient({
        email: email.trim(),
        password,
        ...(orgId.trim() ? { org_id: orgId.trim() } : {}),
        ...(totpCode.trim() ? { totp_code: totpCode.trim() } : {}),
      });
      setPassword("");
      setTotpCode("");
      setShowSignIn(false);
      setStatus(updated);
      setNotice(`Signed in to ${updated.hub_url}.`);
      await onHubChanged?.(updated);
    } catch (err: unknown) {
      setError(formatErr(err));
    } finally {
      setBusy(false);
    }
  };

  const handleSignOut = async () => {
    setBusy(true);
    setError(null);
    try {
      const updated = await logoutWorkspaceHubClient();
      setStatus(updated);
      setNotice("Signed out of cloud hub session.");
      await onHubChanged?.(updated);
    } catch (err: unknown) {
      setError(formatErr(err));
    } finally {
      setBusy(false);
    }
  };

  const airgapped = Boolean(status?.airgapped);
  const badgeLabel = airgapped
    ? "Air-gapped (Offline)"
    : status?.mode === "offline"
      ? "Offline"
      : status?.connected
        ? status.mode === "alithea_cloud"
          ? "Alithea Cloud"
          : "Customer Cloud"
        : "Connecting…";

  return (
    <div
      className="bc-card"
      data-testid="workspace-hub-chooser"
      style={compact ? { padding: "12px 14px", marginBottom: 14 } : undefined}
    >
      <div className="bc-card-header" style={{ alignItems: "center", gap: 10 }}>
        <div>
          <div className="bc-card-title">Cloud Workspace Hub</div>
          <p className="bc-caption" style={{ marginTop: 2, marginBottom: 0 }}>
            {airgapped
              ? "Started with --airgapped. Local workspace only."
              : status?.mode !== "offline" && status?.hub_url
                ? `Connected to ${status.hub_url} (${status.remote_module_count} remote module${status.remote_module_count === 1 ? "" : "s"}).`
                : "Connect this local client to Alithea Cloud, a Customer Cloud URL, or work Offline."}
          </p>
        </div>
        <span
          className={`bc-badge ${
            status?.connected ? "bc-badge-success" : "bc-badge-neutral"
          }`}
          data-testid="workspace-hub-badge"
          data-hub-mode={status?.mode ?? mode}
        >
          {badgeLabel}
        </span>
      </div>

      {!airgapped && (
        <div style={{ marginTop: 10, display: "flex", flexDirection: "column", gap: 10 }}>
          <div style={{ display: "flex", flexWrap: "wrap", gap: 8, alignItems: "center" }}>
            <select
              className="bc-input"
              aria-label="Workspace hub mode"
              data-testid="workspace-hub-mode-select"
              value={mode}
              disabled={busy}
              style={{ maxWidth: 210 }}
              onChange={(e) => {
                const next = e.target.value as WorkspaceHubMode;
                setMode(next);
                if (next === "offline" || next === "alithea_cloud") {
                  void applyHubChoice(next);
                }
              }}
            >
              <option value="offline">Offline (Local only)</option>
              <option value="alithea_cloud">Alithea Cloud</option>
              <option value="custom_cloud">Customer Cloud URL…</option>
            </select>

            {mode === "custom_cloud" && (
              <>
                <input
                  type="url"
                  className="bc-input mono"
                  data-testid="workspace-hub-custom-url"
                  aria-label="Customer Cloud Hub URL"
                  placeholder="https://hub.customer.example"
                  value={customUrl}
                  disabled={busy}
                  style={{ flex: 1, minWidth: 220 }}
                  onChange={(e) => setCustomUrl(e.target.value)}
                />
                <button
                  type="button"
                  className="bc-btn bc-btn-primary bc-btn-sm"
                  data-testid="workspace-hub-connect-btn"
                  disabled={busy || !customUrl.trim()}
                  onClick={() => void applyHubChoice("custom_cloud", customUrl)}
                >
                  {busy ? "Connecting…" : "Connect"}
                </button>
              </>
            )}

            {status && status.mode !== "offline" && (
              status.authenticated ? (
                <button
                  type="button"
                  className="bc-btn bc-btn-ghost bc-btn-sm"
                  data-testid="workspace-hub-signout-btn"
                  disabled={busy}
                  onClick={() => void handleSignOut()}
                >
                  Sign out ({status.auth_actor ?? "hub user"})
                </button>
              ) : (
                <button
                  type="button"
                  className="bc-btn bc-btn-secondary bc-btn-sm"
                  data-testid="workspace-hub-signin-toggle"
                  disabled={busy}
                  onClick={() => setShowSignIn((v) => !v)}
                >
                  {showSignIn ? "Cancel Sign In" : "Sign in to Hub"}
                </button>
              )
            )}
          </div>

          {status?.recent_hubs && status.recent_hubs.length > 0 && (
            <div style={{ display: "flex", gap: 6, flexWrap: "wrap", alignItems: "center" }}>
              <span className="bc-caption">Recent hubs:</span>
              {status.recent_hubs.map((recent) => (
                <button
                  key={recent}
                  type="button"
                  className="bc-btn bc-btn-ghost bc-btn-sm mono"
                  data-testid={`workspace-hub-recent-${recent}`}
                  disabled={busy}
                  onClick={() => {
                    if (recent === status.default_hub_url) {
                      void applyHubChoice("alithea_cloud");
                    } else {
                      setCustomUrl(recent);
                      void applyHubChoice("custom_cloud", recent);
                    }
                  }}
                >
                  {recent}
                </button>
              ))}
            </div>
          )}

          {showSignIn && status && status.mode !== "offline" && (
            <form
              onSubmit={(e) => void handleSignIn(e)}
              style={{
                display: "grid",
                gridTemplateColumns: "repeat(auto-fit, minmax(160px, 1fr))",
                gap: 8,
                padding: 10,
                borderRadius: "var(--bc-radius-sm)",
                background: "var(--bc-surface-2)",
              }}
            >
              <input
                type="email"
                className="bc-input"
                placeholder="Email"
                required
                value={email}
                onChange={(e) => setEmail(e.target.value)}
              />
              <input
                type="password"
                className="bc-input"
                placeholder="Password"
                required
                value={password}
                onChange={(e) => setPassword(e.target.value)}
              />
              <input
                type="text"
                className="bc-input"
                placeholder="Org ID (optional)"
                value={orgId}
                onChange={(e) => setOrgId(e.target.value)}
              />
              <input
                type="text"
                className="bc-input"
                placeholder="TOTP (optional)"
                value={totpCode}
                onChange={(e) => setTotpCode(e.target.value)}
              />
              <button
                type="submit"
                className="bc-btn bc-btn-primary bc-btn-sm"
                disabled={busy || !email.trim() || !password}
              >
                Sign In
              </button>
            </form>
          )}
        </div>
      )}

      {!airgapped && status && status.mode !== "offline" && (
        <div
          data-testid="workspace-hub-key"
          data-hub-key-status={status.hub_key_status ?? "unpinned"}
          style={{ marginTop: 10, display: "flex", flexDirection: "column", gap: 6 }}
        >
          <span className="bc-caption">
            Hub signing key{" "}
            {status.hub_key_status === "changed"
              ? "(CHANGED — module UIs are blocked until you re-trust it)"
              : status.hub_key_status === "pinned"
                ? "(pinned; compare with the key your hub operator publishes)"
                : "(not pinned yet)"}
            :
          </span>
          {status.hub_key && (
            <code className="mono" data-testid="workspace-hub-key-pinned" style={{ wordBreak: "break-all" }}>
              {status.hub_key}
            </code>
          )}
          {status.hub_key_status === "changed" && status.presented_hub_key && (
            <>
              <span className="bc-caption" style={{ color: "var(--bc-danger)" }}>Hub now presents:</span>
              <code className="mono" data-testid="workspace-hub-key-presented" style={{ wordBreak: "break-all" }}>
                {status.presented_hub_key}
              </code>
            </>
          )}
          <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
            <input
              type="text"
              className="bc-input mono"
              aria-label="Hub signing key to trust"
              data-testid="workspace-hub-key-input"
              placeholder="64-hex hub signing key"
              value={hubKeyInput}
              disabled={busy}
              style={{ flex: 1, minWidth: 220 }}
              onChange={(e) => setHubKeyInput(e.target.value)}
            />
            <button
              type="button"
              className="bc-btn bc-btn-secondary bc-btn-sm"
              data-testid="workspace-hub-key-trust-btn"
              disabled={busy || !/^[0-9a-fA-F]{64}$/.test(hubKeyInput.trim())}
              onClick={() => void applyHubChoice(status.mode, status.hub_url ?? customUrl, hubKeyInput)}
            >
              Trust this key
            </button>
          </div>
        </div>
      )}

      {status?.last_error && (
        <p role="alert" className="bc-caption" style={{ color: "var(--bc-danger)", marginTop: 8, marginBottom: 0 }}>
          Hub warning: {status.last_error}
        </p>
      )}
      {error && (
        <p role="alert" className="bc-caption" style={{ color: "var(--bc-danger)", marginTop: 8, marginBottom: 0 }}>
          {error}
        </p>
      )}
      {notice && (
        <p role="status" className="bc-caption" style={{ marginTop: 8, marginBottom: 0 }}>
          {notice}
        </p>
      )}
    </div>
  );
}
