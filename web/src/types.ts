export type MarketplaceSyncState = "air-gapped" | "disconnected" | "connected" | "stale";

export type MarketplaceConsentDecision = "unset" | "accepted" | "declined";

export type WorkspaceHubMode = "offline" | "alithea_cloud" | "custom_cloud";

export interface WorkspaceHubStatus {
  mode: WorkspaceHubMode;
  hub_url?: string | null;
  default_hub_url: string;
  connected: boolean;
  airgapped: boolean;
  authenticated: boolean;
  auth_actor?: string | null;
  remote_module_count: number;
  last_error?: string | null;
  recent_hubs?: string[];
  /** Pinned hub signing key (`U-1`); compare it with the key the hub operator publishes. */
  hub_key?: string | null;
  /** Key the hub presented when it differs from the pinned key. */
  presented_hub_key?: string | null;
  hub_key_status?: "unpinned" | "pinned" | "changed";
}

export interface SubscribedNetworkOffer {
  offer_id: string;
  hub_url: string;
  plugin_id: string;
  version: string;
  title: string;
  description?: string;
  kind: "run" | "download" | string;
  provider_label: string;
  ui_bundle_hash?: string | null;
  ui_bundle_size_bytes?: number | null;
  launch_url?: string | null;
  price_milli_eur?: number;
}

export interface MarketplaceOfferSummary {
  id: string;
  kind: string;
  title: string;
  version?: string | null;
  provider_org?: string | null;
  plugin_id?: string | null;
  ui_bundle_hash?: string | null;
  ui_bundle_size_bytes?: number | null;
  launch_url?: string | null;
  price_milli_eur?: number | null;
  description?: string | null;
}

export interface MarketplaceStatus {
  state: MarketplaceSyncState;
  consent: MarketplaceConsentDecision;
  consent_source: string;
  prompt_required: boolean;
  airgapped: boolean;
  hub_url: string;
  default_hub_url: string;
  decided_at?: string | null;
  last_synced_at?: string | null;
  last_error?: string | null;
  feed_offer_count: number;
  offers_preview?: MarketplaceOfferSummary[];
}

export interface HealthStatus {
  status: string;
  version: string;
  mode: string;
  node_id: string;
  provider_id: string;
  requester_id: string;
  container_engine: string;
  llm_configured: boolean;
  llm_model: string | null;
  catalog_count: number;
  plugin_count: number;
  workspace_hub_url?: string | null;
  workspace_hub?: WorkspaceHubStatus | null;
  marketplace_prompt_required?: boolean;
  marketplace_status?: MarketplaceStatus | null;
}

export type GrantAccessLevel = "none" | "compute_only" | "rows";

export type ScalarValue = string | number | boolean;

export type FilterExpr =
  | {
      op: "eq" | "neq" | "gt" | "gte" | "lt" | "lte";
      column: string;
      value: ScalarValue;
    }
  | {
      op: "starts_with";
      column: string;
      prefix: string;
    }
  | {
      op: "in";
      column: string;
      values: ScalarValue[];
    }
  | {
      op: "is_null" | "is_not_null";
      column: string;
    }
  | {
      op: "and" | "or";
      exprs: FilterExpr[];
    };

export interface OrderBySpec {
  column: string;
  direction?: "asc" | "desc";
}

export interface AttachedCatalogPin {
  catalog_id: string;
  grant_id: string;
  snapshot_id?: number | null;
}

export interface QueryAst {
  table?: string | null;
  columns?: string[];
  filter?: FilterExpr | null;
  order_by?: OrderBySpec[];
  limit?: number;
  offset?: number;
  snapshot_id?: number | null;
  additional_sources?: AttachedCatalogPin[];
}

export interface DatasetGrant {
  grant_id: string;
  label: string;
  org_id: string;
  catalog_id: string;
  level: GrantAccessLevel;
  row_scope?: FilterExpr | null;
  daily_row_budget?: number | null;
  max_rows_per_query: number;
  allow_local_export: boolean;
}

export interface GrantUsageSummary {
  grant_id: string;
  daily_row_budget: number | null;
  rows_used_today: number;
  grant: DatasetGrant;
}

