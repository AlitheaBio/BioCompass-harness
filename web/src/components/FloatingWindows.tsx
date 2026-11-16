import React, { useEffect, useRef, useState } from "react";
import {
  IconCheck,
  IconClose,
  IconMaximize,
  IconMinimize,
  IconModules,
  IconRestore,
  IconShield,
  RunningCompass,
} from "./Icons";
import { SandboxedPluginFrame } from "./SandboxedPluginFrame";
import { ScientificViews } from "./ScientificViews";
import {
  formatMilliAct,
  type DatasetGrant,
  type FrogletVerifyReport,
  type PluginSummary,
  type RunQuote,
  type RunRecord,
} from "../types";

export type WindowTab = "ui" | "run" | "results" | "about";

export interface ModuleWindowState {
  id: string;
  pluginId: string;
  x: number;
  y: number;
  w: number;
  h: number;
  z: number;
  minimized: boolean;
  maximized: boolean;
  activeTab: WindowTab;
  params: Record<string, unknown>;
}

interface SchemaPropertySpec {
  type?: string;
  title?: string;
  description?: string;
  default?: unknown;
  enum?: unknown[];
  minimum?: number;
  maximum?: number;
}

const FROGLET_STAGES = ["Descriptor", "Offer", "Quote", "Deal", "Receipt"] as const;

function humanizeKey(key: string): string {
  return key
    .replace(/_/g, " ")
    .replace(/\b\w/g, (ch) => ch.toUpperCase());
}

