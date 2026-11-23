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

const DEFAULT_ALLELES = ["HLA-A*02:01", "HLA-A*03:01", "HLA-B*07:02", "HLA-B*44:02"];

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
    const applyHostParams = (raw: Record<string, unknown> | null | undefined) => {
      if (!raw) return;
      if (typeof raw["max_ic50_nm"] === "number" && Number.isFinite(raw["max_ic50_nm"])) {
        setMaxIc50(raw["max_ic50_nm"]);
      }
    };

    void client
      .getContext()
      .then((ctx) => {
        if (!active) return;
        setContext(ctx);
        applyHostParams(ctx.pendingParams ?? ctx.draftParams);
        setStatusMsg(`Connected to host · Grant: ${ctx.activeGrantLabel}`);
      })
      .catch((err: unknown) => {
        if (!active) return;
        setStatusMsg(`Host bridge error: ${err instanceof Error ? err.message : String(err)}`);
      });

    const unsub = client.onContextUpdate((updated) => {
      setContext(updated);
      if (updated.accessLevel === "compute_only") {
        setPreviewRows([]);
      }
      applyHostParams(updated.pendingParams ?? updated.draftParams);
      setStatusMsg(`Updated from host · Grant: ${updated.activeGrantLabel}`);
    });

    return () => {
      active = false;
      unsub();
      client.dispose();
    };
  }, [client]);

  const activeAlleles = useMemo(() => {
    const facetAlleles =
      context?.catalogFacets?.["hla_alleles"] ?? context?.catalogFacets?.["hla_allele"];
    return Array.isArray(facetAlleles) && facetAlleles.length > 0
      ? facetAlleles.slice(0, 4)
      : DEFAULT_ALLELES;
  }, [context]);

  useEffect(() => {
    const timer = window.setTimeout(() => {
      client
        .syncDraftParams({
          alleles: activeAlleles,
          max_ic50_nm: maxIc50,
          min_donors: 1,
        })
        .catch(() => {});
    }, 150);
    return () => window.clearTimeout(timer);
  }, [client, activeAlleles, maxIc50]);

  const handlePreviewCohort = async (): Promise<void> => {
    try {
      setStatusMsg("Querying DuckLake catalog via host JSON-RPC 2.0...");
      const res = await client.queryCatalog<SdkPeptideRecord>({
        filter: { op: "lte", column: "affinity_nm", value: maxIc50 },
        limit: 8,
      });
      setPreviewRows(res.rows);
      setStatusMsg(
        `Loaded ${res.rows.length} of ${res.total_in_scope} in-scope ligands (level=${res.access_level}, snapshot=#${res.snapshot_id})`,
      );
    } catch (err: unknown) {
      setPreviewRows([]);
      setStatusMsg(`Catalog RPC error: ${err instanceof Error ? err.message : String(err)}`);
    }
  };

  const handlePrepareRun = async (): Promise<void> => {
    try {
      setStatusMsg("Requesting signed Froglet Quote from host...");
      const quote = await client.prepareRun({
        alleles: activeAlleles,
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
  const isComputeOnly = context?.accessLevel === "compute_only";
  const isDark = context?.theme === "dark";
  const t = isDark
    ? {
        bg: "#121316",
        surface: "#1c1f26",
        inputBg: "#16181d",
        text: "#f3f4f6",
        muted: "#9ca3af",
        border: "rgba(255, 255, 255, 0.1)",
        borderStrong: "rgba(255, 255, 255, 0.18)",
        rowBorder: "rgba(255, 255, 255, 0.06)",
        blueBg: "rgba(96, 165, 250, 0.16)",
        blueText: "#93c5fd",
        greenBg: "rgba(52, 184, 122, 0.16)",
        greenText: "#34b87a",
        amberBg: "rgba(234, 179, 84, 0.16)",
        amberText: "#facc15",
        orangeText: "#fb923c",
        quoteText: "#bfdbfe",
      }
    : {
        bg: "#ffffff",
        surface: "#f9fafb",
        inputBg: "#ffffff",
        text: "#0f1115",
        muted: "#60646c",
        border: "rgba(15, 17, 21, 0.08)",
        borderStrong: "rgba(15, 17, 21, 0.14)",
        rowBorder: "rgba(15, 17, 21, 0.06)",
        blueBg: "#edf3fe",
        blueText: "#3459b2",
        greenBg: "rgba(0, 148, 86, 0.1)",
        greenText: "#007a47",
        amberBg: "rgba(221, 133, 0, 0.12)",
        amberText: "#b45309",
        orangeText: "#e25828",
        quoteText: "#1e3a8a",
      };

  return (
    <div
      style={{
        fontFamily: "Inter, system-ui, -apple-system, BlinkMacSystemFont, sans-serif",
        background: t.bg,
        color: t.text,
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
          borderBottom: `1px solid ${t.border}`,
          paddingBottom: 12,
          marginBottom: 14,
          gap: 12,
          flexWrap: "wrap",
        }}
      >
        <div>
          <div style={{ fontSize: 15, fontWeight: 500, color: t.text }}>
            HLA Antigenic Gene Hotspot Explorer
          </div>
          <div
            data-testid="sandbox-status"
            style={{ fontSize: 12.5, color: t.muted, marginTop: 3 }}
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
              background: t.blueBg,
              color: t.blueText,
            }}
          >
            origin={window.origin}
          </span>
          <span
            data-testid="sandbox-access-badge"
            style={{
              fontSize: 11.5,
              fontWeight: 500,
              padding: "3px 9px",
              borderRadius: 999,
              background: isComputeOnly ? t.amberBg : t.greenBg,
              color: isComputeOnly ? t.amberText : t.greenText,
            }}
          >
            {isComputeOnly ? "Compute-Only Grant (Rows Hidden)" : "Row Access (Unmasked IUPAC)"}
          </span>
        </div>
      </div>

      <div
        style={{
          display: "flex",
          gap: 10,
          alignItems: "center",
          flexWrap: "wrap",
          background: t.surface,
          padding: "10px 14px",
          borderRadius: 12,
          border: `1px solid ${t.border}`,
          marginBottom: 16,
        }}
      >
        <label
          style={{
            fontSize: 13,
            color: t.muted,
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
              background: t.inputBg,
              color: t.text,
              border: `1px solid ${t.borderStrong}`,
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
            background: t.inputBg,
            color: t.text,
            border: `1px solid ${t.borderStrong}`,
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
          Run Container Scan
        </button>
      </div>

      {preparedQuote && (
        <div
          data-testid="sdk-quote-banner"
          style={{
            background: t.blueBg,
            border: "1px solid rgba(65, 118, 230, 0.3)",
            borderRadius: 12,
            padding: "10px 14px",
            marginBottom: 16,
            fontSize: 12.5,
            color: t.quoteText,
          }}
        >
          <strong>Container Run Dispatched:</strong>{" "}
          <code>{preparedQuote.run_id}</code> · workload_hash{" "}
          <code>{preparedQuote.workload_hash.slice(0, 16)}...</code> · Running OCI container in host panel.
        </div>
      )}

      {previewRows.length > 0 && (
        <div style={{ marginBottom: 18 }}>
          <div style={{ fontSize: 12, fontWeight: 500, color: t.muted, marginBottom: 6 }}>
            Scoped DuckLake Preview via @biocompass/plugin-sdk
          </div>
          <table
            data-testid="sdk-preview-table"
            style={{ width: "100%", borderCollapse: "collapse", fontSize: 12.5 }}
          >
            <thead>
              <tr
                style={{
                  textAlign: "left",
                  borderBottom: `1px solid ${t.borderStrong}`,
                  color: t.muted,
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
                  style={{ borderBottom: `1px solid ${t.rowBorder}` }}
                >
                  <td style={{ padding: "6px 8px", fontFamily: "monospace", color: t.muted }}>
                    {row.peptide_id}
                  </td>
                  <td
                    style={{
                      padding: "6px 8px",
                      fontFamily: "monospace",
                      fontWeight: 600,
                      color: t.orangeText,
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
          <div style={{ fontSize: 12, fontWeight: 500, color: t.greenText, marginBottom: 6 }}>
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
                  borderBottom: `1px solid ${t.borderStrong}`,
                  color: t.muted,
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
                  style={{ borderBottom: `1px solid ${t.rowBorder}` }}
                >
                  <td style={{ padding: "6px 8px", fontWeight: 600 }}>{g.gene_symbol}</td>
                  <td style={{ padding: "6px 8px" }}>{g.ligands}</td>
                  <td style={{ padding: "6px 8px", color: t.greenText, fontWeight: 600 }}>
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
