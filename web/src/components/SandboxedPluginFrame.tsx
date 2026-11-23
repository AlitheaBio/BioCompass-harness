import { useEffect, useMemo, useRef } from "react";
import {
  attachPluginHostBridge,
  type PluginHostBridge,
  type PluginHostContext,
  type SdkRunQuote,
  type SdkRunRecord,
} from "@biocompass/plugin-sdk";
import { prepareRun, queryCatalog } from "../api";
import type { DatasetGrant, PluginSummary, RunQuote, RunRecord } from "../types";

function toSdkRunRecord(run: RunRecord | null): SdkRunRecord | null {
  if (!run) return null;
  return {
    run_id: run.run_id,
    plugin_id: run.plugin_id,
    plugin_version: run.plugin_version,
    status: run.status,
    params: run.params,
    result_json: run.result_json,
    artifacts: run.artifacts,
    receipt_verified: run.receipt_verified,
    error_message: run.error_message,
  };
}

function toSdkRunQuote(quote: RunQuote | null): SdkRunQuote | null {
  if (!quote) return null;
  return {
    run_id: quote.run_id,
    plugin_id: quote.plugin_id,
    plugin_version: quote.plugin_version,
    image_ref: quote.image_ref,
    target_node_id: quote.target_node_id,
    estimated_cost_act: quote.estimated_cost_act,
    workload_hash: quote.workload_hash,
    dataset_grant_summary: quote.dataset_grant_summary,
    expires_at: quote.expires_at,
  };
}

