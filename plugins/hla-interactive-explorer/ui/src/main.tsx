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
  const [statusMsg, setStatusMsg] = useState<string>(
    "Connecting to BioCompass host over postMessage JSON-RPC 2.0...",
  );

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
        `Quote ${quote.run_id} signed — approve in the BioCompass Approval Card below to execute container.`,
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
        fontFamily: "Inter, system-ui, -apple-system, BlinkMacSystemFont, sans-serif",
        background: "#ffffff",
        color: "#0f1115",
        minHeight: "100vh",
        padding: "18px 22px",
        boxSizing: "border-box",
        WebkitFontSmoothing: "antialiased",
      }}
    >
      <div
        style={{
          display: "flex",
          justifyContent: "space-between",
          alignItems: "center",
          borderBottom: "1px solid rgba(15, 17, 21, 0.08)",
          paddingBottom: 12,
          marginBottom: 14,
          gap: 12,
          flexWrap: "wrap",
        }}
      >
        <div>
          <div style={{ fontSize: 15, fontWeight: 500, color: "#0f1115" }}>
            HLA Antigenic Gene Hotspot Explorer
          </div>
          <div
            data-testid="sandbox-status"
            style={{ fontSize: 12.5, color: "#60646c", marginTop: 3 }}
          >
            {statusMsg}
          </div>
        </div>
        <div style={{ display: "flex", gap: 8, alignItems: "center" }}>
          <span
            data-testid="sandbox-origin-badge"
            style={{
              fontSize: 11.5,
              fontFamily: "ui-monospace, SFMono-Regular, Menlo, monospace",
              padding: "3px 9px",
              borderRadius: 999,
              background: "#edf3fe",
              color: "#3459b2",
            }}
          >
            origin={window.origin}
          </span>
          <span
            data-testid="sandbox-mask-badge"
            style={{
              fontSize: 11.5,
              fontWeight: 500,
              padding: "3px 9px",
              borderRadius: 999,
              background: context?.sequenceMasked ? "rgba(221, 133, 0, 0.12)" : "rgba(0, 148, 86, 0.1)",
              color: context?.sequenceMasked ? "#b45309" : "#007a47",
            }}
          >
            {context?.sequenceMasked ? "Sequences Masked ('***')" : "Full IUPAC Access"}
          </span>
        </div>
      </div>

      <div
        style={{
          display: "flex",
          gap: 10,
          alignItems: "center",
          flexWrap: "wrap",
          background: "#f9fafb",
          padding: "10px 14px",
          borderRadius: 12,
          border: "1px solid rgba(15, 17, 21, 0.08)",
          marginBottom: 16,
        }}
      >
        <label
          style={{
            fontSize: 13,
            color: "#545557",
            display: "flex",
            alignItems: "center",
            gap: 8,
          }}
        >
          Max IC50 Cutoff (nM):
          <input
            type="number"
            min={10}
            max={1000}
            value={maxIc50}
            onChange={(e) => setMaxIc50(Number(e.target.value) || 250)}
            style={{
              width: 82,
              background: "#ffffff",
              color: "#0f1115",
              border: "1px solid rgba(15, 17, 21, 0.14)",
              borderRadius: 8,
              padding: "5px 8px",
              fontSize: 13,
            }}
          />
        </label>

        <button
          type="button"
          data-testid="sdk-query-btn"
          onClick={() => void handlePreviewCohort()}
          style={{
            background: "#ffffff",
            color: "#0f1115",
            border: "1px solid rgba(15, 17, 21, 0.14)",
            borderRadius: 999,
            padding: "6px 14px",
            fontSize: 12.5,
            fontWeight: 500,
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
            background: "#4176e6",
            color: "#ffffff",
            border: "none",
            borderRadius: 999,
            padding: "6px 14px",
            fontSize: 12.5,
            fontWeight: 500,
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
            background: "#edf3fe",
            border: "1px solid rgba(65, 118, 230, 0.25)",
            borderRadius: 12,
            padding: "10px 14px",
            marginBottom: 16,
            fontSize: 12.5,
            color: "#1e3a8a",
          }}
        >
          <strong>Signed Froglet Quote Ready:</strong>{" "}
          <code>{preparedQuote.run_id}</code> · workload_hash{" "}
          <code>{preparedQuote.workload_hash.slice(0, 16)}...</code> · Approve in host panel to run OCI container.
        </div>
      )}

      {previewRows.length > 0 && (
        <div style={{ marginBottom: 18 }}>
          <div style={{ fontSize: 12, fontWeight: 500, color: "#60646c", marginBottom: 6 }}>
            Scoped DuckDB Preview via @biocompass/plugin-sdk
          </div>
          <table
            data-testid="sdk-preview-table"
            style={{ width: "100%", borderCollapse: "collapse", fontSize: 12.5 }}
          >
            <thead>
              <tr
                style={{
                  textAlign: "left",
                  borderBottom: "1px solid rgba(15, 17, 21, 0.1)",
                  color: "#60646c",
                }}
              >
                <th style={{ padding: "6px 8px", fontWeight: 500 }}>Peptide ID</th>
                <th style={{ padding: "6px 8px", fontWeight: 500 }}>Sequence</th>
                <th style={{ padding: "6px 8px", fontWeight: 500 }}>Allele</th>
                <th style={{ padding: "6px 8px", fontWeight: 500 }}>Gene</th>
                <th style={{ padding: "6px 8px", fontWeight: 500 }}>Organ</th>
                <th style={{ padding: "6px 8px", fontWeight: 500 }}>IC50 (nM)</th>
              </tr>
            </thead>
            <tbody>
              {previewRows.map((row) => (
                <tr
                  key={row.peptide_id}
                  style={{ borderBottom: "1px solid rgba(15, 17, 21, 0.06)" }}
                >
                  <td style={{ padding: "6px 8px", fontFamily: "monospace", color: "#545557" }}>
                    {row.peptide_id}
                  </td>
                  <td
                    style={{
                      padding: "6px 8px",
                      fontFamily: "monospace",
                      fontWeight: 600,
                      color: "#e25828",
                    }}
                  >
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
          <div style={{ fontSize: 12, fontWeight: 500, color: "#007a47", marginBottom: 6 }}>
            Container Output (`/out/result.json`) — Top Antigenic Source Genes
          </div>
          <table
            data-testid="sdk-hotspots-table"
            style={{ width: "100%", borderCollapse: "collapse", fontSize: 12.5 }}
          >
            <thead>
              <tr
                style={{
                  textAlign: "left",
                  borderBottom: "1px solid rgba(15, 17, 21, 0.1)",
                  color: "#60646c",
                }}
              >
                <th style={{ padding: "6px 8px", fontWeight: 500 }}>Gene</th>
                <th style={{ padding: "6px 8px", fontWeight: 500 }}>Total Ligands</th>
                <th style={{ padding: "6px 8px", fontWeight: 500 }}>Strong Binders (&le;50 nM)</th>
                <th style={{ padding: "6px 8px", fontWeight: 500 }}>Best IC50 (nM)</th>
                <th style={{ padding: "6px 8px", fontWeight: 500 }}>Mean MS log2</th>
              </tr>
            </thead>
            <tbody>
              {hotspots.map((g) => (
                <tr
                  key={g.gene_symbol}
                  style={{ borderBottom: "1px solid rgba(15, 17, 21, 0.06)" }}
                >
                  <td style={{ padding: "6px 8px", fontWeight: 600 }}>{g.gene_symbol}</td>
                  <td style={{ padding: "6px 8px" }}>{g.ligands}</td>
                  <td style={{ padding: "6px 8px", color: "#007a47", fontWeight: 600 }}>
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
