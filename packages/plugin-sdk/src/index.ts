/**
 * `@biocompass/plugin-sdk`: Strict JSON-RPC 2.0 `postMessage` bridge between the BioCompass host
 * and `<iframe sandbox="allow-scripts">` custom Vite plugin UIs (`window.origin === "null"`).
 *
 * Security invariants:
 * - Zero direct network egress (`CSP: connect-src 'none'`).
 * - Source-window verification (`event.source === iframe.contentWindow` on host;
 *   `event.source === window.parent` inside sandboxed plugin).
 * - Two-step consent gate: `prepareRun` only requests a signed Froglet `Quote` and surfaces the
 *   host's user confirmation card; sandboxed plugins cannot confirm runs or spend ACT directly.
 */

export const BIOCOMPASS_RPC_CHANNEL = "biocompass.plugin.rpc.v1" as const;

export interface SdkPeptideRecord {
  peptide_id: string;
  sequence: string;
  peptide_length: number;
  hla_allele: string;
  gene_symbol: string;
  organ: string;
  condition: string;
  affinity_nm: number;
  eluted_rank_pct: number;
  ms_intensity_log2: number;
  donor_count: number;
}

export interface SdkCatalogQuery {
  organs?: string[];
  conditions?: string[];
  hla_alleles?: string[];
  gene_symbols?: string[];
  min_length?: number;
  max_length?: number;
  max_affinity_nm?: number;
  limit?: number;
}

export interface SdkCatalogQueryResult {
  catalog_id: string;
  grant_id: string;
  sequence_masked: boolean;
  compiled_view_sql: string;
  total_in_scope: number;
  rows: SdkPeptideRecord[];
}

export interface SdkRunQuote {
  run_id: string;
  plugin_id: string;
  plugin_version: string;
  image_ref: string;
  target_node_id: string;
  estimated_cost_act: number;
  workload_hash: string;
  dataset_grant_summary: string | null;
  expires_at: string;
}

export interface SdkArtifactRef {
  name: string;
  blob_key: string;
  media_type: string;
  size_bytes: number;
  sha256: string;
}

export interface SdkRunRecord {
  run_id: string;
  plugin_id: string;
  plugin_version: string;
  status: "prepared" | "running" | "succeeded" | "failed" | "canceled";
  params: Record<string, unknown>;
  result_json: Record<string, unknown> | null;
  artifacts: SdkArtifactRef[];
  receipt_verified: boolean;
  error_message: string | null;
}

export interface PluginHostContext {
  pluginId: string;
  pluginVersion: string;
  activeGrantId: string;
  activeGrantLabel: string;
  sequenceMasked: boolean;
  defaultCatalogId: string;
  latestRun: SdkRunRecord | null;
}

interface JsonRpcRequestFrame {
  jsonrpc: "2.0";
  channel: typeof BIOCOMPASS_RPC_CHANNEL;
  id: string;
  method: string;
  params?: unknown;
}

interface JsonRpcSuccessFrame {
  jsonrpc: "2.0";
  channel: typeof BIOCOMPASS_RPC_CHANNEL;
  id: string;
  result: unknown;
}

interface JsonRpcErrorFrame {
  jsonrpc: "2.0";
  channel: typeof BIOCOMPASS_RPC_CHANNEL;
  id: string;
  error: {
    code: number;
    message: string;
  };
}

interface JsonRpcNotificationFrame {
  jsonrpc: "2.0";
  channel: typeof BIOCOMPASS_RPC_CHANNEL;
  method: string;
  params?: unknown;
}

type RpcFrame =
  | JsonRpcRequestFrame
  | JsonRpcSuccessFrame
  | JsonRpcErrorFrame
  | JsonRpcNotificationFrame;

function isObjectRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

function isBiocompassFrame(data: unknown): data is RpcFrame {
  return (
    isObjectRecord(data) &&
    data["jsonrpc"] === "2.0" &&
    data["channel"] === BIOCOMPASS_RPC_CHANNEL
  );
}

export interface PluginClientOptions {
  timeoutMs?: number;
}

export interface BioCompassPluginClient {
  getContext(): Promise<PluginHostContext>;
  queryCatalog(query: SdkCatalogQuery): Promise<SdkCatalogQueryResult>;
  prepareRun(params: Record<string, unknown>): Promise<SdkRunQuote>;
  getLatestRun(): Promise<SdkRunRecord | null>;
  onContextUpdate(listener: (ctx: PluginHostContext) => void): () => void;
  dispose(): void;
}

/**
 * Create a JSON-RPC 2.0 client inside a sandboxed plugin `<iframe sandbox="allow-scripts">`.
 */
