import React, { useState } from "react";
import {
  formatErrorInline,
  importFederatedCatalog,
  importFederatedModule,
  registerClientCloudOutbound,
  updateCatalogDataPlacement,
} from "../api";
import type {
  CatalogDataPlacement,
  CatalogManifest,
  FederatedModuleOffer,
} from "../types";

/**
 * Milestone C1 (U4) — Client Cloud Outbound Registration with Alithea,
 * Download-Offer Module Import (with OCI digest & UI-hash verification),
 * and Catalog Data Placement (`stays_with_alithea` vs `licensable_copy`) + Licensed-Copy Import.
 */
export function ClientCloudImportPanel({
  catalogs = [],
  federatedOffers = [],
  onImported,
}: {
  catalogs?: CatalogManifest[];
  federatedOffers?: FederatedModuleOffer[];
  onImported?: () => Promise<void> | void;
}) {
  const [hubUrl, setHubUrl] = useState("");
  const [orgName, setOrgName] = useState("");
  const [importOfferId, setImportOfferId] = useState("");
  const [importPluginId, setImportPluginId] = useState("");
  const [importVersion, setImportVersion] = useState("");
  const [importHubUrl, setImportHubUrl] = useState("");

  const [catOfferId, setCatOfferId] = useState("");
  const [catId, setCatId] = useState("");
  const [catHubUrl, setCatHubUrl] = useState("");

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

  const downloadOffers = federatedOffers.filter(
    (f) => (f.offer_kind ?? f.offer.action) === "download",
  );
  const datasetOffers = federatedOffers.filter(
    (f) => (f.offer_kind ?? f.offer.action) === "dataset",
  );

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

      {/* 1. Outbound Registration with Alithea Hub */}
      <section className="bc-card">
        <div className="bc-card-header">
          <div>
            <span className="bc-card-title">Outbound Hub Registration (U4)</span>
            <p className="bc-caption" style={{ marginTop: 2, marginBottom: 0 }}>
              Register this Client Cloud outbound against Alithea&apos;s hub (<code>POST /v1/registrations</code>, admin-approved).
            </p>
          </div>
        </div>
        <form
          className="bc-form-grid"
          style={{ marginTop: 10 }}
          onSubmit={(e) => {
            e.preventDefault();
            void runAction(async () => {
              const res = await registerClientCloudOutbound({
                ...(hubUrl.trim() ? { hub_url: hubUrl.trim() } : {}),
                ...(orgName.trim() ? { org_name: orgName.trim() } : {}),
              });
              return res.message ?? `Outbound registration submitted (${res.status}).`;
            });
          }}
        >
          <label className="bc-field">
            <span className="bc-field-label">Upstream Hub URL (optional, defaults to chart defaultHubUrl)</span>
            <input
              type="url"
              className="bc-input mono"
              data-testid="u4-register-hub-url"
              placeholder="https://hub.biocompass.alithea.bio"
              value={hubUrl}
              onChange={(e) => setHubUrl(e.target.value)}
            />
          </label>
          <label className="bc-field">
            <span className="bc-field-label">Organization Display Name</span>
            <input
              className="bc-input"
              placeholder="Acme Pharma R&D Cloud"
              value={orgName}
              onChange={(e) => setOrgName(e.target.value)}
            />
          </label>
          <div>
            <button
              type="submit"
              className="bc-btn bc-btn-primary"
              data-testid="u4-register-outbound-btn"
              disabled={busy}
            >
              {busy ? "Registering…" : "Register with Upstream Hub"}
            </button>
          </div>
        </form>
      </section>

      {/* 2. Import Download-Offer Module */}
      <section className="bc-card">
        <div className="bc-card-header">
          <div>
            <span className="bc-card-title">Import Download-Offer Module</span>
            <p className="bc-caption" style={{ marginTop: 2, marginBottom: 0 }}>
              Imports a federated download-offer module into this Client Cloud&apos;s registry with OCI digest and UI-bundle SHA-256 verification.
            </p>
          </div>
        </div>

        {downloadOffers.length > 0 && (
          <div className="table-scroll" style={{ marginTop: 10 }}>
            <table className="data-table">
              <thead>
                <tr>
                  <th>Offer</th>
                  <th>Module</th>
                  <th>Provider</th>
                  <th>Action</th>
                </tr>
              </thead>
              <tbody>
                {downloadOffers.map((item) => (
                  <tr key={`${item.provider}:${item.offer.offer_id}`}>
                    <td>{item.offer.title}</td>
                    <td className="mono">
                      {item.offer.plugin_id}@{item.offer.version}
                    </td>
                    <td>{item.provider}</td>
                    <td>
                      <button
                        type="button"
                        className="bc-btn bc-btn-primary bc-btn-sm"
                        disabled={busy || !item.offer.plugin_id || !item.offer.version}
                        onClick={() =>
                          void runAction(async () => {
                            const res = await importFederatedModule({
                              peer_id: item.peer_id,
                              offer_id: item.offer.offer_id,
                              plugin_id: item.offer.plugin_id!,
                              version: item.offer.version!,
                            });
                            return (
                              res.message ??
                              `Imported ${res.plugin_id ?? item.offer.plugin_id}@${res.version ?? item.offer.version} into Client Cloud.`
                            );
                          })
                        }
                      >
                        Import into Cloud
                      </button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}

        <form
          className="bc-form-grid"
          style={{ marginTop: 10 }}
          onSubmit={(e) => {
            e.preventDefault();
            if (!importOfferId.trim() || !importPluginId.trim() || !importVersion.trim()) return;
            void runAction(async () => {
              const res = await importFederatedModule({
                ...(importHubUrl.trim() ? { hub_url: importHubUrl.trim() } : {}),
                offer_id: importOfferId.trim(),
                plugin_id: importPluginId.trim(),
                version: importVersion.trim(),
              });
              return res.message ?? `Imported module ${importPluginId}@${importVersion} (${res.status}).`;
            });
          }}
        >
          <label className="bc-field">
            <span className="bc-field-label">Offer ID</span>
            <input
              className="bc-input mono"
              data-testid="u4-import-offer-id"
              placeholder="mo-..."
              required
              value={importOfferId}
              onChange={(e) => setImportOfferId(e.target.value)}
            />
          </label>
          <label className="bc-field">
            <span className="bc-field-label">Module ID</span>
            <input
              className="bc-input mono"
              placeholder="alithea/hla-motif-enrichment"
              required
              value={importPluginId}
              onChange={(e) => setImportPluginId(e.target.value)}
            />
          </label>
          <label className="bc-field">
            <span className="bc-field-label">Version</span>
            <input
              className="bc-input mono"
              placeholder="2.0.0"
              required
              value={importVersion}
              onChange={(e) => setImportVersion(e.target.value)}
            />
          </label>
          <label className="bc-field">
            <span className="bc-field-label">Source Hub URL (optional)</span>
            <input
              type="url"
              className="bc-input mono"
              placeholder="https://hub.biocompass.alithea.bio"
              value={importHubUrl}
              onChange={(e) => setImportHubUrl(e.target.value)}
            />
          </label>
          <div>
            <button
              type="submit"
              className="bc-btn bc-btn-secondary"
              data-testid="u4-import-module-btn"
              disabled={busy || !importOfferId.trim() || !importPluginId.trim() || !importVersion.trim()}
            >
              Import Module
            </button>
          </div>
        </form>
      </section>

      {/* 3. Catalog Data Placement & Licensed-Copy Catalog Import */}
      <section className="bc-card">
        <div className="bc-card-header">
          <div>
            <span className="bc-card-title">Catalog Data Placement &amp; Licensed Copies</span>
            <p className="bc-caption" style={{ marginTop: 2, marginBottom: 0 }}>
              Mark catalogs as <code>stays_with_alithea</code> or <code>licensable_copy</code>, or import an approved licensed catalog copy.
            </p>
          </div>
        </div>

        {catalogs.length > 0 && (
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

        {datasetOffers.length > 0 && (
          <div className="table-scroll" style={{ marginTop: 12 }}>
            <table className="data-table">
              <thead>
                <tr>
                  <th>Dataset Offer</th>
                  <th>Catalog</th>
                  <th>Provider</th>
                  <th>Action</th>
                </tr>
              </thead>
              <tbody>
                {datasetOffers.map((item) => (
                  <tr key={`${item.provider}:${item.offer.offer_id}`}>
                    <td>{item.offer.title}</td>
                    <td className="mono">{item.offer.catalog_id ?? item.offer.offer_id}</td>
                    <td>{item.provider}</td>
                    <td>
                      <button
                        type="button"
                        className="bc-btn bc-btn-secondary bc-btn-sm"
                        disabled={busy || !item.offer.catalog_id}
                        onClick={() =>
                          void runAction(async () => {
                            const res = await importFederatedCatalog({
                              peer_id: item.peer_id,
                              offer_id: item.offer.offer_id,
                              catalog_id: item.offer.catalog_id!,
                            });
                            return res.message ?? `Imported licensed catalog copy ${item.offer.catalog_id}.`;
                          })
                        }
                      >
                        Import Licensed Copy
                      </button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}

        <form
          className="bc-form-grid"
          style={{ marginTop: 12 }}
          onSubmit={(e) => {
            e.preventDefault();
            if (!catOfferId.trim() || !catId.trim()) return;
            void runAction(async () => {
              const res = await importFederatedCatalog({
                ...(catHubUrl.trim() ? { hub_url: catHubUrl.trim() } : {}),
                offer_id: catOfferId.trim(),
                catalog_id: catId.trim(),
              });
              return res.message ?? `Imported licensed catalog copy ${catId} (${res.status}).`;
            });
          }}
        >
          <label className="bc-field">
            <span className="bc-field-label">Dataset Offer ID</span>
            <input
              className="bc-input mono"
              placeholder="do-..."
              required
              value={catOfferId}
              onChange={(e) => setCatOfferId(e.target.value)}
            />
          </label>
          <label className="bc-field">
            <span className="bc-field-label">Catalog ID</span>
            <input
              className="bc-input mono"
              placeholder="alithea/hla-atlas-v2026.1"
              required
              value={catId}
              onChange={(e) => setCatId(e.target.value)}
            />
          </label>
          <label className="bc-field">
            <span className="bc-field-label">Source Hub URL (optional)</span>
            <input
              type="url"
              className="bc-input mono"
              placeholder="https://hub.biocompass.alithea.bio"
              value={catHubUrl}
              onChange={(e) => setCatHubUrl(e.target.value)}
            />
          </label>
          <div>
            <button
              type="submit"
              className="bc-btn bc-btn-secondary"
              data-testid="u4-import-catalog-btn"
              disabled={busy || !catOfferId.trim() || !catId.trim()}
            >
              Import Licensed Catalog Copy
            </button>
          </div>
        </form>
      </section>
    </div>
  );
}