export function SandboxedPluginFrame({
  plugin,
  activeGrant,
  defaultCatalogId,
  releaseVersion,
  catalogFacets = {},
  sessionId,
  latestPluginRun,
  draftParams = null,
  pendingQuote = null,
  pendingParams = null,
  isFocused = true,
  theme = "light",
  onQuotePrepared,
  onSendToChat,
  onSyncDraftParams,
}: {
  plugin: PluginSummary;
  activeGrant: DatasetGrant | null;
  defaultCatalogId: string;
  releaseVersion?: number;
  catalogFacets?: Record<string, string[]>;
  sessionId?: string;
  latestPluginRun: RunRecord | null;
  draftParams?: Record<string, unknown> | null;
  pendingQuote?: RunQuote | null;
  pendingParams?: Record<string, unknown> | null;
  isFocused?: boolean;
  theme?: "light" | "dark";
  onQuotePrepared: (quote: RunQuote, params: Record<string, unknown>) => void;
  onSendToChat?: (
    pluginId: string,
    message: string,
    context?: Record<string, unknown>,
  ) => void;
  onSyncDraftParams?: (pluginId: string, params: Record<string, unknown>) => void;
}) {
  const iframeRef = useRef<HTMLIFrameElement | null>(null);
  const bridgeRef = useRef<PluginHostBridge | null>(null);

  const hostContext: PluginHostContext = useMemo(
    () => ({
      pluginId: plugin.manifest.id,
      pluginVersion: plugin.manifest.version,
      activeGrantId: activeGrant?.grant_id ?? "",
      activeGrantLabel: activeGrant?.label ?? "",
      accessLevel: activeGrant?.level ?? "none",
      catalogFacets,
      sequenceMasked: false,
      defaultCatalogId,
      releaseVersion,
      latestRun: toSdkRunRecord(latestPluginRun),
      draftParams,
      pendingRun: toSdkRunQuote(pendingQuote),
      pendingParams,
      theme,
    }),
    [
      plugin.manifest.id,
      plugin.manifest.version,
      activeGrant,
      catalogFacets,
      defaultCatalogId,
      releaseVersion,
      latestPluginRun,
      draftParams,
      pendingQuote,
      pendingParams,
      theme,
    ],
  );

  const contextRef = useRef<PluginHostContext>(hostContext);
  contextRef.current = hostContext;

  const grantRef = useRef<DatasetGrant | null>(activeGrant);
  grantRef.current = activeGrant;

  const sessionIdRef = useRef<string | undefined>(sessionId);
  sessionIdRef.current = sessionId;

  const latestRunRef = useRef<RunRecord | null>(latestPluginRun);
  latestRunRef.current = latestPluginRun;

  const sendToChatRef = useRef(onSendToChat);
  sendToChatRef.current = onSendToChat;

  const syncDraftRef = useRef(onSyncDraftParams);
  syncDraftRef.current = onSyncDraftParams;

  useEffect(() => {
    const iframe = iframeRef.current;
    if (!iframe) return;

    const bridge = attachPluginHostBridge(iframe, {
      getContext: () => contextRef.current,
      queryCatalog: async (query) => {
        if (!grantRef.current) {
          throw new Error("Select a dataset grant with row access before querying a catalog.");
        }
        if (grantRef.current.level !== "rows") {
          throw new Error(
            `Grant '${grantRef.current.label}' is '${grantRef.current.level}' — direct catalog row queries require level 'rows'. Run a curated module instead.`,
          );
        }
        return queryCatalog(
          contextRef.current.defaultCatalogId,
          grantRef.current.grant_id,
          query,
          contextRef.current.releaseVersion,
        );
      },
      prepareRun: async (params) => {
        const quote = await prepareRun({
          plugin_id: contextRef.current.pluginId,
          plugin_version: contextRef.current.pluginVersion,
          marketplace_offer_id: plugin.marketplace_offer_id,
          params,
          grant_id: grantRef.current?.grant_id,
          release_version: grantRef.current ? contextRef.current.releaseVersion : undefined,
          session_id: sessionIdRef.current,
        });
        onQuotePrepared(quote, params);
        return {
          run_id: quote.run_id,
          plugin_id: quote.plugin_id,
          plugin_version: quote.plugin_version,
          image_ref: quote.image_ref,
          target_node_id: quote.target_node_id,
          estimated_cost_act: quote.estimated_cost_act,
          workload_hash: quote.workload_hash,
          dataset_grant_summary: quote.dataset_grant_summary,
          expires_at: quote.expires_at,
        };
      },
      getLatestRun: () => toSdkRunRecord(latestRunRef.current),
      sendToChat: (message, ctx) => {
        sendToChatRef.current?.(plugin.manifest.id, message, ctx);
      },
      syncDraftParams: (params) => {
        syncDraftRef.current?.(plugin.manifest.id, params);
      },
      downloadFile: ({ filename, mimeType, content, encoding }) => {
        let blob: Blob;
        if (encoding === "base64") {
          const bin = window.atob(content);
          const bytes = new Uint8Array(bin.length);
          for (let i = 0; i < bin.length; i += 1) {
            bytes[i] = bin.charCodeAt(i);
          }
          blob = new Blob([bytes], {
            type: mimeType || "application/octet-stream",
          });
        } else {
          blob = new Blob([content], {
            type: mimeType || "text/plain;charset=utf-8",
          });
        }
        const url = URL.createObjectURL(blob);
        const link = document.createElement("a");
        link.href = url;
        link.download = filename;
        link.style.display = "none";
        document.body.appendChild(link);
        link.click();
        link.remove();
        window.setTimeout(() => URL.revokeObjectURL(url), 1000);
      },
    });

    bridgeRef.current = bridge;
    return () => {
      bridgeRef.current = null;
      bridge.dispose();
    };
  }, [plugin.manifest.id, plugin.marketplace_offer_id, onQuotePrepared]);

  useEffect(() => {
    bridgeRef.current?.pushContextUpdate(hostContext);
  }, [hostContext]);

  if (!plugin.ui_entry_url) {
    return null;
  }

  return (
    <div className="sandbox-frame-shell">
      <iframe
        ref={iframeRef}
        data-testid={
          isFocused
            ? "plugin-sandbox-iframe"
            : `plugin-sandbox-iframe-${plugin.manifest.id}`
        }
        title={`${plugin.manifest.name} Sandboxed UI`}
        src={plugin.ui_entry_url}
        sandbox="allow-scripts"
        className="sandbox-iframe"
      />
    </div>
  );
}
