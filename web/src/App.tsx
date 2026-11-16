import React, { useCallback, useEffect, useMemo, useState } from "react";
import {
  confirmRun,
  createSession,
  fetchCatalogs,
  fetchGrants,
  fetchHealth,
  fetchLedger,
  fetchPlugins,
  fetchRuns,
  fetchSessionEvents,
  fetchSessions,
  prepareRun,
  queryCatalog,
  scaffoldPlugin,
  streamAgentChatTurn,
  subscribeRunLogs,
  updateSessionGrant,
  verifyFrogletChain,
} from "./api";
import { BrandWordmark } from "./components/BrandWordmark";
import { SandboxedPluginFrame } from "./components/SandboxedPluginFrame";
import { ScientificViews } from "./components/ScientificViews";
import {
  formatMilliAct,
  type CatalogManifest,
  type CatalogQueryResult,
  type DatasetGrant,
  type FrogletVerifyReport,
  type HealthStatus,
  type LedgerState,
  type PluginSummary,
  type RunQuote,
  type RunRecord,
  type ScaffoldResult,
  type SessionEvent,
  type SessionRecord,
} from "./types";

type ActiveTab = "workbench" | "catalogs" | "plugins" | "studio";

function defaultParamsForPlugin(plugin: PluginSummary): Record<string, unknown> {
  const props = (
    plugin.manifest.inputs.schema["properties"] as
      | Record<string, { default?: unknown }>
      | undefined
  ) ?? {};
  const out: Record<string, unknown> = {};
  for (const [key, spec] of Object.entries(props)) {
    if (spec && "default" in spec && spec.default !== undefined) {
      out[key] = spec.default;
    }
  }
  return out;
}

