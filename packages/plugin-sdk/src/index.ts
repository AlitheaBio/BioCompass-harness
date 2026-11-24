/**
 * `@biocompass/plugin-sdk`: Strict JSON-RPC 2.0 `postMessage` bridge between the BioCompass host
 * and `<iframe sandbox="allow-scripts">` custom Vite plugin UIs (`window.origin === "null"`).
 *
 * Security invariants:
 * - Zero direct network egress (`CSP: connect-src 'none'`).
 * - Source-window verification (`event.source === iframe.contentWindow` on host;
 *   `event.source === window.parent` inside sandboxed plugin).
 * - Two-step consent gate: `prepareRun` only requests a signed Froglet `Quote` and surfaces the
 *   host's user confirmation card; sandboxed plugins cannot confirm runs or spend EUR directly.
 */

export const BIOCOMPASS_RPC_CHANNEL = "biocompass.plugin.rpc.v1" as const;

export type GrantAccessLevel = "none" | "compute_only" | "rows";

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

export type SdkScalarValue = string | number | boolean;

export type SdkFilterExpr =
  | {
      op: "eq" | "neq" | "gt" | "gte" | "lt" | "lte";
      column: string;
      value: SdkScalarValue;
    }
  | {
      op: "starts_with";
      column: string;
      prefix: string;
    }
  | {
      op: "in";
      column: string;
      values: SdkScalarValue[];
    }
  | {
      op: "is_null" | "is_not_null";
      column: string;
    }
  | {
      op: "and" | "or";
      exprs: SdkFilterExpr[];
    };

export interface SdkOrderBySpec {
  column: string;
  direction?: "asc" | "desc";
}

export interface SdkAttachedCatalogPin {
  catalog_id: string;
  grant_id: string;
  snapshot_id?: number | null;
}

export interface SdkQueryAst {
  table?: string | null;
  columns?: string[];
  filter?: SdkFilterExpr | null;
  order_by?: SdkOrderBySpec[];
  limit?: number;
  offset?: number;
  snapshot_id?: number | null;
  additional_sources?: SdkAttachedCatalogPin[];
}

export interface SdkCatalogQuery {
  table?: string | null;
  organs?: string[];
  conditions?: string[];
  hla_alleles?: string[];
  gene_symbols?: string[];
  min_length?: number;
  max_length?: number;
  max_affinity_nm?: number;
  sequence_prefix?: string;
  limit?: number;
  snapshot_id?: number;
}

export interface SdkColumnDescriptor {
  name: string;
  data_type: string;
  nullable: boolean;
}

export interface SdkQueryTableResult<TRow = Record<string, unknown>> {
  catalog_id: string;
  snapshot_id: number;
  dataset_digest: string;
  table: string;
  grant_id: string;
  access_level: GrantAccessLevel;
  columns: SdkColumnDescriptor[];
  total_in_scope: number;
  rows: TRow[];
  remaining_daily_row_budget?: number | null;
}

export type SdkCatalogQueryResult<TRow = SdkPeptideRecord> = SdkQueryTableResult<TRow>;

/**
 * Normalize either a typed `SdkQueryAst` or structured `SdkCatalogQuery` filter object into a
 * canonical `SdkQueryAst` ready for `/api/v1/catalogs/:ns/:name/query`.
 */