export function FloatingWindowsLayer({
  windows,
  plugins,
  runs,
  activeGrant,
  defaultCatalogId,
  pendingQuote,
  isConfirmingRun,
  liveLogs,
  verifyReport,
  onFocusWindow,
  onMoveResizeWindow,
  onToggleMinimize,
  onToggleMaximize,
  onCloseWindow,
  onChangeTab,
  onUpdateParam,
  onPreparePluginRun,
  onQuotePrepared,
  onConfirmPendingQuote,
  onCancelPendingQuote,
  onReverifyChain,
}: {
  windows: ModuleWindowState[];
  plugins: PluginSummary[];
  runs: RunRecord[];
  activeGrant: DatasetGrant | null;
  defaultCatalogId: string;
  pendingQuote: RunQuote | null;
  isConfirmingRun: boolean;
  liveLogs: string[];
  verifyReport: FrogletVerifyReport | null;
  onFocusWindow: (winId: string) => void;
  onMoveResizeWindow: (
    winId: string,
    patch: Partial<Pick<ModuleWindowState, "x" | "y" | "w" | "h">>,
  ) => void;
  onToggleMinimize: (winId: string) => void;
  onToggleMaximize: (winId: string) => void;
  onCloseWindow: (winId: string) => void;
  onChangeTab: (winId: string, tab: WindowTab) => void;
  onUpdateParam: (winId: string, key: string, value: unknown) => void;
  onPreparePluginRun: (plugin: PluginSummary, params: Record<string, unknown>) => void;
  onQuotePrepared: (quote: RunQuote) => void;
  onConfirmPendingQuote: () => void;
  onCancelPendingQuote: () => void;
  onReverifyChain: (run: RunRecord, tamperOneByte: boolean) => void;
}) {
  const visibleWindows = windows.filter((w) => !w.minimized);
  const topWindowId =
    visibleWindows.length > 0
      ? visibleWindows.reduce((best, cur) => (cur.z >= best.z ? cur : best)).id
      : null;

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
      {visibleWindows.map((win) => {
        const plugin = plugins.find((p) => p.manifest.id === win.pluginId);
        if (!plugin) return null;
        const isFocused = win.id === topWindowId;
        const pluginRuns = runs.filter((r) => r.plugin_id === plugin.manifest.id);
        const latestRun = pluginRuns[0] ?? null;
        const latestSucceededRun =
          pluginRuns.find((r) => r.status === "succeeded") ?? null;
        const hasPendingQuote = pendingQuote?.plugin_id === plugin.manifest.id;

        return (
          <ModuleFloatingWindow
            key={win.id}
            win={win}
            plugin={plugin}
            isFocused={isFocused}
            latestRun={latestRun}
            latestSucceededRun={latestSucceededRun}
            runCount={pluginRuns.length}
            activeGrant={activeGrant}
            defaultCatalogId={defaultCatalogId}
            pendingQuote={hasPendingQuote ? pendingQuote : null}
            isConfirmingRun={isConfirmingRun && hasPendingQuote}
            liveLogs={liveLogs}
            verifyReport={isFocused ? verifyReport : null}
            onFocus={() => onFocusWindow(win.id)}
            onMoveResize={(patch) => onMoveResizeWindow(win.id, patch)}
            onMinimize={() => onToggleMinimize(win.id)}
            onMaximize={() => onToggleMaximize(win.id)}
            onClose={() => onCloseWindow(win.id)}
            onChangeTab={(tab) => onChangeTab(win.id, tab)}
            onUpdateParam={(key, val) => onUpdateParam(win.id, key, val)}
            onPrepareRun={() => onPreparePluginRun(plugin, win.params)}
            onQuotePrepared={onQuotePrepared}
            onConfirmPendingQuote={onConfirmPendingQuote}
            onCancelPendingQuote={onCancelPendingQuote}
            onReverifyChain={onReverifyChain}
          />
        );
      })}

      {/* Fallback approval bar if a quote was prepared for a module without an open visible window */}
      {pendingQuote &&
        !visibleWindows.some((w) => w.pluginId === pendingQuote.plugin_id) && (
          <div className="bc-floating-approval-wrap">
            <QuoteApprovalBar
              quote={pendingQuote}
              isConfirmingRun={isConfirmingRun}
              onConfirm={onConfirmPendingQuote}
              onCancel={onCancelPendingQuote}
            />
          </div>
        )}

      {/* Bottom centered HLA-Compass-style floating window tray */}
      {windows.length > 0 && (
        <div className="bc-window-tray" role="toolbar" aria-label="Open module windows">
          {windows.map((win) => {
            const plugin = plugins.find((p) => p.manifest.id === win.pluginId);
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

function QuoteApprovalBar({
  quote,
  isConfirmingRun,
  onConfirm,
  onCancel,
}: {
  quote: RunQuote;
  isConfirmingRun: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  return (
    <div className="bc-approval-bar" data-testid="pending-quote-banner">
      <div className="bc-approval-info">
        <div className="bc-approval-title">
          {isConfirmingRun ? (
            <RunningCompass size={15} />
          ) : (
            <span className="bc-approval-dot" />
          )}
          <span>
            Ready to execute <strong className="mono">{quote.plugin_id}</strong>
          </span>
          <span className="bc-badge bc-badge-accent mono">
            {formatMilliAct(quote.estimated_cost_act)}
          </span>
        </div>
        <div className="bc-approval-sub">
          Grant: <strong>{quote.dataset_grant_summary ?? "Default"}</strong> · Run{" "}
          <span className="mono">{quote.run_id.slice(0, 14)}</span>
        </div>
      </div>
      <div className="bc-approval-actions">
        <button
          type="button"
          data-testid="cancel-quote-btn"
          className="bc-btn bc-btn-ghost"
          disabled={isConfirmingRun}
          onClick={onCancel}
        >
          Cancel
        </button>
        <button
          type="button"
          data-testid="confirm-run-btn"
          className="bc-btn bc-btn-primary"
          disabled={isConfirmingRun}
          onClick={onConfirm}
        >
          {isConfirmingRun
            ? "Running Container..."
            : `Approve & Run (${formatMilliAct(quote.estimated_cost_act)})`}
        </button>
      </div>
    </div>
  );
}

function ModuleFloatingWindow({
  win,
  plugin,
  isFocused,
  latestRun,
  latestSucceededRun,
  runCount,
  activeGrant,
  defaultCatalogId,
  pendingQuote,
  isConfirmingRun,
  liveLogs,
  verifyReport,
  onFocus,
  onMoveResize,
  onMinimize,
  onMaximize,
  onClose,
  onChangeTab,
  onUpdateParam,
  onPrepareRun,
  onQuotePrepared,
  onConfirmPendingQuote,
  onCancelPendingQuote,
  onReverifyChain,
}: {
  win: ModuleWindowState;
  plugin: PluginSummary;
  isFocused: boolean;
  latestRun: RunRecord | null;
  latestSucceededRun: RunRecord | null;
  runCount: number;
  activeGrant: DatasetGrant | null;
  defaultCatalogId: string;
  pendingQuote: RunQuote | null;
  isConfirmingRun: boolean;
  liveLogs: string[];
  verifyReport: FrogletVerifyReport | null;
  onFocus: () => void;
  onMoveResize: (patch: Partial<Pick<ModuleWindowState, "x" | "y" | "w" | "h">>) => void;
  onMinimize: () => void;
  onMaximize: () => void;
  onClose: () => void;
  onChangeTab: (tab: WindowTab) => void;
  onUpdateParam: (key: string, value: unknown) => void;
  onPrepareRun: () => void;
  onQuotePrepared: (quote: RunQuote) => void;
  onConfirmPendingQuote: () => void;
  onCancelPendingQuote: () => void;
  onReverifyChain: (run: RunRecord, tamperOneByte: boolean) => void;
}) {
  const [selectedStageIdx, setSelectedStageIdx] = useState<number>(4);
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
    if (win.maximized) return;
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

  const schemaProps =
    (plugin.manifest.inputs.schema["properties"] as
      | Record<string, SchemaPropertySpec>
      | undefined) ?? {};
  const schemaEntries = Object.entries(schemaProps);

  const isChainValid = verifyReport
    ? verifyReport.valid
    : Boolean(latestRun?.receipt_verified);

  const windowStyle: React.CSSProperties = win.maximized
    ? {
        top: 56,
        left: 16,
        right: 16,
        bottom: 58,
        zIndex: 200 + win.z,
      }
    : {
        top: win.y,
        left: win.x,
        width: win.w,
        height: win.h,
        zIndex: 200 + win.z,
      };

  const logsToDisplay =
    isConfirmingRun || liveLogs.length > 0
      ? liveLogs
      : (latestRun?.logs ?? []);

  return (
    <section
      className="bc-window"
      data-focused={String(isFocused)}
      data-maximized={String(win.maximized)}
      data-testid={`module-window-${plugin.manifest.id}`}
      style={windowStyle}
      onMouseDown={onFocus}
      aria-label={plugin.manifest.name}
    >
      {/* Draggable Window Title Bar */}
      <div
        className="bc-window-titlebar"
        onPointerDown={handleHeaderPointerDown}
        onPointerMove={handleHeaderPointerMove}
        onPointerUp={handleHeaderPointerUp}
        onDoubleClick={onMaximize}
      >
        <div className="bc-window-title-left">
          <span className="bc-window-icon">
            <IconModules size={14} />
          </span>
          <span className="bc-window-title">{plugin.manifest.name}</span>
          <span className="bc-window-version mono">v{plugin.manifest.version}</span>
        </div>

        <div className="bc-window-controls">
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

      {/* Clean Module Navigation Tabs */}
      <div className="bc-window-tabs" role="tablist">
        {plugin.has_custom_ui && (
          <button
            type="button"
            role="tab"
            aria-selected={win.activeTab === "ui"}
            className="bc-window-tab"
            data-active={String(win.activeTab === "ui")}
            onClick={() => onChangeTab("ui")}
          >
            Interactive App
          </button>
        )}
        <button
          type="button"
          role="tab"
          aria-selected={win.activeTab === "run"}
          className="bc-window-tab"
          data-active={String(win.activeTab === "run")}
          onClick={() => onChangeTab("run")}
        >
          Parameters
        </button>
        <button
          type="button"
          role="tab"
          aria-selected={win.activeTab === "results"}
          className="bc-window-tab"
          data-active={String(win.activeTab === "results")}
          onClick={() => onChangeTab("results")}
        >
          <span>Results &amp; Receipt</span>
          {runCount > 0 && <span className="bc-tab-count">{runCount}</span>}
        </button>
        <button
          type="button"
          role="tab"
          aria-selected={win.activeTab === "about"}
          className="bc-window-tab"
          data-active={String(win.activeTab === "about")}
          onClick={() => onChangeTab("about")}
        >
          About
        </button>
      </div>

      {/* Window Body */}
      <div className="bc-window-body">
        {/* Keep custom UI iframe mounted so postMessage run updates stay seamless */}
        {plugin.has_custom_ui && activeGrant && (
          <div
            className="bc-window-iframe-pane"
            style={{ display: win.activeTab === "ui" ? "flex" : "none" }}
          >
            <SandboxedPluginFrame
              plugin={plugin}
              activeGrant={activeGrant}
              defaultCatalogId={defaultCatalogId}
              latestPluginRun={latestSucceededRun}
              isFocused={isFocused}
              onQuotePrepared={onQuotePrepared}
            />
          </div>
        )}

        {win.activeTab === "run" && (
          <div className="bc-window-pane">
            <p className="bc-pane-lead">{plugin.manifest.description}</p>

            {schemaEntries.length === 0 ? (
              <div className="bc-empty-note">
                This module runs with default cohort inputs from your active data grant.
              </div>
            ) : (
              <div className="bc-form-grid">
                {schemaEntries.map(([key, spec]) => {
                  const val = win.params[key] ?? spec.default ?? "";
                  return (
                    <label key={key} className="bc-field">
                      <span className="bc-field-label">
                        {spec.title ?? humanizeKey(key)}
                      </span>
                      {Array.isArray(spec.enum) && spec.enum.length > 0 ? (
                        <select
                          className="bc-input"
                          value={String(val)}
                          onChange={(e) => onUpdateParam(key, e.target.value)}
                        >
                          {spec.enum.map((opt) => (
                            <option key={String(opt)} value={String(opt)}>
                              {String(opt)}
                            </option>
                          ))}
                        </select>
                      ) : spec.type === "boolean" ? (
                        <select
                          className="bc-input"
                          value={val ? "true" : "false"}
                          onChange={(e) =>
                            onUpdateParam(key, e.target.value === "true")
                          }
                        >
                          <option value="true">Enabled</option>
                          <option value="false">Disabled</option>
                        </select>
                      ) : spec.type === "number" || spec.type === "integer" ? (
                        <input
                          type="number"
                          className="bc-input"
                          min={spec.minimum}
                          max={spec.maximum}
                          step={spec.type === "integer" ? 1 : "any"}
                          value={String(val)}
                          onChange={(e) =>
                            onUpdateParam(
                              key,
                              spec.type === "integer"
                                ? parseInt(e.target.value, 10) || 0
                                : parseFloat(e.target.value) || 0,
                            )
                          }
                        />
                      ) : (
                        <input
                          type="text"
                          className="bc-input"
                          value={String(val)}
                          onChange={(e) => onUpdateParam(key, e.target.value)}
                        />
                      )}
                      {spec.description && (
                        <span className="bc-field-hint">{spec.description}</span>
                      )}
                    </label>
                  );
                })}
              </div>
            )}
          </div>
        )}

        {win.activeTab === "results" && (
          <div className="bc-window-pane">
            {!latestRun && !isConfirmingRun ? (
              <div className="bc-empty-note">
                No completed runs for <strong>{plugin.manifest.name}</strong> yet.
                Configure parameters and run the module to view charts, CSV exports, and
                cryptographic receipts.
              </div>
            ) : (
              <div className="bc-results-stack">
                {latestRun?.result_json && (
                  <ScientificViews
                    resultJson={latestRun.result_json}
                    views={plugin.views}
                    exposeTestIds={isFocused}
                  />
                )}

                {latestRun && latestRun.artifacts.length > 0 && (
                  <div
                    className="bc-card"
                    data-testid={isFocused ? "run-artifacts" : undefined}
                  >
                    <div className="bc-card-header">
                      <span className="bc-card-title">Output Files</span>
                      <span className="bc-caption">
                        {latestRun.artifacts.length} file(s)
                      </span>
                    </div>
                    <div className="bc-artifact-list">
                      {latestRun.artifacts.map((art) => (
                        <a
                          key={art.name}
                          className="bc-artifact-chip"
                          href={`/api/v1/runs/${encodeURIComponent(latestRun.run_id)}/artifacts/${encodeURIComponent(art.name)}`}
                          target="_blank"
                          rel="noreferrer"
                        >
                          <span className="mono">{art.name}</span>
                          <span className="bc-caption">
                            {(art.size_bytes / 1024).toFixed(1)} KB
                          </span>
                        </a>
                      ))}
                    </div>
                  </div>
                )}

                <div className="bc-card">
                  <div className="bc-card-header">
                    <span className="bc-card-title">Container Execution Log</span>
                    {isConfirmingRun && (
                      <span className="bc-badge bc-badge-accent">Running</span>
                    )}
                  </div>
                  <pre
                    className="bc-log-pre"
                    data-testid={isFocused ? "live-run-logs" : undefined}
                  >
                    {logsToDisplay.length > 0
                      ? logsToDisplay.join("\n")
                      : "Waiting for container output..."}
                  </pre>
                </div>

                {latestRun && latestRun.froglet_chain.length > 0 && (
                  <div
                    className="bc-card"
                    data-testid={isFocused ? "receipt-inspector" : undefined}
                  >
                    <div className="bc-card-header">
                      <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
                        <IconShield size={15} />
                        <span className="bc-card-title">
                          Verifiable Execution Receipt
                        </span>
                      </div>
                      <span
                        data-testid={isFocused ? "receipt-verified-badge" : undefined}
                        className={`bc-badge ${
                          isChainValid ? "bc-badge-success" : "bc-badge-danger"
                        }`}
                      >
                        {isChainValid
                          ? "SCHNORR CHAIN VALID"
                          : "TAMPER DETECTED / INVALID"}
                      </span>
                    </div>

                    <p className="bc-caption" style={{ marginTop: 4 }}>
                      5-artifact <code>froglet/v1</code> RFC 8785 JCS +{" "}
                      <code>secp256k1</code> Schnorr provenance chain.
                    </p>

                    <div className="bc-receipt-actions">
                      <button
                        type="button"
                        data-testid={isFocused ? "reverify-chain-btn" : undefined}
                        className="bc-btn bc-btn-secondary"
                        onClick={() => onReverifyChain(latestRun, false)}
                      >
                        <IconCheck size={14} />
                        <span>Verify Chain</span>
                      </button>
                      <button
                        type="button"
                        data-testid={isFocused ? "tamper-test-btn" : undefined}
                        className="bc-btn bc-btn-ghost"
                        onClick={() => onReverifyChain(latestRun, true)}
                      >
                        Simulate 1-Byte Tamper
                      </button>
                    </div>

                    <details className="bc-details">
                      <summary>Inspect signed protocol artifacts</summary>
                      <div className="bc-stage-pills">
                        {latestRun.froglet_chain.map((_, idx) => (
                          <button
                            key={idx}
                            type="button"
                            className="bc-btn bc-btn-ghost"
                            data-active={String(selectedStageIdx === idx)}
                            onClick={() => setSelectedStageIdx(idx)}
                          >
                            {idx + 1}. {FROGLET_STAGES[idx] ?? `Artifact ${idx + 1}`}
                          </button>
                        ))}
                      </div>
                      <pre className="bc-code-pre">
                        {JSON.stringify(
                          latestRun.froglet_chain[
                            Math.min(
                              selectedStageIdx,
                              latestRun.froglet_chain.length - 1,
                            )
                          ],
                          null,
                          2,
                        )}
                      </pre>
                    </details>
                  </div>
                )}
              </div>
            )}
          </div>
        )}

        {win.activeTab === "about" && (
          <div className="bc-window-pane">
            <div className="bc-meta-grid">
              <div className="bc-meta-item">
                <span className="bc-caption">Module ID</span>
                <span className="mono">{plugin.manifest.id}</span>
              </div>
              <div className="bc-meta-item">
                <span className="bc-caption">Compute Resources</span>
                <span>
                  {plugin.manifest.resources.cpus} CPU ·{" "}
                  {plugin.manifest.resources.memory_mb} MB RAM ·{" "}
                  {plugin.manifest.resources.timeout_sec}s timeout
                </span>
              </div>
              <div className="bc-meta-item">
                <span className="bc-caption">Container Network</span>
                <span className="mono">{plugin.manifest.network}</span>
              </div>
              <div className="bc-meta-item">
                <span className="bc-caption">Pinned OCI Image</span>
                <span className="mono" style={{ wordBreak: "break-all" }}>
                  {plugin.manifest.image}
                </span>
              </div>
            </div>

            {plugin.skill && (
              <div className="bc-card" style={{ marginTop: 14 }}>
                <div className="bc-card-title" style={{ marginBottom: 6 }}>
                  Agent Skill Specification (SKILL.md)
                </div>
                <pre className="bc-code-pre">{plugin.skill.markdown_body}</pre>
              </div>
            )}
          </div>
        )}
      </div>

      {/* Window Bottom Action / Approval Bar */}
      {pendingQuote ? (
        <QuoteApprovalBar
          quote={pendingQuote}
          isConfirmingRun={isConfirmingRun}
          onConfirm={onConfirmPendingQuote}
          onCancel={onCancelPendingQuote}
        />
      ) : (
        win.activeTab === "run" && (
          <div className="bc-window-footer">
            <div className="bc-caption">
              Active grant: <strong>{activeGrant?.label ?? "Default"}</strong> ·{" "}
              <span className="mono">
                {formatMilliAct(plugin.manifest.price_milli_act)}
              </span>
            </div>
            <button
              type="button"
              data-testid={isFocused ? "plugin-prepare-btn" : undefined}
              className="bc-btn bc-btn-primary"
              onClick={onPrepareRun}
            >
              Run Module ({formatMilliAct(plugin.manifest.price_milli_act)})
            </button>
          </div>
        )
      )}

      {/* Resize Corner Handle */}
      {!win.maximized && (
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
