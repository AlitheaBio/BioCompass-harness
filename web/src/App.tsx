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
  FloatingWindowsLayer,
  type ModuleWindowState,
  type WindowTab,
} from "./components/FloatingWindows";
import {
  CompassMark,
  HeroCompass,
  IconChat,
  IconClose,
  IconDatabase,
  IconModules,
  IconPlus,
  IconSend,
  IconSettings,
  IconSidebar,
  IconWindowOpen,
  RunningCompass,
} from "./components/Icons";
import { SettingsModal, type SettingsSection } from "./components/SettingsModal";
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

type WorkspacePage = "workbench" | "plugins" | "catalogs";

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

const SUGGESTED_PROMPTS = [
  "Summarize strong HLA-A*02:01 binders across lung cohorts",
  "Compare motif length distributions in tumor vs healthy tissue",
  "Which modules are available in this workspace?",
];

export function App() {
  const [activePage, setActivePage] = useState<WorkspacePage>("workbench");
  const [sidebarCollapsed, setSidebarCollapsed] = useState<boolean>(false);
  const [heroHovering, setHeroHovering] = useState<boolean>(false);
  const [moduleSearch, setModuleSearch] = useState<string>("");

  const [darkTheme, setDarkTheme] = useState<boolean>(() => {
    try {
      return window.localStorage.getItem("biocompass-theme") === "dark";
    } catch {
      return false;
    }
  });

  // Settings modal state
  const [settingsOpen, setSettingsOpen] = useState<boolean>(false);
  const [settingsSection, setSettingsSection] = useState<SettingsSection>("general");
  const [scaffoldOutput, setScaffoldOutput] = useState<ScaffoldResult | null>(null);

  // Core data & node state
  const [health, setHealth] = useState<HealthStatus | null>(null);
  const [grants, setGrants] = useState<DatasetGrant[]>([]);
  const [activeGrantId, setActiveGrantId] = useState<string>("");
  const [catalogs, setCatalogs] = useState<CatalogManifest[]>([]);
  const [catalogResult, setCatalogResult] = useState<CatalogQueryResult | null>(null);
  const [catalogMaxIc50, setCatalogMaxIc50] = useState<number>(500);
  const [plugins, setPlugins] = useState<PluginSummary[]>([]);
  const [ledger, setLedger] = useState<LedgerState | null>(null);

  // Runs & verification state
  const [runs, setRuns] = useState<RunRecord[]>([]);
  const [pendingQuote, setPendingQuote] = useState<RunQuote | null>(null);
  const [isConfirmingRun, setIsConfirmingRun] = useState<boolean>(false);
  const [liveLogs, setLiveLogs] = useState<string[]>([]);
  const [verifyReport, setVerifyReport] = useState<FrogletVerifyReport | null>(null);
  const [errorBanner, setErrorBanner] = useState<string | null>(null);

  // Floating module windows state (HLA-Compass window manager)
  const [windows, setWindows] = useState<ModuleWindowState[]>([]);
  const [zCounter, setZCounter] = useState<number>(10);

  // Conversational session state
  const [sessions, setSessions] = useState<SessionRecord[]>([]);
  const [session, setSession] = useState<SessionRecord | null>(null);
  const [sessionEvents, setSessionEvents] = useState<SessionEvent[]>([]);
  const [chatInput, setChatInput] = useState<string>("");
  const [streamingText, setStreamingText] = useState<string>("");
  const [chatBusy, setChatBusy] = useState<boolean>(false);

  // Sync theme attribute
  useEffect(() => {
    document.body.setAttribute("data-theme", darkTheme ? "dark" : "light");
    try {
      window.localStorage.setItem("biocompass-theme", darkTheme ? "dark" : "light");
    } catch {
      // ignore storage errors
    }
  }, [darkTheme]);

  // Cmd/Ctrl + , shortcut to open Settings
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key === ",") {
        e.preventDefault();
        setSettingsSection("general");
        setSettingsOpen((prev) => !prev);
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, []);

  const activeGrant = useMemo<DatasetGrant | null>(() => {
    if (grants.length === 0) return null;
    return grants.find((g) => g.grant_id === activeGrantId) ?? grants[0] ?? null;
  }, [grants, activeGrantId]);

  const defaultCatalogId = catalogs[0]?.id ?? "alithea/hla-atlas-v2026.1";

  const openOrFocusModuleWindow = useCallback(
    (plugin: PluginSummary, preferredTab?: WindowTab) => {
      setZCounter((prevZ) => {
        const nextZ = prevZ + 1;
        setWindows((prev) => {
          const existing = prev.find((w) => w.pluginId === plugin.manifest.id);
          if (existing) {
            return prev.map((w) =>
              w.id === existing.id
                ? {
                    ...w,
                    minimized: false,
                    z: nextZ,
                    activeTab: preferredTab ?? w.activeTab,
                  }
                : w,
            );
          }

          const sidebarW = sidebarCollapsed ? 60 : 232;
          const cascade = (prev.length % 4) * 24;
          const w = Math.min(820, Math.max(540, window.innerWidth - sidebarW - 56));
          const h = Math.min(560, Math.max(410, window.innerHeight - 130));
          const baseX = Math.max(
            sidebarW + 18,
            Math.round(sidebarW + (window.innerWidth - sidebarW - w) / 2),
          );
          const baseY = 64;

          const created: ModuleWindowState = {
            id: `win-${plugin.manifest.id}`,
            pluginId: plugin.manifest.id,
            x: Math.min(window.innerWidth - w - 16, baseX + cascade),
            y: Math.min(window.innerHeight - h - 56, baseY + cascade),
            w,
            h,
            z: nextZ,
            minimized: false,
            maximized: false,
            activeTab: preferredTab ?? (plugin.has_custom_ui ? "ui" : "run"),
            params: defaultParamsForPlugin(plugin),
          };
          return [...prev, created];
        });
        return nextZ;
      });
    },
    [sidebarCollapsed],
  );

  const navigateToPage = (page: WorkspacePage) => {
    setActivePage(page);
    // Minimize open module windows into the bottom tray when switching top-level pages
    // so the destination page (Chat, Modules grid, or Data table) is cleanly visible.
    setWindows((prev) =>
      prev.some((w) => !w.minimized)
        ? prev.map((w) => ({ ...w, minimized: true }))
        : prev,
    );
  };

  const openSettings = (section: SettingsSection = "general") => {
    setSettingsSection(section);
    setSettingsOpen(true);
  };

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
        setLedger(lState);
        setRuns(rList);

        let currentSessions = sList;
        let activeSession = sList[0];
        if (!activeSession && initialGrant) {
          activeSession = await createSession(
            "Immunopeptidomics Exploration",
            initialGrant,
          );
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

  // Re-query catalog whenever activeGrant or IC50 threshold changes
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
      navigateToPage("workbench");
    } catch (err: unknown) {
      setErrorBanner(err instanceof Error ? err.message : String(err));
    }
  };

  const handleSelectSession = async (target: SessionRecord): Promise<void> => {
    try {
      setSession(target);
      navigateToPage("workbench");
      const evs = await fetchSessionEvents(target.session_id);
      setSessionEvents(evs);
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

  const handleQuotePrepared = useCallback(
    (quote: RunQuote) => {
      setPendingQuote(quote);
      setVerifyReport(null);
      setErrorBanner(null);
      const targetPlugin = plugins.find((p) => p.manifest.id === quote.plugin_id);
      if (targetPlugin) {
        openOrFocusModuleWindow(targetPlugin);
      }
    },
    [plugins, openOrFocusModuleWindow],
  );

  const handlePreparePluginRun = async (
    plugin: PluginSummary,
    params?: Record<string, unknown>,
  ): Promise<void> => {
    try {
      setErrorBanner(null);
      openOrFocusModuleWindow(plugin);
      const quote = await prepareRun({
        plugin_id: plugin.manifest.id,
        params: params ?? defaultParamsForPlugin(plugin),
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
    setLiveLogs([
      `[host] Dispatching OCI container for ${quoteToRun.plugin_id} (${quoteToRun.run_id})...`,
    ]);

    // Switch standard parameter windows directly to Results so live logs & charts appear seamlessly;
    // keep custom interactive UI windows on their "ui" tab so postMessage updates stay live.
    setWindows((prev) =>
      prev.map((w) =>
        w.pluginId === quoteToRun.plugin_id && w.activeTab !== "ui"
          ? { ...w, minimized: false, activeTab: "results" }
          : w,
      ),
    );

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

  const handleReverifyReceipt = async (
    run: RunRecord,
    tamperOneByte: boolean,
  ): Promise<void> => {
    if (run.froglet_chain.length === 0) return;
    const chainCopy = JSON.parse(
      JSON.stringify(run.froglet_chain),
    ) as Record<string, unknown>[];

    if (tamperOneByte) {
      const receiptDoc = chainCopy[chainCopy.length - 1];
      const payload =
        receiptDoc &&
        typeof receiptDoc["payload"] === "object" &&
        receiptDoc["payload"] !== null
          ? (receiptDoc["payload"] as Record<string, unknown>)
          : null;
      if (payload && typeof payload["result_hash"] === "string") {
        const orig = payload["result_hash"];
        payload["result_hash"] = (orig.startsWith("0") ? "f" : "0") + orig.slice(1);
      } else if (receiptDoc && typeof receiptDoc["signature"] === "string") {
        const origSig = receiptDoc["signature"];
        receiptDoc["signature"] =
          (origSig.startsWith("0") ? "f" : "0") + origSig.slice(1);
      }
    }
    const report = await verifyFrogletChain(chainCopy);
    setVerifyReport(report);
  };

  const submitChatPrompt = async (rawMsg: string): Promise<void> => {
    const msg = rawMsg.trim();
    if (!session || !msg || chatBusy) return;
    setChatInput("");
    setChatBusy(true);
    setStreamingText("");
    setErrorBanner(null);

    try {
      await streamAgentChatTurn(session.session_id, msg, (ev) => {
        if (ev.type === "text_delta" && typeof ev.data === "string") {
          setStreamingText((prev) => prev + ev.data);
        } else if (ev.type === "run_prepared" && ev.data) {
          handleQuotePrepared(ev.data as RunQuote);
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

  const handleScaffoldModule = async (payload: {
    template: string;
    plugin_id: string;
    display_name: string;
    include_vite_ui: boolean;
  }): Promise<void> => {
    try {
      setErrorBanner(null);
      const res = await scaffoldPlugin(payload);
      setScaffoldOutput(res);
      const updatedPlugins = await fetchPlugins();
      setPlugins(updatedPlugins);
    } catch (err: unknown) {
      setErrorBanner(err instanceof Error ? err.message : String(err));
    }
  };

  // Window manager callbacks
  const handleFocusWindow = useCallback((winId: string) => {
    setZCounter((prevZ) => {
      const nextZ = prevZ + 1;
      setWindows((prev) =>
        prev.map((w) =>
          w.id === winId ? { ...w, minimized: false, z: nextZ } : w,
        ),
      );
      return nextZ;
    });
  }, []);

  const handleMoveResizeWindow = useCallback(
    (
      winId: string,
      patch: Partial<Pick<ModuleWindowState, "x" | "y" | "w" | "h">>,
    ) => {
      setWindows((prev) =>
        prev.map((w) => (w.id === winId ? { ...w, ...patch } : w)),
      );
    },
    [],
  );

  const handleToggleMinimize = useCallback((winId: string) => {
    setZCounter((prevZ) => {
      const nextZ = prevZ + 1;
      setWindows((prev) =>
        prev.map((w) =>
          w.id === winId
            ? { ...w, minimized: !w.minimized, z: w.minimized ? nextZ : w.z }
            : w,
        ),
      );
      return nextZ;
    });
  }, []);

  const handleToggleMaximize = useCallback((winId: string) => {
    setWindows((prev) =>
      prev.map((w) =>
        w.id === winId ? { ...w, maximized: !w.maximized, minimized: false } : w,
      ),
    );
  }, []);

  const handleCloseWindow = useCallback((winId: string) => {
    setWindows((prev) => prev.filter((w) => w.id !== winId));
  }, []);

  const handleChangeWindowTab = useCallback((winId: string, tab: WindowTab) => {
    setWindows((prev) =>
      prev.map((w) => (w.id === winId ? { ...w, activeTab: tab } : w)),
    );
  }, []);

  const handleUpdateWindowParam = useCallback(
    (winId: string, key: string, value: unknown) => {
      setWindows((prev) =>
        prev.map((w) =>
          w.id === winId
            ? { ...w, params: { ...w.params, [key]: value } }
            : w,
        ),
      );
    },
    [],
  );

  const filteredPlugins = useMemo(() => {
    const q = moduleSearch.trim().toLowerCase();
    if (!q) return plugins;
    return plugins.filter(
      (p) =>
        p.manifest.name.toLowerCase().includes(q) ||
        p.manifest.description.toLowerCase().includes(q) ||
        p.manifest.category.toLowerCase().includes(q),
    );
  }, [plugins, moduleSearch]);

  const isEmptyChat =
    sessionEvents.length === 0 && !streamingText && !chatBusy && runs.length === 0;

  const pageTitle =
    activePage === "plugins"
      ? "Modules"
      : activePage === "catalogs"
        ? "Data"
        : session?.title ?? "Chat";

  const renderComposer = () => (
    <form
      className="bc-composer"
      onSubmit={(e) => {
        e.preventDefault();
        void submitChatPrompt(chatInput);
      }}
    >
      <textarea
        className="bc-composer-textarea"
        rows={2}
        placeholder="Ask about HLA cohorts, peptide motifs, or launch an analysis module..."
        value={chatInput}
        onChange={(e) => setChatInput(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" && !e.shiftKey) {
            e.preventDefault();
            void submitChatPrompt(chatInput);
          }
        }}
      />
      <div className="bc-composer-bar">
        <button
          type="button"
          className="bc-btn bc-btn-ghost"
          onClick={() => navigateToPage("plugins")}
        >
          <IconModules size={14} />
          <span>Browse Modules ({plugins.length})</span>
        </button>
        <button
          type="submit"
          className="bc-send-btn"
          aria-label="Send message"
          disabled={chatBusy || !chatInput.trim()}
        >
          {chatBusy ? <RunningCompass size={16} /> : <IconSend size={15} />}
        </button>
      </div>
    </form>
  );

  return (
    <div className="bc-shell">
      {/* ==================== LEFT SIDEBAR ==================== */}
      <aside className="bc-sidebar" data-collapsed={String(sidebarCollapsed)}>
        <div className="bc-sidebar-top">
          <button
            type="button"
            className="bc-brand-btn"
            onClick={() => navigateToPage("workbench")}
            title="BioCompass Home"
          >
            {sidebarCollapsed ? <CompassMark size={24} /> : <BrandWordmark size={22} />}
          </button>

          {!sidebarCollapsed && (
            <button
              type="button"
              className="bc-icon-btn"
              aria-label="Collapse sidebar"
              onClick={() => setSidebarCollapsed(true)}
            >
              <IconSidebar size={16} />
            </button>
          )}
        </div>

        <button
          type="button"
          className="bc-new-chat-btn"
          onClick={() => void handleNewSession()}
          title="New Chat"
        >
          <IconPlus size={15} />
          {!sidebarCollapsed && <span>New Chat</span>}
        </button>

        <nav className="bc-nav-list" aria-label="Workspace navigation">
          <button
            type="button"
            data-testid="nav-workbench"
            className="bc-nav-item"
            data-active={String(activePage === "workbench")}
            onClick={() => navigateToPage("workbench")}
            title="Chat"
          >
            <span className="bc-nav-item-left">
              <IconChat size={16} />
              {!sidebarCollapsed && <span>Chat</span>}
            </span>
          </button>

          <button
            type="button"
            data-testid="nav-plugins"
            className="bc-nav-item"
            data-active={String(activePage === "plugins")}
            onClick={() => navigateToPage("plugins")}
            title="Modules"
          >
            <span className="bc-nav-item-left">
              <IconModules size={16} />
              {!sidebarCollapsed && <span>Modules</span>}
            </span>
            {!sidebarCollapsed && (
              <span className="bc-caption">{plugins.length}</span>
            )}
          </button>

          <button
            type="button"
            data-testid="nav-catalogs"
            className="bc-nav-item"
            data-active={String(activePage === "catalogs")}
            onClick={() => navigateToPage("catalogs")}
            title="Data"
          >
            <span className="bc-nav-item-left">
              <IconDatabase size={16} />
              {!sidebarCollapsed && <span>Data</span>}
            </span>
          </button>
        </nav>

        {!sidebarCollapsed && (
          <>
            <div className="bc-section-label">Recent Chats</div>
            <div className="bc-sessions-scroll">
              {sessions.map((s) => (
                <button
                  key={s.session_id}
                  type="button"
                  className="bc-session-btn"
                  data-active={String(
                    activePage === "workbench" && session?.session_id === s.session_id,
                  )}
                  onClick={() => void handleSelectSession(s)}
                >
                  {s.title}
                </button>
              ))}
            </div>
          </>
        )}

        <div className="bc-sidebar-footer">
          <div
            className="bc-health-row"
            data-testid="health-pill"
            title={
              health
                ? `Engine: ${health.container_engine} · Node: ${health.node_id}`
                : "Connecting..."
            }
          >
            <span className="bc-status-dot" />
            {!sidebarCollapsed && (
              <span className="mono">
                {health ? `Local · ${health.container_engine}` : "Connecting..."}
              </span>
            )}
          </div>

          <button
            type="button"
            data-testid="nav-settings"
            className="bc-nav-item"
            onClick={() => openSettings("general")}
            title="Settings (⌘,)"
          >
            <span className="bc-nav-item-left">
              <IconSettings size={16} />
              {!sidebarCollapsed && <span>Settings</span>}
            </span>
          </button>
        </div>
      </aside>

      {/* ==================== MAIN WORKSPACE ==================== */}
      <main className="bc-main">
        {/* Slim Logical Top Header */}
        <header className="bc-topbar">
          <div className="bc-topbar-left">
            {sidebarCollapsed && (
              <button
                type="button"
                className="bc-icon-btn"
                aria-label="Expand sidebar"
                onClick={() => setSidebarCollapsed(false)}
              >
                <IconSidebar size={16} />
              </button>
            )}
            <div className="bc-breadcrumb">
              <span className="bc-breadcrumb-root">Workspace</span>
              <span className="bc-breadcrumb-sep">/</span>
              <span className="bc-breadcrumb-current">{pageTitle}</span>
            </div>
          </div>

          <div className="bc-topbar-right">
            {grants.length > 0 && (
              <div className="bc-grant-pill-wrap" title="Active Data Access Grant">
                <IconDatabase size={13} />
                <select
                  aria-label="Active Data Grant"
                  data-testid="grant-select"
                  className="bc-grant-select"
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
            )}

            <button
              type="button"
              className="bc-credit-pill mono"
              data-testid="act-balance"
              onClick={() => openSettings("compute")}
              title="View ACT Credit Ledger"
            >
              {ledger?.available_display ?? "0.00 ACT"}
            </button>

            <button
              type="button"
              className="bc-icon-btn"
              aria-label="Open Settings"
              onClick={() => openSettings("general")}
              title="Settings (⌘,)"
            >
              <IconSettings size={16} />
            </button>
          </div>
        </header>

        {errorBanner && (
          <div className="bc-error-banner" role="alert">
            <span>{errorBanner}</span>
            <button
              type="button"
              className="bc-icon-btn"
              aria-label="Dismiss error"
              onClick={() => setErrorBanner(null)}
            >
              <IconClose size={14} />
            </button>
          </div>
        )}

        {/* ------------------ PAGE 1: CHAT ------------------ */}
        {activePage === "workbench" && (
          <div className="bc-chat-layout">
            <div className="bc-chat-scroll">
              {isEmptyChat ? (
                <div className="bc-hero">
                  <div
                    className="bc-hero-heading"
                    onMouseEnter={() => setHeroHovering(true)}
                    onMouseLeave={() => setHeroHovering(false)}
                  >
                    <HeroCompass hovering={heroHovering} />
                    <h1 className="bc-hero-title">What would you like to explore?</h1>
                    <p className="bc-hero-sub">
                      Query governed HLA immunopeptidomics cohorts or open a computational
                      module in a floating window.
                    </p>
                  </div>

                  <div className="bc-composer-wrap">{renderComposer()}</div>

                  {plugins.length > 0 && (
                    <div className="bc-starter-section">
                      <div className="bc-section-label" style={{ paddingLeft: 2 }}>
                        Quick-Launch Modules
                      </div>
                      <div className="bc-starter-grid">
                        {plugins.map((p) => (
                          <button
                            key={p.manifest.id}
                            type="button"
                            data-testid={`quick-prepare-btn-${p.manifest.id}`}
                            className="bc-starter-card"
                            onClick={() => void handlePreparePluginRun(p)}
                          >
                            <div>
                              <div className="bc-starter-title">{p.manifest.name}</div>
                              <div className="bc-starter-desc">
                                {p.has_custom_ui
                                  ? "Interactive window + cohort hot-spots"
                                  : "Containerized analysis · charts & CSV"}
                              </div>
                            </div>
                            <IconWindowOpen size={16} />
                          </button>
                        ))}
                      </div>
                    </div>
                  )}

                  <div className="bc-prompt-pills">
                    {SUGGESTED_PROMPTS.map((prompt) => (
                      <button
                        key={prompt}
                        type="button"
                        className="bc-prompt-pill"
                        onClick={() => void submitChatPrompt(prompt)}
                      >
                        {prompt}
                      </button>
                    ))}
                  </div>
                </div>
              ) : (
                <div className="bc-chat-column">
                  {sessionEvents.map((ev) =>
                    ev.kind === "user" ? (
                      <div key={ev.event_id} className="bc-msg-user">
                        {ev.content}
                      </div>
                    ) : (
                      <div key={ev.event_id} className="bc-msg-assistant">
                        <span className="bc-msg-avatar">
                          <CompassMark size={18} />
                        </span>
                        <div className="bc-msg-body">{ev.content}</div>
                      </div>
                    ),
                  )}

                  {(streamingText || chatBusy) && (
                    <div className="bc-msg-assistant">
                      <span className="bc-msg-avatar">
                        <RunningCompass size={18} />
                      </span>
                      <div className="bc-msg-body">{streamingText || "Thinking..."}</div>
                    </div>
                  )}

                  {runs.map((r) => {
                    const runPlugin = plugins.find((p) => p.manifest.id === r.plugin_id);
                    return (
                      <div key={r.run_id} className="bc-run-summary-card">
                        <div>
                          <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
                            <strong>{runPlugin?.manifest.name ?? r.plugin_id}</strong>
                            <span className="bc-badge bc-badge-success">
                              {r.receipt_verified ? "Verified" : r.status}
                            </span>
                          </div>
                          <div className="bc-caption mono" style={{ marginTop: 2 }}>
                            {r.run_id.slice(0, 16)} ·{" "}
                            {formatMilliAct(r.quote.estimated_cost_act)} ·{" "}
                            {r.artifacts.length} output file(s)
                          </div>
                        </div>
                        {runPlugin && (
                          <button
                            type="button"
                            className="bc-btn bc-btn-secondary"
                            onClick={() => openOrFocusModuleWindow(runPlugin, "results")}
                          >
                            <IconWindowOpen size={14} />
                            <span>Open Results</span>
                          </button>
                        )}
                      </div>
                    );
                  })}
                </div>
              )}
            </div>

            {!isEmptyChat && (
              <div className="bc-composer-wrap">{renderComposer()}</div>
            )}
          </div>
        )}

        {/* ------------------ PAGE 2: MODULES (FLOATING WINDOW LAUNCHER) ------------------ */}
        {activePage === "plugins" && (
          <div className="bc-page-scroll">
            <div className="bc-page-container">
              <div className="bc-page-header">
                <div>
                  <h1 className="bc-page-title">Modules</h1>
                  <p className="bc-page-subtitle">
                    Click any module to open it as a draggable floating window in your
                    workspace.
                  </p>
                </div>

                <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
                  <input
                    type="search"
                    className="bc-input"
                    placeholder="Filter modules..."
                    value={moduleSearch}
                    onChange={(e) => setModuleSearch(e.target.value)}
                  />
                  <button
                    type="button"
                    className="bc-btn bc-btn-secondary"
                    onClick={() => openSettings("developer")}
                  >
                    <IconPlus size={14} />
                    <span>New Module</span>
                  </button>
                </div>
              </div>

              <div className="bc-modules-grid">
                {filteredPlugins.map((p) => (
                  <article
                    key={p.manifest.id}
                    data-testid={`plugin-card-${p.manifest.id}`}
                    className="bc-module-card"
                    onClick={() =>
                      openOrFocusModuleWindow(p, p.has_custom_ui ? "ui" : "run")
                    }
                  >
                    <div>
                      <div className="bc-module-card-top">
                        <span className="bc-badge bc-badge-neutral">
                          {p.manifest.category}
                        </span>
                        {p.has_custom_ui && (
                          <span className="bc-badge bc-badge-accent">Interactive App</span>
                        )}
                      </div>
                      <h2 className="bc-module-title">{p.manifest.name}</h2>
                      <p className="bc-module-desc">{p.manifest.description}</p>
                    </div>

                    <div className="bc-module-card-footer">
                      <span className="bc-caption mono">
                        {formatMilliAct(p.manifest.price_milli_act)} / run
                      </span>
                      <span
                        style={{
                          display: "inline-flex",
                          alignItems: "center",
                          gap: 6,
                          fontSize: 13,
                          fontWeight: 600,
                          color: "var(--bc-accent)",
                        }}
                      >
                        <span>Open Window</span>
                        <IconWindowOpen size={14} />
                      </span>
                    </div>
                  </article>
                ))}
              </div>
            </div>
          </div>
        )}

        {/* ------------------ PAGE 3: DATA CATALOGS ------------------ */}
        {activePage === "catalogs" && (
          <div className="bc-page-scroll">
            <div className="bc-page-container">
              <div className="bc-page-header">
                <div>
                  <h1 className="bc-page-title">Data</h1>
                  <p className="bc-page-subtitle">
                    Cohort preview for <span className="mono">{defaultCatalogId}</span>{" "}
                    governed by your active access grant.
                  </p>
                </div>

                <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
                  <label className="bc-caption" style={{ display: "flex", alignItems: "center", gap: 6 }}>
                    <span>Max IC50 (nM)</span>
                    <input
                      type="number"
                      className="bc-input"
                      style={{ width: 88 }}
                      value={catalogMaxIc50}
                      onChange={(e) => setCatalogMaxIc50(Number(e.target.value) || 500)}
                    />
                  </label>
                  {catalogResult && (
                    <span
                      data-testid="catalog-mask-indicator"
                      className={`bc-badge ${
                        catalogResult.sequence_masked
                          ? "bc-badge-warn"
                          : "bc-badge-success"
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
                  <div className="bc-card">
                    <div className="bc-card-header">
                      <span className="bc-card-title">Ligand Cohort Preview</span>
                      <span className="bc-caption">
                        Showing {catalogResult.rows.length} of{" "}
                        {catalogResult.total_in_scope} in-scope ligands
                      </span>
                    </div>
                    <div className="table-scroll">
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

                  <div className="bc-card">
                    <div className="bc-card-header">
                      <span className="bc-card-title">
                        Enforced DuckDB Governance View
                      </span>
                      <span className="bc-caption">
                        Compiled automatically from active DatasetGrant
                      </span>
                    </div>
                    <pre className="bc-code-pre" data-testid="catalog-sql-view">
                      {catalogResult.compiled_view_sql}
                    </pre>
                  </div>
                </>
              )}
            </div>
          </div>
        )}
      </main>

      {/* ==================== FLOATING MODULE WINDOWS + TRAY ==================== */}
      <FloatingWindowsLayer
        windows={windows}
        plugins={plugins}
        runs={runs}
        activeGrant={activeGrant}
        defaultCatalogId={defaultCatalogId}
        pendingQuote={pendingQuote}
        isConfirmingRun={isConfirmingRun}
        liveLogs={liveLogs}
        verifyReport={verifyReport}
        onFocusWindow={handleFocusWindow}
        onMoveResizeWindow={handleMoveResizeWindow}
        onToggleMinimize={handleToggleMinimize}
        onToggleMaximize={handleToggleMaximize}
        onCloseWindow={handleCloseWindow}
        onChangeTab={handleChangeWindowTab}
        onUpdateParam={handleUpdateWindowParam}
        onPreparePluginRun={(plugin, params) => void handlePreparePluginRun(plugin, params)}
        onQuotePrepared={handleQuotePrepared}
        onConfirmPendingQuote={() => void handleConfirmPendingRun()}
        onCancelPendingQuote={() => setPendingQuote(null)}
        onReverifyChain={(run, tamper) => void handleReverifyReceipt(run, tamper)}
      />

      {/* ==================== SETTINGS MODAL ==================== */}
      <SettingsModal
        open={settingsOpen}
        initialSection={settingsSection}
        darkTheme={darkTheme}
        onSetDarkTheme={setDarkTheme}
        health={health}
        grants={grants}
        activeGrantId={activeGrant?.grant_id ?? ""}
        onSelectGrant={(gid) => void handleGrantChange(gid)}
        ledger={ledger}
        onScaffoldModule={handleScaffoldModule}
        scaffoldOutput={scaffoldOutput}
        onClose={() => setSettingsOpen(false)}
      />
    </div>
  );
}
