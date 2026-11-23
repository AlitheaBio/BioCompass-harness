import { useState, type FormEvent } from "react";
import { ApiClientError, formatErrorInline, importLocalDataset, type LocalImportResult, type UploadImporter } from "../api";
import { localOwnerSession, ownerLinkHelp } from "../localOwner";

export function LocalDatasetImport({ importer, onClose, onPublished }: {
  importer: UploadImporter;
  onClose: () => void;
  onPublished: (id: string) => void;
}) {
  const [name, setName] = useState("");
  const [file, setFile] = useState<File | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [result, setResult] = useState<LocalImportResult | null>(null);
  const owner = Boolean(localOwnerSession());

  async function submit(event: FormEvent) {
    event.preventDefault();
    if (!file) return;
    setError("");
    setBusy(true);
    try {
      setResult(await importLocalDataset(importer, file, name.trim()));
    } catch (cause) {
      setError(cause instanceof ApiClientError && cause.status === 401 ? ownerLinkHelp : formatErrorInline(cause));
    } finally {
      setBusy(false);
    }
  }

  return <div className="bc-modal-backdrop">
    <form className="bc-modal" role="dialog" aria-label={importer.name} onSubmit={submit}>
      <div className="bc-drawer-header"><h2 className="bc-drawer-title">{importer.name}</h2></div>
      <div className="bc-drawer-body" style={{ display: "grid", gap: 12 }}>
        {!owner && <p role="status">{ownerLinkHelp}</p>}
        <div>
          <label className="bc-filter-label" htmlFor="local-import-name">Dataset name</label>
          <input id="local-import-name" className="bc-input" value={name} required pattern="[a-zA-Z0-9_-]{1,80}"
            title="Use letters, numbers, hyphens or underscores." disabled={!owner || busy || Boolean(result?.catalog)}
            onChange={event => setName(event.target.value)} />
        </div>
        <div>
          <label className="bc-filter-label" htmlFor="local-import-source">Source file</label>
          <input id="local-import-source" type="file" className="bc-input" required
            accept={importer.accept.includes("*") ? undefined : importer.accept.join(",")}
            disabled={!owner || busy || Boolean(result?.catalog)}
            onChange={event => { setFile(event.target.files?.[0] ?? null); setResult(null); }} />
        </div>
        {busy && <p role="status">Validating and importing into this workspace…</p>}
        {error && <p role="alert">{error}</p>}
        {result?.catalog && <p role="status">Published private version {result.catalog.version}.</p>}
        {result && !result.catalog && <div role="alert">
          <p>Correct the source before publishing.</p>
          {result.report.diagnostics?.map((item, index) => <p key={index}>{item.message ?? item.code}</p>)}
        </div>}
      </div>
      <div className="bc-drawer-footer">
        <button type="button" className="bc-btn" disabled={busy} onClick={onClose}>Close</button>
        {result?.catalog
          ? <button className="bc-btn bc-btn-primary" type="button" onClick={() => onPublished(result.catalog!.id)}>View dataset</button>
          : <button className="bc-btn bc-btn-primary" type="submit" disabled={!owner || busy || !file || !name.trim()}>Import into workspace</button>}
      </div>
    </form>
  </div>;
}
