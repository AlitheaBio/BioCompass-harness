import React, { useState } from "react";
import { formatErrorInline, verifySavedView } from "../api";
import type {
  SavedViewRecord,
  SavedViewStatus,
  SavedViewVerificationReport,
} from "../types";
import { IconBookmark, IconCheck, IconClose, IconShield } from "./Icons";

export interface SavedViewsPanelProps {
  views: SavedViewRecord[];
  activeViewId: string | null;
  onApplyView: (view: SavedViewRecord) => void;
  onDeleteView: (viewId: string) => Promise<void>;
  onVerifiedView: (
    viewId: string,
    report: SavedViewVerificationReport,
  ) => void;
}

function statusBadgeSpec(status?: SavedViewStatus | null): {
  label: string;
  cls: string;
} {
  switch (status) {
    case "verified":
      return { label: "Verified", cls: "bc-badge-success" };
    case "data_changed":
      return { label: "Data Changed", cls: "bc-badge-warn" };
    case "access_changed":
      return { label: "Access Changed", cls: "bc-badge-danger" };
    default:
      return { label: "Pinned", cls: "bc-badge-neutral" };
  }
}

export function SavedViewsPanel({
  views,
  activeViewId,
  onApplyView,
  onDeleteView,
  onVerifiedView,
}: SavedViewsPanelProps) {
  const [verifyingId, setVerifyingId] = useState<string | null>(null);
  const [lastReport, setLastReport] = useState<SavedViewVerificationReport | null>(
    null,
  );
  const [error, setError] = useState<string | null>(null);

  if (views.length === 0) return null;

  const handleVerify = async (e: React.MouseEvent, viewId: string) => {
    e.stopPropagation();
    setError(null);
    setVerifyingId(viewId);
    try {
      const report = await verifySavedView(viewId, true);
      setLastReport(report);
      onVerifiedView(viewId, report);
    } catch (err: unknown) {
      setError(formatErrorInline(err));
    } finally {
      setVerifyingId(null);
    }
  };

  return (
    <div data-testid="saved-views-panel" style={{ marginBottom: 10 }}>
      <div
        className="bc-saved-views-bar"
        data-testid="saved-views-bar"
        style={{ flexWrap: "wrap", gap: 8 }}
      >
        <span
          className="bc-caption"
          style={{ display: "inline-flex", alignItems: "center", gap: 5 }}
        >
          <IconBookmark size={12} /> Reproducible Saved Views:
        </span>

        {views.map((v) => {
          const badge = statusBadgeSpec(v.last_status);
          const isActive = activeViewId === v.view_id;
          return (
            <div
              key={v.view_id}
              data-testid={`saved-view-pill-${v.view_id}`}
              className="bc-saved-view-pill"
              data-active={String(isActive)}
              style={{
                display: "inline-flex",
                alignItems: "center",
                gap: 6,
                padding: "3px 8px",
              }}
            >
              <button
                type="button"
                className="bc-saved-view-apply"
                onClick={() => onApplyView(v)}
                title={`Pinned snapshot #${v.primary_source.snapshot_id} · ${v.row_count.toLocaleString()} rows · PK sha256:${v.selection_digest.slice(0, 12)}…`}
              >
                <span style={{ fontWeight: 500 }}>{v.name}</span>
                <span className="mono bc-caption" style={{ marginLeft: 5 }}>
                  #{v.primary_source.snapshot_id} ({v.row_count})
                </span>
              </button>

              <span
                className={`bc-badge ${badge.cls}`}
                data-testid={`saved-view-status-${v.view_id}`}
                style={{ fontSize: 10.5, padding: "1px 6px" }}
              >
                {badge.label}
              </span>

              {v.local_provenance && (
                <span
                  className="bc-chip mono"
                  data-testid={`saved-view-local-badge-${v.view_id}`}
                  title="Verification ran against local DuckLake files on this machine"
                  style={{ fontSize: 10 }}
                >
                  Verified locally on this machine
                </span>
              )}

              <button
                type="button"
                className="bc-link-btn"
                data-testid={`verify-saved-view-btn-${v.view_id}`}
                disabled={verifyingId === v.view_id}
                onClick={(e) => void handleVerify(e, v.view_id)}
                title="Re-evaluate primary-key selection digest against pinned snapshot and current head"
                style={{ fontSize: 11 }}
              >
                <IconShield size={11} />{" "}
                {verifyingId === v.view_id ? "Verifying…" : "Verify"}
              </button>

              <button
                type="button"
                className="bc-saved-view-del"
                aria-label={`Delete saved view ${v.name}`}
                onClick={() => void onDeleteView(v.view_id)}
              >
                <IconClose size={10} />
              </button>
            </div>
          );
        })}
      </div>

      {lastReport && (
        <div
          data-testid="saved-view-verification-banner"
          style={{
            marginTop: 6,
            padding: "7px 11px",
            borderRadius: "var(--bc-radius-sm)",
            background: "var(--bc-bg-subtle)",
            border: "1px solid var(--bc-border)",
            display: "flex",
            alignItems: "center",
            justifyContent: "space-between",
            gap: 10,
            fontSize: 12,
          }}
        >
          <div style={{ display: "flex", alignItems: "center", gap: 8, flexWrap: "wrap" }}>
            <IconCheck size={13} />
            <strong>{statusBadgeSpec(lastReport.status).label}:</strong>
            <span>{lastReport.detail}</span>
            <span className="bc-chip mono" style={{ fontSize: 10.5 }}>
              {lastReport.provenance_label}
            </span>
            <span className="mono bc-caption">
              PK digest {lastReport.saved_selection_digest.slice(0, 14)}…
            </span>
          </div>
          <button
            type="button"
            className="bc-icon-btn"
            aria-label="Dismiss verification report"
            onClick={() => setLastReport(null)}
          >
            <IconClose size={12} />
          </button>
        </div>
      )}

      {error && (
        <div
          role="alert"
          style={{
            marginTop: 6,
            padding: "6px 10px",
            borderRadius: "var(--bc-radius-sm)",
            background: "var(--bc-danger-soft)",
            color: "var(--bc-danger)",
            fontSize: 12,
          }}
        >
          {error}
        </div>
      )}
    </div>
  );
}
