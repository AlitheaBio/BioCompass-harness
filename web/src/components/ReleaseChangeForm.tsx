import { useId, useState } from "react";
import { isUnsafeReleaseInteger } from "../api";
import type { ReleaseChangeField, ReleaseChangeScalar, ReleaseChangeSource, ReleaseChangeSourceChoice, ReleaseChangeSpec, ReleaseOperation } from "../api";

export function usableReleaseSource(source?: ReleaseChangeSource | null) {
  if (!source || !/^[a-f0-9]{64}$/i.test(source.validated_sha256)) return false;
  if (source.kind === "validated_upload") return typeof source.upload_record_id === "string" && source.upload_record_id.length > 0;
  return source.kind === "prior_release" && Number.isSafeInteger(source.version) && source.version > 0;
}

export function ReleaseSourceDetails({ source }: { source: ReleaseChangeSource }) {
  return <dl className="bc-detail-kv-grid">
    <div className="bc-detail-kv">
      <dt className="bc-detail-k">{source.kind === "validated_upload" ? "Upload record" : "Prior release"}</dt>
      <dd className="bc-detail-v mono" style={{ margin: 0 }}>
        {source.kind === "validated_upload" ? source.upload_record_id : isUnsafeReleaseInteger(source.version)
          ? "Outside the workbench’s supported integer range" : `Version ${source.version}`}
      </dd>
    </div>
    <div className="bc-detail-kv">
      <dt className="bc-detail-k">Validated source SHA-256</dt>
      <dd className="bc-detail-v mono" style={{ margin: 0 }}>{source.validated_sha256}</dd>
    </div>
  </dl>;
}

function choices(field: ReleaseChangeField) {
  return field.enum ?? (field.type === "boolean" ? [true, false] : undefined);
}

function initialInput(field: ReleaseChangeField) {
  if (field.default === undefined) return "";
  return choices(field) ? JSON.stringify(field.default) : String(field.default);
}

function displayScalar(value: ReleaseChangeScalar) {
  return value === "" ? '""' : String(value);
}

function scalarField(field: ReleaseChangeField, raw: string): ReleaseChangeScalar {
  const options = choices(field);
  let value: ReleaseChangeScalar;
  if (options) {
    const option = options.find(item => JSON.stringify(item) === raw);
    if (option === undefined) throw new Error("Choose a listed value.");
    value = option;
  } else {
    value = field.type === "string" ? raw : Number(raw);
  }
  if (field.type === "string" && typeof value === "string") {
    const length = Array.from(value).length;
    if (field.minLength != null && length < field.minLength) throw new Error(`Enter at least ${field.minLength} characters.`);
    if (field.maxLength != null && length > field.maxLength) throw new Error(`Enter at most ${field.maxLength} characters.`);
    if (field.pattern && !new RegExp(field.pattern, "u").test(value)) throw new Error(`Use the required pattern ${field.pattern}.`);
    return value;
  }
  if (field.type === "boolean" && typeof value === "boolean") return value;
  if (field.type !== "number" && field.type !== "integer") throw new Error("Choose a value of the required type.");
  if (typeof value !== "number" || !Number.isFinite(value)) throw new Error("Enter a finite number.");
  if (field.type === "integer" && !Number.isSafeInteger(value)) throw new Error("Enter a whole number within the browser's exact range.");
  if (field.minimum != null && value < field.minimum) throw new Error(`Enter ${field.minimum} or greater.`);
  if (field.maximum != null && value > field.maximum) throw new Error(`Enter ${field.maximum} or less.`);
  if (field.exclusiveMinimum != null && value <= field.exclusiveMinimum) throw new Error(`Enter a number greater than ${field.exclusiveMinimum}.`);
  if (field.exclusiveMaximum != null && value >= field.exclusiveMaximum) throw new Error(`Enter a number less than ${field.exclusiveMaximum}.`);
  if (field.multipleOf != null) {
    const quotient = value / field.multipleOf;
    if (Math.abs(quotient - Math.round(quotient)) > Number.EPSILON * Math.max(1, Math.abs(quotient))) {
      throw new Error(`Enter a multiple of ${field.multipleOf}.`);
    }
  }
  return value;
}

