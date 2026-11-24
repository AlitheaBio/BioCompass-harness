import { useEffect, useState } from "react";
import {
  ApiClientError, downloadInstalledWorkspaceResult, fetchInstalledWorkspaceRuns,
  formatErrorInline, type InstalledResultsPage,
} from "../api";
import { workspaceReadHandle } from "../workspaceReadSession";

function readError(reason: unknown): string {
  if (reason instanceof ApiClientError && (reason.status === 401 || reason.status === 403)) {
    return "Sign in again under Settings → Workspace to open your saved results.";
  }
  return formatErrorInline(reason);
}

export function InstalledWorkspaceResults({ bearer, handle, refresh = 0, search = "" }: {
  bearer?: string;
  handle: string | null;
  refresh?: number;
  search?: string;
}) {
  const [page, setPage] = useState<InstalledResultsPage>();
  const [opened, setOpened] = useState<{ id: string; text: string } | null>(null);
  const [error, setError] = useState("");
  const [message, setMessage] = useState("");
  const [busy, setBusy] = useState(false);
  const [reload, setReload] = useState(0);

  useEffect(() => {
    let current = true;
    let timer: number | undefined;
    let deadline = Infinity;
    function checkExpiry() {
      if (current && Date.now() >= deadline) {
        setPage(undefined);
        setOpened(null);
        setError("Your workspace session expired. Sign in to open saved results.");
      }
    }
    setPage(undefined);
    setOpened(null);
    setError("");
    setMessage("");
    void fetchInstalledWorkspaceRuns(bearer, handle).then((saved) => {
      if (!current) return;
      if (saved.read_session_expires_at) deadline = Date.parse(saved.read_session_expires_at);
      if (Date.now() >= deadline) { checkExpiry(); return; }
      setPage(saved);
      if (Number.isFinite(deadline)) timer = window.setTimeout(checkExpiry, deadline - Date.now());
    }).catch((reason: unknown) => {
      if (current) setError(readError(reason));
    });
    document.addEventListener("visibilitychange", checkExpiry);
    window.addEventListener("pageshow", checkExpiry);
    return () => {
      current = false;
      window.clearTimeout(timer);
      document.removeEventListener("visibilitychange", checkExpiry);
      window.removeEventListener("pageshow", checkExpiry);
    };
  }, [bearer, handle, refresh, reload]);

  async function perform(action: () => Promise<void>) {
    setError("");
    setMessage("");
    setBusy(true);
    try { await action(); }
    catch (reason: unknown) {
      if (reason instanceof ApiClientError && (reason.status === 401 || reason.status === 403)) {
        setPage(undefined);
        setOpened(null);
      }
      setError(readError(reason));
    } finally { setBusy(false); }
  }

  function canPresentFile() {
    return workspaceReadHandle() === handle
      && (!page?.read_session_expires_at || Date.now() < Date.parse(page.read_session_expires_at));
  }

  async function openResult(id: string) {
    if (opened?.id === id) { setOpened(null); return; }
    const bytes = await downloadInstalledWorkspaceResult(id, bearer, handle);
    const text = await bytes.text();
    if (canPresentFile()) setOpened({ id, text });
  }

  async function download(id: string, artifact?: string) {
    const bytes = await downloadInstalledWorkspaceResult(id, bearer, handle, artifact);
    if (!canPresentFile()) return;
    const url = URL.createObjectURL(bytes);
    const link = document.createElement("a");
    link.href = url;
    link.download = artifact?.split("/").pop() || `${id}.json`;
    link.click();
    window.setTimeout(() => URL.revokeObjectURL(url), 1000);
  }

  const query = search.trim().toLowerCase();
  const matches = (page?.runs ?? []).filter((run) =>
    [run.run_id, run.plugin_id, run.plugin_version, run.status].some((value) => value.toLowerCase().includes(query)));
  if (!matches.length && !error) return null;
  return (
    <section className="bc-card bc-settings-stack" aria-label="Results saved on this installation">
      <div className="bc-card-title">Results saved on this installation</div>
      {page?.offline && <p className="bc-caption" role="status">Home hub offline. Opening saved files from this installation.</p>}
      {matches.map((run) => (
        <div key={run.run_id} className="bc-settings-stack">
          <div className="bc-card-header">
            <span>{run.plugin_id} <code>{run.run_id}</code></span>
            <button type="button" className="bc-btn bc-btn-secondary" disabled={busy}
              aria-expanded={opened?.id === run.run_id} onClick={() => void perform(() => openResult(run.run_id))}>
              {opened?.id === run.run_id ? "Close result" : "Open result"}
            </button>
            <button type="button" className="bc-btn bc-btn-secondary" disabled={busy}
              onClick={() => void perform(() => download(run.run_id))}>Download result</button>
            {run.artifacts.map(({ name }) =>
              <button type="button" key={name} className="bc-btn bc-btn-secondary" disabled={busy}
                onClick={() => void perform(() => download(run.run_id, name))}>Download {name}</button>)}
          </div>
          {opened?.id === run.run_id && <pre className="bc-code-pre" aria-label="Saved result contents">{opened.text}</pre>}
        </div>
      ))}
      {error && <div><p role="alert">{error}</p>
        <button type="button" className="bc-btn bc-btn-secondary" disabled={busy}
          onClick={() => setReload((value) => value + 1)}>Refresh saved results</button></div>}
      {message && <p role="status">{message}</p>}
    </section>
  );
}