export function App() {
  const [activeTab, setActiveTab] = useState<ActiveTab>("workbench");
  const [health, setHealth] = useState<HealthStatus | null>(null);
  const [grants, setGrants] = useState<DatasetGrant[]>([]);
  const [activeGrantId, setActiveGrantId] = useState<string>("");
  const [catalogs, setCatalogs] = useState<CatalogManifest[]>([]);
  const [catalogResult, setCatalogResult] = useState<CatalogQueryResult | null>(null);
  const [catalogMaxIc50, setCatalogMaxIc50] = useState<number>(500);
  const [plugins, setPlugins] = useState<PluginSummary[]>([]);
  const [selectedPluginId, setSelectedPluginId] = useState<string>("");
  const [ledger, setLedger] = useState<LedgerState | null>(null);

  const [runs, setRuns] = useState<RunRecord[]>([]);
  const [selectedRunId, setSelectedRunId] = useState<string>("");
  const [pendingQuote, setPendingQuote] = useState<RunQuote | null>(null);
  const [isConfirmingRun, setIsConfirmingRun] = useState<boolean>(false);
  const [liveLogs, setLiveLogs] = useState<string[]>([]);
  const [verifyReport, setVerifyReport] = useState<FrogletVerifyReport | null>(null);
  const [errorBanner, setErrorBanner] = useState<string | null>(null);

  // Agent session state
  const [session, setSession] = useState<SessionRecord | null>(null);
  const [sessionEvents, setSessionEvents] = useState<SessionEvent[]>([]);
  const [chatInput, setChatInput] = useState<string>("");
  const [streamingText, setStreamingText] = useState<string>("");
  const [chatBusy, setChatBusy] = useState<boolean>(false);

  // Plugin Builder Studio state
  const [studioTemplate, setStudioTemplate] = useState<string>("python-uv");
  const [studioPluginId, setStudioPluginId] = useState<string>("alithea/custom-neoantigen-scorer");
  const [studioName, setStudioName] = useState<string>("Custom Neoantigen Scorer");
  const [studioIncludeUi, setStudioIncludeUi] = useState<boolean>(true);
  const [scaffoldOutput, setScaffoldOutput] = useState<ScaffoldResult | null>(null);

  const activeGrant = useMemo<DatasetGrant | null>(() => {
    if (grants.length === 0) return null;
    return grants.find((g) => g.grant_id === activeGrantId) ?? grants[0] ?? null;
  }, [grants, activeGrantId]);

  const defaultCatalogId = catalogs[0]?.id ?? "alithea/hla-atlas-v2026.1";

  const selectedRun = useMemo<RunRecord | null>(() => {
    if (runs.length === 0) return null;
    return runs.find((r) => r.run_id === selectedRunId) ?? runs[0] ?? null;
  }, [runs, selectedRunId]);

  const selectedPlugin = useMemo<PluginSummary | null>(() => {
    if (plugins.length === 0) return null;
    return plugins.find((p) => p.manifest.id === selectedPluginId) ?? plugins[0] ?? null;
  }, [plugins, selectedPluginId]);

  const latestRunForSelectedPlugin = useMemo<RunRecord | null>(() => {
    if (!selectedPlugin) return null;
    return (
      runs.find(
        (r) => r.plugin_id === selectedPlugin.manifest.id && r.status === "succeeded",
      ) ?? null
    );
  }, [runs, selectedPlugin]);

  // Initial bootstrap
  useEffect(() => {
    void (async () => {
      try {
        const [h, gList, cList, pList, lState, rList, sList] = await Promise.all([
          fetchHealth(),
          fetchGrants(),
          fetchCatalogs(),
          fetchPlugins(),
          fetchLedger(),
          fetchRuns(),
          fetchSessions(),
        ]);
        setHealth(h);
        setGrants(gList);
        const initialGrant = gList[0]?.grant_id ?? "";
        setActiveGrantId(initialGrant);
        setCatalogs(cList);
        setPlugins(pList);
        const explorer = pList.find((p) => p.has_custom_ui) ?? pList[0];
        if (explorer) {
          setSelectedPluginId(explorer.manifest.id);
        }
        setLedger(lState);
        setRuns(rList);
        if (rList[0]) {
          setSelectedRunId(rList[0].run_id);
        }

        let activeSession = sList[0];
        if (!activeSession && initialGrant) {
          activeSession = await createSession("Immunopeptidomics Workbench", initialGrant);
        }
        if (activeSession) {
          setSession(activeSession);
          const evs = await fetchSessionEvents(activeSession.session_id);
          setSessionEvents(evs);
        }
      } catch (err: unknown) {
        setErrorBanner(err instanceof Error ? err.message : String(err));
      }
    })();
  }, []);

  // Re-query catalog automatically whenever activeGrant or catalogMaxIc50 changes
  useEffect(() => {
    if (!activeGrant || catalogs.length === 0) return;
    const catId = catalogs[0]?.id;
    if (!catId) return;
    void queryCatalog(catId, activeGrant.grant_id, {
      max_affinity_nm: catalogMaxIc50,
      limit: 25,
    })
      .then((res) => {
        setCatalogResult(res);
      })
      .catch((err: unknown) => {
        setErrorBanner(err instanceof Error ? err.message : String(err));
      });
  }, [activeGrant, catalogs, catalogMaxIc50]);

  const handleGrantChange = async (newGrantId: string): Promise<void> => {
    setActiveGrantId(newGrantId);
    if (session) {
      try {
        const updated = await updateSessionGrant(session.session_id, newGrantId);
        setSession(updated);
      } catch {
        // non-fatal
      }
    }
  };

  const handleQuotePrepared = useCallback((quote: RunQuote) => {
    setPendingQuote(quote);
    setVerifyReport(null);
    setErrorBanner(null);
  }, []);

  const handleQuickPrepare = async (plugin: PluginSummary): Promise<void> => {
    try {
      setErrorBanner(null);
      const quote = await prepareRun({
        plugin_id: plugin.manifest.id,
        params: defaultParamsForPlugin(plugin),
        grant_id: activeGrant?.grant_id,
        session_id: session?.session_id,
      });
      setPendingQuote(quote);
    } catch (err: unknown) {
      setErrorBanner(err instanceof Error ? err.message : String(err));
    }
  };

  const handleConfirmPendingRun = async (): Promise<void> => {
    if (!pendingQuote || isConfirmingRun) return;
    const quoteToRun = pendingQuote;
    setIsConfirmingRun(true);
    setErrorBanner(null);
    setVerifyReport(null);
    setLiveLogs([`[host] Confirming Froglet Quote ${quoteToRun.run_id} and dispatching container...`]);

    const unsubscribe = subscribeRunLogs(quoteToRun.run_id, (ev) => {
      setLiveLogs((prev) => [...prev, `[${ev.stream}] ${ev.line}`]);
    });

    try {
      const completed = await confirmRun(quoteToRun.run_id);
      setPendingQuote(null);
      const [updatedRuns, updatedLedger] = await Promise.all([
        fetchRuns(),
        fetchLedger(),
      ]);
      setRuns(updatedRuns);
      setSelectedRunId(completed.run_id);
      setLedger(updatedLedger);
      if (completed.logs.length > 0) {
        setLiveLogs(completed.logs);
      }
      if (completed.froglet_chain.length > 0) {
        const report = await verifyFrogletChain(completed.froglet_chain);
        setVerifyReport(report);
      }
    } catch (err: unknown) {
      setErrorBanner(err instanceof Error ? err.message : String(err));
    } finally {
      unsubscribe();
      setIsConfirmingRun(false);
    }
  };

  const handleReverifyReceipt = async (tamperOneByte: boolean): Promise<void> => {
    if (!selectedRun || selectedRun.froglet_chain.length === 0) return;
    const chainCopy = JSON.parse(
      JSON.stringify(selectedRun.froglet_chain),
    ) as Record<string, unknown>[];

    if (tamperOneByte) {
      const receiptDoc = chainCopy[chainCopy.length - 1];
      const payload =
        receiptDoc && typeof receiptDoc["payload"] === "object" && receiptDoc["payload"] !== null
          ? (receiptDoc["payload"] as Record<string, unknown>)
          : null;
      if (payload && typeof payload["result_hash"] === "string") {
        const orig = payload["result_hash"];
        payload["result_hash"] = (orig.startsWith("0") ? "f" : "0") + orig.slice(1);
      } else if (receiptDoc && typeof receiptDoc["signature"] === "string") {
        const origSig = receiptDoc["signature"];
        receiptDoc["signature"] = (origSig.startsWith("0") ? "f" : "0") + origSig.slice(1);
      }
    }
    const report = await verifyFrogletChain(chainCopy);
    setVerifyReport(report);
  };

  const handleChatSubmit = async (e: React.FormEvent): Promise<void> => {
    e.preventDefault();
    if (!session || !chatInput.trim() || chatBusy) return;
    const msg = chatInput.trim();
    setChatInput("");
    setChatBusy(true);
    setStreamingText("");
    setErrorBanner(null);

    try {
      await streamAgentChatTurn(session.session_id, msg, (ev) => {
        if (ev.type === "text_delta" && typeof ev.data === "string") {
          setStreamingText((prev) => prev + ev.data);
        } else if (ev.type === "run_prepared" && ev.data) {
          setPendingQuote(ev.data as RunQuote);
        } else if (ev.type === "error" && typeof ev.data === "string") {
          setErrorBanner(ev.data);
        }
      });
      const evs = await fetchSessionEvents(session.session_id);
      setSessionEvents(evs);
      setStreamingText("");
    } catch (err: unknown) {
      setErrorBanner(err instanceof Error ? err.message : String(err));
    } finally {
      setChatBusy(false);
    }
  };

  const handleScaffoldSubmit = async (e: React.FormEvent): Promise<void> => {
    e.preventDefault();
    try {
      setErrorBanner(null);
      const res = await scaffoldPlugin({
        template: studioTemplate,
        plugin_id: studioPluginId,
        display_name: studioName,
        include_vite_ui: studioIncludeUi,
      });
      setScaffoldOutput(res);
      const updatedPlugins = await fetchPlugins();
      setPlugins(updatedPlugins);
    } catch (err: unknown) {
      setErrorBanner(err instanceof Error ? err.message : String(err));
    }
  };

  const selectedRunPlugin = useMemo(
    () => plugins.find((p) => p.manifest.id === selectedRun?.plugin_id) ?? null,
    [plugins, selectedRun],
  );

  return (
    <div className="app-shell">
      <header className="top-header">
        <div className="brand-cluster">
          <BrandWordmark size={26} />
          {health && (
            <span className="badge badge-emerald" data-testid="health-pill">
              {health.container_engine} · {health.node_id}
            </span>
          )}
        </div>

        <div className="header-controls">
          <div className="grant-selector-wrap">
            <span className="muted" style={{ fontSize: 11, fontWeight: 600 }}>
              DATA GRANT:
            </span>
            <select
              data-testid="grant-select"
              className="grant-select"
              value={activeGrant?.grant_id ?? ""}
              onChange={(e) => void handleGrantChange(e.target.value)}
            >
              {grants.map((g) => (
                <option key={g.grant_id} value={g.grant_id}>
                  {g.label}
                </option>
              ))}
            </select>
          </div>

          <div className="act-pill" data-testid="act-balance">
            {ledger ? ledger.available_display : "— ACT"}
          </div>
        </div>
      </header>

      <nav className="nav-bar">
        <button
          type="button"
          data-testid="nav-workbench"
          className={`nav-tab ${activeTab === "workbench" ? "active" : ""}`}
          onClick={() => setActiveTab("workbench")}
        >
          Workbench &amp; Receipts
        </button>
        <button
          type="button"
          data-testid="nav-catalogs"
          className={`nav-tab ${activeTab === "catalogs" ? "active" : ""}`}
          onClick={() => setActiveTab("catalogs")}
        >
          Data Catalogs (DuckDB)
        </button>
        <button
          type="button"
          data-testid="nav-plugins"
          className={`nav-tab ${activeTab === "plugins" ? "active" : ""}`}
          onClick={() => setActiveTab("plugins")}
        >
          Bio Plugins &amp; Sandbox UI
        </button>
        <button
          type="button"
          data-testid="nav-studio"
          className={`nav-tab ${activeTab === "studio" ? "active" : ""}`}
          onClick={() => setActiveTab("studio")}
        >
          Plugin Studio
        </button>
      </nav>

      {errorBanner && (
        <div
          data-testid="error-banner"
          style={{
            margin: "12px 24px 0",
            padding: "10px 14px",
            borderRadius: 8,
            background: "#35151e",
            border: "1px solid #fb7185",
            color: "#fecdd3",
          }}
        >
          {errorBanner}
        </div>
      )}

      {pendingQuote && (
        <section className="consent-banner" data-testid="pending-quote-banner">
          <div className="consent-meta">
            <div style={{ fontWeight: 700, fontSize: 13 }}>
              Two-Step Froglet Run Consent Required ·{" "}
              <span className="mono">{pendingQuote.plugin_id}</span> (
              {formatMilliAct(pendingQuote.estimated_cost_act)})
            </div>
            <div className="muted" style={{ fontSize: 12 }}>
              Run <span className="mono">{pendingQuote.run_id}</span> · Image{" "}
              <span className="mono">{pendingQuote.image_ref.slice(0, 62)}...</span> · Grant{" "}
              <strong>{pendingQuote.dataset_grant_summary ?? "None"}</strong>
            </div>
          </div>
          <div className="consent-actions">
            <button
              type="button"
              data-testid="cancel-quote-btn"
              className="btn"
              disabled={isConfirmingRun}
              onClick={() => setPendingQuote(null)}
            >
              Dismiss
            </button>
            <button
              type="button"
              data-testid="confirm-run-btn"
              className="btn btn-primary"
              disabled={isConfirmingRun}
              onClick={() => void handleConfirmPendingRun()}
            >
              {isConfirmingRun
                ? "Running Hardened OCI Container..."
                : `Approve & Run (${formatMilliAct(pendingQuote.estimated_cost_act)})`}
            </button>
          </div>
        </section>
      )}

      <main className="main-content">
        {activeTab === "workbench" && (
          <div className="workbench-grid">
            <div className="panel">
              <div className="panel-header">
                <h2 className="panel-title">OCI Plugin Launchers</h2>
                <span className="badge badge-cyan">{plugins.length} Registered</span>
              </div>

              <div className="quick-launcher-list">
                {plugins.map((p) => (
                  <div key={p.manifest.id} className="launcher-card">
                    <div style={{ display: "flex", justifyContent: "space-between", gap: 8 }}>
                      <strong>{p.manifest.name}</strong>
                      <span className="mono muted">
                        {formatMilliAct(p.manifest.price_milli_act)}
                      </span>
                    </div>
                    <div className="muted" style={{ fontSize: 11.5 }}>
                      {p.manifest.description}
                    </div>
                    <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center" }}>
                      <span className="mono muted" style={{ fontSize: 11 }}>
                        --network={p.manifest.network}
                      </span>
                      <button
                        type="button"
                        data-testid={`quick-prepare-btn-${p.manifest.id}`}
                        className="btn btn-cyan"
                        onClick={() => void handleQuickPrepare(p)}
                      >
                        Prepare Quote
                      </button>
                    </div>
                  </div>
                ))}
              </div>

              <div className="panel-header" style={{ marginTop: 18 }}>
                <h2 className="panel-title">BioCompass Agent (SSE)</h2>
                <span className="badge badge-slate">
                  {health?.llm_configured ? health.llm_model ?? "LLM Ready" : "No API Key"}
                </span>
              </div>

              <div className="chat-feed" data-testid="chat-feed">
                {sessionEvents.length === 0 && !streamingText && (
                  <div className="muted" style={{ fontSize: 12 }}>
                    Agent uses SKILL.md context and can prepare quotes, query DuckDB catalogs, and scaffold plugins, but physically cannot bypass user RunQuote confirmation.
                  </div>
                )}
                {sessionEvents.map((ev) => (
                  <div key={ev.event_id} className="chat-bubble">
                    <strong className="mono" style={{ fontSize: 11, color: "#38bdf8" }}>
                      [{ev.kind}]
                    </strong>{" "}
                    {ev.content}
                  </div>
                ))}
                {streamingText && <div className="chat-bubble">{streamingText}</div>}
              </div>

              <form className="chat-form" onSubmit={(e) => void handleChatSubmit(e)}>
                <input
                  type="text"
                  className="text-input"
                  placeholder="Ask BioCompass Agent to query HLA cohorts or prepare a run..."
                  value={chatInput}
                  onChange={(e) => setChatInput(e.target.value)}
                />
                <button type="submit" className="btn btn-primary" disabled={chatBusy}>
                  Send
                </button>
              </form>
            </div>

            <div style={{ display: "flex", flexDirection: "column", gap: 16 }}>
              <div className="panel">
                <div className="panel-header">
                  <div>
                    <h2 className="panel-title">
                      Hardened OCI Execution &amp; Scientific Outputs
                    </h2>
                    {selectedRun && (
                      <div className="muted mono" style={{ fontSize: 11.5, marginTop: 2 }}>
                        {selectedRun.run_id} · {selectedRun.plugin_id} · status={selectedRun.status}
                      </div>
                    )}
                  </div>

                  {runs.length > 0 && (
                    <select
                      className="text-input"
                      style={{ maxWidth: 260 }}
                      value={selectedRun?.run_id ?? ""}
                      onChange={(e) => {
                        setSelectedRunId(e.target.value);
                        setVerifyReport(null);
                      }}
                    >
                      {runs.map((r) => (
                        <option key={r.run_id} value={r.run_id}>
                          {r.run_id.slice(0, 16)} · {r.plugin_id} ({r.status})
                        </option>
                      ))}
                    </select>
                  )}
                </div>

                <div className="terminal-box" data-testid="live-run-logs">
                  {(liveLogs.length > 0
                    ? liveLogs
                    : selectedRun?.logs ?? [
                        "Ready. Click 'Prepare Quote' -> 'Approve & Run' to stream real container stdout/stderr over SSE.",
                      ]
                  ).join("\n")}
                </div>

                {selectedRun?.result_json && (
                  <ScientificViews
                    resultJson={selectedRun.result_json}
                    views={selectedRunPlugin?.views ?? []}
                  />
                )}

                {selectedRun && selectedRun.artifacts.length > 0 && (
                  <div style={{ marginTop: 14 }} data-testid="run-artifacts">
                    <div className="sci-card-title">Stage-Out BlobStore Artifacts (`/out/artifacts/*`)</div>
                    <div style={{ display: "flex", gap: 10, flexWrap: "wrap" }}>
                      {selectedRun.artifacts.map((art) => (
                        <a
                          key={art.name}
                          href={`/api/v1/runs/${encodeURIComponent(selectedRun.run_id)}/artifacts/${art.name}`}
                          target="_blank"
                          rel="noreferrer"
                          className="btn btn-cyan mono"
                          style={{ textDecoration: "none" }}
                        >
                          {art.name} ({art.size_bytes} B · sha256:{art.sha256.slice(0, 10)})
                        </a>
                      ))}
                    </div>
                  </div>
                )}
              </div>

              {selectedRun && selectedRun.froglet_chain.length > 0 && (
                <div className="panel" data-testid="receipt-inspector">
                  <div className="panel-header">
                    <div>
                      <h2 className="panel-title">
                        Froglet v1 RFC 8785 JCS + BIP340 Schnorr 5-Artifact Chain
                      </h2>
                      <div className="muted" style={{ fontSize: 12 }}>
                        Descriptor &rarr; Offer &rarr; Quote &rarr; Deal &rarr; Receipt (
                        <code>settlement_method = &quot;none&quot;</code>)
                      </div>
                    </div>
                    <div style={{ display: "flex", gap: 8, alignItems: "center" }}>
                      <span
                        data-testid="receipt-verified-badge"
                        className={`badge ${
                          (verifyReport ? verifyReport.valid : selectedRun.receipt_verified)
                            ? "badge-emerald"
                            : "badge-rose"
                        }`}
                      >
                        {(verifyReport ? verifyReport.valid : selectedRun.receipt_verified)
                          ? "SCHNORR CHAIN VALID"
                          : "TAMPER DETECTED / INVALID"}
                      </span>
                      <button
                        type="button"
                        data-testid="reverify-chain-btn"
                        className="btn"
                        onClick={() => void handleReverifyReceipt(false)}
                      >
                        Re-Verify Chain
                      </button>
                      <button
                        type="button"
                        data-testid="tamper-test-btn"
                        className="btn"
                        onClick={() => void handleReverifyReceipt(true)}
                      >
                        Simulate 1-Byte Tamper
                      </button>
                    </div>
                  </div>

                  <pre className="sql-pre" style={{ maxHeight: 220 }}>
                    {JSON.stringify(selectedRun.froglet_receipt, null, 2)}
                  </pre>
                </div>
              )}
            </div>
          </div>
        )}

        {activeTab === "catalogs" && (
          <div className="panel">
            <div className="panel-header">
              <div>
                <h2 className="panel-title">
                  Immutable Parquet Catalog Explorer · <span className="mono">{defaultCatalogId}</span>
                </h2>
                <div className="muted" style={{ fontSize: 12 }}>
                  SHA-256 verified shards queried through DuckDB Grant-compiled TEMP VIEWs.
                </div>
              </div>
              <div style={{ display: "flex", gap: 10, alignItems: "center" }}>
                <label className="muted" style={{ fontSize: 12 }}>
                  Max IC50 (nM):{" "}
                  <input
                    type="number"
                    className="text-input"
                    style={{ width: 90 }}
                    value={catalogMaxIc50}
                    onChange={(e) => setCatalogMaxIc50(Number(e.target.value) || 500)}
                  />
                </label>
                {catalogResult && (
                  <span
                    data-testid="catalog-mask-indicator"
                    className={`badge ${
                      catalogResult.sequence_masked ? "badge-rose" : "badge-emerald"
                    }`}
                  >
                    {catalogResult.sequence_masked
                      ? "Sequences Masked ('***')"
                      : "Unmasked Amino-Acid Sequences"}
                  </span>
                )}
              </div>
            </div>

            {catalogResult && (
              <>
                <div className="sci-card-title">Compiled DuckDB Grant SQL View</div>
                <pre className="sql-pre" data-testid="catalog-sql-view">
                  {catalogResult.compiled_view_sql}
                </pre>

                <div className="table-scroll" style={{ marginTop: 12 }}>
                  <table className="data-table" data-testid="catalog-table">
                    <thead>
                      <tr>
                        <th>Peptide ID</th>
                        <th>Sequence</th>
                        <th>Len</th>
                        <th>HLA Allele</th>
                        <th>Gene</th>
                        <th>Organ</th>
                        <th>Condition</th>
                        <th>IC50 (nM)</th>
                        <th>Donors</th>
                      </tr>
                    </thead>
                    <tbody>
                      {catalogResult.rows.map((row) => (
                        <tr key={row.peptide_id}>
                          <td className="mono">{row.peptide_id}</td>
                          <td className="mono accent-seq">{row.sequence}</td>
                          <td>{row.peptide_length}</td>
                          <td>{row.hla_allele}</td>
                          <td>
                            <strong>{row.gene_symbol}</strong>
                          </td>
                          <td>{row.organ}</td>
                          <td>{row.condition}</td>
                          <td>{row.affinity_nm.toFixed(1)}</td>
                          <td>{row.donor_count}</td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              </>
            )}
          </div>
        )}

        {activeTab === "plugins" && (
          <div className="plugins-layout">
            <div className="panel">
              <div className="panel-header">
                <h2 className="panel-title">Installed OCI Plugins</h2>
              </div>
              {plugins.map((p) => (
                <div
                  key={p.manifest.id}
                  data-testid={`plugin-card-${p.manifest.id}`}
                  className={`plugin-list-card ${
                    selectedPlugin?.manifest.id === p.manifest.id ? "selected" : ""
                  }`}
                  onClick={() => setSelectedPluginId(p.manifest.id)}
                >
                  <div style={{ display: "flex", justifyContent: "space-between", gap: 8 }}>
                    <strong>{p.manifest.name}</strong>
                    {p.has_custom_ui && <span className="badge badge-cyan">Vite UI</span>}
                  </div>
                  <div className="mono muted" style={{ fontSize: 11, marginTop: 3 }}>
                    {p.manifest.id}@{p.manifest.version}
                  </div>
                  <div className="muted" style={{ fontSize: 12, marginTop: 6 }}>
                    {p.manifest.description}
                  </div>
                </div>
              ))}
            </div>

            {selectedPlugin && (
              <div className="panel">
                <div className="panel-header">
                  <div>
                    <h2 className="panel-title">{selectedPlugin.manifest.name}</h2>
                    <div className="mono muted" style={{ fontSize: 11.5 }}>
                      {selectedPlugin.manifest.image}
                    </div>
                  </div>
                  <button
                    type="button"
                    data-testid="plugin-prepare-btn"
                    className="btn btn-primary"
                    onClick={() => void handleQuickPrepare(selectedPlugin)}
                  >
                    Prepare Quote ({formatMilliAct(selectedPlugin.manifest.price_milli_act)})
                  </button>
                </div>

                {selectedPlugin.has_custom_ui && activeGrant ? (
                  <SandboxedPluginFrame
                    plugin={selectedPlugin}
                    activeGrant={activeGrant}
                    defaultCatalogId={defaultCatalogId}
                    latestPluginRun={latestRunForSelectedPlugin}
                    onQuotePrepared={handleQuotePrepared}
                  />
                ) : (
                  <div className="sci-card">
                    <div className="sci-card-title">SKILL.md Agent Contract</div>
                    <pre className="sql-pre">
                      {selectedPlugin.skill?.markdown_body ?? "Declarative OCI Plugin"}
                    </pre>
                  </div>
                )}
              </div>
            )}
          </div>
        )}

        {activeTab === "studio" && (
          <div className="studio-grid">
            <form className="panel" onSubmit={(e) => void handleScaffoldSubmit(e)}>
              <h2 className="panel-title" style={{ marginBottom: 14 }}>
                Scaffold New OCI Bio Plugin
              </h2>

              <div className="form-field">
                <label className="muted">Runtime Template</label>
                <select
                  className="text-input"
                  value={studioTemplate}
                  onChange={(e) => setStudioTemplate(e.target.value)}
                >
                  <option value="python-uv">python-uv (Python 3.12 + DuckDB)</option>
                  <option value="r-bioc">r-bioc (R / Bioconductor)</option>
                  <option value="nextflow">nextflow (Single-Container DSL2)</option>
                  <option value="rust-cli">rust-cli (Static Rust Binary)</option>
                </select>
              </div>

              <div className="form-field">
                <label className="muted">Plugin ID (`org/slug`)</label>
                <input
                  type="text"
                  className="text-input mono"
                  value={studioPluginId}
                  onChange={(e) => setStudioPluginId(e.target.value)}
                />
              </div>

              <div className="form-field">
                <label className="muted">Display Name</label>
                <input
                  type="text"
                  className="text-input"
                  value={studioName}
                  onChange={(e) => setStudioName(e.target.value)}
                />
              </div>

              <label
                style={{
                  display: "flex",
                  alignItems: "center",
                  gap: 8,
                  marginBottom: 14,
                  cursor: "pointer",
                }}
              >
                <input
                  type="checkbox"
                  checked={studioIncludeUi}
                  onChange={(e) => setStudioIncludeUi(e.target.checked)}
                />
                Include Custom Sandboxed Vite UI (`ui/`)
              </label>

              <button
                type="submit"
                data-testid="scaffold-submit-btn"
                className="btn btn-primary"
                style={{ width: "100%" }}
              >
                Generate Plugin Bundle (`plugin.toml` + `Dockerfile` + `SKILL.md`)
              </button>
            </form>

            <div className="panel" data-testid="scaffold-files">
              <div className="panel-header">
                <h2 className="panel-title">Generated Bundle Inspector</h2>
                {scaffoldOutput && (
                  <span className="badge badge-emerald mono">{scaffoldOutput.directory}</span>
                )}
              </div>
              {!scaffoldOutput ? (
                <div className="muted">
                  Select one of the 4 OCI starter templates on the left to generate a ready-to-build `plugin.toml` + `Dockerfile` + `SKILL.md` bundle.
                </div>
              ) : (
                scaffoldOutput.files.map((f) => (
                  <div key={f.relative_path} style={{ marginBottom: 14 }}>
                    <div className="mono" style={{ fontWeight: 700, marginBottom: 4 }}>
                      {f.relative_path}
                    </div>
                    <pre className="sql-pre" style={{ maxHeight: 180 }}>
                      {f.content}
                    </pre>
                  </div>
                ))
              )}
            </div>
          </div>
        )}
      </main>
    </div>
  );
}
