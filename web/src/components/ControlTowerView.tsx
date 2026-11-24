import React, { useCallback, useEffect, useState } from "react";
import {
  approveCuratedModule,
  creditOrgInvoice,
  enrollNode,
  fetchAdminPluginVersions,
  fetchCuratedModuleApprovals,
  fetchEnumerationMonitor,
  fetchNodes,
  formatErrorInline,
  reviewAdminPluginVisibility,
  revokeCuratedModule,
  revokeNode,
} from "../api";
import {
  formatMilliEur,
  type AuthPrincipal,
  type CreatedNodeEnrollment,
  type CuratedModuleApprovalRecord,
  type EnumerationMonitorEntry,
  type NodeMode,
  type NodeSummaryRecord,
  type PluginVersionRecord,
  type PluginVisibility,
} from "../types";
import {
  IconCheck,
  IconCopy,
  IconFrogletNode,
  IconShield,
  IconTrash,
} from "./Icons";

export interface ControlTowerViewProps {
  principal: AuthPrincipal;
  onRefreshWorkspace: () => Promise<void>;
}

type ControlTowerTab =
  | "curated"
  | "plugins"
  | "nodes"
  | "billing"
  | "monitor";

export function ControlTowerView({
  principal,
  onRefreshWorkspace,
}: ControlTowerViewProps) {
  const [tab, setTab] = useState<ControlTowerTab>("curated");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [loadErrors, setLoadErrors] = useState<Partial<Record<ControlTowerTab, string>>>({});
  const [statusMsg, setStatusMsg] = useState<string | null>(null);
  const [copiedId, setCopiedId] = useState<string | null>(null);

  const [curatedApprovals, setCuratedApprovals] = useState<CuratedModuleApprovalRecord[]>([]);
  const [pluginVersions, setPluginVersions] = useState<PluginVersionRecord[]>([]);
  const [pluginVisFilter, setPluginVisFilter] = useState<string>("all");
  const [approveAsCurated, setApproveAsCurated] = useState<boolean>(true);
  const [nodes, setNodes] = useState<NodeSummaryRecord[]>([]);
  const [enumEntries, setEnumEntries] = useState<EnumerationMonitorEntry[]>([]);
  const [enumThreshold, setEnumThreshold] = useState<number>(30);

  // Curated form
  const [curPluginId, setCurPluginId] = useState("");
  const [curVersion, setCurVersion] = useState("0.1.0");
  const [curDigest, setCurDigest] = useState("");
  const [curNotes, setCurNotes] = useState("Platform security review passed");

  // Node enrollment form
  const [newNodeId, setNewNodeId] = useState("");
  const [newNodeMode, setNewNodeMode] = useState<NodeMode>("cloud-pool");
  const [newNodeLabel, setNewNodeLabel] = useState("");
  const [createdNode, setCreatedNode] = useState<CreatedNodeEnrollment | null>(null);

  // Invoice credit form
  const [creditOrgId, setCreditOrgId] = useState("");
  const [creditAmountEur, setCreditAmountEur] = useState("100");
  const [creditReference, setCreditReference] = useState("INV-2026-001");

  const copyToClipboard = (id: string, text: string) => {
    void navigator.clipboard?.writeText(text);
    setCopiedId(id);
    window.setTimeout(() => {
      setCopiedId((prev) => (prev === id ? null : prev));
    }, 1600);
  };

  const loadAll = useCallback(async () => {
    const errors: Partial<Record<ControlTowerTab, string>> = {};
    async function loadSection<T>(section: ControlTowerTab, read: Promise<T>, apply: (data: T) => void) {
      try {
        apply(await read);
      } catch (err) {
        errors[section] = formatErrorInline(err);
      }
    }
    await Promise.all([
      loadSection("curated", fetchCuratedModuleApprovals(), setCuratedApprovals),
      loadSection("plugins", fetchAdminPluginVersions(), setPluginVersions),
      loadSection("nodes", fetchNodes(), setNodes),
      loadSection("monitor", fetchEnumerationMonitor(enumThreshold), data => setEnumEntries(data.entries)),
    ]);
    setLoadErrors(errors);
  }, [enumThreshold]);

  useEffect(() => {
    void loadAll();
  }, [loadAll]);

  const handleApproveCurated = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!curPluginId.trim() || !curVersion.trim() || !curDigest.trim()) return;
    setBusy(true);
    setError(null);
    setStatusMsg(null);
    try {
      await approveCuratedModule({
        plugin_id: curPluginId.trim(),
        version: curVersion.trim(),
        image_digest: curDigest.trim(),
      });
      setCurPluginId("");
      setCurDigest("");
      setStatusMsg(`Approved curated digest for ${curPluginId}@${curVersion}.`);
      await Promise.all([loadAll(), onRefreshWorkspace()]);
    } catch (err: unknown) {
      setError(formatErrorInline(err));
    } finally {
      setBusy(false);
    }
  };

  const handleRevokeCurated = async (rec: CuratedModuleApprovalRecord) => {
    setBusy(true);
    setError(null);
    try {
      await revokeCuratedModule({
        plugin_id: rec.plugin_id,
        version: rec.version,
        image_digest: rec.image_digest,
      });
      setStatusMsg(`Revoked curated approval for ${rec.plugin_id}@${rec.version}.`);
      await Promise.all([loadAll(), onRefreshWorkspace()]);
    } catch (err: unknown) {
      setError(formatErrorInline(err));
    } finally {
      setBusy(false);
    }
  };

  const handleReviewPlugin = async (
    rec: PluginVersionRecord,
    visibility: PluginVisibility,
  ) => {
    setBusy(true);
    setError(null);
    setStatusMsg(null);
    try {
      await reviewAdminPluginVisibility(rec.plugin_id, rec.version, {
        visibility,
        approve_curated: visibility === "public_approved" ? approveAsCurated : false,
      });
      setStatusMsg(`Set ${rec.plugin_id}@${rec.version} visibility to ${visibility}.`);
      await Promise.all([loadAll(), onRefreshWorkspace()]);
    } catch (err: unknown) {
      setError(formatErrorInline(err));
    } finally {
      setBusy(false);
    }
  };

  const handleEnrollFleetNode = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!newNodeId.trim()) return;
    setBusy(true);
    setError(null);
    setStatusMsg(null);
    try {
      const res = await enrollNode({
        node_id: newNodeId.trim(),
        mode: newNodeMode,
        provider_pubkey:
          newNodeLabel.trim().length === 64 ? newNodeLabel.trim() : "a".repeat(64),
      });
      setCreatedNode(res);
      setNewNodeId("");
      setNewNodeLabel("");
      setStatusMsg(`Enrolled ${res.mode} worker node ${res.node_id}.`);
      await loadAll();
    } catch (err: unknown) {
      setError(formatErrorInline(err));
    } finally {
      setBusy(false);
    }
  };

  const handleRevokeFleetNode = async (nodeId: string) => {
    setBusy(true);
    setError(null);
    try {
      await revokeNode(nodeId);
      setStatusMsg(`Revoked worker node ${nodeId}.`);
      await loadAll();
    } catch (err: unknown) {
      setError(formatErrorInline(err));
    } finally {
      setBusy(false);
    }
  };

  const handleCreditInvoice = async (e: React.FormEvent) => {
    e.preventDefault();
    const amount = Number(creditAmountEur);
    if (!creditOrgId.trim() || !Number.isFinite(amount) || amount <= 0) return;
    setBusy(true);
    setError(null);
    setStatusMsg(null);
    try {
      const res = await creditOrgInvoice({
        org_id: creditOrgId.trim(),
        amount_milli_eur: Math.round(amount * 1000),
        reference: creditReference.trim() || `INV-${Date.now()}`,
      });
      setStatusMsg(
        `Credited ${formatMilliEur(res.delta_milli_eur)} to ${res.org_id} (new balance: ${formatMilliEur(res.balance_after_milli_eur)}).`,
      );
      await onRefreshWorkspace();
    } catch (err: unknown) {
      setError(formatErrorInline(err));
    } finally {
      setBusy(false);
    }
  };

  const filteredPluginVersions =
    pluginVisFilter === "all"
      ? pluginVersions
      : pluginVersions.filter((p) => p.visibility === pluginVisFilter);

  const navTabs: { id: ControlTowerTab; label: string }[] = [
    { id: "curated", label: `Curated Digests (${curatedApprovals.length})` },
    { id: "plugins", label: `Plugin Review (${pluginVersions.length})` },
    { id: "nodes", label: `Worker Fleet (${nodes.length})` },
    { id: "billing", label: "Invoice EUR Balance" },
    { id: "monitor", label: `Enumeration Monitor (${enumEntries.length})` },
  ];

  const visibleError = error ?? loadErrors[tab];

  return (
    <div className="bc-page-scroll" data-testid="control-tower-view">
      <div className="bc-page-container">
        <div className="bc-page-header">
          <div>
            <h1 className="bc-page-title">Platform Control Tower</h1>
            <p className="bc-page-subtitle">
              Platform Administrator (<span className="mono">{principal.email ?? principal.actor}</span>
              ) · Curated Digest Approvals, Plugin Review, Worker Fleet &amp; Security Monitors
            </p>
          </div>
        </div>

        <div className="bc-module-filter-bar" style={{ marginBottom: 16 }}>
          <div className="bc-filter-pills" role="tablist">
            {navTabs.map((item) => (
              <button
                key={item.id}
                type="button"
                role="tab"
                aria-selected={tab === item.id}
                data-testid={`ct-tab-${item.id}`}
                data-active={String(tab === item.id)}
                className="bc-filter-pill"
                onClick={() => {
                  setTab(item.id);
                  setError(null);
                  setStatusMsg(null);
                }}
              >
                <span>{item.label}</span>
              </button>
            ))}
          </div>
        </div>

        {visibleError && (
          <div
            role="alert"
            style={{
              marginBottom: 12,
              padding: "10px 14px",
              borderRadius: "var(--bc-radius-sm)",
              background: "var(--bc-danger-soft)",
              color: "var(--bc-danger)",
              fontSize: 13,
            }}
          >
            {visibleError}
          </div>
        )}

        {statusMsg && (
          <div
            data-testid="control-tower-status"
            style={{
              marginBottom: 12,
              padding: "10px 14px",
              borderRadius: "var(--bc-radius-sm)",
              background: "var(--bc-bg-subtle)",
              border: "1px solid var(--bc-border)",
              fontSize: 13,
            }}
          >
            {statusMsg}
          </div>
        )}

        {tab === "curated" && (
          <div className="bc-settings-stack">
            <div className="bc-card">
              <div className="bc-card-header">
                <div>
                  <span className="bc-card-title">Approve Curated Module Image Digest</span>
                  <p className="bc-caption" style={{ marginTop: 2 }}>
                    Only exact <code className="mono">(plugin_id, version, image_digest)</code>{" "}
                    tuples listed here may execute against <code className="mono">compute_only</code>{" "}
                    catalogs.
                  </p>
                </div>
              </div>

              <form
                onSubmit={(e) => void handleApproveCurated(e)}
                className="bc-form-grid"
                style={{ marginTop: 10 }}
              >
                <label className="bc-field">
                  <span className="bc-field-label">Plugin ID</span>
                  <input
                    type="text"
                    required
                    data-testid="curated-plugin-id-input"
                    className="bc-input mono"
                    placeholder="alithea/bc-target-validation"
                    value={curPluginId}
                    onChange={(e) => setCurPluginId(e.target.value)}
                  />
                </label>

                <label className="bc-field">
                  <span className="bc-field-label">Version</span>
                  <input
                    type="text"
                    required
                    data-testid="curated-version-input"
                    className="bc-input mono"
                    value={curVersion}
                    onChange={(e) => setCurVersion(e.target.value)}
                  />
                </label>

                <label className="bc-field" style={{ gridColumn: "1 / -1" }}>
                  <span className="bc-field-label">Pinned OCI Image Digest (sha256:…)</span>
                  <input
                    type="text"
                    required
                    data-testid="curated-digest-input"
                    className="bc-input mono"
                    placeholder="sha256:..."
                    value={curDigest}
                    onChange={(e) => setCurDigest(e.target.value)}
                  />
                </label>

                <label className="bc-field" style={{ gridColumn: "1 / -1" }}>
                  <span className="bc-field-label">Audit Review Notes</span>
                  <input
                    type="text"
                    className="bc-input"
                    value={curNotes}
                    onChange={(e) => setCurNotes(e.target.value)}
                  />
                </label>

                <div style={{ gridColumn: "1 / -1" }}>
                  <button
                    type="submit"
                    data-testid="approve-curated-submit"
                    className="bc-btn bc-btn-primary"
                    disabled={busy}
                  >
                    <IconShield size={14} />
                    <span>Approve Digest</span>
                  </button>
                </div>
              </form>
            </div>

            <div className="bc-card" data-testid="curated-approvals-table">
              <div className="bc-card-header" style={{ marginBottom: 8 }}>
                <span className="bc-card-title">
                  Approved Curated Module Digests ({curatedApprovals.length})
                </span>
              </div>
              <div className="table-scroll">
                <table className="data-table">
                  <thead>
                    <tr>
                      <th>Plugin</th>
                      <th>Version</th>
                      <th>Image Digest</th>
                      <th>Approved At</th>
                      <th />
                    </tr>
                  </thead>
                  <tbody>
                    {curatedApprovals.map((rec) => (
                      <tr key={`${rec.plugin_id}@${rec.version}:${rec.image_digest}`}>
                        <td className="mono">{rec.plugin_id}</td>
                        <td className="mono">{rec.version}</td>
                        <td className="mono bc-caption">
                          {rec.image_digest.slice(0, 24)}…
                        </td>
                        <td className="mono bc-caption">
                          {new Date(rec.created_at).toLocaleDateString()}
                        </td>
                        <td style={{ textAlign: "right" }}>
                          <button
                            type="button"
                            className="bc-btn bc-btn-sm"
                            disabled={busy}
                            onClick={() => void handleRevokeCurated(rec)}
                          >
                            <IconTrash size={12} />
                            <span>Revoke</span>
                          </button>
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            </div>
          </div>
        )}

        {tab === "plugins" && (
          <div className="bc-card" data-testid="plugin-review-queue">
            <div className="bc-card-header" style={{ marginBottom: 10 }}>
              <div>
                <span className="bc-card-title">Published Plugin Versions &amp; Review Queue</span>
                <p className="bc-caption" style={{ marginTop: 2 }}>
                  Review developer submissions before exposing them in the public marketplace.
                </p>
              </div>
              <div style={{ display: "flex", gap: 10, alignItems: "center", flexWrap: "wrap" }}>
                <label style={{ display: "inline-flex", alignItems: "center", gap: 6, fontSize: 12 }}>
                  <input
                    type="checkbox"
                    checked={approveAsCurated}
                    onChange={(e) => setApproveAsCurated(e.target.checked)}
                  />
                  <span>Also approve digest as Curated</span>
                </label>
                <select
                  className="bc-input mono"
                  style={{ width: "auto" }}
                  value={pluginVisFilter}
                  onChange={(e) => setPluginVisFilter(e.target.value)}
                >
                  <option value="all">all</option>
                  <option value="public_pending_review">public_pending_review</option>
                  <option value="public_approved">public_approved</option>
                  <option value="org_private">org_private</option>
                  <option value="rejected">rejected</option>
                </select>
              </div>
            </div>

            {filteredPluginVersions.length === 0 ? (
              <div className="bc-empty-note">No plugin versions match the selected filter.</div>
            ) : (
              <div className="table-scroll">
                <table className="data-table">
                  <thead>
                    <tr>
                      <th>Plugin</th>
                      <th>Version</th>
                      <th>Owner Org</th>
                      <th>Visibility</th>
                      <th>Curated</th>
                      <th style={{ textAlign: "right" }}>Review</th>
                    </tr>
                  </thead>
                  <tbody>
                    {filteredPluginVersions.map((pv) => (
                      <tr key={`${pv.plugin_id}@${pv.version}`}>
                        <td className="mono">{pv.plugin_id}</td>
                        <td className="mono">{pv.version}</td>
                        <td className="mono">{pv.org_id}</td>
                        <td>
                          <span className="bc-badge mono">{pv.visibility}</span>
                        </td>
                        <td>
                          {pv.is_curated_approved ? (
                            <span className="bc-badge bc-badge-success">Approved</span>
                          ) : (
                            <span className="bc-caption">No</span>
                          )}
                        </td>
                        <td
                          style={{
                            textAlign: "right",
                            display: "flex",
                            gap: 6,
                            justifyContent: "flex-end",
                          }}
                        >
                          {pv.visibility !== "public_approved" && (
                            <button
                              type="button"
                              data-testid={`approve-plugin-${pv.plugin_id}-${pv.version}`}
                              className="bc-btn bc-btn-primary bc-btn-sm"
                              disabled={busy}
                              onClick={() =>
                                void handleReviewPlugin(pv, "public_approved")
                              }
                            >
                              Approve Public
                            </button>
                          )}
                          {pv.visibility !== "rejected" && (
                            <button
                              type="button"
                              className="bc-btn bc-btn-sm"
                              disabled={busy}
                              onClick={() => void handleReviewPlugin(pv, "rejected")}
                            >
                              Reject
                            </button>
                          )}
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )}
          </div>
        )}

        {tab === "nodes" && (
          <div className="bc-settings-stack">
            <div className="bc-card">
              <div className="bc-card-header">
                <span className="bc-card-title">
                  Enroll Cloud-Pool Worker Node
                </span>
              </div>
              <form
                onSubmit={(e) => void handleEnrollFleetNode(e)}
                className="bc-form-grid"
                style={{ marginTop: 10 }}
              >
                <label className="bc-field">
                  <span className="bc-field-label">Node ID</span>
                  <input
                    type="text"
                    required
                    data-testid="ct-enroll-node-id"
                    className="bc-input mono"
                    placeholder="cloud-worker-02"
                    value={newNodeId}
                    onChange={(e) => setNewNodeId(e.target.value)}
                  />
                </label>

                <label className="bc-field">
                  <span className="bc-field-label">Mode</span>
                  <select
                    data-testid="ct-enroll-node-mode"
                    className="bc-input mono"
                    value={newNodeMode}
                    onChange={(e) => setNewNodeMode(e.target.value as NodeMode)}
                  >
                    <option value="cloud-pool">cloud-pool</option>
                  </select>
                </label>

                <label className="bc-field">
                  <span className="bc-field-label">Provider Pubkey (64-hex, optional)</span>
                  <input
                    type="text"
                    className="bc-input mono"
                    placeholder="64-character BIP340 Schnorr pubkey"
                    value={newNodeLabel}
                    onChange={(e) => setNewNodeLabel(e.target.value)}
                  />
                </label>

                <div style={{ gridColumn: "1 / -1" }}>
                  <button
                    type="submit"
                    data-testid="ct-enroll-node-submit"
                    className="bc-btn bc-btn-primary"
                    disabled={busy}
                  >
                    <IconFrogletNode size={14} />
                    <span>Enroll Worker Node</span>
                  </button>
                </div>
              </form>

              {createdNode && (
                <div
                  style={{
                    marginTop: 12,
                    padding: "10px 12px",
                    borderRadius: "var(--bc-radius-sm)",
                    background: "var(--bc-bg-subtle)",
                    border: "1px solid var(--bc-accent)",
                  }}
                >
                  <div className="bc-caption" style={{ marginBottom: 4 }}>
                    One-time enrolled node credential (<code className="mono">bcn_…</code>):
                  </div>
                  <div style={{ display: "flex", gap: 8, alignItems: "center" }}>
                    <code className="mono" style={{ wordBreak: "break-all" }}>
                      {createdNode.raw_credential}
                    </code>
                    <button
                      type="button"
                      className="bc-btn bc-btn-sm"
                      onClick={() => copyToClipboard("ctnode", createdNode.raw_credential)}
                    >
                      {copiedId === "ctnode" ? <IconCheck size={12} /> : <IconCopy size={12} />}
                      <span>{copiedId === "ctnode" ? "Copied" : "Copy"}</span>
                    </button>
                  </div>
                </div>
              )}
            </div>

            <div className="bc-card" data-testid="ct-worker-nodes-table">
              <div className="bc-card-header" style={{ marginBottom: 8 }}>
                <span className="bc-card-title">Enrolled Worker Fleet ({nodes.length})</span>
              </div>
              <div className="table-scroll">
                <table className="data-table">
                  <thead>
                    <tr>
                      <th>Node ID</th>
                      <th>Mode</th>
                      <th>Owner Org</th>
                      <th>Status</th>
                      <th>Schnorr Pubkey</th>
                      <th />
                    </tr>
                  </thead>
                  <tbody>
                    {nodes.map((n) => (
                      <tr key={n.node_id}>
                        <td className="mono">{n.node_id}</td>
                        <td className="mono">{n.mode}</td>
                        <td className="mono">{n.org_id ?? "platform"}</td>
                        <td>
                          <span
                            className={`bc-badge ${
                              n.online ? "bc-badge-success" : "bc-badge-neutral"
                            }`}
                          >
                            {n.online ? "online" : n.status}
                          </span>
                        </td>
                        <td className="mono bc-caption">
                          {n.provider_pubkey ? `${n.provider_pubkey.slice(0, 16)}…` : "—"}
                        </td>
                        <td style={{ textAlign: "right" }}>
                          {n.status !== "revoked" && (
                            <button
                              type="button"
                              className="bc-btn bc-btn-sm"
                              disabled={busy}
                              onClick={() => void handleRevokeFleetNode(n.node_id)}
                            >
                              <IconTrash size={12} />
                              <span>Revoke</span>
                            </button>
                          )}
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            </div>
          </div>
        )}

        {tab === "billing" && (
          <div className="bc-card">
            <div className="bc-card-header">
              <div>
                <span className="bc-card-title">Invoice EUR Credit Settlement</span>
                <p className="bc-caption" style={{ marginTop: 2 }}>
                  Credits paid customer invoices directly to an organization&apos;s EUR ledger with
                  an immutable audit log entry.
                </p>
              </div>
            </div>
            <form
              onSubmit={(e) => void handleCreditInvoice(e)}
              className="bc-form-grid"
              style={{ marginTop: 12 }}
            >
              <label className="bc-field">
                <span className="bc-field-label">Organization ID</span>
                <input
                  type="text"
                  required
                  data-testid="invoice-org-id-input"
                  className="bc-input mono"
                  placeholder="partner-biotech"
                  value={creditOrgId}
                  onChange={(e) => setCreditOrgId(e.target.value)}
                />
              </label>

              <label className="bc-field">
                <span className="bc-field-label">Amount (EUR)</span>
                <input
                  type="number"
                  step="0.01"
                  min="0.01"
                  required
                  data-testid="invoice-amount-eur-input"
                  className="bc-input mono"
                  value={creditAmountEur}
                  onChange={(e) => setCreditAmountEur(e.target.value)}
                />
              </label>

              <label className="bc-field" style={{ gridColumn: "1 / -1" }}>
                <span className="bc-field-label">Invoice / Wire Reference</span>
                <input
                  type="text"
                  required
                  data-testid="invoice-reference-input"
                  className="bc-input mono"
                  value={creditReference}
                  onChange={(e) => setCreditReference(e.target.value)}
                />
              </label>

              <div style={{ gridColumn: "1 / -1" }}>
                <button
                  type="submit"
                  data-testid="invoice-credit-submit"
                  className="bc-btn bc-btn-primary"
                  disabled={busy}
                >
                  Credit Organization Ledger
                </button>
              </div>
            </form>
          </div>
        )}

        {tab === "monitor" && (
          <div className="bc-card" data-testid="enumeration-monitor-table">
            <div className="bc-card-header" style={{ marginBottom: 10 }}>
              <div>
                <span className="bc-card-title">
                  Input-Cardinality Enumeration Monitor (24h Rolling / Daily Buckets)
                </span>
                <p className="bc-caption" style={{ marginTop: 2 }}>
                  Flags anomalous distinct-input sweeps against{" "}
                  <code className="mono">compute_only</code> catalogs for manual review.
                </p>
              </div>
              <label style={{ display: "flex", alignItems: "center", gap: 6, fontSize: 12 }}>
                <span>Flag threshold:</span>
                <input
                  type="number"
                  min={1}
                  className="bc-input mono"
                  style={{ width: 76, padding: "4px 8px" }}
                  value={enumThreshold}
                  onChange={(e) => setEnumThreshold(Number(e.target.value) || 30)}
                />
              </label>
            </div>

            {enumEntries.length === 0 ? (
              <div className="bc-empty-note">No module execution cardinality buckets recorded yet.</div>
            ) : (
              <div className="table-scroll">
                <table className="data-table">
                  <thead>
                    <tr>
                      <th>Day (UTC)</th>
                      <th>Org ID</th>
                      <th>Plugin</th>
                      <th>Total Runs</th>
                      <th>compute_only Runs</th>
                      <th>Distinct Inputs</th>
                      <th>Status</th>
                    </tr>
                  </thead>
                  <tbody>
                    {enumEntries.map((entry, idx) => (
                      <tr key={`${entry.org_id}:${entry.plugin_id}:${entry.day_utc}:${idx}`}>
                        <td className="mono">{entry.day_utc}</td>
                        <td className="mono">{entry.org_id}</td>
                        <td className="mono">{entry.plugin_id}</td>
                        <td className="mono">{entry.total_runs}</td>
                        <td className="mono">{entry.compute_only_runs}</td>
                        <td className="mono">{entry.distinct_inputs}</td>
                        <td>
                          {entry.flagged ? (
                            <span className="bc-badge bc-badge-danger">Flagged Sweep</span>
                          ) : (
                            <span className="bc-badge bc-badge-success">Normal</span>
                          )}
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )}
          </div>
        )}
      </div>
    </div>
  );
}
