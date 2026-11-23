import React, { useState } from "react";
import { formatErrorInline, updateMarketplaceConsent } from "../api";
import { ownerLinkHelp } from "../localOwner";
import type { MarketplaceStatus } from "../types";

export interface MarketplaceConsentModalProps {
  status: MarketplaceStatus;
  onResolved: (updated: MarketplaceStatus) => void;
  onDismissForSession?: () => void;
}

/**
 * One-time local first-run consent prompt (`[A1]`).
 * No outbound marketplace requests are made until the user explicitly chooses to connect.
 */
export function MarketplaceConsentModal({
  status,
  onResolved,
  onDismissForSession,
}: MarketplaceConsentModalProps) {
  const [hubUrl, setHubUrl] = useState<string>(status.hub_url || status.default_hub_url);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const submitDecision = async (decision: "accepted" | "declined") => {
    setBusy(true);
    setError(null);
    try {
      const updated = await updateMarketplaceConsent({
        decision,
        ...(decision === "accepted" && hubUrl.trim() ? { hub_url: hubUrl.trim() } : {}),
        sync_now: decision === "accepted",
      });
      onResolved(updated);
    } catch (err: unknown) {
      const msg = formatErrorInline(err);
      setError(
        msg.toLowerCase().includes("owner link") || msg.toLowerCase().includes("unauthenticated")
          ? `${msg} ${ownerLinkHelp}`
          : msg,
      );
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="bc-modal-backdrop" role="presentation">
      <div
        className="bc-card"
        role="dialog"
        aria-modal="true"
        aria-labelledby="marketplace-consent-title"
        data-testid="marketplace-consent-modal"
        style={{
          margin: "auto",
          width: "min(520px, 94vw)",
          padding: 24,
          display: "flex",
          flexDirection: "column",
          gap: 14,
        }}
      >
        <div>
          <h2 id="marketplace-consent-title" className="bc-card-title" style={{ fontSize: 18 }}>
            Connect to Global BioCompass Marketplace?
          </h2>
          <p className="bc-caption" style={{ marginTop: 6, lineHeight: 1.55 }}>
            BioCompass starts completely offline on a fresh local data directory and has made{" "}
            <strong>zero outbound marketplace requests</strong>. Choose whether to connect to the
            global Froglet marketplace feed to discover verified scientific modules and dataset
            offers, or stay strictly offline. You can change this at any time in Settings.
          </p>
        </div>

        <label className="bc-field">
          <span className="bc-field-label">Marketplace Hub URL</span>
          <input
            type="url"
            data-testid="marketplace-consent-hub-url"
            className="bc-input mono"
            value={hubUrl}
            onChange={(e) => setHubUrl(e.target.value)}
            disabled={busy}
            placeholder={status.default_hub_url}
          />
        </label>

        {error && (
          <div
            role="alert"
            data-testid="marketplace-consent-error"
            style={{
              padding: "8px 12px",
              borderRadius: "var(--bc-radius-sm)",
              background: "var(--bc-danger-soft)",
              color: "var(--bc-danger)",
              fontSize: 12,
            }}
          >
            {error}
          </div>
        )}

        <div style={{ display: "flex", gap: 8, justifyContent: "flex-end", flexWrap: "wrap" }}>
          {onDismissForSession && (
            <button
              type="button"
              className="bc-btn bc-btn-ghost"
              disabled={busy}
              onClick={onDismissForSession}
            >
              Ask later
            </button>
          )}
          <button
            type="button"
            className="bc-btn bc-btn-secondary"
            data-testid="marketplace-consent-decline"
            disabled={busy}
            onClick={() => void submitDecision("declined")}
          >
            Stay Offline
          </button>
          <button
            type="button"
            className="bc-btn bc-btn-primary"
            data-testid="marketplace-consent-accept"
            disabled={busy}
            onClick={() => void submitDecision("accepted")}
          >
            {busy ? "Connecting…" : "Connect & Sync Feed"}
          </button>
        </div>
      </div>
    </div>
  );
}
