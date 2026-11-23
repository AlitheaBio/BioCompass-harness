import { useWorkspaceReadHandle } from "../workspaceReadSession";
import { useEffect, useState } from "react";
import {
  endInstalledWorkspaceSession,
  fetchWorkspacePreferences, formatErrorInline, loginWorkspaceHub, pairInstalledWorkspace,
  syncInstalledWorkspace, updateWorkspacePreferences, type WorkspacePreferences,
} from "../api";
import { InstalledWorkspaceResults } from "./InstalledWorkspaceResults";

export function WorkspaceSettings({ hub, bearer, onBearer }: {
  hub?: string | null;
  bearer?: string;
  onBearer: (value?: string) => void;
}) {
  const handle = useWorkspaceReadHandle();
  const [preferences, setPreferences] = useState<WorkspacePreferences>();
  const [syncRevision, setSyncRevision] = useState(0);
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [org, setOrg] = useState("");
  const [totp, setTotp] = useState("");
  const [error, setError] = useState("");
  const [message, setMessage] = useState("");
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    setPreferences(undefined);
    setError("");
    if (hub && !bearer) return;
    let current = true;
    void fetchWorkspacePreferences(bearer).then((value) => {
      if (current) setPreferences(value);
    }).catch((reason: unknown) => {
      if (current) setError(formatErrorInline(reason));
    });
    return () => { current = false; };
  }, [hub, bearer]);

  async function action(operation: () => Promise<void>) {
    setError("");
    setMessage("");
    setBusy(true);
    try { await operation(); }
    catch (reason: unknown) { setError(formatErrorInline(reason)); }
    finally { setBusy(false); }
  }

  async function signOut() {
    try { await endInstalledWorkspaceSession(bearer); }
    finally { onBearer(undefined); }
  }

  if (hub && !bearer) {
    return (
      <div className="bc-settings-stack">
        {handle && <>
          <InstalledWorkspaceResults key={handle} handle={handle} />
          <button type="button" className="bc-btn bc-btn-secondary" disabled={busy}
            onClick={() => void action(signOut)}>Sign out of cloud account</button>
        </>}
        <form className="bc-card bc-settings-stack" onSubmit={(event) => {
          event.preventDefault();
          void action(async () => {
            const token = await loginWorkspaceHub({
              email, password, ...(org && { org_id: org }), ...(totp && { totp_code: totp }),
            });
            setPassword("");
            setTotp("");
            onBearer(token);
          });
        }}>
          <div className="bc-card-title">Link a cloud account</div>
          <p className="bc-caption">Use your cloud workspace account to send results to this installation.</p>
          <label className="bc-field">Email
            <input className="bc-input" type="email" autoComplete="username" value={email}
              onChange={(event) => setEmail(event.target.value)} required />
          </label>
          <label className="bc-field">Password
            <input className="bc-input" type="password" autoComplete="current-password" value={password}
              onChange={(event) => setPassword(event.target.value)} required />
          </label>
          <label className="bc-field">Organization ID, if needed
            <input className="bc-input" value={org} onChange={(event) => setOrg(event.target.value)} />
          </label>
          <label className="bc-field">Authenticator code, if enabled
            <input className="bc-input" inputMode="numeric" autoComplete="one-time-code" value={totp}
              onChange={(event) => setTotp(event.target.value)} />
          </label>
          <button className="bc-btn bc-btn-primary" type="submit" disabled={busy}>Sign in</button>
          {error && <p role="alert">{error}</p>}
        </form>
      </div>
    );
  }

  const linked = Boolean(preferences?.local_workspace_id);
  const dual = preferences?.available_locations.length === 2;
  return (
    <div className="bc-settings-stack">
      <div className="bc-card bc-settings-stack">
        {hub && bearer && (
          <div className="bc-card-header">
            {!linked && <button type="button" className="bc-btn bc-btn-primary" disabled={!preferences || busy}
              onClick={() => void action(async () => {
                setPreferences(await pairInstalledWorkspace(bearer));
                setMessage("Installation linked.");
              })}>Link this installation</button>}
            <button type="button" className="bc-btn bc-btn-secondary"
              onClick={() => void action(signOut)}>Sign out of cloud account</button>
          </div>
        )}
        {preferences && (
          <>
            <label className="bc-field">
              <span className="bc-field-label">Default workspace</span>
              <select className="bc-input" aria-label="Default workspace" value={preferences.default_location}
                disabled={!dual || busy} onChange={(event) => {
                  const default_location = event.target.value === "local" ? "local" : "cloud";
                  void action(async () => {
                    setPreferences(await updateWorkspacePreferences({
                      default_location, sync_enabled: preferences.sync_enabled,
                    }, bearer));
                    setMessage("Workspace preference saved.");
                  });
                }}>
                {preferences.available_locations.map((location) => (
                  <option key={location} value={location}>{location === "local" ? "Local installation" : "Cloud workspace"}</option>
                ))}
              </select>
            </label>
            {dual && (
              <>
                <label className="bc-field"><span>
                  <input type="checkbox" checked={preferences.sync_enabled} disabled={busy}
                    onChange={(event) => {
                      const sync_enabled = event.target.checked;
                      void action(async () => {
                        setPreferences(await updateWorkspacePreferences({
                          default_location: preferences.default_location, sync_enabled,
                        }, bearer));
                        setMessage("Workspace preference saved.");
                      });
                    }} /> Sync results to both workspaces
                </span></label>
                <p className="bc-caption">
                  {preferences.sync_enabled
                    ? `The installed workbench checks for results every ${preferences.sync_interval_seconds} seconds while signed in.`
                    : "Only the default workspace receives new results."}
                </p>
                <p className="bc-caption">Pending deliveries have no automatic expiry. Pickup completes after the installation verifies the files.</p>
                <p className="bc-caption">Last successful sync {preferences.last_synced_at
                  ? new Date(preferences.last_synced_at).toLocaleString() : "never"}</p>
                {bearer && <button type="button" className="bc-btn bc-btn-secondary" disabled={busy}
                  onClick={() => void action(async () => {
                    const result = await syncInstalledWorkspace(bearer);
                    setPreferences(await fetchWorkspacePreferences(bearer));
                    setSyncRevision((revision) => revision + 1);
                    const failed = result.errors[0];
                    if (failed) throw new Error(failed.error);
                    setMessage(`${result.delivered} new results saved. ${result.pending} pending.`);
                  })}>Sync now</button>}
              </>
            )}
          </>
        )}
        {error && <p role="alert">{error}</p>}
        {message && <p role="status">{message}</p>}
      </div>
      {hub && bearer && <InstalledWorkspaceResults key={handle ?? bearer} bearer={bearer} handle={handle} refresh={syncRevision} />}
    </div>
  );
}
