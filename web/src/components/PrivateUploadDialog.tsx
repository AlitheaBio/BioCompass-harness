import { useEffect, useState } from "react";
import type { FormEvent } from "react";
import {
  completePrivateUpload,
  validatePrivateDraft,
  fetchQuarantineUploads,
  formatErrorInline,
  MULTIPART_CHUNK_BYTES,
  publishPrivateUpload,
  type UploadImporter,
  putPrivateUploadPart,
  resumePrivateUpload,
  startPrivateUpload,
} from "../api";
import type { DraftCorrection, HostedHlaPublication, QuarantineCompletedPart, QuarantineUploadRecord } from "../types";
import { IconClose } from "./Icons";

interface Checkpoint {
  uploadId: string;
  fingerprint: string;
  completion?: QuarantineCompletedPart[];
}

function partLayout(file: File) {
  const layout: { offset: number; size: number }[] = [];
  for (let offset = 0; offset < file.size; offset += MULTIPART_CHUNK_BYTES) {
    layout.push({ offset, size: Math.min(MULTIPART_CHUNK_BYTES, file.size - offset) });
  }
  return layout;
}

// Compare exact file contents after re-selection with memory bounded to one part.
async function fingerprint(file: File, layout: ReturnType<typeof partLayout>) {
  const digests = new Uint8Array(layout.length * 32);
  for (const [index, { offset, size }] of layout.entries()) {
    const part = await file.slice(offset, offset + size).arrayBuffer();
    digests.set(new Uint8Array(await crypto.subtle.digest("SHA-256", part)), index * 32);
  }
  const digest = await crypto.subtle.digest("SHA-256", digests);
  return Array.from(new Uint8Array(digest), (byte) => byte.toString(16).padStart(2, "0")).join("");
}

interface Diagnostic {
  severity: string;
  code: string;
  table?: string;
  row_key?: Record<string, unknown>;
  field?: string;
  value?: unknown;
}

function DraftIssues({ record, working, onChange }: {
  record: QuarantineUploadRecord; working: boolean; onChange?: (patch: DraftCorrection) => void;
}) {
  const report = record.validation_report;
  const [values, setValues] = useState<Record<string, string>>({});
  if (!report || typeof report.valid !== "boolean") return null;
  const errors: Diagnostic[] = Array.isArray(report.diagnostics)
    ? report.diagnostics.filter((d: Diagnostic) => d.severity === "error") : [];
  const groups = new Map<string, Diagnostic[]>();
  errors.forEach(issue => {
    const key = JSON.stringify([issue.table, issue.row_key]);
    groups.set(key, [...(groups.get(key) ?? []), issue]);
  });

  return <section aria-label="Draft validation" style={{ minWidth: 0, overflowWrap: "anywhere" }}>
    <strong>{report.valid ? "Draft validated" : onChange ? "Draft needs correction" : "Draft validation failed"}</strong>
    {Array.from(groups, ([id, issues]) => {
      const first = issues[0]!;
      const fields = new Map<string, string | number | boolean | null>();
      for (const issue of issues) {
        const value = issue.value;
        if (issue.field && (value === null || typeof value === "string" ||
            typeof value === "number" || typeof value === "boolean")) fields.set(issue.field, value);
      }
      const before = Object.fromEntries(fields);
      const after: Record<string, unknown> = {};
      let invalid = false;
      for (const [field, old] of fields) {
        const input = values[JSON.stringify([id, field])];
        if (input === undefined) { after[field] = old; continue; }
        if (typeof old === "string") { after[field] = input; continue; }
        try {
          const parsed: unknown = JSON.parse(input);
          if (parsed !== null && (typeof parsed === "object" || (old !== null && typeof parsed !== typeof old))) {
            invalid = true;
          }
          after[field] = parsed;
        } catch { invalid = true; }
      }
      return <div key={id} style={{ marginTop: 8 }}>
        <p className="bc-caption">{first.table}</p>
        {first.row_key && <p className="bc-caption">{Object.entries(first.row_key)
          .map(([column, value]) => `${column} = ${String(value)}`).join(" · ")}</p>}
        {issues.map((issue, index) => <p className="bc-caption" key={index}>
          {issue.code} {issue.field} {issue.value !== undefined && String(issue.value)}
        </p>)}
        {onChange && Array.from(fields, ([field, initial]) => {
          const key = JSON.stringify([id, field]);
          return <label className="bc-filter-label" key={key} style={{ display: "grid", gap: 4 }}>{field}
            <input className="bc-input" disabled={working} value={values[key] ?? String(initial)}
              onChange={e => setValues({...values, [key]: e.target.value})}/>
          </label>;
        })}
        {onChange && first.table && first.row_key && fields.size > 0 && <button type="button" className="bc-btn"
          disabled={working || invalid || JSON.stringify(before) === JSON.stringify(after)}
          onClick={() => onChange({table: first.table!, row_key: first.row_key!, before, after})}>
          Save correction and validate</button>}
      </div>;
    })}
    {record.draft_corrections?.map((edit, index) => <p className="bc-caption" key={index}>
      {edit.editor} · {edit.patch.table} · {JSON.stringify(edit.patch.before)} → {JSON.stringify(edit.patch.after)}
    </p>)}
  </section>;
}