export interface CatalogShard {
  relative_path: string;
  sha256: string;
  size_bytes?: number;
  row_count: number;
  delete_sha256?: string | null;
  deleted_row_count?: number;
}

export interface ColumnDescriptor {
  name: string;
  data_type: string;
  nullable: boolean;
}

export interface TableSummary {
  name: string;
  row_count: number;
  primary_key: string[];
  columns: ColumnDescriptor[];
}

export interface SnapshotOverview {
  snapshot_id: number;
  dataset_digest: string;
  row_count: number;
  created_at: string;
  commit_message?: string;
  pinned?: boolean;
}

export type CatalogLocation =
  | { kind: "workspace"; node_id: string }
  | { kind: "cloud"; operator_org_id: string; tenancy_org_id: string }
  | { kind: "network_node"; node_id: string; operator_org_id: string; tenancy_org_id: string; online: boolean };

export interface CatalogSummary {
  release_version?: number;
  offer_grant_ids?: string[];
  location?: CatalogLocation;
  id: string;
  name: string;
  version: string;
  description: string;
  organism?: string | null;
  latest_snapshot_id: number;
  dataset_digest: string;
  row_count: number;
  tables: TableSummary[];
  facets: Record<string, string[]>;
  snapshots: SnapshotOverview[];
  shards?: CatalogShard[];
}

export type CatalogManifest = CatalogSummary;

export interface PeptideRecord {
  peptide_id: string;
  sequence: string;
  peptide_length: number;
  hla_allele: string;
  gene_symbol: string;
  organ: string;
  condition: string;
  affinity_nm: number;
  eluted_rank_pct: number;
  ms_intensity_log2: number;
  donor_count: number;
  [extraColumn: string]: unknown;
}

export interface CatalogQuery {
  table?: string;
  organs?: string[];
  conditions?: string[];
  hla_alleles?: string[];
  gene_symbols?: string[];
  min_length?: number;
  max_length?: number;
  max_affinity_nm?: number;
  sequence_prefix?: string;
  limit?: number;
  snapshot_id?: number;
}

export interface QueryTableResult<TRow = PeptideRecord> {
  catalog_id: string;
  snapshot_id: number;
  dataset_digest: string;
  table: string;
  grant_id: string;
  access_level: GrantAccessLevel;
  columns: ColumnDescriptor[];
  total_in_scope: number;
  rows: TRow[];
  remaining_daily_row_budget?: number | null;
}

export type CatalogQueryResult = QueryTableResult<PeptideRecord>;

export interface DeclarativeViewSpec {
  id: string;
  kind: "metric_cards" | "bar" | "length_distribution" | "table" | string;
  title: string;
  data_pointer: string;
}

export interface PluginOfferSpec {
  id: string;
  name: string;
  price_milli_eur: number;
  base_fee_milli_eur: number;
  max_input_bytes: number;
  max_output_bytes: number;
  max_runtime_sec?: number | null;
  max_memory_mb?: number | null;
}

export interface ResolvedPluginOffer {
  id: string;
  name: string;
  price_milli_eur: number;
  base_fee_milli_eur: number;
  max_input_bytes: number;
  max_output_bytes: number;
  max_runtime_sec: number;
  max_memory_mb: number;
}

export interface PluginManifest {
  id: string;
  name: string;
  version: string;
  author?: string | null;
  description: string;
  category: string;
  icon?: string | null;
  execution?: "local" | "cloud_only" | "cloud" | "either" | string | null;
  requires_catalog_level?: GrantAccessLevel;
  image: string;
  network: string | { allow_domains: string[] };
  price_milli_eur: number;
  max_child_spend_milli_eur?: number | null;
  offers?: PluginOfferSpec[];
  resources: {
    cpus: number;
    memory_mb: number;
    gpu: number;
    timeout_sec: number;
    pids_limit: number;
  };
  inputs: {
    schema: Record<string, unknown>;
  };
  views: DeclarativeViewSpec[];
  ui?: {
    entry: string;
  } | null;
  documentation_url?: string | null;
  video_url?: string | null;
  publication_url?: string | null;
}

