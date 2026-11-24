import React, { useEffect, useRef, useState } from "react";
import {
  IconAbout,
  IconApi,
  IconApp,
  IconClose,
  IconMaximize,
  IconMinimize,
  IconModules,
  IconRestore,
  IconRuns,
  IconSplitView,
  RunningCompass,
} from "./Icons";
import {
  ModuleAboutTab,
  ModuleApiTab,
  ModuleRunsTab,
} from "./ModuleTabs";
import { SandboxedPluginFrame } from "./SandboxedPluginFrame";
import { resolveModuleCardActions } from "../moduleListPolicy";
import type {
  DatasetGrant,
  RunVerification,
  PluginSummary,
  RunQuote,
  RunRecord,
} from "../types";

export type WindowTab = "ui" | "run" | "results" | "about";

export interface ModuleWindowState {
  id: string;
  pluginId: string;
  plugin?: PluginSummary;
  x: number;
  y: number;
  w: number;
  h: number;
  z: number;
  minimized: boolean;
  maximized: boolean;
  docked: boolean;
  activeTab: WindowTab;
  requestedRunId?: string;
  params: Record<string, unknown>;
}

export function FloatingWindowsLayer({
  windows,
  plugins,
  runs,
  activeGrant,
  defaultCatalogId,
  releaseVersion,
  catalogFacets = {},
  sessionId,
  theme = "light",
  pendingQuote,
  pendingQuoteParams = null,
  isConfirmingRun,
  isLocalMode = false,
  liveLogs,
  verifyReport,
  onFocusWindow,
  onMoveResizeWindow,
  onToggleMinimize,
  onToggleMaximize,
  onToggleDock,
  onCloseWindow,
  onChangeTab,
  onUpdateParam,
  onPreparePluginRun,
  onRunApiTest,
  onQuoteConfirmed,
  onQuotePrepared,
  onSendToChat,
  onSyncDraftParams,
  onUpdateRun,
  onReverifyChain,
}: {
  windows: ModuleWindowState[];
  plugins: PluginSummary[];
  runs: RunRecord[];
  activeGrant: DatasetGrant | null;
  defaultCatalogId: string;
  releaseVersion?: number;
  catalogFacets?: Record<string, string[]>;
  sessionId?: string;
  theme?: "light" | "dark";
  pendingQuote: RunQuote | null;
  pendingQuoteParams?: Record<string, unknown> | null;
  isConfirmingRun: boolean;
  isLocalMode?: boolean;
  liveLogs: string[];
  verifyReport: RunVerification | null;
  onFocusWindow: (winId: string) => void;
  onMoveResizeWindow: (
    winId: string,
    patch: Partial<Pick<ModuleWindowState, "x" | "y" | "w" | "h">>,
  ) => void;
  onToggleMinimize: (winId: string) => void;
  onToggleMaximize: (winId: string) => void;
  onToggleDock: (winId: string) => void;
  onCloseWindow: (winId: string) => void;
  onChangeTab: (winId: string, tab: WindowTab, requestedRunId?: string) => void;
  onUpdateParam: (winId: string, key: string, value: unknown) => void;
  onPreparePluginRun: (plugin: PluginSummary, params: Record<string, unknown>) => void;
  onRunApiTest?: (
    plugin: PluginSummary,
    params: Record<string, unknown>,
    offerId?: string,
  ) => Promise<RunRecord | null>;
  onQuoteConfirmed?: (completed: RunRecord) => Promise<void> | void;
  onQuotePrepared: (quote: RunQuote, params?: Record<string, unknown>) => void;
  onSendToChat?: (
    pluginId: string,
    message: string,
    context?: Record<string, unknown>,
  ) => void;
  onSyncDraftParams?: (pluginId: string, params: Record<string, unknown>) => void;
  onUpdateRun: (run: RunRecord) => void;
  onReverifyChain: (run: RunRecord, tamperOneByte: boolean) => void;
}) {
  const floatingWindows = windows.filter((w) => !w.minimized && !w.docked);
  const visibleWindows = windows.filter((w) => !w.minimized);
  const topWindow =
    visibleWindows.length > 0
      ? visibleWindows.reduce((best, cur) => (cur.z >= best.z ? cur : best))
      : null;
  const topWindowId = topWindow?.id ?? null;
  const topFloatingWindow =
    floatingWindows.length > 0
      ? floatingWindows.reduce((best, cur) => (cur.z >= best.z ? cur : best))
      : null;
  const isTopMaximized = Boolean(topFloatingWindow?.maximized);
  const showTray = windows.some((w) => w.minimized || !w.docked);

  const [trayRevealed, setTrayRevealed] = useState(false);
  const hideTrayTimerRef = useRef<number | null>(null);

  const clearHideTrayTimer = () => {
    if (hideTrayTimerRef.current !== null) {
      window.clearTimeout(hideTrayTimerRef.current);
      hideTrayTimerRef.current = null;
    }
  };

  const revealTray = () => {
    clearHideTrayTimer();
    setTrayRevealed(true);
  };

  const scheduleHideTray = (delayMs = 240) => {
    clearHideTrayTimer();
    hideTrayTimerRef.current = window.setTimeout(() => {
      setTrayRevealed(false);
      hideTrayTimerRef.current = null;
    }, delayMs);
  };

  useEffect(() => {
    clearHideTrayTimer();
    setTrayRevealed(false);
  }, [isTopMaximized]);

  useEffect(() => () => clearHideTrayTimer(), []);

  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape" && topWindowId) {
        // Only close if no modal dialog is open
        if (!document.querySelector('[role="dialog"]')) {
          onCloseWindow(topWindowId);
        }
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [topWindowId, onCloseWindow]);

  return (
    <>
      {floatingWindows.map((win) => {
        const plugin = win.plugin ?? plugins.find((p) => p.manifest.id === win.pluginId);
        if (!plugin) return null;
        const isFocused = win.id === topWindowId;
        const pluginRuns = runs.filter((r) => r.plugin_id === plugin.manifest.id);
        const hasPendingQuote = pendingQuote?.plugin_id === plugin.manifest.id && pendingQuote.plugin_version === plugin.manifest.version;

        return (
          <ModuleFloatingWindow
            key={`${win.id}@${plugin.manifest.version}@${plugin.marketplace_offer_id ?? ""}`}
            win={win}
            plugin={plugin}
            isFocused={isFocused}
            runs={pluginRuns}
            runCount={pluginRuns.length}
            activeGrant={activeGrant}
            defaultCatalogId={defaultCatalogId}
            releaseVersion={releaseVersion}
            catalogFacets={catalogFacets}
            sessionId={sessionId}
            theme={theme}
            pendingQuote={hasPendingQuote ? pendingQuote : null}
            pendingQuoteParams={hasPendingQuote ? pendingQuoteParams : null}
            isConfirmingRun={isConfirmingRun && hasPendingQuote}
            isLocalMode={isLocalMode}
            liveLogs={liveLogs}
            verifyReport={isFocused ? verifyReport : null}
            onFocus={() => onFocusWindow(win.id)}
            onMoveResize={(patch) => onMoveResizeWindow(win.id, patch)}
            onMinimize={() => onToggleMinimize(win.id)}
            onMaximize={() => onToggleMaximize(win.id)}
            onDock={() => onToggleDock(win.id)}
            onClose={() => onCloseWindow(win.id)}
            onChangeTab={(tab, runId) => onChangeTab(win.id, tab, runId)}
            onUpdateParam={(key, val) => onUpdateParam(win.id, key, val)}
            onPrepareRun={() => onPreparePluginRun(plugin, win.params)}
            onRunApiTest={
              onRunApiTest
                ? (params, offerId) => onRunApiTest(plugin, params, offerId)
                : undefined
            }
            onQuoteConfirmed={onQuoteConfirmed}
            onQuotePrepared={onQuotePrepared}
            onSendToChat={onSendToChat}
            onSyncDraftParams={onSyncDraftParams}
            onUpdateRun={onUpdateRun}
            onReverifyChain={onReverifyChain}
          />
        );
      })}

      {/* Bottom centered HLA-Compass-style floating window tray */}
      {showTray && isTopMaximized && (
        <div
          className="bc-window-tray-hotzone"
          aria-hidden="true"
          onMouseEnter={revealTray}
          onMouseLeave={() => scheduleHideTray(400)}
        />
      )}
      {showTray && (
        <div
          className="bc-window-tray"
          role="toolbar"
          aria-label="Open module windows"
          data-autohide={String(isTopMaximized)}
          data-revealed={String(!isTopMaximized || trayRevealed)}
          onMouseEnter={isTopMaximized ? revealTray : undefined}
          onMouseLeave={isTopMaximized ? () => scheduleHideTray(220) : undefined}
        >
          {windows.map((win) => {
            const plugin = win.plugin ?? plugins.find((p) => p.manifest.id === win.pluginId);
            const isTop = !win.minimized && win.id === topWindowId;
            return (
              <div
                key={win.id}
                className="bc-tray-pill"
                data-active={String(isTop)}
                data-minimized={String(win.minimized)}
              >
                <button
                  type="button"
                  className="bc-tray-pill-main"
                  onClick={() => {
                    if (win.minimized) {
                      onToggleMinimize(win.id);
                    } else if (isTop) {
                      onToggleMinimize(win.id);
                    } else {
                      onFocusWindow(win.id);
                    }
                  }}
                >
                  <span className="bc-tray-dot" />
                  <span className="bc-tray-title">
                    {plugin?.manifest.name ?? win.pluginId}
                  </span>
                </button>
                <button
                  type="button"
                  className="bc-tray-close"
                  aria-label={`Close ${plugin?.manifest.name ?? win.pluginId}`}
                  onClick={() => onCloseWindow(win.id)}
                >
                  <IconClose size={11} />
                </button>
              </div>
            );
          })}
        </div>
      )}
    </>
  );
}

