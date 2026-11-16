import React, { useEffect, useMemo, useState } from "react";
import { createRoot } from "react-dom/client";
import {
  createPluginClient,
  type PluginHostContext,
  type SdkPeptideRecord,
  type SdkRunQuote,
} from "@biocompass/plugin-sdk";

interface GeneHotspotRow {
  gene_symbol: string;
  ligands: number;
  strong_binders: number;
  best_affinity_nm: number;
  mean_intensity_log2: number;
  max_donors: number;
}

function InteractiveExplorerApp() {
  const client = useMemo(() => createPluginClient({ timeoutMs: 15_000 }), []);
  const [context, setContext] = useState<PluginHostContext | null>(null);
  const [maxIc50, setMaxIc50] = useState<number>(250);
  const [previewRows, setPreviewRows] = useState<SdkPeptideRecord[]>([]);
  const [preparedQuote, setPreparedQuote] = useState<SdkRunQuote | null>(null);
  const [statusMsg, setStatusMsg] = useState<string>("Connecting to BioCompass host over postMessage JSON-RPC 2.0...");

  useEffect(() => {
    let active = true;
    void client
      .getContext()
      .then((ctx) => {
        if (!active) return;
        setContext(ctx);
        setStatusMsg(`Connected to host · Grant: ${ctx.activeGrantLabel}`);
      })
      .catch((err: unknown) => {
        if (!active) return;
        setStatusMsg(`Host bridge error: ${err instanceof Error ? err.message : String(err)}`);
      });

    const unsub = client.onContextUpdate((updated) => {
      setContext(updated);
      setStatusMsg(`Updated from host · Grant: ${updated.activeGrantLabel}`);
    });

    return () => {
      active = false;
      unsub();
      client.dispose();
    };
  }, [client]);

  const handlePreviewCohort = async (): Promise<void> => {
    try {
      setStatusMsg("Querying DuckDB catalog via host JSON-RPC 2.0...");
      const res = await client.queryCatalog({
        max_affinity_nm: maxIc50,
        limit: 8,
      });
      setPreviewRows(res.rows);
      setStatusMsg(
        `Loaded ${res.rows.length} of ${res.total_in_scope} in-scope ligands (masked=${String(res.sequence_masked)})`,
      );
    } catch (err: unknown) {
      setStatusMsg(`Catalog RPC error: ${err instanceof Error ? err.message : String(err)}`);
    }
  };

  const handlePrepareRun = async (): Promise<void> => {
    try {
      setStatusMsg("Requesting signed Froglet Quote from host...");
      const quote = await client.prepareRun({
        alleles: ["HLA-A*02:01", "HLA-A*03:01", "HLA-B*07:02", "HLA-B*44:02"],
        max_ic50_nm: maxIc50,
        min_donors: 1,
      });
      setPreparedQuote(quote);
      setStatusMsg(
        `Quote ${quote.run_id} signed — approve in the BioCompass Host Consent Banner above to execute container.`,
      );
    } catch (err: unknown) {
      setStatusMsg(`Prepare RPC error: ${err instanceof Error ? err.message : String(err)}`);
    }
  };

  const latestResult = context?.latestRun?.result_json ?? null;
  const hotspots: GeneHotspotRow[] = Array.isArray(latestResult?.["gene_hotspots"])
    ? (latestResult["gene_hotspots"] as GeneHotspotRow[])
    : [];

  return (
    <div
      style={{
        fontFamily: "-apple-system, BlinkMacSystemFont, 'Inter', sans-serif",
        background: "#0c1117",
        color: "#e6edf3",
        minHeight: "100vh",
        padding: "16px 20px",
        boxSizing: "border-box",
      }}
    >
      <div
        style={{
          display: "flex",
          justifyContent: "space-between",
          alignItems: "center",
          borderBottom: "1px solid #212936",
          paddingBottom: 12,
          marginBottom: 14,
          gap: 12,
          flexWrap: "wrap",
        }}
      >
        <div>
          <div style={{ fontSize: 15, fontWeight: 600, color: "#f8fafc" }}>
            HLA Antigenic Gene Hotspot Explorer (Custom Vite Sandbox UI)
          </div>
          <div
            data-testid="sandbox-status"
            style={{ fontSize: 12, color: "#94a3b8", marginTop: 3 }}
          >
            {statusMsg}
          </div>
        </div>
        <div style={{ display: "flex", gap: 8, alignItems: "center" }}>
          <span
            data-testid="sandbox-origin-badge"
            style={{
              fontSize: 11,
              fontFamily: "monospace",
              padding: "4px 8px",
              borderRadius: 6,
              background: "#161f2c",
              border: "1px solid #2d3a4f",
              color: "#38bdf8",
            }}
          >
            origin={window.origin}
          </span>
          <span
            data-testid="sandbox-mask-badge"
            style={{
              fontSize: 11,
              fontWeight: 600,
              padding: "4px 8px",
              borderRadius: 6,
              background: context?.sequenceMasked ? "#3b1d16" : "#112b21",
              color: context?.sequenceMasked ? "#fb923c" : "#34d399",
              border: "1px solid currentColor",
            }}
          >
            {context?.sequenceMasked ? "Sequences Masked ('***')" : "Full IUPAC Access"}
          </span>
        </div>
      </div>

      <div
        style={{
          display: "flex",
          gap: 12,
          alignItems: "center",
          flexWrap: "wrap",
          background: "#131b26",
          padding: "12px 14px",
          borderRadius: 8,
          border: "1px solid #222e3f",
          marginBottom: 16,
        }}
      >
        <label style={{ fontSize: 12, color: "#cbd5e1", display: "flex", alignItems: "center", gap: 8 }}>
          Max IC50 Cutoff (nM):
          <input
            type="number"
            min={10}
            max={1000}
            value={maxIc50}
            onChange={(e) => setMaxIc50(Number(e.target.value) || 250)}
            style={{
              width: 84,
              background: "#0c1117",
              color: "#f8fafc",
              border: "1px solid #334155",
              borderRadius: 6,
              padding: "5px 8px",
            }}
          />
        </label>

        <button
          type="button"
          data-testid="sdk-query-btn"
          onClick={() => void handlePreviewCohort()}
          style={{
            background: "#1e293b",
            color: "#f8fafc",
            border: "1px solid #38bdf8",
            borderRadius: 6,
            padding: "6px 12px",
            fontSize: 12,
            fontWeight: 600,
            cursor: "pointer",
          }}
        >
          Preview Scoped Cohort (SDK RPC)
        </button>

        <button
          type="button"
          data-testid="sdk-prepare-btn"
          onClick={() => void handlePrepareRun()}
          style={{
            background: "#f56e3d",
            color: "#ffffff",
            border: "none",
            borderRadius: 6,
            padding: "7px 14px",
            fontSize: 12,
            fontWeight: 600,
            cursor: "pointer",
          }}
        >
          Prepare Container Run Quote (5.00 ACT)
        </button>
      </div>

      {preparedQuote && (
        <div
          data-testid="sdk-quote-banner"
          style={{
            background: "#172334",
            border: "1px solid #38bdf8",
            borderRadius: 8,
            padding: "10px 14px",
            marginBottom: 16,
            fontSize: 12,
          }}
        >
          <strong>Signed Froglet Quote Ready:</strong>{" "}
          <code>{preparedQuote.run_id}</code> · workload_hash{" "}
          <code>{preparedQuote.workload_hash.slice(0, 16)}...</code> · Confirm in host header to run OCI container.
        </div>
      )}

      {previewRows.length > 0 && (
        <div style={{ marginBottom: 18 }}>
          <div style={{ fontSize: 12, fontWeight: 600, color: "#94a3b8", marginBottom: 6 }}>
            Scoped DuckDB Preview via @biocompass/plugin-sdk
          </div>
          <table
            data-testid="sdk-preview-table"
            style={{ width: "100%", borderCollapse: "collapse", fontSize: 12 }}
          >
            <thead>
              <tr style={{ textAlign: "left", borderBottom: "1px solid #263244", color: "#94a3b8" }}>
                <th style={{ padding: "6px 8px" }}>Peptide ID</th>
                <th style={{ padding: "6px 8px" }}>Sequence</th>
                <th style={{ padding: "6px 8px" }}>Allele</th>
                <th style={{ padding: "6px 8px" }}>Gene</th>
                <th style={{ padding: "6px 8px" }}>Organ</th>
                <th style={{ padding: "6px 8px" }}>IC50 (nM)</th>
              </tr>
            </thead>
            <tbody>
              {previewRows.map((row) => (
                <tr key={row.peptide_id} style={{ borderBottom: "1px solid #1a2330" }}>
                  <td style={{ padding: "6px 8px", fontFamily: "monospace" }}>{row.peptide_id}</td>
                  <td style={{ padding: "6px 8px", fontFamily: "monospace", color: "#f56e3d" }}>
                    {row.sequence}
                  </td>
                  <td style={{ padding: "6px 8px" }}>{row.hla_allele}</td>
                  <td style={{ padding: "6px 8px", fontWeight: 600 }}>{row.gene_symbol}</td>
                  <td style={{ padding: "6px 8px" }}>{row.organ}</td>
                  <td style={{ padding: "6px 8px" }}>{row.affinity_nm.toFixed(1)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}

      {hotspots.length > 0 && (
        <div>
          <div style={{ fontSize: 12, fontWeight: 600, color: "#34d399", marginBottom: 6 }}>
            Container Output (`/out/result.json`) — Top Antigenic Source Genes
          </div>
          <table
            data-testid="sdk-hotspots-table"
            style={{ width: "100%", borderCollapse: "collapse", fontSize: 12 }}
          >
            <thead>
              <tr style={{ textAlign: "left", borderBottom: "1px solid #263244", color: "#94a3b8" }}>
                <th style={{ padding: "6px 8px" }}>Gene</th>
                <th style={{ padding: "6px 8px" }}>Total Ligands</th>
                <th style={{ padding: "6px 8px" }}>Strong Binders (&le;50 nM)</th>
                <th style={{ padding: "6px 8px" }}>Best IC50 (nM)</th>
                <th style={{ padding: "6px 8px" }}>Mean MS log2</th>
              </tr>
            </thead>
            <tbody>
              {hotspots.map((g) => (
                <tr key={g.gene_symbol} style={{ borderBottom: "1px solid #1a2330" }}>
                  <td style={{ padding: "6px 8px", fontWeight: 700 }}>{g.gene_symbol}</td>
                  <td style={{ padding: "6px 8px" }}>{g.ligands}</td>
                  <td style={{ padding: "6px 8px", color: "#34d399", fontWeight: 600 }}>
                    {g.strong_binders}
                  </td>
                  <td style={{ padding: "6px 8px" }}>{g.best_affinity_nm.toFixed(1)}</td>
                  <td style={{ padding: "6px 8px" }}>{g.mean_intensity_log2.toFixed(2)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}

const container = document.getElementById("plugin-root");
if (container) {
  createRoot(container).render(
    <React.StrictMode>
      <InteractiveExplorerApp />
    </React.StrictMode>,
  );
}