export interface SkillDoc {
  name: string;
  description: string;
  markdown_body: string;
}

export interface ModuleDownload {
  plugin_id: string;
  name: string;
  version: string;
  image_digest: string;
}

export type ModuleOfferAction = "run" | "download";
export type ModuleOfferScope = "private" | "custom_shared" | "global";
export type ModuleOfferAccess = "granted" | "pending" | "request_required";

export interface ModuleOffer {
  offer_id: string;
  publisher_org_id: string;
  plugin_id: string;
  version: string;
  title: string;
  action: ModuleOfferAction;
  scope: ModuleOfferScope;
  approval_required: boolean;
  serving_org_id?: string | null;
  target_node_id?: string | null;
  access?: ModuleOfferAccess;
  ui_bundle_hash?: string | null;
  ui_bundle_size_bytes?: number | null;
}

export type ModuleSource = "workspace" | "org_cloud" | "network_provider";
export type ModuleRunsIn = "local" | "cloud" | "network_provider";

export interface PluginSummary {
  marketplace_offer_id?: string;
  manifest: PluginManifest;
  skill: SkillDoc | null;
  views: DeclarativeViewSpec[];
  effective_offers?: ResolvedPluginOffer[];
  has_custom_ui: boolean;
  ui_entry_url: string | null;
  author: string;
  icon_url: string | null;
  execution_target: "local" | "froglet_node" | "alithea_cloud" | string;
  execution_label: string;
  source?: ModuleSource;
  runs_in?: ModuleRunsIn;
  ui_bundle_hash?: string | null;
  ui_bundle_size_bytes?: number | null;
  can_install?: boolean;
  installed_locally?: boolean;
  provider_label?: string | null;
  launch_url?: string | null;
  hub_origin?: string | null;
  installed: boolean;
  removable_versions?: string[];
  detached_versions?: string[];
  requires_catalog_level?: GrantAccessLevel;
}

export interface RunQuote {
  run_id: string;
  plugin_id: string;
  plugin_version: string;
  image_ref: string;
  target_node_id: string;
  offer_id?: string;
  offer_name?: string;
  base_fee_milli_eur?: number;
  estimated_cost_milli_eur: number;
  max_input_bytes?: number;
  max_output_bytes?: number;
  max_runtime_sec?: number;
  max_memory_mb?: number;
  workload_hash: string;
  dataset_grant_summary: string | null;
  execution_notice?: string | null;
  serving_org_id?: string | null;
  external_provider_org?: string | null;
  external_provider_notice?: string | null;
  froglet_quote: Record<string, unknown>;
  expires_at: string;
}

export interface ArtifactRef {
  name: string;
  blob_key: string;
  media_type: string;
  size_bytes: number;
  sha256: string;
}

export type RunStatus = "prepared" | "running" | "succeeded" | "failed" | "canceled";

export type AdmissionState = "at_capacity" | "expired" | "canceled" | "rejected";

export function runStatusLabel(run: RunRecord): string {
  switch (run.admission?.state) {
    case "at_capacity": return "At capacity";
    case "expired": return "Approval expired";
    case "canceled": return "Canceled";
    case "rejected": return "Approval rejected";
    default: return run.status;
  }
}

export interface RunInput {
  alias: string;
  source_run_id: string;
  name: string;
  sha256: string;
  size_bytes: number;
  source_receipt_hash: string;
}

export interface RunRecord {
  run_id: string;
  org_id: string;
  session_id: string | null;
  plugin_id: string;
  plugin_version: string;
  status: RunStatus;
  admission?: { state: AdmissionState; requested_at: string } | null;
  params: Record<string, unknown>;
  grant: DatasetGrant | null;
  inputs?: RunInput[];
  parent_run_id?: string | null;
  agent_credential_id?: string | null;
  agent_tool_call_id?: string | null;
  module_fee_milli_eur?: number | null;
  dataset_fee_milli_eur?: number | null;
  quote: RunQuote;
  result_json: Record<string, unknown> | null;
  artifacts: ArtifactRef[];
  froglet_receipt: Record<string, unknown> | null;
  froglet_chain: Record<string, unknown>[];
  receipt_verified: boolean;
  logs: string[];
  error_message: string | null;
  created_at: string;
  completed_at: string | null;
}

