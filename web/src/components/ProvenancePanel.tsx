import React, { useState } from "react";
import { fetchRunProvenance, formatErrorInline } from "../api";
import type { ProvenanceBundle, RunRecord } from "../types";
import { IconDownload, IconShield } from "./Icons";

/**
 * Milestone C1 (W6 / H5) — Run Provenance Chain Viewer & `bc.provenance.v1` Bundle Downloader.
 */
export function ProvenancePanel({ run }: { run: RunRecord }) {
  const [bundle, setBundle] = useState<ProvenanceBundle | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const loadBundle = async (download: boolean) => {
    setLoading(true);
    setError(null);
    try {
      const data = bundle ?? (await fetchRunProvenance(run.run_id));
      setBundle(data);
      if (download) {
        const blob = new Blob([JSON.stringify(data, null, 2)], {
          type: "application/json;charset=utf-8",
        });
        const url = URL.createObjectURL(blob);
        const link = document.createElement("a");
        link.href = url;
        link.download = `provenance-${run.run_id.replace(/[^a-zA-Z0-9_-]/g, "_")}.bc.provenance.v1.json`;
        link.style.display = "none";
        document.body.appendChild(link);
        link.click();
        link.remove();
        window.setTimeout(() => URL.revokeObjectURL(url), 1000);
      }
    } catch (err: unknown) {
      setError(formatErrorInline(err));
    } finally {
      setLoading(false);
    }
  };

  const inlineInputs = run.inputs ?? [];

  return (
    <div className="bc-card" data-testid="provenance-chain-panel" style={{ marginTop: 12 }}>
      <div className="bc-card-header">
        <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
          <IconShield size={15} />
          <span className="bc-card-title">Provenance Chain (bc.provenance.v1)</span>
        </div>
        <div style={{ display: "flex", gap: 8 }}>
          <button
            type="button"
            className="bc-btn bc-btn-ghost bc-btn-sm"
            data-testid="provenance-inspect-btn"
            disabled={loading}
            onClick={() => void loadBundle(false)}
          >
            {loading ? "Loading…" : bundle ? "Refresh Lineage" : "Inspect Lineage"}
          </button>
          <button
            type="button"
            className="bc-btn bc-btn-secondary bc-btn-sm"
            data-testid="provenance-download-btn"
            disabled={loading}
            onClick={() => void loadBundle(true)}
          >
            <IconDownload size={13} />
            <span>Download bc.provenance.v1</span>
          </button>
        </div>
      </div>

      <div className="bc-caption" style={{ marginTop: 6 }}>
        {run.parent_run_id && (
          <div>
            Parent run: <code className="mono">{run.parent_run_id}</code>
          </div>
        )}
        {run.agent_credential_id && (
          <div>
            Agent credential: <code className="mono">{run.agent_credential_id}</code>
          </div>
        )}
        {inlineInputs.length > 0 ? (
          <div style={{ marginTop: 4 }}>
            Chained inputs ({inlineInputs.length}):{" "}
            {inlineInputs.map((inp) => (
              <span key={`${inp.alias}:${inp.sha256}`} className="bc-badge bc-badge-neutral mono" style={{ marginRight: 6 }}>
                {inp.alias} &larr; {inp.source_run_id.slice(0, 10)}/{inp.name}
              </span>
            ))}
          </div>
        ) : (
          <div>No chained run inputs attached to this run.</div>
        )}
      </div>

      {error && (
        <p role="alert" className="bc-caption" style={{ color: "var(--bc-danger)", marginTop: 8 }}>
          {error}
        </p>
      )}

      {bundle && (
        <div style={{ marginTop: 10 }} data-testid="provenance-bundle-summary">
          <div className="bc-meta-grid">
            <div className="bc-meta-item">
              <span className="bc-caption">Schema</span>
              <span className="mono">{bundle.schema}</span>
            </div>
            <div className="bc-meta-item">
              <span className="bc-caption">Runs in Lineage</span>
              <strong className="mono">{bundle.runs.length}</strong>
            </div>
            <div className="bc-meta-item">
              <span className="bc-caption">Lineage Edges</span>
              <strong className="mono">{bundle.edges.length}</strong>
            </div>
            <div className="bc-meta-item">
              <span className="bc-caption">Unresolved Ancestors</span>
              <strong className="mono">{bundle.unresolved_ancestors.length}</strong>
            </div>
          </div>

          {bundle.runs.length > 0 && (
            <ul className="bc-about-contract-list mono" style={{ marginTop: 8 }}>
              {bundle.runs.map((entry) => (
                <li key={entry.run_id}>
                  <strong>{entry.run_id.slice(0, 16)}</strong> · {entry.plugin_id}@{entry.plugin_version} ·{" "}
                  {entry.status} · {entry.froglet_chain.length} Froglet docs
                  {entry.inputs.length > 0 ? ` · ${entry.inputs.length} input(s)` : ""}
                  {entry.child_receipt_hashes.length > 0
                    ? ` · ${entry.child_receipt_hashes.length} child receipt(s)`
                    : ""}
                </li>
              ))}
            </ul>
          )}

          {bundle.unresolved_ancestors.length > 0 && (
            <p className="bc-caption" style={{ color: "var(--bc-warn)", marginTop: 6 }}>
              Unresolved ancestors: {bundle.unresolved_ancestors.join(", ")}
            </p>
          )}
        </div>
      )}
    </div>
  );
}
