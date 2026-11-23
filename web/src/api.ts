import { beginPrepareRequest } from "./prepareRequest";
import { setWorkspaceReadHandle, workspaceReadGeneration, workspaceReadHandle } from "./workspaceReadSession";
import { compileSdkCatalogQuery } from "@biocompass/plugin-sdk";
import { localOwnerHeaders, localOwnerRequest } from "./localOwner";
import type {
  ApiKeyRecord,
  AuditLogRecord,
  AuthPrincipal,
  CatalogManifest,
  CatalogQuery,
  CatalogQueryResult,
  CreatedApiKey,
  CreatedNodeEnrollment,
  CreatedOrgInvitation,
  CuratedModuleApprovalRecord,
  DatasetGrant,
  DatasetHostingMode,
  DatasetOfferRecord,
  DatasetOfferScope,
  DatasetOfferRequest,
  DatasetOfferSubscription,
  EnumerationMonitorEntry,
  FilterExpr,
  FrogletVerifyReport,
  GrantAccessLevel,
  GrantUsageSummary,
  HealthStatus,
  IngestFormat,
  IngestMode,
  HostedHlaPublication,
  LedgerState,
  LedgerTx,
  LlmConfigResponse,
  ModuleDownload,
  ModuleOffer,
  FederatedModuleOffer,
  ModuleOfferRequest,
  NodeMode,
  NodeSummaryRecord,
  OrgInvitationRecord,
  OrgMemberRecord,
  OrgRole,
  PeptideRecord,
  PluginSummary,
  PluginVersionRecord,
  PluginVisibility,
  PresignedPartSpec,
  QuarantineCompletedPart,
  ProjectRecord,
  QuarantineUploadRecord,
  QueryAst,
  QueryTableResult,
  RunLogEvent,
  RunQuote,
  RunRecord,
  SavedViewRecord,
  SavedViewVerificationReport,
  ScaffoldResult,
  SessionEvent,
  SessionRecord,
  TestLlmConnectionResponse,
  ResultShare,
  TierDefinitionRecord,
  TotpEnrollment,
  UpdateLlmConfigRequest,
  UpdateTierInput,
} from "./types";

export interface ParsedErrorDisplay {
  message: string;
  code?: string;
  hint?: string;
}

export class ApiClientError extends Error {
  readonly code?: string;
  readonly hint?: string;
  readonly status?: number;

  constructor(message: string, opts?: { code?: string; hint?: string; status?: number }) {
    super(message);
    this.name = "ApiClientError";
    this.code = opts?.code;
    this.hint = opts?.hint;
    this.status = opts?.status;
  }
}

function normalizeErrorMessage(raw: string): string {
  let cleaned = raw.trim().replace(/^Error:\s*/i, "");
  cleaned = cleaned.replace(/^upstream LLM provider error:\s*/i, "");
  if (!cleaned) return "An unexpected error occurred.";
  return cleaned.charAt(0).toUpperCase() + cleaned.slice(1);
}

export function formatClientError(err: unknown): ParsedErrorDisplay {
  if (err instanceof ApiClientError) {
    return {
      message: normalizeErrorMessage(err.message),
      ...(err.code ? { code: err.code } : {}),
      ...(err.hint ? { hint: err.hint } : {}),
    };
  }
  const raw = err instanceof Error ? err.message : typeof err === "string" ? err : String(err);
  const trimmed = raw.trim();
  if (!trimmed) {
    return { message: "An unexpected error occurred." };
  }
  if (trimmed.startsWith("{") && trimmed.endsWith("}")) {
    try {
      const parsed = JSON.parse(trimmed) as {
        message?: unknown;
        error?: unknown;
        code?: unknown;
        hint?: unknown;
      };
      const msg =
        typeof parsed.message === "string"
          ? parsed.message
          : typeof parsed.error === "string"
            ? parsed.error
            : null;
      if (msg) {
        return {
          message: normalizeErrorMessage(msg),
          ...(typeof parsed.code === "string" ? { code: parsed.code } : {}),
          ...(typeof parsed.hint === "string" ? { hint: parsed.hint } : {}),
        };
      }
    } catch {
      // Fall through to string normalization
    }
  }
  if (/^(failed to fetch|load failed|networkerror when attempting to fetch resource)/i.test(trimmed)) {
    return {
      message: "Cannot connect to the BioCompass server.",
      hint: "Check that the biocompass daemon is running and retry.",
    };
  }
  return { message: normalizeErrorMessage(trimmed) };
}

export function formatErrorInline(err: unknown): string {
  const formatted = formatClientError(err);
  return formatted.hint ? `${formatted.message} ${formatted.hint}` : formatted.message;
}

class ApiNetworkError extends ApiClientError {}

async function fetchWithNetworkError(url: string, init?: RequestInit): Promise<Response> {
  try {
    return await fetch(url, {
      credentials: init?.credentials ?? "include",
      ...init,
    });
  } catch (err: unknown) {
    const formatted = formatClientError(err);
    throw new ApiNetworkError(formatted.message, { hint: formatted.hint });
  }
}

async function parseHttpError(response: Response): Promise<ApiClientError> {
  const statusFallback = response.statusText
    ? `Request failed (${response.status} ${response.statusText})`
    : `Request failed (HTTP ${response.status})`;
  let errText = statusFallback;
  let code: string | undefined;
  let hint: string | undefined;
  try {
    const text = await response.text();
    if (text.trim()) {
      try {
        const body = JSON.parse(text) as {
          error?: unknown;
          message?: unknown;
          code?: unknown;
          hint?: unknown;
        };
        if (typeof body.error === "string" && body.error.trim()) {
          errText = body.error.trim();
        } else if (typeof body.message === "string" && body.message.trim()) {
          errText = body.message.trim();
        }
        if (typeof body.code === "string" && body.code.trim()) {
          code = body.code.trim();
        }
        if (typeof body.hint === "string" && body.hint.trim()) {
          hint = body.hint.trim();
        }
      } catch {
        const plain = text.replace(/<[^>]+>/g, " ").replace(/\s+/g, " ").trim();
        if (plain && !plain.startsWith("<!DOCTYPE")) {
          errText = plain.slice(0, 240);
        }
      }
    }
  } catch {
    // Keep status fallback
  }
  return new ApiClientError(normalizeErrorMessage(errText), {
    code,
    hint,
    status: response.status,
  });
}

async function apiFetch<T>(url: string, init?: RequestInit): Promise<T> {
  const response = await fetchWithNetworkError(url, {
    ...init,
    headers: {
      "Content-Type": "application/json",
      ...(init?.headers ?? {}),
    },
  });
  if (!response.ok) {
    throw await parseHttpError(response);
  }
  return (await response.json()) as T;
}

export interface PersonalProfile {
  user_id: string;
  display_name: string;
  email: string;
  avatar_seed: string | null;
  email_verification: "oidc" | "unavailable";
  requested_email: string | null;
}
export const fetchPersonalProfile = () => apiFetch<PersonalProfile>("/api/v1/auth/profile");
export const savePersonalProfile = (display_name: string, avatar_seed: string | null) =>
  apiFetch<PersonalProfile>("/api/v1/auth/profile", {method:"PUT",body:JSON.stringify({display_name,avatar_seed})});
export const requestPersonalEmail = (email:string) =>
  apiFetch<{authorize_url:string}>("/api/v1/auth/profile/email", {method:"POST",body:JSON.stringify({email})});

export async function fetchHealth(): Promise<HealthStatus> {
  return apiFetch<HealthStatus>("/api/v1/health");
}

export async function fetchLlmConfig(): Promise<LlmConfigResponse> {
  return apiFetch<LlmConfigResponse>("/api/v1/llm/config");
}

export async function updateLlmConfig(
  payload: UpdateLlmConfigRequest,
): Promise<LlmConfigResponse> {
  return apiFetch<LlmConfigResponse>("/api/v1/llm/config", {
    method: "PUT",
    headers: localOwnerHeaders(),
    body: JSON.stringify(payload),
  });
}

export async function testLlmConnection(
  payload?: UpdateLlmConfigRequest,
): Promise<TestLlmConnectionResponse> {
  return apiFetch<TestLlmConnectionResponse>("/api/v1/llm/test", {
    method: "POST",
    headers: localOwnerHeaders(),
    body: JSON.stringify(payload ?? {}),
  });
}

export async function fetchGrants(): Promise<DatasetGrant[]> {
  const data = await apiFetch<{ grants: DatasetGrant[] }>("/api/v1/grants");
  return data.grants;
}