export interface RunLogEvent {
  run_id: string;
  timestamp: string;
  stream: string;
  line: string;
}

export interface FrogletVerifyReport {
  valid: boolean;
  chain_evaluated: boolean;
  artifacts: {
    index: number;
    kind: string | null;
    valid: boolean;
    findings: { code: string; message: string }[];
  }[];
  chain?: {
    valid: boolean;
    findings: { code: string; message: string }[];
  } | null;
}

export interface RunVerification extends FrogletVerifyReport {
  runId: string;
}

export interface LedgerTx {
  tx_id: string;
  org_id: string;
  run_id: string | null;
  kind: string;
  delta_milli_eur: number;
  balance_after_milli_eur: number;
  receipt_hash?: string | null;
  reference: string;
  created_at: string;
}

export interface LedgerState {
  org_id: string;
  available_milli_eur: number;
  reserved_milli_eur?: number;
  available_display?: string;
  daily_spent_milli_eur?: number;
  daily_spend_breaker_milli_eur?: number | null;
  expires_at?: string | null;
  transactions: LedgerTx[];
}

export interface ProjectRecord {
  project_id: string;
  org_id: string;
  name: string;
  created_at: string;
  updated_at: string;
}

export interface SessionRecord {
  session_id: string;
  org_id: string;
  project_id: string | null;
  title: string;
  active_grant_id: string;
  created_at: string;
  updated_at: string;
}

export interface SessionEvent {
  event_id: string;
  session_id: string;
  seq: number;
  kind: string;
  content: string;
  metadata: Record<string, unknown>;
  created_at: string;
}

export interface ScaffoldResult {
  plugin_id: string;
  template: string;
  directory: string;
  registered: boolean;
  files: { relative_path: string; content: string }[];
}

export interface InstalledIde {
  id: string;
  label: string;
  kind: string;
  path: string;
}

export type LlmWireProtocol =
  | "open_ai_chat_completions"
  | "anthropic_messages"
  | "acp_stdio"
  | "claude_code_cli"
  | "codex_cli";

export type BackendAuthState = "signed_in" | "needs_login" | "ready" | "not_installed";

export interface ProviderOptionChoice {
  value: string;
  label: string;
  description?: string;
}

export interface ProviderControlSpec {
  id: string;
  label: string;
  category: string;
  current_value: string;
  options: ProviderOptionChoice[];
}

export interface RedactedLlmConfig {
  protocol: LlmWireProtocol;
  base_url: string;
  model: string;
  max_tokens: number;
  command: string;
  args: string[];
  config_options?: Record<string, string>;
  has_api_key: boolean;
  api_key_hint: string | null;
  display_name: string;
  is_local_cli: boolean;
  provider_id?: string;
}

export interface DiscoveredLlmBackend {
  id: string;
  label: string;
  subtitle: string;
  protocol: LlmWireProtocol;
  command: string;
  default_args: string[];
  installed: boolean;
  resolved_path: string | null;
  auth_state: BackendAuthState;
  auth_detail: string;
  login_command: string | null;
  suggested_models: string[];
}

export interface LlmConfigResponse {
  configured: boolean;
  display_name: string | null;
  active: RedactedLlmConfig | null;
  active_controls: ProviderControlSpec[];
  per_model_controls: Record<string, ProviderControlSpec[]>;
  local_management_enabled: boolean;
  discovered_backends: DiscoveredLlmBackend[];
}

export interface UpdateLlmConfigRequest {
  disconnect?: boolean;
  protocol?: LlmWireProtocol;
  base_url?: string;
  api_key?: string;
  model?: string;
  max_tokens?: number;
  command?: string;
  args?: string[];
  config_options?: Record<string, string>;
}

export interface TestLlmConnectionResponse {
  ok: boolean;
  message: string;
  available_models: string[];
  controls?: ProviderControlSpec[];
  per_model_controls?: Record<string, ProviderControlSpec[]>;
}

