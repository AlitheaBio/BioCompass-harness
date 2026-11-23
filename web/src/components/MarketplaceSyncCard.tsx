import React, { useEffect, useState } from "react";
import {
  fetchMarketplaceStatus,
  formatErrorInline,
  syncMarketplaceNow,
  updateMarketplaceConsent,
} from "../api";
import { ownerLinkHelp } from "../localOwner";
import { formatMarketplaceSyncBadge } from "../milestoneBPolicy";
import type { MarketplaceStatus } from "../types";

export interface MarketplaceSyncCardProps {
  isLocalMode: boolean;
  status: MarketplaceStatus | null;
  onStatusChanged?: (updated: MarketplaceStatus) => void;
}

/**
 * Settings card exposing `[A1]` marketplace consent toggle and `DEV-07` sync status
 * (`air-gapped`, `disconnected`, `connected`, `stale`, last synced timestamp, manual sync).
 */
export function MarketplaceSyncCard({
  isLocalMode,
  status,
  onStatusChanged,
}: MarketplaceSyncCardProps) {
  const [localStatus, setLocalStatus] = useState<MarketplaceStatus | null>(status);
  const [hubUrl, setHubUrl] = useState<string>(
    status?.hub_url ?? status?.default_hub_url ?? "https://hub.biocompass.alithea.bio",
  );
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    setLocalStatus(status);
    if (status?.hub_url) setHubUrl(status.hub_url);
  }, [status]);

  useEffect(() => {
    if (localStatus) return;
    let cancelled = false;
    void fetchMarketplaceStatus()
      .then((res) => {
        if (!cancelled) {
          setLocalStatus(res);
          setHubUrl(res.hub_url || res.default_hub_url);
          onStatusChanged?.(res);
        }
      })
      .catch(() => {
        // Non-fatal if endpoint unavailable
      });
    return () => {
      cancelled = true;
    };
  }, [localStatus, onStatusChanged]);

  const active = localStatus;
  const badge = formatMarketplaceSyncBadge(active);

  const badgeClass =
    badge.tone === "success"
      ? "bc-badge-success"
      : badge.tone === "warn"
        ? "bc-badge-warn"
        : badge.tone === "danger"
          ? "bc-badge-danger"
          : "bc-badge-neutral";

  const formatErr = (err: unknown): string => {
    const msg = formatErrorInline(err);
    return msg.toLowerCase().includes("owner link") || msg.toLowerCase().includes("unauthenticated")
      ? `${msg} ${ownerLinkHelp}`
      : msg;
  };

  const handleSetConsent = async (decision: "accepted" | "declined") => {
    setBusy(true);
    setError(null);
    try {
      const updated = await updateMarketplaceConsent({
        decision,
        ...(decision === "accepted" && hubUrl.trim() ? { hub_url: hubUrl.trim() } : {}),
        sync_now: decision === "accepted",
      });
      setLocalStatus(updated);
      onStatusChanged?.(updated);
    } catch (err: unknown) {
      setError(formatErr(err));
    } finally {
      setBusy(false);
    }
  };

  const handleSyncNow = async () => {
    setBusy(true);
    setError(null);
    try {
      const updated = await syncMarketplaceNow();
      setLocalStatus(updated);
      onStatusChanged?.(updated);
    } catch (err: unknown) {
      setError(formatErr(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="bc-card" data-testid="marketplace-sync-card">
      <div className="bc-card-header" style={{ alignItems: "center" }}>
        <div>
          <div className="bc-card-title">Global Marketplace &amp; Sync Status</div>
          <p className="bc-caption" style={{ marginTop: 2 }}>
            {active?.airgapped
              ? "Started with --airgapped (COMPASS_AIRGAPPED=1). All outbound marketplace connections are blocked."
              : isLocalMode
                ? "Opt-in HTTPS discovery feed for Froglet modules and datasets."
                : "Managed by this Client Cloud deployment's Compass Hub federation peers."}
          </p>
        </div>
        <span
          className={`bc-badge ${badgeClass}`}
          data-testid="marketplace-sync-state-badge"
          data-sync-state={badge.state}
        >
          {badge.label}
        </span>
      </div>

      <div
        style={{
          marginTop: 10,
          display: "grid",
          gridTemplateColumns: "repeat(auto-fit, minmax(180px, 1fr))",
          gap: 8,
          fontSize: 12,
        }}
      >
        <div>
          <span className="bc-caption">Hub URL: </span>
          <span className="mono" data-testid="marketplace-sync-hub-url">
            {active?.hub_url ?? hubUrl}
          </span>
        </div>
        <div>
          <span className="bc-caption">Last synced: </span>
          <span className="mono" data-testid="marketplace-last-synced">
            {active?.last_synced_at
              ? new Date(active.last_synced_at).toLocaleString()
              : "Never"}
          </span>
        </div>
        <div>
          <span className="bc-caption">Discovered offers: </span>
          <span className="mono" data-testid="marketplace-offer-count">
            {active?.feed_offer_count ?? 0}
          </span>
        </div>
      </div>

      {active?.last_error && (
        <div
          role="alert"
          data-testid="marketplace-last-error"
          style={{
            marginTop: 10,
            padding: "8px 10px",
            borderRadius: "var(--bc-radius-sm)",
            background: "var(--bc-danger-soft)",
            color: "var(--bc-danger)",
            fontSize: 12,
          }}
        >
          Last sync error: {active.last_error}
        </div>
      )}

      {error && (
        <div
          role="alert"
          style={{
            marginTop: 10,
            padding: "8px 10px",
            borderRadius: "var(--bc-radius-sm)",
            background: "var(--bc-danger-soft)",
            color: "var(--bc-danger)",
            fontSize: 12,
          }}
        >
          {error}
        </div>
      )}

      {isLocalMode && !active?.airgapped && (
        <div style={{ marginTop: 12, display: "flex", flexDirection: "column", gap: 10 }}>
          <label className="bc-field">
            <span className="bc-field-label">Marketplace Hub URL</span>
            <input
              type="url"
              className="bc-input mono"
              data-testid="marketplace-settings-hub-url"
              value={hubUrl}
              onChange={(e) => setHubUrl(e.target.value)}
              disabled={busy}
            />
          </label>
          <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
            {active?.consent === "accepted" ? (
              <>
                <button
                  type="button"
                  className="bc-btn bc-btn-primary bc-btn-sm"
                  data-testid="marketplace-sync-now-btn"
                  disabled={busy}
                  onClick={() => void handleSyncNow()}
                >
                  {busy ? "Syncing…" : "Sync now"}
                </button>
                <button
                  type="button"
                  className="bc-btn bc-btn-secondary bc-btn-sm"
                  data-testid="marketplace-disconnect-btn"
                  disabled={busy}
                  onClick={() => void handleSetConsent("declined")}
                >
                  Disconnect (Stay Offline)
                </button>
              </>
            ) : (
              <button
                type="button"
                className="bc-btn bc-btn-primary bc-btn-sm"
                data-testid="marketplace-connect-btn"
                disabled={busy}
                onClick={() => void handleSetConsent("accepted")}
              >
                {busy ? "Connecting…" : "Connect to Marketplace"}
              </button>
            )}
          </div>
        </div>
      )}
    </div>
  );
}
