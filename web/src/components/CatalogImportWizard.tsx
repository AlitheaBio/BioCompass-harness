import React, { useState } from "react";
import { formatErrorInline, importCatalogFile } from "../api";
import type { CatalogManifest, IngestFormat, IngestMode } from "../types";
import { IconClose, IconDatabase } from "./Icons";

export interface CatalogImportWizardProps {
  mode: "local" | "cloud";
  defaultNamespace: string;
  activeCatalog: CatalogManifest | null;
  onClose: () => void;
  onImported: (catalog: CatalogManifest) => Promise<void> | void;
}

function inferFormat(filename: string): IngestFormat {
  const lower = filename.toLowerCase();
  if (lower.endsWith(".parquet") || lower.endsWith(".pq")) return "parquet";
  if (lower.endsWith(".tsv") || lower.endsWith(".tab")) return "tsv";
  return "csv";
}

export function CatalogImportWizard({
  mode,
  defaultNamespace,
  activeCatalog,
  onClose,
  onImported,
}: CatalogImportWizardProps) {
  const [file, setFile] = useState<File | null>(null);
  const [format, setFormat] = useState<IngestFormat>("csv");
  const [ingestMode, setIngestMode] = useState<IngestMode>("replace");
  const [targetCatalogId, setTargetCatalogId] = useState("");
  const [targetTable, setTargetTable] = useState("records");
  const [primaryKeyRaw, setPrimaryKeyRaw] = useState("");
  const [catalogName, setCatalogName] = useState<string>("");
  const [organism, setOrganism] = useState("");
  const [description, setDescription] = useState<string>("");

  const [busy, setBusy] = useState(false);
  const [progressStage, setProgressStage] = useState<string>("");
  const [progressFraction, setProgressFraction] = useState<number>(0);
  const [error, setError] = useState<string | null>(null);

  const handleFilePicked = (picked: File | null) => {
    setFile(picked);
    if (picked) {
      setFormat(inferFormat(picked.name));
      if (!targetCatalogId && defaultNamespace) {
        const name = picked.name.replace(/\.[^.]+$/, "").toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "");
        if (name) setTargetCatalogId(`${defaultNamespace}/${name}`);
      }
    }
  };

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!file) {
      setError("Choose a CSV, TSV, or Parquet file to import.");
      return;
    }
    const pkCols = primaryKeyRaw
      .split(",")
      .map((s) => s.trim())
      .filter(Boolean);

    setError(null);
    setBusy(true);
    setProgressStage("Preparing import…");
    setProgressFraction(0.05);
    try {
      const res = await importCatalogFile({
        mode,
        file,
        format,
        ingestMode,
        targetCatalogId: targetCatalogId.trim(),
        targetTable: targetTable.trim() || "records",
        catalogName: catalogName.trim() || undefined,
        catalogDescription: description.trim() || undefined,
        organism: organism.trim() || "Unspecified",
        primaryKey: pkCols,
        onProgress: (stage, frac) => {
          setProgressStage(stage);
          setProgressFraction(frac);
        },
      });
      await onImported(res.catalog);
      onClose();
    } catch (err: unknown) {
      setError(formatErrorInline(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="bc-modal-backdrop" onClick={() => !busy && onClose()}>
      <form
        className="bc-modal"
        style={{ maxWidth: 540 }}
        data-testid="catalog-import-wizard"
        role="dialog"
        aria-label="Import table"
        onClick={(e) => e.stopPropagation()}
        onSubmit={handleSubmit}
      >
        <div className="bc-drawer-header">
          <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
            <IconDatabase size={16} />
            <div>
              <h2 className="bc-drawer-title">Import table</h2>
              <p className="bc-caption" style={{ margin: "2px 0 0" }}>
                {mode === "cloud"
                  ? "Upload to your workspace and save a versioned table."
                  : "Save a versioned table on this device."}
              </p>
            </div>
          </div>
          <button
            type="button"
            className="bc-icon-btn"
            disabled={busy}
            aria-label="Close import wizard"
            onClick={onClose}
          >
            <IconClose size={15} />
          </button>
        </div>

        <div className="bc-drawer-body" style={{ display: "grid", gap: 12 }}>
          {error && (
            <div
              role="alert"
              data-testid="import-wizard-error"
              style={{
                padding: "9px 12px",
                borderRadius: "var(--bc-radius-sm)",
                background: "var(--bc-danger-soft)",
                color: "var(--bc-danger)",
                fontSize: 12.5,
              }}
            >
              {error}
            </div>
          )}

          <div>
            <label className="bc-filter-label" htmlFor="import-file-input">
              Source File (.csv, .tsv, .parquet)
            </label>
            <input
              id="import-file-input"
              data-testid="import-file-input"
              type="file"
              accept=".csv,.tsv,.tab,.parquet,.pq"
              disabled={busy}
              className="bc-input"
              style={{ width: "100%", padding: 8 }}
              onChange={(e) => handleFilePicked(e.target.files?.[0] ?? null)}
            />
          </div>

          <div className="bc-filter-row-2">
            <div>
              <label className="bc-filter-label" htmlFor="import-format-select">
                Format
              </label>
              <select
                id="import-format-select"
                data-testid="import-format-select"
                className="bc-input mono"
                style={{ width: "100%" }}
                disabled={busy}
                value={format}
                onChange={(e) => setFormat(e.target.value as IngestFormat)}
              >
                <option value="csv">csv</option>
                <option value="tsv">tsv</option>
                <option value="parquet">parquet</option>
              </select>
            </div>

            <div>
              <label className="bc-filter-label" htmlFor="import-mode-select">
                Commit Mode
              </label>
              <select
                id="import-mode-select"
                data-testid="import-mode-select"
                className="bc-input mono"
                style={{ width: "100%" }}
                disabled={busy}
                value={ingestMode}
                onChange={(e) => setIngestMode(e.target.value as IngestMode)}
              >
                <option value="replace">replace (new / overwrite table)</option>
                <option value="append">append (add distinct PK rows)</option>
                <option value="edit">edit (upsert by primary key)</option>
              </select>
            </div>
          </div>

          <div className="bc-filter-row-2">
            <div>
              <label className="bc-filter-label" htmlFor="import-catalog-id">
                Target Catalog ID (&lt;namespace&gt;/&lt;slug&gt;)
              </label>
              <input
                id="import-catalog-id"
                data-testid="import-catalog-id"
                type="text"
                required
                disabled={busy}
                className="bc-input mono"
                style={{ width: "100%" }}
                placeholder={`${defaultNamespace || "namespace"}/dataset-name`}
                value={targetCatalogId}
                onChange={(e) => setTargetCatalogId(e.target.value)}
              />
              {activeCatalog && !activeCatalog.id.toLowerCase().startsWith("bc-private-v1/") && (
                <button className="bc-btn" type="button" disabled={busy} onClick={() => {
                  setTargetCatalogId(activeCatalog.id);
                  setTargetTable(activeCatalog.tables?.[0]?.name ?? "records");
                  setPrimaryKeyRaw(activeCatalog.tables?.[0]?.primary_key?.join(", ") ?? "");
                  setOrganism(activeCatalog.organism ?? "");
                }}>Use selected catalog</button>
              )}
            </div>

            <div>
              <label className="bc-filter-label" htmlFor="import-table-name">
                Table Name
              </label>
              <input
                id="import-table-name"
                data-testid="import-table-name"
                type="text"
                required
                disabled={busy}
                className="bc-input mono"
                style={{ width: "100%" }}
                placeholder="records"
                value={targetTable}
                onChange={(e) => setTargetTable(e.target.value)}
              />
            </div>
          </div>

          <div className="bc-filter-row-2">
            <div>
              <label className="bc-filter-label" htmlFor="import-primary-key">
                Primary Key Column(s) (required for edit &amp; Saved Views)
              </label>
              <input
                id="import-primary-key"
                data-testid="import-primary-key"
                type="text"
                disabled={busy}
                className="bc-input mono"
                style={{ width: "100%" }}
                placeholder="e.g. record_id"
                value={primaryKeyRaw}
                onChange={(e) => setPrimaryKeyRaw(e.target.value)}
              />
            </div>

            <div>
              <label className="bc-filter-label" htmlFor="import-organism">
                Organism (optional)
              </label>
              <input
                id="import-organism"
                type="text"
                disabled={busy}
                className="bc-input"
                style={{ width: "100%" }}
                value={organism}
                onChange={(e) => setOrganism(e.target.value)}
              />
            </div>
          </div>

          <div>
            <label className="bc-filter-label" htmlFor="import-catalog-name">
              Display Title (optional for new catalogs)
            </label>
            <input
              id="import-catalog-name"
              data-testid="import-catalog-name"
              type="text"
              disabled={busy}
              className="bc-input"
              style={{ width: "100%" }}
              placeholder="Dataset title"
              value={catalogName}
              onChange={(e) => setCatalogName(e.target.value)}
            />
          </div>

          <div>
            <label className="bc-filter-label" htmlFor="import-catalog-desc">
              Provenance / Cohort Notes (optional)
            </label>
            <input
              id="import-catalog-desc"
              type="text"
              disabled={busy}
              className="bc-input"
              style={{ width: "100%" }}
              placeholder="Describe this dataset"
              value={description}
              onChange={(e) => setDescription(e.target.value)}
            />
          </div>

          {busy && (
            <div data-testid="import-progress-bar" style={{ marginTop: 4 }}>
              <div
                className="bc-caption"
                style={{ display: "flex", justifyContent: "space-between", marginBottom: 4 }}
              >
                <span>{progressStage}</span>
                <span className="mono">{Math.round(progressFraction * 100)}%</span>
              </div>
              <div
                style={{
                  width: "100%",
                  height: 7,
                  borderRadius: 999,
                  background: "var(--bc-bg-subtle)",
                  overflow: "hidden",
                }}
              >
                <div
                  style={{
                    width: `${Math.max(5, Math.round(progressFraction * 100))}%`,
                    height: "100%",
                    background: "var(--bc-accent)",
                    transition: "width 180ms ease",
                  }}
                />
              </div>
            </div>
          )}
        </div>

        <div className="bc-drawer-footer">
          <button
            type="button"
            className="bc-btn"
            disabled={busy}
            onClick={onClose}
          >
            Cancel
          </button>
          <button
            type="submit"
            className="bc-btn bc-btn-primary"
            data-testid="import-submit-btn"
            disabled={busy || !file}
          >
            {busy ? "Importing & Sealing…" : "Validate & Seal Snapshot"}
          </button>
        </div>
      </form>
    </div>
  );
}
