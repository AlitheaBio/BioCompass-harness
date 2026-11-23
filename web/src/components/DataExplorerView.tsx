import React, { useEffect, useMemo, useRef, useState } from "react";
import {
  IconAbout,
  IconBookmark,
  IconChat,
  IconCheck,
  IconCheckMark,
  IconChevronRight,
  IconClose,
  IconColumns,
  IconCopy,
  IconDatabase,
  IconDownload,
  IconEye,
  IconFilter,
  IconGroupBy,
  IconModules,
  IconPlus,
  IconSearch,
  IconShield,
  IconTrash,
  IconWindowOpen,
} from "./Icons";
import {
  formatErrorInline,
  pinCatalogSnapshot,
  rollbackCatalogSnapshot,
  toQueryAst,
} from "../api";
import { CatalogImportWizard } from "./CatalogImportWizard";
import { serializeRowsForExport } from "../csvExport";
import { CatalogTableSearch } from "./CatalogTableSearch";
import { PrivateUploadDialog } from "./PrivateUploadDialog";
import { HostedReleaseChanges } from "./HostedReleaseChanges";
import { LocalDatasetImport } from "./LocalDatasetImport";
import { ImportPicker } from "./ImportPicker";
import type { UploadImporter } from "../api";
import { SavedViewsPanel } from "./SavedViewsPanel";
import type {
  CatalogManifest,
  CatalogQuery,
  CatalogQueryResult,
  CatalogShard,
  ColumnDescriptor,
  DatasetGrant,
  FilterExpr,
  GrantUsageSummary,
  PeptideRecord,
  PluginSummary,
  QueryAst,
  SavedViewRecord,
  SavedViewVerificationReport,
} from "../types";

function isLoopbackWorkbench() {
  const host = window.location.hostname.toLowerCase();
  return host === "localhost" || host.endsWith(".localhost")
    || host === "[::1]" || /^127(?:\.\d{1,3}){3}$/.test(host);
}

function catalogDescription(catalog: CatalogManifest) {
  const location = catalog.location;
  // Older native imports recorded this fixed description before they were shared.
  if (catalog.description === "Local imported DuckLake catalog" && location
      && (location.kind !== "workspace" || !isLoopbackWorkbench())) return null;
  return catalog.description;
}

function CatalogStorage({ catalog, expanded = false }: { catalog: CatalogManifest; expanded?: boolean }) {
  const location = catalog.location;
  let kind = "Unreported";
  let details: string | undefined;
  if (location?.kind === "cloud") {
    kind = "Cloud";
    details = location.tenancy_org_id;
  } else if (location?.kind === "network_node") {
    kind = "Network node";
    details = [location.operator_org_id, expanded && location.node_id, location.online ? "Online" : "Offline"]
      .filter(Boolean).join(" · ");
  } else if (location?.kind === "workspace") {
    const onDevice = isLoopbackWorkbench();
    kind = onDevice ? "Local" : "Network node";
    details = onDevice ? "This device" : location.node_id;
  }
  return (
    <div className="bc-catalog-storage" data-testid={expanded ? "catalog-inspector-location" : `catalog-location-${catalog.id}`}>
      <span>Storage location</span>
      <strong>{kind}</strong>
      {details && <span>{details}</span>}
    </div>
  );
}

export interface DataExplorerViewProps {
  mode?: "local" | "cloud";
  defaultNamespace?: string;
  uploadScope?: string;
  catalogs: CatalogManifest[];
  grants: DatasetGrant[];
  grantUsage?: GrantUsageSummary[];
  activeGrant: DatasetGrant | null;
  selectedCatalogIds: string[];
  activeCatalogId: string;
  catalogQuery: CatalogQuery;
  catalogLoading: boolean;
  catalogResult: CatalogQueryResult | null;
  plugins: PluginSummary[];
  savedViews?: SavedViewRecord[];
  onSelectPreviewCatalog: (catalogId: string) => void;
  onToggleCatalogSelection: (catalogId: string) => void;
  onGrantChange: (grantId: string) => void;
  onUpdateCatalogQuery: (next: CatalogQuery) => void;
  onCreateSavedView?: (
    name: string,
    query: CatalogQuery,
  ) => Promise<SavedViewRecord>;
  onDeleteSavedView?: (viewId: string) => Promise<void>;
  onVerifiedSavedView?: (
    viewId: string,
    report: SavedViewVerificationReport,
  ) => void;
  onRefreshCatalogs?: () => Promise<void>;
  onSendToChat: (payload: {
    catalogId: string;
    catalogName: string;
    rows: PeptideRecord[];
    query: CatalogQuery;
  }) => void;
  onRunInModule: (
    plugin: PluginSummary,
    derivedParams: Record<string, unknown>,
    rows: PeptideRecord[],
  ) => void;
}

type GroupByField = "none" | "organ" | "condition" | "hla_allele" | "gene_symbol";
type RowDensity = "comfortable" | "compact";
type SortDir = "asc" | "desc";

interface ColumnDef {
  key: string;
  label: string;
  mono?: boolean;
  numeric?: boolean;
  defaultHidden?: boolean;
}

const ALL_COLUMNS: ColumnDef[] = [
  { key: "peptide_id", label: "Peptide ID", mono: true },
  { key: "sequence", label: "Sequence", mono: true },
  { key: "peptide_length", label: "Len", numeric: true },
  { key: "hla_allele", label: "HLA Allele", mono: true },
  { key: "gene_symbol", label: "Gene" },
  { key: "organ", label: "Organ" },
  { key: "condition", label: "Condition" },
  { key: "affinity_nm", label: "IC50 (nM)", numeric: true },
  { key: "eluted_rank_pct", label: "%Rank", numeric: true, defaultHidden: true },
  { key: "ms_intensity_log2", label: "MS log2", numeric: true, defaultHidden: true },
  { key: "donor_count", label: "Donors", numeric: true },
];

const DEFAULT_ORGANS = ["Lung", "Skin", "Liver", "Colon", "Kidney"];
const DEFAULT_CONDITIONS = [
  "NSCLC_Tumor",
  "Healthy_Adjacent",
  "Primary_Melanoma",
  "Normal_Donor",
  "CRC_Tumor",
];
const DEFAULT_ALLELES = [
  "HLA-A*02:01",
  "HLA-A*03:01",
  "HLA-B*07:02",
  "HLA-B*44:02",
  "HLA-C*07:02",
];
const DEFAULT_GENES = [
  "EGFR",
  "KRAS",
  "TP53",
  "NKX2-1",
  "SFTPB",
  "PMEL",
  "MAGEA3",
  "ALB",
  "CTNNB1",
  "PAX8",
];

const SAVED_VIEWS_STORAGE_KEY = "biocompass.saved_data_views.v1";

interface SavedDataView {
  id: string;
  name: string;
  catalogId: string;
  query: CatalogQuery;
  hiddenColumns: string[];
  density: RowDensity;
  groupBy: GroupByField;
  createdAt: string;
}

type IdentifierKind = "sequence" | "peptide_id" | "gene_symbol" | "hla_allele";

function formatBytes(bytes?: number): string {
  if (bytes == null || !Number.isFinite(bytes)) return "—";
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(2)} MB`;
}

function computeDatasetDigestPreview(cat: CatalogManifest): string | null {
  if (cat.dataset_digest) {
    return `${cat.dataset_digest.slice(0, 24)}… (#${cat.latest_snapshot_id})`;
  }
  const shards = cat.shards ?? [];
  if (shards.length === 0) return null;
  if (shards.length === 1) return shards[0]!.sha256;
  return `${shards[0]!.sha256.slice(0, 24)}… (${shards.length} shards)`;
}

function parseTagList(raw: string): string[] {
  return Array.from(
    new Set(
      raw
        .split(/[\n,;]+/)
        .map((s) => s.trim())
        .filter(Boolean),
    ),
  );
}

function clampToSchema(
  val: number,
  prop?: { minimum?: unknown; maximum?: unknown },
): number {
  let out = val;
  if (typeof prop?.minimum === "number") out = Math.max(prop.minimum, out);
  if (typeof prop?.maximum === "number") out = Math.min(prop.maximum, out);
  return out;
}

function queryAstToCatalogQuery(ast: QueryAst): CatalogQuery {
  const out: CatalogQuery = {
    ...(ast.table ? { table: ast.table } : {}),
    ...(ast.snapshot_id != null ? { snapshot_id: ast.snapshot_id } : {}),
    ...(ast.limit != null ? { limit: ast.limit } : {}),
  };
  const preds: FilterExpr[] = !ast.filter
    ? []
    : ast.filter.op === "and"
      ? ast.filter.exprs
      : [ast.filter];
  for (const pred of preds) {
    if (pred.op === "in" && Array.isArray(pred.values)) {
      const strs = pred.values.map(String);
      if (pred.column === "organ") out.organs = strs;
      else if (pred.column === "condition") out.conditions = strs;
      else if (pred.column === "hla_allele") out.hla_alleles = strs;
      else if (pred.column === "gene_symbol") out.gene_symbols = strs;
    } else if (pred.op === "gte" && pred.column === "peptide_length") {
      out.min_length = Number(pred.value);
    } else if (pred.op === "lte" && pred.column === "peptide_length") {
      out.max_length = Number(pred.value);
    } else if (pred.op === "lte" && pred.column === "affinity_nm") {
      out.max_affinity_nm = Number(pred.value);
    } else if (pred.op === "starts_with" && pred.column === "sequence") {
      out.sequence_prefix = String(pred.prefix ?? "");
    }
  }
  return out;
}

function deriveModuleParamsFromSelection(
  plugin: PluginSummary,
  rows: PeptideRecord[],
  query: CatalogQuery,
): Record<string, unknown> {
  const schemaObj = plugin.manifest.inputs?.schema ?? {};
  const rawProps =
    typeof schemaObj["properties"] === "object" && schemaObj["properties"] !== null
      ? (schemaObj["properties"] as Record<string, Record<string, unknown>>)
      : {};

  const out: Record<string, unknown> = {};
  for (const [k, spec] of Object.entries(rawProps)) {
    if (spec && "default" in spec && spec["default"] !== undefined) {
      out[k] = spec["default"];
    }
  }

  const uniqueAlleles = Array.from(new Set(rows.map((r) => r.hla_allele).filter(Boolean)));
  const uniqueGenes = Array.from(new Set(rows.map((r) => r.gene_symbol).filter(Boolean)));
  const lengths = rows.map((r) => r.peptide_length).filter((n) => Number.isFinite(n));
  const affinities = rows.map((r) => r.affinity_nm).filter((n) => Number.isFinite(n));

  if (uniqueAlleles.length > 0) {
    if ("alleles" in rawProps) out["alleles"] = uniqueAlleles;
    if ("hla_alleles" in rawProps) out["hla_alleles"] = uniqueAlleles;
    if ("hla_allele" in rawProps) out["hla_allele"] = uniqueAlleles[0];
  } else if (query.hla_alleles && query.hla_alleles.length > 0) {
    if ("alleles" in rawProps) out["alleles"] = query.hla_alleles;
    if ("hla_alleles" in rawProps) out["hla_alleles"] = query.hla_alleles;
  }

  if (lengths.length > 0) {
    const minL = Math.min(...lengths);
    const maxL = Math.max(...lengths);
    if ("min_length" in rawProps) {
      out["min_length"] = clampToSchema(minL, rawProps["min_length"]);
    }
    if ("max_length" in rawProps) {
      out["max_length"] = clampToSchema(maxL, rawProps["max_length"]);
    }
  }

  if (affinities.length > 0) {
    const maxIc50 = Math.ceil(Math.max(...affinities));
    if ("max_ic50_nm" in rawProps) {
      out["max_ic50_nm"] = clampToSchema(maxIc50, rawProps["max_ic50_nm"]);
    }
    if ("max_affinity_nm" in rawProps) {
      out["max_affinity_nm"] = clampToSchema(maxIc50, rawProps["max_affinity_nm"]);
    }
  } else if (typeof query.max_affinity_nm === "number") {
    if ("max_ic50_nm" in rawProps) {
      out["max_ic50_nm"] = clampToSchema(query.max_affinity_nm, rawProps["max_ic50_nm"]);
    }
  }

  if (uniqueGenes.length > 0) {
    if ("genes" in rawProps) out["genes"] = uniqueGenes;
    if ("gene_symbols" in rawProps) out["gene_symbols"] = uniqueGenes;
  }

  const seqs = Array.from(new Set(rows.map((r) => r.sequence).filter(Boolean)));
  if (seqs.length > 0) {
    if ("peptides" in rawProps) out["peptides"] = seqs;
    if ("sequences" in rawProps) out["sequences"] = seqs;
  }

  return out;
}

function downloadTextFile(filename: string, content: string, mime: string): void {
  const blob = new Blob([content], { type: mime });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  document.body.removeChild(a);
  window.setTimeout(() => URL.revokeObjectURL(url), 1000);
}

function binderTierBadge(ic50: number): { label: string; cls: string } {
  if (ic50 <= 50) return { label: "Strong binder (≤50 nM)", cls: "bc-badge-success" };
  if (ic50 <= 500) return { label: "Weak binder (≤500 nM)", cls: "bc-badge-neutral" };
  return { label: "Low affinity (>500 nM)", cls: "bc-badge-warn" };
}

