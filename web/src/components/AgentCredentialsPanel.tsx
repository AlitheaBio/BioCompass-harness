import React, { useCallback, useEffect, useState } from "react";
import {
  createAgentCredential,
  fetchAgentCredentials,
  formatErrorInline,
  revokeAgentCredential,
} from "../api";
import {
  formatMilliAct,
  type AgentCredentialRecord,
  type AgentCredentialRole,
  type CreatedAgentCredential,
} from "../types";
import { IconCheck, IconCopy, IconShield, IconTrash } from "./Icons";

function parseCsvAllowlist(raw: string): string[] | null {
  const trimmed = raw.trim();
  if (!trimmed) return null;
  return trimmed
    .split(/[\s,]+/)
    .map((s) => s.trim())
    .filter(Boolean);
}

function parseEurToMilli(raw: string): number | null {
  const trimmed = raw.trim();
  if (!trimmed) return null;
  const parsed = Number(trimmed);
  if (!Number.isFinite(parsed) || parsed < 0) return null;
  return Math.round(parsed * 1000);
}

/**
 * Milestone C1 (H1) — Admin-issued Agent Credentials (`bc_agent_...`) with role capped <= developer,
 * expiry, catalog/module allow-lists, and ledger-enforced daily + total EUR spending ceilings.
 */