export function createPluginClient(
  options: PluginClientOptions = {},
): BioCompassPluginClient {
  const timeoutMs = options.timeoutMs ?? 15_000;
  let seq = 0;

  const pending = new Map<
    string,
    {
      resolve: (value: unknown) => void;
      reject: (reason: Error) => void;
      timer: ReturnType<typeof setTimeout>;
    }
  >();
  const contextListeners = new Set<(ctx: PluginHostContext) => void>();

  const onMessage = (event: MessageEvent<unknown>): void => {
    if (event.source !== window.parent) {
      return;
    }
    const frame = event.data;
    if (!isBiocompassFrame(frame)) {
      return;
    }

    if ("id" in frame && ("result" in frame || "error" in frame)) {
      const entry = pending.get(frame.id);
      if (!entry) {
        return;
      }
      clearTimeout(entry.timer);
      pending.delete(frame.id);
      if ("error" in frame) {
        entry.reject(new Error(frame.error.message));
      } else {
        entry.resolve(frame.result);
      }
      return;
    }

    if (
      !("id" in frame) &&
      frame.method === "biocompass.contextUpdated" &&
      isObjectRecord(frame.params)
    ) {
      const ctx = frame.params as unknown as PluginHostContext;
      for (const listener of contextListeners) {
        listener(ctx);
      }
    }
  };

  window.addEventListener("message", onMessage);

  function call<TResult>(method: string, params?: unknown): Promise<TResult> {
    if (window.parent === window) {
      return Promise.reject(
        new Error("BioCompass plugin UI must run inside a host iframe"),
      );
    }
    seq += 1;
    const id = `rpc_${Date.now()}_${seq}`;
    const payload: JsonRpcRequestFrame = {
      jsonrpc: "2.0",
      channel: BIOCOMPASS_RPC_CHANNEL,
      id,
      method,
      params,
    };

    return new Promise<TResult>((resolve, reject) => {
      const timer = setTimeout(() => {
        pending.delete(id);
        reject(new Error(`RPC timeout (${timeoutMs}ms) calling ${method}`));
      }, timeoutMs);

      pending.set(id, {
        resolve: (val) => resolve(val as TResult),
        reject,
        timer,
      });

      window.parent.postMessage(payload, "*");
    });
  }

  return {
    getContext: () => call<PluginHostContext>("biocompass.getContext"),
    queryCatalog: (query) =>
      call<SdkCatalogQueryResult>("biocompass.queryCatalog", query),
    prepareRun: (params) =>
      call<SdkRunQuote>("biocompass.prepareRun", params),
    getLatestRun: () => call<SdkRunRecord | null>("biocompass.getLatestRun"),
    onContextUpdate: (listener) => {
      contextListeners.add(listener);
      return () => {
        contextListeners.delete(listener);
      };
    },
    dispose: () => {
      window.removeEventListener("message", onMessage);
      for (const [, entry] of pending) {
        clearTimeout(entry.timer);
        entry.reject(new Error("Plugin RPC client disposed"));
      }
      pending.clear();
      contextListeners.clear();
    },
  };
}

export interface PluginHostHandlers {
  getContext(): Promise<PluginHostContext> | PluginHostContext;
  queryCatalog(query: SdkCatalogQuery): Promise<SdkCatalogQueryResult>;
  prepareRun(params: Record<string, unknown>): Promise<SdkRunQuote>;
  getLatestRun(): Promise<SdkRunRecord | null> | SdkRunRecord | null;
}

export interface PluginHostBridge {
  pushContextUpdate(ctx: PluginHostContext): void;
  dispose(): void;
}

/**
 * Attach a host-side JSON-RPC 2.0 handler strictly bound to a single `HTMLIFrameElement`.
 */
export function attachPluginHostBridge(
  iframe: HTMLIFrameElement,
  handlers: PluginHostHandlers,
): PluginHostBridge {
  const onMessage = (event: MessageEvent<unknown>): void => {
    if (!iframe.contentWindow || event.source !== iframe.contentWindow) {
      return;
    }
    const frame = event.data;
    if (!isBiocompassFrame(frame) || !("id" in frame) || !("method" in frame)) {
      return;
    }

    const req = frame as JsonRpcRequestFrame;
    void (async () => {
      try {
        let result: unknown;
        switch (req.method) {
          case "biocompass.getContext":
            result = await handlers.getContext();
            break;
          case "biocompass.queryCatalog": {
            const q = isObjectRecord(req.params)
              ? (req.params as SdkCatalogQuery)
              : {};
            result = await handlers.queryCatalog(q);
            break;
          }
          case "biocompass.prepareRun": {
            if (!isObjectRecord(req.params)) {
              throw new Error("prepareRun params must be a JSON object");
            }
            result = await handlers.prepareRun(req.params);
            break;
          }
          case "biocompass.getLatestRun":
            result = await handlers.getLatestRun();
            break;
          default:
            throw new Error(`Unsupported BioCompass RPC method '${req.method}'`);
        }

        const reply: JsonRpcSuccessFrame = {
          jsonrpc: "2.0",
          channel: BIOCOMPASS_RPC_CHANNEL,
          id: req.id,
          result,
        };
        iframe.contentWindow?.postMessage(reply, "*");
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        const reply: JsonRpcErrorFrame = {
          jsonrpc: "2.0",
          channel: BIOCOMPASS_RPC_CHANNEL,
          id: req.id,
          error: {
            code: -32000,
            message,
          },
        };
        iframe.contentWindow?.postMessage(reply, "*");
      }
    })();
  };

  window.addEventListener("message", onMessage);

  return {
    pushContextUpdate: (ctx) => {
      if (!iframe.contentWindow) {
        return;
      }
      const notification: JsonRpcNotificationFrame = {
        jsonrpc: "2.0",
        channel: BIOCOMPASS_RPC_CHANNEL,
        method: "biocompass.contextUpdated",
        params: ctx,
      };
      iframe.contentWindow.postMessage(notification, "*");
    },
    dispose: () => {
      window.removeEventListener("message", onMessage);
    },
  };
}
