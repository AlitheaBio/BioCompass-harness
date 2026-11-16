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
}

export interface DatasetGrant {
  grant_id: string;
  label: string;
  org_id: string;
  catalog_id: string;
  allowed_organs: string[] | null;
  allowed_conditions: string[] | null;
  mask_peptide_sequences: boolean;
  max_rows: number;
}

export interface CatalogShard {
  relative_path: string;
  sha256: string;
  row_count: number;
}

export interface CatalogManifest {
  id: string;
  name: string;
  version: string;
  description: string;
  shards: CatalogShard[];
}

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
}

export interface CatalogQuery {
  organs?: string[];
  conditions?: string[];
  hla_alleles?: string[];
  gene_symbols?: string[];
  min_length?: number;
  max_length?: number;
  max_affinity_nm?: number;
  limit?: number;
}

export interface CatalogQueryResult {
  catalog_id: string;
  grant_id: string;
  sequence_masked: boolean;
  compiled_view_sql: string;
  total_in_scope: number;
  rows: PeptideRecord[];
}

export interface DeclarativeViewSpec {
  id: string;
  kind: "metric_cards" | "bar" | "length_distribution" | "table" | string;
  title: string;
  data_pointer: string;
}

export interface PluginManifest {
  id: string;
  name: string;
  version: string;
  description: string;
  category: string;
  image: string;
  network: string;
  price_milli_act: number;
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
}

export interface SkillDoc {
  name: string;
  description: string;
  markdown_body: string;
}

export interface PluginSummary {
  manifest: PluginManifest;
  skill: SkillDoc | null;
  views: DeclarativeViewSpec[];
  has_custom_ui: boolean;
  ui_entry_url: string | null;
}

export interface RunQuote {
  run_id: string;
  plugin_id: string;
  plugin_version: string;
  image_ref: string;
  target_node_id: string;
  estimated_cost_act: number;
  workload_hash: string;
  dataset_grant_summary: string | null;
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

export interface RunRecord {
  run_id: string;
  org_id: string;
  session_id: string | null;
  plugin_id: string;
  plugin_version: string;
  status: RunStatus;
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

export interface LedgerTx {
  tx_id: string;
  org_id: string;
  run_id: string | null;
  kind: string;
  delta_act: number;
  balance_after_act: number;
  reference: string;
  created_at: string;
}

export interface LedgerState {
  org_id: string;
  available_milli_act: number;
  available_display: string;
  transactions: LedgerTx[];
}

export interface SessionRecord {
  session_id: string;
  org_id: string;
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

export function formatMilliAct(milliAct: number): string {
  return `${(milliAct / 1000).toFixed(2)} ACT`;
}
