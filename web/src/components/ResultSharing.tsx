import { useEffect, useRef, useState, type FormEvent } from "react";
import {
  createResultShare, fetchResultShares, formatErrorInline, downloadResultShare, revokeResultShare,
} from "../api";
import { safeLaunchUrl } from "../safeUrl";
import type { ResultShare, RunRecord } from "../types";

export function ShareResultButton({ run }: { run: RunRecord }) {
  const [open, setOpen] = useState(false);
  return (
    <>
      <button type="button" className="bc-btn bc-btn-secondary" data-testid={`share-result-${run.run_id}`}
        onClick={() => setOpen(true)}>Share file</button>
      {open && <ShareFileDialog run={run} onClose={() => setOpen(false)} />}
    </>
  );
}

function ShareFileDialog({ run, onClose }: { run: RunRecord; onClose: () => void }) {
  const dialog = useRef<HTMLDialogElement>(null);
  const files = [...new Set([
    "result.json", ...run.artifacts.map(artifact => artifact.name),
  ])];
  useEffect(() => { dialog.current?.showModal(); }, []);
  const [recipient, setRecipient] = useState("");
  const [file, setFile] = useState(files[0] ?? "");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [shared, setShared] = useState<ResultShare | null>(null);

  async function submit(event: FormEvent) {
    event.preventDefault();
    if (!recipient.trim() || !files.includes(file) || busy) return;
    setError("");
    setBusy(true);
    try {
      setShared(await createResultShare(run.run_id, file, recipient.trim()));
    } catch (err) {
      setError(formatErrorInline(err));
    } finally {
      setBusy(false);
    }
  }

  return (
      <dialog ref={dialog} aria-label="Share a result file" className="bc-remove-release" onClose={onClose}>
        <h2>Share a result file</h2>
        {shared ? (
          <>
            <p role="status">{shared.recipient_org_id} can download {shared.filename} until {new Date(shared.expires_at).toLocaleString()}.</p>
            <button type="button" className="bc-btn bc-btn-primary" onClick={() => dialog.current?.close()}>Done</button>
          </>
        ) : (
          <form onSubmit={submit} className="bc-settings-stack">
            <label className="bc-field">File
              <select className="bc-input" required value={file} onChange={e => setFile(e.target.value)} disabled={busy}>
                {files.map(name => <option key={name} value={name}>{name}</option>)}
              </select>
            </label>
            <label className="bc-field">Recipient organization ID
              <input className="bc-input mono" required value={recipient} onChange={e => setRecipient(e.target.value)}
                autoComplete="off" disabled={busy} />
            </label>
            {error && <p role="alert">{error}</p>}
            <div className="bc-module-version-actions">
              <button type="button" className="bc-btn bc-btn-secondary" disabled={busy}
                onClick={() => dialog.current?.close()}>Cancel</button>
              <button className="bc-btn bc-btn-primary" type="submit" disabled={busy || !recipient.trim()}>
                {busy ? "Sharing…" : "Share for 7 days"}
              </button>
            </div>
          </form>
        )}
      </dialog>
  );
}

export function SharedResultFiles({ orgId, canPublish, open, onOpen, onClose }: {
  orgId: string; canPublish: boolean; open: boolean; onOpen: () => void; onClose: () => void;
}) {
  return (
    <>
      <button type="button" className="bc-btn bc-btn-secondary" onClick={onOpen}>Shared files</button>
      {open && <SharedFilesDialog orgId={orgId} canPublish={canPublish} onClose={onClose} />}
    </>
  );
}

function SharedFilesDialog({ orgId, canPublish, onClose }: { orgId: string; canPublish: boolean; onClose: () => void }) {
  const dialog = useRef<HTMLDialogElement>(null);
  const [listing, setListing] = useState<Awaited<ReturnType<typeof fetchResultShares>> | null>(null);
  const shares = listing?.shares;
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    let active = true;
    dialog.current?.showModal();
    void fetchResultShares().then(result => { if (active) setListing(result); })
      .catch(err => { if (active) setError(formatErrorInline(err)); });
    return () => { active = false; };
  }, []);

  async function download(share: ResultShare) {
    setBusy(true);
    setError("");
    try {
      const verifiedFile = await downloadResultShare(share);
      if (!dialog.current?.open) return;
      const url = URL.createObjectURL(verifiedFile);
      const link = document.createElement("a");
      link.href = url;
      link.download = share.filename;
      document.body.appendChild(link);
      link.click();
      link.remove();
      window.setTimeout(() => URL.revokeObjectURL(url), 1000);
    } catch (err) {
      setError(formatErrorInline(err));
    } finally {
      setBusy(false);
    }
  }

  async function revoke(id: string) {
    setBusy(true);
    setError("");
    try {
      await revokeResultShare(id);
      setListing(current => current && { ...current, shares: current.shares.filter(share => share.share_id !== id) });
    } catch (err) {
      setError(formatErrorInline(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <dialog ref={dialog} aria-label="Shared result files" className="bc-remove-release bc-saved-run-dialog" onClose={onClose}>
      <div className="bc-card-header">
        <h2>Shared result files</h2>
        <button type="button" className="bc-btn bc-btn-secondary" onClick={() => dialog.current?.close()}>Close</button>
      </div>
      {error && <p role="alert">{error}</p>}
      {!error && !shares && <p role="status">Loading files…</p>}
      {shares?.length === 0 && <p>No files are currently shared with or by this organization on this workspace.</p>}
      {listing?.providers?.map(provider => {
        const safeHref = safeLaunchUrl(provider.launch_url);
        if (!safeHref) return null;
        return (
          <p key={provider.launch_url}>
            <a className="bc-btn bc-btn-secondary" href={safeHref} rel="noopener noreferrer">Open shared files on {provider.label}</a>
          </p>
        );
      })}
      {shares?.map(share => (
        <article key={share.share_id} data-testid={`shared-file-${share.share_id}`} className="bc-card">
          <strong>{share.filename}</strong>
          <p>{share.owner_org_id === orgId ? `Shared with ${share.recipient_org_id}` : `From ${share.owner_org_id}`}
            {" · "}{share.byte_length.toLocaleString()} bytes · Expires {new Date(share.expires_at).toLocaleString()}</p>
          <details>
            <summary>File checksum</summary>
            <code style={{ overflowWrap: "anywhere" }}>SHA-256 {share.sha256}</code>
          </details>
          <div className="bc-module-version-actions">
            {share.recipient_org_id === orgId && (
              <button type="button" className="bc-btn bc-btn-primary" disabled={busy}
                onClick={() => void download(share)}>Download</button>
            )}
            {share.owner_org_id === orgId && canPublish && (
              <button type="button" className="bc-btn bc-btn-secondary" disabled={busy}
                onClick={() => void revoke(share.share_id)}>Revoke access</button>
            )}
          </div>
        </article>
      ))}
    </dialog>
  );
}