export async function fetchGrantsWithUsage(): Promise<{
  grants: DatasetGrant[];
  grant_usage: GrantUsageSummary[];
}> {
  const data = await apiFetch<{
    grants: DatasetGrant[];
    grant_usage?: GrantUsageSummary[];
  }>("/api/v1/grants");
  return {
    grants: data.grants,
    grant_usage: data.grant_usage ?? [],
  };
}

export async function createGrant(payload: {
  grantee_org_id?: string;
  catalog_id: string;
  label: string;
  level: GrantAccessLevel;
  row_scope?: FilterExpr | null;
  daily_row_budget?: number | null;
  max_rows_per_query: number;
  allow_local_export: boolean;
}): Promise<DatasetGrant> {
  return apiFetch<DatasetGrant>("/api/v1/grants", {
    method: "POST",
    body: JSON.stringify(payload),
  });
}

export async function updateGrant(
  grantId: string,
  payload: {
    label?: string;
    level: GrantAccessLevel;
    row_scope?: FilterExpr | null;
    daily_row_budget?: number | null;
    max_rows_per_query: number;
    allow_local_export: boolean;
  },
): Promise<DatasetGrant> {
  return apiFetch<DatasetGrant>(`/api/v1/grants/${encodeURIComponent(grantId)}`, {
    method: "PATCH",
    body: JSON.stringify(payload),
  });
}

export async function deleteGrant(grantId: string): Promise<void> {
  await apiFetch<{ deleted: boolean }>(`/api/v1/grants/${encodeURIComponent(grantId)}`, {
    method: "DELETE",
  });
}

export async function fetchCatalogs(): Promise<CatalogManifest[]> {
  const data = await apiFetch<{ catalogs: CatalogManifest[] }>("/api/v1/catalogs");
  return data.catalogs;
}

export function toQueryAst(query: QueryAst | CatalogQuery): QueryAst {
  return compileSdkCatalogQuery(query) as QueryAst;
}

// Private releases can have a version in the catalog name. Keep the complete
// name in one URL segment so every catalog action uses the same identity.
function catalogPath(id: string): string {
  const slash = id.indexOf("/");
  if (slash < 1 || slash === id.length - 1) throw new Error(`Invalid catalog ID: ${id}`);
  return `/api/v1/catalogs/${encodeURIComponent(id.slice(0, slash))}/${encodeURIComponent(id.slice(slash + 1))}`;
}

export type ReleaseChangeScalar = string | boolean | number;

export function isUnsafeReleaseInteger(value: unknown): boolean {
  return typeof value === "number" && Number.isInteger(value) && !Number.isSafeInteger(value);
}

export interface ReleaseChangeField {
  type: "string" | "boolean" | "integer" | "number";
  title?: string;
  description?: string;
  default?: ReleaseChangeScalar;
  enum?: ReleaseChangeScalar[];
  minLength?: number;
  maxLength?: number;
  pattern?: string;
  minimum?: number;
  maximum?: number;
  exclusiveMinimum?: number;
  exclusiveMaximum?: number;
  multipleOf?: number;
}

export interface ReleaseChangeSpec {
  id: string;
  title: string;
  secondary_input?: ReleaseChangeSource["kind"];
  parameters: {
    type: "object";
    additionalProperties: false;
    properties: Record<string, ReleaseChangeField>;
    required?: string[];
  };
}

export type ReleaseOperation = { kind: string } & Record<string, ReleaseChangeScalar>;

export type ReleaseChangeSource =
  | { kind: "validated_upload"; upload_record_id: string; validated_sha256: string }
  | { kind: "prior_release"; version: number; validated_sha256: string };

export interface ReleaseChangeSourceChoice {
  source: ReleaseChangeSource;
  label: string;
}

export interface ReleaseChangePreview {
  change_id: string;
  base_version: number;
  operation: ReleaseOperation;
  source_pin?: ReleaseChangeSource | null;
  effects: Record<string, { affected_sha256: string; [metric: string]: string | number }>;
  validated_sha256: string;
  published_version?: number | null;
}

export interface ReleaseChanges {
  catalog_id: string;
  base_version: number;
  head_version: number;
  may_edit: boolean;
  may_publish: boolean;
  operations: string[];
  operation_specs?: ReleaseChangeSpec[];
  source_choices?: ReleaseChangeSourceChoice[];
  native_append?: { plugin_id: string; version: string; accept: string[]; dataset_name: string };
  attachment_files?: { upload_record_id: string; files: { path: string; sha256: string; bytes: number }[] } | null;
  previews: ReleaseChangePreview[];
}

export function fetchReleaseChanges(catalogId: string, local = false) {
  return apiFetch<ReleaseChanges>(`${catalogPath(catalogId)}/changes`, { cache: "no-store", ...(local && localOwnerRequest()) });
}

export function previewReleaseChange(catalogId: string, baseVersion: number, operation: ReleaseOperation, source?: ReleaseChangeSource, local = false) {
  return apiFetch<{ preview: ReleaseChangePreview }>(`${catalogPath(catalogId)}/changes/preview`, {
    method: "POST", ...(local && localOwnerRequest()),
    body: JSON.stringify({ base_version: baseVersion, operation, source }),
  });
}

export function publishReleaseChange(catalogId: string, changeId: string, local = false) {
  return apiFetch<{ version: number; catalog: { manifest: { id: string } } }>(`${catalogPath(catalogId)}/changes/${encodeURIComponent(changeId)}/publish`, {
    method: "POST", ...(local && localOwnerRequest()),
    body: JSON.stringify({}),
  });
}

export async function queryCatalog<TRow = PeptideRecord>(
  catalogId: string,
  grantId: string,
  query: QueryAst | CatalogQuery,
  releaseVersion?: number,
  signal?: AbortSignal,
): Promise<QueryTableResult<TRow>> {
  return apiFetch<QueryTableResult<TRow>>(`${catalogPath(catalogId)}/query`, {
    method: "POST",
    signal,
    body: JSON.stringify({ grant_id: grantId, release_version: releaseVersion, query: toQueryAst(query) }),
  });
}

export async function exportCatalogRows(
  catalogId: string,
  grantId: string,
  query: QueryAst | CatalogQuery,
  releaseVersion?: number,
): Promise<CatalogQueryResult> {
  return apiFetch<CatalogQueryResult>(`${catalogPath(catalogId)}/export`, {
    method: "POST",
    body: JSON.stringify({ grant_id: grantId, release_version: releaseVersion, query: toQueryAst(query) }),
  });
}

export async function rollbackCatalogSnapshot(
  catalogId: string,
  targetSnapshotId: number,
): Promise<{ catalog: CatalogManifest }> {
  return apiFetch<{ catalog: CatalogManifest }>(
    `${catalogPath(catalogId)}/rollback`,
    {
      method: "POST",
      headers: localOwnerHeaders(),
      body: JSON.stringify({ target_snapshot_id: targetSnapshotId }),
    },
  );
}

export async function pinCatalogSnapshot(
  catalogId: string,
  snapshotId: number,
  pinned: boolean,
): Promise<{ catalog: CatalogManifest }> {
  return apiFetch<{ catalog: CatalogManifest }>(
    `${catalogPath(catalogId)}/snapshots/${snapshotId}/pin`,
    {
      method: "POST",
      body: JSON.stringify({ pinned }),
    },
  );
}

export async function expireCatalogSnapshots(
  catalogId: string,
  keepLatest = 1,
): Promise<{ catalog: CatalogManifest; expired_snapshot_ids: number[] }> {
  return apiFetch<{ catalog: CatalogManifest; expired_snapshot_ids: number[] }>(
    `${catalogPath(catalogId)}/expire-snapshots`,
    {
      method: "POST",
      body: JSON.stringify({ keep_latest: keepLatest }),
    },
  );
}

export async function fetchPlugins(): Promise<PluginSummary[]> {
  const data = await apiFetch<{ plugins: PluginSummary[] }>("/api/v1/plugins");
  return data.plugins;
}

export async function fetchModuleDownloads(bearer?: string): Promise<ModuleDownload[]> {
  const result = await apiFetch<{ plugins: ModuleDownload[] }>("/api/v1/workspace/modules", {
    cache: "no-store",
    ...(bearer ? { credentials: "omit", headers: { Authorization: `Bearer ${bearer}` } } : {}),
  });
  return result.plugins;
}

