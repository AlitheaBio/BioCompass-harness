import { useEffect, useMemo, useRef } from "react";
import {
  attachPluginHostBridge,
  type PluginHostBridge,
  type PluginHostContext,
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

export function SandboxedPluginFrame({
  plugin,
  activeGrant,
  defaultCatalogId,
  latestPluginRun,
  isFocused = true,
  onQuotePrepared,
}: {
  plugin: PluginSummary;
  activeGrant: DatasetGrant;
  defaultCatalogId: string;
  latestPluginRun: RunRecord | null;
  isFocused?: boolean;
  onQuotePrepared: (quote: RunQuote) => void;
}) {
  const iframeRef = useRef<HTMLIFrameElement | null>(null);
  const bridgeRef = useRef<PluginHostBridge | null>(null);

  const hostContext: PluginHostContext = useMemo(
    () => ({
      pluginId: plugin.manifest.id,
      pluginVersion: plugin.manifest.version,
      activeGrantId: activeGrant.grant_id,
      activeGrantLabel: activeGrant.label,
      sequenceMasked: activeGrant.mask_peptide_sequences,
      defaultCatalogId,
      latestRun: toSdkRunRecord(latestPluginRun),
    }),
    [plugin.manifest.id, plugin.manifest.version, activeGrant, defaultCatalogId, latestPluginRun],
  );

  const contextRef = useRef<PluginHostContext>(hostContext);
  contextRef.current = hostContext;

  const grantRef = useRef<DatasetGrant>(activeGrant);
  grantRef.current = activeGrant;

  const latestRunRef = useRef<RunRecord | null>(latestPluginRun);
  latestRunRef.current = latestPluginRun;

  useEffect(() => {
    const iframe = iframeRef.current;
    if (!iframe) return;

    const bridge = attachPluginHostBridge(iframe, {
      getContext: () => contextRef.current,
      queryCatalog: async (query) => {
        return queryCatalog(
          contextRef.current.defaultCatalogId,
          grantRef.current.grant_id,
          query,
        );
      },
      prepareRun: async (params) => {
        const quote = await prepareRun({
          plugin_id: plugin.manifest.id,
          params,
          grant_id: grantRef.current.grant_id,
        });
        onQuotePrepared(quote);
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
    });

    bridgeRef.current = bridge;
    return () => {
      bridgeRef.current = null;
      bridge.dispose();
    };
  }, [plugin.manifest.id, onQuotePrepared]);

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
