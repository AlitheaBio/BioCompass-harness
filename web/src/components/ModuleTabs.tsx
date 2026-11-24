import { useEffect, useMemo, useRef, useState } from "react";
import { cancelRun, confirmRun, formatErrorInline, prepareRun } from "../api";
import { getExternalProviderConsentInfo } from "../milestoneBPolicy";
import { IconCheck, IconCopy, IconRuns, IconShield } from "./Icons";
import { ProvenancePanel } from "./ProvenancePanel";
import { ScientificViews } from "./ScientificViews";
import {
  effectiveRunCostMilli,
  formatMilliEur,
  runStatusLabel,
  type DatasetGrant,
  type RunVerification,
  type PluginSummary,
  type ResolvedPluginOffer,
  type RunQuote,
  type RunRecord,
} from "../types";

export interface SchemaPropertySpec {
  type?: string;
  title?: string;
  description?: string;
  default?: unknown;
  example?: unknown;
  examples?: unknown[];
  enum?: unknown[];
  minimum?: number;
  maximum?: number;
  items?: { type?: string };
}

const FROGLET_STAGES = ["Descriptor", "Offer", "Quote", "Deal", "Receipt"] as const;

type SnippetLanguage = "curl" | "python" | "mcp";

function humanizeKey(key: string): string {
  return key
    .replace(/_/g, " ")
    .replace(/\b\w/g, (ch) => ch.toUpperCase());
}

function formatDuration(startIso: string, endIso: string | null): string | null {
  if (!endIso) return null;
  const start = Date.parse(startIso);
  const end = Date.parse(endIso);
  if (!Number.isFinite(start) || !Number.isFinite(end) || end < start) return null;
  const ms = end - start;
  if (ms < 1000) return `${ms}ms`;
  const sec = ms / 1000;
  if (sec < 60) return `${sec.toFixed(1)}s`;
  const mins = Math.floor(sec / 60);
  const rem = Math.round(sec % 60);
  return `${mins}m ${rem}s`;
}