export function compileSdkCatalogQuery(
  input: SdkQueryAst | SdkCatalogQuery,
): SdkQueryAst {
  const rec = input as Record<string, unknown>;
  if (
    "filter" in rec ||
    "columns" in rec ||
    "order_by" in rec ||
    "offset" in rec ||
    "additional_sources" in rec
  ) {
    return input as SdkQueryAst;
  }

  const legacy = input as SdkCatalogQuery;
  const exprs: SdkFilterExpr[] = [];
  if (legacy.organs && legacy.organs.length > 0) {
    exprs.push({ op: "in", column: "organ", values: legacy.organs });
  }
  if (legacy.conditions && legacy.conditions.length > 0) {
    exprs.push({ op: "in", column: "condition", values: legacy.conditions });
  }
  if (legacy.hla_alleles && legacy.hla_alleles.length > 0) {
    exprs.push({ op: "in", column: "hla_allele", values: legacy.hla_alleles });
  }
  if (legacy.gene_symbols && legacy.gene_symbols.length > 0) {
    exprs.push({ op: "in", column: "gene_symbol", values: legacy.gene_symbols });
  }
  if (typeof legacy.min_length === "number" && Number.isFinite(legacy.min_length)) {
    exprs.push({ op: "gte", column: "peptide_length", value: legacy.min_length });
  }
  if (typeof legacy.max_length === "number" && Number.isFinite(legacy.max_length)) {
    exprs.push({ op: "lte", column: "peptide_length", value: legacy.max_length });
  }
  if (typeof legacy.max_affinity_nm === "number" && Number.isFinite(legacy.max_affinity_nm)) {
    exprs.push({ op: "lte", column: "affinity_nm", value: legacy.max_affinity_nm });
  }
  if (typeof legacy.sequence_prefix === "string" && legacy.sequence_prefix.trim().length > 0) {
    exprs.push({
      op: "starts_with",
      column: "sequence",
      prefix: legacy.sequence_prefix.trim().toUpperCase(),
    });
  }

  const filter: SdkFilterExpr | null =
    exprs.length === 0 ? null : exprs.length === 1 ? exprs[0]! : { op: "and", exprs };

  return {
    ...(legacy.table != null ? { table: legacy.table } : {}),
    ...(filter ? { filter } : {}),
    ...(typeof legacy.limit === "number" ? { limit: legacy.limit } : {}),
    ...(typeof legacy.snapshot_id === "number" ? { snapshot_id: legacy.snapshot_id } : {}),
  };
}

export interface SdkRunQuote {
  run_id: string;
  plugin_id: string;
  plugin_version: string;
  image_ref: string;
  target_node_id: string;
  estimated_cost_milli_eur: number;
  workload_hash: string;
  dataset_grant_summary: string | null;
  expires_at: string;
}