export async function loginWorkspaceHub(payload: {
  email: string; password: string; org_id?: string; totp_code?: string;
}): Promise<string> {
  const result = await apiFetch<{ session_token: string }>("/api/v1/workspace/auth/login", {
    ...workspaceSavedSession(undefined, workspaceReadHandle()), method: "POST", body: JSON.stringify(payload),
  });
  if (!result.session_token) throw new Error("The hub did not return a login session.");
  setWorkspaceReadHandle(null);
  return result.session_token;
}

export interface WorkspacePreferences {
  available_locations: ("local" | "cloud")[];
  local_workspace_id: string | null;
  default_location: "local" | "cloud";
  sync_enabled: boolean;
  sync_interval_seconds: number | null;
  last_synced_at: string | null;
  read_session_token?: string | null;
}

function workspaceSession(bearer?: string): RequestInit {
  return {
    cache: "no-store",
    ...(bearer ? { credentials: "omit", headers: { Authorization: `Bearer ${bearer}` } } : {}),
  };
}

function workspaceSavedSession(bearer?: string, handle?: string | null): RequestInit {
  return {
    cache: "no-store", credentials: "omit",
    headers: {
      ...(bearer && { Authorization: `Bearer ${bearer}` }),
      ...(handle && { "X-Workspace-Read": handle }),
    },
  };
}

async function workspacePreferences(path: string, bearer: string | undefined, init?: RequestInit): Promise<WorkspacePreferences> {
  const generation = workspaceReadGeneration();
  const preferences = await apiFetch<WorkspacePreferences>(path, { ...workspaceSession(bearer), ...init });
  if (bearer && generation === workspaceReadGeneration() && preferences.read_session_token !== undefined) {
    setWorkspaceReadHandle(preferences.read_session_token);
  }
  return preferences;
}

export function fetchWorkspacePreferences(bearer?: string): Promise<WorkspacePreferences> {
  return workspacePreferences("/api/v1/workspace/settings", bearer);
}

export function updateWorkspacePreferences(
  preferences: Pick<WorkspacePreferences, "default_location" | "sync_enabled">,
  bearer?: string,
): Promise<WorkspacePreferences> {
  return workspacePreferences("/api/v1/workspace/settings", bearer, { method: "PUT", body: JSON.stringify(preferences) });
}

export function pairInstalledWorkspace(bearer: string): Promise<WorkspacePreferences> {
  return workspacePreferences("/api/v1/workspace/pair", bearer, { method: "POST" });
}

export interface WorkspaceSyncResult {
  delivered: number;
  acknowledged: number;
  pending: number;
  errors: { run_id: string; error: string }[];
  last_synced_at?: string;
}

export function syncInstalledWorkspace(bearer: string): Promise<WorkspaceSyncResult> {
  return apiFetch("/api/v1/workspace/sync", { ...workspaceSession(bearer), method: "POST" });
}

export async function endInstalledWorkspaceSession(bearer?: string): Promise<void> {
  const handle = workspaceReadHandle();
  try {
    const response = await fetchWithNetworkError("/api/v1/workspace/sync-session", {
      ...workspaceSavedSession(bearer, handle), method: "DELETE",
    });
    if (!response.ok) throw await parseHttpError(response);
  } finally {
    if (workspaceReadHandle() === handle) setWorkspaceReadHandle(null);
  }
}

export interface InstalledResultsPage {
  runs: RunRecord[];
  offline?: boolean;
  read_session_expires_at?: string | null;
}

export function fetchInstalledWorkspaceRuns(bearer: string | undefined, handle: string | null): Promise<InstalledResultsPage> {
  return apiFetch("/api/v1/workspace/runs", workspaceSavedSession(bearer, handle));
}

export async function downloadInstalledWorkspaceResult(id: string, bearer: string | undefined, handle: string | null, artifact?: string): Promise<Blob> {
  const suffix = artifact === undefined ? "result" : "artifacts/" + artifact.split("/").map(encodeURIComponent).join("/");
  const response = await fetchWithNetworkError(
    `/api/v1/workspace/runs/${encodeURIComponent(id)}/${suffix}`, workspaceSavedSession(bearer, handle),
  );
  if (!response.ok) throw await parseHttpError(response);
  return response.blob();
}

export interface WorkspaceArchive {
  run_id: string;
  sha256: string;
  receipt_hash: string;
  archived_at: string;
  download_url?: string;
  sign_in_url?: string;
}

export async function fetchWorkspaceArchiveTargets(): Promise<{ id: string; label: string }[]> {
  return (await apiFetch<{targets: {id: string; label: string}[]}>(
    "/api/v1/workspace/archive-targets", workspaceSession(),
  )).targets;
}

export function archiveWorkspaceResult(id: string, bearer?: string, providerId?: string): Promise<WorkspaceArchive> {
  return apiFetch(`/api/v1/workspace/archives/${encodeURIComponent(id)}`, {
    ...workspaceSession(bearer), method: "POST",
    ...(providerId && { body: JSON.stringify({ provider_id: providerId }) }),
  });
}

export async function installModuleDownload(release: ModuleDownload, bearer?: string): Promise<PluginSummary> {
  const parts = release.plugin_id.split("/");
  if (parts.length !== 2 || parts.some((part) => !part)) throw new Error("Invalid module ID.");
  const response = await apiFetch<{
    installed: boolean; plugin_id: string; plugin_version: string; plugin: PluginSummary;
  }>(`/api/v1/plugins/${parts.map(encodeURIComponent).join("/")}/install`, {
    method: "POST",
    ...(bearer ? { credentials: "omit" } : {}),
    headers: {
      ...localOwnerHeaders(),
      ...(bearer ? { Authorization: `Bearer ${bearer}` } : {}),
    },
    body: JSON.stringify({ plugin_version: release.version }),
  });
  if (!response.installed || response.plugin_id !== release.plugin_id || response.plugin_version !== release.version ||
      !response.plugin?.installed || response.plugin.manifest.id !== release.plugin_id ||
      response.plugin.manifest.version !== release.version ||
      !response.plugin.manifest.image.endsWith(`@${release.image_digest}`)) {
    throw new Error("The installed module does not match the selected release.");
  }
  return response.plugin;
}

function workspaceModulePath(pluginId: string): string {
  const parts = pluginId.split("/");
  if (parts.length !== 2 || parts.some((part) => !part)) throw new Error("Invalid module ID.");
  return `/api/v1/plugins/${parts.map(encodeURIComponent).join("/")}`;
}

export async function removeModuleRelease(id: string, version: string, bearer?: string): Promise<void> {
  const response = await apiFetch<{ plugin_id: string; plugin_version: string; removed: boolean }>(
    `${workspaceModulePath(id)}/versions/${encodeURIComponent(version)}/install`, {
      method: "DELETE",
      ...(bearer ? { credentials: "omit" } : {}),
      headers: {
        ...localOwnerHeaders(),
        ...(bearer ? { Authorization: `Bearer ${bearer}` } : {}),
      },
    },
  );
  if (response.plugin_id !== id || response.plugin_version !== version || typeof response.removed !== "boolean") {
    throw new Error("The workspace did not confirm the selected module version.");
  }
}

export async function restoreModuleSource(id: string, version: string, bearer?: string): Promise<PluginSummary> {
  const response = await apiFetch<{
    plugin_id: string; plugin_version: string; installed: boolean; plugin: PluginSummary;
  }>(`${workspaceModulePath(id)}/install`, {
    method: "POST",
    ...(bearer ? { credentials: "omit" } : {}),
    headers: {
      ...localOwnerHeaders(),
      ...(bearer ? { Authorization: `Bearer ${bearer}` } : {}),
    },
    body: JSON.stringify({ plugin_version: version, source: "workspace" }),
  });
  if (!response.installed || response.plugin_id !== id || response.plugin_version !== version ||
      !response.plugin?.installed || response.plugin.manifest.id !== id || response.plugin.manifest.version !== version) {
    throw new Error("The workspace did not restore the selected module version.");
  }
  return response.plugin;
}

export async function installPlugin(
  pluginId: string,
): Promise<{ plugin_id: string; installed: boolean }> {
  const parts = pluginId.split("/");
  if (parts.length !== 2 || !parts[0] || !parts[1]) {
    throw new Error(`Invalid plugin ID: ${pluginId}`);
  }
  const [ns, slug] = parts;
  return apiFetch<{ plugin_id: string; installed: boolean }>(
    `/api/v1/plugins/${encodeURIComponent(ns)}/${encodeURIComponent(slug)}/install`,
    { method: "POST", headers: localOwnerHeaders(), body: JSON.stringify({}) },
  );
}