function formatRunTimestamp(iso: string): string {
  const ms = Date.parse(iso);
  if (!Number.isFinite(ms)) return iso;
  return new Date(ms).toLocaleString(undefined, {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

function isSafeExternalUrl(url: string | null | undefined): url is string {
  if (!url) return false;
  return /^https?:\/\/\S+$/i.test(url.trim());
}

function formatSchemaType(spec: SchemaPropertySpec): string {
  const base = spec.type ?? "any";
  const typed =
    base === "array" && spec.items?.type ? `array<${spec.items.type}>` : base;
  const bounds: string[] = [];
  if (typeof spec.minimum === "number") bounds.push(`>= ${spec.minimum}`);
  if (typeof spec.maximum === "number") bounds.push(`<= ${spec.maximum}`);
  return bounds.length > 0 ? `${typed} (${bounds.join(", ")})` : typed;
}

function defaultValueForSpec(spec: SchemaPropertySpec): unknown {
  if (spec.default !== undefined) return spec.default;
  if (spec.example !== undefined) return spec.example;
  if (Array.isArray(spec.examples) && spec.examples.length > 0) return spec.examples[0];
  if (Array.isArray(spec.enum) && spec.enum.length > 0) return spec.enum[0];
  switch (spec.type) {
    case "integer":
      return typeof spec.minimum === "number" ? Math.ceil(spec.minimum) : 1;
    case "number":
      return typeof spec.minimum === "number" ? spec.minimum : 0;
    case "boolean":
      return false;
    case "array":
      return [];
    case "object":
      return {};
    default:
      return "";
  }
}

function buildSeedParams(plugin: PluginSummary): Record<string, unknown> {
  const schema = plugin.manifest.inputs.schema ?? {};
  const props =
    (schema["properties"] as Record<string, SchemaPropertySpec> | undefined) ?? {};
  const out: Record<string, unknown> = {};
  for (const [key, spec] of Object.entries(props)) {
    out[key] = defaultValueForSpec(spec);
  }
  return out;
}

function buildCurlSnippet(
  baseUrl: string,
  manifest: PluginSummary["manifest"],
  params: Record<string, unknown>,
  grantId?: string,
  marketplaceOfferId?: string,
): string {
  const preparePayload = JSON.stringify(
    {
      plugin_id: manifest.id,
      plugin_version: manifest.version,
      params,
      ...(grantId ? { grant_id: grantId } : {}),
      ...(marketplaceOfferId ? { marketplace_offer_id: marketplaceOfferId } : {}),
    },
    null,
    2,
  );
  return `# 1. Prepare a Froglet quote for this run
QUOTE_JSON=$(curl -sS -X POST "${baseUrl}/api/v1/runs/prepare" \\
  -H "Content-Type: application/json" \\
  -d '${preparePayload}')

RUN_ID=$(echo "$QUOTE_JSON" | python3 -c "import sys, json; print(json.load(sys.stdin)['run_id'])")

# 2. Dispatch containerized execution & sign verifiable Froglet receipt
curl -sS -X POST "${baseUrl}/api/v1/runs/\${RUN_ID}/confirm" \\
  -H "Content-Type: application/json"

# 3. Poll run status, outputs & receipt
curl -sS "${baseUrl}/api/v1/runs/\${RUN_ID}"`;
}

function buildPythonSnippet(
  baseUrl: string,
  manifest: PluginSummary["manifest"],
  params: Record<string, unknown>,
  grantId?: string,
  marketplaceOfferId?: string,
): string {
  const payloadPy = JSON.stringify(
    {
      plugin_id: manifest.id,
      plugin_version: manifest.version,
      params,
      ...(grantId ? { grant_id: grantId } : {}),
      ...(marketplaceOfferId ? { marketplace_offer_id: marketplaceOfferId } : {}),
    },
    null,
    2,
  );
  return `import requests

BASE_URL = "${baseUrl}"
payload = ${payloadPy}

# 1. Prepare run quote
quote = requests.post(f"{BASE_URL}/api/v1/runs/prepare", json=payload, timeout=30).json()
run_id = quote["run_id"]

# 2. Execute module container and retrieve results + Froglet receipt
run = requests.post(f"{BASE_URL}/api/v1/runs/{run_id}/confirm", timeout=300).json()
print("Status:", run["status"], "| Receipt valid:", run["receipt_verified"])
print("Results:", run.get("result_json"))`;
}

function buildMcpSnippet(
  manifest: PluginSummary["manifest"],
  params: Record<string, unknown>,
  hasGrant: boolean,
): string {
  return JSON.stringify(
    {
      server: "biocompass",
      tool: "prepare_plugin_run",
      arguments: {
        plugin_id: manifest.id,
        plugin_version: manifest.version,
        params,
        attach_active_grant: hasGrant,
      },
    },
    null,
    2,
  );
}

// ---------------------------------------------------------------------------
// 1. API Documentation & Live Interactive Tester Tab
// ---------------------------------------------------------------------------

export function ModuleApiTab({
  plugin,
  initialParams,
  activeGrant,
  sessionId,
  isFocused,
  isConfirmingRun,
  isLocalMode = false,
  onSyncDraftParams,
  onRunApiTest,
  onQuoteConfirmed,
  onInspectRun,
}: {
  plugin: PluginSummary;
  initialParams: Record<string, unknown>;
  activeGrant: DatasetGrant | null;
  sessionId?: string;
  isFocused: boolean;
  isConfirmingRun: boolean;
  isLocalMode?: boolean;
  onSyncDraftParams?: (pluginId: string, params: Record<string, unknown>) => void;
  onRunApiTest?: (
    params: Record<string, unknown>,
    offerId?: string,
  ) => Promise<RunRecord | null>;
  onQuoteConfirmed?: (completed: RunRecord) => Promise<void> | void;
  onInspectRun: (runId: string) => void;
}) {
  const isLocalRun =
    plugin.runs_in === "local" ||
    (isLocalMode && plugin.runs_in !== "cloud" && plugin.runs_in !== "network_provider");
  const fmtAmount = (milliEur: number) =>
    isLocalRun ? "Local run (no charge)" : formatMilliEur(milliEur);
  const schema = plugin.manifest.inputs.schema ?? {};
  const schemaProps =
    (schema["properties"] as Record<string, SchemaPropertySpec> | undefined) ?? {};
  const requiredSet = useMemo(() => {
    const req = schema["required"];
    return new Set(Array.isArray(req) ? req.filter((x): x is string => typeof x === "string") : []);
  }, [schema]);

  const effectiveOffers = useMemo<ResolvedPluginOffer[]>(() => {
    if (plugin.effective_offers && plugin.effective_offers.length > 0) {
      return plugin.effective_offers;
    }
    return [
      {
        id: "default",
        name: "Standard Run",
        price_milli_eur: plugin.manifest.price_milli_eur,
        base_fee_milli_eur: 0,
        max_input_bytes: 64 * 1024 * 1024,
        max_output_bytes: 32 * 1024 * 1024,
        max_runtime_sec: plugin.manifest.resources.timeout_sec,
        max_memory_mb: plugin.manifest.resources.memory_mb,
      },
    ];
  }, [plugin]);

  const [selectedOfferId, setSelectedOfferId] = useState<string>(
    effectiveOffers[0]?.id ?? "default",
  );
  const selectedOffer =
    effectiveOffers.find((o) => o.id === selectedOfferId) ?? effectiveOffers[0]!;

  const schemaEntries = useMemo(() => Object.entries(schemaProps), [schemaProps]);
  const seedParams = useMemo(() => buildSeedParams(plugin), [plugin]);

  const initialBody = useMemo(() => {
    const merged =
      Object.keys(initialParams).length > 0 ? initialParams : seedParams;
    return JSON.stringify(merged, null, 2);
  }, [initialParams, seedParams]);

  const [requestBody, setRequestBody] = useState<string>(initialBody);
  const reviewInputs = JSON.stringify([
    plugin.manifest.id, plugin.manifest.version, selectedOffer.id, activeGrant?.grant_id, sessionId, requestBody,
  ]);
  const currentReviewInputs = useRef(reviewInputs);
  currentReviewInputs.current = reviewInputs;
  const [reviewed, setReviewed] = useState<{ quote: RunQuote; inputs: string } | null>(null);
  const preparedQuote = reviewed?.inputs === reviewInputs ? reviewed.quote : null;
  const preparedExternalProvider = preparedQuote
    ? getExternalProviderConsentInfo(preparedQuote)
    : null;
  const [externalQuoteConsentAck, setExternalQuoteConsentAck] = useState(false);
  const confirmingQuote = useRef<string | null>(null);
  const [preparingQuote, setPreparingQuote] = useState(false);

  useEffect(() => {
    setExternalQuoteConsentAck(false);
  }, [preparedQuote?.run_id]);

  const [snippetLang, setSnippetLang] = useState<SnippetLanguage>("curl");
  const [copiedKey, setCopiedKey] = useState<string | null>(null);
  const [testing, setTesting] = useState(false);
  const [testError, setTestError] = useState<string | null>(null);
  const [lastTestRun, setLastTestRun] = useState<RunRecord | null>(null);

  const parsedEditorParams = useMemo<Record<string, unknown> | null>(() => {
    try {
      const parsed: unknown = JSON.parse(requestBody);
      if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
        return parsed as Record<string, unknown>;
      }
      return null;
    } catch {
      return null;
    }
  }, [requestBody]);

  const liveParamsForSnippets = parsedEditorParams ?? seedParams;
  const origin =
    typeof window !== "undefined" ? window.location.origin : "http://127.0.0.1:8080";

  const activeSnippet = useMemo(() => {
    if (snippetLang === "curl") {
      return buildCurlSnippet(
        origin,
        plugin.manifest,
        liveParamsForSnippets,
        activeGrant?.grant_id,
        plugin.marketplace_offer_id,
      );
    }
    if (snippetLang === "python") {
      return buildPythonSnippet(
        origin,
        plugin.manifest,
        liveParamsForSnippets,
        activeGrant?.grant_id,
        plugin.marketplace_offer_id,
      );
    }
    return buildMcpSnippet(
      plugin.manifest,
      liveParamsForSnippets,
      Boolean(activeGrant),
    );
  }, [snippetLang, origin, plugin.manifest, plugin.marketplace_offer_id, liveParamsForSnippets, activeGrant]);

  const handleCopy = (key: string, text: string) => {
    void navigator.clipboard?.writeText(text);
    setCopiedKey(key);
    window.setTimeout(() => {
      setCopiedKey((prev) => (prev === key ? null : prev));
    }, 1600);
  };

  const handleResetDefaults = () => {
    const nextText = JSON.stringify(seedParams, null, 2);
    setRequestBody(nextText);
    setTestError(null);
    setReviewed(null);
    onSyncDraftParams?.(plugin.manifest.id, seedParams);
  };

  const handlePrepareQuoteForReview = async () => {
    if (preparingQuote || testing || isConfirmingRun) return;
    if (!parsedEditorParams) {
      setTestError("Request payload must be a valid JSON object.");
      return;
    }
    setPreparingQuote(true);
    setTestError(null);
    onSyncDraftParams?.(plugin.manifest.id, parsedEditorParams);
    try {
      const q = await prepareRun({
        plugin_id: plugin.manifest.id,
        plugin_version: plugin.manifest.version,
        offer_id: selectedOffer.id,
        marketplace_offer_id: plugin.marketplace_offer_id,
        params: parsedEditorParams,
        grant_id: activeGrant?.grant_id,
        session_id: sessionId,
      });
      if (currentReviewInputs.current === reviewInputs) setReviewed({ quote: q, inputs: reviewInputs });
    } catch (err: unknown) {
      setTestError(formatErrorInline(err));
    } finally {
      setPreparingQuote(false);
    }
  };

  const handleConfirmPreparedQuote = async () => {
    if (!preparedQuote || testing || isConfirmingRun || confirmingQuote.current) return;
    confirmingQuote.current = preparedQuote.run_id;
    setTesting(true);
    setTestError(null);
    try {
      const completed = await confirmRun(preparedQuote.run_id);
      setReviewed(null);
      setLastTestRun(completed);
      await onQuoteConfirmed?.(completed);
    } catch (err: unknown) {
      setTestError(formatErrorInline(err));
    } finally {
      confirmingQuote.current = null;
      setTesting(false);
    }
  };

  const handleRunTest = async () => {
    if (!onRunApiTest || testing || isConfirmingRun) return;
    if (!parsedEditorParams) {
      setTestError("Request payload must be a valid JSON object.");
      return;
    }
    setTesting(true);
    setTestError(null);
    setLastTestRun(null);
    setReviewed(null);
    onSyncDraftParams?.(plugin.manifest.id, parsedEditorParams);
    try {
      const completed = await onRunApiTest(parsedEditorParams, selectedOffer.id);
      if (completed) {
        setLastTestRun(completed);
      }
    } catch (err: unknown) {
      setTestError(formatErrorInline(err));
    } finally {
      setTesting(false);
    }
  };

  return (
    <div className="bc-window-pane bc-api-pane">
      {/* Endpoint Overview Card */}
      <div className="bc-card">
        <div className="bc-card-header">
          <div>
            <span className="bc-card-title">Module Execution API</span>
            <p className="bc-caption" style={{ marginTop: 3 }}>
              Two-phase verifiable execution over {plugin.marketplace_offer_id ? "REST" : "REST or BioCompass MCP"}. Active grant:{" "}
              <strong>{activeGrant?.label ?? "Default"}</strong> · Cost:{" "}
              <strong>{fmtAmount(plugin.manifest.price_milli_eur)}</strong>
            </p>
          </div>
          <span className="bc-badge bc-badge-accent mono">{plugin.marketplace_offer_id ? "REST" : "REST + MCP"}</span>
        </div>

        <div className="bc-api-endpoints">
          <div className="bc-api-endpoint-row">
            <span className="bc-http-method bc-http-post">POST</span>
            <code className="mono">/api/v1/runs/prepare</code>
            <span className="bc-caption">Validate params & sign Froglet quote</span>
          </div>
          <div className="bc-api-endpoint-row">
            <span className="bc-http-method bc-http-post">POST</span>
            <code className="mono">/api/v1/runs/&#123;run_id&#125;/confirm</code>
            <span className="bc-caption">Dispatch OCI container & sign receipt</span>
          </div>
          <div className="bc-api-endpoint-row">
            <span className="bc-http-method bc-http-get">GET</span>
            <code className="mono">/api/v1/runs/&#123;run_id&#125;</code>
            <span className="bc-caption">Fetch results, artifacts & Schnorr chain</span>
          </div>
        </div>
      </div>

      {/* Input Schema Specification Table */}
      <div
        className="bc-card"
        data-testid={isFocused ? "module-api-params-table" : undefined}
      >
        <div className="bc-card-header">
          <span className="bc-card-title">Input Parameters Specification</span>
          <span className="bc-caption">
            {schemaEntries.length} parameter{schemaEntries.length === 1 ? "" : "s"}
          </span>
        </div>
        {schemaEntries.length === 0 ? (
          <div className="bc-empty-note" style={{ marginTop: 6 }}>
            This module accepts an empty parameter object <code>&#123;&#125;</code> and reads
            directly from your active governed dataset slice.
          </div>
        ) : (
          <div className="bc-api-table-wrap">
            <table className="bc-api-table">
              <thead>
                <tr>
                  <th>Parameter</th>
                  <th>Type</th>
                  <th>Required</th>
                  <th>Default</th>
                  <th>Description</th>
                </tr>
              </thead>
              <tbody>
                {schemaEntries.map(([key, spec]) => (
                  <tr key={key}>
                    <td>
                      <div className="mono bc-api-param-name">{key}</div>
                      {spec.title && spec.title !== key && (
                        <div className="bc-caption">{spec.title}</div>
                      )}
                    </td>
                    <td className="mono bc-api-type-cell">{formatSchemaType(spec)}</td>
                    <td>
                      {requiredSet.has(key) ? (
                        <span className="bc-badge bc-badge-accent">Required</span>
                      ) : (
                        <span className="bc-badge">Optional</span>
                      )}
                    </td>
                    <td className="mono bc-api-default-cell">
                      {spec.default !== undefined ? JSON.stringify(spec.default) : "—"}
                    </td>
                    <td>
                      <div>{spec.description ?? humanizeKey(key)}</div>
                      {Array.isArray(spec.enum) && spec.enum.length > 0 && (
                        <div className="bc-api-enum-list">
                          {spec.enum.map((opt) => (
                            <code key={String(opt)} className="bc-api-enum-chip">
                              {String(opt)}
                            </code>
                          ))}
                        </div>
                      )}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </div>

      {/* Interactive Request Tester ("Try It") */}
      <div
        className="bc-card"
        data-testid={isFocused ? "module-api-try-it" : undefined}
      >
        <div className="bc-card-header">
          <div>
            <span className="bc-card-title">Try It — Live API Test &amp; Froglet Offer</span>
            <p className="bc-caption" style={{ marginTop: 2 }}>
              Select an Offer tier, inspect its signed execution limits, and execute via{" "}
              <code>/api/v1/runs/prepare</code> → <code>confirm</code>.
            </p>
          </div>
          <div className="bc-api-actions">
            <select
              aria-label="Execution Offer Tier"
              data-testid={isFocused ? "offer-select" : undefined}
              className="bc-input mono"
              style={{ width: "auto", padding: "5px 9px", fontSize: 12 }}
              disabled={testing || preparingQuote || isConfirmingRun}
              value={selectedOffer.id}
              onChange={(e) => {
                setSelectedOfferId(e.target.value);
                setReviewed(null);
              }}
            >
              {effectiveOffers.map((off) => (
                <option key={off.id} value={off.id}>
                  {off.name} ({fmtAmount(off.price_milli_eur)})
                </option>
              ))}
            </select>
            <button
              type="button"
              className="bc-btn bc-btn-ghost"
              onClick={handleResetDefaults}
              disabled={testing || preparingQuote || isConfirmingRun}
            >
              Reset Defaults
            </button>
            <button
              type="button"
              data-testid={isFocused ? "review-quote-btn" : undefined}
              className="bc-btn bc-btn-secondary"
              disabled={testing || preparingQuote || isConfirmingRun || !parsedEditorParams}
              onClick={() => void handlePrepareQuoteForReview()}
            >
              <IconShield size={13} />
              <span>{preparingQuote ? "Signing Quote…" : "Review Quote & Limits"}</span>
            </button>
            <button
              type="button"
              data-testid={isFocused ? "api-run-test-btn" : undefined}
              className="bc-btn bc-btn-primary"
              disabled={testing || preparingQuote || isConfirmingRun || !parsedEditorParams || !onRunApiTest}
              onClick={() => void handleRunTest()}
            >
              {testing || isConfirmingRun ? "Executing..." : "Send Request"}
            </button>
          </div>
        </div>

        <div
          className="bc-caption mono"
          style={{
            marginBottom: 8,
            display: "flex",
            gap: 10,
            flexWrap: "wrap",
          }}
        >
          <span>Offer: {selectedOffer.name}</span>
          <span>Price: {fmtAmount(selectedOffer.price_milli_eur)}</span>
          <span>
            Max Input: {(selectedOffer.max_input_bytes / (1024 * 1024)).toFixed(1)} MiB
          </span>
          <span>
            Max Output: {(selectedOffer.max_output_bytes / (1024 * 1024)).toFixed(1)} MiB
          </span>
          <span>Max Runtime: {selectedOffer.max_runtime_sec}s</span>
          <span>Max Memory: {selectedOffer.max_memory_mb} MiB</span>
        </div>

        {preparedQuote && (
          <div
            data-testid={isFocused ? "quote-consent-card" : undefined}
            style={{
              marginBottom: 10,
              padding: "12px 14px",
              borderRadius: "var(--bc-radius-sm)",
              background: "var(--bc-bg-subtle)",
              border: "1px solid var(--bc-accent)",
              display: "grid",
              gap: 8,
            }}
          >
            <div
              style={{
                display: "flex",
                alignItems: "center",
                justifyContent: "space-between",
                gap: 10,
                flexWrap: "wrap",
              }}
            >
              <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
                <IconShield size={15} />
                <strong>
                  Signed Froglet Quote · {preparedQuote.offer_name ?? selectedOffer.name}
                </strong>
                <span className="bc-badge bc-badge-accent mono">
                  {fmtAmount(preparedQuote.estimated_cost_milli_eur)}
                </span>
              </div>
              <span className="bc-caption mono">
                workload sha256:{preparedQuote.workload_hash.slice(0, 16)}…
              </span>
            </div>

            <div
              className="mono bc-caption"
              style={{ display: "flex", gap: 12, flexWrap: "wrap" }}
            >
              <span>
                Enforced Input Limit:{" "}
                {(
                  (preparedQuote.max_input_bytes ?? selectedOffer.max_input_bytes) /
                  (1024 * 1024)
                ).toFixed(1)}{" "}
                MiB
              </span>
              <span>
                Enforced Output Limit:{" "}
                {(
                  (preparedQuote.max_output_bytes ?? selectedOffer.max_output_bytes) /
                  (1024 * 1024)
                ).toFixed(1)}{" "}
                MiB
              </span>
              <span>
                Timeout:{" "}
                {preparedQuote.max_runtime_sec ?? selectedOffer.max_runtime_sec}s
              </span>
              <span>
                Memory:{" "}
                {preparedQuote.max_memory_mb ?? selectedOffer.max_memory_mb} MiB
              </span>
              {!isLocalRun &&
                typeof preparedQuote.base_fee_milli_eur === "number" &&
                preparedQuote.base_fee_milli_eur > 0 && (
                  <span>
                    Non-refundable failed-run base fee:{" "}
                    {fmtAmount(preparedQuote.base_fee_milli_eur)}
                  </span>
                )}
            </div>

            {preparedExternalProvider && (
              <div
                data-testid="quote-external-provider-banner"
                style={{
                  padding: "8px 10px",
                  borderRadius: "var(--bc-radius-sm)",
                  background: "var(--bc-danger-soft)",
                  border: "1px solid var(--bc-danger)",
                  fontSize: 12,
                  display: "flex",
                  flexDirection: "column",
                  gap: 6,
                }}
              >
                <div style={{ fontWeight: 600 }}>
                  External Provider Node · {preparedExternalProvider.providerLabel} (
                  <span className="mono">{preparedExternalProvider.nodeId}</span>)
                </div>
                <div>{preparedExternalProvider.notice}</div>
                <label style={{ display: "flex", alignItems: "flex-start", gap: 6, cursor: "pointer" }}>
                  <input
                    type="checkbox"
                    data-testid="quote-external-provider-checkbox"
                    checked={externalQuoteConsentAck}
                    onChange={(e) => setExternalQuoteConsentAck(e.target.checked)}
                    disabled={testing}
                  />
                  <span>
                    I consent to sending my parameters/uploads to{" "}
                    <strong>{preparedExternalProvider.providerLabel}</strong>.
                  </span>
                </label>
              </div>
            )}

            {preparedQuote.execution_notice && (
              <div
                data-testid="quote-execution-notice"
                className="bc-caption"
                style={{
                  padding: "6px 8px",
                  borderRadius: "var(--bc-radius-sm)",
                  background: "var(--bc-bg-elevated, rgba(255, 193, 7, 0.08))",
                  border: "1px solid var(--bc-border)",
                }}
              >
                {preparedQuote.execution_notice}
              </div>
            )}

            <div style={{ display: "flex", gap: 8, justifyContent: "flex-end" }}>
              <button
                type="button"
                data-testid={isFocused ? "cancel-quote-run-btn" : undefined}
                className="bc-btn bc-btn-sm"
                disabled={testing}
                onClick={() => setReviewed(null)}
              >
                Cancel
              </button>
              <button
                type="button"
                data-testid={isFocused ? "confirm-quote-run-btn" : undefined}
                className="bc-btn bc-btn-primary bc-btn-sm"
                disabled={testing || (preparedExternalProvider !== null && !externalQuoteConsentAck)}
                onClick={() => void handleConfirmPreparedQuote()}
              >
                {testing ? "Executing Container…" : "Confirm & Execute"}
              </button>
            </div>
          </div>
        )}

        <textarea
          className="bc-api-json-editor mono"
          data-testid={isFocused ? "api-json-editor" : undefined}
          rows={Math.min(12, Math.max(5, requestBody.split("\n").length))}
          spellCheck={false}
          aria-label="API Request JSON Parameters"
          value={requestBody}
          onChange={(e) => {
            setReviewed(null);
            setRequestBody(e.target.value);
            setTestError(null);
            try {
              const parsed: unknown = JSON.parse(e.target.value);
              if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
                onSyncDraftParams?.(plugin.manifest.id, parsed as Record<string, unknown>);
              }
            } catch {
              // ignore partial JSON typing
            }
          }}
        />

        {!parsedEditorParams && (
          <div className="bc-api-error-note">
            Invalid JSON syntax — fix payload formatting before sending.
          </div>
        )}

        {testError && <div className="bc-api-error-note">{testError}</div>}

        {lastTestRun && (
          <div
            className="bc-api-response"
            data-testid={isFocused ? "api-test-output" : undefined}
          >
            <div className="bc-api-response-bar">
              <div style={{ display: "flex", alignItems: "center", gap: 8, flexWrap: "wrap" }}>
                <span
                  className={`bc-badge ${
                    lastTestRun.status === "succeeded"
                      ? "bc-badge-success"
                      : lastTestRun.admission?.state === "at_capacity" ? "bc-badge-accent" : "bc-badge-danger"
                  }`}
                >
                  {runStatusLabel(lastTestRun).toUpperCase()}
                </span>
                <span className="mono bc-caption">{lastTestRun.run_id}</span>
                <span className="bc-caption">
                  {lastTestRun.artifacts.length} artifact(s) ·{" "}
                  {isLocalRun ? "Local run (no charge)" : `Quoted ${formatMilliEur(lastTestRun.quote.estimated_cost_milli_eur)}`}
                </span>
              </div>
              <button
                type="button"
                className="bc-btn bc-btn-secondary"
                data-testid={isFocused ? "api-open-in-runs-btn" : undefined}
                onClick={() => onInspectRun(lastTestRun.run_id)}
              >
                <IconRuns size={13} />
                <span>Open in Runs</span>
              </button>
            </div>
            <pre className="bc-code-pre">
              {JSON.stringify(
                {
                  run_id: lastTestRun.run_id,
                  status: lastTestRun.status,
                  admission: lastTestRun.admission,
                  receipt_verified: lastTestRun.receipt_verified,
                  error_message: lastTestRun.error_message,
                  artifacts: lastTestRun.artifacts.map((a) => ({
                    name: a.name,
                    size_bytes: a.size_bytes,
                  })),
                  result_json: lastTestRun.result_json,
                },
                null,
                2,
              )}
            </pre>
          </div>
        )}
      </div>

      {/* Code Examples */}
      <div className="bc-card">
        <div className="bc-card-header">
          <span className="bc-card-title">Code Snippets</span>
          <div className="bc-api-actions">
            <div className="bc-segmented" role="tablist" aria-label="Code example language">
              {(
                [
                  { id: "curl", label: "cURL" },
                  { id: "python", label: "Python" },
                  { id: "mcp", label: "MCP Tool" },
                ] as const
              ).filter((opt) => !plugin.marketplace_offer_id || opt.id !== "mcp").map((opt) => (
                <button
                  key={opt.id}
                  type="button"
                  role="tab"
                  aria-selected={snippetLang === opt.id}
                  className="bc-segmented-btn"
                  data-active={String(snippetLang === opt.id)}
                  onClick={() => setSnippetLang(opt.id)}
                >
                  {opt.label}
                </button>
              ))}
            </div>
            <button
              type="button"
              className="bc-btn bc-btn-ghost"
              onClick={() => handleCopy(snippetLang, activeSnippet)}
              title="Copy snippet"
            >
              {copiedKey === snippetLang ? (
                <>
                  <IconCheck size={13} />
                  <span>Copied</span>
                </>
              ) : (
                <>
                  <IconCopy size={13} />
                  <span>Copy</span>
                </>
              )}
            </button>
          </div>
        </div>
        <pre className="bc-code-pre bc-api-snippet-pre">{activeSnippet}</pre>
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// 2. Runs History & Selected Run Inspector Tab
// ---------------------------------------------------------------------------

export function CapacityRequest({ run, onUpdate }: {
  run: RunRecord;
  onUpdate: (record: RunRecord) => void;
}) {
  const [canceling, setCanceling] = useState(false);
  const [error, setError] = useState<string | null>(null);
  if (run.admission?.state !== "at_capacity") return null;

  const cancel = async () => {
    setCanceling(true);
    setError(null);
    try {
      onUpdate(await cancelRun(run.run_id));
    } catch (err) {
      setError(formatErrorInline(err));
    } finally {
      setCanceling(false);
    }
  };

  return (
    <div>
      <p className="bc-caption">All workers are busy. This approved run will start when capacity is available if its quote is still valid.</p>
      <button type="button" className="bc-btn bc-btn-secondary" disabled={canceling} onClick={() => void cancel()}>
        {canceling ? "Canceling…" : "Cancel request"}
      </button>
      {error && <div role="alert" className="bc-api-error-note">{error}</div>}
    </div>
  );
}

export function ModuleRunsTab({
  plugin,
  runs,
  requestedRunId,
  isFocused,
  isConfirmingRun,
  isLocalMode = false,
  liveLogs,
  verifyReport,
  onClearRequestedRunId,
  onUpdateRun,
  onReverifyChain,
}: {
  plugin?: PluginSummary;
  runs: RunRecord[];
  requestedRunId?: string | null;
  isFocused: boolean;
  isConfirmingRun: boolean;
  isLocalMode?: boolean;
  liveLogs: string[];
  verifyReport: RunVerification | null;
  onClearRequestedRunId?: () => void;
  onUpdateRun: (run: RunRecord) => void;
  onReverifyChain: (run: RunRecord, tamperOneByte: boolean) => void;
}) {
  const isLocalRun = plugin
    ? plugin.runs_in === "local" ||
      (isLocalMode && plugin.runs_in !== "cloud" && plugin.runs_in !== "network_provider")
    : isLocalMode;
  const moduleName = plugin?.manifest.name ?? runs[0]?.plugin_id ?? "this module";
  const [selectedRunId, setSelectedRunId] = useState<string | null>(
    () => requestedRunId ?? runs[0]?.run_id ?? null,
  );
  const [selectedStageIdx, setSelectedStageIdx] = useState<number>(4);
  const prevTopRunIdRef = useRef<string | null>(runs[0]?.run_id ?? null);

  useEffect(() => {
    const newestId = runs[0]?.run_id ?? null;
    const previousNewestId = prevTopRunIdRef.current;
    prevTopRunIdRef.current = newestId;
    if (requestedRunId && runs.some((r) => r.run_id === requestedRunId)) {
      setSelectedRunId(requestedRunId);
      setSelectedStageIdx(4);
      onClearRequestedRunId?.();
    } else if (newestId && newestId !== previousNewestId && selectedRunId === previousNewestId) {
      setSelectedRunId(newestId);
      setSelectedStageIdx(4);
    }
  }, [requestedRunId, runs, selectedRunId, onClearRequestedRunId]);

  const activeRun = useMemo(() => {
    if (runs.length === 0) return null;
    if (selectedRunId) {
      const found = runs.find((r) => r.run_id === selectedRunId);
      if (found) return found;
    }
    return runs[0] ?? null;
  }, [runs, selectedRunId]);

  const isLatestSelected = Boolean(activeRun && runs[0] && activeRun.run_id === runs[0].run_id);
  const logsToDisplay =
    isConfirmingRun && isLatestSelected && liveLogs.length > 0
      ? liveLogs
      : (activeRun?.logs ?? (isConfirmingRun ? liveLogs : []));

  const emptyLogMessage = activeRun && ["succeeded", "failed", "canceled"].includes(activeRun.status)
    ? "No execution logs were recorded."
    : "Waiting for container output...";
  const hasReceipt = Boolean(activeRun?.froglet_receipt);
  const report = verifyReport?.runId === activeRun?.run_id ? verifyReport : null;
  const verified = report?.valid ?? Boolean(activeRun?.receipt_verified);
  const badgeClass = verified ? "bc-badge-success" : report ? "bc-badge-danger" : "bc-badge-neutral";
  const verificationLabel = verified ? "SCHNORR CHAIN VALID" : report ? "TAMPER DETECTED / INVALID" : "UNVERIFIED";

  if (runs.length === 0 && !isConfirmingRun) {
    return (
      <div className="bc-window-pane">
        <div className="bc-empty-note">
          No completed runs for <strong>{moduleName}</strong> yet. Use the{" "}
          <strong>App</strong> or <strong>API</strong> tab (or ask the agent) to execute this
          module and inspect run history, artifacts, and Froglet receipts.
        </div>
      </div>
    );
  }

  return (
    <div className="bc-window-pane">
      <div className="bc-results-stack">
        {/* Run History Selector */}
        {runs.length > 0 && (
          <div
            className="bc-card"
            data-testid={isFocused ? "module-runs-history" : undefined}
          >
            <div className="bc-card-header">
              <span className="bc-card-title">Run History</span>
              <span className="bc-caption">
                {runs.length} run{runs.length === 1 ? "" : "s"} · click to inspect
              </span>
            </div>
            <div
              className="bc-runs-history-list"
              role="listbox"
              aria-label={`${moduleName} run history`}
              data-testid={isFocused ? "run-history-list" : undefined}
            >
              {runs.map((run, idx) => {
                const isSelected = activeRun?.run_id === run.run_id;
                const duration = formatDuration(run.created_at, run.completed_at);
                const statusBadge =
                  run.status === "succeeded"
                    ? "bc-badge-success"
                    : run.status === "failed" || run.status === "canceled"
                      ? "bc-badge-danger"
                      : "bc-badge-accent";
                return (
                  <button
                    key={run.run_id}
                    type="button"
                    role="option"
                    aria-selected={isSelected}
                    data-active={String(isSelected)}
                    data-testid={
                      isFocused ? `run-history-item-${run.run_id}` : undefined
                    }
                    className="bc-run-history-row"
                    onClick={() => {
                      setSelectedRunId(run.run_id);
                      setSelectedStageIdx(4);
                    }}
                  >
                    <div className="bc-run-history-left">
                      <span className={`bc-badge ${statusBadge}`}>{runStatusLabel(run)}</span>
                      <span className="mono bc-run-history-id">{run.run_id}</span>
                      {idx === 0 && <span className="bc-badge">Latest</span>}
                    </div>
                    <div className="bc-run-history-meta">
                      <span>{formatRunTimestamp(run.created_at)}</span>
                      {duration && <span>· {duration}</span>}
                      <span>· {run.artifacts.length} file(s)</span>
                      <span>
                        ·{" "}
                        {isLocalRun
                          ? "Local run (no charge)"
                          : `Quoted ${formatMilliEur(run.quote.estimated_cost_milli_eur)}`}
                      </span>
                      {run.receipt_verified && (
                        <span className="bc-run-verified-dot" title="Schnorr receipt verified">
                          <IconShield size={12} />
                        </span>
                      )}
                    </div>
                  </button>
                );
              })}
            </div>
          </div>
        )}

        {/* Selected Run Summary Strip */}
        {activeRun && (
          <div
            className="bc-card bc-selected-run-card"
            data-testid={isFocused ? "selected-run-detail" : undefined}
          >
            <div className="bc-card-header">
              <div style={{ display: "flex", alignItems: "center", gap: 8, flexWrap: "wrap" }}>
                <span className="bc-card-title">Selected Run</span>
                <code className="mono bc-caption">{activeRun.run_id}</code>
              </div>
              <span className="bc-caption">
                {formatRunTimestamp(activeRun.created_at)}
                {activeRun.grant ? ` · Grant: ${activeRun.grant.label}` : ""}
              </span>
            </div>
            <CapacityRequest key={activeRun.run_id} run={activeRun} onUpdate={onUpdateRun} />
            {activeRun.error_message && (
              <div className="bc-api-error-note" style={{ marginTop: 8 }}>
                {formatErrorInline(activeRun.error_message)}
              </div>
            )}
            <details className="bc-details" style={{ marginTop: 8 }}>
              <summary>Submitted parameters ({Object.keys(activeRun.params).length})</summary>
              <pre className="bc-code-pre" style={{ marginTop: 8 }}>
                {JSON.stringify(activeRun.params, null, 2)}
              </pre>
            </details>
          </div>
        )}

        {/* Scientific Views */}
        {activeRun?.result_json && (plugin ? (
          <ScientificViews
            resultJson={activeRun.result_json}
            views={plugin.views}
            exposeTestIds={isFocused}
          />
        ) : (
          <section className="bc-card" aria-label="Result JSON">
            <div className="bc-card-title">Result JSON</div>
            <pre className="bc-code-pre" data-testid="saved-run-json">{JSON.stringify(activeRun.result_json, null, 2)}</pre>
          </section>
        ))}
        {!plugin && activeRun?.froglet_receipt && (
          <details className="bc-card">
            <summary>Saved receipt</summary>
            <pre className="bc-code-pre" data-testid="saved-run-receipt">{JSON.stringify(activeRun.froglet_receipt, null, 2)}</pre>
          </details>
        )}

        {/* Output Files */}
        {activeRun && activeRun.artifacts.length > 0 && (
          <div
            className="bc-card"
            data-testid={isFocused ? "run-artifacts" : undefined}
          >
            <div className="bc-card-header">
              <span className="bc-card-title">Output Files</span>
              <span className="bc-caption">
                {activeRun.artifacts.length} file(s)
              </span>
            </div>
            <div className="bc-artifact-list">
              {activeRun.artifacts.map((art) => {
                const encodedArtPath = art.name
                  .split("/")
                  .map(encodeURIComponent)
                  .join("/");
                const artifactUrl = `/api/v1/runs/${encodeURIComponent(activeRun.run_id)}/artifacts/${encodedArtPath}`;
                const basename = art.name.split("/").pop() || "artifact.bin";
                return (
                  <button
                    key={art.name}
                    type="button"
                    className="bc-artifact-chip"
                    onClick={() => {
                      // SEC-18: Download artifacts as inert application/octet-stream blobs
                      // instead of opening container-authored HTML/SVG inline in the Workbench origin.
                      void fetch(artifactUrl, { credentials: "include" })
                        .then((r) => {
                          if (!r.ok) throw new Error(`HTTP ${r.status}`);
                          return r.arrayBuffer();
                        })
                        .then((buf) => {
                          const blob = new Blob([buf], {
                            type: "application/octet-stream",
                          });
                          const url = URL.createObjectURL(blob);
                          const a = document.createElement("a");
                          a.href = url;
                          a.download = basename;
                          document.body.appendChild(a);
                          a.click();
                          a.remove();
                          window.setTimeout(() => URL.revokeObjectURL(url), 1000);
                        })
                        .catch(() => undefined);
                    }}
                  >
                    <span className="mono">{art.name}</span>
                    <span className="bc-caption">
                      {(art.size_bytes / 1024).toFixed(1)} KB
                    </span>
                  </button>
                );
              })}
            </div>
          </div>
        )}

        {/* Container Execution Logs */}
        <div className="bc-card">
          <div className="bc-card-header">
            <span className="bc-card-title">Container Execution Log</span>
            {isConfirmingRun && isLatestSelected && (
              <span className="bc-badge bc-badge-accent">Running</span>
            )}
          </div>
          <pre
            className="bc-log-pre"
            data-testid={isFocused ? "live-run-logs" : undefined}
          >
            {logsToDisplay.length > 0
              ? logsToDisplay.join("\n")
              : emptyLogMessage}
          </pre>
        </div>

        {/* Froglet 5-artifact Schnorr Receipt */}
        {activeRun && activeRun.froglet_chain.length > 0 && (
          <div
            className="bc-card"
            data-testid={isFocused ? "receipt-inspector" : undefined}
          >
            <div className="bc-card-header">
              <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
                <IconShield size={15} />
                <span className="bc-card-title">Verifiable Execution Receipt</span>
              </div>
              <span
                data-testid={isFocused ? "receipt-verified-badge" : undefined}
                className={`bc-badge ${hasReceipt ? badgeClass : "bc-badge-neutral"}`}
              >
                {hasReceipt ? verificationLabel : "NO RECEIPT YET"}
              </span>
            </div>

            <p className="bc-caption" style={{ marginTop: 4 }}>
              {activeRun.froglet_chain.length} signed <code>froglet/v1</code> artifacts for <code className="mono">{activeRun.run_id}</code>.
            </p>

            {hasReceipt && <div className="bc-receipt-actions">
              <button
                type="button"
                data-testid={isFocused ? "reverify-chain-btn" : undefined}
                className="bc-btn bc-btn-secondary"
                onClick={() => {
                  onReverifyChain(activeRun, false);
                }}
              >
                <IconCheck size={14} />
                <span>Verify Chain</span>
              </button>
              <button
                type="button"
                data-testid={isFocused ? "tamper-test-btn" : undefined}
                className="bc-btn bc-btn-ghost"
                onClick={() => {
                  onReverifyChain(activeRun, true);
                }}
              >
                Simulate 1-Byte Tamper
              </button>
            </div>}

            <details className="bc-details">
              <summary>Inspect signed protocol artifacts</summary>
              <div className="bc-stage-pills">
                {activeRun.froglet_chain.map((_, idx) => (
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
                  activeRun.froglet_chain[
                    Math.min(
                      selectedStageIdx,
                      activeRun.froglet_chain.length - 1,
                    )
                  ],
                  null,
                  2,
                )}
              </pre>
            </details>
          </div>
        )}

        {activeRun && <ProvenancePanel run={activeRun} />}
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// 3. About Tab (Author, Contract, Resources & External Links, SKILL.md)
// ---------------------------------------------------------------------------

export function ModuleAboutTab({
  plugin,
  isFocused,
  isLocalMode = false,
}: {
  plugin: PluginSummary;
  isFocused: boolean;
  isLocalMode?: boolean;
}) {
  const isLocalRun =
    plugin.runs_in === "local" ||
    (isLocalMode && plugin.runs_in !== "cloud" && plugin.runs_in !== "network_provider");
  const schema = plugin.manifest.inputs.schema ?? {};
  const schemaProps =
    (schema["properties"] as Record<string, SchemaPropertySpec> | undefined) ?? {};
  const requiredSet = new Set(
    Array.isArray(schema["required"])
      ? (schema["required"] as unknown[]).filter((x): x is string => typeof x === "string")
      : [],
  );
  const inputEntries = Object.entries(schemaProps);

  const resourceLinks = [
    {
      key: "documentation",
      label: "Documentation & PDF Guide",
      url: plugin.manifest.documentation_url,
      placeholder: "PDF documentation link not yet attached",
    },
    {
      key: "video",
      label: "Video Walkthrough (YouTube)",
      url: plugin.manifest.video_url,
      placeholder: "Video walkthrough link not yet attached",
    },
    {
      key: "publication",
      label: "Scientific Publication",
      url: plugin.manifest.publication_url,
      placeholder: "Publication / DOI link not yet attached",
    },
  ];

  return (
    <div className="bc-window-pane">
      {/* Header Description Card */}
      <div className="bc-card">
        <div className="bc-card-header">
          <div>
            <span className="bc-card-title">{plugin.manifest.name}</span>
            <p className="bc-pane-lead" style={{ marginTop: 6, marginBottom: 0 }}>
              {plugin.manifest.description}
            </p>
          </div>
          <span className="bc-badge">{plugin.manifest.category}</span>
        </div>
      </div>

      {/* Author & Technical Metadata Grid */}
      <div
        className="bc-meta-grid"
        style={{ marginTop: 12 }}
        data-testid={isFocused ? "module-about-meta" : undefined}
      >
        <div className="bc-meta-item">
          <span className="bc-caption">Author</span>
          <strong>{plugin.author}</strong>
        </div>
        <div className="bc-meta-item">
          <span className="bc-caption">Version</span>
          <span className="mono">v{plugin.manifest.version}</span>
        </div>
        <div className="bc-meta-item">
          <span className="bc-caption">Category</span>
          <span>{plugin.manifest.category}</span>
        </div>
        <div className="bc-meta-item">
          <span className="bc-caption">Run Cost</span>
          <span data-testid="module-run-cost">
            {isLocalRun
              ? "Local run (no charge)"
              : formatMilliEur(effectiveRunCostMilli(plugin))}
          </span>
        </div>
        <div className="bc-meta-item">
          <span className="bc-caption">Execution Target</span>
          <span>{plugin.execution_label}</span>
        </div>
        <div className="bc-meta-item">
          <span className="bc-caption">Module ID</span>
          <span className="mono">{plugin.manifest.id}</span>
        </div>
        <div className="bc-meta-item">
          <span className="bc-caption">Compute Resources</span>
          <span>
            {plugin.manifest.resources.cpus} CPU ·{" "}
            {plugin.manifest.resources.memory_mb} MB RAM
            {plugin.manifest.resources.gpu > 0
              ? ` · ${plugin.manifest.resources.gpu} GPU`
              : ""}{" "}
            · {plugin.manifest.resources.timeout_sec}s timeout
          </span>
        </div>
        <div className="bc-meta-item">
          <span className="bc-caption">Container Network</span>
          <span className="mono">
            {typeof plugin.manifest.network === "string"
              ? plugin.manifest.network
              : `allow: ${plugin.manifest.network.allow_domains.join(", ")}`}
          </span>
        </div>
        <div className="bc-meta-item" style={{ gridColumn: "1 / -1" }}>
          <span className="bc-caption">Pinned OCI Image</span>
          <span className="mono" style={{ wordBreak: "break-all" }}>
            {plugin.manifest.image}
          </span>
        </div>
      </div>

      {/* Inputs & Outputs Contract */}
      <div className="bc-card" style={{ marginTop: 14 }}>
        <div className="bc-card-header">
          <span className="bc-card-title">Inputs & Outputs Contract</span>
          <span className="bc-caption">
            {plugin.has_custom_ui ? "Custom Interactive App" : "Standard Schema Module"}
          </span>
        </div>
        <div className="bc-about-contract-grid">
          <div className="bc-about-contract-box">
            <div className="bc-caption" style={{ marginBottom: 4 }}>
              Inputs (/in/params.json + governed Parquet slice)
            </div>
            {inputEntries.length === 0 ? (
              <div className="mono bc-caption">() — cohort grant defaults only</div>
            ) : (
              <ul className="bc-about-contract-list mono">
                {inputEntries.map(([key, spec]) => (
                  <li key={key}>
                    <span>
                      {key}
                      {requiredSet.has(key) ? "*" : ""}
                    </span>
                    <span className="bc-caption">: {formatSchemaType(spec)}</span>
                  </li>
                ))}
              </ul>
            )}
          </div>
          <div className="bc-about-contract-box">
            <div className="bc-caption" style={{ marginBottom: 4 }}>
              Outputs (/out/result.json + /out/artifacts/*)
            </div>
            {plugin.views.length === 0 ? (
              <div className="mono bc-caption">JSON summary & downloadable files</div>
            ) : (
              <ul className="bc-about-contract-list mono">
                {plugin.views.map((view) => (
                  <li key={view.id}>
                    <span>{view.title}</span>
                    <span className="bc-caption"> ({view.kind})</span>
                  </li>
                ))}
              </ul>
            )}
          </div>
        </div>
      </div>

      {/* Documentation, PDF, Video & Publication Links */}
      <div
        className="bc-card"
        style={{ marginTop: 14 }}
        data-testid={isFocused ? "module-about-resources" : undefined}
      >
        <div className="bc-card-header">
          <span className="bc-card-title">Documentation & Resources</span>
          <span className="bc-caption">External references</span>
        </div>
        <div className="bc-about-resources-list">
          {resourceLinks.map((item) => (
            <div key={item.key} className="bc-about-resource-row">
              <span className="bc-about-resource-label">{item.label}</span>
              {isSafeExternalUrl(item.url) ? (
                <a
                  href={item.url}
                  target="_blank"
                  rel="noreferrer"
                  className="bc-about-resource-link"
                >
                  Open Resource ↗
                </a>
              ) : (
                <span className="bc-badge">{item.placeholder}</span>
              )}
            </div>
          ))}
        </div>
      </div>

      {/* Agent Skill Specification */}
      {plugin.skill && (
        <div className="bc-card" style={{ marginTop: 14 }}>
          <div className="bc-card-title" style={{ marginBottom: 6 }}>
            Agent Skill Specification (SKILL.md)
          </div>
          <pre className="bc-code-pre">{plugin.skill.markdown_body}</pre>
        </div>
      )}
    </div>
  );
}