export function DataExplorerView({
  mode = "local",
  defaultNamespace = "alithea",
  uploadScope,
  catalogs,
  grants,
  grantUsage = [],
  activeGrant,
  selectedCatalogIds,
  activeCatalogId,
  catalogQuery,
  catalogLoading,
  catalogResult,
  plugins,
  savedViews: backendSavedViews = [],
  onSelectPreviewCatalog,
  onToggleCatalogSelection,
  onGrantChange,
  onUpdateCatalogQuery,
  onCreateSavedView,
  onDeleteSavedView,
  onVerifiedSavedView,
  onRefreshCatalogs,
  onSendToChat,
  onRunInModule,
}: DataExplorerViewProps) {
  const [catalogSearch, setCatalogSearch] = useState("");
  const [quickRowSearch, setQuickRowSearch] = useState("");
  const [selectedRowIds, setSelectedRowIds] = useState<Set<string>>(new Set());

  // Table presentation controls
  const [hiddenByTable, setHiddenByTable] = useState(new Map<string, Set<string>>());
  const [density, setDensity] = useState<RowDensity>("comfortable");
  const [groupBy, setGroupBy] = useState<GroupByField>("none");
  const [collapsedGroups, setCollapsedGroups] = useState<Set<string>>(new Set());
  const [sortBy, setSortBy] = useState<{ key: string; dir: SortDir } | null>(null);
  const [pageSize, setPageSize] = useState<number>(50);
  const [currentPage, setCurrentPage] = useState<number>(1);

  // Popovers, drawers, and modals
  const [openMenu, setOpenMenu] = useState<
    "columns" | "group" | "export" | "module" | null
  >(null);
  const [filterDrawerOpen, setFilterDrawerOpen] = useState(false);
  const [provenanceCatalogId, setProvenanceCatalogId] = useState<string | null>(null);

  // Row detail modal state (supports single record or multi-record drill-down)
  const [detailRows, setDetailRows] = useState<PeptideRecord[] | null>(null);
  const [focusedDetailRow, setFocusedDetailRow] = useState<PeptideRecord | null>(null);

  // Paste Identifiers modal state
  const [pasteModalOpen, setPasteModalOpen] = useState(false);
  const [pasteKind, setPasteKind] = useState<IdentifierKind>("sequence");
  const [pasteInput, setPasteInput] = useState("");
  const [pasteReport, setPasteReport] = useState<{
    matchedRows: PeptideRecord[];
    unmatchedTerms: string[];
    matchedTermCount: number;
  } | null>(null);

  // Saved Views state (local browser persistence)
  const [savedViews, setSavedViews] = useState<SavedDataView[]>(() => {
    try {
      const raw = localStorage.getItem(SAVED_VIEWS_STORAGE_KEY);
      if (raw) {
        const parsed: unknown = JSON.parse(raw);
        if (Array.isArray(parsed)) return parsed as SavedDataView[];
      }
    } catch {
      // ignore storage errors
    }
    return [];
  });
  const [activeSavedViewId, setActiveSavedViewId] = useState<string | null>(null);
  const [saveViewModalOpen, setSaveViewModalOpen] = useState(false);
  const [newViewName, setNewViewName] = useState("");
  const [savingView, setSavingView] = useState(false);
  const [saveViewError, setSaveViewError] = useState<string | null>(null);
  const [importWizardOpen, setImportWizardOpen] = useState(false);
  const [pickerOpen, setPickerOpen] = useState(false);
  const [transfer, setTransfer] = useState<"opaque" | "legacy" | UploadImporter | null>(null);
  const [snapshotActionBusy, setSnapshotActionBusy] = useState(false);
  const [snapshotActionError, setSnapshotActionError] = useState<string | null>(null);

  // Copy feedback
  const [copiedToken, setCopiedToken] = useState<string | null>(null);
  const copyText = (token: string, text: string) => {
    if (typeof navigator !== "undefined" && navigator.clipboard?.writeText) {
      void navigator.clipboard.writeText(text).then(() => {
        setCopiedToken(token);
        window.setTimeout(
          () => setCopiedToken((prev) => (prev === token ? null : prev)),
          1600,
        );
      });
    }
  };

  // Draft state inside Filter Drawer
  const [draftOrgans, setDraftOrgans] = useState<string[]>([]);
  const [draftConditions, setDraftConditions] = useState<string[]>([]);
  const [draftAlleles, setDraftAlleles] = useState<string[]>([]);
  const [draftGenes, setDraftGenes] = useState<string[]>([]);
  const [draftMinLen, setDraftMinLen] = useState<string>("");
  const [draftMaxLen, setDraftMaxLen] = useState<string>("");
  const [draftMaxIc50, setDraftMaxIc50] = useState<string>("");
  const [draftSeqPrefix, setDraftSeqPrefix] = useState<string>("");
  const [draftLimit, setDraftLimit] = useState<number>(250);
  const [customAlleleInput, setCustomAlleleInput] = useState("");
  const [customGeneInput, setCustomGeneInput] = useState("");

  const toolbarRef = useRef<HTMLDivElement | null>(null);

  // Close popovers on outside click or Escape
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        if (openMenu) setOpenMenu(null);
        else if (focusedDetailRow && detailRows && detailRows.length > 1) {
          setFocusedDetailRow(null);
        } else if (detailRows) {
          setDetailRows(null);
          setFocusedDetailRow(null);
        } else if (pasteModalOpen) setPasteModalOpen(false);
        else if (saveViewModalOpen) setSaveViewModalOpen(false);
        else if (filterDrawerOpen) setFilterDrawerOpen(false);
        else if (provenanceCatalogId) setProvenanceCatalogId(null);
      }
    };
    const onPointerDown = (e: MouseEvent) => {
      if (!openMenu) return;
      const target = e.target as Node | null;
      if (toolbarRef.current && target && !toolbarRef.current.contains(target)) {
        const selBar = document.querySelector('[data-testid="catalog-selection-bar"]');
        if (selBar && selBar.contains(target)) return;
        setOpenMenu(null);
      }
    };
    window.addEventListener("keydown", onKeyDown);
    window.addEventListener("mousedown", onPointerDown);
    return () => {
      window.removeEventListener("keydown", onKeyDown);
      window.removeEventListener("mousedown", onPointerDown);
    };
  }, [
    openMenu,
    detailRows,
    focusedDetailRow,
    pasteModalOpen,
    saveViewModalOpen,
    filterDrawerOpen,
    provenanceCatalogId,
  ]);

  // Clear row selection & reset page to 1 when switching catalog or grant
  const scopeResetKey = `${activeCatalogId}::${activeGrant?.grant_id ?? ""}::${catalogQuery.table ?? ""}`;
  const [prevScopeKey, setPrevScopeKey] = useState(scopeResetKey);
  if (scopeResetKey !== prevScopeKey) {
    setPrevScopeKey(scopeResetKey);
    setSelectedRowIds(new Set());
    setQuickRowSearch("");
    setSortBy(null);
    setCurrentPage(1);
  }

  const activeCatalog = useMemo(
    () => catalogs.find((c) => c.id === activeCatalogId) ?? catalogs[0] ?? null,
    [catalogs, activeCatalogId],
  );

  const provenanceCatalog = useMemo(
    () =>
      provenanceCatalogId
        ? catalogs.find((c) => c.id === provenanceCatalogId) ?? activeCatalog
        : null,
    [provenanceCatalogId, catalogs, activeCatalog],
  );

  const privateRelease = provenanceCatalog?.id.toLowerCase().startsWith("bc-private-v1/");
  const localRelease = mode === "local" && provenanceCatalog?.location?.kind === "workspace";
  const releaseVersion = localRelease
    ? Number(provenanceCatalog?.version)
    : mode === "cloud" && uploadScope && provenanceCatalog?.location?.kind === "cloud"
      ? provenanceCatalog.release_version : undefined;

  const provenanceDescription = provenanceCatalog ? catalogDescription(provenanceCatalog) : null;
  const selectedCatalogCount = useMemo(
    () => catalogs.filter((c) => selectedCatalogIds.includes(c.id)).length,
    [catalogs, selectedCatalogIds],
  );

  const filteredCatalogs = useMemo(() => {
    const q = catalogSearch.trim().toLowerCase();
    if (!q) return catalogs;
    return catalogs.filter((c) =>
      `${c.name} ${c.id} ${c.organism ?? ""} ${catalogDescription(c) ?? ""}`
        .toLowerCase()
        .includes(q),
    );
  }, [catalogs, catalogSearch]);

  const isComputeOnly = activeGrant?.level === "compute_only";
  const canLocalExport = Boolean(
    activeGrant ? activeGrant.level === "rows" && activeGrant.allow_local_export : true,
  );
  const activeGrantUsage = useMemo(
    () =>
      activeGrant
        ? grantUsage.find((u) => u.grant_id === activeGrant.grant_id) ?? null
        : null,
    [activeGrant, grantUsage],
  );
  const remainingDailyBudget = useMemo<number | null>(() => {
    if (catalogResult?.remaining_daily_row_budget != null) {
      return catalogResult.remaining_daily_row_budget;
    }
    if (activeGrant?.daily_row_budget != null) {
      const used = activeGrantUsage?.rows_used_today ?? 0;
      return Math.max(0, activeGrant.daily_row_budget - used);
    }
    return null;
  }, [catalogResult, activeGrant, activeGrantUsage]);
  const selectedTable = activeCatalog?.tables?.find(table => table.name === catalogQuery.table) ?? activeCatalog?.tables?.[0];
  const activeSchema = catalogResult && catalogResult.catalog_id === activeCatalogId
    && (!selectedTable || catalogResult.table === selectedTable.name)
    ? catalogResult.columns : selectedTable?.columns;
  const schemaNames = useMemo(() => new Set(activeSchema?.map(col => col.name)), [activeSchema]);
  const isGenericSchema = !ALL_COLUMNS.every(col => schemaNames.has(col.key));
  const tableColumns = useMemo<ColumnDef[]>(() => {
    const actual = (activeSchema ?? []).map(col => ({ key: col.name, label: col.name }));
    if (isGenericSchema) return actual;
    return [...ALL_COLUMNS, ...actual.filter(col => !ALL_COLUMNS.some(known => known.key === col.key))];
  }, [activeSchema, isGenericSchema]);
  const tableKey = JSON.stringify([activeCatalogId, selectedTable?.name]);
  const hiddenColumns = useMemo(() =>
    hiddenByTable.get(tableKey) ?? new Set(tableColumns.filter(col => col.defaultHidden).map(col => col.key)),
    [hiddenByTable, tableKey, tableColumns],
  );
  const setHiddenColumns = (columns: Set<string>, key = tableKey) =>
    setHiddenByTable(previous => new Map(previous).set(key, columns));
  const genericPkColumns = useMemo(() => {
    const tbl = activeCatalog?.tables?.find(table => table.name === catalogResult?.table);
    if (tbl?.primary_key && tbl.primary_key.length > 0) {
      return tbl.primary_key;
    }
    const firstCol = catalogResult?.columns[0]?.name;
    return firstCol ? [firstCol] : [];
  }, [activeCatalog, catalogResult]);
  const getRowId = (row: PeptideRecord, idx = 0): string => {
    if (!isGenericSchema && row.peptide_id) {
      return String(row.peptide_id);
    }
    if (genericPkColumns.length > 0) {
      return genericPkColumns.map((k) => String(row[k] ?? "")).join("|");
    }
    return `row-${idx}`;
  };

  // Distinct option suggestions combining catalog non-sensitive facets, defaults, and loaded rows
  const organSuggestions = useMemo(() => {
    const set = new Set<string>(
      activeCatalog?.facets?.["organs"] ?? activeCatalog?.facets?.["organ"] ?? DEFAULT_ORGANS,
    );
    for (const r of catalogResult?.rows ?? []) {
      if (r.organ) set.add(r.organ);
    }
    return Array.from(set).sort();
  }, [activeCatalog, catalogResult]);

  const conditionSuggestions = useMemo(() => {
    const set = new Set<string>(
      activeCatalog?.facets?.["conditions"] ??
        activeCatalog?.facets?.["condition"] ??
        DEFAULT_CONDITIONS,
    );
    for (const r of catalogResult?.rows ?? []) {
      if (r.condition) set.add(r.condition);
    }
    return Array.from(set).sort();
  }, [activeCatalog, catalogResult]);

  const alleleSuggestions = useMemo(() => {
    const set = new Set<string>(
      activeCatalog?.facets?.["hla_alleles"] ??
        activeCatalog?.facets?.["hla_allele"] ??
        DEFAULT_ALLELES,
    );
    for (const r of catalogResult?.rows ?? []) {
      if (r.hla_allele) set.add(r.hla_allele);
    }
    for (const a of catalogQuery.hla_alleles ?? []) set.add(a);
    return Array.from(set).sort();
  }, [activeCatalog, catalogResult, catalogQuery.hla_alleles]);

  const geneSuggestions = useMemo(() => {
    const set = new Set<string>(
      activeCatalog?.facets?.["gene_symbols"] ??
        activeCatalog?.facets?.["gene_symbol"] ??
        DEFAULT_GENES,
    );
    for (const r of catalogResult?.rows ?? []) {
      if (r.gene_symbol) set.add(r.gene_symbol);
    }
    for (const g of catalogQuery.gene_symbols ?? []) set.add(g);
    return Array.from(set).sort();
  }, [activeCatalog, catalogResult, catalogQuery.gene_symbols]);

  const openFilterDrawer = () => {
    setDraftOrgans(catalogQuery.organs ?? []);
    setDraftConditions(catalogQuery.conditions ?? []);
    setDraftAlleles(catalogQuery.hla_alleles ?? []);
    setDraftGenes(catalogQuery.gene_symbols ?? []);
    setDraftMinLen(catalogQuery.min_length != null ? String(catalogQuery.min_length) : "");
    setDraftMaxLen(catalogQuery.max_length != null ? String(catalogQuery.max_length) : "");
    setDraftMaxIc50(
      catalogQuery.max_affinity_nm != null ? String(catalogQuery.max_affinity_nm) : "",
    );
    setDraftSeqPrefix(catalogQuery.sequence_prefix ?? "");
    setDraftLimit(catalogQuery.limit ?? 250);
    setCustomAlleleInput("");
    setCustomGeneInput("");
    setFilterDrawerOpen(true);
  };

  const applyFilterDrawer = (e: React.FormEvent) => {
    e.preventDefault();
    const minL = draftMinLen.trim() ? Number(draftMinLen) : undefined;
    const maxL = draftMaxLen.trim() ? Number(draftMaxLen) : undefined;
    const maxIc50 = draftMaxIc50.trim() ? Number(draftMaxIc50) : undefined;
    const extraAlleles = parseTagList(customAlleleInput);
    const extraGenes = parseTagList(customGeneInput);
    const mergedAlleles = Array.from(new Set([...draftAlleles, ...extraAlleles]));
    const mergedGenes = Array.from(new Set([...draftGenes, ...extraGenes]));

    const next: CatalogQuery = {
      table: catalogQuery.table,
      snapshot_id: catalogQuery.snapshot_id,
      ...(draftOrgans.length > 0 ? { organs: draftOrgans } : {}),
      ...(draftConditions.length > 0 ? { conditions: draftConditions } : {}),
      ...(mergedAlleles.length > 0 ? { hla_alleles: mergedAlleles } : {}),
      ...(mergedGenes.length > 0 ? { gene_symbols: mergedGenes } : {}),
      ...(minL != null && Number.isFinite(minL) && minL > 0
        ? { min_length: Math.round(minL) }
        : {}),
      ...(maxL != null && Number.isFinite(maxL) && maxL > 0
        ? { max_length: Math.round(maxL) }
        : {}),
      ...(maxIc50 != null && Number.isFinite(maxIc50) && maxIc50 > 0
        ? { max_affinity_nm: maxIc50 }
        : {}),
      ...(draftSeqPrefix.trim()
        ? { sequence_prefix: draftSeqPrefix.trim().toUpperCase() }
        : {}),
      limit: draftLimit,
    };
    setActiveSavedViewId(null);
    setCurrentPage(1);
    onUpdateCatalogQuery(next);
    setFilterDrawerOpen(false);
  };

  // Active filter chips derived from catalogQuery + quickRowSearch
  const filterChips = useMemo(() => {
    const chips: Array<{ id: string; label: string; onRemove: () => void }> = [];
    for (const org of catalogQuery.organs ?? []) {
      chips.push({
        id: `organ:${org}`,
        label: `Organ: ${org}`,
        onRemove: () => {
          const rest = (catalogQuery.organs ?? []).filter((o) => o !== org);
          setActiveSavedViewId(null);
          setCurrentPage(1);
          onUpdateCatalogQuery({
            ...catalogQuery,
            organs: rest.length > 0 ? rest : undefined,
          });
        },
      });
    }
    for (const cond of catalogQuery.conditions ?? []) {
      chips.push({
        id: `cond:${cond}`,
        label: `Condition: ${cond}`,
        onRemove: () => {
          const rest = (catalogQuery.conditions ?? []).filter((c) => c !== cond);
          setActiveSavedViewId(null);
          setCurrentPage(1);
          onUpdateCatalogQuery({
            ...catalogQuery,
            conditions: rest.length > 0 ? rest : undefined,
          });
        },
      });
    }
    for (const allele of catalogQuery.hla_alleles ?? []) {
      chips.push({
        id: `allele:${allele}`,
        label: `Allele: ${allele}`,
        onRemove: () => {
          const rest = (catalogQuery.hla_alleles ?? []).filter((a) => a !== allele);
          setActiveSavedViewId(null);
          setCurrentPage(1);
          onUpdateCatalogQuery({
            ...catalogQuery,
            hla_alleles: rest.length > 0 ? rest : undefined,
          });
        },
      });
    }
    for (const gene of catalogQuery.gene_symbols ?? []) {
      chips.push({
        id: `gene:${gene}`,
        label: `Gene: ${gene}`,
        onRemove: () => {
          const rest = (catalogQuery.gene_symbols ?? []).filter((g) => g !== gene);
          setActiveSavedViewId(null);
          setCurrentPage(1);
          onUpdateCatalogQuery({
            ...catalogQuery,
            gene_symbols: rest.length > 0 ? rest : undefined,
          });
        },
      });
    }
    if (catalogQuery.min_length != null || catalogQuery.max_length != null) {
      const minStr = catalogQuery.min_length ?? "any";
      const maxStr = catalogQuery.max_length ?? "any";
      chips.push({
        id: "length",
        label: `Length: ${minStr}–${maxStr} aa`,
        onRemove: () => {
          const next = { ...catalogQuery };
          delete next.min_length;
          delete next.max_length;
          setActiveSavedViewId(null);
          setCurrentPage(1);
          onUpdateCatalogQuery(next);
        },
      });
    }
    if (catalogQuery.max_affinity_nm != null) {
      chips.push({
        id: "ic50",
        label: `IC50 ≤ ${catalogQuery.max_affinity_nm} nM`,
        onRemove: () => {
          const next = { ...catalogQuery };
          delete next.max_affinity_nm;
          setActiveSavedViewId(null);
          setCurrentPage(1);
          onUpdateCatalogQuery(next);
        },
      });
    }
    if (catalogQuery.sequence_prefix) {
      chips.push({
        id: "seq_prefix",
        label: `Sequence starts with: ${catalogQuery.sequence_prefix}`,
        onRemove: () => {
          const next = { ...catalogQuery };
          delete next.sequence_prefix;
          setActiveSavedViewId(null);
          setCurrentPage(1);
          onUpdateCatalogQuery(next);
        },
      });
    }
    if (catalogQuery.snapshot_id != null) {
      chips.push({
        id: "snapshot_pin",
        label: `Time-travel snapshot #${catalogQuery.snapshot_id}`,
        onRemove: () => {
          const next = { ...catalogQuery };
          delete next.snapshot_id;
          setActiveSavedViewId(null);
          setCurrentPage(1);
          onUpdateCatalogQuery(next);
        },
      });
    }
    if (quickRowSearch.trim()) {
      chips.push({
        id: "quick_search",
        label: `Row search: "${quickRowSearch.trim()}"`,
        onRemove: () => {
          setQuickRowSearch("");
          setCurrentPage(1);
        },
      });
    }
    return chips;
  }, [catalogQuery, quickRowSearch, onUpdateCatalogQuery]);

  const resetAllFilters = () => {
    setQuickRowSearch("");
    setActiveSavedViewId(null);
    setCurrentPage(1);
    onUpdateCatalogQuery({ table: catalogQuery.table, limit: catalogQuery.limit ?? 250 });
  };

  // Client-side quick search + sorting over loaded rows
  const filteredAndSortedRows = useMemo<PeptideRecord[]>(() => {
    const base = catalogResult?.rows ?? [];
    const needle = quickRowSearch.trim().toLowerCase();
    const matched = needle
      ? base.filter((r) =>
          isGenericSchema
            ? Object.values(r)
                .map((v) => String(v ?? ""))
                .join(" ")
                .toLowerCase()
                .includes(needle)
            : `${r.peptide_id} ${r.sequence} ${r.hla_allele} ${r.gene_symbol} ${r.organ} ${r.condition}`
                .toLowerCase()
                .includes(needle),
        )
      : base;

    if (!sortBy || !schemaNames.has(sortBy.key)) return matched;
    const { key, dir } = sortBy;
    const factor = dir === "asc" ? 1 : -1;
    return [...matched].sort((a, b) => {
      const va = a[key];
      const vb = b[key];
      if (typeof va === "number" && typeof vb === "number") {
        return (va - vb) * factor;
      }
      return String(va).localeCompare(String(vb)) * factor;
    });
  }, [catalogResult, quickRowSearch, isGenericSchema, sortBy, schemaNames]);

  const totalPages = Math.max(1, Math.ceil(filteredAndSortedRows.length / pageSize));
  const safePage = Math.min(currentPage, totalPages);

  const pagedRows = useMemo(() => {
    const start = (safePage - 1) * pageSize;
    return filteredAndSortedRows.slice(start, start + pageSize);
  }, [filteredAndSortedRows, safePage, pageSize]);

  const visibleColumns = useMemo(
    () => tableColumns.filter((c) => !hiddenColumns.has(c.key)),
    [tableColumns, hiddenColumns],
  );

  // Grouped buckets over pagedRows when groupBy !== "none"
  const groupedBuckets = useMemo(() => {
    if (isGenericSchema || groupBy === "none") return null;
    const map = new Map<string, PeptideRecord[]>();
    for (const row of pagedRows) {
      const label = String(row[groupBy] || "Unspecified");
      const arr = map.get(label);
      if (arr) arr.push(row);
      else map.set(label, [row]);
    }
    return Array.from(map.entries()).map(([groupLabel, rows]) => {
      const sortedIc50 = rows.map((r) => r.affinity_nm).sort((a, b) => a - b);
      const mid = Math.floor(sortedIc50.length / 2);
      const medianIc50 =
        sortedIc50.length === 0
          ? 0
          : sortedIc50.length % 2 === 1
            ? sortedIc50[mid]!
            : (sortedIc50[mid - 1]! + sortedIc50[mid]!) / 2;
      return { groupLabel, rows, medianIc50 };
    });
  }, [pagedRows, groupBy, isGenericSchema]);

  const selectedRecords = useMemo(() => {
    if (selectedRowIds.size === 0) return [];
    const all = catalogResult?.rows ?? [];
    return all.filter((r, i) => selectedRowIds.has(getRowId(r, i)));
  }, [catalogResult, selectedRowIds, isGenericSchema, genericPkColumns]);

  const allPagedSelected =
    pagedRows.length > 0 &&
    pagedRows.every((r, i) => selectedRowIds.has(getRowId(r, i)));

  const toggleSelectAllPaged = () => {
    setSelectedRowIds((prev) => {
      const next = new Set(prev);
      if (allPagedSelected) {
        pagedRows.forEach((r, i) => next.delete(getRowId(r, i)));
      } else {
        pagedRows.forEach((r, i) => next.add(getRowId(r, i)));
      }
      return next;
    });
  };

  const toggleRowSelection = (peptideId: string) => {
    setSelectedRowIds((prev) => {
      const next = new Set(prev);
      if (next.has(peptideId)) next.delete(peptideId);
      else next.add(peptideId);
      return next;
    });
  };

  const handleSortHeader = (key: string) => {
    setSortBy((prev) => {
      if (!prev || prev.key !== key) return { key, dir: "asc" };
      if (prev.dir === "asc") return { key, dir: "desc" };
      return null;
    });
  };

  // Saved Views persistence helpers
  const persistSavedViews = (next: SavedDataView[]) => {
    setSavedViews(next);
    try {
      localStorage.setItem(SAVED_VIEWS_STORAGE_KEY, JSON.stringify(next));
    } catch {
      // ignore storage errors
    }
  };

  const handleCreateSavedView = async (e: React.FormEvent) => {
    e.preventDefault();
    const trimmed = newViewName.trim();
    if (!trimmed || !activeCatalog) return;
    setSaveViewError(null);
    if (onCreateSavedView) {
      setSavingView(true);
      try {
        const rec = await onCreateSavedView(trimmed, { ...catalogQuery });
        setActiveSavedViewId(rec.view_id);
        setNewViewName("");
        setSaveViewModalOpen(false);
      } catch (err: unknown) {
        setSaveViewError(formatErrorInline(err));
      } finally {
        setSavingView(false);
      }
      return;
    }
    const created: SavedDataView = {
      id: `view_${Date.now()}_${Math.random().toString(36).slice(2, 7)}`,
      name: trimmed,
      catalogId: activeCatalog.id,
      query: { ...catalogQuery },
      hiddenColumns: Array.from(hiddenColumns),
      density,
      groupBy,
      createdAt: new Date().toISOString(),
    };
    persistSavedViews([created, ...savedViews]);
    setActiveSavedViewId(created.id);
    setNewViewName("");
    setSaveViewModalOpen(false);
  };

  const handleApplySavedView = (view: SavedDataView) => {
    if (view.catalogId !== activeCatalogId) {
      onSelectPreviewCatalog(view.catalogId);
    }
    const sanitizedQuery: CatalogQuery = { ...view.query };
    const savedCatalog = catalogs.find(catalog => catalog.id === view.catalogId);
    const savedTable = view.query.table ?? savedCatalog?.tables?.[0]?.name;
    setHiddenColumns(new Set(view.hiddenColumns), JSON.stringify([view.catalogId, savedTable]));
    setDensity(view.density);
    setGroupBy(view.groupBy);
    setQuickRowSearch("");
    setCurrentPage(1);
    setActiveSavedViewId(view.id);
    onUpdateCatalogQuery(sanitizedQuery);
  };

  const handleApplyBackendSavedView = (view: SavedViewRecord) => {
    if (view.primary_source.catalog_id !== activeCatalogId) {
      onSelectPreviewCatalog(view.primary_source.catalog_id);
    }
    const nextQ = queryAstToCatalogQuery(view.query_ast);
    setQuickRowSearch("");
    setCurrentPage(1);
    setActiveSavedViewId(view.view_id);
    onUpdateCatalogQuery(nextQ);
  };

  const handleDeleteSavedView = (viewId: string) => {
    persistSavedViews(savedViews.filter((v) => v.id !== viewId));
    if (activeSavedViewId === viewId) setActiveSavedViewId(null);
  };

  const viewsForActiveCatalog = useMemo(
    () => savedViews.filter((v) => v.catalogId === activeCatalogId),
    [savedViews, activeCatalogId],
  );
  const backendViewsForActiveCatalog = useMemo(
    () =>
      backendSavedViews.filter(
        (v) => v.primary_source.catalog_id === activeCatalogId,
      ),
    [backendSavedViews, activeCatalogId],
  );

  // Paste Identifiers resolver
  const runIdentifierMatching = () => {
    const terms = Array.from(
      new Set(
        pasteInput
          .split(/[\r\n,;\t ]+/)
          .map((t) => t.trim())
          .filter(Boolean),
      ),
    );
    if (terms.length === 0) return;

    const rows = catalogResult?.rows ?? [];
    const upperTerms = new Map(terms.map((t) => [t.toUpperCase(), t]));
    const matchedTermKeys = new Set<string>();
    const matchedRows: PeptideRecord[] = [];

    for (const row of rows) {
      const candidate =
        pasteKind === "sequence"
          ? row.sequence
          : pasteKind === "peptide_id"
            ? row.peptide_id
            : pasteKind === "gene_symbol"
              ? row.gene_symbol
              : row.hla_allele;
      const key = candidate.toUpperCase();
      if (upperTerms.has(key)) {
        matchedTermKeys.add(key);
        matchedRows.push(row);
      }
    }

    const unmatchedTerms = terms.filter((t) => !matchedTermKeys.has(t.toUpperCase()));
    setPasteReport({
      matchedRows,
      unmatchedTerms,
      matchedTermCount: matchedTermKeys.size,
    });
  };

  const applyPasteMatches = () => {
    if (!pasteReport || pasteReport.matchedRows.length === 0) return;
    setSelectedRowIds(new Set(pasteReport.matchedRows.map((r) => r.peptide_id)));
    setPasteModalOpen(false);
  };

  const handleExport = (format: "csv" | "tsv" | "json", targetRows?: PeptideRecord[]) => {
    const rowsToExport =
      targetRows ??
      (selectedRecords.length > 0 ? selectedRecords : filteredAndSortedRows);
    const slug = (activeCatalog?.id ?? "catalog").replace(/[^a-zA-Z0-9_-]/g, "_");
    const content = serializeRowsForExport(rowsToExport, visibleColumns, format);
    const mime =
      format === "json"
        ? "application/json"
        : format === "tsv"
          ? "text/tab-separated-values"
          : "text/csv";
    downloadTextFile(`${slug}_export.${format}`, content, mime);
    setOpenMenu(null);
  };

  const installedPlugins = useMemo(() => {
    const scorePlugin = (p: (typeof plugins)[number]): number => {
      const props = p.manifest.inputs?.schema?.["properties"] as
        | Record<string, unknown>
        | undefined;
      if (!props) return 0;
      let score = 0;
      if ("alleles" in props || "hla_alleles" in props) score += 3;
      if ("peptides" in props || "sequences" in props) score += 3;
      if ("max_ic50_nm" in props || "max_affinity_nm" in props) score += 2;
      if ("genes" in props || "gene_symbols" in props) score += 1;
      return score;
    };
    return plugins
      .filter((p) => p.installed)
      .sort((a, b) => scorePlugin(b) - scorePlugin(a));
  }, [plugins]);

  const schemaColumnMap = useMemo(() => {
    const m = new Map<string, ColumnDescriptor>();
    for (const tbl of activeCatalog?.tables ?? []) {
      for (const col of tbl.columns) {
        if (!m.has(col.name)) {
          m.set(col.name, col);
        }
      }
    }
    return m;
  }, [activeCatalog]);

  const renderRowCells = (row: PeptideRecord, rowIdx = 0) => {
    const rid = getRowId(row, rowIdx);
    const isChecked = selectedRowIds.has(rid);
    if (isGenericSchema && catalogResult) {
      return (
        <tr
          key={rid}
          className="bc-explorer-row"
          data-selected={String(isChecked)}
          onClick={(event) => {
            if ((event.target as HTMLElement).closest("input, button, a, label")) return;
            setDetailRows([row]);
            setFocusedDetailRow(row);
          }}
        >
          <td className="bc-table-check-cell">
            <input
              type="checkbox"
              aria-label={`Select ${rid}`}
              checked={isChecked}
              onChange={() => toggleRowSelection(rid)}
            />
          </td>
          {visibleColumns.map((col) => (
            <td key={col.key} className="mono">
              {row[col.key] == null ? "—" : String(row[col.key])}
            </td>
          ))}
        </tr>
      );
    }
    return (
      <tr
        key={row.peptide_id}
        className="bc-explorer-row"
        data-selected={String(isChecked)}
        onClick={(e) => {
          const target = e.target as HTMLElement;
          if (target.closest("input, button, a, label")) return;
          setDetailRows([row]);
          setFocusedDetailRow(row);
        }}
      >
        <td className="bc-table-check-cell">
          <input
            type="checkbox"
            aria-label={`Select ${row.peptide_id}`}
            checked={isChecked}
            onChange={() => toggleRowSelection(row.peptide_id)}
          />
        </td>
        {visibleColumns.map((col) => {
          switch (col.key) {
            case "peptide_id":
              return (
                <td key={col.key} className="mono">
                  {row.peptide_id}
                </td>
              );
            case "sequence":
              return (
                <td key={col.key} className="mono accent-seq">
                  {row.sequence}
                </td>
              );
            case "peptide_length":
              return <td key={col.key}>{row.peptide_length}</td>;
            case "hla_allele":
              return (
                <td key={col.key} className="mono">
                  {row.hla_allele}
                </td>
              );
            case "gene_symbol":
              return (
                <td key={col.key}>
                  <strong>{row.gene_symbol}</strong>
                </td>
              );
            case "organ":
              return <td key={col.key}>{row.organ}</td>;
            case "condition":
              return <td key={col.key}>{row.condition}</td>;
            case "affinity_nm":
              return <td key={col.key}>{Number(row.affinity_nm ?? 0).toFixed(1)}</td>;
            case "eluted_rank_pct":
              return <td key={col.key}>{Number(row.eluted_rank_pct ?? 0).toFixed(2)}%</td>;
            case "ms_intensity_log2":
              return <td key={col.key}>{Number(row.ms_intensity_log2 ?? 0).toFixed(2)}</td>;
            case "donor_count":
              return <td key={col.key}>{row.donor_count}</td>;
            default:
              return <td key={col.key}>{row[col.key] == null ? "—" : String(row[col.key])}</td>;
          }
        })}
      </tr>
    );
  };

  const activeInspectionRecord =
    focusedDetailRow ?? (detailRows && detailRows.length === 1 ? detailRows[0]! : null);

  return (
    <div className="bc-data-explorer-shell">
      <div className="bc-page-scroll">
        <div className="bc-page-container bc-data-explorer-container">
        {/* ==================== PAGE HEADER ==================== */}
        <div className="bc-page-header">
          <div>
            <h1 className="bc-page-title">Data</h1>
            <p className="bc-page-subtitle">
              {catalogs.length} accessible {catalogs.length === 1 ? "database" : "databases"} (
              {selectedCatalogCount} selected for chat)
              {activeCatalog && (
                <>
                  {" "}
                  · Exploring <span className="mono">{activeCatalog.id}</span>
                </>
              )}
            </p>
          </div>

          <div className="bc-data-header-controls">
            {catalogs.length > 1 && (
              <div className="bc-search-field">
                <IconSearch size={13} />
                <input
                  type="search"
                  data-testid="catalog-search-input"
                  placeholder="Search databases…"
                  value={catalogSearch}
                  onChange={(e) => setCatalogSearch(e.target.value)}
                  aria-label="Search databases"
                />
              </div>
            )}

            {activeGrant?.level === "rows" && activeCatalog?.tables && activeCatalog.tables.length > 1 && (
              <label className="bc-grant-pill-wrap">
                Table
                <select
                  aria-label="Dataset table"
                  className="bc-grant-select"
                  value={activeCatalog.tables.find(table => table.name === catalogQuery.table)?.name ?? activeCatalog.tables[0]?.name}
                  onChange={event => onUpdateCatalogQuery({ ...catalogQuery, table: event.target.value })}
                >
                  {activeCatalog.tables.map(table => <option key={table.name} value={table.name}>{table.name}</option>)}
                </select>
              </label>
            )}

            {grants.length > 0 && (
              <div className="bc-grant-pill-wrap" title="Active Data Access Grant">
                <IconShield size={13} />
                <select
                  aria-label="Active Data Grant"
                  data-testid="grant-select"
                  className="bc-grant-select"
                  value={activeGrant?.grant_id ?? ""}
                  onChange={(e) => onGrantChange(e.target.value)}
                >
                  {grants.map((g) => (
                    <option key={g.grant_id} value={g.grant_id}>
                      {g.label}
                    </option>
                  ))}
                </select>
              </div>
            )}

            {activeGrant && (
              <span
                data-testid="catalog-access-indicator"
                className={`bc-badge ${
                  isComputeOnly ? "bc-badge-warn" : "bc-badge-success"
                }`}
              >
                {isComputeOnly
                  ? "Compute-Only (Curated Modules)"
                  : activeGrant.row_scope
                    ? "Scoped Unmasked Rows"
                    : "Full Unmasked Row Access"}
              </span>
            )}

            {remainingDailyBudget != null && (
              <span
                data-testid="daily-row-budget-pill"
                className="bc-chip mono"
                title="Remaining daily row inspection budget under active grant"
              >
                {remainingDailyBudget.toLocaleString()} rows left today
              </span>
            )}

            <button
              type="button"
              className="bc-btn bc-btn-primary bc-btn-sm"
              data-testid="open-import-wizard-btn"
              onClick={() => setPickerOpen(true)}
            >
              <IconPlus size={12} />
              <span>Upload or import</span>
            </button>
          </div>
        </div>

        {/* ==================== DATABASE CARDS ==================== */}
        {filteredCatalogs.length > 0 ? (
          <div className="bc-catalog-grid" data-testid="catalog-cards-grid">
            {filteredCatalogs.map((cat) => {
              const isSelected = selectedCatalogIds.includes(cat.id);
              const isPreview = cat.id === activeCatalog?.id;
              const columnCount =
                cat.tables?.reduce((acc, tbl) => acc + tbl.columns.length, 0) ?? 0;
              const shardList = cat.shards ?? [];
              const description = catalogDescription(cat);
              return (
                <div
                  key={cat.id}
                  className="bc-catalog-card"
                  data-testid={`catalog-card-${cat.id}`}
                  data-selected={String(isSelected)}
                  data-preview={String(isPreview)}
                  onClick={() => onSelectPreviewCatalog(cat.id)}
                  role="button"
                  tabIndex={0}
                  onKeyDown={(e) => {
                    if (e.key === "Enter" || e.key === " ") {
                      e.preventDefault();
                      onSelectPreviewCatalog(cat.id);
                    }
                  }}
                >
                  <div className="bc-catalog-card-top">
                    <div className="bc-catalog-card-id-wrap">
                      <span className="bc-catalog-card-icon">
                        <IconDatabase size={16} />
                      </span>
                      <div style={{ minWidth: 0 }}>
                        <h3 className="bc-catalog-card-title">{cat.name}</h3>
                        <span className="bc-catalog-card-id mono">{cat.id}</span>
                      </div>
                    </div>

                    <div className="bc-catalog-card-actions">
                      <button
                        type="button"
                        className="bc-icon-btn bc-catalog-info-btn"
                        data-testid={`catalog-inspect-${cat.id}`}
                        title="Inspect schema, Parquet shards & governance policy"
                        aria-label={`Inspect ${cat.name}`}
                        onClick={(e) => {
                          e.stopPropagation();
                          setProvenanceCatalogId(cat.id);
                        }}
                      >
                        <IconAbout size={14} />
                      </button>

                      <button
                        type="button"
                        className="bc-catalog-select-btn"
                        data-testid={`catalog-select-${cat.id}`}
                        data-selected={String(isSelected)}
                        aria-pressed={isSelected}
                        onClick={(e) => {
                          e.stopPropagation();
                          onToggleCatalogSelection(cat.id);
                        }}
                      >
                        {isSelected && <IconCheckMark size={12} />}
                        <span>{isSelected ? "Selected" : "Select"}</span>
                      </button>
                    </div>
                  </div>

                  <CatalogStorage catalog={cat} />
                  {description && <p className="bc-catalog-card-desc">{description}</p>}

                  <div className="bc-catalog-card-meta">
                    <span className="bc-chip mono">v{cat.version}</span>
                    {cat.organism && <span className="bc-chip">{cat.organism}</span>}
                    {typeof cat.row_count === "number" && (
                      <span className="bc-chip mono">{cat.row_count.toLocaleString()} rows</span>
                    )}
                    {columnCount > 0 && <span className="bc-chip mono">{columnCount} cols</span>}
                    {shardList.length > 0 && (
                      <span className="bc-chip mono">
                        {shardList.length} {shardList.length === 1 ? "shard" : "shards"}
                      </span>
                    )}
                    {isPreview && (
                      <span className="bc-chip bc-chip-active">Exploring</span>
                    )}
                  </div>
                </div>
              );
            })}
          </div>
        ) : (
          <div className="bc-empty-state">
            <p>
              {catalogSearch.trim()
                ? "No databases match your search."
                : "No accessible databases were returned for this node."}
            </p>
          </div>
        )}

        {/* ==================== SAVED VIEWS STRIP ==================== */}
        {onCreateSavedView ? (
          <SavedViewsPanel
            views={backendViewsForActiveCatalog}
            activeViewId={activeSavedViewId}
            onApplyView={handleApplyBackendSavedView}
            onDeleteView={async (viewId) => {
              if (onDeleteSavedView) {
                await onDeleteSavedView(viewId);
              }
              if (activeSavedViewId === viewId) {
                setActiveSavedViewId(null);
              }
            }}
            onVerifiedView={(viewId, report) => onVerifiedSavedView?.(viewId, report)}
          />
        ) : (
          viewsForActiveCatalog.length > 0 && (
            <div className="bc-saved-views-bar" data-testid="saved-views-bar">
              <span className="bc-saved-views-label">
                <IconBookmark size={12} />
                <span>Saved views</span>
              </span>
              <div className="bc-saved-views-list">
                {viewsForActiveCatalog.map((view) => {
                  const isActive = view.id === activeSavedViewId;
                  return (
                    <div
                      key={view.id}
                      className="bc-saved-view-pill"
                      data-active={String(isActive)}
                    >
                      <button
                        type="button"
                        className="bc-saved-view-btn"
                        onClick={() => handleApplySavedView(view)}
                      >
                        {view.name}
                      </button>
                      <button
                        type="button"
                        className="bc-saved-view-del"
                        aria-label={`Delete saved view ${view.name}`}
                        onClick={() => handleDeleteSavedView(view.id)}
                      >
                        <IconClose size={11} />
                      </button>
                    </div>
                  );
                })}
              </div>
            </div>
          )
        )}

        {/* ==================== DATA EXPLORER WORKBENCH ==================== */}
        {activeCatalog && (
          <div className="bc-card bc-explorer-card">
            {/* Workbench Header */}
            <div className="bc-explorer-header">
              <div className="bc-explorer-title-group">
                <span className="bc-card-title">{activeCatalog.name}</span>
                {catalogResult && (
                  <span className="bc-caption mono">
                    {filteredAndSortedRows.length.toLocaleString()} shown ·{" "}
                    {catalogResult.total_in_scope.toLocaleString()} in grant scope
                    {catalogLoading ? " · Refreshing…" : ""}
                  </span>
                )}
              </div>

              <div className="bc-explorer-header-actions">
                {activeGrant?.level === "rows" && !catalogLoading && catalogResult?.access_level === "rows" &&
                  catalogResult.catalog_id === activeCatalog.id && catalogResult.grant_id === activeGrant.grant_id &&
                  catalogResult.columns.length > 0 && (!selectedTable || catalogResult.table === selectedTable.name) &&
                  <CatalogTableSearch
                    key={JSON.stringify([activeCatalog.id, activeCatalog.release_version, activeGrant, catalogResult.table,
                      catalogResult.snapshot_id, catalogResult.dataset_digest, catalogResult.columns, catalogQuery])}
                    preview={catalogResult} query={catalogQuery} primaryKey={selectedTable?.primary_key ?? []}
                    releaseVersion={activeCatalog.release_version}
                  />}
                <button
                  type="button"
                  className="bc-btn bc-btn-sm"
                  data-testid="open-provenance-drawer"
                  onClick={() => setProvenanceCatalogId(activeCatalog.id)}
                >
                  <IconShield size={13} />
                  <span>Provenance &amp; Policy</span>
                </button>
              </div>
            </div>

            {/* Workbench Toolbar */}
            <div className="bc-data-toolbar" ref={toolbarRef}>
              <button
                type="button"
                className="bc-btn bc-btn-sm"
                data-testid="inspect-selected-btn"
                disabled={selectedRecords.length === 0}
                title={
                  selectedRecords.length > 0
                    ? `Inspect ${selectedRecords.length} selected record(s)`
                    : "Select rows to inspect details"
                }
                onClick={() => {
                  setDetailRows(selectedRecords);
                  setFocusedDetailRow(
                    selectedRecords.length === 1 ? selectedRecords[0]! : null,
                  );
                }}
              >
                <IconEye size={13} />
                <span>
                  View{selectedRecords.length > 0 ? ` (${selectedRecords.length})` : ""}
                </span>
              </button>

              <button
                type="button"
                className={`bc-btn bc-btn-sm ${
                  filterChips.length > 0 ? "bc-btn-accent-outline" : ""
                }`}
                data-testid="open-filter-drawer"
                onClick={openFilterDrawer}
              >
                <IconFilter size={13} />
                <span>
                  Filters{filterChips.length > 0 ? ` (${filterChips.length})` : ""}
                </span>
              </button>

              {/* Group By Popover */}
              {!isGenericSchema && <div className="bc-popover-anchor">
                <button
                  type="button"
                  className={`bc-btn bc-btn-sm ${
                    groupBy !== "none" ? "bc-btn-accent-outline" : ""
                  }`}
                  data-testid="group-by-btn"
                  onClick={() =>
                    setOpenMenu((prev) => (prev === "group" ? null : "group"))
                  }
                >
                  <IconGroupBy size={13} />
                  <span>
                    Group{groupBy !== "none" ? `: ${groupBy.replace("_", " ")}` : ""}
                  </span>
                </button>
                {openMenu === "group" && (
                  <div className="bc-popover-menu" role="menu">
                    <div className="bc-popover-section-label">Group page rows by</div>
                    {(
                      [
                        { id: "none", label: "None" },
                        { id: "organ", label: "Organ" },
                        { id: "condition", label: "Condition" },
                        { id: "hla_allele", label: "HLA Allele" },
                        { id: "gene_symbol", label: "Gene Symbol" },
                      ] as const
                    ).map((opt) => (
                      <button
                        key={opt.id}
                        type="button"
                        className="bc-popover-item"
                        data-active={String(groupBy === opt.id)}
                        onClick={() => {
                          setGroupBy(opt.id);
                          setCollapsedGroups(new Set());
                          setOpenMenu(null);
                        }}
                      >
                        <span>{opt.label}</span>
                        {groupBy === opt.id && <IconCheck size={13} />}
                      </button>
                    ))}
                  </div>
                )}
              </div>}

              {/* Columns & Density Popover */}
              <div className="bc-popover-anchor">
                <button
                  type="button"
                  className="bc-btn bc-btn-sm"
                  data-testid="columns-popover-btn"
                  onClick={() =>
                    setOpenMenu((prev) => (prev === "columns" ? null : "columns"))
                  }
                >
                  <IconColumns size={13} />
                  <span>
                    Columns{" "}
                    <span className="mono" style={{ opacity: 0.75 }}>
                      {visibleColumns.length}/{tableColumns.length}
                    </span>
                  </span>
                </button>
                {openMenu === "columns" && (
                  <div className="bc-popover-menu bc-popover-wide" role="dialog">
                    <div className="bc-popover-section-label">Row spacing</div>
                    <div className="bc-segmented-row">
                      <button
                        type="button"
                        className="bc-segmented-btn"
                        data-active={String(density === "compact")}
                        onClick={() => setDensity("compact")}
                      >
                        Compact
                      </button>
                      <button
                        type="button"
                        className="bc-segmented-btn"
                        data-active={String(density === "comfortable")}
                        onClick={() => setDensity("comfortable")}
                      >
                        Comfortable
                      </button>
                    </div>

                    <div
                      className="bc-popover-section-label"
                      style={{
                        marginTop: 10,
                        display: "flex",
                        justifyContent: "space-between",
                        alignItems: "center",
                      }}
                    >
                      <span>Visible columns</span>
                      {visibleColumns.length < tableColumns.length && (
                        <button
                          type="button"
                          className="bc-link-btn"
                          onClick={() => setHiddenColumns(new Set())}
                        >
                          Show all
                        </button>
                      )}
                    </div>
                    <div className="bc-columns-checklist">
                      {tableColumns.map((col) => {
                        const checked = !hiddenColumns.has(col.key);
                        return (
                          <label key={col.key} className="bc-check-row">
                            <input
                              type="checkbox"
                              checked={checked}
                              disabled={checked && visibleColumns.length <= (isGenericSchema ? 1 : 2)}
                              onChange={(e) => {
                                const next = new Set(hiddenColumns);
                                if (e.target.checked) next.delete(col.key);
                                else next.add(col.key);
                                setHiddenColumns(next);
                              }}
                            />
                            <span>{col.label}</span>
                          </label>
                        );
                      })}
                    </div>
                  </div>
                )}
              </div>

              {/* Quick Row Filter */}
              <div className="bc-search-field bc-toolbar-search">
                <IconSearch size={13} />
                <input
                  type="search"
                  data-testid="row-quick-search"
                  placeholder={isGenericSchema ? "Search values in these rows…" : "Filter rows (sequence, allele, gene, organ)…"}
                  value={quickRowSearch}
                  onChange={(e) => {
                    setQuickRowSearch(e.target.value);
                    setCurrentPage(1);
                  }}
                  aria-label="Filter loaded rows"
                />
              </div>

              {!isGenericSchema && <button
                type="button"
                className="bc-btn bc-btn-sm"
                data-testid="paste-identifiers-btn"
                onClick={() => {
                  setPasteKind("sequence");
                  setPasteInput("");
                  setPasteReport(null);
                  setPasteModalOpen(true);
                }}
              >
                <span>Paste IDs</span>
              </button>}

              <div style={{ flex: 1 }} />

              <button
                type="button"
                className="bc-btn bc-btn-sm"
                data-testid="save-view-btn"
                onClick={() => {
                  setNewViewName("");
                  setSaveViewError(null);
                  setSaveViewModalOpen(true);
                }}
              >
                <IconBookmark size={13} />
                <span>Save View</span>
              </button>

              <div className="bc-popover-anchor">
                <button
                  type="button"
                  className="bc-btn bc-btn-sm"
                  data-testid="export-menu-btn"
                  disabled={filteredAndSortedRows.length === 0 || !canLocalExport}
                  title={
                    !canLocalExport
                      ? "Local export disabled by active dataset grant"
                      : undefined
                  }
                  onClick={() =>
                    setOpenMenu((prev) => (prev === "export" ? null : "export"))
                  }
                >
                  <IconDownload size={13} />
                  <span>
                    Export{selectedRecords.length > 0 ? ` (${selectedRecords.length})` : ""}
                  </span>
                </button>
                {openMenu === "export" && (
                  <div className="bc-popover-menu bc-popover-right" role="menu">
                    <div className="bc-popover-section-label">
                      {selectedRecords.length > 0
                        ? `Export ${selectedRecords.length} selected rows`
                        : `Export ${filteredAndSortedRows.length} visible rows`}
                    </div>
                    <button
                      type="button"
                      className="bc-popover-item"
                      onClick={() => handleExport("csv")}
                    >
                      <span>Download CSV (.csv)</span>
                    </button>
                    <button
                      type="button"
                      className="bc-popover-item"
                      onClick={() => handleExport("tsv")}
                    >
                      <span>Download TSV (.tsv)</span>
                    </button>
                    <button
                      type="button"
                      className="bc-popover-item"
                      onClick={() => handleExport("json")}
                    >
                      <span>Download JSON (.json)</span>
                    </button>
                  </div>
                )}
              </div>
            </div>

            {/* Active Filter Chips */}
            {filterChips.length > 0 && (
              <div className="bc-filter-chips-bar" data-testid="active-filter-chips">
                <span className="bc-caption">Matching all:</span>
                {filterChips.map((chip) => (
                  <span key={chip.id} className="bc-filter-chip mono">
                    <span>{chip.label}</span>
                    <button
                      type="button"
                      aria-label={`Remove ${chip.label}`}
                      onClick={chip.onRemove}
                    >
                      <IconClose size={10} />
                    </button>
                  </span>
                ))}
                <button
                  type="button"
                  className="bc-link-btn"
                  onClick={openFilterDrawer}
                >
                  <IconPlus size={11} /> Add
                </button>
                <div style={{ flex: 1 }} />
                <button
                  type="button"
                  className="bc-link-btn"
                  onClick={resetAllFilters}
                >
                  Clear all
                </button>
              </div>
            )}

            {/* Data Table */}
            {filteredAndSortedRows.length > 0 ? (
              <div className="table-scroll">
                <table
                  className="data-table bc-explorer-table"
                  data-testid="catalog-table"
                  data-density={density}
                >
                  <thead>
                    <tr>
                      <th className="bc-table-check-cell">
                        <input
                          type="checkbox"
                          aria-label="Select all rows on this page"
                          checked={allPagedSelected}
                          onChange={toggleSelectAllPaged}
                        />
                      </th>
                      {visibleColumns.map((col) => {
                            const isSorted = sortBy?.key === col.key;
                            return (
                              <th key={col.key}>
                                <button
                                  type="button"
                                  className="bc-th-sort-btn"
                                  onClick={() => handleSortHeader(col.key)}
                                >
                                  <span>{col.label}</span>
                                  <span className="bc-sort-indicator" aria-hidden="true">
                                    {isSorted ? (sortBy.dir === "asc" ? "↑" : "↓") : ""}
                                  </span>
                                </button>
                              </th>
                            );
                          })}
                    </tr>
                  </thead>
                  <tbody>
                    {groupedBuckets && !isGenericSchema
                      ? groupedBuckets.map((bucket) => {
                          const isCollapsed = collapsedGroups.has(bucket.groupLabel);
                          return (
                            <React.Fragment key={bucket.groupLabel}>
                              <tr className="bc-group-header-row">
                                <td colSpan={visibleColumns.length + 1}>
                                  <button
                                    type="button"
                                    className="bc-group-toggle-btn"
                                    aria-expanded={!isCollapsed}
                                    onClick={() => {
                                      setCollapsedGroups((prev) => {
                                        const next = new Set(prev);
                                        if (next.has(bucket.groupLabel)) {
                                          next.delete(bucket.groupLabel);
                                        } else {
                                          next.add(bucket.groupLabel);
                                        }
                                        return next;
                                      });
                                    }}
                                  >
                                    <span
                                      className="bc-group-chevron"
                                      data-open={String(!isCollapsed)}
                                    >
                                      <IconChevronRight size={11} />
                                    </span>
                                    <strong>{bucket.groupLabel}</strong>
                                    <span className="bc-chip mono">
                                      {bucket.rows.length}{" "}
                                      {bucket.rows.length === 1 ? "row" : "rows"}
                                    </span>
                                    <span className="bc-caption mono">
                                      median IC50 {bucket.medianIc50.toFixed(1)} nM
                                    </span>
                                  </button>
                                </td>
                              </tr>
                              {!isCollapsed &&
                                bucket.rows.map((r, idx) => renderRowCells(r, idx))}
                            </React.Fragment>
                          );
                        })
                      : pagedRows.map((r, idx) => renderRowCells(r, idx))}
                  </tbody>
                </table>
              </div>
            ) : isComputeOnly ? (
              <div className="bc-explorer-empty" data-testid="compute-only-banner">
                <p style={{ margin: 0, fontWeight: 600 }}>
                  Compute-Only Access Policy Active ({activeGrant?.label})
                </p>
                <p className="bc-caption" style={{ marginTop: 4, maxWidth: 560, marginInline: "auto" }}>
                  Direct row inspection and local exports are blocked on{" "}
                  <code className="mono">compute_only</code> grants. Non-sensitive schema and facets remain
                  inspectable, and approved curated modules execute against sealed snapshot{" "}
                  <code className="mono">#{activeCatalog.latest_snapshot_id}</code>.
                </p>
                {Object.keys(activeCatalog.facets ?? {}).length > 0 && (
                  <div
                    style={{
                      marginTop: 10,
                      display: "flex",
                      gap: 6,
                      flexWrap: "wrap",
                      justifyContent: "center",
                    }}
                  >
                    {Object.entries(activeCatalog.facets).map(([facetKey, vals]) => (
                      <span key={facetKey} className="bc-chip mono">
                        {facetKey}: {vals.slice(0, 4).join(", ")}
                        {vals.length > 4 ? ` +${vals.length - 4}` : ""}
                      </span>
                    ))}
                  </div>
                )}
                {installedPlugins[0] && (
                  <div style={{ marginTop: 14, display: "flex", justifyContent: "center" }}>
                    <button
                      type="button"
                      className="bc-btn bc-btn-primary bc-btn-sm"
                      data-testid="compute-only-run-module-btn"
                      onClick={() => {
                        const target = installedPlugins[0]!;
                        const params = deriveModuleParamsFromSelection(
                          target,
                          [],
                          catalogQuery,
                        );
                        onRunInModule(target, params, []);
                      }}
                    >
                      <IconModules size={13} />
                      <span>Run in Curated Module ({installedPlugins[0].manifest.name})</span>
                    </button>
                  </div>
                )}
              </div>
            ) : (
              <div className="bc-explorer-empty">
                <p style={{ margin: 0, fontWeight: 500 }}>
                  {catalogLoading
                    ? "Loading cohort records…"
                    : filterChips.length > 0
                      ? "No records match the current filters."
                      : "No records are available under this dataset grant."}
                </p>
                <p className="bc-caption" style={{ marginTop: 4 }}>
                  This reflects the active filters and grant policy, not biological absence.
                </p>
                {filterChips.length > 0 && (
                  <div style={{ marginTop: 12, display: "flex", gap: 8, justifyContent: "center" }}>
                    <button
                      type="button"
                      className="bc-btn bc-btn-sm"
                      onClick={openFilterDrawer}
                    >
                      Edit filters
                    </button>
                    <button
                      type="button"
                      className="bc-btn bc-btn-sm"
                      onClick={resetAllFilters}
                    >
                      Reset filters
                    </button>
                  </div>
                )}
              </div>
            )}

            {/* Pagination Footer */}
            {filteredAndSortedRows.length > 0 && (
              <div className="bc-explorer-footer">
                <div className="bc-caption mono">
                  Showing {(safePage - 1) * pageSize + 1}–
                  {Math.min(safePage * pageSize, filteredAndSortedRows.length)} of{" "}
                  {filteredAndSortedRows.length} loaded rows
                  {catalogResult &&
                    catalogResult.total_in_scope > catalogResult.rows.length &&
                    ` (${catalogResult.total_in_scope.toLocaleString()} total in scope — increase limit in Filters)`}
                </div>

                <div className="bc-pagination-controls">
                  <label className="bc-caption" style={{ display: "flex", alignItems: "center", gap: 6 }}>
                    <span>Rows / page</span>
                    <select
                      className="bc-page-size-select mono"
                      value={pageSize}
                      onChange={(e) => {
                        setPageSize(Number(e.target.value));
                        setCurrentPage(1);
                      }}
                    >
                      <option value={25}>25</option>
                      <option value={50}>50</option>
                      <option value={100}>100</option>
                    </select>
                  </label>

                  <button
                    type="button"
                    className="bc-btn bc-btn-sm"
                    disabled={safePage <= 1}
                    onClick={() => setCurrentPage((p) => Math.max(1, p - 1))}
                  >
                    Prev
                  </button>
                  <span className="bc-caption mono">
                    Page {safePage} / {totalPages}
                  </span>
                  <button
                    type="button"
                    className="bc-btn bc-btn-sm"
                    disabled={safePage >= totalPages}
                    onClick={() => setCurrentPage((p) => Math.min(totalPages, p + 1))}
                  >
                    Next
                  </button>
                </div>
              </div>
            )}
          </div>
        )}
      </div>
    </div>

      {/* Bottom Docked Selection Action Bar */}
      {selectedRecords.length > 0 && activeCatalog && (
        <div className="bc-selection-bar-dock" data-testid="catalog-selection-bar">
          <div className="bc-selection-bar">
            <span className="bc-selection-count mono">{selectedRecords.length}</span>
            <span style={{ fontWeight: 500, fontSize: 12.5 }}>
              {isGenericSchema ? (selectedRecords.length === 1 ? "row selected" : "rows selected")
                : (selectedRecords.length === 1 ? "ligand selected" : "ligands selected")}
            </span>

            <button
              type="button"
              className="bc-btn bc-btn-sm"
              onClick={() => {
                setDetailRows(selectedRecords);
                setFocusedDetailRow(
                  selectedRecords.length === 1 ? selectedRecords[0]! : null,
                );
              }}
            >
              <IconEye size={13} />
              <span>Inspect</span>
            </button>

            <div style={{ flex: 1 }} />

            <button
              type="button"
              className="bc-btn bc-btn-sm"
              data-testid="selection-send-to-chat"
              onClick={() =>
                onSendToChat({
                  catalogId: activeCatalog.id,
                  catalogName: activeCatalog.name,
                  rows: selectedRecords,
                  query: catalogQuery,
                })
              }
            >
              <IconChat size={13} />
              <span>Send to Chat</span>
            </button>

            {mode === "local" &&
              canLocalExport &&
              plugins.some(
                (p) => p.installed && p.execution_target && p.execution_target !== "local",
              ) && (
                <button
                  type="button"
                  className="bc-btn bc-btn-sm"
                  data-testid="selection-send-to-cloud-module"
                  onClick={() => {
                    const cloudTarget = plugins.find(
                      (p) => p.installed && p.execution_target && p.execution_target !== "local",
                    );
                    if (!cloudTarget) return;
                    const params = deriveModuleParamsFromSelection(
                      cloudTarget,
                      selectedRecords,
                      catalogQuery,
                    );
                    onRunInModule(cloudTarget, params, selectedRecords);
                  }}
                >
                  <IconWindowOpen size={13} />
                  <span>Send to Cloud Module</span>
                </button>
              )}

            {installedPlugins.length > 0 && (
              <div className="bc-popover-anchor">
                <button
                  type="button"
                  className="bc-btn bc-btn-primary bc-btn-sm"
                  data-testid="selection-run-in-module"
                  onClick={() => {
                    if (installedPlugins.length === 1) {
                      const target = installedPlugins[0]!;
                      const params = deriveModuleParamsFromSelection(
                        target,
                        selectedRecords,
                        catalogQuery,
                      );
                      onRunInModule(target, params, selectedRecords);
                    } else {
                      setOpenMenu((prev) => (prev === "module" ? null : "module"));
                    }
                  }}
                >
                  <IconModules size={13} />
                  <span>Run in Module</span>
                </button>

                {openMenu === "module" && (
                  <div className="bc-popover-menu bc-popover-up bc-popover-right" role="menu">
                    <div className="bc-popover-section-label">
                      Pre-fill module with {selectedRecords.length} selected row(s)
                    </div>
                    {installedPlugins.map((p) => (
                      <button
                        key={p.manifest.id}
                        type="button"
                        className="bc-popover-item"
                        onClick={() => {
                          const params = deriveModuleParamsFromSelection(
                            p,
                            selectedRecords,
                            catalogQuery,
                          );
                          setOpenMenu(null);
                          onRunInModule(p, params, selectedRecords);
                        }}
                      >
                        <div>
                          <div style={{ fontWeight: 500 }}>{p.manifest.name}</div>
                          <div className="bc-caption mono">#{p.manifest.id}</div>
                        </div>
                        <IconWindowOpen size={13} />
                      </button>
                    ))}
                  </div>
                )}
              </div>
            )}

            {canLocalExport && (
              <button
                type="button"
                className="bc-btn bc-btn-sm"
                onClick={() => handleExport("csv", selectedRecords)}
              >
                <IconDownload size={13} />
                <span>CSV</span>
              </button>
            )}

            <button
              type="button"
              className="bc-icon-btn"
              title="Clear selection"
              aria-label="Clear selection"
              onClick={() => setSelectedRowIds(new Set())}
            >
              <IconClose size={14} />
            </button>
          </div>
        </div>
      )}

      {/* ==================== FILTER DRAWER ==================== */}
      {filterDrawerOpen && (
        <div
          className="bc-drawer-backdrop"
          onClick={() => setFilterDrawerOpen(false)}
        >
          <aside
            className="bc-drawer"
            data-testid="filter-drawer"
            role="dialog"
            aria-label={isGenericSchema ? "Table filters" : "Cohort Filters"}
            onClick={(e) => e.stopPropagation()}
          >
            <div className="bc-drawer-header">
              <div>
                <h2 className="bc-drawer-title">{isGenericSchema ? "Table filters" : "Cohort Filters"}</h2>
                <p className="bc-caption" style={{ margin: "2px 0 0" }}>
                  Compiled into bound DuckDB parameters against {activeCatalog?.id}
                </p>
              </div>
              <button
                type="button"
                className="bc-icon-btn"
                aria-label="Close filters"
                onClick={() => setFilterDrawerOpen(false)}
              >
                <IconClose size={15} />
              </button>
            </div>

            <form className="bc-drawer-body" onSubmit={applyFilterDrawer}>
              {/* Organs */}
              {schemaNames.has("organ") && <div className="bc-filter-section">
                <label className="bc-filter-label">Organs</label>
                <div className="bc-tag-toggle-grid">
                  {organSuggestions.map((org) => {
                    const active = draftOrgans.includes(org);
                    return (
                      <button
                        key={org}
                        type="button"
                        className="bc-tag-toggle"
                        data-active={String(active)}
                        onClick={() =>
                          setDraftOrgans((prev) =>
                            prev.includes(org)
                              ? prev.filter((x) => x !== org)
                              : [...prev, org],
                          )
                        }
                      >
                        {org}
                      </button>
                    );
                  })}
                </div>
              </div>}

              {/* Conditions */}
              {schemaNames.has("condition") && <div className="bc-filter-section">
                <label className="bc-filter-label">Sample Condition / Cohort</label>
                <div className="bc-tag-toggle-grid">
                  {conditionSuggestions.map((cond) => {
                    const active = draftConditions.includes(cond);
                    return (
                      <button
                        key={cond}
                        type="button"
                        className="bc-tag-toggle"
                        data-active={String(active)}
                        onClick={() =>
                          setDraftConditions((prev) =>
                            prev.includes(cond)
                              ? prev.filter((x) => x !== cond)
                              : [...prev, cond],
                          )
                        }
                      >
                        {cond}
                      </button>
                    );
                  })}
                </div>
              </div>}

              {/* HLA Alleles */}
              {schemaNames.has("hla_allele") && <div className="bc-filter-section">
                <label className="bc-filter-label">HLA Alleles</label>
                <div className="bc-tag-toggle-grid">
                  {alleleSuggestions.map((allele) => {
                    const active = draftAlleles.includes(allele);
                    return (
                      <button
                        key={allele}
                        type="button"
                        className="bc-tag-toggle mono"
                        data-active={String(active)}
                        onClick={() =>
                          setDraftAlleles((prev) =>
                            prev.includes(allele)
                              ? prev.filter((x) => x !== allele)
                              : [...prev, allele],
                          )
                        }
                      >
                        {allele}
                      </button>
                    );
                  })}
                </div>
                <input
                  type="text"
                  className="bc-input mono"
                  style={{ marginTop: 8, width: "100%" }}
                  placeholder="Additional alleles (comma-separated, e.g. HLA-A*24:02)"
                  value={customAlleleInput}
                  onChange={(e) => setCustomAlleleInput(e.target.value)}
                />
              </div>}

              {/* Gene Symbols */}
              {schemaNames.has("gene_symbol") && <div className="bc-filter-section">
                <label className="bc-filter-label">Source Genes</label>
                <div className="bc-tag-toggle-grid">
                  {geneSuggestions.map((gene) => {
                    const active = draftGenes.includes(gene);
                    return (
                      <button
                        key={gene}
                        type="button"
                        className="bc-tag-toggle mono"
                        data-active={String(active)}
                        onClick={() =>
                          setDraftGenes((prev) =>
                            prev.includes(gene)
                              ? prev.filter((x) => x !== gene)
                              : [...prev, gene],
                          )
                        }
                      >
                        {gene}
                      </button>
                    );
                  })}
                </div>
                <input
                  type="text"
                  className="bc-input mono"
                  style={{ marginTop: 8, width: "100%" }}
                  placeholder="Additional genes (comma-separated, e.g. BRAF, MYC)"
                  value={customGeneInput}
                  onChange={(e) => setCustomGeneInput(e.target.value)}
                />
              </div>}

              {/* Affinity & Length */}
              {(schemaNames.has("affinity_nm") || schemaNames.has("peptide_length")) && <div className="bc-filter-row-2">
                {schemaNames.has("affinity_nm") && <div className="bc-filter-section">
                  <label className="bc-filter-label" htmlFor="filter-max-ic50">
                    Max Affinity IC50 (nM)
                  </label>
                  <input
                    id="filter-max-ic50"
                    type="number"
                    min={1}
                    step="any"
                    className="bc-input mono"
                    style={{ width: "100%" }}
                    placeholder="No cutoff (e.g. 50 or 500)"
                    value={draftMaxIc50}
                    onChange={(e) => setDraftMaxIc50(e.target.value)}
                  />
                  <div className="bc-quick-presets">
                    <button
                      type="button"
                      className="bc-link-btn"
                      onClick={() => setDraftMaxIc50("50")}
                    >
                      ≤50 nM (Strong)
                    </button>
                    <button
                      type="button"
                      className="bc-link-btn"
                      onClick={() => setDraftMaxIc50("500")}
                    >
                      ≤500 nM (Weak)
                    </button>
                    <button
                      type="button"
                      className="bc-link-btn"
                      onClick={() => setDraftMaxIc50("")}
                    >
                      Any
                    </button>
                  </div>
                </div>}

                {schemaNames.has("peptide_length") && <div className="bc-filter-section">
                  <label className="bc-filter-label">Peptide Length (aa)</label>
                  <div style={{ display: "flex", gap: 6, alignItems: "center" }}>
                    <input
                      type="number"
                      min={1}
                      max={50}
                      className="bc-input mono"
                      style={{ width: "100%" }}
                      placeholder="Min (8)"
                      aria-label="Minimum peptide length"
                      value={draftMinLen}
                      onChange={(e) => setDraftMinLen(e.target.value)}
                    />
                    <span className="bc-caption">–</span>
                    <input
                      type="number"
                      min={1}
                      max={50}
                      className="bc-input mono"
                      style={{ width: "100%" }}
                      placeholder="Max (12)"
                      aria-label="Maximum peptide length"
                      value={draftMaxLen}
                      onChange={(e) => setDraftMaxLen(e.target.value)}
                    />
                  </div>
                </div>}
              </div>}

              {/* Sequence Prefix */}
              {schemaNames.has("sequence") && <div className="bc-filter-section">
                <label className="bc-filter-label" htmlFor="filter-seq-prefix">
                  Peptide Sequence Prefix
                </label>
                <input
                  id="filter-seq-prefix"
                  type="text"
                  className="bc-input mono"
                  style={{ width: "100%" }}
                  placeholder="e.g. KLF or YLL"
                  value={draftSeqPrefix}
                  onChange={(e) => setDraftSeqPrefix(e.target.value)}
                />
              </div>}

              {/* Fetch Limit */}
              <div className="bc-filter-section">
                <label className="bc-filter-label" htmlFor="filter-row-limit">
                  Max Rows Fetched from Engine
                </label>
                <select
                  id="filter-row-limit"
                  className="bc-input mono"
                  style={{ width: "100%" }}
                  value={draftLimit}
                  onChange={(e) => setDraftLimit(Number(e.target.value))}
                >
                  <option value={50}>50 rows</option>
                  <option value={100}>100 rows</option>
                  <option value={250}>250 rows</option>
                  <option value={500}>500 rows</option>
                  <option value={1000}>1,000 rows</option>
                </select>
              </div>

              <div className="bc-drawer-footer">
                <button
                  type="button"
                  className="bc-btn"
                  onClick={() => {
                    setDraftOrgans([]);
                    setDraftConditions([]);
                    setDraftAlleles([]);
                    setDraftGenes([]);
                    setDraftMinLen("");
                    setDraftMaxLen("");
                    setDraftMaxIc50("");
                    setDraftSeqPrefix("");
                    setCustomAlleleInput("");
                    setCustomGeneInput("");
                  }}
                >
                  <IconTrash size={13} />
                  <span>Clear draft</span>
                </button>
                <div style={{ flex: 1 }} />
                <button
                  type="button"
                  className="bc-btn"
                  onClick={() => setFilterDrawerOpen(false)}
                >
                  Cancel
                </button>
                <button
                  type="submit"
                  className="bc-btn bc-btn-primary"
                  data-testid="apply-filters-btn"
                >
                  Apply Filters
                </button>
              </div>
            </form>
          </aside>
        </div>
      )}

      {/* ==================== CATALOG PROVENANCE & SCHEMA DRAWER ==================== */}
      {provenanceCatalog && (
        <div
          className="bc-drawer-backdrop"
          onClick={() => setProvenanceCatalogId(null)}
        >
          <aside
            className="bc-drawer bc-drawer-wide"
            data-testid="catalog-provenance-drawer"
            role="dialog"
            aria-label="Catalog Provenance and Governance"
            onClick={(e) => e.stopPropagation()}
          >
            <div className="bc-drawer-header">
              <div>
                <h2 className="bc-drawer-title">{provenanceCatalog.name}</h2>
                <p className="bc-caption mono" style={{ margin: "2px 0 0" }}>
                  {provenanceCatalog.id} · v{provenanceCatalog.version}
                </p>
              </div>
              <button
                type="button"
                className="bc-icon-btn"
                aria-label="Close provenance drawer"
                onClick={() => setProvenanceCatalogId(null)}
              >
                <IconClose size={15} />
              </button>
            </div>

            <div className="bc-drawer-body">
              <CatalogStorage catalog={provenanceCatalog} expanded />
              {provenanceDescription && (
                <p style={{ margin: "0 0 14px", fontSize: 13, lineHeight: 1.5, color: "var(--bc-text-secondary)" }}>
                  {provenanceDescription}
                </p>
              )}

              {/* Summary Metadata Grid */}
              <div className="bc-detail-kv-grid">
                <div className="bc-detail-kv">
                  <span className="bc-detail-k">Organism</span>
                  <span className="bc-detail-v">{provenanceCatalog.organism ?? "—"}</span>
                </div>
                <div className="bc-detail-kv">
                  <span className="bc-detail-k">Total Rows</span>
                  <span className="bc-detail-v mono">
                    {provenanceCatalog.row_count?.toLocaleString() ?? "—"}
                  </span>
                </div>
                <div className="bc-detail-kv">
                  <span className="bc-detail-k">Declared Columns</span>
                  <span className="bc-detail-v mono">
                    {provenanceCatalog.tables?.reduce((acc, tbl) => acc + tbl.columns.length, 0) ?? 0}
                  </span>
                </div>
                <div className="bc-detail-kv">
                  <span className="bc-detail-k">Sealed Snapshot</span>
                  <span className="bc-detail-v mono" style={{ fontSize: 11.5 }}>
                    {computeDatasetDigestPreview(provenanceCatalog) ?? "Verified"}
                  </span>
                </div>
              </div>

              {privateRelease && provenanceCatalogId === provenanceCatalog.id
                && releaseVersion != null && releaseVersion > 0 && (
                <HostedReleaseChanges
                  key={`${uploadScope ?? "local"}/${provenanceCatalog.id}`}
                  catalogId={provenanceCatalog.id}
                  version={releaseVersion}
                  local={localRelease}
                  onPublished={onRefreshCatalogs}
                  onOpenCatalog={(id) => {
                    onSelectPreviewCatalog(id);
                    setProvenanceCatalogId(null);
                  }}
                />
              )}

              {/* Active Dataset Grant Policy */}
              {activeGrant && (
                <div className="bc-provenance-section">
                  <h3 className="bc-provenance-heading">Active Access Grant Policy</h3>
                  <div className="bc-detail-kv-grid">
                    <div className="bc-detail-kv">
                      <span className="bc-detail-k">Grant</span>
                      <span className="bc-detail-v">
                        {activeGrant.label}{" "}
                        <span className="mono bc-caption">({activeGrant.grant_id})</span>
                      </span>
                    </div>
                    <div className="bc-detail-kv">
                      <span className="bc-detail-k">Access Level</span>
                      <span className="bc-detail-v mono">{activeGrant.level}</span>
                    </div>
                    <div className="bc-detail-kv">
                      <span className="bc-detail-k">Max Rows / Query</span>
                      <span className="bc-detail-v mono">
                        {activeGrant.max_rows_per_query.toLocaleString()} rows
                      </span>
                    </div>
                    <div className="bc-detail-kv">
                      <span className="bc-detail-k">Daily Row Budget</span>
                      <span className="bc-detail-v mono">
                        {activeGrant.daily_row_budget != null
                          ? `${activeGrant.daily_row_budget.toLocaleString()} rows`
                          : "Unlimited"}
                      </span>
                    </div>
                    <div className="bc-detail-kv">
                      <span className="bc-detail-k">Local Export</span>
                      <span className="bc-detail-v">
                        {activeGrant.allow_local_export ? "Allowed" : "Disabled"}
                      </span>
                    </div>
                    <div className="bc-detail-kv">
                      <span className="bc-detail-k">Row Scope</span>
                      <span className="bc-detail-v mono" style={{ fontSize: 11.5 }}>
                        {activeGrant.row_scope ? JSON.stringify(activeGrant.row_scope) : "Unrestricted"}
                      </span>
                    </div>
                  </div>
                </div>
              )}

              {/* Sealed Snapshots */}
              {provenanceCatalog.snapshots && provenanceCatalog.snapshots.length > 0 && (
                <div className="bc-provenance-section">
                  <h3 className="bc-provenance-heading">
                    Sealed DuckLake Snapshots ({provenanceCatalog.snapshots.length})
                  </h3>
                  {snapshotActionError && (
                    <div
                      className="bc-inline-alert bc-inline-alert-error"
                      style={{ marginBottom: 8 }}
                    >
                      <span>{snapshotActionError}</span>
                    </div>
                  )}
                  <div className="bc-shard-list">
                    {provenanceCatalog.snapshots.map((snap) => {
                      const isLatest =
                        snap.snapshot_id === provenanceCatalog.latest_snapshot_id;
                      return (
                        <div key={snap.snapshot_id} className="bc-shard-card">
                          <div
                            style={{
                              display: "flex",
                              justifyContent: "space-between",
                              alignItems: "center",
                              gap: 8,
                            }}
                          >
                            <span className="mono" style={{ fontWeight: 600, fontSize: 12 }}>
                              snapshot #{snap.snapshot_id}
                              {isLatest ? " · head" : ""}
                              {snap.pinned ? " · pinned" : ""}
                            </span>
                            <span className="bc-caption mono">
                              {snap.row_count.toLocaleString()} rows
                            </span>
                          </div>
                          <div className="bc-shard-digest-row">
                            <span className="mono bc-caption" style={{ wordBreak: "break-all" }}>
                              sha256:{snap.dataset_digest}
                            </span>
                          </div>
                          <div
                            style={{
                              display: "flex",
                              gap: 6,
                              marginTop: 8,
                              flexWrap: "wrap",
                            }}
                          >
                            {!privateRelease && (
                              <>
                                <button
                                  type="button"
                                  className="bc-btn bc-btn-sm"
                                  data-testid={`pin-snapshot-btn-${snap.snapshot_id}`}
                                  disabled={snapshotActionBusy}
                                  onClick={async () => {
                                    setSnapshotActionBusy(true);
                                    setSnapshotActionError(null);
                                    try {
                                      await pinCatalogSnapshot(
                                        provenanceCatalog.id,
                                        snap.snapshot_id,
                                        !snap.pinned,
                                      );
                                      await onRefreshCatalogs?.();
                                    } catch (err: unknown) {
                                      setSnapshotActionError(formatErrorInline(err));
                                    } finally {
                                      setSnapshotActionBusy(false);
                                    }
                                  }}
                                >
                                  {snap.pinned ? "Unpin" : "Pin Snapshot"}
                                </button>
                                {!isLatest && (
                                  <button
                                    type="button"
                                    className="bc-btn bc-btn-sm"
                                    data-testid={`rollback-snapshot-btn-${snap.snapshot_id}`}
                                    disabled={snapshotActionBusy}
                                    onClick={async () => {
                                      setSnapshotActionBusy(true);
                                      setSnapshotActionError(null);
                                      try {
                                        await rollbackCatalogSnapshot(
                                          provenanceCatalog.id,
                                          snap.snapshot_id,
                                        );
                                        await onRefreshCatalogs?.();
                                      } catch (err: unknown) {
                                        setSnapshotActionError(formatErrorInline(err));
                                      } finally {
                                        setSnapshotActionBusy(false);
                                      }
                                    }}
                                  >
                                    Rollback Head
                                  </button>
                                )}
                              </>
                            )}
                            {provenanceCatalog.id === activeCatalog?.id && (
                              <button
                                type="button"
                                className="bc-btn bc-btn-sm"
                                data-testid={`timetravel-snapshot-btn-${snap.snapshot_id}`}
                                onClick={() => {
                                  onUpdateCatalogQuery({
                                    ...catalogQuery,
                                    snapshot_id: snap.snapshot_id,
                                  });
                                  setProvenanceCatalogId(null);
                                }}
                              >
                                Time-Travel View
                              </button>
                            )}
                          </div>
                        </div>
                      );
                    })}
                  </div>
                </div>
              )}

              {/* Sealed Parquet Shards */}
              {(() => {
                const shards: CatalogShard[] = provenanceCatalog.shards ?? [];
                if (shards.length === 0) return null;
                return (
                  <div className="bc-provenance-section">
                    <h3 className="bc-provenance-heading">
                      Sealed Parquet Shards ({shards.length})
                    </h3>
                    <div className="bc-shard-list">
                      {shards.map((sh) => (
                        <div key={sh.relative_path} className="bc-shard-card">
                          <div style={{ display: "flex", justifyContent: "space-between", gap: 8 }}>
                            <span className="mono" style={{ fontWeight: 600, fontSize: 12 }}>
                              {sh.relative_path}
                            </span>
                            <span className="bc-caption mono">
                              {sh.row_count.toLocaleString()} rows
                              {sh.size_bytes != null ? ` · ${formatBytes(sh.size_bytes)}` : ""}
                            </span>
                          </div>
                          <div className="bc-shard-digest-row">
                            <span className="mono bc-caption" style={{ wordBreak: "break-all" }}>
                              sha256:{sh.sha256}
                            </span>
                            <button
                              type="button"
                              className="bc-icon-btn"
                              style={{ width: 24, height: 24 }}
                              title="Copy SHA-256 digest"
                              onClick={() => copyText(`shard:${sh.relative_path}`, sh.sha256)}
                            >
                              {copiedToken === `shard:${sh.relative_path}` ? (
                                <IconCheck size={12} />
                              ) : (
                                <IconCopy size={12} />
                              )}
                            </button>
                          </div>
                        </div>
                      ))}
                    </div>
                  </div>
                );
              })()}

              {/* Schema Columns */}
              {provenanceCatalog.tables && provenanceCatalog.tables.length > 0 && (
                <div className="bc-provenance-section">
                  <h3 className="bc-provenance-heading">Arrow Table Schema</h3>
                  {provenanceCatalog.tables.map((tbl) => (
                    <div key={tbl.name} style={{ marginBottom: 12 }}>
                      <div className="bc-caption mono" style={{ marginBottom: 4 }}>
                        <strong>{tbl.name}</strong> ({tbl.row_count.toLocaleString()} rows · PK:{" "}
                        {tbl.primary_key.join(", ") || "—"})
                      </div>
                      <div className="table-scroll">
                        <table className="data-table" data-density="compact">
                          <thead>
                            <tr>
                              <th>Column</th>
                              <th>Arrow Type</th>
                              <th>Nullability</th>
                            </tr>
                          </thead>
                          <tbody>
                            {tbl.columns.map((c) => (
                              <tr key={c.name}>
                                <td className="mono">{c.name}</td>
                                <td className="mono bc-caption">{c.data_type}</td>
                                <td>
                                  <span className="bc-caption">
                                    {c.nullable ? "Nullable" : "Required"}
                                  </span>
                                </td>
                              </tr>
                            ))}
                          </tbody>
                        </table>
                      </div>
                    </div>
                  ))}
                </div>
              )}

              {/* Effective Typed QueryAst */}
              {activeGrant && (
                <div className="bc-provenance-section">
                  <details className="bc-details" open>
                    <summary className="bc-provenance-heading" style={{ cursor: "pointer" }}>
                      Effective Typed QueryAst &amp; Grant Scope
                    </summary>
                    <div style={{ position: "relative", marginTop: 8 }}>
                      <pre className="bc-code-pre" data-testid="catalog-query-ast">
                        {JSON.stringify(
                          {
                            grant_level: activeGrant.level,
                            row_scope: activeGrant.row_scope ?? null,
                            query: toQueryAst(catalogQuery),
                          },
                          null,
                          2,
                        )}
                      </pre>
                    </div>
                  </details>
                </div>
              )}
            </div>
          </aside>
        </div>
      )}

      {/* ==================== ROW DETAIL MODAL ==================== */}
      {detailRows && (
        <div
          className="bc-modal-backdrop"
          onClick={() => {
            setDetailRows(null);
            setFocusedDetailRow(null);
          }}
        >
          <div
            className="bc-modal bc-row-detail-modal"
            data-testid="row-detail-modal"
            role="dialog"
            aria-label={isGenericSchema ? "Record details" : "Ligand Record Details"}
            onClick={(e) => e.stopPropagation()}
          >
            <div className="bc-drawer-header">
              <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
                {detailRows.length > 1 && activeInspectionRecord && (
                  <button
                    type="button"
                    className="bc-btn bc-btn-sm"
                    onClick={() => setFocusedDetailRow(null)}
                  >
                    ← Back ({detailRows.length})
                  </button>
                )}
                <div>
                  <h2 className="bc-drawer-title">
                    {isGenericSchema ? (detailRows.length === 1 ? "Record details" : `${detailRows.length} selected rows`) : activeInspectionRecord
                      ? `Ligand ${activeInspectionRecord.peptide_id}`
                      : `${detailRows.length} Selected Ligands`}
                  </h2>
                  <p className="bc-caption mono" style={{ margin: "2px 0 0" }}>
                    {activeCatalog?.id}
                  </p>
                </div>
              </div>
              <button
                type="button"
                className="bc-icon-btn"
                aria-label="Close record details"
                onClick={() => {
                  setDetailRows(null);
                  setFocusedDetailRow(null);
                }}
              >
                <IconClose size={15} />
              </button>
            </div>

            <div className="bc-drawer-body">
              {isGenericSchema ? (
                <div className="bc-table-scroll">
                  <table className="bc-explorer-table">
                    <thead><tr>{tableColumns.map(col => <th key={col.key}>{col.label}</th>)}</tr></thead>
                    <tbody>{detailRows.map((row, index) => (
                      <tr key={index}>{tableColumns.map(col => {
                        const value = row[col.key];
                        return <td key={col.key}>{value == null ? "—" :
                          typeof value === "object" ? JSON.stringify(value) : String(value)}</td>;
                      })}</tr>
                    ))}</tbody>
                  </table>
                </div>
              ) : activeInspectionRecord ? (
                <>
                  <div className="bc-record-hero">
                    <div>
                      <div className="bc-caption">Amino-Acid Sequence</div>
                      <div className="bc-record-seq mono">
                        {activeInspectionRecord.sequence}
                      </div>
                    </div>
                    <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
                      <span
                        className={`bc-badge ${
                          binderTierBadge(activeInspectionRecord.affinity_nm).cls
                        }`}
                      >
                        {binderTierBadge(activeInspectionRecord.affinity_nm).label}
                      </span>
                      <button
                        type="button"
                        className="bc-btn bc-btn-sm"
                        onClick={() =>
                          copyText("seq", activeInspectionRecord.sequence)
                        }
                      >
                        {copiedToken === "seq" ? (
                          <IconCheck size={12} />
                        ) : (
                          <IconCopy size={12} />
                        )}
                        <span>{copiedToken === "seq" ? "Copied" : "Copy Seq"}</span>
                      </button>
                    </div>
                  </div>

                  <div className="bc-detail-kv-grid" style={{ marginTop: 14 }}>
                    {ALL_COLUMNS.map((col) => {
                      const spec = schemaColumnMap.get(col.key);
                      const rawVal = activeInspectionRecord[col.key];
                      const formatted =
                        col.key === "affinity_nm"
                          ? `${activeInspectionRecord.affinity_nm.toFixed(2)} nM`
                          : col.key === "eluted_rank_pct"
                            ? `${activeInspectionRecord.eluted_rank_pct.toFixed(3)} %`
                            : col.key === "ms_intensity_log2"
                              ? activeInspectionRecord.ms_intensity_log2.toFixed(2)
                              : col.key === "peptide_length"
                                ? `${activeInspectionRecord.peptide_length} aa`
                                : String(rawVal);
                      return (
                        <div key={col.key} className="bc-detail-kv">
                          <span className="bc-detail-k">
                            {col.label}
                            {spec ? (
                              <span className="mono" style={{ opacity: 0.6, marginLeft: 5 }}>
                                {spec.data_type}
                              </span>
                            ) : null}
                          </span>
                          <span className={`bc-detail-v ${col.mono ? "mono" : ""}`}>
                            {formatted}
                          </span>
                        </div>
                      );
                    })}
                  </div>

                  {/* External Reference Links */}
                  <div className="bc-provenance-section">
                    <h3 className="bc-provenance-heading">External Reference Lookups</h3>
                    <div style={{ display: "flex", flexWrap: "wrap", gap: 8 }}>
                      <a
                        className="bc-btn bc-btn-sm"
                        href={`https://www.uniprot.org/uniprotkb?query=${encodeURIComponent(
                          `gene:${activeInspectionRecord.gene_symbol} AND organism_id:9606`,
                        )}`}
                        target="_blank"
                        rel="noopener noreferrer"
                      >
                        <span>UniProtKB ({activeInspectionRecord.gene_symbol})</span>
                        <IconWindowOpen size={12} />
                      </a>
                      <a
                        className="bc-btn bc-btn-sm"
                        href={`https://www.iedb.org/result_v3.php?cookie_id=seq&linear_sequence=${encodeURIComponent(
                          activeInspectionRecord.sequence,
                        )}`}
                        target="_blank"
                        rel="noopener noreferrer"
                      >
                        <span>IEDB Epitope Lookup</span>
                        <IconWindowOpen size={12} />
                      </a>
                    </div>
                  </div>
                </>
              ) : (
                <div className="bc-multi-record-list">
                  <p className="bc-caption" style={{ marginTop: 0 }}>
                    Click any ligand below to drill into its full quantitative profile.
                  </p>
                  {detailRows.map((r) => (
                    <button
                      key={r.peptide_id}
                      type="button"
                      className="bc-multi-record-item"
                      onClick={() => setFocusedDetailRow(r)}
                    >
                      <span className="mono accent-seq">{r.sequence}</span>
                      <span className="mono">{r.hla_allele}</span>
                      <span>
                        <strong>{r.gene_symbol}</strong> · {r.organ} ({r.condition})
                      </span>
                      <span className="mono">{r.affinity_nm.toFixed(1)} nM</span>
                    </button>
                  ))}
                </div>
              )}
            </div>

            <div className="bc-drawer-footer">
              <button
                type="button"
                className="bc-btn"
                onClick={() => {
                  const rowsToUse = activeInspectionRecord
                    ? [activeInspectionRecord]
                    : detailRows;
                  if (!activeCatalog) return;
                  setDetailRows(null);
                  setFocusedDetailRow(null);
                  onSendToChat({
                    catalogId: activeCatalog.id,
                    catalogName: activeCatalog.name,
                    rows: rowsToUse,
                    query: catalogQuery,
                  });
                }}
              >
                <IconChat size={13} />
                <span>Send to Chat</span>
              </button>

              {installedPlugins[0] && (
                <button
                  type="button"
                  className="bc-btn bc-btn-primary"
                  onClick={() => {
                    const target = installedPlugins[0]!;
                    const rowsToUse = activeInspectionRecord
                      ? [activeInspectionRecord]
                      : detailRows;
                    const params = deriveModuleParamsFromSelection(
                      target,
                      rowsToUse,
                      catalogQuery,
                    );
                    setDetailRows(null);
                    setFocusedDetailRow(null);
                    onRunInModule(target, params, rowsToUse);
                  }}
                >
                  <IconModules size={13} />
                  <span>Open in {installedPlugins[0].manifest.name}</span>
                </button>
              )}
            </div>
          </div>
        </div>
      )}

      {/* ==================== PASTE IDENTIFIERS MODAL ==================== */}
      {pasteModalOpen && (
        <div className="bc-modal-backdrop" onClick={() => setPasteModalOpen(false)}>
          <div
            className="bc-modal"
            data-testid="paste-identifiers-modal"
            role="dialog"
            aria-label="Match Identifiers to Records"
            onClick={(e) => e.stopPropagation()}
          >
            <div className="bc-drawer-header">
              <div>
                <h2 className="bc-drawer-title">Match Identifiers</h2>
                <p className="bc-caption" style={{ margin: "2px 0 0" }}>
                  Paste peptide sequences, IDs, genes, or alleles (newline, comma, or space separated)
                </p>
              </div>
              <button
                type="button"
                className="bc-icon-btn"
                aria-label="Close identifier matcher"
                onClick={() => setPasteModalOpen(false)}
              >
                <IconClose size={15} />
              </button>
            </div>

            <div className="bc-drawer-body">
              <label className="bc-filter-label" htmlFor="paste-id-kind">
                Identifier Type
              </label>
              <select
                id="paste-id-kind"
                className="bc-input"
                style={{ width: "100%", marginBottom: 12 }}
                value={pasteKind}
                onChange={(e) => {
                  setPasteKind(e.target.value as IdentifierKind);
                  setPasteReport(null);
                }}
              >
                <option value="sequence">Peptide Sequence (exact)</option>
                <option value="peptide_id">Peptide ID (exact)</option>
                <option value="gene_symbol">Gene Symbol (exact)</option>
                <option value="hla_allele">HLA Allele (exact)</option>
              </select>

              <label className="bc-filter-label" htmlFor="paste-id-textarea">
                Identifiers
              </label>
              <textarea
                id="paste-id-textarea"
                className="bc-api-json-editor mono"
                rows={5}
                placeholder={
                  pasteKind === "sequence"
                    ? "KLFGTSGQKT\nYLLPWQGRV\nYLEPGPVTA"
                    : pasteKind === "gene_symbol"
                      ? "EGFR, KRAS, TP53"
                      : pasteKind === "hla_allele"
                        ? "HLA-A*02:01, HLA-B*07:02"
                        : "PEP_0001\nPEP_0002"
                }
                value={pasteInput}
                onChange={(e) => {
                  setPasteInput(e.target.value);
                  setPasteReport(null);
                }}
              />

              {pasteReport && (
                <div className="bc-paste-summary">
                  <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", gap: 8 }}>
                    <span>
                      <strong>{pasteReport.matchedRows.length}</strong> matching row(s) across{" "}
                      <strong>{pasteReport.matchedTermCount}</strong> identifier(s) ·{" "}
                      <strong>{pasteReport.unmatchedTerms.length}</strong> unmatched
                    </span>
                    {pasteReport.unmatchedTerms.length > 0 && (
                      <button
                        type="button"
                        className="bc-btn bc-btn-sm"
                        onClick={() =>
                          copyText("unmatched", pasteReport.unmatchedTerms.join("\n"))
                        }
                      >
                        <IconCopy size={12} />
                        <span>
                          {copiedToken === "unmatched"
                            ? "Copied"
                            : `Copy unmatched (${pasteReport.unmatchedTerms.length})`}
                        </span>
                      </button>
                    )}
                  </div>
                </div>
              )}
            </div>

            <div className="bc-drawer-footer">
              <button
                type="button"
                className="bc-btn"
                onClick={() => setPasteModalOpen(false)}
              >
                Cancel
              </button>
              <button
                type="button"
                className="bc-btn"
                disabled={!pasteInput.trim()}
                onClick={runIdentifierMatching}
              >
                Find Matches
              </button>
              <button
                type="button"
                className="bc-btn bc-btn-primary"
                disabled={!pasteReport || pasteReport.matchedRows.length === 0}
                onClick={applyPasteMatches}
              >
                Select Matched ({pasteReport?.matchedRows.length ?? 0})
              </button>
            </div>
          </div>
        </div>
      )}

      {/* ==================== SAVE VIEW MODAL ==================== */}
      {saveViewModalOpen && (
        <div className="bc-modal-backdrop" onClick={() => setSaveViewModalOpen(false)}>
          <form
            className="bc-modal"
            style={{ maxWidth: 420 }}
            data-testid="save-view-modal"
            role="dialog"
            aria-label="Save Data View"
            onClick={(e) => e.stopPropagation()}
            onSubmit={handleCreateSavedView}
          >
            <div className="bc-drawer-header">
              <div>
                <h2 className="bc-drawer-title">Save Exploration View</h2>
                <p className="bc-caption" style={{ margin: "2px 0 0" }}>
                  Saves active filters, column layout, row density, and grouping for {activeCatalog?.name}
                </p>
              </div>
              <button
                type="button"
                className="bc-icon-btn"
                aria-label="Close save view"
                onClick={() => setSaveViewModalOpen(false)}
              >
                <IconClose size={15} />
              </button>
            </div>

            <div className="bc-drawer-body">
              {saveViewError && (
                <div
                  className="bc-inline-alert bc-inline-alert-error"
                  data-testid="save-view-error"
                  style={{ marginBottom: 10 }}
                >
                  <span>{saveViewError}</span>
                </div>
              )}
              <label className="bc-filter-label" htmlFor="save-view-name">
                View Name
              </label>
              <input
                id="save-view-name"
                data-testid="save-view-name-input"
                type="text"
                className="bc-input"
                style={{ width: "100%" }}
                placeholder="e.g. Lung Strong Binders (≤50 nM)"
                autoFocus
                value={newViewName}
                onChange={(e) => setNewViewName(e.target.value)}
              />
            </div>

            <div className="bc-drawer-footer">
              <button
                type="button"
                className="bc-btn"
                onClick={() => setSaveViewModalOpen(false)}
              >
                Cancel
              </button>
              <button
                type="submit"
                className="bc-btn bc-btn-primary"
                data-testid="save-view-submit-btn"
                disabled={!newViewName.trim() || savingView}
              >
                {savingView ? "Saving…" : "Save View"}
              </button>
            </div>
          </form>
        </div>
      )}

      {pickerOpen && (
        <ImportPicker mode={mode} scope={uploadScope} onClose={() => setPickerOpen(false)}
          onTable={() => { setPickerOpen(false); setImportWizardOpen(true); }}
          onTransfer={selected => { setPickerOpen(false); setTransfer(selected); }} />
      )}
      {mode === "local" && transfer && typeof transfer === "object" && (
        <LocalDatasetImport importer={transfer} onClose={() => setTransfer(null)} onPublished={id => {
          setTransfer(null);
          void onRefreshCatalogs?.();
          onSelectPreviewCatalog(id);
        }} />
      )}
      {mode === "cloud" && transfer && uploadScope && (
        <PrivateUploadDialog
          key={typeof transfer === "string" ? transfer : `${transfer.plugin_id}@${transfer.version}`}
          importer={typeof transfer === "object" ? transfer : undefined}
          legacy={transfer === "legacy"}
          namespace={defaultNamespace}
          scope={uploadScope}
          onClose={() => setTransfer(null)}
          onPublished={(id) => {
            onSelectPreviewCatalog(id);
            void onRefreshCatalogs?.();
          }}
        />
      )}

      {importWizardOpen && (
        <CatalogImportWizard
          mode={mode}
          defaultNamespace={defaultNamespace}
          activeCatalog={mode === "local" || activeCatalog?.id.startsWith(`${defaultNamespace}/`) ? activeCatalog : null}
          onClose={() => setImportWizardOpen(false)}
          onImported={async (cat) => {
            await onRefreshCatalogs?.();
            onSelectPreviewCatalog(cat.id);
          }}
        />
      )}
    </div>
  );
}