export async function scaffoldPlugin(payload: {
  template: string;
  plugin_id: string;
  display_name: string;
  include_vite_ui: boolean;
}): Promise<ScaffoldResult> {
  return apiFetch<ScaffoldResult>("/api/v1/plugins/scaffold", {
    method: "POST",
    headers: localOwnerHeaders(),
    body: JSON.stringify(payload),
  });
}

export async function fetchInstalledIdes(): Promise<{
  ides: import("./types").InstalledIde[];
  default_ide_id: string | null;
}> {
  return apiFetch<{
    ides: import("./types").InstalledIde[];
    default_ide_id: string | null;
  }>("/api/v1/plugins/ides");
}

export async function openPluginInDefaultIde(
  directory: string,
  options?: { ide?: string; custom_command?: string },
): Promise<{
  launched: string;
  directory: string;
}> {
  return apiFetch<{ launched: string; directory: string }>(
    "/api/v1/plugins/open-ide",
    {
      method: "POST",
      headers: localOwnerHeaders(),
      body: JSON.stringify({
        directory,
        ...(options?.ide ? { ide: options.ide } : {}),
        ...(options?.custom_command ? { custom_command: options.custom_command } : {}),
      }),
    },
  );
}

export async function fetchRuns(): Promise<RunRecord[]> {
  const data = await apiFetch<{ runs: RunRecord[] }>("/api/v1/runs");
  return data.runs;
}

export async function fetchRun(runId: string, signal?: AbortSignal): Promise<RunRecord> {
  return apiFetch<RunRecord>(`/api/v1/runs/${encodeURIComponent(runId)}`, { signal });
}

export async function prepareRun(payload: {
  plugin_id: string;
  plugin_version?: string;
  offer_id?: string;
  marketplace_offer_id?: string;
  params: Record<string, unknown>;
  grant_id?: string;
  release_version?: number;
  session_id?: string;
}): Promise<RunQuote> {
  const attempt = await beginPrepareRequest(payload);
  let quote: RunQuote;
  try {
    quote = await apiFetch<RunQuote>("/api/v1/runs/prepare", {
      method: "POST",
      body: JSON.stringify(attempt ? { ...payload, request_key: attempt.key } : payload),
    });
  } catch (error) {
    // A transport failure or a gateway/server error can follow a committed upstream quote.
    if (error instanceof ApiClientError && error.status && error.status >= 400 && error.status < 500
        && error.status !== 408 && error.status !== 429) attempt?.finish();
    throw error;
  }
  if (
    quote.plugin_id !== payload.plugin_id ||
    (payload.plugin_version !== undefined && quote.plugin_version !== payload.plugin_version)
  ) {
    const requested = payload.plugin_version === undefined
      ? payload.plugin_id
      : `${payload.plugin_id}@${payload.plugin_version}`;
    throw new ApiClientError(
      `Requested ${requested}, but received a quote for ${quote.plugin_id}@${quote.plugin_version}.`,
    );
  }
  const requestedOffer = payload.offer_id?.trim();
  const quotedOffer = quote.offer_id ?? "default";
  if (requestedOffer && requestedOffer !== quotedOffer) {
    throw new ApiClientError(`Requested offer '${requestedOffer}', but received a quote for '${quotedOffer}'.`);
  }
  attempt?.finish();
  return quote;
}

export function cancelRun(runId: string): Promise<RunRecord> {
  return apiFetch<RunRecord>(`/api/v1/runs/${encodeURIComponent(runId)}/cancel`, { method: "POST" });
}

function runEvents(runId: string, onLog?: (ev: RunLogEvent) => void) {
  const source = typeof EventSource === "undefined" ? null : new EventSource(
    `/api/v1/runs/${encodeURIComponent(runId)}/events`, { withCredentials: true },
  );
  let terminal = false;
  let disconnected = !source;
  let pending = false;
  let wake: (() => void) | undefined;
  const notify = () => { pending = true; wake?.(); };
  source?.addEventListener("terminal", (ev: MessageEvent<string>) => {
    try {
      const event = JSON.parse(ev.data);
      if (terminal || event?.run_id !== runId || !["succeeded", "failed", "canceled"].includes(event.status)) return;
      terminal = true;
      source.close();
      notify();
    } catch { /* Ignore malformed events. Only the status endpoint can supply a result. */ }
  });
  source?.addEventListener("error", () => {
    if (!disconnected && !terminal) { disconnected = true; notify(); }
  });
  source?.addEventListener("open", () => { disconnected = false; });
  if (onLog) source?.addEventListener("log", (ev: MessageEvent<string>) => {
    try {
      const log: RunLogEvent = JSON.parse(ev.data);
      if (log.run_id === runId) onLog(log);
    } catch { /* Ignore malformed log frames. */ }
  });
  return {
    close: () => source?.close(),
    async wait(backoffMs: number, signal: AbortSignal | undefined, atCapacity: boolean) {
      // Older nodes and relays can keep an SSE connection open without sending terminal events.
      const timeout = disconnected || terminal || atCapacity ? backoffMs : 15_000;
      if (!pending && !signal?.aborted) {
        await new Promise<void>((resolve) => {
          const finish = () => {
            clearTimeout(timer);
            signal?.removeEventListener("abort", finish);
            wake = undefined;
            resolve();
          };
          const timer = setTimeout(finish, timeout);
          wake = finish;
          signal?.addEventListener("abort", finish, { once: true });
        });
      }
      pending = false;
    },
  };
}

async function readUntilSettled(
  record: RunRecord, events: ReturnType<typeof runEvents>, followAdmission: boolean,
  signal?: AbortSignal, onUpdate?: (record: RunRecord) => void,
): Promise<RunRecord> {
  const runId = record.run_id;
  const deadline = followAdmission ? Infinity : Date.now() + 30 * 60 * 1000;
  let backoffMs = 500;
  while ((record.status === "running" || record.status === "prepared") &&
         (!record.admission || (followAdmission && record.admission.state === "at_capacity")) &&
         Date.now() < deadline) {
    await events.wait(backoffMs, signal, record.admission?.state === "at_capacity");
    signal?.throwIfAborted();
    try {
      const updated = await fetchRun(runId, signal);
      signal?.throwIfAborted();
      if (updated.run_id !== runId) throw new ApiClientError("Status returned a different run ID.");
      record = updated;
      onUpdate?.(record);
    } catch (err) {
      signal?.throwIfAborted();
      if (!(err instanceof ApiClientError) || (err.status !== 429 && err.status !== 408 &&
          !(err.status && err.status >= 500) && !(err instanceof ApiNetworkError))) throw err;
      // The confirmation has already been accepted. Retry only the authorized status read.
    }
    backoffMs = Math.min(backoffMs * 2, 8000);
  }
  return record;
}

export async function followActiveRun(record: RunRecord, signal: AbortSignal, onUpdate: (r: RunRecord) => void) {
  const events = runEvents(record.run_id);
  try { await readUntilSettled(record, events, true, signal, onUpdate); }
  finally { events.close(); }
}

let runSession = new AbortController();

export async function confirmRun(runId: string, idempotencyKey?: string, onLog?: (ev: RunLogEvent) => void): Promise<RunRecord> {
  // Open before POST so a fast worker can finish during the confirmation round-trip.
  const events = runEvents(runId, onLog);
  const signal = runSession.signal;
  try {
    const record = await apiFetch<RunRecord>(`/api/v1/runs/${encodeURIComponent(runId)}/confirm`, {
      method: "POST", signal, body: JSON.stringify({ idempotency_key: idempotencyKey ?? `idem-${runId}` }),
    });
    if (record.run_id !== runId) throw new ApiClientError("Confirmation returned a different run ID.");
    signal.throwIfAborted();
    return await readUntilSettled(record, events, false, signal);
  } finally { events.close(); }
}

export async function verifyFrogletChain(
  documents: Record<string, unknown>[],
): Promise<FrogletVerifyReport> {
  return apiFetch<FrogletVerifyReport>("/api/v1/froglet/verify", {
    method: "POST",
    body: JSON.stringify({ documents }),
  });
}

export async function fetchLedger(): Promise<LedgerState> {
  return apiFetch<LedgerState>("/api/v1/ledger");
}

export async function fetchSessions(): Promise<SessionRecord[]> {
  const data = await apiFetch<{ sessions: SessionRecord[] }>("/api/v1/sessions");
  return data.sessions;
}

