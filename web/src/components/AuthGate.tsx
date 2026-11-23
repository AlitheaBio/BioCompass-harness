import React, { useState } from "react";
import {
  acceptOrgInvitation,
  ApiClientError,
  fetchOidcAuthorizeUrl,
  formatErrorInline,
  loginPassword,
  registerDemoAccount,
} from "../api";
import type { AuthPrincipal } from "../types";
import { isSafeHttpUrl } from "../safeUrl";
import { CompassMark, IconShield } from "./Icons";

export interface AuthGateProps {
  onAuthenticated: (principal: AuthPrincipal) => void;
}

type AuthTab = "login" | "register" | "invite";

export function AuthGate({ onAuthenticated }: AuthGateProps) {
  const [tab, setTab] = useState<AuthTab>("login");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Sign-In state
  const [loginEmail, setLoginEmail] = useState("");
  const [loginPasswordVal, setLoginPasswordVal] = useState("");
  const [loginTotp, setLoginTotp] = useState("");
  const [needsTotp, setNeedsTotp] = useState(false);
  const [loginOrgId, setLoginOrgId] = useState("");

  // Demo Registration state
  const [regDisplayName, setRegDisplayName] = useState("");
  const [regOrgName, setRegOrgName] = useState("");
  const [regEmail, setRegEmail] = useState("");
  const [regPassword, setRegPassword] = useState("");

  // Invitation Acceptance state
  const [inviteToken, setInviteToken] = useState("");
  const [inviteEmail, setInviteEmail] = useState("");
  const [inviteDisplayName, setInviteDisplayName] = useState("");
  const [invitePassword, setInvitePassword] = useState("");
  const [inviteTotp, setInviteTotp] = useState("");
  const [inviteNeedsTotp, setInviteNeedsTotp] = useState(false);

  const switchTab = (next: AuthTab) => {
    setTab(next);
    setError(null);
  };

  const handleLogin = async (e: React.FormEvent) => {
    e.preventDefault();
    setError(null);
    setBusy(true);
    try {
      const res = await loginPassword({
        email: loginEmail.trim(),
        password: loginPasswordVal,
        ...(loginTotp.trim() ? { totp_code: loginTotp.trim() } : {}),
        ...(loginOrgId.trim() ? { org_id: loginOrgId.trim() } : {}),
      });
      onAuthenticated(res.principal);
    } catch (err: unknown) {
      const msg = formatErrorInline(err);
      if (
        msg.toUpperCase().includes("MFA_TOTP_REQUIRED") ||
        msg.toLowerCase().includes("totp")
      ) {
        setNeedsTotp(true);
        setError("Two-factor authentication is enabled on this account. Enter your 6-digit TOTP code.");
      } else {
        setError(msg);
      }
    } finally {
      setBusy(false);
    }
  };

  const handleRegisterDemo = async (e: React.FormEvent) => {
    e.preventDefault();
    setError(null);
    setBusy(true);
    try {
      await registerDemoAccount({
        email: regEmail.trim(),
        password: regPassword,
        ...(regDisplayName.trim() ? { display_name: regDisplayName.trim() } : {}),
        ...(regOrgName.trim() ? { org_name: regOrgName.trim() } : {}),
      });
      const loggedIn = await loginPassword({
        email: regEmail.trim(),
        password: regPassword,
      });
      onAuthenticated(loggedIn.principal);
    } catch (err: unknown) {
      setError(formatErrorInline(err));
    } finally {
      setBusy(false);
    }
  };

  const handleAcceptInvite = async (e: React.FormEvent) => {
    e.preventDefault();
    setError(null);
    setBusy(true);
    try {
      const res = await acceptOrgInvitation({
        invite_token: inviteToken.trim(),
        email: inviteEmail.trim(),
        ...(inviteTotp.trim() ? { totp_code: inviteTotp.trim() } : {}),
        ...(invitePassword ? { password: invitePassword } : {}),
        ...(inviteDisplayName.trim() ? { display_name: inviteDisplayName.trim() } : {}),
      });
      onAuthenticated(res.principal);
    } catch (err: unknown) {
      const message = formatErrorInline(err);
      if (message.toUpperCase().includes("TOTP")) setInviteNeedsTotp(true);
      setError(message);
    } finally {
      setBusy(false);
    }
  };

  const handleOidcSso = async () => {
    setError(null);
    setBusy(true);
    try {
      const url = await fetchOidcAuthorizeUrl(loginOrgId.trim() || undefined);
      const safeUrl = isSafeHttpUrl(url, { allowRelative: true });
      if (!safeUrl) {
        setError("Identity provider returned an invalid or unsafe redirect URL.");
        setBusy(false);
        return;
      }
      window.location.assign(safeUrl);
    } catch (err: unknown) {
      if (err instanceof ApiClientError) {
        const message = formatErrorInline(err);
      if (message.toUpperCase().includes("TOTP")) setInviteNeedsTotp(true);
      setError(message);
      } else {
        setError("Enterprise OIDC SSO is not configured on this Compass Hub instance.");
      }
      setBusy(false);
    }
  };

  return (
    <div
      data-testid="auth-gate"
      style={{
        width: "100%",
        height: "100%",
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        padding: 24,
        background: "var(--bc-bg-app)",
        overflowY: "auto",
      }}
    >
      <div
        className="bc-card"
        style={{
          width: "100%",
          maxWidth: 460,
          padding: 28,
          background: "var(--bc-bg-surface)",
          border: "1px solid var(--bc-border-strong)",
          borderRadius: "var(--bc-radius-lg)",
          boxShadow: "var(--bc-shadow-win)",
        }}
      >
        <div style={{ display: "flex", alignItems: "center", gap: 12, marginBottom: 18 }}>
          <CompassMark size={32} />
          <div>
            <div style={{ fontWeight: 700, fontSize: 18, letterSpacing: "-0.01em" }}>
              BioCompass Cloud
            </div>
            <div className="bc-caption" style={{ marginTop: 1 }}>
              Verifiable Immunopeptidomics &amp; Froglet Compute Workbench
            </div>
          </div>
        </div>

        <div
          className="bc-segmented-row"
          style={{ marginBottom: 18 }}
          role="tablist"
          aria-label="Authentication mode"
        >
          <button
            type="button"
            role="tab"
            aria-selected={tab === "login"}
            className="bc-segmented-btn"
            data-testid="auth-tab-login"
            data-active={String(tab === "login")}
            onClick={() => switchTab("login")}
          >
            Sign In
          </button>
          <button
            type="button"
            role="tab"
            aria-selected={tab === "register"}
            className="bc-segmented-btn"
            data-testid="auth-tab-register"
            data-active={String(tab === "register")}
            onClick={() => switchTab("register")}
          >
            Start Free Demo
          </button>
          <button
            type="button"
            role="tab"
            aria-selected={tab === "invite"}
            className="bc-segmented-btn"
            data-testid="auth-tab-invite"
            data-active={String(tab === "invite")}
            onClick={() => switchTab("invite")}
          >
            Accept Invite
          </button>
        </div>

        {error && (
          <div
            role="alert"
            data-testid="auth-error-banner"
            style={{
              padding: "10px 12px",
              marginBottom: 14,
              borderRadius: "var(--bc-radius-sm)",
              background: "var(--bc-danger-soft)",
              color: "var(--bc-danger)",
              fontSize: 12.5,
              lineHeight: 1.45,
            }}
          >
            {error}
          </div>
        )}

        {tab === "login" && (
          <form onSubmit={handleLogin} style={{ display: "grid", gap: 12 }}>
            <div>
              <label className="bc-filter-label" htmlFor="auth-login-email">
                Email
              </label>
              <input
                id="auth-login-email"
                data-testid="auth-login-email"
                type="email"
                required
                autoComplete="username"
                className="bc-input"
                style={{ width: "100%" }}
                placeholder="scientist@organization.bio"
                value={loginEmail}
                onChange={(e) => setLoginEmail(e.target.value)}
              />
            </div>

            <div>
              <label className="bc-filter-label" htmlFor="auth-login-password">
                Password
              </label>
              <input
                id="auth-login-password"
                data-testid="auth-login-password"
                type="password"
                required
                autoComplete="current-password"
                className="bc-input"
                style={{ width: "100%" }}
                placeholder="••••••••••••"
                value={loginPasswordVal}
                onChange={(e) => setLoginPasswordVal(e.target.value)}
              />
            </div>

            {(needsTotp || loginTotp.length > 0) && (
              <div>
                <label className="bc-filter-label" htmlFor="auth-login-totp">
                  6-Digit Authenticator Code (TOTP)
                </label>
                <input
                  id="auth-login-totp"
                  data-testid="auth-login-totp"
                  type="text"
                  inputMode="numeric"
                  pattern="[0-9]{6}"
                  maxLength={6}
                  className="bc-input mono"
                  style={{ width: "100%", letterSpacing: "0.2em" }}
                  placeholder="123456"
                  value={loginTotp}
                  onChange={(e) => setLoginTotp(e.target.value)}
                />
              </div>
            )}

            <details className="bc-details">
              <summary className="bc-caption" style={{ cursor: "pointer" }}>
                Advanced options (TOTP / Organization ID)
              </summary>
              <div style={{ display: "grid", gap: 10, marginTop: 8 }}>
                {!needsTotp && (
                  <button
                    type="button"
                    className="bc-btn bc-btn-sm"
                    onClick={() => setNeedsTotp(true)}
                  >
                    Enter 2FA TOTP code
                  </button>
                )}
                <div>
                  <label className="bc-filter-label" htmlFor="auth-login-org">
                    Organization ID (optional)
                  </label>
                  <input
                    id="auth-login-org"
                    type="text"
                    className="bc-input mono"
                    style={{ width: "100%" }}
                    placeholder="org-alithea-cloud"
                    value={loginOrgId}
                    onChange={(e) => setLoginOrgId(e.target.value)}
                  />
                </div>
              </div>
            </details>

            <button
              type="submit"
              className="bc-btn bc-btn-primary"
              data-testid="auth-login-submit"
              disabled={busy}
              style={{ width: "100%", justifyContent: "center", marginTop: 4 }}
            >
              {busy ? "Signing in…" : "Sign In"}
            </button>

            <div
              style={{
                display: "flex",
                alignItems: "center",
                gap: 10,
                margin: "4px 0",
                color: "var(--bc-text-muted)",
                fontSize: 11.5,
              }}
            >
              <div style={{ flex: 1, height: 1, background: "var(--bc-border)" }} />
              <span>OR</span>
              <div style={{ flex: 1, height: 1, background: "var(--bc-border)" }} />
            </div>

            <button
              type="button"
              className="bc-btn"
              data-testid="auth-oidc-btn"
              disabled={busy}
              onClick={handleOidcSso}
              style={{ width: "100%", justifyContent: "center" }}
            >
              <IconShield size={14} />
              <span>Continue with Enterprise SSO (OIDC)</span>
            </button>
          </form>
        )}

        {tab === "register" && (
          <form onSubmit={handleRegisterDemo} style={{ display: "grid", gap: 12 }}>
            <div
              style={{
                padding: "10px 12px",
                borderRadius: "var(--bc-radius-sm)",
                background: "var(--bc-bg-subtle)",
                fontSize: 12,
                color: "var(--bc-text-secondary)",
              }}
            >
              Your demo organization starts with 0 ACT. A platform administrator can add
              compute credits. When available, you can explore the{" "}
              <code className="mono">alithea/hla-public-demo-v1</code> catalog.
            </div>

            <div>
              <label className="bc-filter-label" htmlFor="auth-reg-name">
                Your Name
              </label>
              <input
                id="auth-reg-name"
                data-testid="auth-reg-name"
                type="text"
                className="bc-input"
                style={{ width: "100%" }}
                placeholder="Dr. Ada Lovelace"
                value={regDisplayName}
                onChange={(e) => setRegDisplayName(e.target.value)}
              />
            </div>

            <div>
              <label className="bc-filter-label" htmlFor="auth-reg-org-name">
                Workspace Name (optional)
              </label>
              <input
                id="auth-reg-org-name"
                data-testid="auth-reg-org-name"
                type="text"
                className="bc-input"
                style={{ width: "100%" }}
                placeholder="Translational Immunology Lab"
                value={regOrgName}
                onChange={(e) => setRegOrgName(e.target.value)}
              />
            </div>

            <div>
              <label className="bc-filter-label" htmlFor="auth-reg-email">
                Work Email
              </label>
              <input
                id="auth-reg-email"
                data-testid="auth-reg-email"
                type="email"
                required
                autoComplete="email"
                className="bc-input"
                style={{ width: "100%" }}
                placeholder="ada@biotech.org"
                value={regEmail}
                onChange={(e) => setRegEmail(e.target.value)}
              />
            </div>

            <div>
              <label className="bc-filter-label" htmlFor="auth-reg-password">
                Password (min. 10 characters)
              </label>
              <input
                id="auth-reg-password"
                data-testid="auth-reg-password"
                type="password"
                required
                minLength={10}
                autoComplete="new-password"
                className="bc-input"
                style={{ width: "100%" }}
                placeholder="At least 10 characters"
                value={regPassword}
                onChange={(e) => setRegPassword(e.target.value)}
              />
            </div>

            <button
              type="submit"
              className="bc-btn bc-btn-primary"
              data-testid="auth-reg-submit"
              disabled={busy}
              style={{ width: "100%", justifyContent: "center", marginTop: 4 }}
            >
              {busy ? "Provisioning Demo Workspace…" : "Create Free Demo Workspace"}
            </button>
          </form>
        )}

        {tab === "invite" && (
          <form onSubmit={handleAcceptInvite} style={{ display: "grid", gap: 12 }}>
            <div>
              <label className="bc-filter-label" htmlFor="auth-inv-token">
                Invitation Token
              </label>
              <input
                id="auth-inv-token"
                data-testid="auth-inv-token"
                type="text"
                required
                className="bc-input mono"
                style={{ width: "100%" }}
                placeholder="bci_..."
                value={inviteToken}
                onChange={(e) => setInviteToken(e.target.value)}
              />
            </div>

            <div>
              <label className="bc-filter-label" htmlFor="auth-inv-email">
                Invited Email
              </label>
              <input
                id="auth-inv-email"
                data-testid="auth-inv-email"
                type="email"
                required
                className="bc-input"
                style={{ width: "100%" }}
                placeholder="invited@partner.bio"
                value={inviteEmail}
                onChange={(e) => setInviteEmail(e.target.value)}
              />
            </div>

            <div>
              <label className="bc-filter-label" htmlFor="auth-inv-name">
                Display Name
              </label>
              <input
                id="auth-inv-name"
                type="text"
                className="bc-input"
                style={{ width: "100%" }}
                placeholder="Your Name"
                value={inviteDisplayName}
                onChange={(e) => setInviteDisplayName(e.target.value)}
              />
            </div>

            <div>
              <label className="bc-filter-label" htmlFor="auth-inv-password">
                Account password (at least 10 characters for a new account)
              </label>
              <input
                id="auth-inv-password"
                data-testid="auth-inv-password"
                type="password"
                minLength={10}
                className="bc-input"
                style={{ width: "100%" }}
                placeholder="••••••••••••"
                value={invitePassword}
                onChange={(e) => setInvitePassword(e.target.value)}
              />
            </div>

            {inviteNeedsTotp && <label className="bc-filter-label">Authenticator code
              <input className="bc-input" inputMode="numeric" autoComplete="one-time-code" maxLength={6}
                value={inviteTotp} onChange={e => setInviteTotp(e.target.value)} />
            </label>}

            <button
              type="submit"
              className="bc-btn bc-btn-primary"
              data-testid="auth-inv-submit"
              disabled={busy}
              style={{ width: "100%", justifyContent: "center", marginTop: 4 }}
            >
              {busy ? "Joining Organization…" : "Accept Invitation & Sign In"}
            </button>
          </form>
        )}
      </div>
    </div>
  );
}
