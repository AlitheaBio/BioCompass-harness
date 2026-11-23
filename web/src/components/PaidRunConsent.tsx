import { useEffect, useRef, useState } from "react";
import { getExternalProviderConsentInfo } from "../milestoneBPolicy";
import { formatMilliAct, type RunQuote } from "../types";

export function PaidRunConsent({ quote, params, busy, error, onApprove, onDismiss }: {
  quote: RunQuote;
  params: Record<string, unknown> | null;
  busy: boolean;
  error?: string;
  onApprove: (runId: string) => void;
  onDismiss: (runId: string) => void;
}) {
  const dialog = useRef<HTMLDialogElement>(null);
  const dismiss = useRef<HTMLButtonElement>(null);
  const externalProvider = getExternalProviderConsentInfo(quote);
  const [externalAck, setExternalAck] = useState(false);

  useEffect(() => {
    setExternalAck(false);
    const element = dialog.current;
    if (!element) return;
    element.showModal();
    dismiss.current?.focus();
    return () => element.close();
  }, [quote.run_id]);

  const approveDisabled = busy || (externalProvider !== null && !externalAck);

  return (
    <dialog ref={dialog} aria-labelledby="paid-run-title" data-testid="paid-run-consent"
      className="bc-card" style={{ margin: "auto", width: "min(480px, 92vw)", padding: 22 }}
      onKeyDown={(event) => event.stopPropagation()}
      onCancel={(event) => { event.preventDefault(); if (!busy) onDismiss(quote.run_id); }}>
      <h2 id="paid-run-title" className="bc-card-title">
        {externalProvider ? "External Provider Execution Consent" : "Approve this run?"}
      </h2>
      <p>{quote.plugin_id} · v{quote.plugin_version}</p>
      <p>{quote.offer_name ?? quote.offer_id ?? "Run"} · <strong>{formatMilliAct(quote.estimated_cost_act)}</strong></p>
      <p className="bc-caption mono">Run {quote.run_id}</p>
      {externalProvider && (
        <div
          data-testid="external-provider-consent-banner"
          role="region"
          aria-label="External provider data boundary notice"
          style={{
            marginTop: 12,
            padding: "10px 12px",
            borderRadius: "var(--bc-radius-sm)",
            background: "var(--bc-danger-soft)",
            border: "1px solid var(--bc-danger)",
            fontSize: 12.5,
            lineHeight: 1.5,
          }}
        >
          <div style={{ fontWeight: 600, marginBottom: 4 }}>
            External Provider Node · {externalProvider.providerLabel} (
            <span className="mono">{externalProvider.nodeId}</span>)
          </div>
          <p style={{ margin: 0 }}>{externalProvider.notice}</p>
          <p className="bc-caption" style={{ marginTop: 6, marginBottom: 8 }}>
            Only your run parameters and uploads leave Alithea infrastructure. Protected or{" "}
            <code className="mono">compute_only</code> Alithea catalogs are never sent to external
            provider nodes.
          </p>
          <label
            style={{
              display: "flex",
              alignItems: "flex-start",
              gap: 8,
              cursor: "pointer",
              fontWeight: 500,
            }}
          >
            <input
              type="checkbox"
              data-testid="external-provider-consent-checkbox"
              checked={externalAck}
              onChange={(e) => setExternalAck(e.target.checked)}
              disabled={busy}
            />
            <span>
              I consent to sending my inputs to external provider{" "}
              <strong>{externalProvider.providerLabel}</strong> on node{" "}
              <code className="mono">{externalProvider.nodeId}</code>.
            </span>
          </label>
        </div>
      )}
      {quote.execution_notice && (
        <p data-testid="paid-run-execution-notice" className="bc-caption" style={{ marginTop: 8 }}>
          {quote.execution_notice}
        </p>
      )}
      <details style={{ marginBlock: 12 }}>
        <summary>Run details</summary>
        {quote.dataset_grant_summary && <p>Dataset access · {quote.dataset_grant_summary}</p>}
        {params ? <pre style={{ overflow: "auto", maxHeight: "30vh" }}>
          {JSON.stringify(Object.fromEntries(Object.entries(params).filter(([name]) => name !== "_ui_state")), null, 2)}
        </pre> : <p>Run inputs are unavailable.</p>}
      </details>
      {error && <p role="alert">{error}</p>}
      <div style={{ display: "flex", gap: 8, justifyContent: "end" }}>
        <button ref={dismiss} type="button" className="bc-btn bc-btn-secondary" data-testid="decline-paid-run"
          disabled={busy} onClick={(e) => { if (e.detail <= 1) onDismiss(quote.run_id); }}>Dismiss quote</button>
        <button type="button" className="bc-btn bc-btn-primary" data-testid="approve-paid-run"
          disabled={approveDisabled} onClick={(e) => { if (e.detail <= 1) onApprove(quote.run_id); }}>
          {busy ? "Confirming…" : `Approve ${formatMilliAct(quote.estimated_cost_act)}`}
        </button>
      </div>
    </dialog>
  );
}
