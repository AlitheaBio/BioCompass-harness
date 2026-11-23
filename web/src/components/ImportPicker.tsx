import { useEffect, useState } from "react";
import { fetchUploadImporters, formatErrorInline, type UploadImporter } from "../api";

export function ImportPicker({ mode, scope, onClose, onTable, onTransfer }: {
  mode: "local" | "cloud";
  scope?: string;
  onClose: () => void;
  onTable: () => void;
  onTransfer: (type: "opaque" | "legacy" | UploadImporter) => void;
}) {
  const hosted = mode === "cloud" && Boolean(scope);
  const canDiscover = mode === "local" || hosted;
  const [available, setAvailable] = useState<UploadImporter[]>([]);
  const [loading, setLoading] = useState(canDiscover);
  const [error, setError] = useState("");
  const legacy = hosted && localStorage.getItem(`biocompass.hla-upload.v1:${scope}`) !== null;

  useEffect(() => {
    setAvailable([]);
    setError("");
    setLoading(canDiscover);
    if (!canDiscover) return;
    let active = true;
    fetchUploadImporters(mode)
      .then(data => { if (active) setAvailable(data.importers); })
      .catch(cause => { if (active) setError(formatErrorInline(cause)); })
      .finally(() => { if (active) setLoading(false); });
    return () => { active = false; };
  }, [canDiscover, mode, scope]);

  return <div className="bc-modal-backdrop" onClick={onClose}>
    <div className="bc-modal" role="dialog" aria-label="Upload or import" onClick={event => event.stopPropagation()}>
      <div className="bc-drawer-header"><h2 className="bc-drawer-title">Upload or import</h2></div>
      <div className="bc-drawer-body" style={{ display:"grid", gap:10 }}>
        {legacy && <button className="bc-btn" onClick={() => onTransfer("legacy")}>Resume previous bundle upload</button>}
        {hosted && <button className="bc-btn" onClick={() => onTransfer("opaque")}>Upload original file</button>}
        <button className="bc-btn" onClick={onTable}>Import a CSV, TSV or Parquet table</button>
        {loading && <p role="status" className="bc-caption">Checking available importers…</p>}
        {error && <p role="alert" className="bc-caption">Could not load importers. {error}</p>}
        {canDiscover && !loading && !error && available.length === 0 &&
          <p className="bc-caption">No import modules are available in this workspace.</p>}
        {available.map(module => <button className="bc-btn" key={`${module.plugin_id}@${module.version}`}
          onClick={() => onTransfer(module)}>{module.name} · {module.version} ({module.accept.join(", ")})</button>)}
      </div>
      <div className="bc-drawer-footer"><button className="bc-btn" onClick={onClose}>Close</button></div>
    </div>
  </div>;
}