export function PrivateUploadDialog({ namespace, scope, importer, legacy = false, onClose, onPublished }: {
  importer?: UploadImporter;
  legacy?: boolean;
  namespace: string;
  scope: string;
  onClose: () => void;
  onPublished: (catalogId: string) => void;
}) {
  const format = legacy ? "hla_bundle" : importer ? "module" : "opaque";
  const importKey = importer ? `${importer.plugin_id}@${importer.version}` : "opaque";
  const storageKey = legacy ? `biocompass.hla-upload.v1:${scope}` : `biocompass.upload.v1:${scope}:${importKey}`;
  const title = importer?.name ?? (legacy ? "Upload HLA bundle" : "Upload original file");
  const accept = legacy ? [".zip"] : importer?.accept;
  const [storedOriginal, setStoredOriginal] = useState<QuarantineUploadRecord | null>(null);
  const [file, setFile] = useState<File | null>(null);
  const [target, setTarget] = useState(`${namespace}/`);
  const [checkpoint, setCheckpoint] = useState<Checkpoint | null>(null);
  const [record, setRecord] = useState<QuarantineUploadRecord | null>(null);
  const [publication, setPublication] = useState<HostedHlaPublication | null>(null);
  const [loading, setLoading] = useState(true);
  const [working, setWorking] = useState(false);
  const [stage, setStage] = useState("");
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let active = true;
    async function reopen() {
      try {
        const saved = localStorage.getItem(storageKey);
        if (!saved) return;
        const pending = JSON.parse(saved) as Checkpoint;
        setCheckpoint(pending);
        const records = await fetchQuarantineUploads();
        if (!active) return;
        const upload = records.find((u) => u.upload_record_id === pending.uploadId && u.format === format);
        if (upload && (upload.status === "completed" || upload.status === "ingested"
            || (upload.status === "initiated" && Date.parse(upload.expires_at) > Date.now()))) {
          setRecord(upload);
          setTarget(upload.target_catalog_id);
        } else {
          localStorage.removeItem(storageKey);
          setCheckpoint(null);
          setStage("The previous upload is no longer available. You can start a new upload.");
        }
      } catch (err) {
        if (active) setError(`${formatErrorInline(err)} Close and reopen this dialog to check the saved upload.`);
      } finally {
        if (active) setLoading(false);
      }
    }
    void reopen();
    return () => { active = false; };
  }, [storageKey, format]);

  function remember(pending: Checkpoint) {
    localStorage.setItem(storageKey, JSON.stringify(pending));
    setCheckpoint(pending);
  }

  async function validate(patch?: DraftCorrection) {
    if (!record) return;
    setWorking(true);
    setError(null);
    try {
      setRecord((await validatePrivateDraft(record, patch)).upload);
    } catch (err) {
      setError(formatErrorInline(err));
    } finally {
      setWorking(false);
    }
  }

  async function submit(event: FormEvent) {
    event.preventDefault();
    setWorking(true);
    setError(null);
    try {
      if (record?.status === "completed" || record?.status === "ingested") {
        setStage("Validating and publishing the private release…");
        const receipt = await publishPrivateUpload(record.upload_record_id);
        if (receipt.catalog === null) setStoredOriginal(receipt.upload);
        else setPublication(receipt);
        setStage("");
        localStorage.removeItem(storageKey);
        return;
      }

      // The gateway can commit a transfer even if its response is interrupted.
      if (checkpoint?.completion) {
        setStage("Confirming the uploaded parts…");
        setRecord((await completePrivateUpload(checkpoint.uploadId, checkpoint.completion)).upload);
        setStage("");
        return;
      }

      if (!file || file.size === 0) throw new Error("Choose a non-empty file.");
      if (accept && !accept.some(ext => ext === "*" || file.name.toLowerCase().endsWith(ext.toLowerCase()))) {
        throw new Error(`Choose a file matching ${accept.join(", ")}.`);
      }
      if (file.size > 32 * 1024 ** 3) throw new Error("The maximum upload size is 32 GiB.");
      setStage("Checking the selected file…");
      const layout = partLayout(file);
      const fileFingerprint = await fingerprint(file, layout);
      if (checkpoint && fileFingerprint !== checkpoint.fingerprint) {
        throw new Error("This file differs from the original upload. Select the original file to resume.");
      }
      const sizes = layout.map(({ size }) => size);
      const resumed = checkpoint ? await resumePrivateUpload(checkpoint.uploadId) : null;
      const transfer = resumed ?? await startPrivateUpload(file, target.trim(), sizes, format, importer);
      const pending = { uploadId: transfer.upload.upload_record_id, fingerprint: fileFingerprint };
      setRecord(transfer.upload);
      remember(pending);

      const parts = new Map<number, QuarantineCompletedPart>();
      for (const part of resumed?.completed_parts ?? []) {
        if (sizes[part.part_number - 1] !== part.size_bytes || parts.has(part.part_number)) {
          throw new Error("A stored part does not match the original upload layout.");
        }
        parts.set(part.part_number, { part_number: part.part_number, etag: part.etag });
      }
      const restored = parts.size;
      for (const part of transfer.parts) {
        const slot = layout[part.part_number - 1];
        if (!slot || slot.size !== part.content_length || parts.has(part.part_number)) {
          throw new Error("A requested part does not match the original upload layout.");
        }
        setStage(`Uploading part ${part.part_number} of ${sizes.length}${restored ? ` · ${restored} already stored` : ""}…`);
        parts.set(part.part_number, await putPrivateUploadPart(part, file.slice(slot.offset, slot.offset + slot.size)));
      }
      if (parts.size !== sizes.length) throw new Error("The transfer is missing part information. Resume to check storage again.");
      const completion = Array.from(parts.values()).sort((a, b) => a.part_number - b.part_number);
      remember({ ...pending, completion });
      setStage("Confirming the uploaded parts…");
      setRecord((await completePrivateUpload(pending.uploadId, completion)).upload);
      setStage("");
    } catch (err) {
      setStage("");
      setError(formatErrorInline(err));
    } finally {
      setWorking(false);
    }
  }

  const uploaded = record?.status === "completed" || record?.status === "ingested";
  return (
    <div className="bc-modal-backdrop" onClick={() => !working && onClose()}>
      <form className="bc-modal" style={{ maxWidth: 540 }} role="dialog" aria-modal="true"
        aria-labelledby="hla-bundle-title" onSubmit={(event) => void submit(event)} onClick={(event) => event.stopPropagation()}>
        <div className="bc-drawer-header">
          <h2 id="hla-bundle-title" className="bc-drawer-title">{title}</h2>
          <button type="button" className="bc-icon-btn" disabled={working} aria-label="Close upload dialog" onClick={onClose}>
            <IconClose size={15} />
          </button>
        </div>
        <div className="bc-drawer-body" style={{ display: "grid", gridTemplateColumns: "minmax(0, 1fr)", gap: 12 }}>
          {error && <div className="bc-caption" role="alert">{error}</div>}
          {loading && <p className="bc-caption" role="status">Checking for an unfinished upload…</p>}
          {!loading && !publication && !storedOriginal && (
            <>
              <div>
                <label className="bc-filter-label" htmlFor="hla-target">Target dataset ID</label>
                <input id="hla-target" className="bc-input mono" style={{ width: "100%" }} value={target}
                  onChange={(event) => setTarget(event.target.value)} disabled={working || Boolean(record)} required />
              </div>
              {record && <p className="bc-caption">{record.filename}</p>}
              {!uploaded && !checkpoint?.completion && (
                <div>
                  <label className="bc-filter-label" htmlFor="hla-source">Source file</label>
                  <input id="hla-source" type="file" accept={accept?.includes("*") ? undefined : accept?.join(",")} className="bc-input"
                    style={{ width: "100%" }} disabled={working} onChange={(event) => setFile(event.target.files?.[0] ?? null)} />
                </div>
              )}
              {record?.status === "initiated" && !working && !checkpoint?.completion && (
                <p className="bc-caption">Upload unfinished. Select the original file to resume.</p>
              )}
              {record?.status === "completed" && format === "module" && importer && <>
                <button type="button" className="bc-btn" disabled={working} onClick={() => void validate()}>Validate draft</button>
                <DraftIssues key={record.draft_corrections?.length ?? 0} record={record} working={working}
                  onChange={importer.row_corrections ? patch => void validate(patch) : undefined}/>
              </>}
              {uploaded && !working && <p role="status" className="bc-caption">
                {record.status === "ingested" ? "Private version already published." : (record.validation_report?.valid === false ? "Source retained. Fix the reported problems to publish." : "Upload complete. Ready to publish a private version.")}
              </p>}
            </>
          )}
          {stage && <p className="bc-caption" role="status">{stage}</p>}
          {storedOriginal && <div role="status">
            <h3>Original file saved</h3>
            <a href={`/api/v1/uploads/quarantine/${encodeURIComponent(storedOriginal.upload_record_id)}/source`} download={storedOriginal.filename}>{storedOriginal.filename}</a>
          </div>}
          {publication && <div role="status">
            <h3>Private version {publication.release.snapshot_id} published</h3>
            <p className="bc-caption mono">{publication.catalog.manifest.id}</p>
            {(publication.attachments ?? []).map((attachment) => <p key={attachment.path}>{attachment.path.split("/").pop()}</p>)}
            {(publication.raw_files ?? []).filter((entry) => entry.availability === "metadata_only").map((entry) => (
              <p className="bc-caption" key={entry.file_name}>{entry.file_name} · metadata only</p>
            ))}
          </div>}
        </div>
        <div className="bc-drawer-footer">
          <button className="bc-btn" type="button" disabled={working} onClick={onClose}>Close</button>
          {publication ? (
            <button className="bc-btn bc-btn-primary" type="button" onClick={() => {
              onPublished(publication.catalog.manifest.id);
              onClose();
            }}>View dataset</button>
          ) : storedOriginal ? null : (
            <button className="bc-btn bc-btn-primary" type="submit"
              disabled={loading || working || record?.validation_report?.valid === false || Boolean(checkpoint && !record) || (!uploaded && !checkpoint?.completion && !file)}>
              {working ? "Working…" : uploaded ? record.status === "ingested" ? "View publication" : format === "opaque" ? "Save original" : "Publish private version"
                : checkpoint?.completion ? "Confirm upload" : record ? "Resume upload" : "Upload file"}
            </button>
          )}
        </div>
      </form>
    </div>
  );
}
