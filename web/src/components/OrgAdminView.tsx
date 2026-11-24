import React, { useCallback, useEffect, useState } from "react";
import { AgentCredentialsPanel } from "./AgentCredentialsPanel";
import { BillingStatementPanel } from "./BillingStatementPanel";
import { ClientCloudImportPanel } from "./ClientCloudImportPanel";
import { DatasetOfferVersionsView } from "./DatasetOfferVersionsView";
import { DocumentsPanel } from "./DocumentsPanel";
import { ModuleOffersView } from "./ModuleOffersView";
import { WebhooksPanel } from "./WebhooksPanel";
import {
  approveDatasetOffer,
  createApiKey,
  createGrant,
  createOrgInvitation,
  deleteGrant,
  enrollNode,
  enrollTotp,
  fetchApiKeys,
  fetchAuditLogs,
  fetchDatasetOffers,
  fetchDatasetOfferRequests,
  fetchNodes,
  fetchOrgInvitations,
  fetchOrgMembers,
  formatErrorInline,
  publishDatasetOffer,
  revokeApiKey,
  revokeNode,
  subscribeDatasetOffer,
  updateGrant,
  updateOrgMemberRole,
  verifyTotp,
} from "../api";
import {
  formatMilliAct,
  hasMinOrgRole,
  type ApiKeyRecord,
  type AuditLogRecord,
  type AuthPrincipal,
  type CatalogManifest,
  type CreatedApiKey,
  type CreatedNodeEnrollment,
  type CreatedOrgInvitation,
  type DatasetGrant,
  type DatasetHostingMode,
  type DatasetOfferRecord,
  type DatasetOfferRequest,
  type DatasetOfferScope,
  type FilterExpr,
  type GrantAccessLevel,
  type GrantUsageSummary,
  type LedgerState,
  type ModuleOffer,
  type NodeSummaryRecord,
  type OrgInvitationRecord,
  type OrgMemberRecord,
  type OrgRole,
  type TotpEnrollment,
} from "../types";
import {
  IconCheck,
  IconCopy,
  IconDatabase,
  IconFrogletNode,
  IconPlus,
  IconShield,
  IconTrash,
} from "./Icons";

export interface OrgAdminViewProps {
  principal: AuthPrincipal;
  catalogs: CatalogManifest[];
  grants: DatasetGrant[];
  grantUsage: GrantUsageSummary[];
  ledger: LedgerState | null;
  onRefreshWorkspace: () => Promise<void>;
  onOpenModuleOffer: (offer: ModuleOffer) => Promise<void>;
  initialTab?: "overview" | "module-offers";
}

type OrgSubTab =
  | "overview"
  | "members"
  | "security"
  | "agent-credentials"
  | "statements"
  | "webhooks"
  | "documents"
  | "grants"
  | "offers"
  | "module-offers"
  | "client-cloud-import"
  | "nodes"
  | "audit";

