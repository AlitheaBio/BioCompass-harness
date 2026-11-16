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
import {
  FishLogo,
  HeroFish,
  IconChatLines,
  IconCheckCircle,
  IconClose,
  IconCodeBrackets,
  IconFolderOpen,
  IconLayers,
  IconMoon,
  IconNewChat,
  IconPanelLeft,
  IconPanelRight,
  IconPluginPinwheel,
  IconPlus,
  IconSendArrow,
  IconShield,
  IconSun,
  IconWarning,
  RunningCompass,
} from "./components/DswIcons";
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

const FROGLET_STAGE_NAMES = ["Descriptor", "Offer", "Quote", "Deal", "Receipt"] as const;

function defaultParamsForPlugin(plugin: PluginSummary): Record<string, unknown> {
  const props =
    (plugin.manifest.inputs.schema["properties"] as
      | Record<string, { default?: unknown }>
      | undefined) ?? {};
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
  const [sidebarCollapsed, setSidebarCollapsed] = useState<boolean>(false);
  const [rightbarOpen, setRightbarOpen] = useState<boolean>(false);
  const [heroHovering, setHeroHovering] = useState<boolean>(false);
  const [heroForcedBlank, setHeroForcedBlank] = useState<boolean>(false);
  const [darkTheme, setDarkTheme] = useState<boolean>(() => {
    try {
      return window.localStorage.getItem("biocompass-dsw-dark") === "1";
    } catch {
      return false;
    }
  });

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
  const [selectedArtifactIdx, setSelectedArtifactIdx] = useState<number>(4);
  const [pendingQuote, setPendingQuote] = useState<RunQuote | null>(null);
  const [isConfirmingRun, setIsConfirmingRun] = useState<boolean>(false);
  const [liveLogs, setLiveLogs] = useState<string[]>([]);
  const [verifyReport, setVerifyReport] = useState<FrogletVerifyReport | null>(null);
  const [errorBanner, setErrorBanner] = useState<string | null>(null);

  // Agent session state
  const [sessions, setSessions] = useState<SessionRecord[]>([]);
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

  // Sync DeepSeek Harness dark-theme attribute on <body>
  useEffect(() => {
    document.body.toggleAttribute("data-ds-dark-theme", darkTheme);
    try {
      window.localStorage.setItem("biocompass-dsw-dark", darkTheme ? "1" : "0");
    } catch {
      // ignore storage quota errors
    }
  }, [darkTheme]);

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

  const selectedRunPlugin = useMemo(
    () => plugins.find((p) => p.manifest.id === selectedRun?.plugin_id) ?? null,
    [plugins, selectedRun],
  );

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
          setRightbarOpen(true);
        }

        let currentSessions = sList;
        let activeSession = sList[0];
        if (!activeSession && initialGrant) {
          activeSession = await createSession("Immunopeptidomics Exploration", initialGrant);
          currentSessions = [activeSession];
        }
        setSessions(currentSessions);
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

  const handleNewSession = async (): Promise<void> => {
    try {
      setErrorBanner(null);
      const created = await createSession(
        `HLA Session ${sessions.length + 1}`,
        activeGrant?.grant_id ?? "",
      );
      setSessions((prev) => [created, ...prev]);
      setSession(created);
      setSessionEvents([]);
      setStreamingText("");
      setHeroForcedBlank(true);
      setActiveTab("workbench");
    } catch (err: unknown) {
      setErrorBanner(err instanceof Error ? err.message : String(err));
    }
  };

  const handleSelectSession = async (target: SessionRecord): Promise<void> => {
    try {
      setSession(target);
      setActiveTab("workbench");
      const evs = await fetchSessionEvents(target.session_id);
      setSessionEvents(evs);
      setHeroForcedBlank(evs.length === 0);
    } catch (err: unknown) {
      setErrorBanner(err instanceof Error ? err.message : String(err));
    }
  };

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
    setHeroForcedBlank(false);
    setRightbarOpen(true);
    setErrorBanner(null);
    setVerifyReport(null);
    setLiveLogs([
      `[host] Confirming Froglet Quote ${quoteToRun.run_id} and dispatching hardened OCI container...`,
    ]);

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
      setSelectedArtifactIdx(Math.max(0, completed.froglet_chain.length - 1));
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

  const submitChatPrompt = async (rawMsg: string): Promise<void> => {
    const msg = rawMsg.trim();
    if (!session || !msg || chatBusy) return;
    setChatInput("");
    setHeroForcedBlank(false);
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

  const handleChatSubmit = async (e: React.FormEvent): Promise<void> => {
    e.preventDefault();
    await submitChatPrompt(chatInput);
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

  const isHeroPhase =
    activeTab === "workbench" &&
    sessionEvents.length === 0 &&
    !streamingText &&
    !chatBusy &&
    (runs.length === 0 || heroForcedBlank);

  const headerTitle = useMemo(() => {
    if (activeTab === "plugins") return "Plugins";
    if (activeTab === "catalogs") return "Data Catalogs";
    if (activeTab === "studio") return "Plugin Studio";
    if (isHeroPhase) return "";
    return session?.title ?? "Immunopeptidomics Exploration";
  }, [activeTab, isHeroPhase, session]);

  const renderApprovalPanel = () => {
    if (!pendingQuote) return null;
    return (
      <section className="dsh-approval-panel" data-testid="pending-quote-banner">
        <div className="dsh-approval-strip">
          <span style={{ display: "inline-flex", alignItems: "center", gap: 6 }}>
            <IconWarning size={15} />
            Two-Step Froglet Run Consent Required
          </span>
          <span className="mono">{formatMilliAct(pendingQuote.estimated_cost_act)}</span>
        </div>
        <div className="dsh-approval-body">
          <div className="dsh-approval-headline">
            Execute <span className="mono">{pendingQuote.plugin_id}</span>@{pendingQuote.plugin_version}
          </div>
          <div className="dsh-approval-meta">
            Run <span className="mono">{pendingQuote.run_id}</span> · Image{" "}
            <span className="mono">{pendingQuote.image_ref.slice(0, 68)}...</span>
          </div>
          <div className="dsh-approval-meta">
            Grant: <strong>{pendingQuote.dataset_grant_summary ?? "None"}</strong> · workload_hash{" "}
            <span className="mono">{pendingQuote.workload_hash.slice(0, 18)}...</span>
          </div>
        </div>
        <div className="dsh-approval-actions">
          <button
            type="button"
            data-testid="cancel-quote-btn"
            className="dsw-btn"
            disabled={isConfirmingRun}
            onClick={() => setPendingQuote(null)}
          >
            Dismiss
          </button>
          <button
            type="button"
            data-testid="confirm-run-btn"
            className="dsw-btn dsw-btn-primary"
            disabled={isConfirmingRun}
            onClick={() => void handleConfirmPendingRun()}
          >
            {isConfirmingRun
              ? "Running Hardened OCI Container..."
              : `Approve & Run (${formatMilliAct(pendingQuote.estimated_cost_act)})`}
          </button>
        </div>
      </section>
    );
  };

  const renderInputCard = () => (
    <form className="dsh-input-card" onSubmit={(e) => void handleChatSubmit(e)}>
      <textarea
        className="dsh-textarea"
        rows={isHeroPhase ? 2 : 1}
        placeholder="Ask BioCompass to explore HLA cohorts, query DuckDB catalogs, or prepare an OCI plugin run..."
        value={chatInput}
        onChange={(e) => setChatInput(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" && !e.shiftKey) {
            e.preventDefault();
            void submitChatPrompt(chatInput);
          }
        }}
      />
      <div className="dsh-input-toolbar">
        <div className="dsh-input-tools">
          <button
            type="button"
            className="dsh-plus-btn"
            title="Browse OCI Bio Plugins"
            onClick={() => setActiveTab("plugins")}
          >
            <IconPlus size={14} />
          </button>
          <button
            type="button"
            className="dsh-mode-chip"
            onClick={() => setActiveTab("catalogs")}
          >
            <IconLayers size={14} />
            <span>{defaultCatalogId}</span>
          </button>
          <span className="dsw-tag">
            {activeGrant?.mask_peptide_sequences ? "Masked ('***')" : "Unmasked IUPAC"}
          </span>
        </div>

        <div className="dsh-input-trailing">
          <span className="dsh-model-label">
            {health?.llm_configured ? health.llm_model ?? "LLM Connected" : "SKILL.md Agent"}
          </span>
          <button
            type="submit"
            className="dsh-send-circle"
            aria-label="Send message"
            disabled={chatBusy || !chatInput.trim()}
          >
            <IconSendArrow size={15} />
          </button>
        </div>
      </div>
    </form>
  );

  const inspectedChainDoc = useMemo(() => {
    if (!selectedRun || selectedRun.froglet_chain.length === 0) return null;
    const clampedIdx = Math.min(
      Math.max(0, selectedArtifactIdx),
      selectedRun.froglet_chain.length - 1,
    );
    return selectedRun.froglet_chain[clampedIdx] ?? selectedRun.froglet_receipt;
  }, [selectedRun, selectedArtifactIdx]);

  return (
    <div
      className="dsh-frame"
      data-sidebar-collapsed={String(sidebarCollapsed)}
      data-rightbar-open={String(rightbarOpen)}
    >
      {/* ====================================================================
          Column 1: Left Collapsible Sidebar (DeepSeek Harness SidebarRoot)
         ==================================================================== */}
      <aside className="dsh-sidebar" data-collapsed={String(sidebarCollapsed)}>
        <div className="dsh-sidebar-top">
          {!sidebarCollapsed ? (
            <BrandWordmark size={23} />
          ) : (
            <span data-testid="brand-wordmark" style={{ display: "inline-flex" }}>
              <FishLogo size={24} />
            </span>
          )}
          <button
            type="button"
            className="dsh-icon-btn"
            title={sidebarCollapsed ? "Expand sidebar" : "Collapse sidebar"}
            onClick={() => setSidebarCollapsed((v) => !v)}
          >
            <IconPanelLeft size={16} />
          </button>
        </div>

        <button
          type="button"
          className="dsh-new-session-btn"
          title="New Session"
          onClick={() => void handleNewSession()}
        >
          <IconNewChat size={16} />
          {!sidebarCollapsed && <span>New Session</span>}
        </button>

        <nav className="dsh-panel-list" aria-label="Primary navigation">
          <button
            type="button"
            data-testid="nav-workbench"
            className="dsh-panel-row"
            data-active={String(activeTab === "workbench")}
            title="Conversation & Workbench"
            onClick={() => setActiveTab("workbench")}
          >
            <IconChatLines size={16} />
            {!sidebarCollapsed && <span>Conversation</span>}
          </button>

          <button
            type="button"
            data-testid="nav-plugins"
            className="dsh-panel-row"
            data-active={String(activeTab === "plugins")}
            title="Plugins"
            onClick={() => setActiveTab("plugins")}
          >
            <IconPluginPinwheel size={16} />
            {!sidebarCollapsed && (
              <>
                <span>Plugins</span>
                <span className="dsh-panel-badge">{plugins.length}</span>
              </>
            )}
          </button>

          <button
            type="button"
            data-testid="nav-catalogs"
            className="dsh-panel-row"
            data-active={String(activeTab === "catalogs")}
            title="Data Catalogs (DuckDB)"
            onClick={() => setActiveTab("catalogs")}
          >
            <IconLayers size={16} />
            {!sidebarCollapsed && <span>Data Catalogs</span>}
          </button>

          <button
            type="button"
            data-testid="nav-studio"
            className="dsh-panel-row"
            data-active={String(activeTab === "studio")}
            title="Plugin Studio"
            onClick={() => setActiveTab("studio")}
          >
            <IconCodeBrackets size={16} />
            {!sidebarCollapsed && <span>Plugin Studio</span>}
          </button>
        </nav>

        {!sidebarCollapsed && (
          <div className="dsh-sidebar-scroll">
            <div>
              <div className="dsh-section-label">
                <IconFolderOpen size={14} />
                <span>Immunopeptidomics Atlas</span>
              </div>
              <div className="dsh-tree-list">
                {sessions.map((s) => (
                  <button
                    key={s.session_id}
                    type="button"
                    className="dsh-tree-row"
                    data-active={String(
                      activeTab === "workbench" && session?.session_id === s.session_id,
                    )}
                    onClick={() => void handleSelectSession(s)}
                  >
                    <IconChatLines size={14} />
                    <span className="dsh-tree-row-label">{s.title}</span>
                  </button>
                ))}
              </div>
            </div>

            {runs.length > 0 && (
              <div>
                <div className="dsh-section-label">
                  <span>Verifiable OCI Runs</span>
                </div>
                <div className="dsh-tree-list">
                  {runs.slice(0, 6).map((r) => (
                    <button
                      key={r.run_id}
                      type="button"
                      className="dsh-tree-row"
                      data-active={String(selectedRun?.run_id === r.run_id && rightbarOpen)}
                      onClick={() => {
                        setSelectedRunId(r.run_id);
                        setVerifyReport(null);
                        setRightbarOpen(true);
                      }}
                    >
                      <span
                        className={`dsh-dot ${r.receipt_verified ? "" : "dsh-dot-warn"}`}
                      />
                      <span className="dsh-tree-row-label">
                        {r.plugin_id.replace("alithea/", "")}
                      </span>
                      <span className="dsw-caption mono">
                        {formatMilliAct(r.quote.estimated_cost_act)}
                      </span>
                    </button>
                  ))}
                </div>
              </div>
            )}
          </div>
        )}

        <div className="dsh-sidebar-footer">
          <div
            className="dsh-health-pill"
            data-testid="health-pill"
            title={health ? `${health.container_engine} · ${health.node_id}` : "Checking node"}
          >
            <span className="dsh-dot" />
            {!sidebarCollapsed ? (
              <span>
                {health ? `${health.container_engine} · ${health.node_id}` : "connecting..."}
              </span>
            ) : (
              <span style={{ fontSize: 10 }}>
                {health ? health.container_engine : "..."}
              </span>
            )}
          </div>

          <button
            type="button"
            className="dsh-icon-btn"
            title={darkTheme ? "Switch to Light theme" : "Switch to Dark theme"}
            onClick={() => setDarkTheme((d) => !d)}
          >
            {darkTheme ? <IconSun size={16} /> : <IconMoon size={16} />}
          </button>
        </div>
      </aside>

      {/* ====================================================================
          Column 2: Center Surface (Conversation Hero / Stream / Entry Pages)
         ==================================================================== */}
      <main className="dsh-center">
        <header className="dsh-header" data-blank={String(isHeroPhase)}>
          <div className="dsh-header-leading">
            {headerTitle && <span className="dsh-header-title">{headerTitle}</span>}
          </div>

          <div className="dsh-header-utilities">
            <div className="dsh-grant-chip" title="Active DuckDB DatasetGrant">
              <IconShield size={14} />
              <select
                data-testid="grant-select"
                className="dsh-grant-select"
                aria-label="Data Grant"
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

            <div className="dsh-act-pill" data-testid="act-balance">
              {ledger ? ledger.available_display : "— ACT"}
            </div>

            <button
              type="button"
              className="dsh-inspector-toggle"
              data-active={String(rightbarOpen)}
              onClick={() => setRightbarOpen((o) => !o)}
            >
              <IconPanelRight size={15} />
              <span>Inspector</span>
              {runs.length > 0 && <span className="dsh-dot" />}
            </button>
          </div>
        </header>

        {errorBanner && (
          <div
            data-testid="error-banner"
            style={{
              margin: "8px 24px 0",
              padding: "10px 14px",
              borderRadius: 12,
              background: "var(--dsw-alias-state-error-tertiary)",
              color: "var(--dsw-alias-state-error-primary)",
              fontSize: 13,
              display: "flex",
              justifyContent: "space-between",
              alignItems: "center",
            }}
          >
            <span>{errorBanner}</span>
            <button
              type="button"
              className="dsh-icon-btn"
              onClick={() => setErrorBanner(null)}
            >
              <IconClose size={14} />
            </button>
          </div>
        )}

        {/* ------------------ CONVERSATION / WORKBENCH VIEW ------------------ */}
        {activeTab === "workbench" && (
          <>
            <div className="dsh-conversation-scroll" data-hero={String(isHeroPhase)}>
              {isHeroPhase ? (
                <div className="dsh-hero-stack">
                  <div className="dsh-hero-headline">
                    <span
                      className="dsh-hero-fish-hitbox"
                      onPointerEnter={() => setHeroHovering(true)}
                      onPointerLeave={() => setHeroHovering(false)}
                    >
                      <HeroFish hovering={heroHovering} />
                    </span>
                    <span style={{ display: "inline-flex", alignItems: "center", gap: 8 }}>
                      <span>What would you like to explore?</span>
                      <span className="dsh-preview-badge">froglet/v1</span>
                    </span>
                  </div>

                  <div className="dsh-hero-workspace-row">
                    <button
                      type="button"
                      className="dsh-workspace-chip"
                      onClick={() => setActiveTab("catalogs")}
                    >
                      <IconFolderOpen size={16} />
                      <span>{defaultCatalogId}</span>
                    </button>
                    <span className="dsw-caption">
                      Hardened rootless OCI · BIP340 Schnorr receipts
                    </span>
                  </div>

                  {renderApprovalPanel()}
                  {renderInputCard()}

                  <div className="dsh-starter-pills">
                    {plugins.map((p) => (
                      <button
                        key={p.manifest.id}
                        type="button"
                        data-testid={`quick-prepare-btn-${p.manifest.id}`}
                        className="dsh-starter-pill"
                        onClick={() => void handleQuickPrepare(p)}
                      >
                        <IconPluginPinwheel size={15} />
                        <span>Run {p.manifest.name}</span>
                        <span className="dsh-starter-cost">
                          {formatMilliAct(p.manifest.price_milli_act)}
                        </span>
                      </button>
                    ))}
                    <button
                      type="button"
                      className="dsh-starter-pill"
                      onClick={() =>
                        void submitChatPrompt(
                          "Query strong HLA-A*02:01 peptide binders in our active cohort.",
                        )
                      }
                    >
                      <span>Explore HLA-A*02:01 binders</span>
                    </button>
                  </div>
                </div>
              ) : (
                <div className="dsh-chat-stream" data-testid="chat-feed">
                  <div className="dsh-quick-bar">
                    <span className="dsw-caption" style={{ marginRight: 4 }}>
                      Quick Prepare OCI Plugin:
                    </span>
                    {plugins.map((p) => (
                      <button
                        key={p.manifest.id}
                        type="button"
                        data-testid={`quick-prepare-btn-${p.manifest.id}`}
                        className="dsh-starter-pill"
                        style={{ height: 30, padding: "0 12px", fontSize: 12.5 }}
                        onClick={() => void handleQuickPrepare(p)}
                      >
                        <IconPluginPinwheel size={14} />
                        <span>{p.manifest.name}</span>
                        <span className="dsh-starter-cost">
                          {formatMilliAct(p.manifest.price_milli_act)}
                        </span>
                      </button>
                    ))}
                  </div>

                  {sessionEvents.map((ev) =>
                    ev.kind === "user" ? (
                      <div key={ev.event_id} className="dsh-user-row">
                        <div className="dsh-user-bubble">{ev.content}</div>
                      </div>
                    ) : (
                      <div key={ev.event_id} className="dsh-assistant-row">
                        <span className="dsh-assistant-avatar">
                          <FishLogo size={22} />
                        </span>
                        <div className="dsh-assistant-body">
                          {ev.kind !== "assistant" && (
                            <span className="dsw-tag mono" style={{ marginRight: 6 }}>
                              {ev.kind}
                            </span>
                          )}
                          {ev.content}
                        </div>
                      </div>
                    ),
                  )}

                  {(streamingText || chatBusy) && (
                    <div className="dsh-assistant-row">
                      <span className="dsh-assistant-avatar">
                        <RunningCompass size={20} />
                      </span>
                      <div className="dsh-assistant-body">
                        {streamingText || "Thinking..."}
                      </div>
                    </div>
                  )}

                  {runs.map((r) => (
                    <div
                      key={r.run_id}
                      className="dsh-run-turn-card"
                      data-selected={String(selectedRun?.run_id === r.run_id)}
                    >
                      <div style={{ minWidth: 0 }}>
                        <div
                          style={{
                            display: "flex",
                            alignItems: "center",
                            gap: 8,
                            fontWeight: 500,
                            fontSize: 13.5,
                          }}
                        >
                          <IconCheckCircle size={15} />
                          <span>{r.plugin_id}</span>
                          <span className="dsw-tag dsw-tag-success">
                            {r.receipt_verified ? "Schnorr Verified" : r.status}
                          </span>
                        </div>
                        <div className="dsw-caption mono" style={{ marginTop: 3 }}>
                          {r.run_id} · Settled {formatMilliAct(r.quote.estimated_cost_act)} ·{" "}
                          {r.artifacts.length} artifact(s)
                        </div>
                      </div>
                      <button
                        type="button"
                        className="dsw-btn"
                        onClick={() => {
                          setSelectedRunId(r.run_id);
                          setRightbarOpen(true);
                        }}
                      >
                        Inspect Output &amp; Receipt
                      </button>
                    </div>
                  ))}
                </div>
              )}
            </div>

            {!isHeroPhase && (
              <div className="dsh-composer-dock">
                {renderApprovalPanel()}
                {renderInputCard()}
              </div>
            )}
          </>
        )}

        {/* ------------------ PLUGINS 960PX ENTRY PAGE ------------------ */}
        {activeTab === "plugins" && (
          <>
            <div className="dsh-entry-scroll">
              <div className="dsh-entry-page">
                <div className="dsh-entry-header">
                  <div>
                    <h1 className="dsh-entry-title">Plugins</h1>
                    <p className="dsh-entry-subtitle">
                      Digest-pinned OCI bio containers and sandboxed{" "}
                      <code>&lt;iframe sandbox=&quot;allow-scripts&quot;&gt;</code> UIs.
                    </p>
                  </div>
                  <button
                    type="button"
                    className="dsw-btn dsw-btn-pill"
                    onClick={() => setActiveTab("studio")}
                  >
                    <IconPlus size={14} />
                    <span>Create Plugin</span>
                  </button>
                </div>

                <div>
                  <div className="dsh-section-label" style={{ paddingLeft: 2, marginBottom: 6 }}>
                    INSTALLED ({plugins.length})
                  </div>
                  <div className="dsh-group-card">
                    {plugins.map((p) => (
                      <div
                        key={p.manifest.id}
                        data-testid={`plugin-card-${p.manifest.id}`}
                        className="dsh-plugin-row"
                        data-selected={String(selectedPlugin?.manifest.id === p.manifest.id)}
                        onClick={() => setSelectedPluginId(p.manifest.id)}
                      >
                        <div style={{ display: "flex", alignItems: "flex-start", gap: 12 }}>
                          <span
                            style={{
                              display: "grid",
                              placeItems: "center",
                              width: 34,
                              height: 34,
                              borderRadius: 10,
                              background: "var(--dsw-alias-interactive-bg-hover)",
                              flex: "none",
                              marginTop: 2,
                            }}
                          >
                            <IconPluginPinwheel size={18} />
                          </span>
                          <div>
                            <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
                              <strong style={{ fontSize: 14, fontWeight: 500 }}>
                                {p.manifest.name}
                              </strong>
                              <span className="dsw-tag mono">v{p.manifest.version}</span>
                              {p.has_custom_ui && (
                                <span className="dsw-tag dsw-tag-info">Vite Sandbox UI</span>
                              )}
                            </div>
                            <div
                              style={{
                                fontSize: 13,
                                color: "var(--dsw-alias-label-secondary)",
                                marginTop: 3,
                              }}
                            >
                              {p.manifest.description}
                            </div>
                          </div>
                        </div>

                        <span className="dsw-tag mono">
                          {formatMilliAct(p.manifest.price_milli_act)}
                        </span>
                      </div>
                    ))}
                  </div>
                </div>

                {selectedPlugin && (
                  <div className="dsh-group-card dsh-card-pad">
                    <div
                      style={{
                        display: "flex",
                        justifyContent: "space-between",
                        alignItems: "flex-start",
                        gap: 12,
                        marginBottom: 14,
                        flexWrap: "wrap",
                      }}
                    >
                      <div>
                        <div style={{ fontSize: 16, fontWeight: 600 }}>
                          {selectedPlugin.manifest.name}
                        </div>
                        <div className="dsw-caption mono" style={{ marginTop: 3 }}>
                          {selectedPlugin.manifest.image}
                        </div>
                      </div>
                      <button
                        type="button"
                        data-testid="plugin-prepare-btn"
                        className="dsw-btn dsw-btn-primary dsw-btn-pill"
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
                      <div>
                        <div className="sci-card-title">SKILL.md Agent Contract</div>
                        <pre className="sql-pre">
                          {selectedPlugin.skill?.markdown_body ?? "Declarative OCI Plugin"}
                        </pre>
                      </div>
                    )}
                  </div>
                )}
              </div>
            </div>

            {pendingQuote && <div className="dsh-composer-dock">{renderApprovalPanel()}</div>}
          </>
        )}

        {/* ------------------ DATA CATALOGS 960PX ENTRY PAGE ------------------ */}
        {activeTab === "catalogs" && (
          <>
            <div className="dsh-entry-scroll">
              <div className="dsh-entry-page">
                <div className="dsh-entry-header">
                  <div>
                    <h1 className="dsh-entry-title">Data Catalogs</h1>
                    <p className="dsh-entry-subtitle">
                      SHA-256 verified Parquet shards queried via DuckDB Grant-compiled{" "}
                      <code>TEMP VIEW</code>s · <span className="mono">{defaultCatalogId}</span>
                    </p>
                  </div>

                  <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
                    <label style={{ fontSize: 13, color: "var(--dsw-alias-label-secondary)" }}>
                      Max IC50 (nM):{" "}
                      <input
                        type="number"
                        className="dsw-input"
                        style={{ width: 90, marginLeft: 6 }}
                        value={catalogMaxIc50}
                        onChange={(e) => setCatalogMaxIc50(Number(e.target.value) || 500)}
                      />
                    </label>
                    {catalogResult && (
                      <span
                        data-testid="catalog-mask-indicator"
                        className={`dsw-tag ${
                          catalogResult.sequence_masked ? "dsw-tag-warn" : "dsw-tag-success"
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
                  <div className="dsh-group-card dsh-card-pad">
                    <div className="sci-card-title">Compiled DuckDB Grant SQL View</div>
                    <pre className="sql-pre" data-testid="catalog-sql-view">
                      {catalogResult.compiled_view_sql}
                    </pre>

                    <div className="table-scroll" style={{ marginTop: 14 }}>
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
                  </div>
                )}
              </div>
            </div>

            {pendingQuote && <div className="dsh-composer-dock">{renderApprovalPanel()}</div>}
          </>
        )}

        {/* ------------------ PLUGIN STUDIO 960PX ENTRY PAGE ------------------ */}
        {activeTab === "studio" && (
          <>
            <div className="dsh-entry-scroll">
              <div className="dsh-entry-page">
                <div className="dsh-entry-header">
                  <div>
                    <h1 className="dsh-entry-title">Plugin Studio</h1>
                    <p className="dsh-entry-subtitle">
                      Scaffold a Universal OCI Plugin (<code>plugin.toml</code> +{" "}
                      <code>Dockerfile</code> + <code>SKILL.md</code> + optional Vite Sandbox UI).
                    </p>
                  </div>
                </div>

                <div className="dsh-group-card dsh-card-pad">
                  <form
                    onSubmit={(e) => void handleScaffoldSubmit(e)}
                    style={{
                      display: "grid",
                      gridTemplateColumns: "repeat(auto-fit, minmax(220px, 1fr))",
                      gap: 14,
                      alignItems: "end",
                    }}
                  >
                    <div>
                      <div className="sci-card-title">Runtime Template</div>
                      <select
                        className="dsw-input"
                        style={{ width: "100%" }}
                        value={studioTemplate}
                        onChange={(e) => setStudioTemplate(e.target.value)}
                      >
                        <option value="python-uv">Python 3.12 + uv + Polars/PyArrow</option>
                        <option value="r-rocker">R 4.4 Rocker + arrow</option>
                        <option value="rust-cli">Rust 2024 Static Binary</option>
                      </select>
                    </div>

                    <div>
                      <div className="sci-card-title">Plugin ID (namespace/slug)</div>
                      <input
                        type="text"
                        data-testid="studio-plugin-id-input"
                        className="dsw-input"
                        style={{ width: "100%" }}
                        value={studioPluginId}
                        onChange={(e) => setStudioPluginId(e.target.value)}
                      />
                    </div>

                    <div>
                      <div className="sci-card-title">Display Name</div>
                      <input
                        type="text"
                        className="dsw-input"
                        style={{ width: "100%" }}
                        value={studioName}
                        onChange={(e) => setStudioName(e.target.value)}
                      />
                    </div>

                    <div style={{ display: "flex", alignItems: "center", gap: 12 }}>
                      <label style={{ fontSize: 13, display: "flex", alignItems: "center", gap: 6 }}>
                        <input
                          type="checkbox"
                          checked={studioIncludeUi}
                          onChange={(e) => setStudioIncludeUi(e.target.checked)}
                        />
                        Include Vite Sandbox UI
                      </label>
                      <button
                        type="submit"
                        data-testid="studio-scaffold-btn"
                        className="dsw-btn dsw-btn-primary"
                      >
                        Generate Plugin
                      </button>
                    </div>
                  </form>

                  {scaffoldOutput && (
                    <div style={{ marginTop: 18 }} data-testid="scaffold-output">
                      <div className="sci-card-title">
                        Scaffolded into{" "}
                        <span className="mono">{scaffoldOutput.directory}</span>
                      </div>
                      <pre className="sql-pre">
                        {scaffoldOutput.files.map((f) => f.relative_path).join("\n")}
                      </pre>
                    </div>
                  )}
                </div>
              </div>
            </div>

            {pendingQuote && <div className="dsh-composer-dock">{renderApprovalPanel()}</div>}
          </>
        )}
      </main>

      {/* ====================================================================
          Column 3: Right Collapsible Inspector Dock (SidebarRight)
         ==================================================================== */}
      <aside className="dsh-rightbar">
        <div className="dsh-rightbar-header">
          <div style={{ display: "flex", alignItems: "center", gap: 8, minWidth: 0 }}>
            <strong style={{ fontSize: 13.5, fontWeight: 600 }}>Execution &amp; Receipt</strong>
            {runs.length > 0 && (
              <select
                className="dsw-input mono"
                style={{ height: 28, fontSize: 11.5, maxWidth: 190, padding: "0 8px" }}
                value={selectedRun?.run_id ?? ""}
                onChange={(e) => {
                  setSelectedRunId(e.target.value);
                  setVerifyReport(null);
                }}
              >
                {runs.map((r) => (
                  <option key={r.run_id} value={r.run_id}>
                    {r.run_id.slice(0, 14)} · {r.plugin_id.replace("alithea/", "")}
                  </option>
                ))}
              </select>
            )}
          </div>

          <button
            type="button"
            className="dsh-icon-btn"
            title="Close Inspector"
            onClick={() => setRightbarOpen(false)}
          >
            <IconClose size={15} />
          </button>
        </div>

        <div className="dsh-rightbar-scroll">
          <div className="dsh-inspector-section">
            <div className="dsh-inspector-title">
              <span>Hardened OCI Container Stream</span>
              {selectedRun && (
                <span className="dsw-tag mono">{selectedRun.status}</span>
              )}
            </div>
            <pre className="terminal-box" data-testid="live-run-logs">
              {(liveLogs.length > 0
                ? liveLogs
                : selectedRun?.logs ?? [
                    "Ready. Prepare a Froglet Quote and click 'Approve & Run' to stream real container stdout/stderr over SSE.",
                  ]
              ).join("\n")}
            </pre>
          </div>

          {selectedRun?.result_json && (
            <div className="dsh-inspector-section">
              <div className="dsh-inspector-title">
                <span>Declarative Scientific Views (`/out/result.json`)</span>
              </div>
              <ScientificViews
                resultJson={selectedRun.result_json}
                views={selectedRunPlugin?.views ?? []}
              />
            </div>
          )}

          {selectedRun && selectedRun.artifacts.length > 0 && (
            <div className="dsh-inspector-section" data-testid="run-artifacts">
              <div className="dsh-inspector-title">
                <span>Stage-Out BlobStore Artifacts</span>
              </div>
              <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
                {selectedRun.artifacts.map((art) => (
                  <a
                    key={art.name}
                    href={`/api/v1/runs/${encodeURIComponent(selectedRun.run_id)}/artifacts/${art.name}`}
                    target="_blank"
                    rel="noreferrer"
                    className="dsw-btn mono"
                    style={{ fontSize: 11.5 }}
                  >
                    {art.name} ({art.size_bytes} B · {art.sha256.slice(0, 8)})
                  </a>
                ))}
              </div>
            </div>
          )}

          {selectedRun && selectedRun.froglet_chain.length > 0 && (
            <div className="dsh-inspector-section" data-testid="receipt-inspector">
              <div className="dsh-inspector-title">
                <span>Froglet v1 Schnorr 5-Artifact Chain</span>
                <span
                  data-testid="receipt-verified-badge"
                  className={`dsw-tag ${
                    (verifyReport ? verifyReport.valid : selectedRun.receipt_verified)
                      ? "dsw-tag-success"
                      : "dsw-tag-danger"
                  }`}
                >
                  {(verifyReport ? verifyReport.valid : selectedRun.receipt_verified)
                    ? "SCHNORR CHAIN VALID"
                    : "TAMPER DETECTED / INVALID"}
                </span>
              </div>

              <div className="dsw-caption">
                RFC 8785 JCS + BIP340 secp256k1 (<code>settlement_method = &quot;none&quot;</code>)
              </div>

              <div style={{ display: "flex", gap: 6, flexWrap: "wrap" }}>
                {selectedRun.froglet_chain.map((_, idx) => (
                  <button
                    key={idx}
                    type="button"
                    className="dsw-btn"
                    style={{
                      height: 26,
                      padding: "0 10px",
                      fontSize: 11.5,
                      borderColor:
                        selectedArtifactIdx === idx
                          ? "var(--dsw-alias-state-business-primary)"
                          : undefined,
                    }}
                    onClick={() => setSelectedArtifactIdx(idx)}
                  >
                    {FROGLET_STAGE_NAMES[idx] ?? `Step ${idx + 1}`}
                  </button>
                ))}
              </div>

              <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
                <button
                  type="button"
                  data-testid="reverify-chain-btn"
                  className="dsw-btn"
                  onClick={() => void handleReverifyReceipt(false)}
                >
                  Re-Verify Chain
                </button>
                <button
                  type="button"
                  data-testid="tamper-test-btn"
                  className="dsw-btn"
                  onClick={() => void handleReverifyReceipt(true)}
                >
                  Simulate 1-Byte Tamper
                </button>
              </div>

              <pre className="sql-pre" style={{ maxHeight: 220 }}>
                {JSON.stringify(inspectedChainDoc, null, 2)}
              </pre>
            </div>
          )}
        </div>
      </aside>
    </div>
  );
}