/** Format a milli-EUR integer as euros to the cent (`€0.00`, `€5.00`, `-€0.05`). */
export function formatMilliEur(milliEur: number): string {
  const abs = Math.abs(milliEur);
  const totalCents = Math.floor((abs + 5) / 10);
  const sign = milliEur < 0 && totalCents > 0 ? "-" : "";
  const whole = Math.floor(totalCents / 100);
  const frac = String(totalCents % 100).padStart(2, "0");
  return `${sign}€${whole}.${frac}`;
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

export type PluginTheme = "light" | "dark";

export interface PluginHostContext {
  pluginId: string;
  pluginVersion: string;
  activeGrantId: string;
  activeGrantLabel: string;
  accessLevel: GrantAccessLevel;
  catalogFacets: Record<string, string[]>;
  /** @deprecated v2 3-level grants never mask columns with '***'; always `false`. */
  sequenceMasked: boolean;
  defaultCatalogId: string;
  releaseVersion?: number;
  latestRun: SdkRunRecord | null;
  draftParams?: Record<string, unknown> | null;
  pendingRun?: SdkRunQuote | null;
  pendingParams?: Record<string, unknown> | null;
  theme?: PluginTheme;
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
  /** Deadline for short RPCs. The host owns worker startup and quote deadlines. */
  timeoutMs?: number;
}

export interface SdkDownloadFileRequest {
  filename: string;
  mimeType?: string;
  content: string;
  encoding?: "utf-8" | "base64";
}

export interface BioCompassPluginClient {
  getContext(): Promise<PluginHostContext>;
  queryCatalog<TRow = SdkPeptideRecord>(
    query: SdkQueryAst | SdkCatalogQuery,
  ): Promise<SdkQueryTableResult<TRow>>;
  prepareRun(params: Record<string, unknown>): Promise<SdkRunQuote>;
  getLatestRun(): Promise<SdkRunRecord | null>;
  sendToChat(message: string, context?: Record<string, unknown>): Promise<void>;
  syncDraftParams(params: Record<string, unknown>): Promise<void>;
  downloadFile(req: SdkDownloadFileRequest): Promise<void>;
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
      timer: ReturnType<typeof setTimeout> | undefined;
    }
  >();
  const contextListeners = new Set<(ctx: PluginHostContext) => void>();

  const syncDocumentTheme = (ctx: PluginHostContext | null | undefined): void => {
    if (typeof document === "undefined") return;
    const theme = ctx?.theme;
    if (theme === "dark" || theme === "light") {
      document.documentElement.setAttribute("data-theme", theme);
      document.documentElement.style.colorScheme = theme;
      if (document.body) {
        document.body.setAttribute("data-theme", theme);
      }
    }
  };

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
      syncDocumentTheme(ctx);
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
      // Preparation can wait for a cold worker. The host reports its own deadline
      // and can still present a valid paid quote until that request finishes.
      const timer = method === "biocompass.prepareRun" ? undefined : setTimeout(() => {
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
    getContext: async () => {
      const ctx = await call<PluginHostContext>("biocompass.getContext");
      syncDocumentTheme(ctx);
      return ctx;
    },
    queryCatalog: <TRow = SdkPeptideRecord>(query: SdkQueryAst | SdkCatalogQuery) =>
      call<SdkQueryTableResult<TRow>>(
        "biocompass.queryCatalog",
        compileSdkCatalogQuery(query),
      ),
    prepareRun: (params) =>
      call<SdkRunQuote>("biocompass.prepareRun", params),
    getLatestRun: () => call<SdkRunRecord | null>("biocompass.getLatestRun"),
    sendToChat: (message, context) =>
      call<void>("biocompass.sendToChat", { message, context }),
    syncDraftParams: (params) =>
      call<void>("biocompass.syncDraftParams", params),
    downloadFile: (req) =>
      call<void>(
        "biocompass.downloadFile",
        req as unknown as Record<string, unknown>,
      ),
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
  queryCatalog(query: SdkQueryAst): Promise<SdkQueryTableResult<unknown>>;
  prepareRun(params: Record<string, unknown>): Promise<SdkRunQuote>;
  getLatestRun(): Promise<SdkRunRecord | null> | SdkRunRecord | null;
  sendToChat?: (message: string, context?: Record<string, unknown>) => Promise<void> | void;
  syncDraftParams?: (params: Record<string, unknown>) => Promise<void> | void;
  downloadFile?: (req: SdkDownloadFileRequest) => Promise<void> | void;
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
              ? compileSdkCatalogQuery(req.params as SdkQueryAst | SdkCatalogQuery)
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
          case "biocompass.sendToChat": {
            if (!isObjectRecord(req.params) || typeof req.params["message"] !== "string") {
              throw new Error("sendToChat requires { message: string }");
            }
            const ctx = isObjectRecord(req.params["context"])
              ? req.params["context"]
              : undefined;
            if (!handlers.sendToChat) {
              throw new Error("Host does not handle biocompass.sendToChat");
            }
            await handlers.sendToChat(req.params["message"], ctx);
            result = { ok: true };
            break;
          }
          case "biocompass.syncDraftParams": {
            if (!isObjectRecord(req.params)) {
              throw new Error("syncDraftParams params must be a JSON object");
            }
            if (handlers.syncDraftParams) {
              await handlers.syncDraftParams(req.params);
            }
            result = { ok: true };
            break;
          }
          case "biocompass.downloadFile": {
            if (
              !isObjectRecord(req.params) ||
              typeof req.params["filename"] !== "string" ||
              typeof req.params["content"] !== "string"
            ) {
              throw new Error("downloadFile requires { filename: string, content: string }");
            }
            if (!handlers.downloadFile) {
              throw new Error("Host does not handle biocompass.downloadFile");
            }
            const safeFilename =
              req.params["filename"]
                .replace(/[/\\]+/g, "_")
                .replace(/\.\.+/g, ".")
                .trim() || "download";
            const enc = req.params["encoding"] === "base64" ? "base64" : "utf-8";
            await handlers.downloadFile({
              filename: safeFilename,
              ...(typeof req.params["mimeType"] === "string"
                ? { mimeType: req.params["mimeType"] }
                : {}),
              content: req.params["content"],
              encoding: enc,
            });
            result = { ok: true };
            break;
          }
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