export function OrgAdminView({
  principal,
  catalogs,
  grants,
  grantUsage,
  ledger,
  onRefreshWorkspace,
  onOpenModuleOffer,
  initialTab = "overview",
}: OrgAdminViewProps) {
  const isOrgAdmin = hasMinOrgRole(principal.role, "admin");
  const canRequestOffer = hasMinOrgRole(principal.role, "member");
  const [subTab, setSubTab] = useState<OrgSubTab>(initialTab);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [statusMsg, setStatusMsg] = useState<string | null>(null);
  const [copiedId, setCopiedId] = useState<string | null>(null);

  // Loaded data
  const [members, setMembers] = useState<OrgMemberRecord[]>([]);
  const [invitations, setInvitations] = useState<OrgInvitationRecord[]>([]);
  const [apiKeys, setApiKeys] = useState<ApiKeyRecord[]>([]);
  const [datasetOffers, setDatasetOffers] = useState<DatasetOfferRecord[]>([]);
  const [offerRequests, setOfferRequests] = useState<DatasetOfferRequest[]>([]);
  const [nodes, setNodes] = useState<NodeSummaryRecord[]>([]);
  const [auditLogs, setAuditLogs] = useState<AuditLogRecord[]>([]);

  // Invite form
  const [inviteEmail, setInviteEmail] = useState("");
  const [inviteRole, setInviteRole] = useState<OrgRole>("member");
  const [createdInvite, setCreatedInvite] = useState<CreatedOrgInvitation | null>(null);

  // Security (TOTP + API Key)
  const [totpEnrollment, setTotpEnrollment] = useState<TotpEnrollment | null>(null);
  const [totpVerifyCode, setTotpVerifyCode] = useState("");
  const [totpEnabledLocal, setTotpEnabledLocal] = useState<boolean>(
    Boolean(principal.totp_enabled),
  );
  const [newKeyLabel, setNewKeyLabel] = useState("");
  const [newKeyRole, setNewKeyRole] = useState<OrgRole>("developer");
  const [createdKey, setCreatedKey] = useState<CreatedApiKey | null>(null);

  // Grant form
  const [grantGranteeOrg, setGrantGranteeOrg] = useState(principal.org_id);
  const [grantCatalogId, setGrantCatalogId] = useState(catalogs[0]?.id ?? "");
  const [grantLabel, setGrantLabel] = useState("Partner Cohort Access");
  const [grantLevel, setGrantLevel] = useState<GrantAccessLevel>("compute_only");
  const [grantDailyBudget, setGrantDailyBudget] = useState("500");
  const [grantMaxRows, setGrantMaxRows] = useState("250");
  const [grantAllowExport, setGrantAllowExport] = useState(false);
  const [grantRowScopeJson, setGrantRowScopeJson] = useState("");

  // Dataset Offer form
  const [offerCatalogId, setOfferCatalogId] = useState(catalogs[0]?.id ?? "");
  const [offerHostingMode, setOfferHostingMode] =
    useState<DatasetHostingMode>("cloud_hosted");
  const [offerNodeId, setOfferNodeId] = useState("");
  const [offerScope, setOfferScope] = useState<DatasetOfferScope>("private");
  const [offerRecipients, setOfferRecipients] = useState("");
  const [offerApproval, setOfferApproval] = useState(false);
  const [offerTitle, setOfferTitle] = useState("");
  const [offerDescription, setOfferDescription] = useState("");
  const [offerDefaultLevel, setOfferDefaultLevel] =
    useState<GrantAccessLevel>("compute_only");
  const [offerDailyBudget, setOfferDailyBudget] = useState("500");
  const [offerMaxRows, setOfferMaxRows] = useState("250");
  // Entered in euros (to the cent); sent to the hub in milli-euro.
  const [offerPriceMilliAct, setOfferPriceMilliAct] = useState("0");

  // Hospital-Edge Node form
  const [nodeIdInput, setNodeIdInput] = useState("");
  const [nodePublicKey, setNodePublicKey] = useState("");
  const [createdNode, setCreatedNode] = useState<CreatedNodeEnrollment | null>(null);

  const copyToClipboard = (id: string, val: string) => {
    void navigator.clipboard?.writeText(val);
    setCopiedId(id);
    window.setTimeout(() => {
      setCopiedId((prev) => (prev === id ? null : prev));
    }, 1600);
  };

  const refreshAdminLists = useCallback(async () => {
    setError(null);
    try {
      if (subTab === "offers") {
        const [offers, requests] = await Promise.all([
          fetchDatasetOffers(),
          canRequestOffer ? fetchDatasetOfferRequests() : Promise.resolve([]),
        ]);
        setDatasetOffers(offers);
        setOfferRequests(requests);
      }
      if (!isOrgAdmin) return;
      if (subTab === "members") {
        const [people, pending] = await Promise.all([fetchOrgMembers(), fetchOrgInvitations()]);
        setMembers(people);
        setInvitations(pending);
      } else if (subTab === "security") {
        setApiKeys(await fetchApiKeys());
      } else if (subTab === "nodes") {
        setNodes(await fetchNodes());
      } else if (subTab === "audit") {
        setAuditLogs(await fetchAuditLogs());
      }
    } catch (err: unknown) {
      setError(formatErrorInline(err));
    }
  }, [canRequestOffer, isOrgAdmin, subTab]);

  useEffect(() => {
    void refreshAdminLists();
  }, [refreshAdminLists]);

  useEffect(() => {
    if (!grantCatalogId && catalogs[0]?.id) {
      setGrantCatalogId(catalogs[0].id);
    }
    if (!offerCatalogId && catalogs[0]?.id) {
      setOfferCatalogId(catalogs[0].id);
    }
  }, [catalogs, grantCatalogId, offerCatalogId]);

  const actLeftTodayDisplay = (() => {
    if (!ledger || ledger.daily_act_breaker_milli == null) {
      return "Unlimited";
    }
    const spent = ledger.daily_act_spent_milli ?? 0;
    const remaining = Math.max(0, ledger.daily_act_breaker_milli - spent);
    return `${formatMilliAct(remaining)} / ${formatMilliAct(ledger.daily_act_breaker_milli)}`;
  })();

  const handleCreateInvite = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!inviteEmail.trim()) return;
    setBusy(true);
    setError(null);
    setStatusMsg(null);
    try {
      const res = await createOrgInvitation({
        email: inviteEmail.trim(),
        role: inviteRole,
      });
      setCreatedInvite(res);
      setInviteEmail("");
      setStatusMsg(`Created invitation for ${res.email}.`);
      await refreshAdminLists();
    } catch (err: unknown) {
      setError(formatErrorInline(err));
    } finally {
      setBusy(false);
    }
  };

  const handleChangeMemberRole = async (userId: string, role: OrgRole) => {
    setBusy(true);
    setError(null);
    setStatusMsg(null);
    try {
      await updateOrgMemberRole(userId, role);
      setStatusMsg(`Updated role for ${userId} to ${role}.`);
      await refreshAdminLists();
    } catch (err: unknown) {
      setError(formatErrorInline(err));
    } finally {
      setBusy(false);
    }
  };

  const handleStartTotp = async () => {
    setBusy(true);
    setError(null);
    setStatusMsg(null);
    try {
      const enrolled = await enrollTotp();
      setTotpEnrollment(enrolled);
    } catch (err: unknown) {
      setError(formatErrorInline(err));
    } finally {
      setBusy(false);
    }
  };

  const handleVerifyTotp = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!totpVerifyCode.trim()) return;
    setBusy(true);
    setError(null);
    setStatusMsg(null);
    try {
      await verifyTotp(totpVerifyCode.trim());
      setTotpEnabledLocal(true);
      setTotpEnrollment(null);
      setTotpVerifyCode("");
      setStatusMsg("Two-factor TOTP authentication enabled on your account.");
    } catch (err: unknown) {
      setError(formatErrorInline(err));
    } finally {
      setBusy(false);
    }
  };

  const handleCreateApiKey = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!newKeyLabel.trim()) return;
    setBusy(true);
    setError(null);
    setStatusMsg(null);
    try {
      const created = await createApiKey({
        label: newKeyLabel.trim(),
        role: newKeyRole,
      });
      setCreatedKey(created);
      setNewKeyLabel("");
      setStatusMsg(`Issued scoped API key "${created.label}" (${created.role}).`);
      await refreshAdminLists();
    } catch (err: unknown) {
      setError(formatErrorInline(err));
    } finally {
      setBusy(false);
    }
  };

  const handleRevokeApiKey = async (keyId: string) => {
    setBusy(true);
    setError(null);
    try {
      await revokeApiKey(keyId);
      setStatusMsg(`Revoked API key ${keyId}.`);
      await refreshAdminLists();
    } catch (err: unknown) {
      setError(formatErrorInline(err));
    } finally {
      setBusy(false);
    }
  };

  const handleCreateGrant = async (e: React.FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    setStatusMsg(null);
    try {
      let parsedScope: FilterExpr | null = null;
      if (grantRowScopeJson.trim()) {
        parsedScope = JSON.parse(grantRowScopeJson) as FilterExpr;
      }
      const dailyBudget = grantDailyBudget.trim() ? Number(grantDailyBudget) : null;
      await createGrant({
        grantee_org_id: grantGranteeOrg.trim(),
        catalog_id: grantCatalogId.trim(),
        label: grantLabel.trim() || "Dataset Grant",
        level: grantLevel,
        row_scope: parsedScope,
        daily_row_budget:
          dailyBudget != null && Number.isFinite(dailyBudget) ? dailyBudget : null,
        max_rows_per_query: Number(grantMaxRows) || 250,
        allow_local_export: grantLevel === "rows" ? grantAllowExport : false,
      });
      setStatusMsg(`Issued ${grantLevel} grant on ${grantCatalogId} for ${grantGranteeOrg}.`);
      await onRefreshWorkspace();
    } catch (err: unknown) {
      setError(formatErrorInline(err));
    } finally {
      setBusy(false);
    }
  };

  const handleDowngradeGrantToComputeOnly = async (g: DatasetGrant) => {
    setBusy(true);
    setError(null);
    try {
      await updateGrant(g.grant_id, {
        label: g.label,
        level: "compute_only",
        row_scope: g.row_scope ?? null,
        daily_row_budget: g.daily_row_budget ?? null,
        max_rows_per_query: g.max_rows_per_query,
        allow_local_export: false,
      });
      setStatusMsg(`Tightened grant ${g.grant_id} to compute_only.`);
      await onRefreshWorkspace();
    } catch (err: unknown) {
      setError(formatErrorInline(err));
    } finally {
      setBusy(false);
    }
  };

  const handleRevokeGrant = async (grantId: string) => {
    setBusy(true);
    setError(null);
    try {
      await deleteGrant(grantId);
      setStatusMsg(`Revoked grant ${grantId}.`);
      await onRefreshWorkspace();
    } catch (err: unknown) {
      setError(formatErrorInline(err));
    } finally {
      setBusy(false);
    }
  };

  const handlePublishOffer = async (e: React.FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    setStatusMsg(null);
    try {
      const budget = offerDailyBudget.trim() ? Number(offerDailyBudget) : null;
      const offer = await publishDatasetOffer({
        catalog_id: offerCatalogId.trim(),
        scope: offerScope,
        allowed_orgs: offerScope === "custom_shared" ? offerRecipients.split(",").map((org) => org.trim()).filter(Boolean) : [],
        approval_required: offerApproval,
        hosting_mode: offerHostingMode,
        ...(offerHostingMode === "node_hosted" && offerNodeId.trim()
          ? { node_id: offerNodeId.trim() }
          : {}),
        title: offerTitle.trim() || `${offerCatalogId} Governed Access`,
        description: offerDescription.trim(),
        default_level: offerDefaultLevel,
        daily_row_budget: budget != null && Number.isFinite(budget) ? budget : null,
        max_rows_per_query: Number(offerMaxRows) || 250,
        price_milli_act: Math.round((Number(offerPriceMilliAct) || 0) * 1000),
      });
      setOfferTitle("");
      setOfferDescription("");
      setStatusMsg(`Published Froglet Dataset Offer "${offer.title}" (${offer.offer_id}).`);
      await refreshAdminLists();
    } catch (err: unknown) {
      setError(formatErrorInline(err));
    } finally {
      setBusy(false);
    }
  };

  const handleSubscribeOffer = async (offerId: string) => {
    setBusy(true);
    setError(null);
    setStatusMsg(null);
    try {
      const res = await subscribeDatasetOffer(offerId);
      setStatusMsg(res.status === "pending" ? "Access requested. Publisher approval is pending."
        : `Access granted for ${res.offer.title}.`);
      await Promise.all([refreshAdminLists(), onRefreshWorkspace()]);
    } catch (err: unknown) {
      setError(formatErrorInline(err));
    } finally {
      setBusy(false);
    }
  };

  const handleApproveOffer = async (request: DatasetOfferRequest) => {
    setBusy(true);
    setError(null);
    setStatusMsg(null);
    try {
      await approveDatasetOffer(request.offer_id, request.subscriber_org_id);
      setStatusMsg(`Approved ${request.title} for ${request.subscriber_org_id}.`);
      await Promise.all([refreshAdminLists(), onRefreshWorkspace()]);
    } catch (err: unknown) {
      setError(formatErrorInline(err));
    } finally {
      setBusy(false);
    }
  };

  const handleEnrollEdgeNode = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!nodeIdInput.trim()) return;
    const publicKey = nodePublicKey.trim().toLowerCase();
    if (!/^[0-9a-f]{64}$/.test(publicKey)) {
      setError("Enter the 64-character signing public key generated on the customer node.");
      return;
    }
    setBusy(true);
    setError(null);
    setStatusMsg(null);
    try {
      const res = await enrollNode({
        node_id: nodeIdInput.trim(),
        mode: "provider-edge",
        org_id: principal.org_id,
        provider_pubkey: publicKey,
      });
      setCreatedNode(res);
      setNodeIdInput("");
      setNodePublicKey("");
      setStatusMsg(`Enrolled provider-edge node ${res.node_id}.`);
      await refreshAdminLists();
    } catch (err: unknown) {
      setError(formatErrorInline(err));
    } finally {
      setBusy(false);
    }
  };

  const handleRevokeEdgeNode = async (nodeId: string) => {
    setBusy(true);
    setError(null);
    try {
      await revokeNode(nodeId);
      setStatusMsg(`Revoked node ${nodeId}.`);
      await refreshAdminLists();
    } catch (err: unknown) {
      setError(formatErrorInline(err));
    } finally {
      setBusy(false);
    }
  };

  const tabs: { id: OrgSubTab; label: string; adminOnly?: boolean }[] = [
    { id: "overview", label: "Budgets & Overview" },
    { id: "statements", label: "EUR Statements" },
    { id: "agent-credentials", label: "Agent Credentials", adminOnly: true },
    { id: "webhooks", label: "Webhooks", adminOnly: true },
    { id: "documents", label: "Documents" },
    { id: "security", label: "2FA & API Keys" },
    { id: "members", label: "Members & Invites", adminOnly: true },
    { id: "grants", label: "Dataset Grants", adminOnly: true },
    { id: "offers", label: "Dataset Offers" },
    { id: "module-offers", label: "Module Offers" },
    { id: "client-cloud-import", label: "Client Cloud Import", adminOnly: true },
    { id: "nodes", label: "Provider-Edge Nodes", adminOnly: true },
    { id: "audit", label: "Audit Trail", adminOnly: true },
  ];

  return (
    <div className="bc-page-scroll" data-testid="org-admin-view">
      <div className="bc-page-container">
        <div className="bc-page-header">
          <div>
            <h1 className="bc-page-title">Organization &amp; Governance</h1>
            <p className="bc-page-subtitle">
              Organization <span className="mono">{principal.org_id}</span> · Role{" "}
              <span className="bc-badge bc-badge-accent mono">{principal.role}</span>
              {principal.tier_id && (
                <>
                  {" "}
                  · Tier <span className="bc-chip mono">{principal.tier_id}</span>
                </>
              )}
            </p>
          </div>
        </div>

        <div className="bc-module-filter-bar" style={{ marginBottom: 16 }}>
          <div className="bc-filter-pills" role="tablist">
            {tabs
              .filter((t) => !t.adminOnly || isOrgAdmin)
              .map((t) => (
                <button
                  key={t.id}
                  type="button"
                  role="tab"
                  aria-selected={subTab === t.id}
                  data-testid={`org-tab-${t.id}`}
                  data-active={String(subTab === t.id)}
                  className="bc-filter-pill"
                  onClick={() => {
                    if (t.id !== "nodes") setCreatedNode(null);
                    setSubTab(t.id);
                    setError(null);
                    setStatusMsg(null);
                  }}
                >
                  <span>{t.label}</span>
                </button>
              ))}
          </div>
        </div>

        {error && (
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
            {error}
          </div>
        )}

        {statusMsg && (
          <div
            data-testid="org-admin-status"
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

        {subTab === "overview" && (
          <div className="bc-settings-stack">
            <div className="bc-meta-grid">
              <div className="bc-meta-item">
                <span className="bc-caption">Available Balance (EUR)</span>
                <strong
                  className="mono"
                  style={{ fontSize: 18, color: "var(--bc-accent)" }}
                  data-testid="org-available-act"
                >
                  {ledger?.available_display ?? "€0.00"}
                </strong>
              </div>
              <div className="bc-meta-item">
                <span className="bc-caption">EUR Left Today (Daily Breaker)</span>
                <strong
                  className="mono"
                  style={{ fontSize: 16 }}
                  data-testid="org-act-left-today"
                >
                  {actLeftTodayDisplay}
                </strong>
              </div>
              <div className="bc-meta-item">
                <span className="bc-caption">Subscription Tier</span>
                <span className="mono">{ledger?.tier_id ?? principal.tier_id ?? "standard"}</span>
              </div>
              <div className="bc-meta-item">
                <span className="bc-caption">Trial / Account Expiry</span>
                <span className="mono">
                  {ledger?.expires_at
                    ? new Date(ledger.expires_at).toLocaleDateString()
                    : "Active (No Expiry)"}
                </span>
              </div>
            </div>

            <div className="bc-card" data-testid="org-grant-budget-list">
              <div className="bc-card-header" style={{ marginBottom: 8 }}>
                <span className="bc-card-title">Active Dataset Grants &amp; Daily Row Budgets</span>
                <span className="bc-caption">{grants.length} grant(s)</span>
              </div>
              {grants.length === 0 ? (
                <div className="bc-empty-note">No dataset grants issued to this organization.</div>
              ) : (
                <div className="table-scroll">
                  <table className="data-table">
                    <thead>
                      <tr>
                        <th>Catalog</th>
                        <th>Grant</th>
                        <th>Level</th>
                        <th>Max Rows / Query</th>
                        <th>Rows Left Today</th>
                      </tr>
                    </thead>
                    <tbody>
                      {grants.map((g) => {
                        const usage = grantUsage.find((u) => u.grant_id === g.grant_id);
                        const usedToday = usage?.rows_used_today ?? 0;
                        const leftToday =
                          g.daily_row_budget != null
                            ? `${Math.max(0, g.daily_row_budget - usedToday).toLocaleString()} / ${g.daily_row_budget.toLocaleString()}`
                            : "Unlimited";
                        return (
                          <tr key={g.grant_id}>
                            <td className="mono">{g.catalog_id}</td>
                            <td>{g.label}</td>
                            <td>
                              <span
                                className={`bc-badge mono ${
                                  g.level === "rows" ? "bc-badge-success" : "bc-badge-warn"
                                }`}
                              >
                                {g.level}
                              </span>
                            </td>
                            <td className="mono">{g.max_rows_per_query.toLocaleString()}</td>
                            <td className="mono">{leftToday}</td>
                          </tr>
                        );
                      })}
                    </tbody>
                  </table>
                </div>
              )}
            </div>
          </div>
        )}

        {subTab === "security" && (
          <div className="bc-settings-stack">
            <div className="bc-card">
              <div className="bc-card-header">
                <div>
                  <span className="bc-card-title">Two-Factor Authentication (TOTP)</span>
                  <p className="bc-caption" style={{ marginTop: 3 }}>
                    Protect interactive cloud sign-in with an RFC 6238 authenticator app.
                  </p>
                </div>
                <span
                  className={`bc-badge ${
                    totpEnabledLocal ? "bc-badge-success" : "bc-badge-neutral"
                  }`}
                >
                  {totpEnabledLocal ? "2FA Enabled" : "Not Enrolled"}
                </span>
              </div>

              {!totpEnrollment ? (
                <div style={{ marginTop: 10 }}>
                  <button
                    type="button"
                    data-testid="enroll-totp-btn"
                    className="bc-btn bc-btn-secondary"
                    disabled={busy}
                    onClick={() => void handleStartTotp()}
                  >
                    <IconShield size={14} />
                    <span>{totpEnabledLocal ? "Rotate TOTP Secret" : "Enroll 2FA Authenticator"}</span>
                  </button>
                </div>
              ) : (
                <form
                  onSubmit={(e) => void handleVerifyTotp(e)}
                  style={{ marginTop: 12, display: "grid", gap: 10 }}
                >
                  <div className="bc-caption">
                    Add this Base32 secret or URI to 1Password, Authy, or Google Authenticator, then
                    enter a 6-digit code to activate:
                  </div>
                  <pre className="bc-code-pre mono" data-testid="totp-secret-display">
                    Secret: {totpEnrollment.secret_base32}
                    {"\n"}URI:    {totpEnrollment.otpauth_uri}
                  </pre>
                  <div style={{ display: "flex", gap: 8, alignItems: "center", flexWrap: "wrap" }}>
                    <input
                      type="text"
                      inputMode="numeric"
                      pattern="[0-9]{6}"
                      maxLength={6}
                      required
                      data-testid="totp-verify-input"
                      className="bc-input mono"
                      placeholder="123456"
                      value={totpVerifyCode}
                      onChange={(e) => setTotpVerifyCode(e.target.value)}
                    />
                    <button
                      type="submit"
                      data-testid="totp-verify-submit"
                      className="bc-btn bc-btn-primary"
                      disabled={busy}
                    >
                      Verify &amp; Enable 2FA
                    </button>
                  </div>
                </form>
              )}
            </div>

            {isOrgAdmin && (
              <div className="bc-card">
                <div className="bc-card-header">
                  <div>
                    <span className="bc-card-title">Scoped Organization API Keys</span>
                    <p className="bc-caption" style={{ marginTop: 3 }}>
                      Keys carry an explicit RBAC role and cannot exceed your own role ({principal.role}).
                    </p>
                  </div>
                </div>

                <form
                  onSubmit={(e) => void handleCreateApiKey(e)}
                  style={{
                    marginTop: 10,
                    display: "flex",
                    gap: 8,
                    flexWrap: "wrap",
                    alignItems: "flex-end",
                  }}
                >
                  <label className="bc-field" style={{ flex: 1, minWidth: 200 }}>
                    <span className="bc-field-label">Key Label</span>
                    <input
                      type="text"
                      required
                      data-testid="api-key-label-input"
                      className="bc-input"
                      placeholder="CI Publish / Headless Pipeline"
                      value={newKeyLabel}
                      onChange={(e) => setNewKeyLabel(e.target.value)}
                    />
                  </label>
                  <label className="bc-field" style={{ minWidth: 150 }}>
                    <span className="bc-field-label">Scoped Role</span>
                    <select
                      data-testid="api-key-role-select"
                      className="bc-input mono"
                      value={newKeyRole}
                      onChange={(e) => setNewKeyRole(e.target.value as OrgRole)}
                    >
                      <option value="viewer">viewer</option>
                      <option value="member">member</option>
                      <option value="developer">developer</option>
                      <option value="admin">admin</option>
                    </select>
                  </label>
                  <button
                    type="submit"
                    data-testid="create-api-key-btn"
                    className="bc-btn bc-btn-primary"
                    disabled={busy}
                  >
                    <IconPlus size={14} />
                    <span>Create API Key</span>
                  </button>
                </form>

                {createdKey && (
                  <div
                    data-testid="created-api-key-banner"
                    style={{
                      marginTop: 12,
                      padding: "10px 12px",
                      borderRadius: "var(--bc-radius-sm)",
                      background: "var(--bc-bg-subtle)",
                      border: "1px solid var(--bc-accent)",
                    }}
                  >
                    <div className="bc-caption" style={{ marginBottom: 4 }}>
                      Copy this secret API key now — it is hashed with SHA-256 and never shown again:
                    </div>
                    <div style={{ display: "flex", gap: 8, alignItems: "center" }}>
                      <code className="mono" style={{ wordBreak: "break-all", fontSize: 12.5 }}>
                        {createdKey.raw_key}
                      </code>
                      <button
                        type="button"
                        className="bc-btn bc-btn-sm"
                        onClick={() => copyToClipboard("apikey", createdKey.raw_key)}
                      >
                        {copiedId === "apikey" ? <IconCheck size={12} /> : <IconCopy size={12} />}
                        <span>{copiedId === "apikey" ? "Copied" : "Copy"}</span>
                      </button>
                    </div>
                  </div>
                )}

                {apiKeys.length > 0 && (
                  <div className="table-scroll" style={{ marginTop: 12 }}>
                    <table className="data-table">
                      <thead>
                        <tr>
                          <th>Label</th>
                          <th>Prefix</th>
                          <th>Role</th>
                          <th>Status</th>
                          <th />
                        </tr>
                      </thead>
                      <tbody>
                        {apiKeys.map((k) => (
                          <tr key={k.key_id}>
                            <td>{k.label}</td>
                            <td className="mono">{k.key_prefix}…</td>
                            <td className="mono">{k.role}</td>
                            <td>
                              <span
                                className={`bc-badge ${
                                  k.revoked_at ? "bc-badge-danger" : "bc-badge-success"
                                }`}
                              >
                                {k.revoked_at ? "Revoked" : "Active"}
                              </span>
                            </td>
                            <td style={{ textAlign: "right" }}>
                              {!k.revoked_at && (
                                <button
                                  type="button"
                                  className="bc-btn bc-btn-sm"
                                  disabled={busy}
                                  onClick={() => void handleRevokeApiKey(k.key_id)}
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
                )}
              </div>
            )}
          </div>
        )}

        {subTab === "members" && isOrgAdmin && (
          <div className="bc-settings-stack">
            <div className="bc-card">
              <div className="bc-card-header">
                <span className="bc-card-title">Invite Organization Member</span>
              </div>
              <form
                onSubmit={(e) => void handleCreateInvite(e)}
                style={{
                  marginTop: 10,
                  display: "flex",
                  gap: 8,
                  flexWrap: "wrap",
                  alignItems: "flex-end",
                }}
              >
                <label className="bc-field" style={{ flex: 1, minWidth: 220 }}>
                  <span className="bc-field-label">Invitee Email</span>
                  <input
                    type="email"
                    required
                    data-testid="invite-member-email"
                    className="bc-input"
                    placeholder="colleague@biotech.org"
                    value={inviteEmail}
                    onChange={(e) => setInviteEmail(e.target.value)}
                  />
                </label>
                <label className="bc-field" style={{ minWidth: 150 }}>
                  <span className="bc-field-label">Assigned Role</span>
                  <select
                    data-testid="invite-member-role"
                    className="bc-input mono"
                    value={inviteRole}
                    onChange={(e) => setInviteRole(e.target.value as OrgRole)}
                  >
                    <option value="viewer">viewer</option>
                    <option value="member">member</option>
                    <option value="developer">developer</option>
                    <option value="admin">admin</option>
                  </select>
                </label>
                <button
                  type="submit"
                  data-testid="create-invite-btn"
                  className="bc-btn bc-btn-primary"
                  disabled={busy}
                >
                  <IconPlus size={14} />
                  <span>Generate Invite Token</span>
                </button>
              </form>

              {createdInvite && (
                <div
                  data-testid="created-invite-banner"
                  style={{
                    marginTop: 12,
                    padding: "10px 12px",
                    borderRadius: "var(--bc-radius-sm)",
                    background: "var(--bc-bg-subtle)",
                    border: "1px solid var(--bc-accent)",
                  }}
                >
                  <div className="bc-caption" style={{ marginBottom: 4 }}>
                    Share this one-time invitation token with{" "}
                    <strong>{createdInvite.email}</strong>:
                  </div>
                  <div style={{ display: "flex", gap: 8, alignItems: "center" }}>
                    <code className="mono" style={{ wordBreak: "break-all" }}>
                      {createdInvite.invite_token}
                    </code>
                    <button
                      type="button"
                      className="bc-btn bc-btn-sm"
                      onClick={() => copyToClipboard("invite", createdInvite.invite_token)}
                    >
                      {copiedId === "invite" ? <IconCheck size={12} /> : <IconCopy size={12} />}
                      <span>{copiedId === "invite" ? "Copied" : "Copy"}</span>
                    </button>
                  </div>
                </div>
              )}
            </div>

            <div className="bc-card">
              <div className="bc-card-header" style={{ marginBottom: 8 }}>
                <span className="bc-card-title">Organization Members ({members.length})</span>
              </div>
              <div className="table-scroll">
                <table className="data-table">
                  <thead>
                    <tr>
                      <th>Member</th>
                      <th>Email</th>
                      <th>2FA</th>
                      <th>Role</th>
                    </tr>
                  </thead>
                  <tbody>
                    {members.map((m) => (
                      <tr key={m.user_id}>
                        <td>{m.display_name}</td>
                        <td className="mono">{m.email}</td>
                        <td>{m.totp_enabled ? "Enabled" : "—"}</td>
                        <td>
                          <select
                            className="bc-input mono"
                            style={{ padding: "3px 8px", width: "auto" }}
                            disabled={busy}
                            value={m.role}
                            onChange={(e) =>
                              void handleChangeMemberRole(m.user_id, e.target.value as OrgRole)
                            }
                          >
                            <option value="viewer">viewer</option>
                            <option value="member">member</option>
                            <option value="developer">developer</option>
                            <option value="admin">admin</option>
                            <option value="owner">owner</option>
                          </select>
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            </div>

            {invitations.length > 0 && (
              <div className="bc-card">
                <div className="bc-card-header" style={{ marginBottom: 8 }}>
                  <span className="bc-card-title">Invitations ({invitations.length})</span>
                </div>
                <div className="table-scroll">
                  <table className="data-table">
                    <thead>
                      <tr>
                        <th>Email</th>
                        <th>Role</th>
                        <th>Status</th>
                        <th>Expires</th>
                      </tr>
                    </thead>
                    <tbody>
                      {invitations.map((inv) => (
                        <tr key={inv.invitation_id}>
                          <td className="mono">{inv.email}</td>
                          <td className="mono">{inv.role}</td>
                          <td>
                            <span className="bc-badge bc-badge-neutral">
                              {inv.accepted_at ? "Accepted" : "Pending"}
                            </span>
                          </td>
                          <td className="mono bc-caption">
                            {new Date(inv.expires_at).toLocaleDateString()}
                          </td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              </div>
            )}
          </div>
        )}

        {subTab === "grants" && isOrgAdmin && (
          <div className="bc-settings-stack">
            <div className="bc-card">
              <div className="bc-card-header">
                <div>
                  <span className="bc-card-title">Issue 3-Level Dataset Grant</span>
                  <p className="bc-caption" style={{ marginTop: 2 }}>
                    Grant <code className="mono">compute_only</code> or scoped unmasked{" "}
                    <code className="mono">rows</code> access on a catalog owned by your org.
                  </p>
                </div>
              </div>

              <form
                onSubmit={(e) => void handleCreateGrant(e)}
                className="bc-form-grid"
                style={{ marginTop: 10 }}
              >
                <label className="bc-field">
                  <span className="bc-field-label">Grantee Org ID</span>
                  <input
                    type="text"
                    required
                    data-testid="grant-grantee-org-input"
                    className="bc-input mono"
                    value={grantGranteeOrg}
                    onChange={(e) => setGrantGranteeOrg(e.target.value)}
                  />
                </label>

                <label className="bc-field">
                  <span className="bc-field-label">Catalog ID</span>
                  <input
                    type="text"
                    required
                    data-testid="grant-catalog-id-input"
                    className="bc-input mono"
                    placeholder="org/catalog-v1"
                    value={grantCatalogId}
                    onChange={(e) => setGrantCatalogId(e.target.value)}
                  />
                </label>

                <label className="bc-field">
                  <span className="bc-field-label">Grant Label</span>
                  <input
                    type="text"
                    required
                    className="bc-input"
                    value={grantLabel}
                    onChange={(e) => setGrantLabel(e.target.value)}
                  />
                </label>

                <label className="bc-field">
                  <span className="bc-field-label">Access Level</span>
                  <select
                    data-testid="grant-level-select"
                    className="bc-input mono"
                    value={grantLevel}
                    onChange={(e) => setGrantLevel(e.target.value as GrantAccessLevel)}
                  >
                    <option value="compute_only">compute_only (Curated Modules Only)</option>
                    <option value="rows">rows (Unmasked Scoped Rows)</option>
                  </select>
                </label>

                <label className="bc-field">
                  <span className="bc-field-label">Daily Row Budget (blank = unlimited)</span>
                  <input
                    type="number"
                    min={1}
                    className="bc-input mono"
                    value={grantDailyBudget}
                    onChange={(e) => setGrantDailyBudget(e.target.value)}
                  />
                </label>

                <label className="bc-field">
                  <span className="bc-field-label">Max Rows / Query</span>
                  <input
                    type="number"
                    min={1}
                    required
                    className="bc-input mono"
                    value={grantMaxRows}
                    onChange={(e) => setGrantMaxRows(e.target.value)}
                  />
                </label>

                {grantLevel === "rows" && (
                  <label className="bc-field" style={{ gridColumn: "1 / -1" }}>
                    <span className="bc-field-label">
                      Optional Row Scope JSON (e.g. &#123;&quot;organ&quot;: [&quot;Lung&quot;]&#125;)
                    </span>
                    <input
                      type="text"
                      className="bc-input mono"
                      placeholder='{"organ": ["Lung"]}'
                      value={grantRowScopeJson}
                      onChange={(e) => setGrantRowScopeJson(e.target.value)}
                    />
                  </label>
                )}

                {grantLevel === "rows" && (
                  <label
                    style={{
                      gridColumn: "1 / -1",
                      display: "inline-flex",
                      alignItems: "center",
                      gap: 8,
                      fontSize: 13,
                    }}
                  >
                    <input
                      type="checkbox"
                      checked={grantAllowExport}
                      onChange={(e) => setGrantAllowExport(e.target.checked)}
                    />
                    <span>Allow local CSV / TSV / JSON file export</span>
                  </label>
                )}

                <div style={{ gridColumn: "1 / -1" }}>
                  <button
                    type="submit"
                    data-testid="issue-grant-btn"
                    className="bc-btn bc-btn-primary"
                    disabled={busy}
                  >
                    Issue Dataset Grant
                  </button>
                </div>
              </form>
            </div>

            <div className="bc-card">
              <div className="bc-card-header" style={{ marginBottom: 8 }}>
                <span className="bc-card-title">Manage Grants ({grants.length})</span>
              </div>
              <div className="table-scroll">
                <table className="data-table">
                  <thead>
                    <tr>
                      <th>Grant ID</th>
                      <th>Catalog</th>
                      <th>Level</th>
                      <th>Daily Budget</th>
                      <th style={{ textAlign: "right" }}>Actions</th>
                    </tr>
                  </thead>
                  <tbody>
                    {grants.map((g) => (
                      <tr key={g.grant_id}>
                        <td className="mono">{g.grant_id}</td>
                        <td className="mono">{g.catalog_id}</td>
                        <td className="mono">{g.level}</td>
                        <td className="mono">
                          {g.daily_row_budget != null
                            ? g.daily_row_budget.toLocaleString()
                            : "Unlimited"}
                        </td>
                        <td style={{ textAlign: "right", display: "flex", gap: 6, justifyContent: "flex-end" }}>
                          {g.level === "rows" && (
                            <button
                              type="button"
                              className="bc-btn bc-btn-sm"
                              disabled={busy}
                              onClick={() => void handleDowngradeGrantToComputeOnly(g)}
                            >
                              Downgrade to compute_only
                            </button>
                          )}
                          <button
                            type="button"
                            className="bc-btn bc-btn-sm"
                            disabled={busy}
                            onClick={() => void handleRevokeGrant(g.grant_id)}
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

        {subTab === "module-offers" && (
          <ModuleOffersView key={principal.org_id} principal={principal} onOpenRun={onOpenModuleOffer} />
        )}

        {subTab === "offers" && (
          <div className="bc-settings-stack">
            <div className="bc-card">
              <div className="bc-card-header" style={{ marginBottom: 8 }}>
                <div>
                  <span className="bc-card-title">Froglet Dataset Offers Marketplace</span>
                  <p className="bc-caption" style={{ marginTop: 2 }}>
                    Subscribe to cloud-hosted or hospital-edge governed datasets with signed, euro-denominated
                    settlement.
                  </p>
                </div>
              </div>

              {datasetOffers.length === 0 ? (
                <div className="bc-empty-note">No active Dataset Offers published yet.</div>
              ) : (
                <div className="table-scroll">
                  <table className="data-table">
                    <thead>
                      <tr>
                        <th>Title</th>
                        <th>Catalog</th>
                        <th>Visibility</th>
                        <th>Hosting</th>
                        <th>Access Level</th>
                        <th>Price</th>
                        <th />
                      </tr>
                    </thead>
                    <tbody>
                      {datasetOffers.map((off) => (
                        <tr key={off.offer_id}>
                          <td>
                            <strong>{off.title}</strong>
                            {off.description && (
                              <div className="bc-caption">{off.description}</div>
                            )}
                          </td>
                          <td>
                            <span className="mono">{off.catalog_id}</span>
                            <DatasetOfferVersionsView offer={off} publisher={isOrgAdmin && principal.org_id === off.publisher_org_id} />
                          </td>
                          <td>{off.scope === "custom_shared" ? "Custom shared" : off.scope === "global" ? "Public global"
                            : off.scope === "legacy_session_only" ? "Signed-in discovery" : "Private"}</td>
                          <td>
                            <span className="bc-chip mono">
                              <IconDatabase size={11} /> {off.hosting_mode}
                            </span>
                          </td>
                          <td className="mono">{off.default_level}</td>
                          <td className="mono">{formatMilliAct(off.price_milli_act)}</td>
                          <td style={{ textAlign: "right" }}>
                            <button
                              type="button"
                              data-testid={`subscribe-offer-btn-${off.offer_id}`}
                              className="bc-btn bc-btn-primary bc-btn-sm"
                              disabled={busy || !off.active || !(off.approval_required ? canRequestOffer : isOrgAdmin)}
                              onClick={() => void handleSubscribeOffer(off.offer_id)}
                            >
                              {off.approval_required ? "Request access" : "Subscribe"}
                            </button>
                          </td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              )}
            </div>

            {offerRequests.length > 0 && (
              <section className="bc-card" aria-label="Dataset access requests">
                <h3 className="bc-card-title">Access requests</h3>
                {offerRequests.map((request) => (
                  <div className="bc-card-header" key={`${request.offer_id}:${request.subscriber_org_id}`}>
                    <div>
                      <strong>{request.title}</strong>
                      <p className="bc-caption">{request.subscriber_org_id} · {request.status === "pending" ? "Pending" : "Approved"}</p>
                    </div>
                    {isOrgAdmin && request.publisher_org_id === principal.org_id && request.status === "pending" && (
                      <button type="button" className="bc-btn bc-btn-primary bc-btn-sm" disabled={busy}
                        onClick={() => void handleApproveOffer(request)}>Approve {request.title}</button>
                    )}
                  </div>
                ))}
              </section>
            )}

            {isOrgAdmin && (
              <div className="bc-card">
                <div className="bc-card-header">
                  <span className="bc-card-title">Publish Froglet Dataset Offer</span>
                </div>
                <form
                  onSubmit={(e) => void handlePublishOffer(e)}
                  className="bc-form-grid"
                  style={{ marginTop: 10 }}
                >
                  <label className="bc-field">
                    <span className="bc-field-label">Catalog ID</span>
                    <input
                      type="text"
                      required
                      data-testid="publish-offer-catalog-id"
                      className="bc-input mono"
                      value={offerCatalogId}
                      onChange={(e) => setOfferCatalogId(e.target.value)}
                    />
                  </label>

                  <label className="bc-field">
                    <span className="bc-field-label">Hosting Mode</span>
                    <select
                      className="bc-input mono"
                      value={offerHostingMode}
                      onChange={(e) => setOfferHostingMode(e.target.value as DatasetHostingMode)}
                    >
                      <option value="cloud_hosted">cloud_hosted (Alithea Cloud S3)</option>
                      <option value="node_hosted">node_hosted (Hospital-Edge Node)</option>
                    </select>
                  </label>

                  {offerHostingMode === "node_hosted" && (
                    <label className="bc-field">
                      <span className="bc-field-label">Hospital-Edge Node ID</span>
                      <input
                        type="text"
                        required
                        className="bc-input mono"
                        placeholder="zurich-hospital-edge-1"
                        value={offerNodeId}
                        onChange={(e) => setOfferNodeId(e.target.value)}
                      />
                    </label>
                  )}

                  <label className="bc-field">
                    <span className="bc-field-label">Offer Title</span>
                    <input
                      type="text"
                      required
                      data-testid="publish-offer-title"
                      className="bc-input"
                      placeholder="NSCLC Immunopeptidomics Cohort"
                      value={offerTitle}
                      onChange={(e) => setOfferTitle(e.target.value)}
                    />
                  </label>

                  <label className="bc-field">
                    <span className="bc-field-label">Offer visibility</span>
                    <select className="bc-input" value={offerScope} onChange={(e) => setOfferScope(e.target.value as DatasetOfferScope)}>
                      <option value="private">Private</option>
                      <option value="custom_shared">Custom shared</option>
                      <option value="global">Public global</option>
                    </select>
                  </label>
                  {offerScope === "custom_shared" && (
                    <label className="bc-field">
                      <span className="bc-field-label">Recipient organization IDs</span>
                      <input className="bc-input" placeholder="org-a, org-b" required value={offerRecipients}
                        onChange={(e) => setOfferRecipients(e.target.value)} />
                    </label>
                  )}
                  {offerScope === "global" && <p className="bc-caption">The offer title, description, and dataset identifier are public.</p>}
                  <label className="bc-field">
                    <span className="bc-field-label">Access process</span>
                    <select className="bc-input" value={offerApproval ? "approval" : "self_service"}
                      onChange={(e) => setOfferApproval(e.target.value === "approval")}>
                      <option value="self_service">Self-service</option>
                      <option value="approval">Publisher approval required</option>
                    </select>
                  </label>

                  <label className="bc-field">
                    <span className="bc-field-label">Granted Access Level</span>
                    <select
                      className="bc-input mono"
                      value={offerDefaultLevel}
                      onChange={(e) => setOfferDefaultLevel(e.target.value as GrantAccessLevel)}
                    >
                      <option value="compute_only">compute_only</option>
                      <option value="rows">rows</option>
                    </select>
                  </label>

                  <label className="bc-field">
                    <span className="bc-field-label">Subscription fee (EUR)</span>
                    <input
                      type="number"
                      min={0}
                      step="0.01"
                      className="bc-input mono"
                      value={offerPriceMilliAct}
                      onChange={(e) => setOfferPriceMilliAct(e.target.value)}
                    />
                  </label>

                  <label className="bc-field">
                    <span className="bc-field-label">Daily Row Budget</span>
                    <input
                      type="number"
                      min={1}
                      className="bc-input mono"
                      value={offerDailyBudget}
                      onChange={(e) => setOfferDailyBudget(e.target.value)}
                    />
                  </label>

                  <label className="bc-field">
                    <span className="bc-field-label">Max Rows / Query</span>
                    <input
                      type="number"
                      min={1}
                      className="bc-input mono"
                      value={offerMaxRows}
                      onChange={(e) => setOfferMaxRows(e.target.value)}
                    />
                  </label>

                  <label className="bc-field" style={{ gridColumn: "1 / -1" }}>
                    <span className="bc-field-label">Description</span>
                    <input
                      type="text"
                      className="bc-input"
                      value={offerDescription}
                      onChange={(e) => setOfferDescription(e.target.value)}
                    />
                  </label>

                  <div style={{ gridColumn: "1 / -1" }}>
                    <button
                      type="submit"
                      data-testid="publish-offer-submit"
                      className="bc-btn bc-btn-primary"
                      disabled={busy}
                    >
                      Publish Dataset Offer
                    </button>
                  </div>
                </form>
              </div>
            )}
          </div>
        )}

        {subTab === "nodes" && isOrgAdmin && (
          <div className="bc-settings-stack">
            <div className="bc-card">
              <div className="bc-card-header">
                <div>
                  <span className="bc-card-title">Enroll Provider-Edge Worker Node</span>
                  <p className="bc-caption" style={{ marginTop: 2 }}>
                    Self-enroll an org-isolated <code className="mono">provider-edge</code> node bound to{" "}
                    <span className="mono">{principal.org_id}</span>. Provider nodes receive zero Alithea
                    database or S3 credentials and execute only this organization&apos;s offers or
                    consented buyer inputs (non-demo tiers only).
                  </p>
                </div>
              </div>

              <form
                onSubmit={(e) => void handleEnrollEdgeNode(e)}
                style={{
                  marginTop: 10,
                  display: "flex",
                  gap: 8,
                  flexWrap: "wrap",
                  alignItems: "flex-end",
                }}
              >
                <label className="bc-field" style={{ flex: 1, minWidth: 200 }}>
                  <span className="bc-field-label">Node ID</span>
                  <input
                    type="text"
                    required
                    data-testid="enroll-edge-node-id"
                    className="bc-input mono"
                    placeholder="provider-edge-node-01"
                    value={nodeIdInput}
                    onChange={(e) => setNodeIdInput(e.target.value)}
                  />
                </label>
                <label className="bc-field" style={{ flex: 1, minWidth: 200 }}>
                  <span className="bc-field-label">Node signing public key</span>
                  <input
                    type="text"
                    className="bc-input mono"
                    required
                    pattern="[a-fA-F0-9]{64}"
                    maxLength={64}
                    title="Enter the 64-character public key printed by the worker"
                    placeholder="64-character BIP340 signing key"
                    value={nodePublicKey}
                    onChange={(e) => setNodePublicKey(e.target.value)}
                  />
                </label>
                <button
                  type="submit"
                  data-testid="enroll-edge-node-btn"
                  className="bc-btn bc-btn-primary"
                  disabled={busy}
                >
                  <IconFrogletNode size={14} />
                  <span>Enroll Provider-Edge Node</span>
                </button>
              </form>

              {createdNode && (
                <div
                  data-testid="enrolled-node-credential-banner"
                  style={{
                    marginTop: 12,
                    padding: "10px 12px",
                    borderRadius: "var(--bc-radius-sm)",
                    background: "var(--bc-bg-subtle)",
                    border: "1px solid var(--bc-accent)",
                  }}
                >
                  <div className="bc-caption" style={{ marginBottom: 4 }}>
                    Save this credential in a private file on the worker. Start the worker with{" "}
                    <code className="mono">--node-credential-file &lt;path&gt;</code>.
                  </div>
                  <div style={{ display: "flex", gap: 8, alignItems: "center" }}>
                    <code className="mono" style={{ wordBreak: "break-all" }}>
                      {createdNode.raw_credential}
                    </code>
                    <button
                      type="button"
                      className="bc-btn bc-btn-sm"
                      onClick={() => copyToClipboard("nodecred", createdNode.raw_credential)}
                    >
                      {copiedId === "nodecred" ? <IconCheck size={12} /> : <IconCopy size={12} />}
                      <span>{copiedId === "nodecred" ? "Copied" : "Copy"}</span>
                    </button>
                    <button type="button" className="bc-btn bc-btn-sm" onClick={() => setCreatedNode(null)}>
                      Dismiss
                    </button>
                  </div>
                </div>
              )}
            </div>

            <div className="bc-card">
              <div className="bc-card-header" style={{ marginBottom: 8 }}>
                <span className="bc-card-title">Organization Nodes ({nodes.length})</span>
              </div>
              {nodes.length === 0 ? (
                <div className="bc-empty-note">No provider-edge nodes enrolled yet.</div>
              ) : (
                <div className="table-scroll">
                  <table className="data-table">
                    <thead>
                      <tr>
                        <th>Node ID</th>
                        <th>Mode</th>
                        <th>Status</th>
                        <th>Pubkey</th>
                        <th />
                      </tr>
                    </thead>
                    <tbody>
                      {nodes.map((n) => (
                        <tr key={n.node_id}>
                          <td className="mono">{n.node_id}</td>
                          <td className="mono">{n.mode}</td>
                          <td>
                            <span
                              className={`bc-badge ${
                                n.online ? "bc-badge-success" : "bc-badge-neutral"
                              }`}
                            >
                              {n.status}
                            </span>
                          </td>
                          <td className="mono bc-caption">
                            {n.provider_pubkey
                              ? `${n.provider_pubkey.slice(0, 16)}…`
                              : "Pending heartbeat"}
                          </td>
                          <td style={{ textAlign: "right" }}>
                            {n.status !== "revoked" && (
                              <button
                                type="button"
                                className="bc-btn bc-btn-sm"
                                disabled={busy}
                                onClick={() => void handleRevokeEdgeNode(n.node_id)}
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
              )}
            </div>
          </div>
        )}

        {subTab === "agent-credentials" && isOrgAdmin && (
          <AgentCredentialsPanel catalogOptions={catalogs.map((c) => c.id)} />
        )}

        {subTab === "statements" && <BillingStatementPanel />}

        {subTab === "webhooks" && isOrgAdmin && <WebhooksPanel />}

        {subTab === "documents" && <DocumentsPanel />}

        {subTab === "client-cloud-import" && isOrgAdmin && (
          <ClientCloudImportPanel
            catalogs={catalogs}
            onImported={onRefreshWorkspace}
          />
        )}

        {subTab === "audit" && isOrgAdmin && (
          <div className="bc-card">
            <div className="bc-card-header" style={{ marginBottom: 8 }}>
              <span className="bc-card-title">Organization Audit Trail ({auditLogs.length})</span>
            </div>
            {auditLogs.length === 0 ? (
              <div className="bc-empty-note">No audit events recorded yet.</div>
            ) : (
              <div className="table-scroll">
                <table className="data-table">
                  <thead>
                    <tr>
                      <th>Time</th>
                      <th>Action</th>
                      <th>Actor</th>
                      <th>Target</th>
                    </tr>
                  </thead>
                  <tbody>
                    {auditLogs.map((a) => (
                      <tr key={a.audit_id}>
                        <td className="mono bc-caption">
                          {new Date(a.created_at).toLocaleString()}
                        </td>
                        <td className="mono">{a.action}</td>
                        <td className="mono">{a.actor ?? "system"}</td>
                        <td className="mono bc-caption">{a.target_id ?? "—"}</td>
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