function splitRoundedCents(milliUnits: number): { sign: string; whole: number; frac: string } {
  const abs = Math.abs(milliUnits);
  const totalCents = Math.floor((abs + 5) / 10);
  const sign = milliUnits < 0 && totalCents > 0 ? "-" : "";
  const whole = Math.floor(totalCents / 100);
  const frac = String(totalCents % 100).padStart(2, "0");
  return { sign, whole, frac };
}

/** Format milli-EUR as euros to the cent (`€0.00`, `€5.00`, `-€0.05`). */
export function formatMilliEur(milliEur: number): string {
  const { sign, whole, frac } = splitRoundedCents(milliEur);
  return `${sign}€${whole}.${frac}`;
}

/**
 * The per-run price an organization pays (price + base fee, milli-EUR) from the module's default offer.
 */
export function effectiveRunCostMilli(
  plugin: { manifest: Pick<PluginManifest, "price_milli_eur" | "offers">; effective_offers?: ResolvedPluginOffer[] },
): number {
  const offer = plugin.effective_offers?.[0] ?? plugin.manifest.offers?.[0];
  if (offer) {
    return offer.price_milli_eur + offer.base_fee_milli_eur;
  }
  return plugin.manifest.price_milli_eur;
}

export type OrgRole = "owner" | "admin" | "developer" | "member" | "viewer";

export interface AuthPrincipal {
  org_namespace?: string;
  mode?: string;
  org_id: string;
  user_id?: string | null;
  actor: string;
  email?: string;
  role: OrgRole | string;
  org_expires_at?: string | null;
  is_platform_admin: boolean;
  auth_method?: string;
  totp_verified?: boolean;
  totp_enabled?: boolean;
}

export interface TotpEnrollment {
  user_id: string;
  secret_base32: string;
  otpauth_uri: string;
}

export interface ApiKeyRecord {
  key_id: string;
  org_id: string;
  label: string;
  role: OrgRole | string;
  is_platform_admin: boolean;
  key_prefix: string;
  revoked_at: string | null;
  created_at: string;
}

export interface CreatedApiKey {
  key_id: string;
  org_id: string;
  label: string;
  role: OrgRole | string;
  is_platform_admin: boolean;
  key_prefix: string;
  raw_key: string;
  created_at: string;
}

export interface OrgMemberRecord {
  org_id: string;
  user_id: string;
  email: string;
  display_name: string;
  role: OrgRole | string;
  totp_enabled: boolean;
  is_platform_admin: boolean;
  created_at: string;
}

export interface OrgInvitationRecord {
  invitation_id: string;
  org_id: string;
  email: string;
  role: OrgRole | string;
  invited_by: string;
  accepted_at: string | null;
  expires_at: string;
  created_at: string;
}

export interface CreatedOrgInvitation {
  invitation_id: string;
  org_id: string;
  email: string;
  role: OrgRole | string;
  invite_token: string;
  expires_at: string;
  created_at: string;
}

export interface AuditLogRecord {
  audit_id: number;
  org_id: string;
  actor: string;
  action: string;
  target_id: string | null;
  metadata: Record<string, unknown>;
  created_at: string;
}

export type SavedViewStatus = "verified" | "access_changed" | "data_changed";

export interface SavedViewSourcePin {
  catalog_id: string;
  table_name: string;
  snapshot_id: number;
  dataset_digest: string;
  grant_id: string;
  grant_scope_digest: string;
}

export interface SavedViewRecord {
  view_id: string;
  org_id: string;
  created_by: string;
  name: string;
  plane: "local" | "cloud";
  primary_source: SavedViewSourcePin;
  additional_sources: SavedViewSourcePin[];
  primary_key_columns: string[];
  query_ast: QueryAst;
  row_count: number;
  selection_digest: string;
  last_status?: SavedViewStatus | null;
  last_verified_at?: string | null;
  last_detail?: string | null;
  local_provenance: boolean;
  created_at: string;
}

export interface SavedViewVerificationReport {
  view_id: string;
  status: SavedViewStatus;
  cached: boolean;
  local_provenance: boolean;
  provenance_label: string;
  saved_row_count: number;
  saved_selection_digest: string;
  current_row_count?: number | null;
  current_selection_digest?: string | null;
  detail: string;
  verified_at: string;
}

