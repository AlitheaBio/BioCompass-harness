import { useEffect, useRef } from "react";
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
  useEffect(() => {
    const element = dialog.current;
    if (!element) return;
    element.showModal();
    dismiss.current?.focus();
    return () => element.close();
  }, [quote.run_id]);

  return (
    <dialog ref={dialog} aria-labelledby="paid-run-title" data-testid="paid-run-consent"
      className="bc-card" style={{ margin: "auto", width: "min(440px, 92vw)", padding: 22 }}
      onKeyDown={(event) => event.stopPropagation()}
      onCancel={(event) => { event.preventDefault(); if (!busy) onDismiss(quote.run_id); }}>
      <h2 id="paid-run-title" className="bc-card-title">Approve this run?</h2>
      <p>{quote.plugin_id} · v{quote.plugin_version}</p>
      <p>{quote.offer_name ?? quote.offer_id ?? "Run"} · <strong>{formatMilliAct(quote.estimated_cost_act)}</strong></p>
      <p className="bc-caption mono">Run {quote.run_id}</p>
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
          disabled={busy} onClick={(e) => { if (e.detail <= 1) onApprove(quote.run_id); }}>
          {busy ? "Confirming…" : `Approve ${formatMilliAct(quote.estimated_cost_act)}`}
        </button>
      </div>
    </dialog>
  );
}
