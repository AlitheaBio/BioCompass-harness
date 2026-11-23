import { useEffect, useRef, useState } from "react";
import {
  ApiClientError, fetchReleaseChanges, formatErrorInline, importLocalDataset, isUnsafeReleaseInteger, previewReleaseChange, publishReleaseChange, releaseAttachmentUrl,
} from "../api";
import type { ReleaseChangePreview, ReleaseChanges, ReleaseChangeSource, ReleaseChangeSpec, ReleaseOperation } from "../api";
import { ReleaseChangeForm, ReleaseSourceDetails, usableReleaseSource } from "./ReleaseChangeForm";
import { ownerLinkHelp } from "../localOwner";

function unavailable(error: unknown) {
  return error instanceof ApiClientError && [401, 403, 404].includes(error.status ?? 0);
}

const rangeLabel = "Outside the workbench’s supported integer range";

function displayNumber(value: number) {
  return isUnsafeReleaseInteger(value) ? rangeLabel : String(value);
}

function containsUnsafeInteger(value: unknown): boolean {
  if (isUnsafeReleaseInteger(value)) return true;
  return value !== null && typeof value === "object" && Object.values(value).some(containsUnsafeInteger);
}

function ChangeEffects({ effects }: { effects: ReleaseChangePreview["effects"] }) {
  return <dl className="bc-detail-kv-grid">{Object.entries(effects).map(([resource, effect]) => {
    const counts = Object.entries(effect).filter(([, value]) => typeof value === "number" && value > 0);
    if (!counts.length) return null;
    return <div className="bc-detail-kv" key={resource}>
      <dt className="bc-detail-k mono">{resource}</dt>
      {counts.map(([metric, count]) => (
        <dd className="bc-detail-v" style={{ margin: 0 }} key={metric}>{metric} · {isUnsafeReleaseInteger(count) ? rangeLabel : count.toLocaleString()}</dd>
      ))}
    </div>;
  })}</dl>;
}

function previewTitle(item: ReleaseChangePreview, spec?: ReleaseChangeSpec) {
  return `${spec?.title ?? item.operation.kind} · version ${displayNumber(item.base_version)}`;
}

