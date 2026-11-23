import { useId, useRef, useState } from "react";
import { ApiClientError, formatErrorInline, removeModuleRelease, restoreModuleSource } from "../api";
import type { PluginSummary } from "../types";

interface Props {
  plugin: PluginSummary;
  bearer?: string;
  onRemoved: (id: string, version: string) => void;
  onRestored: (plugin: PluginSummary) => void;
}

export function WorkspaceModuleVersions({ plugin, bearer, onRemoved, onRestored }: Props) {
  const dialog = useRef<HTMLDialogElement>(null);
  const titleId = useId();
  const [action, setAction] = useState<"remove" | "restore">("remove");
  const [version, setVersion] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const { id, name } = plugin.manifest;
  const removable = plugin.removable_versions ?? [];
  const detached = plugin.detached_versions ?? [];
  const removing = action === "remove";
  const versions = removing ? removable : detached;
  if (!removable.length && !detached.length) return null;

  function open(next: "remove" | "restore") {
    const available = next === "remove" ? removable : detached;
    setAction(next);
    setVersion(available.includes(plugin.manifest.version) ? plugin.manifest.version : available[0]!);
    setError("");
    dialog.current?.showModal();
  }

  async function confirm() {
    if (busy || !versions.includes(version)) return;
    setBusy(true);
    setError("");
    try {
      if (removing) {
        await removeModuleRelease(id, version, bearer);
        dialog.current?.close();
        onRemoved(id, version);
      } else {
        const restored = await restoreModuleSource(id, version, bearer);
        dialog.current?.close();
        onRestored(restored);
      }
    } catch (reason) {
      setError(reason instanceof ApiClientError && reason.status === 401
        ? "Sign in to the workspace hub through Marketplace, then try again."
        : formatErrorInline(reason));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="bc-module-version-actions" onClick={(event) => event.stopPropagation()}>
      {removable.length > 0 && (
        <button type="button" className="bc-btn bc-btn-secondary"
          data-testid={`manage-installed-${id}`} onClick={() => open("remove")}>Remove…</button>
      )}
      {detached.length > 0 && (
        <button type="button" className="bc-btn bc-btn-secondary"
          data-testid={`restore-source-${id}`} onClick={() => open("restore")}>Restore…</button>
      )}
      <dialog ref={dialog} className="bc-remove-release" aria-labelledby={titleId}
        onCancel={(event) => { if (busy) event.preventDefault(); }}>
        <h2 id={titleId}>{removing ? `Remove ${name} from this workspace?` : `Restore ${name} from local files?`}</h2>
        <label>{removing ? "Installed version" : "Version to restore"}
          <select className="bc-select" value={version} data-testid={removing ? "remove-version" : "restore-version"}
            disabled={busy} onChange={(event) => { setVersion(event.target.value); setError(""); }}>
            {versions.map((item) => <option key={item} value={item}>{item}</option>)}
          </select>
        </label>
        <p>{removing
          ? "This removes the selected version from this workspace. Saved runs and results remain available. Any published copy stays on the hub."
          : "The workspace will use the original module files on this computer."}</p>
        {error && <p role="alert">{error}</p>}
        <div className="bc-module-version-actions">
          <button type="button" className="bc-btn bc-btn-secondary" autoFocus disabled={busy}
            onClick={() => dialog.current?.close()}>Cancel</button>
          <button type="button" className="bc-btn bc-btn-primary" data-testid={`confirm-${action}-release`}
            disabled={busy || !versions.includes(version)} onClick={() => void confirm()}>
            {busy ? "Working…" : `${removing ? "Remove" : "Restore"} v${version}`}
          </button>
        </div>
      </dialog>
    </div>
  );
}