function readOperation(spec: ReleaseChangeSpec, inputs: Map<string, string>): { operation?: ReleaseOperation; error?: string } {
  const values: [string, ReleaseChangeScalar][] = [];
  for (const [name, field] of Object.entries(spec.parameters.properties)) {
    const raw = inputs.get(name) ?? initialInput(field);
    const required = spec.parameters.required?.includes(name);
    if (raw === "") {
      if (!required && (field.default !== "" || inputs.has(name))) continue;
      if (required && (field.type !== "string" || choices(field) || (field.minLength ?? 0) > 0)) return {};
    }
    try {
      values.push([name, scalarField(field, raw)]);
    } catch (err) {
      return { error: `${field.title ?? name} — ${err instanceof Error ? err.message : "Invalid value."}` };
    }
  }
  return { operation: { ...Object.fromEntries(values), kind: spec.id } };
}

export function ReleaseChangeForm({ spec, working, sourceChoices, stageUpload, accept, onPreview }: {
  spec: ReleaseChangeSpec;
  working: boolean;
  sourceChoices: ReleaseChangeSourceChoice[];
  stageUpload?: (file: File) => Promise<ReleaseChangeSource | undefined>;
  accept?: string[];
  onPreview: (operation: ReleaseOperation, source?: ReleaseChangeSource) => void;
}) {
  const id = useId();
  const [inputs, setInputs] = useState(new Map<string, string>());
  const [sourceKey, setSourceKey] = useState("");
  if (Object.values(spec.parameters.properties).some(field =>
    Object.values(field).some(value => Array.isArray(value) ? value.some(isUnsafeReleaseInteger) : isUnsafeReleaseInteger(value)))) {
    return <p className="bc-caption">This action declares a value outside the workbench’s supported integer range.</p>;
  }
  const result = readOperation(spec, inputs);
  const candidates = sourceChoices.filter(choice => choice.source.kind === spec.secondary_input && usableReleaseSource(choice.source));
  const picked = candidates.find(choice => JSON.stringify(choice.source) === sourceKey)?.source;
  const canPreview = !working && Boolean(result.operation) && (!spec.secondary_input || Boolean(picked));
  const change = (name: string, value: string) => setInputs(old => new Map(old).set(name, value));
  return (
    <form onSubmit={event => { event.preventDefault(); if (result.operation && canPreview) onPreview(result.operation, picked); }}
      style={{ display: "grid", gap: 8 }}>
      {Object.entries(spec.parameters.properties).map(([name, field]) => {
        const options = choices(field);
        const controlId = `${id}-${name}`;
        const value = inputs.get(name) ?? initialInput(field);
        const common = {
          id: controlId, name, className: "bc-input", value, disabled: working, style: { width: "100%" },
          "aria-describedby": field.description ? controlId + "-help" : undefined,
          "aria-required": spec.parameters.required?.includes(name) ?? false,
        };
        return <div key={name}>
          <label className="bc-filter-label" htmlFor={controlId}>{field.title ?? name}</label>
          {options ? <select {...common} onChange={e => change(name, e.target.value)}>
            <option value="">Choose a value</option>
            {options.map(option => <option key={JSON.stringify(option)} value={JSON.stringify(option)}>{displayScalar(option)}</option>)}
          </select> : <input {...common} type={field.type === "string" ? "text" : "number"} step="any"
            onChange={e => change(name, e.target.value)} />}
          {field.description && <div id={controlId + "-help"} className="bc-caption">{field.description}</div>}
        </div>;
      })}
      {stageUpload && <div>
        <label className="bc-filter-label" htmlFor={id + "-file"}>Validate a new source file</label>
        <input id={id + "-file"} className="bc-input" type="file" disabled={working}
          accept={accept?.includes("*") ? undefined : accept?.join(",")}
          onChange={async event => {
            const file = event.currentTarget.files?.[0];
            event.currentTarget.value = "";
            if (!file) return;
            setSourceKey("");
            const source = await stageUpload(file);
            if (source) setSourceKey(JSON.stringify(source));
          }} />
      </div>}
      {spec.secondary_input && <div>
        <label htmlFor={id + "-source"} className="bc-filter-label">
          {spec.secondary_input === "validated_upload" ? "Validated upload" : "Prior release"}
        </label>
        <select id={id + "-source"} className="bc-input" style={{ width: "100%" }} required
          disabled={working || !candidates.length} value={picked ? sourceKey : ""} onChange={e => setSourceKey(e.target.value)}>
          <option value="">{candidates.length ? "Choose a source" : "No eligible sources available"}</option>
          {candidates.map(({ source, label }) => (
            <option key={JSON.stringify(source)} value={JSON.stringify(source)}>{label}</option>
          ))}
        </select>
        {picked && <ReleaseSourceDetails source={picked} />}
      </div>}
      {result.error && <div role="alert" className="bc-caption">{result.error}</div>}
      <div><button type="submit" className="bc-btn bc-btn-sm" disabled={!canPreview}>Preview change</button></div>
    </form>
  );
}