export type IngestFormat = "parquet" | "csv" | "tsv";
export type IngestMode = "append" | "replace" | "edit";

export interface PresignedPartSpec {
  part_number: number;
  content_length: number;
  url: string;
}

export interface DraftCorrection {
  table: string;
  row_key: Record<string, unknown>;
  before: Record<string, unknown>;
  after: Record<string, unknown>;
}

export interface QuarantineUploadRecord {
  draft_corrections?: { editor: string; at: string; patch: DraftCorrection }[];
  upload_record_id: string;
  org_id: string;
  created_by: string;
  s3_key: string;
  s3_upload_id: string;
  filename: string;
  format: IngestFormat | "hla_bundle" | "module" | "opaque";
  importer?: { plugin_id: string; version: string; image: string; manifest_sha256: string } | null;
  mode: IngestMode;
  target_catalog_id: string;
  target_table: string;
  catalog_name?: string | null;
  catalog_description?: string | null;
  organism?: string | null;
  primary_key: string[];
  declared_size_bytes: number;
  part_sizes?: number[];
  actual_size_bytes?: number | null;
  part_count: number;
  status: string;
  validation_report?: Record<string, unknown> | null;
  error_message?: string | null;
  expires_at: string;
  created_at: string;
  completed_at?: string | null;
}

export interface QuarantineCompletedPart {
  part_number: number;
  etag: string;
}

export interface HostedHlaPublication {
  catalog: { manifest: CatalogManifest };
  release: { snapshot_id: number; visibility: "private"; dataset_digest: string };
  attachments: { path: string; sha256: string; bytes: number }[];
  raw_files: { file_name: string; availability: "available" | "metadata_only" }[];
}

export type DatasetHostingMode = "cloud_hosted" | "node_hosted";
export type DatasetOfferScope = "private" | "custom_shared" | "global";
export type DatasetOfferSubscription =
  | { status: "pending"; offer_id: string; subscriber_org_id: string }
  | { status: "granted"; offer: DatasetOfferRecord; grant: DatasetGrant };


export interface DatasetOfferRecord {
  offer_id: string;
  publisher_org_id: string;
  scope: DatasetOfferScope | "legacy_session_only";
  allowed_orgs?: string[];
  approval_required: boolean;
  catalog_id: string;
  hosting_mode: DatasetHostingMode;
  node_id?: string | null;
  title: string;
  description: string;
  default_level: GrantAccessLevel;
  row_scope?: FilterExpr | null;
  daily_row_budget?: number | null;
  max_rows_per_query: number;
  price_milli_eur: number;
  provider_pubkey: string;
  descriptor_doc: Record<string, unknown>;
  froglet_offer_doc: Record<string, unknown>;
  active: boolean;
  created_at: string;
}

export type PluginVisibility =
  | "org_private"
  | "public_pending_review"
  | "public_approved"
  | "rejected";

export interface PluginVersionRecord {
  plugin_id: string;
  org_id: string;
  name: string;
  version: string;
  image_ref: string;
  image_digest: string;
  visibility: PluginVisibility;
  has_ui_bundle: boolean;
  is_curated_approved: boolean;
  reviewed_by?: string | null;
  review_note?: string | null;
  reviewed_at?: string | null;
  created_at: string;
}

export interface CuratedModuleApprovalRecord {
  plugin_id: string;
  version: string;
  image_digest: string;
  approved_by: string;
  created_at: string;
}

export type NodeMode = "cloud-pool";

export interface NodeSummaryRecord {
  node_id: string;
  org_id: string | null;
  provider_pubkey: string;
  mode: NodeMode;
  status: string;
  online: boolean;
  container_engine: string;
  enrolled_by: string | null;
  revoked_at: string | null;
  last_heartbeat_at: string;
}

export interface CreatedNodeEnrollment {
  node_id: string;
  org_id: string | null;
  provider_pubkey: string;
  mode: NodeMode;
  raw_credential: string;
}