export async function createSession(
  title: string,
  activeGrantId: string,
  projectId?: string,
): Promise<SessionRecord> {
  return apiFetch<SessionRecord>("/api/v1/sessions", {
    method: "POST",
    body: JSON.stringify({
      title,
      active_grant_id: activeGrantId,
      ...(projectId ? { project_id: projectId } : {}),
    }),
  });
}

export async function updateSession(
  sessionId: string,
  patch: {
    title?: string;
    active_grant_id?: string;
    project_id?: string | null;
  },
): Promise<SessionRecord> {
  return apiFetch<SessionRecord>(
    `/api/v1/sessions/${encodeURIComponent(sessionId)}`,
    {
      method: "PATCH",
      body: JSON.stringify(patch),
    },
  );
}

export async function updateSessionGrant(
  sessionId: string,
  activeGrantId: string,
): Promise<SessionRecord> {
  return updateSession(sessionId, { active_grant_id: activeGrantId });
}

export async function renameSession(
  sessionId: string,
  title: string,
): Promise<SessionRecord> {
  return updateSession(sessionId, { title });
}

export async function assignSessionProject(
  sessionId: string,
  projectId: string | null,
): Promise<SessionRecord> {
  return updateSession(sessionId, { project_id: projectId });
}

export async function deleteSession(sessionId: string): Promise<void> {
  await apiFetch<{ deleted: boolean }>(
    `/api/v1/sessions/${encodeURIComponent(sessionId)}`,
    { method: "DELETE" },
  );
}

export async function fetchProjects(): Promise<ProjectRecord[]> {
  const data = await apiFetch<{ projects: ProjectRecord[] }>("/api/v1/projects");
  return data.projects;
}

export async function createProject(name: string): Promise<ProjectRecord> {
  return apiFetch<ProjectRecord>("/api/v1/projects", {
    method: "POST",
    body: JSON.stringify({ name }),
  });
}

export async function renameProject(
  projectId: string,
  name: string,
): Promise<ProjectRecord> {
  return apiFetch<ProjectRecord>(
    `/api/v1/projects/${encodeURIComponent(projectId)}`,
    {
      method: "PATCH",
      body: JSON.stringify({ name }),
    },
  );
}

export async function deleteProject(projectId: string): Promise<void> {
  await apiFetch<{ deleted: boolean }>(
    `/api/v1/projects/${encodeURIComponent(projectId)}`,
    { method: "DELETE" },
  );
}

export async function fetchSessionEvents(sessionId: string): Promise<SessionEvent[]> {
  const data = await apiFetch<{ events: SessionEvent[] }>(
    `/api/v1/sessions/${encodeURIComponent(sessionId)}/events`,
  );
  return data.events;
}

// A self-hosted workbench on a LAN can run outside a secure browser context.
export function newExecutionId(): string {
  if (typeof crypto.randomUUID === "function") return crypto.randomUUID();
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  bytes[6] = (bytes[6]! & 0x0f) | 0x40;
  bytes[8] = (bytes[8]! & 0x3f) | 0x80;
  const hex = Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
  return [hex.slice(0, 8), hex.slice(8, 12), hex.slice(12, 16), hex.slice(16, 20), hex.slice(20)].join("-");
}

export type ExecutionBudget = { mode: "finite"; limit_milli_act: number } | { mode: "unlimited" };

export interface AgentSseEnvelope {
  type:
    | "thought_delta"
    | "text_delta"
    | "tool_call"
    | "tool_result"
    | "module_draft_updated"
    | "run_prepared"
    | "agent_admission"
    | "completed"
    | "error";
  data: unknown;
}

export async function streamAgentChatTurn(
  sessionId: string,
  message: string,
  onEvent: (ev: AgentSseEnvelope) => void,
  activeModuleContext?: Record<string, unknown> | null,
  execution?: { execution_id: string; execution_budget?: ExecutionBudget },
): Promise<void> {
  const response = await fetchWithNetworkError(
    `/api/v1/sessions/${encodeURIComponent(sessionId)}/chat`,
    {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        message,
        ...execution,
        ...(activeModuleContext ? { active_module_context: activeModuleContext } : {}),
      }),
    },
  );
  if (!response.ok) {
    throw await parseHttpError(response);
  }
  if (!response.body) {
    throw new ApiClientError("Chat stream did not return a response body.");
  }
  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let buffer = "";

  while (true) {
    const { value, done } = await reader.read();
    if (done) break;
    buffer += decoder.decode(value, { stream: true });

    let boundary = buffer.indexOf("\n\n");
    while (boundary !== -1) {
      const chunk = buffer.slice(0, boundary);
      buffer = buffer.slice(boundary + 2);
      boundary = buffer.indexOf("\n\n");

      for (const line of chunk.split("\n")) {
        if (line.startsWith("data:")) {
          const rawJson = line.slice(5).trim();
          if (rawJson) {
            const parsed = JSON.parse(rawJson) as AgentSseEnvelope;
            onEvent(parsed);
          }
        }
      }
    }
  }
}

// =============================================================================
// Milestone 4: Authentication, Saved Views, Quarantine Multipart Import,
// Dataset Offers, Org Admin & Platform Control Tower APIs (§8)
// =============================================================================

export async function fetchWhoami(): Promise<AuthPrincipal> {
  return apiFetch<AuthPrincipal>("/api/v1/auth/whoami");
}

export async function loginPassword(payload: {
  email: string;
  password: string;
  totp_code?: string;
  org_id?: string;
}): Promise<{ session_token: string; principal: AuthPrincipal }> {
  return apiFetch<{ session_token: string; principal: AuthPrincipal }>(
    "/api/v1/auth/login",
    {
      method: "POST",
      body: JSON.stringify(payload),
    },
  );
}

export async function registerDemoAccount(payload: {
  email: string;
  password: string;
  display_name?: string;
  org_name?: string;
}): Promise<{ principal: AuthPrincipal }> {
  return apiFetch<{ principal: AuthPrincipal }>("/api/v1/auth/register", {
    method: "POST",
    body: JSON.stringify(payload),
  });
}

export async function acceptOrgInvitation(payload: {
  invite_token: string;
  email: string;
  password?: string;
  display_name?: string;
  totp_code?: string;
}): Promise<{ session_token: string; principal: AuthPrincipal }> {
  return apiFetch<{ session_token: string; principal: AuthPrincipal }>(
    "/api/v1/auth/invitations/accept",
    {
      method: "POST",
      body: JSON.stringify(payload),
    },
  );
}

export async function logoutSession(): Promise<void> {
  runSession.abort();
  runSession = new AbortController();
  await apiFetch<{ logged_out: boolean }>("/api/v1/auth/logout", {
    method: "POST",
  });
}

export async function fetchOidcAuthorizeUrl(orgId?: string): Promise<string> {
  const qs = orgId?.trim() ? `?org_id=${encodeURIComponent(orgId.trim())}` : "";
  const data = await apiFetch<{ authorize_url: string }>(`/api/v1/auth/oidc/authorize${qs}`);
  return data.authorize_url;
}

export async function enrollTotp(): Promise<TotpEnrollment> {
  return apiFetch<TotpEnrollment>("/api/v1/auth/totp/enroll", {
    method: "POST",
    body: JSON.stringify({}),
  });
}

export async function verifyTotp(code: string): Promise<{ enabled: boolean }> {
  return apiFetch<{ enabled: boolean }>("/api/v1/auth/totp/verify", {
    method: "POST",
    body: JSON.stringify({ code: code.trim() }),
  });
}

export async function fetchApiKeys(): Promise<ApiKeyRecord[]> {
  const data = await apiFetch<{ keys: ApiKeyRecord[] }>("/api/v1/auth/api-keys");
  return data.keys;
}

export async function createApiKey(payload: {
  label: string;
  role?: OrgRole | string;
}): Promise<CreatedApiKey> {
  return apiFetch<CreatedApiKey>("/api/v1/auth/api-keys", {
    method: "POST",
    body: JSON.stringify(payload),
  });
}

export async function revokeApiKey(keyId: string): Promise<void> {
  await apiFetch<{ revoked: boolean }>(
    `/api/v1/auth/api-keys/${encodeURIComponent(keyId)}`,
    { method: "DELETE" },
  );
}

export async function fetchOrgMembers(): Promise<OrgMemberRecord[]> {
  const data = await apiFetch<{ members: OrgMemberRecord[] }>("/api/v1/orgs/members");
  return data.members;
}

