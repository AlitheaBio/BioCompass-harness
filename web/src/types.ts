export type MarketplaceSyncState = "air-gapped" | "disconnected" | "connected" | "stale";

export type MarketplaceConsentDecision = "unset" | "accepted" | "declined";

export interface MarketplaceOfferSummary {
  id: string;
  kind: string;
  title: string;
  version?: string | null;
  provider_org?: string | null;
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
  price_milli_act: number;
  base_fee_milli_act: number;
  max_input_bytes: number;
  max_output_bytes: number;
  max_runtime_sec?: number | null;
  max_memory_mb?: number | null;
}

export interface ResolvedPluginOffer {
  id: string;
  name: string;
  price_milli_act: number;
  base_fee_milli_act: number;
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
  price_milli_act: number;
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
export type ModuleOfferAccess = "granted" | "pending" | "request_required" | "tier_restricted";

export interface ModuleOffer {
  offer_id: string;
  publisher_org_id: string;
  plugin_id: string;
  version: string;
  title: string;
  action: ModuleOfferAction;
  scope: ModuleOfferScope;
  approval_required: boolean;
  access?: ModuleOfferAccess;
}

export interface FederatedOfferSummary {
  offer_id: string;
  publisher_org_id?: string | null;
  plugin_id?: string | null;
  version?: string | null;
  catalog_id?: string | null;
  title: string;
  action: ModuleOfferAction | "dataset" | string;
  scope: ModuleOfferScope | string;
  approval_required?: boolean;
  access?: ModuleOfferAccess;
}

export interface FederatedModuleOffer {
  offer_kind?: "run" | "download" | "dataset" | string;
  peer_id?: string;
  offer_hash?: string;
  offer: FederatedOfferSummary;
  provider: string;
  launch_url: string | null;
}

export interface ModuleOfferRequest {
  offer_id: string;
  title: string;
  publisher_org_id: string;
  subscriber_org_id: string;
  access: "granted" | "pending";
}

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
  base_fee_act?: number;
  estimated_cost_act: number;
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

export interface ResultShare {
  share_id: string;
  owner_org_id: string;
  recipient_org_id: string;
  filename: string;
  media_type: string;
  byte_length: number;
  sha256: string;
  created_at: string;
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
  delta_act: number;
  balance_after_act: number;
  receipt_hash?: string | null;
  reference: string;
  created_at: string;
}

export interface LedgerState {
  org_id: string;
  tier_id?: string;
  available_milli_act: number;
  available_display: string;
  daily_act_spent_milli?: number;
  daily_act_breaker_milli?: number | null;
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

export function formatMilliAct(milliAct: number): string {
  return `${(milliAct / 1000).toFixed(milliAct % 10 === 0 ? 2 : 3)} ACT`;
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
  tier_id?: string;
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
export interface DatasetOfferRequest {
  offer_id: string;
  title: string;
  publisher_org_id: string;
  subscriber_org_id: string;
  status: "pending" | "approved";
}
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
  price_milli_act: number;
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

export interface TierDefaultGrant {
  catalog_id: string;
  label: string;
  level: GrantAccessLevel;
  row_scope?: FilterExpr | null;
  daily_row_budget?: number | null;
  max_rows_per_query: number;
  allow_local_export: boolean;
}

export interface TierDefinitionRecord {
  tier_id: string;
  name: string;
  duration_days: number | null;
  max_users: number;
  initial_act_milli: number;
  daily_act_breaker_milli: number | null;
  storage_quota_bytes: number;
  max_snapshots_per_catalog: number;
  burst_rpm: number;
  allowed_modules: string[] | null;
  allow_custom_modules: boolean;
  default_grants: TierDefaultGrant[];
  default_module_offer_ids: string[];
  default_dataset_offer_ids: string[];
  updated_at: string;
}

export interface UpdateTierInput {
  name: string;
  duration_days: number | null;
  max_users: number;
  initial_act_milli: number;
  daily_act_breaker_milli: number | null;
  storage_quota_bytes: number;
  max_snapshots_per_catalog: number;
  burst_rpm: number;
  allowed_modules: string[] | null;
  allow_custom_modules: boolean;
  default_grants: TierDefaultGrant[];
  default_module_offer_ids?: string[];
  default_dataset_offer_ids?: string[];
}

export type NodeMode = "cloud-pool" | "query-provider" | "provider-edge" | "hospital-edge";

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

export type FederationPeerStatus = "pending" | "approved" | "rejected" | "suspended";

export interface FederationPeerRecord {
  id: string;
  peer_url: string;
  peer_pubkey: string;
  org_name: string;
  status: FederationPeerStatus;
  requested_at: string;
  approved_by?: string | null;
  approved_at?: string | null;
  last_sync_at?: string | null;
  last_error?: string | null;
  enabled: boolean;
  role: string;
  label: string;
  origin: string;
  backchannel: string;
  home_org: string | null;
  destination_org: string | null;
  archive_enabled: boolean;
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
