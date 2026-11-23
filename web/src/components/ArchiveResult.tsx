import { useEffect, useRef, useState } from "react";
import { archiveWorkspaceResult, fetchWorkspaceArchiveTargets, formatErrorInline, type WorkspaceArchive } from "../api";
import { safeLaunchUrl } from "../safeUrl";

export function ArchiveResult({ runId }: { runId: string }) {
  const [open, setOpen] = useState(false);
  const [receipt, setReceipt] = useState<WorkspaceArchive>();
  const [targets, setTargets] = useState<{ id: string; label: string }[]>();
  const [target, setTarget] = useState("");
  const [error, setError] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const dialog = useRef<HTMLDialogElement>(null);

  useEffect(() => {
    if (!open) return;
    dialog.current?.showModal();
    let current = true;
    void fetchWorkspaceArchiveTargets().then((items) => {
      if (current) setTargets(items);
    }).catch((reason: unknown) => { if (current) setError(formatErrorInline(reason)); });
    return () => { current = false; };
  }, [open]);

  if (!open) return <button type="button" className="bc-btn bc-btn-secondary"
    onClick={() => setOpen(true)}>Archive result</button>;

  return (
    <dialog ref={dialog} className="bc-remove-release bc-saved-run-dialog"
      aria-label="Archive result" onClose={() => setOpen(false)}>
      <h2>Archive result</h2>
      <div className="bc-settings-stack">
      {receipt ? (() => {
        const safeSignIn = receipt.sign_in_url ? safeLaunchUrl(receipt.sign_in_url) : null;
        const safeDownload =
          (receipt.download_url ? safeLaunchUrl(receipt.download_url) : null) ??
          `/api/v1/workspace/archives/${encodeURIComponent(runId)}`;
        return (
          <>
            <p role="status">Archived result · SHA-256 <code style={{ overflowWrap: "anywhere" }}>{receipt.sha256}</code></p>
            {safeSignIn && <a href={safeSignIn} target="_blank" rel="noopener noreferrer">Sign in to archive destination</a>}
            <a href={safeDownload}
              target={receipt.download_url && safeDownload !== `/api/v1/workspace/archives/${encodeURIComponent(runId)}` ? "_blank" : undefined}
              rel="noopener noreferrer">Download archived result</a>
          </>
        );
      })() : (
        <>
          {targets && targets.length > 0 && <label className="bc-field">Archive destination
            <select className="bc-input" value={target} onChange={(event) => setTarget(event.target.value)}>
              <option value="">This workspace</option>
              {targets.map((item) => <option key={item.id} value={item.id}>{item.label}</option>)}
            </select>
          </label>}
          <button type="button" className="bc-btn bc-btn-secondary" disabled={submitting || !targets}
            onClick={() => {
              setError("");
              setSubmitting(true);
              void archiveWorkspaceResult(runId, undefined, target || undefined).then(setReceipt)
                .catch((reason: unknown) => setError(formatErrorInline(reason)))
                .finally(() => setSubmitting(false));
            }}>Save archive</button>
        </>
      )}
      <button type="button" className="bc-btn bc-btn-ghost" onClick={() => dialog.current?.close()}>Close archive</button>
      {error && <p role="alert">{error}</p>}
      </div>
    </dialog>
  );
}