export function AgentCredentialsPanel({
  catalogOptions = [],
  moduleOptions = [],
}: {
  catalogOptions?: string[];
  moduleOptions?: string[];
}) {
  const [credentials, setCredentials] = useState<AgentCredentialRecord[]>([]);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [statusMsg, setStatusMsg] = useState<string | null>(null);
  const [created, setCreated] = useState<CreatedAgentCredential | null>(null);
  const [copied, setCopied] = useState(false);

  // Create form state
  const [label, setLabel] = useState("");
  const [role, setRole] = useState<AgentCredentialRole>("developer");
  const [expiresInDays, setExpiresInDays] = useState("30");
  const [allowedCatalogs, setAllowedCatalogs] = useState("");
  const [allowedModules, setAllowedModules] = useState("");
  const [dailyCapEur, setDailyCapEur] = useState("5.00");
  const [totalCapEur, setTotalCapEur] = useState("50.00");
  const [burstPerMinute, setBurstPerMinute] = useState("60");

  const refresh = useCallback(async () => {
    setError(null);
    try {
      setCredentials(await fetchAgentCredentials());
    } catch (err: unknown) {
      setError(formatErrorInline(err));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const handleCreate = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!label.trim()) return;
    setBusy(true);
    setError(null);
    setStatusMsg(null);
    try {
      const res = await createAgentCredential({
        label: label.trim(),
        role,
        expires_in_days: Math.max(1, Number.parseInt(expiresInDays, 10) || 30),
        allowed_catalogs: parseCsvAllowlist(allowedCatalogs),
        allowed_modules: parseCsvAllowlist(allowedModules),
        daily_ceiling_milli_act: parseEurToMilli(dailyCapEur),
        total_ceiling_milli_act: parseEurToMilli(totalCapEur),
        burst_per_minute: Math.max(1, Number.parseInt(burstPerMinute, 10) || 60),
      });
      setCreated(res);
      setLabel("");
      setStatusMsg(`Issued agent credential "${res.credential.label}". Copy the bearer token now.`);
      await refresh();
    } catch (err: unknown) {
      setError(formatErrorInline(err));
    } finally {
      setBusy(false);
    }
  };

  const handleRevoke = async (credentialId: string, credLabel: string) => {
    setBusy(true);
    setError(null);
    setStatusMsg(null);
    try {
      await revokeAgentCredential(credentialId);
      setStatusMsg(`Revoked agent credential "${credLabel}".`);
      await refresh();
    } catch (err: unknown) {
      setError(formatErrorInline(err));
    } finally {
      setBusy(false);
    }
  };

  const copyToken = (token: string) => {
    void navigator.clipboard?.writeText(token);
    setCopied(true);
    window.setTimeout(() => setCopied(false), 1800);
  };

  return (
    <div className="bc-settings-stack" data-testid="agent-credentials-panel">
      {error && (
        <div role="alert" className="bc-inline-alert bc-inline-alert-error">
          {error}
        </div>
      )}
      {statusMsg && (
        <div role="status" className="bc-inline-alert">
          {statusMsg}
        </div>
      )}

      {created && (
        <div
          className="bc-card"
          data-testid="agent-credential-created-banner"
          style={{ border: "1px solid var(--bc-accent)" }}
        >
          <div className="bc-card-header">
            <span className="bc-card-title">New Agent Bearer Token (shown once)</span>
            <button
              type="button"
              className="bc-btn bc-btn-secondary bc-btn-sm"
              onClick={() => copyToken(created.raw_token)}
            >
              {copied ? <IconCheck size={13} /> : <IconCopy size={13} />}
              <span>{copied ? "Copied" : "Copy token"}</span>
            </button>
          </div>
          <p className="bc-caption" style={{ marginTop: 4 }}>
            Use with <code>Authorization: Bearer &lt;token&gt;</code> against <code>/api/v1/mcp</code> or REST agent endpoints.
          </p>
          <pre className="bc-code-pre mono" data-testid="agent-credential-raw-token">
            {created.raw_token}
          </pre>
        </div>
      )}

      <section className="bc-card">
        <div className="bc-card-header">
          <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
            <IconShield size={15} />
            <span className="bc-card-title">Issue Agent Credential</span>
          </div>
          <span className="bc-caption">Role strictly capped at developer · ledger-enforced EUR ceilings</span>
        </div>
        <form className="bc-form-grid" style={{ marginTop: 12 }} onSubmit={(e) => void handleCreate(e)}>
          <label className="bc-field">
            <span className="bc-field-label">Credential label</span>
            <input
              className="bc-input"
              data-testid="agent-cred-label-input"
              placeholder="e.g. Research MCP Agent"
              required
              value={label}
              onChange={(e) => setLabel(e.target.value)}
            />
          </label>
          <label className="bc-field">
            <span className="bc-field-label">Org role (max developer)</span>
            <select
              className="bc-input"
              data-testid="agent-cred-role-select"
              value={role}
              onChange={(e) => setRole(e.target.value as AgentCredentialRole)}
            >
              <option value="developer">developer</option>
              <option value="member">member</option>
              <option value="viewer">viewer</option>
            </select>
          </label>
          <label className="bc-field">
            <span className="bc-field-label">Expires in (days)</span>
            <input
              type="number"
              min={1}
              max={365}
              className="bc-input mono"
              data-testid="agent-cred-expiry-input"
              value={expiresInDays}
              onChange={(e) => setExpiresInDays(e.target.value)}
            />
          </label>
          <label className="bc-field">
            <span className="bc-field-label">Daily ceiling (EUR)</span>
            <input
              type="number"
              step="0.01"
              min="0"
              className="bc-input mono"
              data-testid="agent-cred-daily-cap-input"
              placeholder="Empty = unlimited"
              value={dailyCapEur}
              onChange={(e) => setDailyCapEur(e.target.value)}
            />
          </label>
          <label className="bc-field">
            <span className="bc-field-label">Total ceiling (EUR)</span>
            <input
              type="number"
              step="0.01"
              min="0"
              className="bc-input mono"
              data-testid="agent-cred-total-cap-input"
              placeholder="Empty = unlimited"
              value={totalCapEur}
              onChange={(e) => setTotalCapEur(e.target.value)}
            />
          </label>
          <label className="bc-field">
            <span className="bc-field-label">Burst limit (req/min)</span>
            <input
              type="number"
              min={1}
              className="bc-input mono"
              value={burstPerMinute}
              onChange={(e) => setBurstPerMinute(e.target.value)}
            />
          </label>
          <label className="bc-field" style={{ gridColumn: "1 / -1" }}>
            <span className="bc-field-label">Allowed catalogs (comma-separated, blank = all org catalogs)</span>
            <input
              className="bc-input mono"
              data-testid="agent-cred-catalogs-input"
              placeholder={catalogOptions.slice(0, 2).join(", ") || "alithea/hla-atlas-v2026.1"}
              value={allowedCatalogs}
              onChange={(e) => setAllowedCatalogs(e.target.value)}
            />
          </label>
          <label className="bc-field" style={{ gridColumn: "1 / -1" }}>
            <span className="bc-field-label">Allowed modules (comma-separated, blank = all org modules)</span>
            <input
              className="bc-input mono"
              data-testid="agent-cred-modules-input"
              placeholder={moduleOptions.slice(0, 2).join(", ") || "alithea/hla-motif-enrichment"}
              value={allowedModules}
              onChange={(e) => setAllowedModules(e.target.value)}
            />
          </label>
          <div>
            <button
              type="submit"
              className="bc-btn bc-btn-primary"
              data-testid="agent-cred-create-btn"
              disabled={busy || !label.trim()}
            >
              {busy ? "Issuing…" : "Issue Agent Credential"}
            </button>
          </div>
        </form>
      </section>

      <section className="bc-card">
        <div className="bc-card-header">
          <span className="bc-card-title">Issued Agent Credentials</span>
          <button type="button" className="bc-btn bc-btn-ghost bc-btn-sm" onClick={() => void refresh()}>
            Refresh
          </button>
        </div>
        {loading ? (
          <p className="bc-empty-note">Loading agent credentials…</p>
        ) : credentials.length === 0 ? (
          <p className="bc-empty-note">No agent credentials issued for this organization yet.</p>
        ) : (
          <div className="table-scroll">
            <table className="data-table">
              <thead>
                <tr>
                  <th>Label &amp; Prefix</th>
                  <th>Role</th>
                  <th>Allow-lists</th>
                  <th>Daily / Total EUR Cap</th>
                  <th>Spent</th>
                  <th>Expires</th>
                  <th>Status</th>
                  <th>Action</th>
                </tr>
              </thead>
              <tbody>
                {credentials.map((cred) => {
                  const expired = Date.parse(cred.expires_at) <= Date.now();
                  const statusLabel = cred.revoked_at ? "Revoked" : expired ? "Expired" : "Active";
                  const badgeClass = cred.revoked_at
                    ? "bc-badge-danger"
                    : expired
                      ? "bc-badge-warn"
                      : "bc-badge-success";
                  return (
                    <tr key={cred.credential_id} data-testid={`agent-cred-row-${cred.credential_id}`}>
                      <td>
                        <strong>{cred.label}</strong>
                        <div className="bc-caption mono">{cred.token_prefix}…</div>
                      </td>
                      <td className="mono">{cred.role}</td>
                      <td className="bc-caption mono">
                        <div>Cat: {cred.allowed_catalogs ? cred.allowed_catalogs.join(", ") || "none" : "all"}</div>
                        <div>Mod: {cred.allowed_modules ? cred.allowed_modules.join(", ") || "none" : "all"}</div>
                      </td>
                      <td className="mono">
                        {cred.daily_ceiling_milli_act != null ? formatMilliAct(cred.daily_ceiling_milli_act) : "∞"} /{" "}
                        {cred.total_ceiling_milli_act != null ? formatMilliAct(cred.total_ceiling_milli_act) : "∞"}
                      </td>
                      <td className="mono">{formatMilliAct(cred.spent_total_milli_act)}</td>
                      <td className="bc-caption mono">{new Date(cred.expires_at).toLocaleDateString()}</td>
                      <td>
                        <span className={`bc-badge ${badgeClass}`}>{statusLabel}</span>
                      </td>
                      <td>
                        {!cred.revoked_at && (
                          <button
                            type="button"
                            className="bc-btn bc-btn-ghost bc-btn-sm"
                            data-testid={`agent-cred-revoke-${cred.credential_id}`}
                            disabled={busy}
                            onClick={() => void handleRevoke(cred.credential_id, cred.label)}
                          >
                            <IconTrash size={13} />
                            <span>Revoke</span>
                          </button>
                        )}
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        )}
      </section>
    </div>
  );
}