export interface EnumerationMonitorEntry {
  org_id: string;
  org_name: string;
  plugin_id: string;
  day_utc: string;
  total_runs: number;
  completed_runs: number;
  compute_only_runs: number;
  distinct_inputs: number;
  flagged: boolean;
}

export function hasMinOrgRole(actualRole: string | undefined, required: OrgRole): boolean {
  const rank: Record<string, number> = {
    viewer: 1,
    member: 2,
    developer: 3,
    admin: 4,
    owner: 5,
  };
  return (rank[actualRole ?? ""] ?? 0) >= (rank[required] ?? 0);
}

// ---------------------------------------------------------------------------
// Milestone C1 Enterprise Types (H1, H3, H4, H5, U4)
// ---------------------------------------------------------------------------

export type AgentCredentialRole = "viewer" | "member" | "developer";

export interface AgentCredentialRecord {
  credential_id: string;
  org_id: string;
  label: string;
  role: AgentCredentialRole;
  token_prefix: string;
  allowed_catalogs: string[] | null;
  allowed_modules: string[] | null;
  daily_ceiling_milli_eur: number | null;
  total_ceiling_milli_eur: number | null;
  spent_total_milli_eur: number;
  spent_today_milli_eur?: number;
  /** True when no daily ceiling is set (spend bounded only by the org balance and breaker). */
  unlimited_daily?: boolean;
  /** True when no total ceiling is set. */
  unlimited_total?: boolean;
  burst_per_minute: number;
  expires_at: string;
  created_by: string;
  created_at: string;
  revoked_at: string | null;
  last_used_at: string | null;
}

/** Which spending ceilings a credential lacks (server flags win; fall back to null ceilings). */
export function unlimitedCeilings(cred: Pick<
  AgentCredentialRecord,
  "unlimited_daily" | "unlimited_total" | "daily_ceiling_milli_eur" | "total_ceiling_milli_eur"
>): { daily: boolean; total: boolean } {
  return {
    daily: cred.unlimited_daily ?? cred.daily_ceiling_milli_eur == null,
    total: cred.unlimited_total ?? cred.total_ceiling_milli_eur == null,
  };
}

/** Plain-language warning for a credential issued without one or both ceilings. */
export function unlimitedCeilingWarning(daily: boolean, total: boolean): string | null {
  if (daily && total) {
    return "No daily or total spending ceiling: this agent can spend up to the organization's whole balance, limited only by the daily breaker.";
  }
  if (daily) {
    return "No daily spending ceiling: this agent can spend its whole total ceiling in one day.";
  }
  if (total) {
    return "No total spending ceiling: this agent can keep spending its daily ceiling every day until it expires.";
  }
  return null;
}

export interface CreateAgentCredentialInput {
  label: string;
  role?: AgentCredentialRole;
  expires_at?: string;
  expires_in_days?: number;
  allowed_catalogs?: string[] | null;
  allowed_modules?: string[] | null;
  daily_ceiling_milli_eur?: number | null;
  total_ceiling_milli_eur?: number | null;
  burst_per_minute?: number;
}

export interface CreatedAgentCredential {
  credential: AgentCredentialRecord;
  raw_token: string;
}

export interface BillingStatementTotals {
  run_count: number;
  /** Runs whose reservation is still open (not yet settled or refunded). */
  in_flight_run_count?: number;
  /** Settled spend only; in-flight reservations are reported separately. */
  total_milli_eur: number;
  module_fee_milli_eur: number;
  dataset_fee_milli_eur: number;
  refunded_milli_eur: number;
  reserved_in_flight_milli_eur?: number;
}

export interface BillingStatementPage {
  limit: number;
  has_more: boolean;
  next_cursor?: string | null;
}

export interface BillingLedgerKindTotal {
  kind: string;
  entries: number;
  delta_milli_eur: number;
}

export interface BillingLedgerEntry {
  tx_id: string;
  kind: string;
  delta_milli_eur: number;
  balance_after_milli_eur: number;
  reference: string;
  created_at: string;
}

/** Reconciliation of the statement range with the organization ledger (org admins only). */
export interface BillingLedgerReconciliation {
  opening_balance_milli_eur: number;
  closing_balance_milli_eur: number;
  movements_milli_eur: number;
  reconciled: boolean;
  by_kind: BillingLedgerKindTotal[];
  non_run_entries: BillingLedgerEntry[];
  reserved_now_milli_eur: number;
}