export function ModuleFloatingWindow({
  win,
  plugin,
  isFocused,
  runs: moduleRuns,
  runCount,
  activeGrant,
  defaultCatalogId,
  releaseVersion,
  catalogFacets = {},
  sessionId,
  theme = "light",
  pendingQuote,
  pendingQuoteParams = null,
  isConfirmingRun,
  isLocalMode = false,
  liveLogs,
  verifyReport,
  onFocus,
  onMoveResize,
  onMinimize,
  onMaximize,
  onDock,
  onClose,
  onChangeTab,
  onUpdateParam: _onUpdateParam,
  onPrepareRun: _onPrepareRun,
  onRunApiTest,
  onQuoteConfirmed,
  onQuotePrepared,
  onSendToChat,
  onSyncDraftParams,
  onUpdateRun,
  onReverifyChain,
}: {
  win: ModuleWindowState;
  plugin: PluginSummary;
  isFocused: boolean;
  runs: RunRecord[];
  runCount: number;
  activeGrant: DatasetGrant | null;
  defaultCatalogId: string;
  releaseVersion?: number;
  catalogFacets?: Record<string, string[]>;
  sessionId?: string;
  theme?: "light" | "dark";
  pendingQuote: RunQuote | null;
  pendingQuoteParams?: Record<string, unknown> | null;
  isConfirmingRun: boolean;
  isLocalMode?: boolean;
  liveLogs: string[];
  verifyReport: RunVerification | null;
  onFocus: () => void;
  onMoveResize: (patch: Partial<Pick<ModuleWindowState, "x" | "y" | "w" | "h">>) => void;
  onMinimize: () => void;
  onMaximize: () => void;
  onDock: () => void;
  onClose: () => void;
  onChangeTab: (tab: WindowTab, requestedRunId?: string) => void;
  onUpdateParam: (key: string, value: unknown) => void;
  onPrepareRun: () => void;
  onRunApiTest?: (
    params: Record<string, unknown>,
    offerId?: string,
  ) => Promise<RunRecord | null>;
  onQuoteConfirmed?: (completed: RunRecord) => Promise<void> | void;
  onQuotePrepared: (quote: RunQuote, params?: Record<string, unknown>) => void;
  onSendToChat?: (
    pluginId: string,
    message: string,
    context?: Record<string, unknown>,
  ) => void;
  onSyncDraftParams?: (pluginId: string, params: Record<string, unknown>) => void;
  onUpdateRun: (run: RunRecord) => void;
  onReverifyChain: (run: RunRecord, tamperOneByte: boolean) => void;
}) {
  const dragState = useRef<{
    startX: number;
    startY: number;
    origX: number;
    origY: number;
  } | null>(null);
  const resizeState = useRef<{
    startX: number;
    startY: number;
    origW: number;
    origH: number;
  } | null>(null);

  const handleHeaderPointerDown = (e: React.PointerEvent<HTMLDivElement>) => {
    if (win.maximized || win.docked) return;
    if ((e.target as HTMLElement).closest("button")) return;
    onFocus();
    e.currentTarget.setPointerCapture(e.pointerId);
    dragState.current = {
      startX: e.clientX,
      startY: e.clientY,
      origX: win.x,
      origY: win.y,
    };
  };

  const handleHeaderPointerMove = (e: React.PointerEvent<HTMLDivElement>) => {
    if (!dragState.current) return;
    const dx = e.clientX - dragState.current.startX;
    const dy = e.clientY - dragState.current.startY;
    const maxX = Math.max(80, window.innerWidth - 160);
    const maxY = Math.max(80, window.innerHeight - 90);
    onMoveResize({
      x: Math.min(maxX, Math.max(16, dragState.current.origX + dx)),
      y: Math.min(maxY, Math.max(54, dragState.current.origY + dy)),
    });
  };

  const handleHeaderPointerUp = (e: React.PointerEvent<HTMLDivElement>) => {
    if (dragState.current) {
      dragState.current = null;
      if (e.currentTarget.hasPointerCapture(e.pointerId)) {
        e.currentTarget.releasePointerCapture(e.pointerId);
      }
    }
  };

  const handleResizePointerDown = (e: React.PointerEvent<HTMLDivElement>) => {
    e.stopPropagation();
    onFocus();
    e.currentTarget.setPointerCapture(e.pointerId);
    resizeState.current = {
      startX: e.clientX,
      startY: e.clientY,
      origW: win.w,
      origH: win.h,
    };
  };

  const handleResizePointerMove = (e: React.PointerEvent<HTMLDivElement>) => {
    if (!resizeState.current) return;
    const dx = e.clientX - resizeState.current.startX;
    const dy = e.clientY - resizeState.current.startY;
    const maxW = Math.max(520, window.innerWidth - win.x - 16);
    const maxH = Math.max(380, window.innerHeight - win.y - 24);
    onMoveResize({
      w: Math.min(maxW, Math.max(500, resizeState.current.origW + dx)),
      h: Math.min(maxH, Math.max(380, resizeState.current.origH + dy)),
    });
  };

  const handleResizePointerUp = (e: React.PointerEvent<HTMLDivElement>) => {
    if (resizeState.current) {
      resizeState.current = null;
      if (e.currentTarget.hasPointerCapture(e.pointerId)) {
        e.currentTarget.releasePointerCapture(e.pointerId);
      }
    }
  };

  const versionRuns = moduleRuns.filter((run) => run.plugin_version === plugin.manifest.version);
  const latestRun = versionRuns[0];
  const terminalProblem =
    latestRun?.status === "failed" || latestRun?.status === "canceled" ? latestRun : null;
  const frameRun = terminalProblem ?? versionRuns.find((run) => run.status === "succeeded") ?? null;

  const windowStyle: React.CSSProperties | undefined = win.docked
    ? undefined
    : win.maximized
      ? {
          top: 0,
          left: 0,
          right: 0,
          bottom: 0,
          width: "100vw",
          height: "100dvh",
          zIndex: 200 + win.z,
        }
      : {
          top: win.y,
          left: win.x,
          width: win.w,
          height: win.h,
          zIndex: 200 + win.z,
        };

  return (
    <section
      className="bc-window"
      data-focused={String(isFocused)}
      data-maximized={String(win.maximized)}
      data-docked={String(Boolean(win.docked))}
      data-testid={`module-window-${plugin.manifest.id}`}
      style={windowStyle}
      onMouseDown={onFocus}
      aria-label={plugin.manifest.name}
    >
      {/* Top Window Chrome + Autohiding Centered Module Navigation Tabs */}
      <div className="bc-window-chrome-top">
        <div
          className="bc-window-titlebar"
          onPointerDown={handleHeaderPointerDown}
          onPointerMove={handleHeaderPointerMove}
          onPointerUp={handleHeaderPointerUp}
          onDoubleClick={win.docked ? undefined : onMaximize}
        >
          <div className="bc-window-title-left">
            <span className="bc-window-icon">
              <IconModules size={14} />
            </span>
            <span className="bc-window-title">{plugin.manifest.name}</span>
            <span className="bc-window-version mono">v{plugin.manifest.version}</span>
          </div>

          <span className="bc-window-tab-indicator" aria-hidden="true" />

          <div className="bc-window-controls">
            <button
              type="button"
              className="bc-win-ctrl"
              data-active={String(Boolean(win.docked))}
              aria-label={win.docked ? "Undock from chat" : "Split screen with chat"}
              aria-pressed={Boolean(win.docked)}
              title={
                win.docked
                  ? "Undock to floating window"
                  : "Split screen next to chat (Canvas)"
              }
              onClick={onDock}
            >
              <IconSplitView size={13} />
            </button>
            <button
              type="button"
              className="bc-win-ctrl"
              aria-label="Minimize window"
              title="Minimize to tray"
              onClick={onMinimize}
            >
              <IconMinimize size={13} />
            </button>
            <button
              type="button"
              className="bc-win-ctrl"
              aria-label={win.maximized ? "Restore window" : "Maximize window"}
              title={win.maximized ? "Restore size" : "Maximize"}
              onClick={onMaximize}
            >
              {win.maximized ? <IconRestore size={13} /> : <IconMaximize size={12} />}
            </button>
            <button
              type="button"
              className="bc-win-ctrl bc-win-ctrl-close"
              aria-label="Close window"
              title="Close (Esc)"
              onClick={onClose}
            >
              <IconClose size={13} />
            </button>
          </div>
        </div>

        <div className="bc-window-tabs-drawer">
          <div className="bc-window-tabs" role="tablist">
            {plugin.has_custom_ui && (
              <button
                type="button"
                role="tab"
                aria-selected={win.activeTab === "ui"}
                className="bc-window-tab"
                data-active={String(win.activeTab === "ui")}
                onClick={(e) => {
                  e.currentTarget.blur();
                  onChangeTab("ui");
                }}
              >
                <IconApp size={13} />
                <span>App</span>
              </button>
            )}
            <button
              type="button"
              role="tab"
              aria-selected={win.activeTab === "run"}
              className="bc-window-tab"
              data-active={String(win.activeTab === "run")}
              onClick={(e) => {
                e.currentTarget.blur();
                onChangeTab("run");
              }}
            >
              <IconApi size={13} />
              <span>API</span>
            </button>
            <button
              type="button"
              role="tab"
              aria-selected={win.activeTab === "results"}
              className="bc-window-tab"
              data-active={String(win.activeTab === "results")}
              onClick={(e) => {
                e.currentTarget.blur();
                onChangeTab("results");
              }}
            >
              <IconRuns size={13} />
              <span>Runs</span>
              {runCount > 0 && <span className="bc-tab-count">{runCount}</span>}
            </button>
            <button
              type="button"
              role="tab"
              aria-selected={win.activeTab === "about"}
              className="bc-window-tab"
              data-active={String(win.activeTab === "about")}
              onClick={(e) => {
                e.currentTarget.blur();
                onChangeTab("about");
              }}
            >
              <IconAbout size={13} />
              <span>About</span>
            </button>
          </div>
        </div>
      </div>

      {/* Window Body */}
      <div className="bc-window-body">
        {/* Keep custom UI iframe mounted so postMessage run updates stay seamless */}
        {plugin.has_custom_ui && (() => {
          const actions = resolveModuleCardActions(plugin, isLocalMode);
          return (
          <div
            className="bc-window-iframe-pane"
            style={{ display: win.activeTab === "ui" ? "flex" : "none" }}
          >
            {actions.isNetworkRunOnly && (
              <div
                data-testid="network-run-handoff-banner"
                style={{
                  padding: "8px 12px",
                  borderBottom: "1px solid var(--bc-border)",
                  background: "var(--bc-bg-subtle)",
                  display: "flex",
                  alignItems: "center",
                  justifyContent: "space-between",
                  gap: 10,
                  fontSize: 12.5,
                }}
              >
                <span>
                  <span className="bc-badge bc-badge-accent" style={{ marginRight: 8 }}>
                    {actions.runsAtProviderLabel ?? "Runs at provider"}
                  </span>
                  UI served from this workspace&apos;s verified cache. Execution runs on the provider&apos;s hub.
                </span>
                {actions.safeHandoffUrl && (
                  <a
                    className="bc-btn bc-btn-secondary bc-btn-sm"
                    href={actions.safeHandoffUrl}
                    target="_blank"
                    rel="noopener noreferrer"
                    data-testid="network-run-handoff-link"
                  >
                    Run at {plugin.provider_label ?? "provider"} ↗
                  </a>
                )}
              </div>
            )}
            {terminalProblem && (
              <div role="alert" className="bc-error-banner" data-testid="module-run-problem" data-run-id={terminalProblem.run_id}>
                <div className="bc-error-banner-body">
                  <strong>Run {terminalProblem.status}</strong>
                  {terminalProblem.error_message && <span className="bc-error-banner-message">{terminalProblem.error_message}</span>}
                  <span className="bc-error-banner-hint">{terminalProblem.run_id}</span>
                </div>
                <button type="button" className="bc-btn" onClick={() => onChangeTab("results", terminalProblem.run_id)}>View run</button>
              </div>
            )}
            <SandboxedPluginFrame
              key={plugin.manifest.version}
              plugin={plugin}
              activeGrant={activeGrant}
              defaultCatalogId={defaultCatalogId}
              releaseVersion={releaseVersion}
              catalogFacets={catalogFacets}
              sessionId={sessionId}
              latestPluginRun={frameRun}
              draftParams={win.params}
              pendingQuote={pendingQuote}
              pendingParams={pendingQuoteParams}
              isFocused={isFocused}
              theme={theme}
              onQuotePrepared={onQuotePrepared}
              onSendToChat={onSendToChat}
              onSyncDraftParams={onSyncDraftParams}
            />
          </div>
          );
        })()}

        {win.activeTab === "run" && (
          <ModuleApiTab
            key={plugin.manifest.version}
            plugin={plugin}
            initialParams={win.params}
            activeGrant={activeGrant}
            sessionId={sessionId}
            isFocused={isFocused}
            isConfirmingRun={isConfirmingRun}
            isLocalMode={isLocalMode}
            onSyncDraftParams={onSyncDraftParams}
            onRunApiTest={onRunApiTest}
            onQuoteConfirmed={onQuoteConfirmed}
            onInspectRun={(runId) => onChangeTab("results", runId)}
          />
        )}

        {win.activeTab === "results" && (
          <ModuleRunsTab
            plugin={plugin}
            runs={moduleRuns}
            requestedRunId={win.requestedRunId}
            isFocused={isFocused}
            isConfirmingRun={isConfirmingRun}
            isLocalMode={isLocalMode}
            liveLogs={liveLogs}
            verifyReport={verifyReport}
            onClearRequestedRunId={() => onChangeTab("results")}
            onUpdateRun={onUpdateRun}
            onReverifyChain={onReverifyChain}
          />
        )}

        {win.activeTab === "about" && (
          <ModuleAboutTab
            plugin={plugin}
            isFocused={isFocused}
            isLocalMode={isLocalMode}
          />
        )}
      </div>

      {/* Resize Corner Handle */}
      {!win.maximized && !win.docked && (
        <div
          className="bc-window-resize"
          onPointerDown={handleResizePointerDown}
          onPointerMove={handleResizePointerMove}
          onPointerUp={handleResizePointerUp}
          aria-hidden="true"
        />
      )}
    </section>
  );
}