export async function updateOrgMemberRole(
  userId: string,
  role: OrgRole | string,
): Promise<void> {
  await apiFetch<{ updated: boolean }>(
    `/api/v1/orgs/members/${encodeURIComponent(userId)}`,
    {
      method: "PATCH",
      body: JSON.stringify({ role }),
    },
  );
}

export async function fetchOrgInvitations(): Promise<OrgInvitationRecord[]> {
  const data = await apiFetch<{ invitations: OrgInvitationRecord[] }>(
    "/api/v1/orgs/invitations",
  );
  return data.invitations;
}

export async function createOrgInvitation(payload: {
  email: string;
  role: OrgRole | string;
  org_id?: string;
}): Promise<CreatedOrgInvitation> {
  return apiFetch<CreatedOrgInvitation>("/api/v1/orgs/invitations", {
    method: "POST",
    body: JSON.stringify(payload),
  });
}

export async function fetchAuditLogs(): Promise<AuditLogRecord[]> {
  const data = await apiFetch<{ logs: AuditLogRecord[] }>("/api/v1/audit-logs");
  return data.logs;
}

export async function fetchSavedViews(): Promise<SavedViewRecord[]> {
  const data = await apiFetch<{ saved_views: SavedViewRecord[] }>("/api/v1/saved-views");
  return data.saved_views;
}

export async function createSavedView(payload: {
  name: string;
  catalog_id: string;
  grant_id: string;
  release_version?: number;
  query: QueryAst | CatalogQuery;
}): Promise<SavedViewRecord> {
  return apiFetch<SavedViewRecord>("/api/v1/saved-views", {
    method: "POST",
    body: JSON.stringify({
      name: payload.name,
      catalog_id: payload.catalog_id,
      grant_id: payload.grant_id,
      release_version: payload.release_version,
      query: toQueryAst(payload.query),
    }),
  });
}

export async function deleteSavedView(viewId: string): Promise<void> {
  await apiFetch<{ deleted: boolean }>(
    `/api/v1/saved-views/${encodeURIComponent(viewId)}`,
    { method: "DELETE" },
  );
}

export async function verifySavedView(
  viewId: string,
  force = false,
): Promise<SavedViewVerificationReport> {
  return apiFetch<SavedViewVerificationReport>(
    `/api/v1/saved-views/${encodeURIComponent(viewId)}/verify`,
    {
      method: "POST",
      body: JSON.stringify({ force }),
    },
  );
}

export async function fetchQuarantineUploads(): Promise<QuarantineUploadRecord[]> {
  const data = await apiFetch<{ uploads: QuarantineUploadRecord[] }>(
    "/api/v1/uploads/quarantine",
  );
  return data.uploads;
}

export const MULTIPART_CHUNK_BYTES = 5 * 1024 * 1024;

interface QuarantineParts {
  upload: QuarantineUploadRecord;
  parts: PresignedPartSpec[];
}

export interface UploadImporter {
  row_corrections?: boolean;
  plugin_id: string;
  name: string;
  version: string;
  accept: string[];
  offer_id?: string | null;
}

export function fetchUploadImporters(location: "local" | "cloud" = "cloud") {
  const endpoint = location === "local" ? "/api/v1/local/importers" : "/api/v1/uploads/quarantine/importers";
  return apiFetch<{ importers: UploadImporter[] }>(endpoint);
}

export interface LocalImportResult {
  source_sha256: string;
  catalog: { id: string; version: string; dataset_digest: string } | null;
  report: { valid: boolean; diagnostics?: { code?: string; message?: string }[] };
  source?: Extract<ReleaseChangeSource, { kind: "validated_upload" }>;
}

export function importLocalDataset(importer: Pick<UploadImporter, "plugin_id" | "version">, file: File, name: string, forAppend = false) {
  const module = importer.plugin_id.split("/").map(encodeURIComponent).join("/");
  const query = new URLSearchParams({ filename: file.name, version: importer.version, dataset_name: name });
  if (forAppend) query.set("for_append", "true");
  const owner = localOwnerRequest();
  return apiFetch<LocalImportResult>(`/api/v1/local/importers/${module}/import?${query}`, {
    ...owner, method: "POST", body: file, headers: { ...owner.headers, "Content-Type": "application/octet-stream" },
  });
}

export function startPrivateUpload(
  file: File, targetCatalogId: string, partSizes: number[], format: "hla_bundle" | "module" | "opaque",
  importer?: UploadImporter,
) {
  return apiFetch<QuarantineParts>("/api/v1/uploads/quarantine/init", {
    method: "POST",
    body: JSON.stringify({
      filename: file.name.replace(/[^a-zA-Z0-9._-]/g, "_").slice(0, 120),
      format,
      importer: importer && { plugin_id: importer.plugin_id, version: importer.version, offer_id: importer.offer_id },
      mode: "replace",
      target_catalog_id: targetCatalogId,
      part_sizes: partSizes,
    }),
  });
}

function quarantineAction(uploadId: string, action: string) {
  return `/api/v1/uploads/quarantine/${encodeURIComponent(uploadId)}/${action}`;
}

export function releaseAttachmentUrl(uploadId: string, version: number, path: string) {
  const query = new URLSearchParams({ version: String(version), path });
  return `${quarantineAction(uploadId, "attachments")}?${query}`;
}

export function resumePrivateUpload(uploadId: string) {
  return apiFetch<QuarantineParts & { completed_parts: (QuarantineCompletedPart & { size_bytes: number })[] }>(
    quarantineAction(uploadId, "resume"), { method: "POST", body: "{}" },
  );
}

export async function putPrivateUploadPart(part: PresignedPartSpec, data: Blob) {
  const response = await fetch(part.url, {
    method: "PUT", mode: "cors", credentials: "omit", body: data,
  }).catch(() => {
    throw new ApiClientError("Could not reach file storage. Reconnect to storage and resume the upload.");
  });
  if (!response.ok) throw new ApiClientError(`Storage rejected part ${part.part_number} (HTTP ${response.status}).`);
  const etag = response.headers.get("etag")?.trim();
  if (!etag) throw new ApiClientError(`Storage did not expose an ETag for part ${part.part_number}.`);
  return { part_number: part.part_number, etag };
}

export function completePrivateUpload(uploadId: string, parts: QuarantineCompletedPart[]) {
  return apiFetch<{ upload: QuarantineUploadRecord }>(quarantineAction(uploadId, "complete"), {
    method: "POST", body: JSON.stringify({ parts }),
  });
}

export function validatePrivateDraft(upload: QuarantineUploadRecord, patch?: import("./types").DraftCorrection) {
  return apiFetch<{ upload: QuarantineUploadRecord }>(quarantineAction(upload.upload_record_id, "draft"), {
    method: "POST", body: JSON.stringify({ revision: upload.draft_corrections?.length ?? 0, patch }),
  });
}

export function publishPrivateUpload(uploadId: string) {
  return apiFetch<HostedHlaPublication | { catalog: null; upload: QuarantineUploadRecord }>(quarantineAction(uploadId, "ingest"), { method: "POST", body: "{}" });
}

export interface ImportCatalogFileOptions {
  mode: "local" | "cloud";
  file: File;
  format: IngestFormat;
  ingestMode: IngestMode;
  targetCatalogId: string;
  targetTable?: string;
  catalogName?: string;
  catalogDescription?: string;
  organism?: string;
  primaryKey: string[];
  onProgress?: (stage: string, fraction: number) => void;
}

