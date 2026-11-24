import React, { useState } from "react";
import {
  formatErrorInline,
  updateCatalogDataPlacement,
} from "../api";
import type {
  CatalogDataPlacement,
  CatalogManifest,
} from "../types";

/**
 * Catalog Data Placement (`stays_with_alithea` vs `licensable_copy`).
 */
export function ClientCloudImportPanel({
  catalogs = [],
  onImported,
}: {
  catalogs?: CatalogManifest[];
  onImported?: () => Promise<void> | void;
}) {
  const [placementCatalogId, setPlacementCatalogId] = useState(catalogs[0]?.id ?? "");
  const [placementMode, setPlacementMode] = useState<CatalogDataPlacement>("stays_with_alithea");

  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [statusMsg, setStatusMsg] = useState<string | null>(null);

  const runAction = async (fn: () => Promise<string>) => {
    setBusy(true);
    setError(null);
    setStatusMsg(null);
    try {
      const msg = await fn();
      setStatusMsg(msg);
      await onImported?.();
    } catch (err: unknown) {
      setError(formatErrorInline(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="bc-settings-stack" data-testid="client-cloud-import-panel">
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

      <section className="bc-card">
        <div className="bc-card-header">
          <div>
            <span className="bc-card-title">Catalog Data Placement</span>
            <p className="bc-caption" style={{ marginTop: 2, marginBottom: 0 }}>
              Mark catalogs as <code>stays_with_alithea</code> or <code>licensable_copy</code>.
            </p>
          </div>
        </div>

        {catalogs.length === 0 ? (
          <p className="bc-empty-note" style={{ marginTop: 10 }}>
            No organization catalogs available.
          </p>
        ) : (
          <div style={{ display: "flex", gap: 8, flexWrap: "wrap", alignItems: "end", marginTop: 10 }}>
            <label className="bc-field" style={{ flex: 1, minWidth: 200 }}>
              <span className="bc-field-label">Catalog</span>
              <select
                className="bc-input mono"
                data-testid="u4-placement-catalog-select"
                value={placementCatalogId}
                onChange={(e) => setPlacementCatalogId(e.target.value)}
              >
                {catalogs.map((c) => (
                  <option key={c.id} value={c.id}>
                    {c.id}
                  </option>
                ))}
              </select>
            </label>
            <label className="bc-field">
              <span className="bc-field-label">Placement Policy</span>
              <select
                className="bc-input mono"
                data-testid="u4-placement-mode-select"
                value={placementMode}
                onChange={(e) => setPlacementMode(e.target.value as CatalogDataPlacement)}
              >
                <option value="stays_with_alithea">stays_with_alithea</option>
                <option value="licensable_copy">licensable_copy</option>
              </select>
            </label>
            <button
              type="button"
              className="bc-btn bc-btn-secondary"
              data-testid="u4-placement-save-btn"
              disabled={busy || !placementCatalogId}
              onClick={() =>
                void runAction(async () => {
                  const res = await updateCatalogDataPlacement(placementCatalogId, placementMode);
                  return `Updated ${res.catalog_id} placement to ${res.data_placement}.`;
                })
              }
            >
              Update Placement
            </button>
          </div>
        )}
      </section>
    </div>
  );
}