export interface BillingStatementModuleBreakdown {
  plugin_id: string;
  run_count: number;
  total_milli_eur: number;
  module_fee_milli_eur: number;
}

export interface BillingStatementDatasetBreakdown {
  catalog_id: string;
  run_count: number;
  dataset_fee_milli_eur: number;
}

export interface BillingStatementCredentialBreakdown {
  credential_id?: string | null;
  label: string;
  run_count: number;
  total_milli_eur: number;
}

export interface BillingStatementEntry {
  run_id: string;
  plugin_id: string;
  plugin_version: string;
  catalog_id?: string | null;
  agent_credential_id?: string | null;
  agent_credential_label?: string | null;
  status: string;
  in_flight?: boolean;
  reserved_in_flight_milli_eur?: number;
  settled_milli_eur: number;
  refunded_milli_eur: number;
  module_fee_milli_eur: number;
  dataset_fee_milli_eur: number;
  receipt_hash?: string | null;
  created_at: string;
  completed_at?: string | null;
}

export interface BillingStatement {
  org_id: string;
  currency: "EUR" | string;
  from?: string | null;
  to?: string | null;
  totals: BillingStatementTotals;
  by_module: BillingStatementModuleBreakdown[];
  by_dataset: BillingStatementDatasetBreakdown[];
  by_credential: BillingStatementCredentialBreakdown[];
  entries: BillingStatementEntry[];
  page?: BillingStatementPage;
  ledger?: BillingLedgerReconciliation | null;
}

export interface WebhookEndpointRecord {
  endpoint_id: string;
  org_id: string;
  label: string;
  url: string;
  event_types: string[];
  enabled: boolean;
  created_by: string;
  created_at: string;
  updated_at: string;
  disabled_at?: string | null;
}

export interface CreatedWebhookEndpoint {
  webhook: WebhookEndpointRecord;
  signing_secret: string;
}

export interface WebhookDeliveryRecord {
  delivery_id: string;
  event_id: number;
  endpoint_id: string;
  event_type: string;
  status: "pending" | "delivered" | "failed" | "dead_letter" | string;
  attempt_count: number;
  max_attempts: number;
  next_attempt_at?: string | null;
  last_http_status?: number | null;
  last_error?: string | null;
  delivered_at?: string | null;
  created_at: string;
  updated_at: string;
}

export interface ModuleDocumentRecord {
  id: string;
  org_id: string;
  project_id?: string | null;
  plugin_id: string;
  title: string;
  latest_version: number;
  created_by: string;
  created_at: string;
  updated_at: string;
}

export interface ModuleDocumentVersionRecord {
  doc_id: string;
  version: number;
  content_sha256: string;
  base_version: number;
  author_user_id?: string | null;
  author_credential_id?: string | null;
  created_at: string;
}

export interface ModuleDocumentDetail extends ModuleDocumentRecord {
  version: number;
  content: Record<string, unknown>;
  content_sha256: string;
  base_version: number;
  author_user_id?: string | null;
  author_credential_id?: string | null;
  version_created_at: string;
}

export interface ProvenanceLineageEdge {
  run_id: string;
  source_kind: string;
  source_ref: string;
  target_kind: string;
  target_ref: string;
}

export interface ProvenanceRunEntry {
  run_id: string;
  plugin_id: string;
  plugin_version: string;
  status: string;
  parent_run_id?: string | null;
  created_at: string;
  completed_at?: string | null;
  manifest_preimage?: Record<string, unknown> | null;
  inputs: RunInput[];
  child_receipt_hashes: string[];
  froglet_chain: Record<string, unknown>[];
}

export interface ProvenanceBundle {
  schema: "bc.provenance.v1" | string;
  exported_at: string;
  root_run_id: string;
  runs: ProvenanceRunEntry[];
  edges: ProvenanceLineageEdge[];
  unresolved_ancestors: string[];
}

export type CatalogDataPlacement = "stays_with_alithea" | "licensable_copy";