export async function importCatalogFile(
  opts: ImportCatalogFileOptions,
): Promise<{ catalog: CatalogManifest; upload?: QuarantineUploadRecord }> {
  if (opts.file.size === 0) {
    throw new ApiClientError("Selected file is empty (0 bytes).");
  }

  const sanitizedFileName =
    opts.file.name.replace(/[^a-zA-Z0-9._-]/g, "_").slice(0, 120) || "table.csv";

  if (opts.mode === "local") {
    opts.onProgress?.("Validating & sealing local DuckLake snapshot…", 0.35);
    const qs = new URLSearchParams({
      filename: sanitizedFileName,
      format: opts.format,
      mode: opts.ingestMode,
      target_catalog_id: opts.targetCatalogId,
    });
    if (opts.targetTable?.trim()) qs.set("target_table", opts.targetTable.trim());
    if (opts.catalogName?.trim()) qs.set("catalog_name", opts.catalogName.trim());
    if (opts.catalogDescription?.trim()) {
      qs.set("catalog_description", opts.catalogDescription.trim());
    }
    if (opts.organism?.trim()) qs.set("organism", opts.organism.trim());
    if (opts.primaryKey.length > 0) {
      qs.set("primary_key", opts.primaryKey.join(","));
    }

    const buf = await opts.file.arrayBuffer();
    const resp = await fetchWithNetworkError(`/api/v1/catalogs/import-local?${qs.toString()}`, {
      method: "POST",
      headers: {
        "Content-Type": "application/octet-stream",
        ...localOwnerHeaders(),
      },
      body: buf,
    });
    if (!resp.ok) {
      throw await parseHttpError(resp);
    }
    opts.onProgress?.("Sealed DuckLake snapshot committed.", 1);
    return (await resp.json()) as { catalog: CatalogManifest };
  }

  // Cloud mode: Direct-to-S3 SigV4 Multipart Upload -> Complete -> Airgapped Stage 1/2 Ingest
  const partSizes: number[] = [];
  let offset = 0;
  while (offset < opts.file.size) {
    const end = Math.min(offset + MULTIPART_CHUNK_BYTES, opts.file.size);
    partSizes.push(end - offset);
    offset = end;
  }

  opts.onProgress?.("Initiating presigned S3 quarantine upload…", 0.08);
  const initResp = await apiFetch<{
    upload: QuarantineUploadRecord;
    parts: PresignedPartSpec[];
  }>("/api/v1/uploads/quarantine/init", {
    method: "POST",
    body: JSON.stringify({
      filename: sanitizedFileName,
      format: opts.format,
      mode: opts.ingestMode,
      target_catalog_id: opts.targetCatalogId,
      target_table: opts.targetTable?.trim() || "records",
      ...(opts.catalogName?.trim() ? { catalog_name: opts.catalogName.trim() } : {}),
      ...(opts.catalogDescription?.trim()
        ? { catalog_description: opts.catalogDescription.trim() }
        : {}),
      ...(opts.organism?.trim() ? { organism: opts.organism.trim() } : {}),
      primary_key: opts.primaryKey,
      part_sizes: partSizes,
    }),
  });

  const completedParts: { part_number: number; etag: string }[] = [];
  let byteCursor = 0;
  for (let i = 0; i < initResp.parts.length; i += 1) {
    const part = initResp.parts[i]!;
    const sliceBuf = await opts.file
      .slice(byteCursor, byteCursor + part.content_length)
      .arrayBuffer();
    byteCursor += part.content_length;

    opts.onProgress?.(
      `Uploading part ${part.part_number}/${initResp.parts.length} directly to S3 quarantine…`,
      0.12 + 0.48 * ((i + 0.5) / initResp.parts.length),
    );

    let s3Resp: Response;
    try {
      s3Resp = await fetch(part.url, {
        method: "PUT",
        mode: "cors",
        credentials: "omit",
        body: sliceBuf,
      });
    } catch (err: unknown) {
      await fetchWithNetworkError(
        `/api/v1/uploads/quarantine/${encodeURIComponent(initResp.upload.upload_record_id)}/abort`,
        { method: "POST", headers: { "Content-Type": "application/json" }, body: "{}" },
      ).catch(() => undefined);
      throw new ApiClientError(
        `Direct S3 quarantine upload failed on part ${part.part_number}: ${formatErrorInline(err)}`,
      );
    }

    if (!s3Resp.ok) {
      await fetchWithNetworkError(
        `/api/v1/uploads/quarantine/${encodeURIComponent(initResp.upload.upload_record_id)}/abort`,
        { method: "POST", headers: { "Content-Type": "application/json" }, body: "{}" },
      ).catch(() => undefined);
      throw new ApiClientError(
        `S3 rejected quarantine part ${part.part_number} (HTTP ${s3Resp.status}).`,
      );
    }

    const etag = s3Resp.headers.get("etag") ?? s3Resp.headers.get("ETag") ?? "";
    if (!etag.trim()) {
      throw new ApiClientError(
        `S3 upload part ${part.part_number} succeeded without exposing an ETag header.`,
      );
    }
    completedParts.push({ part_number: part.part_number, etag: etag.trim() });
  }

  opts.onProgress?.("Completing S3 multipart upload & verifying storage quota…", 0.68);
  await apiFetch<{ upload: QuarantineUploadRecord }>(
    `/api/v1/uploads/quarantine/${encodeURIComponent(initResp.upload.upload_record_id)}/complete`,
    {
      method: "POST",
      body: JSON.stringify({ parts: completedParts }),
    },
  );

  opts.onProgress?.(
    "Running airgapped validator container & sealing DuckLake snapshot…",
    0.84,
  );
  const ingested = await apiFetch<{
    upload: QuarantineUploadRecord;
    catalog: CatalogManifest;
  }>(
    `/api/v1/uploads/quarantine/${encodeURIComponent(initResp.upload.upload_record_id)}/ingest`,
    {
      method: "POST",
      body: JSON.stringify({}),
    },
  );
  opts.onProgress?.("Sealed DuckLake snapshot committed.", 1);
  return ingested;
}

export async function fetchDatasetOffers(): Promise<DatasetOfferRecord[]> {
  const data = await apiFetch<{ offers: DatasetOfferRecord[] }>("/api/v1/dataset-offers");
  return data.offers;
}

export async function publishDatasetOffer(payload: {
  catalog_id: string;
  scope: DatasetOfferScope;
  allowed_orgs: string[];
  approval_required: boolean;
  hosting_mode: DatasetHostingMode;
  node_id?: string | null;
  title: string;
  description: string;
  default_level: GrantAccessLevel;
  row_scope?: FilterExpr | null;
  daily_row_budget?: number | null;
  max_rows_per_query: number;
  price_milli_act: number;
}): Promise<DatasetOfferRecord> {
  return apiFetch<DatasetOfferRecord>("/api/v1/dataset-offers", {
    method: "POST",
    body: JSON.stringify(payload),
  });
}

export interface DatasetOfferVersions {
  versions: { snapshot_id: number; dataset_digest: string; release_version?: number | null; catalog_id?: string }[];
  available_snapshots: number[];
  available_releases?: { release_version: number; catalog_id: string; snapshot_id: number; dataset_digest: string }[];
}

export function fetchDatasetOfferVersions(offerId: string): Promise<DatasetOfferVersions> {
  return apiFetch(`/api/v1/dataset-offers/${encodeURIComponent(offerId)}/versions`);
}

export function addDatasetOfferVersion(offerId: string, version: { snapshot_id: number } | { release_version: number }) {
  return apiFetch(`/api/v1/dataset-offers/${encodeURIComponent(offerId)}/versions`, {
    method: "POST", body: JSON.stringify(version),
  });
}

export async function subscribeDatasetOffer(
  offerId: string,
): Promise<DatasetOfferSubscription> {
  return apiFetch<DatasetOfferSubscription>(
    `/api/v1/dataset-offers/${encodeURIComponent(offerId)}/subscribe`,
    {
      method: "POST",
      body: JSON.stringify({}),
    },
  );
}

export async function fetchDatasetOfferRequests(): Promise<DatasetOfferRequest[]> {
  return (await apiFetch<{requests: DatasetOfferRequest[]}>("/api/v1/dataset-offers/requests")).requests;
}

export function approveDatasetOffer(offerId: string, subscriberOrgId: string) {
  return apiFetch<DatasetOfferSubscription>(`/api/v1/dataset-offers/${encodeURIComponent(offerId)}/approve`, {
    method: "POST", body: JSON.stringify({subscriber_org_id: subscriberOrgId}),
  });
}

export async function fetchModuleOffers(): Promise<ModuleOffer[]> {
  const data = await apiFetch<{ offers: ModuleOffer[] }>("/api/v1/module-offers");
  return data.offers;
}

export function fetchFederatedModuleOffers() {
  return apiFetch<{ offers: FederatedModuleOffer[]; unavailable: string[] }>("/api/v1/auth/federation/offers");
}

export async function fetchModuleOfferRequests(): Promise<ModuleOfferRequest[]> {
  const data = await apiFetch<{ requests: ModuleOfferRequest[] }>("/api/v1/module-offers/requests");
  return data.requests;
}

export function publishModuleOffer(payload: Pick<ModuleOffer,
  "plugin_id" | "version" | "action" | "title" | "scope" | "approval_required"
> & { allowed_orgs: string[] }) {
  return apiFetch<{ offer_id: string }>("/api/v1/module-offers", {
    method: "POST", body: JSON.stringify(payload),
  });
}

function moduleOfferPath(id: string): string {
  return `/api/v1/module-offers/${encodeURIComponent(id)}`;
}