export function HostedReleaseChanges({ catalogId, version, local = false, onPublished, onOpenCatalog }: {
  catalogId: string;
  version: number;
  local?: boolean;
  onPublished?: () => Promise<void>;
  onOpenCatalog: (catalogId: string) => void;
}) {
  const [changes, setChanges] = useState<ReleaseChanges | null>(null);
  const [revision, setRevision] = useState(0);
  const [selection, setSelection] = useState("");
  const [action, setAction] = useState<"validate" | "preview" | "publish" | null>(null);
  const working = action !== null;
  const [error, setError] = useState("");
  const [loadError, setLoadError] = useState("");
  const [locked, setLocked] = useState(false);
  const [published, setPublished] = useState<{ id: string; version: number } | null>(null);
  const mounted = useRef(false);

  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; };
  }, []);

  useEffect(() => {
    let current = true;
    setLoadError("");
    void fetchReleaseChanges(catalogId, local).then(result => {
      if (current && result.catalog_id === catalogId && result.base_version === version) {
        setLocked(false);
        setChanges(result);
      }
    }).catch(err => {
      if (!current) return;
      setChanges(null);
      setLocked(local && err instanceof ApiClientError && err.status === 401);
      if (unavailable(err)) setError("");
      else setLoadError(formatErrorInline(err));
    });
    return () => { current = false; };
  }, [catalogId, version, local, revision]);

  function refresh() {
    setChanges(null);
    setRevision(value => value + 1);
  }

  function failed(err: unknown) {
    if (!mounted.current) return;
    setError(unavailable(err) ? "" : formatErrorInline(err));
    if (err instanceof ApiClientError && (unavailable(err) || err.status === 409)) refresh();
  }

  async function stageSource(file: File): Promise<ReleaseChangeSource | undefined> {
    const importer = changes?.native_append;
    if (!local || !changes?.may_edit || !importer || working) return;
    setError("");
    setAction("validate");
    try {
      const result = await importLocalDataset(importer, file, importer.dataset_name, true);
      if (!result.report.valid || !usableReleaseSource(result.source)) {
        const diagnostics = result.report.diagnostics?.map(item => item.message ?? item.code).filter(Boolean);
        throw new Error(diagnostics?.join(" · ") || "The importer could not validate this file.");
      }
      const next = await fetchReleaseChanges(catalogId, true);
      if (!mounted.current || next.catalog_id !== catalogId || next.base_version !== version) return;
      setChanges(next);
      const source = next.source_choices?.find(({ source }) => source.kind === "validated_upload"
        && source.upload_record_id === result.source?.upload_record_id && source.validated_sha256 === result.source.validated_sha256)?.source;
      if (!source) throw new Error("The validated file is unavailable for this release.");
      return source;
    } catch (err) {
      failed(err);
    } finally {
      if (mounted.current) setAction(null);
    }
  }

  async function preview(operation: ReleaseOperation, source?: ReleaseChangeSource) {
    if (!changes?.may_edit || working) return;
    setAction("preview");
    setError("");
    try {
      const result = await previewReleaseChange(catalogId, changes.base_version, operation, source, local);
      if (!mounted.current) return;
      setChanges(previous => previous && {
        ...previous,
        previews: [result.preview, ...previous.previews.filter(item => item.change_id !== result.preview.change_id)],
      });
    } catch (err) {
      failed(err);
    } finally {
      if (mounted.current) setAction(null);
    }
  }

  async function publish(item: ReleaseChangePreview) {
    if (!changes?.may_publish || working || containsUnsafeInteger(item) || isUnsafeReleaseInteger(changes.head_version)) return;
    setAction("publish");
    setError("");
    try {
      const result = await publishReleaseChange(catalogId, item.change_id, local);
      if (!mounted.current) return;
      setPublished({ id: result.catalog.manifest.id, version: result.version });
      refresh();
      await onPublished?.();
    } catch (err) {
      failed(err);
    } finally {
      if (mounted.current) setAction(null);
    }
  }

  const currentHead = changes?.head_version === version && !isUnsafeReleaseInteger(version) && !published;
  const specs = changes?.operation_specs ?? [];
  const activeSpecs = specs.filter(spec => changes?.operations.includes(spec.id));
  const selectedSpec = activeSpecs.find(spec => spec.id === selection) ?? activeSpecs[0];
  const mayEdit = currentHead && changes?.may_edit && Boolean(selectedSpec);
  const mayPublish = currentHead && changes?.may_publish;
  const historical = changes && changes.head_version > version;
  const previews = changes && (changes.may_edit || changes.may_publish || historical)
    ? changes.previews.filter(item => item.base_version === version)
    : [];
  const showChanges = Boolean(mayEdit || previews.length || published || loadError || error || (mayPublish && changes?.operations.length));
  const attachments = changes?.catalog_id === catalogId && changes.base_version === version && Number.isSafeInteger(version) && version > 0
    ? changes.attachment_files : null;

  return <>
    {locked && <p className="bc-caption">{ownerLinkHelp}</p>}
    {showChanges && <section className="bc-provenance-section" aria-label="Release changes">
      <h3 className="bc-provenance-heading">Release changes</h3>
      {published && <div role="status">
        <p>Published version {displayNumber(published.version)}.</p>
        <button type="button" className="bc-btn bc-btn-sm" disabled={working}
          onClick={() => onOpenCatalog(published.id)}>{isUnsafeReleaseInteger(published.version) ? "Open published version" : `Open version ${published.version}`}</button>
      </div>}
      {mayEdit && selectedSpec && <>
        {activeSpecs.length > 1 ? <>
          <label htmlFor="release-operation" className="bc-filter-label">Change to preview</label>
          <select id="release-operation" className="bc-input" disabled={working} value={selectedSpec.id}
            style={{ width: "100%", marginBottom: 8 }} onChange={e => setSelection(e.target.value)}>
            {activeSpecs.map(spec => <option key={spec.id} value={spec.id}>{spec.title}</option>)}
          </select>
        </> : <h4>{selectedSpec.title}</h4>}
        <ReleaseChangeForm key={JSON.stringify(selectedSpec)} spec={selectedSpec} working={working}
          sourceChoices={changes?.source_choices ?? []}
          stageUpload={local && changes?.native_append && selectedSpec.secondary_input === "validated_upload" ? stageSource : undefined}
          accept={changes?.native_append?.accept}
          onPreview={(operation, source) => void preview(operation, source)} />
      </>}
      {working && <p role="status" className="bc-caption">{action === "validate" ? "Validating source file…" : action === "preview" ? "Preparing preview…" : "Publishing version…"}</p>}
      {(error || loadError) && <p role="alert" className="bc-inline-alert bc-inline-alert-error">{error || loadError}</p>}
      {previews.map(item => {
        const spec = specs.find(entry => entry.id === item.operation.kind);
        const title = previewTitle(item, spec);
        const available = changes?.operations.includes(item.operation.kind);
        const outsideRange = containsUnsafeInteger(item);
        const missingSource = (item.source_pin != null && !usableReleaseSource(item.source_pin))
          || (spec?.secondary_input != null && spec.secondary_input !== item.source_pin?.kind);
        return <article aria-label={title} className="bc-provenance-section" key={item.change_id}>
          <h4 className="bc-provenance-heading">{title}</h4>
          <dl className="bc-detail-kv-grid">
            {Object.entries(item.operation).filter(([field]) => field !== "kind").map(([field, value]) =>
              <div key={field} className="bc-detail-kv">
                <dt className="bc-detail-k">{spec?.parameters.properties[field]?.title ?? field}</dt>
                <dd className="bc-detail-v" style={{ margin: 0, whiteSpace: "pre-wrap" }}>{isUnsafeReleaseInteger(value) ? rangeLabel : value === "" ? '""' : String(value)}</dd>
              </div>
            )}
          </dl>
          {item.source_pin && <ReleaseSourceDetails source={item.source_pin} />}
          <ChangeEffects effects={item.effects} />
          {outsideRange && <div className="bc-caption" role="note" style={{ overflowWrap: "anywhere" }}>
            <p>The workbench supports exact integers from −9,007,199,254,740,991 to 9,007,199,254,740,991. This receipt is read-only.</p>
            <div>Change ID <code>{item.change_id}</code></div>
            {item.validated_sha256 && <div>Validated SHA-256 <code>{item.validated_sha256}</code></div>}
          </div>}
          {item.published_version != null ? <p className="bc-caption">Published in version {displayNumber(item.published_version)}.</p>
            : outsideRange ? null
            : missingSource ? <p className="bc-caption">The pinned source could not be verified.</p>
            : mayPublish && available ? <button type="button" className="bc-btn bc-btn-sm bc-btn-primary" disabled={working}
                onClick={() => void publish(item)}>Confirm and publish</button>
            : <p className="bc-caption">{historical
                ? `A newer version is published. Preview this change on version ${displayNumber(changes.head_version)}.`
                : available ? "Awaiting publication." : "This operation is unavailable."}</p>}
        </article>;
      })}
      {mayPublish && !mayEdit && previews.length === 0 && <p className="bc-caption">No changes awaiting publication.</p>}
      {(loadError || mayEdit || mayPublish) && <button type="button" className="bc-btn bc-btn-sm" disabled={working}
        onClick={() => { setError(""); refresh(); }}>Refresh changes</button>}
    </section>}
    {attachments && attachments.files.length > 0 && <section className="bc-provenance-section" aria-label="Release attachments">
      <h3 className="bc-provenance-heading">Attachments for version {version}</h3>
      <div className="bc-shard-list">
        {attachments.files.map(file => (
          <div key={file.path} className="bc-shard-card" style={{ overflowWrap: "anywhere" }}>
            <a download={file.path.split("/").at(-1)} href={releaseAttachmentUrl(attachments.upload_record_id, version, file.path)}>
              {file.path}
            </a>
            <span className="bc-caption">{isUnsafeReleaseInteger(file.bytes) ? rangeLabel : `${file.bytes.toLocaleString()} ${file.bytes === 1 ? "byte" : "bytes"}`}</span>
            <span className="bc-caption">SHA-256 <code>{file.sha256}</code></span>
          </div>
        ))}
      </div>
    </section>}
  </>;
}
