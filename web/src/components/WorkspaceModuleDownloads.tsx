import { useState, type FormEvent } from "react";
import {
  ApiClientError,
  fetchModuleDownloads,
  formatErrorInline,
  installModuleDownload,
  loginWorkspaceHub, fetchWorkspacePreferences,
} from "../api";
import type { ModuleDownload, PluginSummary } from "../types";

interface Props {
  hub: string;
  bearer?: string;
  onBearer: (value: string | undefined) => void;
  onInstalled: (plugin: PluginSummary) => void;
}

export function WorkspaceModuleDownloads({ hub, bearer, onBearer, onInstalled }: Props) {
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [signIn, setSignIn] = useState(false);
  const [error, setError] = useState("");
  const [downloads, setDownloads] = useState<ModuleDownload[]>([]);
  const [moduleId, setModuleId] = useState("");
  const [version, setVersion] = useState("");
  const modules = new Map(downloads.map((release) => [release.plugin_id, release.name]));
  const versions = downloads.filter((release) => release.plugin_id === moduleId);
  const selected = versions.find((release) => release.version === version);

  function failed(reason: unknown) {
    if (reason instanceof ApiClientError && reason.status === 401) {
      setSignIn(true);
      setDownloads([]);
      onBearer(undefined);
    }
    setError(formatErrorInline(reason));
  }

  async function load(token?: string) {
    const releases = await fetchModuleDownloads(token);
    setDownloads(releases);
    setModuleId(releases[0]?.plugin_id ?? "");
    setVersion(releases[0]?.version ?? "");
    setSignIn(false);
  }

  async function browse() {
    if (open) { setOpen(false); return; }
    setOpen(true);
    setBusy(true);
    setError("");
    try {
      await load(bearer);
    } catch (reason) {
      if (reason instanceof ApiClientError && reason.status === 401) {
        setSignIn(true);
        onBearer(undefined);
        setDownloads([]);
      } else { failed(reason); }
    } finally { setBusy(false); }
  }

  async function login(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const form = event.currentTarget;
    const data = new FormData(form);
    setBusy(true);
    setError("");
    try {
      const token = await loginWorkspaceHub({
        email: String(data.get("email")).trim(),
        password: String(data.get("password")),
        org_id: String(data.get("org") ?? "").trim() || undefined,
        totp_code: String(data.get("totp") ?? "").trim() || undefined,
      });
      form.reset();
      onBearer(token);
      // A marketplace sign-in can also belong to the scientist already linked to this installation.
      void fetchWorkspacePreferences(token).catch(() => undefined);
      await load(token);
    } catch (reason) { failed(reason); }
    finally { setBusy(false); }
  }

  async function install() {
    if (!selected) return;
    setBusy(true);
    setError("");
    try {
      const plugin = await installModuleDownload(selected, bearer);
      onInstalled(plugin);
    } catch (reason) { failed(reason); }
    finally { setBusy(false); }
  }

  return (
    <section aria-label="Module marketplace" style={{ marginBottom: 16 }}>
      <button type="button" className="bc-btn bc-btn-secondary" data-testid="workspace-marketplace"
        aria-expanded={open} disabled={busy} onClick={() => void browse()}>Marketplace</button>
      {open && (
        <div className="bc-card" style={{ display: "grid", gap: 12, padding: 16, marginTop: 8 }}>
          <div className="bc-caption">Downloads from {hub}</div>
          {error && <div role="alert">{error}</div>}
          {signIn ? (
            <form onSubmit={(event) => void login(event)} style={{ display: "grid", gap: 8, maxWidth: 420 }}>
              <label>Email<input className="bc-input" name="email" type="email" autoComplete="username" required /></label>
              <label>Password<input className="bc-input" name="password" type="password" autoComplete="current-password" required /></label>
              <label>Organization (optional)<input className="bc-input" name="org" /></label>
              <label>Authentication code (optional)<input className="bc-input" name="totp" inputMode="numeric" autoComplete="one-time-code" /></label>
              <button type="submit" disabled={busy} className="bc-btn bc-btn-primary">Sign in</button>
            </form>
          ) : selected ? (
            <div style={{ display: "flex", gap: 12, alignItems: "end", flexWrap: "wrap" }}>
              <label>Module<select className="bc-select" data-testid="download-module" disabled={busy} value={moduleId}
                onChange={(event) => {
                  const id = event.target.value;
                  setModuleId(id);
                  setVersion(downloads.find((release) => release.plugin_id === id)?.version ?? "");
                }}>
                {[...modules].map(([id, name]) => <option key={id} value={id}>{name} ({id})</option>)}
              </select></label>
              <label>Version<select className="bc-select" data-testid="download-version" disabled={busy} value={version}
                onChange={(event) => setVersion(event.target.value)}>
                {versions.map((release) => <option key={release.version}>{release.version}</option>)}
              </select></label>
              <button type="button" className="bc-btn bc-btn-primary" data-testid="install-release"
                disabled={busy} onClick={() => void install()}>{busy ? "Installing…" : "Install and open"}</button>
            </div>
          ) : !error && <div>{busy ? "Loading downloads…" : "Your organization has no downloadable module releases."}</div>}
        </div>
      )}
    </section>
  );
}