export function subscribeModuleOffer(id: string) {
  return apiFetch(`${moduleOfferPath(id)}/subscribe`, { method: "POST" });
}

export function approveModuleOffer(id: string, subscriberOrgId: string) {
  return apiFetch(`${moduleOfferPath(id)}/approve`, {
    method: "POST", body: JSON.stringify({ subscriber_org_id: subscriberOrgId }),
  });
}

export async function fetchOfferedRunModule(offer: ModuleOffer): Promise<PluginSummary> {
  const plugin = await apiFetch<PluginSummary>(
    `/api/v1/workbench/module-offers/${encodeURIComponent(offer.offer_id)}/run-module`,
  );
  if (plugin.manifest.id !== offer.plugin_id || plugin.manifest.version !== offer.version) {
    throw new ApiClientError("The module release returned by the provider differs from the selected offer.");
  }
  return { ...plugin, marketplace_offer_id: offer.offer_id };
}

export function offeredModuleDownloadUrl(offer: ModuleOffer, item: "package" | "image"): string {
  return `${workspaceModulePath(offer.plugin_id)}/versions/${encodeURIComponent(offer.version)}/${item}?` +
    new URLSearchParams({ marketplace_offer_id: offer.offer_id }).toString();
}

export async function fetchOrgPluginVersions(
  visibility?: PluginVisibility,
): Promise<PluginVersionRecord[]> {
  const qs = visibility ? `?visibility=${encodeURIComponent(visibility)}` : "";
  const data = await apiFetch<{ versions: PluginVersionRecord[] }>(
    `/api/v1/plugins/versions${qs}`,
  );
  return data.versions;
}

export async function fetchAdminPluginVersions(
  visibility?: PluginVisibility,
): Promise<PluginVersionRecord[]> {
  const qs = visibility ? `?visibility=${encodeURIComponent(visibility)}` : "";
  const data = await apiFetch<{ versions: PluginVersionRecord[] }>(
    `/api/v1/admin/plugins/versions${qs}`,
  );
  return data.versions;
}

export async function reviewAdminPluginVisibility(
  pluginId: string,
  version: string,
  payload: {
    visibility: PluginVisibility;
    approve_curated?: boolean;
    note?: string;
  },
): Promise<PluginVersionRecord> {
  const parts = pluginId.split("/");
  if (parts.length !== 2 || !parts[0] || !parts[1]) {
    throw new Error(`Invalid plugin ID: ${pluginId}`);
  }
  const [ns, slug] = parts;
  const data = await apiFetch<{ plugin_version: PluginVersionRecord }>(
    `/api/v1/admin/plugins/${encodeURIComponent(ns)}/${encodeURIComponent(slug)}/versions/${encodeURIComponent(version)}/visibility`,
    {
      method: "POST",
      body: JSON.stringify(payload),
    },
  );
  return data.plugin_version;
}

export function createResultShare(runId: string, artifactName: string, recipientOrgId: string) {
  return apiFetch<ResultShare>("/api/v1/result-shares", {
    method: "POST",
    body: JSON.stringify({ run_id: runId, artifact_name: artifactName, recipient_org_id: recipientOrgId }),
  });
}

export async function fetchResultShares() {
  return apiFetch<{ shares: ResultShare[]; providers?: { label: string; launch_url: string }[] }>("/api/v1/result-shares");
}

export async function downloadResultShare(share: ResultShare): Promise<Blob> {
  if (!Number.isSafeInteger(share.byte_length) || share.byte_length < 0 || share.byte_length > 8 * 1024 * 1024 ||
      !/^[a-f0-9]{64}$/i.test(share.sha256)) {
    throw new Error("Invalid shared file size or checksum.");
  }
  if (!globalThis.crypto?.subtle) {
    throw new Error("Open the Workbench over HTTPS or localhost to verify this download.");
  }
  const response = await fetchWithNetworkError(`/api/v1/result-shares/${encodeURIComponent(share.share_id)}/download`, {
    cache: "no-store", credentials: "same-origin",
  });
  if (!response.ok) throw await parseHttpError(response);
  if (!response.body) throw new Error("The shared file has no response body.");
  const reader = response.body.getReader();
  const bytes = new Uint8Array(share.byte_length);
  let size = 0;
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      if (size + value.byteLength > bytes.length) {
        await reader.cancel();
        throw new Error("The downloaded file exceeds the published size.");
      }
      bytes.set(value, size);
      size += value.byteLength;
    }
  } finally {
    reader.releaseLock();
  }
  if (size !== bytes.length) throw new Error("The downloaded file size differs from the published size.");
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", bytes));
  const sha256 = Array.from(digest, byte => byte.toString(16).padStart(2, "0")).join("");
  if (sha256 !== share.sha256.toLowerCase()) throw new Error("The downloaded file checksum differs from the published checksum.");
  return new Blob([bytes], { type: "application/octet-stream" });
}

export async function revokeResultShare(id: string) {
  const response = await fetchWithNetworkError(`/api/v1/result-shares/${encodeURIComponent(id)}`, { method: "DELETE" });
  if (!response.ok) throw await parseHttpError(response);
}

export async function fetchAdminTiers(): Promise<TierDefinitionRecord[]> {
  const data = await apiFetch<{ tiers: TierDefinitionRecord[] }>("/api/v1/admin/tiers");
  return data.tiers;
}

export async function updateAdminTier(
  tierId: string,
  input: UpdateTierInput,
): Promise<TierDefinitionRecord> {
  const data = await apiFetch<{ tier: TierDefinitionRecord }>(
    `/api/v1/admin/tiers/${encodeURIComponent(tierId)}`,
    {
      method: "PUT",
      body: JSON.stringify(input),
    },
  );
  return data.tier;
}

export function confirmDemoTier(orgId: string, tierId: string) {
  return apiFetch<{ org_id: string; tier_id: string; changed: boolean }>(
    `/api/v1/admin/orgs/${encodeURIComponent(orgId)}/confirm-tier`,
    { method: "POST", body: JSON.stringify({ tier_id: tierId }) },
  );
}

export async function fetchCuratedModuleApprovals(): Promise<CuratedModuleApprovalRecord[]> {
  const data = await apiFetch<{ approvals: CuratedModuleApprovalRecord[] }>(
    "/api/v1/admin/curated-modules",
  );
  return data.approvals;
}

export async function approveCuratedModule(payload: {
  plugin_id: string;
  version: string;
  image_digest: string;
}): Promise<CuratedModuleApprovalRecord> {
  return apiFetch<CuratedModuleApprovalRecord>("/api/v1/admin/curated-modules/approve", {
    method: "POST",
    body: JSON.stringify(payload),
  });
}

export async function revokeCuratedModule(payload: {
  plugin_id: string;
  version: string;
  image_digest: string;
}): Promise<void> {
  await apiFetch<{ revoked: boolean }>("/api/v1/admin/curated-modules", {
    method: "DELETE",
    body: JSON.stringify(payload),
  });
}

export async function fetchNodes(): Promise<NodeSummaryRecord[]> {
  const data = await apiFetch<{ nodes: NodeSummaryRecord[] }>("/api/v1/nodes");
  return data.nodes;
}

export async function enrollNode(payload: {
  node_id: string;
  org_id?: string;
  provider_pubkey: string;
  mode: NodeMode;
}): Promise<CreatedNodeEnrollment> {
  return apiFetch<CreatedNodeEnrollment>("/api/v1/nodes/enroll", {
    method: "POST",
    body: JSON.stringify(payload),
  });
}

export async function revokeNode(nodeId: string): Promise<void> {
  await apiFetch<{ revoked: boolean }>(
    `/api/v1/nodes/${encodeURIComponent(nodeId)}/revoke`,
    {
      method: "POST",
      body: JSON.stringify({}),
    },
  );
}

export async function creditOrgInvoice(payload: {
  org_id?: string;
  amount_milli_act: number;
  reference: string;
}): Promise<LedgerTx> {
  return apiFetch<LedgerTx>("/api/v1/billing/invoices/credit", {
    method: "POST",
    body: JSON.stringify(payload),
  });
}

export async function fetchEnumerationMonitor(days = 30): Promise<{
  lookback_days: number;
  entries: EnumerationMonitorEntry[];
}> {
  return apiFetch<{ lookback_days: number; entries: EnumerationMonitorEntry[] }>(
    `/api/v1/admin/enumeration-monitor?days=${encodeURIComponent(String(days))}`,
  );
}
