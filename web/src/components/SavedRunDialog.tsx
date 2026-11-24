import { useEffect, useRef, useState } from "react";
import { fetchRun, formatErrorInline } from "../api";
import type { RunRecord, RunVerification } from "../types";
import { ModuleRunsTab } from "./ModuleTabs";

interface Props {
  runId: string;
  verifyReport: RunVerification | null;
  isLocalMode?: boolean;
  onClose: () => void;
  onUpdate: (run: RunRecord) => void;
  onReverify: (run: RunRecord, tamper: boolean) => void;
}

export function SavedRunDialog({ runId, verifyReport, isLocalMode = false, onClose, onUpdate, onReverify }: Props) {
  const dialog = useRef<HTMLDialogElement>(null);
  const [run, setRun] = useState<RunRecord>();
  const [error, setError] = useState("");
  useEffect(() => {
    dialog.current?.showModal();
    let current = true;
    void fetchRun(runId).then((record) => {
      if (!current) return;
      if (record.run_id !== runId) throw new Error("The workspace returned a different run.");
      setRun(record);
    }).catch((reason) => { if (current) setError(formatErrorInline(reason)); });
    return () => { current = false; };
  }, [runId]);

  return (
    <dialog ref={dialog} aria-labelledby="saved-run-title" data-testid="saved-run-dialog"
      className="bc-remove-release bc-saved-run-dialog" onClose={onClose}>
      <div className="bc-card-header">
        <h2 id="saved-run-title">Saved result</h2>
        <button type="button" className="bc-btn bc-btn-secondary" autoFocus
          onClick={() => dialog.current?.close()}>Close</button>
      </div>
      {error ? <p role="alert">{error}</p> : run ? (
        <>
          <p>{run.plugin_id} v{run.plugin_version}</p>
          <ModuleRunsTab runs={[run]} isFocused isConfirmingRun={false} isLocalMode={isLocalMode} liveLogs={[]}
            verifyReport={verifyReport} onReverifyChain={onReverify}
            onUpdateRun={(updated) => { setRun(updated); onUpdate(updated); }} />
        </>
      ) : <p role="status">Loading saved result…</p>}
    </dialog>
  );
}
