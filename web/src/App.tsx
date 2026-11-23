import { setPrepareCaller } from "./prepareRequest";
import {
  formatMarketplaceSyncBadge,
  requiresExternalProviderConsent,
  shouldPromptQuoteConsent,
  shouldShowMarketplaceConsentModal,
} from "./milestoneBPolicy";
import { buildStagedPluginChatDraft } from "./pluginBridgePolicy";
import { useWorkspaceReadHandle, workspaceReadStorageChanged } from "./workspaceReadSession";
import React, { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  ApiClientError,
  endInstalledWorkspaceSession,
  assignSessionProject,
  confirmRun,
  createProject,
  createSavedView,
  createSession,
  deleteProject,
  deleteSavedView,
  deleteSession,
  fetchCatalogs,
  fetchGrantsWithUsage,
  fetchHealth,
  fetchLedger,
  fetchLlmConfig,
  fetchPlugins,
  fetchOfferedRunModule,
  fetchPersonalProfile,
  type PersonalProfile,
  fetchModuleOffers,
  fetchProjects,
  fetchRun,
  fetchRuns,
  fetchSavedViews,
  fetchSessionEvents,
  fetchSessions,
  fetchWhoami,
  formatClientError,
  installPlugin,
  logoutSession,
  newExecutionId,
  openPluginInDefaultIde,
  type ExecutionBudget,
  type ParsedErrorDisplay,
  prepareRun,
  queryCatalog,
  renameProject,
  renameSession,
  scaffoldPlugin,
  streamAgentChatTurn,
  followActiveRun,
  updateLlmConfig,
  updateSessionGrant,
  verifyFrogletChain,
} from "./api";
import { ExecutionBudgetControl, readExecutionBudget } from "./components/ExecutionBudgetControl";
import { AuthGate } from "./components/AuthGate";
import { PersonalAvatar } from "./components/PersonalAccountSettings";
import { ShareResultButton, SharedResultFiles } from "./components/ResultSharing";
import { SavedRunDialog } from "./components/SavedRunDialog";
import { WorkspaceModuleVersions } from "./components/WorkspaceModuleVersions";
import { ArchiveResult } from "./components/ArchiveResult";
import { MarketplaceConsentModal } from "./components/MarketplaceConsentModal";
import { PaidRunConsent } from "./components/PaidRunConsent";
import { InstalledWorkspaceResults } from "./components/InstalledWorkspaceResults";
import { WorkspaceModuleDownloads } from "./components/WorkspaceModuleDownloads";
import { BrandWordmark } from "./components/BrandWordmark";
import { ChatMarkdown } from "./components/ChatMarkdown";
import { ControlTowerView } from "./components/ControlTowerView";
import { DataExplorerView } from "./components/DataExplorerView";
import {
  FloatingWindowsLayer,
  ModuleFloatingWindow,
  type ModuleWindowState,
  type WindowTab,
} from "./components/FloatingWindows";
import {
  CompassMark,
  HeroCompass,
  IconAlitheaCloud,
  IconChat,
  IconCheckMark,
  IconChevronRight,
  IconClose,
  IconDatabase,
  IconDownload,
  IconEffort,
  IconFolder,
  IconFrogletNode,
  IconLaptop,
  IconMode,
  IconModules,
  IconPencil,
  IconPlus,
  IconResults,
  IconSend,
  IconSettings,
  IconShield,
  IconSidebar,
  IconSplitView,
  IconTrash,
  IconWindowOpen,
  ProviderLogo,
  RunningCompass,
} from "./components/Icons";
import { CapacityRequest } from "./components/ModuleTabs";
import { OrgAdminView } from "./components/OrgAdminView";
import { SettingsModal, type SettingsSection } from "./components/SettingsModal";
import {
  formatMilliAct,
  runStatusLabel,
  hasMinOrgRole,
  type AuthPrincipal,
  type CatalogManifest,
  type CatalogQuery,
  type CatalogQueryResult,
  type DatasetGrant,
  type RunVerification,
  type GrantUsageSummary,
  type HealthStatus,
  type LedgerState,
  type LlmConfigResponse,
  type MarketplaceStatus,
  type PeptideRecord,
  type PluginSummary,
  type ModuleOffer,
  type ProjectRecord,
  type ProviderControlSpec,
  type RunQuote,
  type RunRecord,
  type SavedViewRecord,
  type ScaffoldResult,
  type SessionEvent,
  type SessionRecord,
} from "./types";

type WorkspacePage =
  | "workbench"
  | "plugins"
  | "results"
  | "catalogs"
  | "org-admin"
  | "control-tower";
type ModuleRuntimeFilter = "all" | "local" | "alithea_cloud" | "froglet_node";

function grantCoversCatalog(grant: DatasetGrant, catalog: CatalogManifest | undefined): boolean {
  if (!grant.catalog_id || grant.catalog_id === "*/*" || grant.catalog_id === "*") {
    return true;
  }
  return grant.catalog_id === catalog?.id || Boolean(catalog?.offer_grant_ids?.includes(grant.grant_id));
}

function sanitizeQueryForCatalog(
  query: CatalogQuery,
  catalog: CatalogManifest | undefined,
): CatalogQuery {
  const table = catalog?.tables?.find(item => item.name === query.table) ?? catalog?.tables?.[0];
  const cols = table?.columns;
  if (!cols || cols.length === 0) return query;
  const colSet = new Set(cols.map((c) => c.name));
  return {
    ...(query.table && table ? { table: table.name } : {}),
    ...(colSet.has("organ") && query.organs ? { organs: query.organs } : {}),
    ...(colSet.has("condition") && query.conditions ? { conditions: query.conditions } : {}),
    ...(colSet.has("hla_allele") && query.hla_alleles
      ? { hla_alleles: query.hla_alleles }
      : {}),
    ...(colSet.has("gene_symbol") && query.gene_symbols
      ? { gene_symbols: query.gene_symbols }
      : {}),
    ...(colSet.has("peptide_length") && query.min_length != null
      ? { min_length: query.min_length }
      : {}),
    ...(colSet.has("peptide_length") && query.max_length != null
      ? { max_length: query.max_length }
      : {}),
    ...(colSet.has("affinity_nm") && query.max_affinity_nm != null
      ? { max_affinity_nm: query.max_affinity_nm }
      : {}),
    ...(colSet.has("sequence") && query.sequence_prefix
      ? { sequence_prefix: query.sequence_prefix }
      : {}),
    ...(query.limit != null ? { limit: query.limit } : {}),
    ...(query.snapshot_id != null ? { snapshot_id: query.snapshot_id } : {}),
  };
}

function defaultParamsForPlugin(plugin: PluginSummary): Record<string, unknown> {
  const props =
    (plugin.manifest.inputs.schema["properties"] as
      | Record<string, { default?: unknown }>
      | undefined) ?? {};
  const out: Record<string, unknown> = {};
  for (const [key, spec] of Object.entries(props)) {
    if (spec && "default" in spec && spec.default !== undefined) {
      out[key] = spec.default;
    }
  }
  return out;
}

const SUGGESTED_PROMPTS = [
  "Summarize strong HLA-A*02:01 binders across lung cohorts",
  "Compare motif length distributions in tumor vs healthy tissue",
  "Which modules are available in this workspace?",
];

function extractImagesFromContext(
  value: unknown,
  keyHint = "",
  out: { src: string; alt: string }[] = [],
): { src: string; alt: string }[] {
  if (out.length >= 6) return out;
  if (typeof value === "string") {
    const trimmed = value.trim();
    if (trimmed.startsWith("data:image/")) {
      out.push({ src: trimmed, alt: keyHint || "Module visual output" });
    } else if (
      (keyHint === "image/png" ||
        keyHint === "image/jpeg" ||
        keyHint === "image/webp" ||
        keyHint === "image/svg+xml") &&
      trimmed.length > 64 &&
      !trimmed.includes("[")
    ) {
      out.push({
        src: `data:${keyHint};base64,${trimmed}`,
        alt: "Notebook plot output",
      });
    }
    return out;
  }
  if (Array.isArray(value)) {
    for (const item of value) {
      extractImagesFromContext(item, keyHint, out);
      if (out.length >= 6) break;
    }
    return out;
  }
  if (value && typeof value === "object") {
    for (const [k, v] of Object.entries(value as Record<string, unknown>)) {
      extractImagesFromContext(v, k, out);
      if (out.length >= 6) break;
    }
  }
  return out;
}

function redactValueForAgent(value: unknown, keyHint = ""): unknown {
  if (typeof value === "string") {
    const isImageKey =
      keyHint === "image/png" ||
      keyHint === "image/jpeg" ||
      keyHint === "image/webp" ||
      keyHint === "image/svg+xml";
    if ((isImageKey || value.startsWith("data:image/")) && value.length > 512) {
      return `[${keyHint || "image"} base64 omitted (${value.length} chars)]`;
    }
    return value;
  }
  if (Array.isArray(value)) {
    return value.map((item) => redactValueForAgent(item));
  }
  if (value && typeof value === "object") {
    const out: Record<string, unknown> = {};
    for (const [k, v] of Object.entries(value as Record<string, unknown>)) {
      out[k] = redactValueForAgent(v, k);
    }
    return out;
  }
  return value;
}

function ThinkingBlock({
  thinking,
  streaming = false,
}: {
  thinking: string;
  streaming?: boolean;
}) {
  const [expanded, setExpanded] = useState<boolean>(streaming);
  useEffect(() => {
    if (streaming) setExpanded(true);
  }, [streaming]);
  const clean = thinking.trim();
  if (!clean) return null;
  return (
    <div
      className="bc-thinking-block"
      data-streaming={String(streaming)}
      data-testid="thinking-block"
    >
      <button
        type="button"
        className="bc-thinking-toggle"
        aria-expanded={expanded}
        onClick={() => setExpanded((prev) => !prev)}
      >
        <IconEffort size={13} />
        <span>{streaming ? "Thinking…" : "Thought process"}</span>
        <IconChevronRight
          size={12}
          className={`bc-thinking-chevron ${expanded ? "is-open" : ""}`}
        />
      </button>
      {expanded && <div className="bc-thinking-content">{clean}</div>}
    </div>
  );
}

export function App() {
  const [activePage, setActivePage] = useState<WorkspacePage>("workbench");
  const [sidebarCollapsed, setSidebarCollapsed] = useState<boolean>(false);
  const [dockWidthPct, setDockWidthPct] = useState<number>(54);
  const splitContainerRef = useRef<HTMLDivElement | null>(null);
  const splitResizingRef = useRef<boolean>(false);
  const [heroHovering, setHeroHovering] = useState<boolean>(false);
  const [moduleSearch, setModuleSearch] = useState<string>("");
  const [moduleRuntimeFilter, setModuleRuntimeFilter] = useState<ModuleRuntimeFilter>("all");
  const [resultsSearch, setResultsSearch] = useState<string>("");
  const [workspaceBearer, setWorkspaceBearer] = useState<string>();
  const workspaceRead = useWorkspaceReadHandle();
  useEffect(() => {
    const accountChanged = (event: StorageEvent) => {
      if (workspaceReadStorageChanged(event)) setWorkspaceBearer(undefined);
    };
    window.addEventListener("storage", accountChanged);
    return () => window.removeEventListener("storage", accountChanged);
  }, []);
  const [archivedRunId, setArchivedRunId] = useState<string | null>(null);
  const [sharedFilesScope, setSharedFilesScope] = useState<string | null>(null);
  const [moduleUpdate, setModuleUpdate] = useState("");
  const [installingPluginId, setInstallingPluginId] = useState<string | null>(null);

  const [darkTheme, setDarkTheme] = useState<boolean>(() => {
    try {
      return window.localStorage.getItem("biocompass-theme") === "dark";
    } catch {
      return false;
    }
  });

  // Settings modal state
  const [settingsOpen, setSettingsOpen] = useState<boolean>(false);
  const [settingsSection, setSettingsSection] = useState<SettingsSection>("general");
  const [scaffoldOutput, setScaffoldOutput] = useState<ScaffoldResult | null>(null);

  // Core data & node state
  const [health, setHealth] = useState<HealthStatus | null>(null);
  const [principal, setPrincipal] = useState<AuthPrincipal | null>(null);
  const profileUserId = principal?.auth_method === "session" ? principal.user_id : undefined;
  const [profile, setProfile] = useState<PersonalProfile>();
  const myProfile = profile?.user_id === profileUserId ? profile : undefined;
  useEffect(() => {
    setProfile(undefined);
    if (!profileUserId) return;
    let active = true;
    void fetchPersonalProfile().then((current) => {
      if (active && current.user_id === profileUserId) setProfile(current);
    }).catch(() => {
      // The profile editor reports load errors and supports retrying by reopening it.
    });
    return () => { active = false; };
  }, [profileUserId]);

  const [authRequired, setAuthRequired] = useState<boolean>(false);
  const [llmConfig, setLlmConfig] = useState<LlmConfigResponse | null>(null);
  const [savingComposerLlm, setSavingComposerLlm] = useState<boolean>(false);
  const [grants, setGrants] = useState<DatasetGrant[]>([]);
  const [grantUsage, setGrantUsage] = useState<GrantUsageSummary[]>([]);
  const [activeGrantId, setActiveGrantId] = useState<string>("");
  const [catalogs, setCatalogs] = useState<CatalogManifest[]>([]);
  const [savedViews, setSavedViews] = useState<SavedViewRecord[]>([]);
  const [selectedCatalogIds, setSelectedCatalogIds] = useState<string[]>(() => {
    try {
      const raw = localStorage.getItem("biocompass.selected_catalogs");
      if (raw !== null) {
        const parsed = JSON.parse(raw);
        if (Array.isArray(parsed)) {
          return parsed.filter((item): item is string => typeof item === "string");
        }
      }
    } catch {
      // ignore storage errors
    }
    return [];
  });
  const [previewCatalogId, setPreviewCatalogId] = useState<string>("");
  const [catalogResult, setCatalogResult] = useState<CatalogQueryResult | null>(null);
  const [catalogQuery, setCatalogQuery] = useState<CatalogQuery>({
    max_affinity_nm: 500,
    limit: 250,
  });
  const [catalogLoading, setCatalogLoading] = useState<boolean>(false);

  const toggleCatalogSelection = (catalogId: string) => {
    setSelectedCatalogIds((prev) => {
      const next = prev.includes(catalogId)
        ? prev.filter((id) => id !== catalogId)
        : [...prev, catalogId];
      try {
        localStorage.setItem("biocompass.selected_catalogs", JSON.stringify(next));
      } catch {
        // ignore storage errors
      }
      return next;
    });
  };
  const [plugins, setPlugins] = useState<PluginSummary[]>([]);
  const [ledger, setLedger] = useState<LedgerState | null>(null);

  // Runs & verification state
  const [runs, setRuns] = useState<RunRecord[]>([]);
  const [paidQuotes, setPaidQuotes] = useState<Array<{ quote: RunQuote; params: Record<string, unknown> | null; error?: string }>>([]);
  const pendingQuote = paidQuotes[0]?.quote ?? null;
  const pendingQuoteParams = paidQuotes[0]?.params ?? null;
  const declinedRunIdsRef = useRef(new Set<string>());
  const approvalScope = principal ? JSON.stringify([principal.org_id, principal.actor]) : null;
  const approvalSession = useRef(0);
  const sessionGeneration = approvalSession.current;
  const currentApprovalScope = useRef(approvalScope);
  currentApprovalScope.current = approvalScope;
  const [draftParamsByPlugin, setDraftParamsByPlugin] = useState<
    Record<string, Record<string, unknown>>
  >({});
  const [liveUiStateByPlugin, setLiveUiStateByPlugin] = useState<
    Record<string, Record<string, unknown>>
  >({});
  const [isConfirmingRun, setIsConfirmingRun] = useState<boolean>(false);
  const [liveLogs, setLiveLogs] = useState<string[]>([]);
  const [verifyReport, setVerifyReport] = useState<RunVerification | null>(null);
  const [errorBanner, setErrorBanner] = useState<ParsedErrorDisplay | null>(null);
  const showErrorBanner = useCallback((err: unknown) => {
    setErrorBanner(formatClientError(err));
  }, []);

  const updateRun = useCallback((updated: RunRecord) => {
    setRuns((current) => current.map((r) => r.run_id === updated.run_id ? updated : r));
  }, []);

  // Resume runs submitted on another page or held for capacity, under the current session.
  const confirmingRunId = useRef<string | null>(null);
  const followedRuns = useRef(new Map<string, AbortController>());
  useEffect(() => {
    const subscriptions = followedRuns.current;
    return () => {
      for (const controller of subscriptions.values()) controller.abort();
      subscriptions.clear();
    };
  }, [approvalScope]);
  useEffect(() => {
    const active = runs.filter((r) => r.run_id !== confirmingRunId.current &&
      (r.status === "running" || r.admission?.state === "at_capacity"));
    const ids = new Set(active.map((r) => r.run_id));
    for (const [id, controller] of followedRuns.current) {
      if (!ids.has(id)) { controller.abort(); followedRuns.current.delete(id); }
    }
    if (!approvalScope) return;
    for (const run of active) {
      if (followedRuns.current.has(run.run_id)) continue;
      const controller = new AbortController();
      followedRuns.current.set(run.run_id, controller);
      void followActiveRun(run, controller.signal, (updated) => {
        if (controller.signal.aborted) return;
        updateRun(updated);
        if (["succeeded", "failed", "canceled"].includes(updated.status)) {
          void fetchLedger().then((state) => {
            if (currentApprovalScope.current === approvalScope) setLedger(state);
          }).catch(() => {});
        }
      }).catch((err) => { if (!controller.signal.aborted) showErrorBanner(err); });
    }
  }, [runs, approvalScope, isConfirmingRun, updateRun, showErrorBanner]);

  // Floating module windows state (HLA-Compass window manager)
  const [windows, setWindows] = useState<ModuleWindowState[]>([]);
  const [zCounter, setZCounter] = useState<number>(10);

  // Conversational session & project state
  const [projects, setProjects] = useState<ProjectRecord[]>([]);
  const [collapsedProjects, setCollapsedProjects] = useState<Record<string, boolean>>({});
  const [creatingProject, setCreatingProject] = useState<boolean>(false);
  const [newProjectName, setNewProjectName] = useState<string>("");
  const [editingProjectId, setEditingProjectId] = useState<string | null>(null);
  const [editingProjectName, setEditingProjectName] = useState<string>("");
  const [editingSessionId, setEditingSessionId] = useState<string | null>(null);
  const [editingSessionTitle, setEditingSessionTitle] = useState<string>("");
  const [confirmDeleteSessionId, setConfirmDeleteSessionId] = useState<string | null>(null);
  const [confirmDeleteProjectId, setConfirmDeleteProjectId] = useState<string | null>(null);
  const [moveMenuSessionId, setMoveMenuSessionId] = useState<string | null>(null);
  const [sessions, setSessions] = useState<SessionRecord[]>([]);
  const [session, setSession] = useState<SessionRecord | null>(null);
  const [sessionEvents, setSessionEvents] = useState<SessionEvent[]>([]);
  const [chatInput, setChatInput] = useState<string>("");
  const [attachedModuleContext, setAttachedModuleContext] = useState<{
    pluginId: string;
    label: string;
    preview?: string;
    defaultMessage: string;
    context: Record<string, unknown>;
    images?: { src: string; alt: string }[];
  } | null>(null);
  const [userImagesByPrompt, setUserImagesByPrompt] = useState<
    Record<string, { src: string; alt: string }[]>
  >({});
  const [lightboxImage, setLightboxImage] = useState<{
    src: string;
    alt?: string;
  } | null>(null);
  const composerTextareaRef = useRef<HTMLTextAreaElement | null>(null);
  const dispatchedRunIdsRef = useRef(new Set<string>());

  useEffect(() => {
    const el = composerTextareaRef.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, 160)}px`;
  }, [chatInput, activePage, sessionEvents.length]);
  const [mentionTrigger, setMentionTrigger] = useState<{
    kind: "module" | "catalog";
    query: string;
    start: number;
    end: number;
  } | null>(null);
  const [mentionActiveIndex, setMentionActiveIndex] = useState<number>(0);
  const [streamingThinking, setStreamingThinking] = useState<string>("");
  const [streamingText, setStreamingText] = useState<string>("");
  const [pendingUserPrompt, setPendingUserPrompt] = useState<string | null>(null);
  const [liveToolNames, setLiveToolNames] = useState<string[]>([]);
  const [chatBusy, setChatBusy] = useState<boolean>(false);

  // Sync theme attribute
  useEffect(() => {
    document.body.setAttribute("data-theme", darkTheme ? "dark" : "light");
    try {
      window.localStorage.setItem("biocompass-theme", darkTheme ? "dark" : "light");
    } catch {
      // ignore storage errors
    }
  }, [darkTheme]);

  // Cmd/Ctrl + , shortcut to open Settings
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key === ",") {
        e.preventDefault();
        setSettingsSection("general");
        setSettingsOpen((prev) => !prev);
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, []);

  const isCloudMode = health?.mode === "cloud-web" || health?.mode === "cloud";
  useEffect(() => { setPrepareCaller(isCloudMode ? principal : null); }, [isCloudMode, principal]);

  const [marketplaceStatus, setMarketplaceStatus] = useState<MarketplaceStatus | null>(null);
  const [marketplaceModalDismissed, setMarketplaceModalDismissed] = useState(false);
  const effectiveMarketplaceStatus = marketplaceStatus ?? health?.marketplace_status ?? null;
  const marketplaceBadge = formatMarketplaceSyncBadge(effectiveMarketplaceStatus);
  const showMarketplaceConsentModal = shouldShowMarketplaceConsentModal(
    health,
    effectiveMarketplaceStatus,
    marketplaceModalDismissed,
  );

  const [availableUpdate, setAvailableUpdate] = useState<string>();
  useEffect(() => {
    if (health?.mode !== "local") return;
    const controller = new AbortController();
    const check = async () => {
      try {
        const response = await fetch("/api/v1/app-distribution", {
          cache: "no-store", signal: controller.signal,
        });
        const release = response.ok ? await response.json() : null;
        if (!controller.signal.aborted) {
          setAvailableUpdate(release?.state === "update" ? release.version : undefined);
        }
      } catch {
        if (!controller.signal.aborted) setAvailableUpdate(undefined);
      }
    };
    void check();
    const timer = window.setInterval(() => void check(), 24 * 60 * 60 * 1000);
    return () => { controller.abort(); window.clearInterval(timer); };
  }, [health?.mode]);

  const activePreviewCatalogId =
    previewCatalogId && catalogs.some((c) => c.id === previewCatalogId)
      ? previewCatalogId
      : (selectedCatalogIds.find((id) => catalogs.some((c) => c.id === id)) ??
        catalogs[0]?.id ??
        "alithea/hla-atlas-v2026.1");
  const defaultCatalogId = activePreviewCatalogId;
  const activePreviewCatalog = useMemo(
    () => catalogs.find((c) => c.id === defaultCatalogId),
    [catalogs, defaultCatalogId],
  );
  const effectiveCatalogQuery = useMemo(
    () => sanitizeQueryForCatalog(catalogQuery, activePreviewCatalog),
    [catalogQuery, activePreviewCatalog],
  );
  const grantsForActiveCatalog = useMemo(
    () => grants.filter(grant => grantCoversCatalog(grant, activePreviewCatalog)),
    [grants, activePreviewCatalog],
  );
  const activeGrant =
    grantsForActiveCatalog.find(grant => grant.grant_id === activeGrantId) ??
    grantsForActiveCatalog.find(grant => grant.level === "rows") ??
    grantsForActiveCatalog[0] ?? null;
  const defaultCatalogFacets = useMemo(
    () => activePreviewCatalog?.facets ?? {},
    [activePreviewCatalog],
  );
  const selectedCatalogCount = catalogs.filter((c) => selectedCatalogIds.includes(c.id)).length;

  const openOrFocusModuleWindow = useCallback(
    (plugin: PluginSummary, preferredTab?: WindowTab, requestedRunId?: string) => {
      setZCounter((prevZ) => {
        const nextZ = prevZ + 1;
        setWindows((prev) => {
          const existing = prev.find((w) => w.pluginId === plugin.manifest.id);
          if (existing) {
            return prev.map((w) =>
              w.id === existing.id
                ? {
                    ...w,
                    minimized: false,
                    z: nextZ,
                    plugin,
                    ...(w.plugin?.manifest.version !== plugin.manifest.version
                      ? { params: defaultParamsForPlugin(plugin) } : {}),
                    activeTab: preferredTab ?? w.activeTab,
                    requestedRunId,
                  }
                : w,
            );
          }

          const sidebarW = sidebarCollapsed ? 60 : 232;
          const cascade = (prev.length % 4) * 24;
          const w = Math.min(820, Math.max(540, window.innerWidth - sidebarW - 56));
          const h = Math.min(560, Math.max(410, window.innerHeight - 130));
          const baseX = Math.max(
            sidebarW + 18,
            Math.round(sidebarW + (window.innerWidth - sidebarW - w) / 2),
          );
          const baseY = 64;

          const created: ModuleWindowState = {
            id: `win-${plugin.manifest.id}`,
            pluginId: plugin.manifest.id,
            plugin,
            x: Math.min(window.innerWidth - w - 16, baseX + cascade),
            y: Math.min(window.innerHeight - h - 56, baseY + cascade),
            w,
            h,
            z: nextZ,
            minimized: false,
            maximized: false,
            docked: false,
            activeTab: preferredTab ?? (plugin.has_custom_ui ? "ui" : "run"),
            params: defaultParamsForPlugin(plugin),
            requestedRunId,
          };
          return [...prev, created];
        });
        return nextZ;
      });
    },
    [sidebarCollapsed],
  );

  const navigateToPage = useCallback((page: WorkspacePage) => {
    if (page !== "results") {
      setArchivedRunId(null);
      setSharedFilesScope(null);
    }
    setActivePage(page);
    // Minimize open floating module windows into the bottom tray when switching top-level pages
    // so the destination page (Chat, Modules grid, or Data table) is cleanly visible.
    setWindows((prev) =>
      prev.some((w) => !w.minimized)
        ? prev.map((w) =>
            page === "workbench" && w.docked
              ? w
              : { ...w, minimized: true, docked: false },
          )
        : prev,
    );
  }, []);

  const openOfferedModule = useCallback(async (offer: ModuleOffer) => {
    const plugin = await fetchOfferedRunModule(offer);
    if (currentApprovalScope.current !== approvalScope || approvalSession.current !== sessionGeneration) return;
    navigateToPage("workbench");
    openOrFocusModuleWindow(plugin, plugin.has_custom_ui ? "ui" : "run");
  }, [approvalScope, navigateToPage, openOrFocusModuleWindow, sessionGeneration]);

  const [federatedOfferLanding, setFederatedOfferLanding] = useState(false);

  useEffect(() => {
    if (!approvalScope) {
      setSharedFilesScope(null);
      return;
    }
    const url = new URL(window.location.href);
    if (url.searchParams.get("shared_files") !== "1") return;
    url.searchParams.delete("shared_files");
    window.history.replaceState(window.history.state, "", url);
    navigateToPage("results");
    setSharedFilesScope(approvalScope);
  }, [approvalScope, navigateToPage]);

  useEffect(() => {
    if (principal?.auth_method !== "federated") return;
    const url = new URL(window.location.href);
    const offerId = url.searchParams.get("module_offer");
    if (!offerId) return;
    url.searchParams.delete("module_offer");
    window.history.replaceState(window.history.state, "", url);
    void fetchModuleOffers().then((offers) => {
      const offer = offers.find((entry) => entry.offer_id === offerId && entry.action === "run");
      if (currentApprovalScope.current !== approvalScope || approvalSession.current !== sessionGeneration) return;
      if (!offer) throw new Error("This hosted module offer is unavailable.");
      if (offer.access === "granted") return openOfferedModule(offer);
      setFederatedOfferLanding(true);
      navigateToPage("org-admin");
    }).catch(showErrorBanner);
  }, [approvalScope, navigateToPage, openOfferedModule, principal?.auth_method, sessionGeneration, showErrorBanner]);

  const openSettings = (section: SettingsSection = "general") => {
    setSettingsSection(section);
    setSettingsOpen(true);
  };

  const refreshWorkspaceData = useCallback(async () => {
    try {
      const [gData, cList, pList, lState, rList, vList] = await Promise.all([
        fetchGrantsWithUsage(),
        fetchCatalogs(),
        fetchPlugins(),
        fetchLedger(),
        fetchRuns(),
        fetchSavedViews().catch(() => [] as SavedViewRecord[]),
      ]);
      setGrants(gData.grants);
      setGrantUsage(gData.grant_usage);
      setActiveGrantId((prev) =>
        prev && gData.grants.some((g) => g.grant_id === prev)
          ? prev
          : (gData.grants[0]?.grant_id ?? ""),
      );
      setCatalogs(cList);
      setPreviewCatalogId((prev) =>
        prev && cList.some((c) => c.id === prev) ? prev : (cList[0]?.id ?? ""),
      );
      setPlugins(pList);
      setLedger(lState);
      setRuns(rList);
      setSavedViews(vList);
    } catch (err: unknown) {
      showErrorBanner(err);
    }
  }, [showErrorBanner]);

  const bootstrapWorkspace = useCallback(async () => {
    const [gData, cList, pList, lState, rList, sList, projList, vList, llmCfg] =
      await Promise.all([
        fetchGrantsWithUsage(),
        fetchCatalogs(),
        fetchPlugins(),
        fetchLedger(),
        fetchRuns(),
        fetchSessions(),
        fetchProjects(),
        fetchSavedViews().catch(() => [] as SavedViewRecord[]),
        fetchLlmConfig().catch(() => null),
      ]);
    if (llmCfg) {
      setLlmConfig(llmCfg);
    }
    setGrants(gData.grants);
    setGrantUsage(gData.grant_usage);
    const initialGrant = gData.grants[0]?.grant_id ?? "";
    setActiveGrantId(initialGrant);
    setCatalogs(cList);
    setSavedViews(vList);
    const validIds = new Set(cList.map((c) => c.id));
    let initializedSelection: string[] | null = null;
    try {
      const raw = localStorage.getItem("biocompass.selected_catalogs");
      if (raw !== null) {
        const parsed = JSON.parse(raw);
        if (Array.isArray(parsed)) {
          initializedSelection = parsed.filter(
            (id): id is string => typeof id === "string" && validIds.has(id),
          );
        }
      }
    } catch {
      // ignore storage errors
    }
    if (initializedSelection === null || initializedSelection.length === 0) {
      initializedSelection = cList.map((c) => c.id);
      try {
        localStorage.setItem(
          "biocompass.selected_catalogs",
          JSON.stringify(initializedSelection),
        );
      } catch {
        // ignore storage errors
      }
    }
    setSelectedCatalogIds(initializedSelection);
    setPreviewCatalogId((prev) =>
      prev && validIds.has(prev) ? prev : (initializedSelection[0] ?? cList[0]?.id ?? ""),
    );
    setPlugins(pList);
    setLedger(lState);
    setRuns(rList);
    setProjects(projList);

    let currentSessions = sList;
    let activeSession = sList[0];
    if (!activeSession && initialGrant) {
      activeSession = await createSession(
        "Immunopeptidomics Exploration",
        initialGrant,
      ).catch(() => undefined);
      if (activeSession) {
        currentSessions = [activeSession];
      }
    }
    setSessions(currentSessions);
    if (activeSession) {
      setSession(activeSession);
      const evs = await fetchSessionEvents(activeSession.session_id);
      setSessionEvents(evs);
    } else {
      setSession(null);
      setSessionEvents([]);
    }
  }, []);

  // Initial bootstrap
  useEffect(() => {
    void (async () => {
      try {
        const h = await fetchHealth();
        setHealth(h);
        if (h.marketplace_status) {
          setMarketplaceStatus(h.marketplace_status);
        }
        const cloud = h.mode === "cloud-web" || h.mode === "cloud";
        try {
          const me = await fetchWhoami();
          setPrincipal(me);
          setAuthRequired(false);
          await bootstrapWorkspace();
        } catch (authErr: unknown) {
          if (cloud && authErr instanceof ApiClientError && authErr.status === 401) {
            setPrincipal(null);
            setAuthRequired(true);
            return;
          }
          throw authErr;
        }
      } catch (err: unknown) {
        showErrorBanner(err);
      }
    })();
  }, [bootstrapWorkspace, showErrorBanner]);

  // Only the Data Explorer consumes the scientific row preview.
  useEffect(() => {
    if (activePage !== "catalogs" || authRequired) return;
    if (!activeGrant || activeGrant.level !== "rows" || catalogs.length === 0) {
      setCatalogResult(null);
      setCatalogLoading(false);
      return;
    }
    const catId = activePreviewCatalogId;
    if (!catId) return;
    let cancelled = false;
    setCatalogResult(null);
    setCatalogLoading(true);
    void queryCatalog(catId, activeGrant.grant_id, effectiveCatalogQuery, activePreviewCatalog?.release_version)
      .then((res) => {
        if (!cancelled) {
          setCatalogResult(res);
          void fetchGrantsWithUsage()
            .then((gu) => {
              if (!cancelled) setGrantUsage(gu.grant_usage);
            })
            .catch(() => {
              // non-fatal
            });
        }
      })
      .catch((err: unknown) => {
        if (!cancelled) {
          showErrorBanner(err);
        }
      })
      .finally(() => {
        if (!cancelled) {
          setCatalogLoading(false);
        }
      });
    return () => {
      cancelled = true;
    };
  }, [
    activePage,
    authRequired,
    activeGrant,
    catalogs,
    effectiveCatalogQuery,
    activePreviewCatalogId,
    activePreviewCatalog,
    showErrorBanner,
  ]);

  const handleNewSession = async (projectId?: string): Promise<void> => {
    try {
      setErrorBanner(null);
      const created = await createSession(
        `HLA Session ${sessions.length + 1}`,
        activeGrant?.grant_id ?? "",
        projectId,
      );
      setSessions((prev) => [created, ...prev]);
      setSession(created);
      setSessionEvents([]);
      setStreamingThinking("");
      setStreamingText("");
      if (projectId) {
        setCollapsedProjects((prev) => ({ ...prev, [projectId]: false }));
      }
      navigateToPage("workbench");
    } catch (err: unknown) {
      showErrorBanner(err);
    }
  };

  const handleRenameSessionSubmit = async (sessionId: string): Promise<void> => {
    const trimmed = editingSessionTitle.trim();
    setEditingSessionId(null);
    if (!trimmed) return;
    const existing = sessions.find((s) => s.session_id === sessionId);
    if (existing && existing.title === trimmed) return;
    try {
      setErrorBanner(null);
      const updated = await renameSession(sessionId, trimmed);
      setSessions((prev) =>
        prev.map((s) => (s.session_id === sessionId ? updated : s)),
      );
      setSession((prev) => (prev?.session_id === sessionId ? updated : prev));
    } catch (err: unknown) {
      showErrorBanner(err);
    }
  };

  const handleDeleteSession = async (sessionId: string): Promise<void> => {
    setConfirmDeleteSessionId(null);
    if (moveMenuSessionId === sessionId) setMoveMenuSessionId(null);
    try {
      setErrorBanner(null);
      await deleteSession(sessionId);
      const remaining = sessions.filter((s) => s.session_id !== sessionId);
      if (session?.session_id === sessionId) {
        if (remaining.length > 0) {
          setSessions(remaining);
          const next = remaining[0]!;
          setSession(next);
          const evs = await fetchSessionEvents(next.session_id);
          setSessionEvents(evs);
        } else {
          const fresh = await createSession(
            "Immunopeptidomics Exploration",
            activeGrant?.grant_id ?? "",
          );
          setSessions([fresh]);
          setSession(fresh);
          setSessionEvents([]);
        }
        setStreamingThinking("");
        setStreamingText("");
      } else {
        setSessions(remaining);
      }
    } catch (err: unknown) {
      showErrorBanner(err);
    }
  };

  const handleAssignSessionProject = async (
    sessionId: string,
    projectId: string | null,
  ): Promise<void> => {
    setMoveMenuSessionId(null);
    try {
      setErrorBanner(null);
      const updated = await assignSessionProject(sessionId, projectId);
      setSessions((prev) =>
        prev.map((s) => (s.session_id === sessionId ? updated : s)),
      );
      setSession((prev) => (prev?.session_id === sessionId ? updated : prev));
      if (projectId) {
        setCollapsedProjects((prev) => ({ ...prev, [projectId]: false }));
      }
    } catch (err: unknown) {
      showErrorBanner(err);
    }
  };

  const handleCreateProjectSubmit = async (): Promise<void> => {
    const trimmed = newProjectName.trim();
    setCreatingProject(false);
    setNewProjectName("");
    if (!trimmed) return;
    try {
      setErrorBanner(null);
      const created = await createProject(trimmed);
      setProjects((prev) => [created, ...prev]);
    } catch (err: unknown) {
      showErrorBanner(err);
    }
  };

  const handleRenameProjectSubmit = async (projectId: string): Promise<void> => {
    const trimmed = editingProjectName.trim();
    setEditingProjectId(null);
    if (!trimmed) return;
    const existing = projects.find((p) => p.project_id === projectId);
    if (existing && existing.name === trimmed) return;
    try {
      setErrorBanner(null);
      const updated = await renameProject(projectId, trimmed);
      setProjects((prev) =>
        prev.map((p) => (p.project_id === projectId ? updated : p)),
      );
    } catch (err: unknown) {
      showErrorBanner(err);
    }
  };

  const handleDeleteProject = async (projectId: string): Promise<void> => {
    setConfirmDeleteProjectId(null);
    try {
      setErrorBanner(null);
      await deleteProject(projectId);
      setProjects((prev) => prev.filter((p) => p.project_id !== projectId));
      setSessions((prev) =>
        prev.map((s) =>
          s.project_id === projectId ? { ...s, project_id: null } : s,
        ),
      );
      setSession((prev) =>
        prev?.project_id === projectId ? { ...prev, project_id: null } : prev,
      );
    } catch (err: unknown) {
      showErrorBanner(err);
    }
  };


  const handleSelectSession = async (target: SessionRecord): Promise<void> => {
    try {
      setSession(target);
      navigateToPage("workbench");
      const evs = await fetchSessionEvents(target.session_id);
      setSessionEvents(evs);
    } catch (err: unknown) {
      showErrorBanner(err);
    }
  };


  const handleGrantChange = async (newGrantId: string): Promise<void> => {
    setActiveGrantId(newGrantId);
    if (session) {
      try {
        const updated = await updateSessionGrant(session.session_id, newGrantId);
        setSession(updated);
      } catch {
        // non-fatal
      }
    }
  };

  const recordConfirmedRun = useCallback((completed: RunRecord) => {
    setRuns((previous) => [
      completed,
      ...previous.filter((run) => run.run_id !== completed.run_id),
    ]);
    if (completed.logs.length > 0) {
      setLiveLogs(completed.logs);
    }
    // The run record is authoritative even if a secondary read is rate limited.
    void fetchLedger().then(setLedger).catch(showErrorBanner);
  }, [showErrorBanner]);

  const executePreparedQuote = useCallback(
    async (
      quoteToRun: RunQuote,
      options?: { stayOnActiveTab?: boolean },
    ): Promise<RunRecord | null> => {
      const generation = approvalSession.current;
      if (dispatchedRunIdsRef.current.has(quoteToRun.run_id)) return null;
      dispatchedRunIdsRef.current.add(quoteToRun.run_id);
      setIsConfirmingRun(true);
      setErrorBanner(null);
      setVerifyReport(null);
      setLiveLogs([
        `[host] Dispatching OCI container for ${quoteToRun.plugin_id} (${quoteToRun.run_id})...`,
      ]);

      // Switch standard parameter windows directly to Runs so live logs & charts appear seamlessly;
      // keep custom interactive App windows (or explicit API test requests) on their active tab.
      if (!options?.stayOnActiveTab) {
        setWindows((prev) =>
          prev.map((w) =>
            w.pluginId === quoteToRun.plugin_id && w.activeTab !== "ui"
              ? { ...w, minimized: false, activeTab: "results" }
              : w,
          ),
        );
      }

      confirmingRunId.current = quoteToRun.run_id;
      try {
        const completed = await confirmRun(quoteToRun.run_id, undefined, (ev) => {
          if (approvalSession.current === generation) setLiveLogs((prev) => [...prev, `[${ev.stream}] ${ev.line}`]);
        });
        if (approvalSession.current !== generation) return null;
        setPaidQuotes((current) => current.filter((item) => item.quote.run_id !== quoteToRun.run_id));
        recordConfirmedRun(completed);
        if (completed.admission) {
          setResultsSearch(completed.run_id);
          navigateToPage("results");
        } else if (completed.froglet_chain.length > 0) {
          void verifyFrogletChain(completed.froglet_chain)
            .then((report) => setVerifyReport({ ...report, runId: completed.run_id }))
            .catch(showErrorBanner);
        }
        return completed;
      } catch (err: unknown) {
        if (approvalSession.current !== generation) return null;
        dispatchedRunIdsRef.current.delete(quoteToRun.run_id);
        showErrorBanner(err);
        throw err;
      } finally {
        if (confirmingRunId.current === quoteToRun.run_id) confirmingRunId.current = null;
        if (approvalSession.current === generation) setIsConfirmingRun(false);
      }
    },
    [recordConfirmedRun, showErrorBanner],
  );

  const queuePaidQuote = useCallback((quote: RunQuote, params?: Record<string, unknown>) => {
    if (declinedRunIdsRef.current.has(quote.run_id) || dispatchedRunIdsRef.current.has(quote.run_id)) return;
    setPaidQuotes((current) => current.some((item) => item.quote.run_id === quote.run_id)
      ? current : [...current, { quote, params: params ?? null }]);
  }, []);

  const handleQuotePrepared = useCallback(
    (quote: RunQuote, params?: Record<string, unknown>, source: "module" | "agent" = "module") => {
      if (!approvalScope || currentApprovalScope.current !== approvalScope || approvalSession.current !== sessionGeneration) return;
      if (declinedRunIdsRef.current.has(quote.run_id) || dispatchedRunIdsRef.current.has(quote.run_id)) return;
      setVerifyReport(null);
      if (params) {
        const { _ui_state: _ignoredUi, ...cleanParams } = params;
        setDraftParamsByPlugin((prev) => ({ ...prev, [quote.plugin_id]: cleanParams }));
        setWindows((prev) => prev.map((w) =>
          w.pluginId === quote.plugin_id && w.plugin?.manifest.version === quote.plugin_version
            ? { ...w, params: cleanParams } : w));
      }
      if (shouldPromptQuoteConsent(quote)) {
        queuePaidQuote(quote, params);
        if (!params) {
          void fetchRun(quote.run_id).then((run) => {
            setPaidQuotes((current) => current.map((item) =>
              item.quote.run_id === run.run_id ? { ...item, params: run.params } : item));
          }).catch(() => { /* The signed quote can be reviewed without its draft. */ });
        }
        if (source === "agent") navigateToPage("workbench");
        return;
      }
      const targetPlugin = plugins.find((p) =>
        p.manifest.id === quote.plugin_id && p.manifest.version === quote.plugin_version);
      if (targetPlugin) openOrFocusModuleWindow(targetPlugin);
      void executePreparedQuote(quote).catch(() => { /* executePreparedQuote displays the error. */ });
    },
    [plugins, openOrFocusModuleWindow, executePreparedQuote, navigateToPage, queuePaidQuote, approvalScope, sessionGeneration],
  );

  const openInstalledRelease = (plugin: PluginSummary): void => {
    setPlugins((current) => [
      ...current.filter((entry) => entry.manifest.id !== plugin.manifest.id),
      plugin,
    ]);
    openOrFocusModuleWindow(plugin, plugin.has_custom_ui ? "ui" : "run");
  };

  const afterModuleRemoval = (id: string, version: string) => {
    setModuleUpdate(`Removed ${id} v${version} from this workspace.`);
    setWindows((current) => current.filter((win) =>
      win.pluginId !== id || (win.plugin ?? plugins.find((p) => p.manifest.id === id))?.manifest.version !== version,
    ));
    // Drop the exact removed release immediately, then resolve any remaining default or detached source.
    setPlugins((current) => current.filter((p) => p.manifest.id !== id || p.manifest.version !== version)
      .map((p) => p.manifest.id === id
        ? { ...p, removable_versions: p.removable_versions?.filter((v) => v !== version) }
        : p));
    void fetchPlugins().then(setPlugins).catch(showErrorBanner);
  };

  const handleInstallPlugin = async (plugin: PluginSummary): Promise<void> => {
    if (installingPluginId) return;
    setInstallingPluginId(plugin.manifest.id);
    setErrorBanner(null);
    try {
      await installPlugin(plugin.manifest.id);
      const updated = await fetchPlugins();
      setPlugins(updated);
    } catch (err: unknown) {
      showErrorBanner(err);
    } finally {
      setInstallingPluginId(null);
    }
  };

  const handlePreparePluginRun = async (
    plugin: PluginSummary,
    params?: Record<string, unknown>,
    options?: { stayOnActiveTab?: boolean; offerId?: string },
  ): Promise<RunRecord | null> => {
    if (isConfirmingRun) return null;
    try {
      setErrorBanner(null);
      openOrFocusModuleWindow(plugin);
      const effectiveParams = params ?? defaultParamsForPlugin(plugin);
      const quote = await prepareRun({
        plugin_id: plugin.manifest.id,
        plugin_version: plugin.manifest.version,
        params: effectiveParams,
        grant_id: activeGrant?.grant_id,
        release_version: activeGrant ? activePreviewCatalog?.release_version : undefined,
        session_id: session?.session_id,
        ...(options?.offerId ? { offer_id: options.offerId } : {}),
        marketplace_offer_id: plugin.marketplace_offer_id,
      });
      if (!approvalScope || currentApprovalScope.current !== approvalScope || approvalSession.current !== sessionGeneration) return null;
      if (shouldPromptQuoteConsent(quote)) {
        queuePaidQuote(quote, effectiveParams);
        return null;
      }
      return await executePreparedQuote(quote, options);
    } catch (err: unknown) {
      showErrorBanner(err);
      if (options?.stayOnActiveTab) {
        throw err;
      }
      return null;
    }
  };

  const handleQuoteConfirmedFromConsent = useCallback(
    async (completed: RunRecord) => {
      recordConfirmedRun(completed);
      if (completed.admission) {
        setResultsSearch(completed.run_id);
        navigateToPage("results");
        return;
      }
      if (completed.froglet_chain.length > 0) {
        void verifyFrogletChain(completed.froglet_chain)
          .then((report) => setVerifyReport({ ...report, runId: completed.run_id }))
          .catch(showErrorBanner);
      }
      setWindows((prev) =>
        prev.map((w) =>
          w.pluginId === completed.plugin_id && w.activeTab !== "ui"
            ? { ...w, minimized: false, activeTab: "results" }
            : w,
        ),
      );
    },
    [recordConfirmedRun, showErrorBanner],
  );

  const handleLogout = async () => {
    setArchivedRunId(null);
    if (workspaceBearer || workspaceRead) {
      void endInstalledWorkspaceSession(workspaceBearer).catch(showErrorBanner);
    }
    setWorkspaceBearer(undefined);
    approvalSession.current += 1;
    currentApprovalScope.current = null;
    setPrincipal(null);
    setAuthRequired(true);
    setIsConfirmingRun(false);
    setPaidQuotes([]);
    setRuns([]);
    setWindows([]);
    setActivePage("workbench");
    declinedRunIdsRef.current.clear();
    dispatchedRunIdsRef.current.clear();
    try {
      await logoutSession();
    } catch {
      // The local view has already discarded its signed-in state.
    }
  };

  const handleConfirmPendingRun = (displayedRunId: string): void => {
    if (pendingQuote?.run_id !== displayedRunId || isConfirmingRun ||
        declinedRunIdsRef.current.has(displayedRunId)) return;
    void executePreparedQuote(pendingQuote).catch((err: unknown) => {
      setPaidQuotes((current) => current.map((item) => item.quote.run_id === displayedRunId
        ? { ...item, error: formatClientError(err).message } : item));
    });
  };

  const declinePaidRun = (displayedRunId: string): void => {
    if (pendingQuote?.run_id !== displayedRunId || dispatchedRunIdsRef.current.has(displayedRunId)) return;
    declinedRunIdsRef.current.add(displayedRunId);
    setPaidQuotes((current) => current.filter((item) => item.quote.run_id !== displayedRunId));
  };

  const handleReverifyReceipt = async (
    run: RunRecord,
    tamperOneByte: boolean,
  ): Promise<void> => {
    if (run.froglet_chain.length === 0) return;
    const chainCopy = JSON.parse(
      JSON.stringify(run.froglet_chain),
    ) as Record<string, unknown>[];

    if (tamperOneByte) {
      const receiptDoc = chainCopy[chainCopy.length - 1];
      const payload =
        receiptDoc &&
        typeof receiptDoc["payload"] === "object" &&
        receiptDoc["payload"] !== null
          ? (receiptDoc["payload"] as Record<string, unknown>)
          : null;
      if (payload && typeof payload["result_hash"] === "string") {
        const orig = payload["result_hash"];
        payload["result_hash"] = (orig.startsWith("0") ? "f" : "0") + orig.slice(1);
      } else if (receiptDoc && typeof receiptDoc["signature"] === "string") {
        const origSig = receiptDoc["signature"];
        receiptDoc["signature"] =
          (origSig.startsWith("0") ? "f" : "0") + origSig.slice(1);
      }
    }
    const report = await verifyFrogletChain(chainCopy);
    setVerifyReport({ ...report, runId: run.run_id });
  };

  const buildModuleSnapshot = useCallback(
    (win: ModuleWindowState) => {
      const pluginMeta = win.plugin ?? plugins.find((p) => p.manifest.id === win.pluginId);
      const pluginRuns = runs.filter((r) => r.plugin_id === win.pluginId);
      const latest = pluginRuns[0] ?? null;
      const draft = draftParamsByPlugin[win.pluginId] ?? win.params;
      const liveUi = liveUiStateByPlugin[win.pluginId] ?? null;
      return {
        plugin_id: win.pluginId,
        plugin_version: pluginMeta?.manifest.version,
        plugin_name: pluginMeta?.manifest.name ?? win.pluginId,
        active_tab: win.activeTab,
        minimized: win.minimized,
        docked: win.docked,
        draft_params: draft,
        ...(liveUi ? { live_ui_state: redactValueForAgent(liveUi) } : {}),
        ...(pendingQuote?.plugin_id === win.pluginId
          ? {
              pending_quote: {
                run_id: pendingQuote.run_id,
                plugin_version: pendingQuote.plugin_version,
                estimated_cost_act: pendingQuote.estimated_cost_act,
                params: pendingQuoteParams,
              },
            }
          : {}),
        latest_run: latest
          ? {
              run_id: latest.run_id,
              plugin_version: latest.plugin_version,
              status: latest.status,
              receipt_verified: latest.receipt_verified,
              params: latest.params,
              result_json: redactValueForAgent(latest.result_json),
              logs: latest.logs.slice(-40),
              artifacts: latest.artifacts.map((a) => ({
                name: a.name,
                media_type: a.media_type,
                size_bytes: a.size_bytes,
              })),
              error_message: latest.error_message,
            }
          : null,
      };
    },
    [
      plugins,
      runs,
      draftParamsByPlugin,
      liveUiStateByPlugin,
      pendingQuote,
      pendingQuoteParams,
    ],
  );

  const buildActiveModuleContext = useCallback(
    (override?: { pluginId: string; context?: Record<string, unknown> }) => {
      if (override) {
        const win = windows.find((w) => w.pluginId === override.pluginId) ?? null;
        const base = win
          ? buildModuleSnapshot(win)
          : {
              plugin_id: override.pluginId,
              draft_params: draftParamsByPlugin[override.pluginId] ?? null,
              live_ui_state: liveUiStateByPlugin[override.pluginId]
                ? redactValueForAgent(liveUiStateByPlugin[override.pluginId])
                : null,
            };
        return {
          ...base,
          ...(redactValueForAgent(override.context) as Record<string, unknown>),
          open_modules: windows.map((w) => ({
            plugin_id: w.pluginId,
            active_tab: w.activeTab,
            minimized: w.minimized,
            docked: w.docked,
            draft_params: draftParamsByPlugin[w.pluginId] ?? w.params,
          })),
        };
      }
      if (windows.length === 0) return null;
      const openWin =
        windows.find((w) => w.docked && !w.minimized) ??
        windows
          .filter((w) => !w.minimized)
          .sort((a, b) => b.z - a.z)[0] ??
        windows.sort((a, b) => b.z - a.z)[0] ??
        null;
      if (!openWin) return null;
      return {
        ...buildModuleSnapshot(openWin),
        open_modules: windows.map((w) => buildModuleSnapshot(w)),
      };
    },
    [
      windows,
      buildModuleSnapshot,
      draftParamsByPlugin,
      liveUiStateByPlugin,
    ],
  );

  const handleApplyAgentDraftUpdate = useCallback(
    (raw: unknown) => {
      if (!raw || typeof raw !== "object") return;
      const rec = raw as {
        plugin_id?: unknown;
        params?: unknown;
        merge?: unknown;
        open_tab?: unknown;
      };
      if (typeof rec.plugin_id !== "string" || !rec.params || typeof rec.params !== "object") {
        return;
      }
      const pluginId = rec.plugin_id;
      const { _ui_state: _ignoredUi, ...patchParams } = rec.params as Record<string, unknown>;
      const merge = rec.merge !== false;
      const tab =
        rec.open_tab === "ui" || rec.open_tab === "run" || rec.open_tab === "results"
          ? rec.open_tab
          : undefined;

      setDraftParamsByPlugin((prev) => {
        const nextParams = merge
          ? { ...(prev[pluginId] ?? {}), ...patchParams }
          : patchParams;
        return { ...prev, [pluginId]: nextParams };
      });

      const targetPlugin = plugins.find((p) => p.manifest.id === pluginId);
      if (targetPlugin) {
        openOrFocusModuleWindow(targetPlugin, tab);
      }

      setWindows((prev) =>
        prev.map((w) => {
          if (w.pluginId !== pluginId) return w;
          const nextParams = merge ? { ...w.params, ...patchParams } : patchParams;
          return {
            ...w,
            params: nextParams,
            ...(tab ? { activeTab: tab } : {}),
          };
        }),
      );
    },
    [plugins, openOrFocusModuleWindow],
  );

  const submitChatPrompt = async (
    rawMsg: string,
    moduleOverride?: { pluginId: string; context?: Record<string, unknown> },
    executionBudget?: ExecutionBudget,
  ): Promise<void> => {
    const activeAttachment = !moduleOverride ? attachedModuleContext : null;
    const msg =
      rawMsg.trim() || (activeAttachment ? activeAttachment.defaultMessage : "");
    if (!session || !msg || chatBusy) return;
    setChatInput("");
    setMentionTrigger(null);
    setMentionActiveIndex(0);
    if (activeAttachment) {
      if (activeAttachment.images && activeAttachment.images.length > 0) {
        const key = `${session.session_id}:${msg}`;
        const imgs = activeAttachment.images;
        setUserImagesByPrompt((prev) => ({ ...prev, [key]: imgs }));
      }
      setAttachedModuleContext(null);
    }
    setChatBusy(true);
    setPendingUserPrompt(msg);
    setLiveToolNames([]);
    setStreamingThinking("");
    setStreamingText("");
    setErrorBanner(null);

    const effectiveOverride =
      moduleOverride ??
      (activeAttachment
        ? {
            pluginId: activeAttachment.pluginId,
            context: activeAttachment.context,
          }
        : undefined);
    const baseCtx = buildActiveModuleContext(effectiveOverride);
    const mentionedPlugins = plugins.filter((p) =>
      msg.includes(`#${p.manifest.id}`),
    );
    const mentionedCatalogs = catalogs.filter((c) => msg.includes(`@${c.id}`));
    const moduleCtx =
      mentionedPlugins.length > 0 || mentionedCatalogs.length > 0
        ? {
            ...(baseCtx ?? {}),
            ...(mentionedPlugins.length > 0
              ? {
                  mentioned_modules: mentionedPlugins.map((p) => ({
                    id: p.manifest.id,
                    name: p.manifest.name,
                    category: p.manifest.category,
                    description: p.manifest.description,
                  })),
                }
              : {}),
            ...(mentionedCatalogs.length > 0
              ? {
                  mentioned_catalogs: mentionedCatalogs.map((c) => ({
                    id: c.id,
                    name: c.name,
                    version: c.version,
                    organism: c.organism ?? null,
                    row_count: c.row_count ?? null,
                    columns:
                      c.tables?.[0]?.columns.map((col) => col.name) ?? null,
                  })),
                }
              : {}),
          }
        : baseCtx;

    try {
      await streamAgentChatTurn(
        session.session_id,
        msg,
        (ev) => {
          if (ev.type === "thought_delta" && typeof ev.data === "string") {
            setStreamingThinking((prev) => prev + ev.data);
          } else if (ev.type === "text_delta" && typeof ev.data === "string") {
            setStreamingText((prev) => prev + ev.data);
          } else if (
            ev.type === "tool_call" &&
            ev.data &&
            typeof ev.data === "object" &&
            "name" in ev.data &&
            typeof (ev.data as { name?: unknown }).name === "string"
          ) {
            const toolName = (ev.data as { name: string }).name;
            setLiveToolNames((prev) =>
              prev.includes(toolName) ? prev : [...prev, toolName],
            );
          } else if (ev.type === "module_draft_updated" && ev.data) {
            handleApplyAgentDraftUpdate(ev.data);
          } else if (ev.type === "agent_admission" && ev.data && typeof ev.data === "object") {
            const admission = ev.data as { status?: string; message?: string; run_id?: string };
            if (currentApprovalScope.current !== approvalScope || approvalSession.current !== sessionGeneration) return;
            if (admission.status === "submitted" && admission.run_id) {
              void fetchRun(admission.run_id).then((run) => {
                if (currentApprovalScope.current === approvalScope && approvalSession.current === sessionGeneration) {
                  recordConfirmedRun(run);
                }
              }).catch(showErrorBanner);
            } else if (admission.status === "insufficient_credits" && admission.message) {
              showErrorBanner(admission.message);
            }
          } else if (ev.type === "run_prepared" && ev.data) {
            handleQuotePrepared(ev.data as RunQuote, undefined, "agent");
          } else if (ev.type === "error" && typeof ev.data === "string") {
            showErrorBanner(ev.data);
          }
        },
        moduleCtx,
        isCloudMode ? { execution_id: newExecutionId(), execution_budget: executionBudget } : undefined,
      );
      const evs = await fetchSessionEvents(session.session_id);
      setSessionEvents(evs);
      setStreamingThinking("");
      setStreamingText("");
    } catch (err: unknown) {
      showErrorBanner(err);
    } finally {
      setPendingUserPrompt(null);
      setLiveToolNames([]);
      setChatBusy(false);
    }
  };

  const handleSyncDraftParams = useCallback(
    (pluginId: string, params: Record<string, unknown>) => {
      const { _ui_state, ...cleanParams } = params;
      if (_ui_state && typeof _ui_state === "object" && !Array.isArray(_ui_state)) {
        const uiRec = _ui_state as Record<string, unknown>;
        const uiSig = JSON.stringify(uiRec);
        setLiveUiStateByPlugin((prev) =>
          JSON.stringify(prev[pluginId] ?? null) === uiSig
            ? prev
            : { ...prev, [pluginId]: uiRec },
        );
      }

      const cleanSig = JSON.stringify(cleanParams);
      setDraftParamsByPlugin((prev) =>
        JSON.stringify(prev[pluginId] ?? null) === cleanSig
          ? prev
          : { ...prev, [pluginId]: cleanParams },
      );
      setWindows((prev) => {
        let changed = false;
        const next = prev.map((w) => {
          if (w.pluginId !== pluginId) return w;
          if (JSON.stringify(w.params) === cleanSig) return w;
          changed = true;
          return { ...w, params: cleanParams };
        });
        return changed ? next : prev;
      });
    },
    [],
  );

  const handleSendModuleToChat = useCallback(
    (pluginId: string, message: string, context?: Record<string, unknown>) => {
      // SEC-27: Never auto-submit plugin `sendToChat` calls; always stage the message
      // and attachment into the composer for explicit user confirmation.
      setActivePage("workbench");
      const hasPluginWindow = windows.some((w) => w.pluginId === pluginId);
      if (hasPluginWindow) {
        setWindows((prev) =>
          prev.map((w) =>
            w.pluginId === pluginId
              ? { ...w, docked: true, minimized: false }
              : w.docked
                ? { ...w, docked: false, minimized: true }
                : { ...w, minimized: true },
          ),
        );
        setSidebarCollapsed(true);
      }
      const images = context ? extractImagesFromContext(context) : [];
      const draft = buildStagedPluginChatDraft(pluginId, message, context, chatInput);
      setAttachedModuleContext({
        pluginId: draft.pluginId,
        label: draft.label,
        preview: draft.preview,
        defaultMessage: draft.defaultMessage,
        context: draft.context ?? {},
        ...(images.length > 0 ? { images } : {}),
      });
      if (draft.stagedComposerText && !chatInput.trim()) {
        setChatInput(draft.stagedComposerText);
      }
      window.setTimeout(() => {
        composerTextareaRef.current?.focus();
      }, 40);
    },
    [windows, chatInput],
  );

  const handleScaffoldModule = async (payload: {
    template: string;
    plugin_id: string;
    display_name: string;
    include_vite_ui: boolean;
  }): Promise<void> => {
    try {
      setErrorBanner(null);
      const res = await scaffoldPlugin(payload);
      setScaffoldOutput(res);
      const updatedPlugins = await fetchPlugins();
      setPlugins(updatedPlugins);
    } catch (err: unknown) {
      showErrorBanner(err);
    }
  };

  const handleOpenPluginInIde = useCallback(
    async (
      directory: string,
      options?: { ide?: string; custom_command?: string },
    ): Promise<string> => {
      const res = await openPluginInDefaultIde(directory, options);
      return res.launched;
    },
    [],
  );

  // Window manager callbacks
  const handleFocusWindow = useCallback((winId: string) => {
    setZCounter((prevZ) => {
      const nextZ = prevZ + 1;
      setWindows((prev) =>
        prev.map((w) =>
          w.id === winId ? { ...w, minimized: false, z: nextZ } : w,
        ),
      );
      return nextZ;
    });
  }, []);

  const handleMoveResizeWindow = useCallback(
    (
      winId: string,
      patch: Partial<Pick<ModuleWindowState, "x" | "y" | "w" | "h">>,
    ) => {
      setWindows((prev) =>
        prev.map((w) => (w.id === winId ? { ...w, ...patch } : w)),
      );
    },
    [],
  );

  const handleToggleMinimize = useCallback((winId: string) => {
    setZCounter((prevZ) => {
      const nextZ = prevZ + 1;
      setWindows((prev) =>
        prev.map((w) =>
          w.id === winId
            ? {
                ...w,
                minimized: !w.minimized,
                docked: w.minimized ? w.docked : false,
                z: w.minimized ? nextZ : w.z,
              }
            : w,
        ),
      );
      return nextZ;
    });
  }, []);

  const handleToggleMaximize = useCallback((winId: string) => {
    setWindows((prev) =>
      prev.map((w) =>
        w.id === winId
          ? { ...w, maximized: !w.maximized, minimized: false, docked: false }
          : w,
      ),
    );
  }, []);

  const handleToggleDock = useCallback(
    (winId: string) => {
      const target = windows.find((w) => w.id === winId);
      const willDock = !target?.docked;
      if (willDock) {
        setActivePage("workbench");
        setSidebarCollapsed((collapsed) => (collapsed ? collapsed : true));
      }
      setZCounter((prevZ) => {
        const nextZ = prevZ + 1;
        setWindows((prev) =>
          prev.map((w) =>
            w.id === winId
              ? {
                  ...w,
                  docked: willDock,
                  maximized: false,
                  minimized: false,
                  z: nextZ,
                }
              : willDock
                ? { ...w, docked: false }
                : w,
          ),
        );
        return nextZ;
      });
    },
    [windows],
  );

  const handleCloseWindow = useCallback(
    (winId: string) => {
      setWindows((prev) => prev.filter((w) => w.id !== winId));
    },
    [],
  );

  const handleChangeWindowTab = useCallback((winId: string, tab: WindowTab, requestedRunId?: string) => {
    setWindows((prev) =>
      prev.map((w) => (w.id === winId ? { ...w, activeTab: tab, requestedRunId } : w)),
    );
  }, []);

  const handleUpdateWindowParam = useCallback(
    (winId: string, key: string, value: unknown) => {
      setWindows((prev) =>
        prev.map((w) => {
          if (w.id !== winId) return w;
          const nextParams = { ...w.params, [key]: value };
          setDraftParamsByPlugin((dps) => ({
            ...dps,
            [w.pluginId]: nextParams,
          }));
          return { ...w, params: nextParams };
        }),
      );
    },
    [],
  );

  const searchMatchedPlugins = useMemo(() => {
    const q = moduleSearch.trim().toLowerCase();
    if (!q) return plugins;
    return plugins.filter((p) => {
      const targetHint =
        p.execution_target === "alithea_cloud"
          ? "cloud alithea"
          : p.execution_target === "froglet_node"
            ? "network froglet remote"
            : "local laptop";
      return (
        p.manifest.name.toLowerCase().includes(q) ||
        p.manifest.id.toLowerCase().includes(q) ||
        p.manifest.category.toLowerCase().includes(q) ||
        p.manifest.description.toLowerCase().includes(q) ||
        p.author.toLowerCase().includes(q) ||
        (p.manifest.author ?? "").toLowerCase().includes(q) ||
        p.execution_label.toLowerCase().includes(q) ||
        p.execution_target.toLowerCase().includes(q) ||
        targetHint.includes(q)
      );
    });
  }, [plugins, moduleSearch]);

  const moduleRuntimeCounts = useMemo(
    () => ({
      all: searchMatchedPlugins.length,
      local: searchMatchedPlugins.filter((p) => p.execution_target === "local").length,
      alithea_cloud: searchMatchedPlugins.filter(
        (p) => p.execution_target === "alithea_cloud",
      ).length,
      froglet_node: searchMatchedPlugins.filter(
        (p) => p.execution_target === "froglet_node",
      ).length,
    }),
    [searchMatchedPlugins],
  );

  const filteredPlugins = useMemo(() => {
    if (moduleRuntimeFilter === "all") return searchMatchedPlugins;
    return searchMatchedPlugins.filter(
      (p) => p.execution_target === moduleRuntimeFilter,
    );
  }, [searchMatchedPlugins, moduleRuntimeFilter]);

  const filteredRuns = useMemo(() => {
    const ordered = [...runs].sort((a, b) => b.created_at.localeCompare(a.created_at));
    const q = resultsSearch.trim().toLowerCase();
    if (!q) return ordered;
    return ordered.filter((r) => {
      const pluginName =
        plugins.find((p) => p.manifest.id === r.plugin_id)?.manifest.name ?? "";
      return (
        r.run_id.toLowerCase().includes(q) ||
        r.plugin_id.toLowerCase().includes(q) ||
        pluginName.toLowerCase().includes(q) ||
        runStatusLabel(r).toLowerCase().includes(q)
      );
    });
  }, [runs, plugins, resultsSearch]);

  const isEmptyChat =
    sessionEvents.length === 0 &&
    !pendingUserPrompt &&
    !streamingThinking &&
    !streamingText &&
    !chatBusy;

  const dockedWindow = useMemo(
    () => windows.find((w) => w.docked && !w.minimized) ?? null,
    [windows],
  );
  const dockedPlugin = useMemo(
    () =>
      dockedWindow
        ? dockedWindow.plugin ?? plugins.find((p) => p.manifest.id === dockedWindow.pluginId) ?? null
        : null,
    [dockedWindow, plugins],
  );
  const dockedRuns = useMemo(
    () =>
      dockedPlugin
        ? runs.filter((r) => r.plugin_id === dockedPlugin.manifest.id)
        : [],
    [dockedPlugin, runs],
  );
  const [isSplitResizing, setIsSplitResizing] = useState<boolean>(false);

  const handleSplitPointerDown = (e: React.PointerEvent<HTMLDivElement>) => {
    e.preventDefault();
    e.currentTarget.setPointerCapture(e.pointerId);
    splitResizingRef.current = true;
    setIsSplitResizing(true);
  };

  const handleSplitPointerMove = (e: React.PointerEvent<HTMLDivElement>) => {
    if (!splitResizingRef.current || !splitContainerRef.current) return;
    const rect = splitContainerRef.current.getBoundingClientRect();
    if (rect.width <= 0) return;
    const rightPx = rect.right - e.clientX;
    const pct = Math.min(72, Math.max(30, Math.round((rightPx / rect.width) * 1000) / 10));
    setDockWidthPct(pct);
  };

  const handleSplitPointerUp = (e: React.PointerEvent<HTMLDivElement>) => {
    if (splitResizingRef.current) {
      splitResizingRef.current = false;
      setIsSplitResizing(false);
      if (e.currentTarget.hasPointerCapture(e.pointerId)) {
        e.currentTarget.releasePointerCapture(e.pointerId);
      }
    }
  };

  const composerControls = useMemo<ProviderControlSpec[]>(() => {
    if (!llmConfig?.configured) return [];
    const base = llmConfig.active_controls ?? [];
    const activeModel =
      llmConfig.active?.model || base.find((c) => c.id === "model")?.current_value || "";
    const modelExtra = llmConfig.per_model_controls?.[activeModel] ?? [];
    const merged = base.map((c) => {
      if (c.id === "model") {
        return { ...c, current_value: activeModel };
      }
      const override = modelExtra.find((m) => m.id === c.id);
      const spec = override ?? c;
      const saved = llmConfig.active?.config_options?.[spec.id];
      return {
        ...spec,
        current_value: saved && saved.length > 0 ? saved : spec.current_value,
      };
    });
    for (const extra of modelExtra) {
      if (!merged.some((m) => m.id === extra.id)) {
        const saved = llmConfig.active?.config_options?.[extra.id];
        merged.push({
          ...extra,
          current_value: saved && saved.length > 0 ? saved : extra.current_value,
        });
      }
    }
    return merged;
  }, [llmConfig]);

  const handleComposerControlChange = async (
    controlId: string,
    nextValue: string,
  ): Promise<void> => {
    const active = llmConfig?.active;
    if (!active || savingComposerLlm || chatBusy) return;
    const nextModel = controlId === "model" ? nextValue : active.model;
    const nextOptions: Record<string, string> = { ...(active.config_options ?? {}) };
    if (controlId !== "model") {
      nextOptions[controlId] = nextValue;
    } else {
      const modelExtra = llmConfig.per_model_controls?.[nextValue] ?? [];
      for (const extra of modelExtra) {
        const current = nextOptions[extra.id];
        if (!current || !extra.options.some((o) => o.value === current)) {
          nextOptions[extra.id] = extra.current_value;
        }
      }
    }

    setSavingComposerLlm(true);
    try {
      const updated = await updateLlmConfig({
        protocol: active.protocol,
        base_url: active.base_url,
        command: active.command,
        args: active.args,
        model: nextModel,
        max_tokens: active.max_tokens,
        config_options: nextOptions,
      });
      setLlmConfig(updated);
      const freshHealth = await fetchHealth();
      setHealth(freshHealth);
    } catch (err: unknown) {
      showErrorBanner(err);
    } finally {
      setSavingComposerLlm(false);
    }
  };

  const activeProviderLabel = useMemo(() => {
    const pid = llmConfig?.active?.provider_id;
    if (!llmConfig?.configured && !health?.llm_configured) return "Connect LLM";
    if (pid === "vibe") return "Mistral Vibe";
    if (pid === "codex") return "Codex";
    if (pid === "claude") return "Claude";
    if (pid === "opencode") return "OpenCode";
    if (pid === "gemini") return "Gemini";
    if (pid === "openai") return "OpenAI";
    if (pid === "mistral") return "Mistral";
    if (pid === "deepseek") return "DeepSeek";
    if (pid === "ollama") return "Ollama";
    return llmConfig?.display_name ?? health?.llm_model ?? "Connected";
  }, [llmConfig, health]);

  const notebookPlugin = useMemo(
    () =>
      plugins.find((p) => p.manifest.id === "alithea/bc-jupyter-notebook") ??
      plugins.find((p) => p.manifest.id.toLowerCase().includes("notebook")) ??
      null,
    [plugins],
  );

  const dockPluginInSplitView = useCallback(
    (plugin: PluginSummary, preferredTab?: WindowTab, requestedRunId?: string) => {
      setActivePage("workbench");
      setSidebarCollapsed(true);
      setZCounter((prevZ) => {
        const nextZ = prevZ + 1;
        setWindows((prev) => {
          const existing = prev.find((w) => w.pluginId === plugin.manifest.id);
          if (existing) {
            return prev.map((w) =>
              w.id === existing.id
                ? {
                    ...w,
                    minimized: false,
                    docked: true,
                    z: nextZ,
                    activeTab: preferredTab ?? w.activeTab,
                    requestedRunId,
                  }
                : w.docked
                  ? { ...w, docked: false, minimized: true }
                  : w,
            );
          }
          const sidebarW = 60;
          const w = Math.min(820, Math.max(540, window.innerWidth - sidebarW - 56));
          const h = Math.min(560, Math.max(410, window.innerHeight - 130));
          const created: ModuleWindowState = {
            id: `win-${plugin.manifest.id}`,
            pluginId: plugin.manifest.id,
            plugin,
            x: Math.max(sidebarW + 18, Math.round(sidebarW + (window.innerWidth - sidebarW - w) / 2)),
            y: 64,
            w,
            h,
            z: nextZ,
            minimized: false,
            maximized: false,
            docked: true,
            activeTab: preferredTab ?? (plugin.has_custom_ui ? "ui" : "run"),
            requestedRunId,
            params: { ...defaultParamsForPlugin(plugin), ...(draftParamsByPlugin[plugin.manifest.id] ?? {}) },
          };
          return [...prev.map((x) => (x.docked ? { ...x, docked: false, minimized: true } : x)), created];
        });
        return nextZ;
      });
    },
    [draftParamsByPlugin],
  );

  const handleInsertCodeIntoNotebook = useCallback(
    (code: string, _lang: string) => {
      if (!notebookPlugin) return;
      const pluginId = notebookPlugin.manifest.id;
      const currentDraft =
        draftParamsByPlugin[pluginId] ??
        windows.find((w) => w.pluginId === pluginId)?.params ??
        defaultParamsForPlugin(notebookPlugin);
      const rawCells = Array.isArray(currentDraft["cells"])
        ? [...(currentDraft["cells"] as Record<string, unknown>[])]
        : [];
      rawCells.push({
        id: `cell-${Date.now().toString(36)}`,
        cell_type: "code",
        source: code,
      });
      const nextParams = { ...currentDraft, cells: rawCells };
      setDraftParamsByPlugin((prev) => ({ ...prev, [pluginId]: nextParams }));
      setWindows((prev) =>
        prev.map((w) => (w.pluginId === pluginId ? { ...w, params: nextParams } : w)),
      );
      dockPluginInSplitView(notebookPlugin, "ui");
    },
    [notebookPlugin, draftParamsByPlugin, windows, dockPluginInSplitView],
  );

  const controlIcon = (ctrl: ProviderControlSpec): React.ReactNode => {
    if (ctrl.id === "model") return null;
    if (ctrl.id === "mode") return <IconMode size={13} />;
    if (ctrl.category === "thought_level" || ctrl.id === "effort" || ctrl.id === "thinking") {
      return <IconEffort size={13} />;
    }
    return null;
  };

  type ComposerMentionItem =
    | {
        kind: "module";
        id: string;
        title: string;
        subtitle: string;
        badge: string;
        iconUrl?: string | null;
      }
    | {
        kind: "catalog";
        id: string;
        title: string;
        subtitle: string;
        badge: string;
      };

  const mentionItems = useMemo<ComposerMentionItem[]>(() => {
    if (!mentionTrigger) return [];
    const q = mentionTrigger.query.trim().toLowerCase();
    if (mentionTrigger.kind === "module") {
      return plugins
        .filter((p) => {
          if (!q) return true;
          const hay = [
            p.manifest.name,
            p.manifest.id,
            p.manifest.category,
            p.manifest.description,
          ]
            .join(" ")
            .toLowerCase();
          return hay.includes(q);
        })
        .map((p) => ({
          kind: "module" as const,
          id: p.manifest.id,
          title: p.manifest.name,
          subtitle: p.manifest.id,
          badge: p.manifest.category,
          iconUrl: p.icon_url,
        }));
    }
    return catalogs
      .filter((c) => {
        if (!q) return true;
        const hay = [c.name, c.id, c.description, c.organism ?? "", c.version]
          .join(" ")
          .toLowerCase();
        return hay.includes(q);
      })
      .map((c) => {
        const rows =
          c.row_count ??
          (c.shards ?? []).reduce(
            (sum, s) => sum + (s.row_count ?? 0),
            0,
          );
        return {
          kind: "catalog" as const,
          id: c.id,
          title: c.name,
          subtitle: c.id,
          badge: rows > 0 ? `${rows.toLocaleString()} rows` : `v${c.version}`,
        };
      });
  }, [mentionTrigger, plugins, catalogs]);

  const syncMentionTriggerFromInput = (
    value: string,
    selectionStart: number | null,
  ) => {
    const caret = selectionStart ?? value.length;
    const before = value.slice(0, caret);
    const match = /(^|[\s(\["'])([#@])([^\s#@]*)$/.exec(before);
    if (!match) {
      setMentionTrigger(null);
      return;
    }
    const lead = match[1] ?? "";
    const sigil = match[2] as "#" | "@";
    const query = match[3] ?? "";
    const tokenStart = match.index + lead.length;
    const nextKind = sigil === "#" ? "module" : "catalog";
    setMentionTrigger((prev) => {
      if (!prev || prev.kind !== nextKind || prev.query !== query) {
        setMentionActiveIndex(0);
      }
      return {
        kind: nextKind,
        query,
        start: tokenStart,
        end: caret,
      };
    });
  };

  const applyMentionSuggestion = (item: ComposerMentionItem) => {
    if (!mentionTrigger) return;
    const prefix = item.kind === "module" ? "#" : "@";
    const inserted = `${prefix}${item.id} `;
    const nextValue =
      chatInput.slice(0, mentionTrigger.start) +
      inserted +
      chatInput.slice(mentionTrigger.end);
    const nextCaret = mentionTrigger.start + inserted.length;
    setChatInput(nextValue);
    setMentionTrigger(null);
    setMentionActiveIndex(0);
    window.requestAnimationFrame(() => {
      const el = composerTextareaRef.current;
      if (el) {
        el.focus();
        el.setSelectionRange(nextCaret, nextCaret);
      }
    });
  };

  const renderUserPromptTokens = (text: string) => {
    const pluginIds = new Set(plugins.map((p) => p.manifest.id));
    const catalogIds = new Set(catalogs.map((c) => c.id));
    const parts = text.split(/([#@][a-zA-Z0-9._/-]+)/g);
    if (parts.length <= 1) return text;
    return parts.map((part, idx) => {
      if (part.startsWith("#") && pluginIds.has(part.slice(1))) {
        const pl = plugins.find((p) => p.manifest.id === part.slice(1));
        if (!pl) return <span key={idx}>{part}</span>;
        return (
          <button
            key={idx}
            type="button"
            className="bc-mention-token"
            data-kind="module"
            title={`Open module: ${pl.manifest.name}`}
            onClick={() => openOrFocusModuleWindow(pl, "ui")}
          >
            {part}
          </button>
        );
      }
      if (part.startsWith("@") && catalogIds.has(part.slice(1))) {
        const cat = catalogs.find((c) => c.id === part.slice(1));
        return (
          <button
            key={idx}
            type="button"
            className="bc-mention-token"
            data-kind="catalog"
            title={`Open catalog: ${cat?.name ?? part.slice(1)}`}
            onClick={() => navigateToPage("catalogs")}
          >
            {part}
          </button>
        );
      }
      return <span key={idx}>{part}</span>;
    });
  };

  const renderComposer = () => {
    const hasModelControl = composerControls.some(
      (c) => c.id === "model" && c.options.length > 0,
    );
    const activeModelLabel = llmConfig?.active?.model?.trim();
    const safeMentionIdx =
      mentionItems.length > 0
        ? Math.min(mentionActiveIndex, mentionItems.length - 1)
        : 0;

    return (
      <form
        className="bc-composer"
        onSubmit={(e) => {
          e.preventDefault();
          try {
            const budget = isCloudMode ? readExecutionBudget(e.currentTarget) : undefined;
            void submitChatPrompt(chatInput, undefined, budget);
          } catch (error) {
            showErrorBanner(error);
          }
        }}
      >
        {mentionTrigger && (
          <div
            className="bc-mention-popover"
            role="listbox"
            aria-label={
              mentionTrigger.kind === "module"
                ? "Module suggestions"
                : "Data catalog suggestions"
            }
            data-testid="composer-mention-popover"
          >
            <div className="bc-mention-head">
              <span className="bc-mention-head-title">
                {mentionTrigger.kind === "module" ? (
                  <>
                    <IconModules size={13} />
                    <span>Modules</span>
                  </>
                ) : (
                  <>
                    <IconDatabase size={13} />
                    <span>Data Catalogs</span>
                  </>
                )}
                {mentionTrigger.query && (
                  <code className="bc-mention-query mono">
                    {mentionTrigger.kind === "module" ? "#" : "@"}
                    {mentionTrigger.query}
                  </code>
                )}
              </span>
              <span className="bc-mention-hint">
                ↑↓ navigate · Enter/Tab insert · Esc dismiss
              </span>
            </div>
            {mentionItems.length === 0 ? (
              <div className="bc-mention-empty">
                No matching{" "}
                {mentionTrigger.kind === "module" ? "modules" : "data catalogs"}
              </div>
            ) : (
              <div className="bc-mention-list">
                {mentionItems.map((item, idx) => (
                  <button
                    key={`${item.kind}:${item.id}`}
                    type="button"
                    role="option"
                    aria-selected={idx === safeMentionIdx}
                    className="bc-mention-item"
                    data-active={String(idx === safeMentionIdx)}
                    data-testid={`mention-option-${item.id}`}
                    onMouseEnter={() => setMentionActiveIndex(idx)}
                    onMouseDown={(e) => {
                      e.preventDefault();
                      applyMentionSuggestion(item);
                    }}
                  >
                    <span className="bc-mention-item-icon">
                      {item.kind === "module" ? (
                        item.iconUrl ? (
                          <img
                            src={item.iconUrl}
                            alt=""
                            className="bc-mention-plugin-img"
                          />
                        ) : (
                          <IconModules size={14} />
                        )
                      ) : (
                        <IconDatabase size={14} />
                      )}
                    </span>
                    <span className="bc-mention-item-main">
                      <span className="bc-mention-item-title">{item.title}</span>
                      <span className="bc-mention-item-id mono">
                        {item.kind === "module" ? "#" : "@"}
                        {item.subtitle}
                      </span>
                    </span>
                    <span className="bc-mention-item-badge">{item.badge}</span>
                  </button>
                ))}
              </div>
            )}
          </div>
        )}
        {attachedModuleContext && (
          <div className="bc-composer-attachment" data-testid="composer-attachment">
            <div className="bc-composer-attachment-info">
              <div className="bc-composer-attachment-header">
                <span>{attachedModuleContext.label}</span>
                <span className="bc-composer-attachment-scope">
                  {attachedModuleContext.context["source"] === "data_explorer_selection"
                    ? "Cohort selection attached"
                    : "Entire notebook attached"}
                </span>
              </div>
              {attachedModuleContext.preview && (
                <div className="bc-composer-attachment-preview">
                  {attachedModuleContext.preview}
                </div>
              )}
              {attachedModuleContext.images &&
                attachedModuleContext.images.length > 0 && (
                  <div className="bc-composer-attachment-thumbs">
                    {attachedModuleContext.images.map((img, idx) => (
                      <img
                        key={idx}
                        src={img.src}
                        alt={img.alt}
                        className="bc-chat-thumb"
                        onClick={() => setLightboxImage(img)}
                      />
                    ))}
                  </div>
                )}
            </div>
            <button
              type="button"
              className="bc-composer-attachment-clear"
              aria-label="Remove attached context"
              title="Remove attached context"
              onClick={() => setAttachedModuleContext(null)}
            >
              <IconClose size={13} />
            </button>
          </div>
        )}
        <textarea
          ref={composerTextareaRef}
          className="bc-composer-textarea"
          rows={1}
          placeholder={
            attachedModuleContext
              ? `Ask about ${attachedModuleContext.label} (# module, @ catalog)...`
              : "Ask about HLA cohorts, type # for modules, or @ for data catalogs..."
          }
          value={chatInput}
          onChange={(e) => {
            const val = e.target.value;
            setChatInput(val);
            syncMentionTriggerFromInput(val, e.target.selectionStart);
          }}
          onClick={(e) =>
            syncMentionTriggerFromInput(
              e.currentTarget.value,
              e.currentTarget.selectionStart,
            )
          }
          onBlur={() => setMentionTrigger(null)}
          onKeyDown={(e) => {
            if (mentionTrigger && mentionItems.length > 0) {
              if (e.key === "ArrowDown") {
                e.preventDefault();
                setMentionActiveIndex((i) => (i + 1) % mentionItems.length);
                return;
              }
              if (e.key === "ArrowUp") {
                e.preventDefault();
                setMentionActiveIndex(
                  (i) => (i - 1 + mentionItems.length) % mentionItems.length,
                );
                return;
              }
              if ((e.key === "Enter" && !e.shiftKey) || e.key === "Tab") {
                e.preventDefault();
                const chosen = mentionItems[safeMentionIdx];
                if (chosen) applyMentionSuggestion(chosen);
                return;
              }
            }
            if (mentionTrigger && e.key === "Escape") {
              e.preventDefault();
              setMentionTrigger(null);
              return;
            }
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault();
              e.currentTarget.form?.requestSubmit();
            }
          }}
        />
        <div className="bc-composer-bar">
          <div className="bc-composer-controls">
            <button
              type="button"
              className="bc-composer-provider-btn"
              data-connected={String(Boolean(llmConfig?.configured ?? health?.llm_configured))}
              data-testid="llm-status-pill"
              onClick={() => openSettings("agent")}
              title={
                llmConfig?.configured
                  ? `Provider: ${llmConfig.display_name ?? activeProviderLabel} (click to switch)`
                  : "Connect a subscription CLI (Codex / Claude Code), Mistral Vibe, OpenCode, or API key"
              }
            >
              <ProviderLogo
                provider={llmConfig?.active?.provider_id ?? "acp"}
                size={14}
              />
              <span className="bc-llm-pill-dot" />
              <span>{activeProviderLabel}</span>
            </button>

            {isCloudMode && (
              <ExecutionBudgetControl
                key={JSON.stringify([approvalScope, session?.session_id, chatBusy])}
                disabled={chatBusy}
              />
            )}
            {llmConfig?.configured &&
              composerControls.map((ctrl) =>
                ctrl.options.length > 0 ? (
                  <label
                    key={ctrl.id}
                    className="bc-composer-control"
                    title={`${ctrl.label}: ${ctrl.options.find((o) => o.value === ctrl.current_value)?.label ?? ctrl.current_value}`}
                  >
                    {controlIcon(ctrl) && (
                      <span className="bc-composer-control-icon">
                        {controlIcon(ctrl)}
                      </span>
                    )}
                    <select
                      className="bc-composer-select mono"
                      aria-label={ctrl.label}
                      data-testid={`composer-control-${ctrl.id}`}
                      value={ctrl.current_value}
                      disabled={chatBusy || savingComposerLlm}
                      onChange={(e) =>
                        void handleComposerControlChange(ctrl.id, e.target.value)
                      }
                    >
                      {ctrl.options.map((opt) => (
                        <option
                          key={opt.value}
                          value={opt.value}
                          title={opt.description ?? opt.label}
                        >
                          {opt.label}
                        </option>
                      ))}
                    </select>
                  </label>
                ) : null,
              )}

            {llmConfig?.configured && !hasModelControl && activeModelLabel && (
              <button
                type="button"
                className="bc-composer-control mono"
                onClick={() => openSettings("agent")}
                title="Change model in Settings"
              >
                {activeModelLabel}
              </button>
            )}
          </div>

          <div className="bc-composer-actions">
            <button
              type="submit"
              className="bc-send-btn"
              aria-label="Send message"
              disabled={chatBusy || (!chatInput.trim() && !attachedModuleContext)}
            >
              {chatBusy ? <RunningCompass size={16} /> : <IconSend size={15} />}
            </button>
          </div>
        </div>
      </form>
    );
  };

  const projectIdsSet = new Set(projects.map((p) => p.project_id));
  const sessionsByProject = new Map<string, SessionRecord[]>();
  const ungroupedSessions: SessionRecord[] = [];
  for (const s of sessions) {
    if (s.project_id && projectIdsSet.has(s.project_id)) {
      const bucket = sessionsByProject.get(s.project_id) ?? [];
      bucket.push(s);
      sessionsByProject.set(s.project_id, bucket);
    } else {
      ungroupedSessions.push(s);
    }
  }

  const renderSessionRow = (s: SessionRecord) => {
    const isActive = activePage === "workbench" && session?.session_id === s.session_id;
    const isEditing = editingSessionId === s.session_id;
    const isConfirmingDelete = confirmDeleteSessionId === s.session_id;
    const isMoveOpen = moveMenuSessionId === s.session_id;

    return (
      <React.Fragment key={s.session_id}>
        {isEditing ? (
          <form
            className="bc-inline-edit-form"
            onSubmit={(e) => {
              e.preventDefault();
              void handleRenameSessionSubmit(s.session_id);
            }}
          >
            <input
              type="text"
              className="bc-inline-edit-input"
              aria-label="Rename chat title"
              autoFocus
              value={editingSessionTitle}
              onChange={(e) => setEditingSessionTitle(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Escape") {
                  setEditingSessionId(null);
                }
              }}
              onBlur={() => void handleRenameSessionSubmit(s.session_id)}
            />
          </form>
        ) : (
          <div
            className="bc-session-row"
            data-active={String(isActive)}
            data-menu-open={String(isMoveOpen || isConfirmingDelete)}
          >
            <button
              type="button"
              className="bc-session-btn"
              title={s.title}
              onClick={() => void handleSelectSession(s)}
              onDoubleClick={() => {
                setMoveMenuSessionId(null);
                setConfirmDeleteSessionId(null);
                setEditingSessionId(s.session_id);
                setEditingSessionTitle(s.title);
              }}
            >
              {s.title}
            </button>
            <div className="bc-row-actions">
              {isConfirmingDelete ? (
                <>
                  <button
                    type="button"
                    className="bc-sidebar-icon-btn confirm"
                    title="Confirm remove chat"
                    aria-label="Confirm remove chat"
                    onClick={(e) => {
                      e.stopPropagation();
                      void handleDeleteSession(s.session_id);
                    }}
                  >
                    <IconCheckMark size={12} />
                  </button>
                  <button
                    type="button"
                    className="bc-sidebar-icon-btn"
                    title="Cancel remove"
                    aria-label="Cancel remove"
                    onClick={(e) => {
                      e.stopPropagation();
                      setConfirmDeleteSessionId(null);
                    }}
                  >
                    <IconClose size={12} />
                  </button>
                </>
              ) : (
                <>
                  <button
                    type="button"
                    className="bc-sidebar-icon-btn"
                    title="Move to project"
                    aria-label="Move chat to project"
                    onClick={(e) => {
                      e.stopPropagation();
                      setConfirmDeleteSessionId(null);
                      setMoveMenuSessionId((prev) =>
                        prev === s.session_id ? null : s.session_id,
                      );
                    }}
                  >
                    <IconFolder size={13} />
                  </button>
                  <button
                    type="button"
                    className="bc-sidebar-icon-btn"
                    title="Rename chat"
                    aria-label="Rename chat"
                    onClick={(e) => {
                      e.stopPropagation();
                      setMoveMenuSessionId(null);
                      setConfirmDeleteSessionId(null);
                      setEditingSessionId(s.session_id);
                      setEditingSessionTitle(s.title);
                    }}
                  >
                    <IconPencil size={12} />
                  </button>
                  <button
                    type="button"
                    className="bc-sidebar-icon-btn danger"
                    title="Remove chat"
                    aria-label="Remove chat"
                    onClick={(e) => {
                      e.stopPropagation();
                      setMoveMenuSessionId(null);
                      setConfirmDeleteSessionId(s.session_id);
                    }}
                  >
                    <IconTrash size={12} />
                  </button>
                </>
              )}
            </div>
          </div>
        )}
        {isMoveOpen && (
          <div className="bc-move-project-bar">
            <span className="bc-move-project-title">Move to project</span>
            <button
              type="button"
              className="bc-move-project-chip"
              data-selected={String(!s.project_id)}
              onClick={() => void handleAssignSessionProject(s.session_id, null)}
            >
              No project
            </button>
            {projects.map((proj) => (
              <button
                key={proj.project_id}
                type="button"
                className="bc-move-project-chip"
                data-selected={String(s.project_id === proj.project_id)}
                onClick={() =>
                  void handleAssignSessionProject(s.session_id, proj.project_id)
                }
              >
                {proj.name}
              </button>
            ))}
            {projects.length === 0 && (
              <button
                type="button"
                className="bc-move-project-chip"
                onClick={() => {
                  setMoveMenuSessionId(null);
                  setCreatingProject(true);
                }}
              >
                + New project
              </button>
            )}
          </div>
        )}
      </React.Fragment>
    );
  };

  if (isCloudMode && authRequired) {
    return (
      <AuthGate
        onAuthenticated={(me) => {
          setPrincipal(me);
          setAuthRequired(false);
          setErrorBanner(null);
          void bootstrapWorkspace().catch((err: unknown) => showErrorBanner(err));
        }}
      />
    );
  }

  return (
    <div className="bc-shell">
      {/* ==================== LEFT SIDEBAR ==================== */}
      <aside className="bc-sidebar" data-collapsed={String(sidebarCollapsed)}>
        <div className="bc-sidebar-top">
          {sidebarCollapsed ? (
            <div className="bc-collapsed-brand-wrap">
              <span className="bc-collapsed-brand-mark" aria-hidden="true">
                <CompassMark size={24} />
              </span>
              <button
                type="button"
                className="bc-sidebar-expand-btn"
                aria-label="Expand sidebar"
                title="Expand sidebar"
                data-testid="expand-sidebar-btn"
                onClick={() => setSidebarCollapsed(false)}
              >
                <IconSidebar size={16} />
              </button>
            </div>
          ) : (
            <>
              <button
                type="button"
                className="bc-brand-btn"
                onClick={() => navigateToPage("workbench")}
                title="BioCompass Home"
              >
                <BrandWordmark size={22} />
              </button>

              <button
                type="button"
                className="bc-icon-btn"
                aria-label="Collapse sidebar"
                onClick={() => setSidebarCollapsed(true)}
              >
                <IconSidebar size={16} />
              </button>
            </>
          )}
        </div>

        <button
          type="button"
          className="bc-new-chat-btn"
          onClick={() => void handleNewSession()}
          title="New Chat"
        >
          <IconPlus size={15} />
          {!sidebarCollapsed && <span>New Chat</span>}
        </button>

        <nav className="bc-nav-list" aria-label="Workspace navigation">
          <button
            type="button"
            data-testid="nav-workbench"
            className="bc-nav-item"
            data-active={String(activePage === "workbench")}
            onClick={() => navigateToPage("workbench")}
            title="Chat"
          >
            <span className="bc-nav-item-left">
              <IconChat size={16} />
              {!sidebarCollapsed && <span>Chat</span>}
            </span>
          </button>

          <button
            type="button"
            data-testid="nav-plugins"
            className="bc-nav-item"
            data-active={String(activePage === "plugins")}
            onClick={() => navigateToPage("plugins")}
            title="Modules"
          >
            <span className="bc-nav-item-left">
              <IconModules size={16} />
              {!sidebarCollapsed && <span>Modules</span>}
            </span>
            {!sidebarCollapsed && (
              <span className="bc-caption">{plugins.length}</span>
            )}
          </button>

          <button
            type="button"
            data-testid="nav-results"
            className="bc-nav-item"
            data-active={String(activePage === "results")}
            onClick={() => navigateToPage("results")}
            title="Results"
          >
            <span className="bc-nav-item-left">
              <IconResults size={16} />
              {!sidebarCollapsed && <span>Results</span>}
            </span>
            {!sidebarCollapsed && !((workspaceBearer || workspaceRead) && health?.workspace_hub_url && !isCloudMode) && (
              <span className="bc-caption">{runs.length}</span>
            )}
          </button>

          <button
            type="button"
            data-testid="nav-catalogs"
            className="bc-nav-item"
            data-active={String(activePage === "catalogs")}
            onClick={() => navigateToPage("catalogs")}
            title={
              sidebarCollapsed
                ? `Data (${selectedCatalogCount} selected)`
                : "Data"
            }
          >
            <span className="bc-nav-item-left">
              <IconDatabase size={16} />
              {!sidebarCollapsed && <span>Data</span>}
            </span>
            {!sidebarCollapsed ? (
              <span className="bc-nav-notify" data-testid="catalogs-selected-count">
                {selectedCatalogCount}
              </span>
            ) : (
              selectedCatalogCount > 0 && (
                <span className="bc-nav-notify-mini" data-testid="catalogs-selected-count-mini">
                  {selectedCatalogCount}
                </span>
              )
            )}
          </button>

          {isCloudMode && principal && (
            <button
              type="button"
              data-testid="nav-org-admin"
              className="bc-nav-item"
              data-active={String(activePage === "org-admin")}
              onClick={() => navigateToPage("org-admin")}
              title="Organization"
            >
              <span className="bc-nav-item-left">
                <IconShield size={16} />
                {!sidebarCollapsed && <span>Organization</span>}
              </span>
            </button>
          )}

          {isCloudMode && principal?.is_platform_admin && (
            <button
              type="button"
              data-testid="nav-control-tower"
              className="bc-nav-item"
              data-active={String(activePage === "control-tower")}
              onClick={() => navigateToPage("control-tower")}
              title="Control Tower"
            >
              <span className="bc-nav-item-left">
                <IconAlitheaCloud size={16} />
                {!sidebarCollapsed && <span>Control Tower</span>}
              </span>
            </button>
          )}
        </nav>

        {!sidebarCollapsed && (
          <>
            <div className="bc-section-label-row">
              <span className="bc-section-label">Projects</span>
              <button
                type="button"
                data-testid="new-project-btn"
                className="bc-sidebar-icon-btn"
                title="New project"
                aria-label="New project"
                onClick={() => {
                  setCreatingProject((prev) => !prev);
                  setNewProjectName("");
                }}
              >
                <IconPlus size={13} />
              </button>
            </div>

            {creatingProject && (
              <form
                className="bc-inline-edit-form"
                onSubmit={(e) => {
                  e.preventDefault();
                  void handleCreateProjectSubmit();
                }}
              >
                <input
                  type="text"
                  className="bc-inline-edit-input"
                  placeholder="Project name..."
                  aria-label="New project name"
                  autoFocus
                  value={newProjectName}
                  onChange={(e) => setNewProjectName(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Escape") {
                      setCreatingProject(false);
                      setNewProjectName("");
                    }
                  }}
                  onBlur={() => void handleCreateProjectSubmit()}
                />
              </form>
            )}

            <div className="bc-sessions-scroll">
              {projects.map((proj) => {
                const projSessions = sessionsByProject.get(proj.project_id) ?? [];
                const isExpanded = !collapsedProjects[proj.project_id];
                const isEditingProj = editingProjectId === proj.project_id;
                const isConfirmingProjDel = confirmDeleteProjectId === proj.project_id;
                return (
                  <div key={proj.project_id} className="bc-project-group">
                    {isEditingProj ? (
                      <form
                        className="bc-inline-edit-form"
                        onSubmit={(e) => {
                          e.preventDefault();
                          void handleRenameProjectSubmit(proj.project_id);
                        }}
                      >
                        <input
                          type="text"
                          className="bc-inline-edit-input"
                          aria-label="Rename project"
                          autoFocus
                          value={editingProjectName}
                          onChange={(e) => setEditingProjectName(e.target.value)}
                          onKeyDown={(e) => {
                            if (e.key === "Escape") {
                              setEditingProjectId(null);
                            }
                          }}
                          onBlur={() => void handleRenameProjectSubmit(proj.project_id)}
                        />
                      </form>
                    ) : (
                      <div className="bc-project-row">
                        <button
                          type="button"
                          className="bc-project-toggle"
                          title={proj.name}
                          onClick={() =>
                            setCollapsedProjects((prev) => ({
                              ...prev,
                              [proj.project_id]: !prev[proj.project_id],
                            }))
                          }
                        >
                          <IconChevronRight
                            size={11}
                            className="bc-project-chevron"
                            data-expanded={String(isExpanded)}
                          />
                          <IconFolder size={13} />
                          <span className="bc-project-name">{proj.name}</span>
                          <span className="bc-project-count">{projSessions.length}</span>
                        </button>
                        <div className="bc-row-actions">
                          {isConfirmingProjDel ? (
                            <>
                              <button
                                type="button"
                                className="bc-sidebar-icon-btn confirm"
                                title="Confirm delete project"
                                aria-label="Confirm delete project"
                                onClick={(e) => {
                                  e.stopPropagation();
                                  void handleDeleteProject(proj.project_id);
                                }}
                              >
                                <IconCheckMark size={12} />
                              </button>
                              <button
                                type="button"
                                className="bc-sidebar-icon-btn"
                                title="Cancel delete"
                                aria-label="Cancel delete project"
                                onClick={(e) => {
                                  e.stopPropagation();
                                  setConfirmDeleteProjectId(null);
                                }}
                              >
                                <IconClose size={12} />
                              </button>
                            </>
                          ) : (
                            <>
                              <button
                                type="button"
                                className="bc-sidebar-icon-btn"
                                title={`New chat in ${proj.name}`}
                                aria-label={`New chat in ${proj.name}`}
                                onClick={(e) => {
                                  e.stopPropagation();
                                  void handleNewSession(proj.project_id);
                                }}
                              >
                                <IconPlus size={12} />
                              </button>
                              <button
                                type="button"
                                className="bc-sidebar-icon-btn"
                                title="Rename project"
                                aria-label="Rename project"
                                onClick={(e) => {
                                  e.stopPropagation();
                                  setConfirmDeleteProjectId(null);
                                  setEditingProjectId(proj.project_id);
                                  setEditingProjectName(proj.name);
                                }}
                              >
                                <IconPencil size={12} />
                              </button>
                              <button
                                type="button"
                                className="bc-sidebar-icon-btn danger"
                                title="Delete project"
                                aria-label="Delete project"
                                onClick={(e) => {
                                  e.stopPropagation();
                                  setConfirmDeleteProjectId(proj.project_id);
                                }}
                              >
                                <IconTrash size={12} />
                              </button>
                            </>
                          )}
                        </div>
                      </div>
                    )}
                    {isExpanded && (
                      <div className="bc-project-children">
                        {projSessions.length > 0 ? (
                          projSessions.map((s) => renderSessionRow(s))
                        ) : (
                          <div className="bc-project-empty">No chats yet</div>
                        )}
                      </div>
                    )}
                  </div>
                );
              })}

              <div className="bc-section-label bc-section-label-standalone">
                {projects.length > 0 ? "Chats" : "Recent Chats"}
              </div>
              {ungroupedSessions.map((s) => renderSessionRow(s))}
            </div>
          </>
        )}

        <div className="bc-sidebar-footer">
          <div
            className="bc-health-row"
            data-testid="health-pill"
            title={
              health
                ? `Engine: ${health.container_engine} · Node: ${health.node_id}`
                : "Connecting..."
            }
          >
            <span className="bc-status-dot" />
            {!sidebarCollapsed && (
              <span className="mono">
                {health ? (isCloudMode ? "Cloud" : "Local") : "Connecting..."}
              </span>
            )}
          </div>

          {health && effectiveMarketplaceStatus && (
            <button
              type="button"
              className="bc-nav-item"
              data-testid="sidebar-marketplace-sync-pill"
              data-sync-state={marketplaceBadge.state}
              onClick={() => openSettings("general")}
              title={`Global Marketplace: ${marketplaceBadge.label}${effectiveMarketplaceStatus.last_synced_at ? ` · Last synced ${new Date(effectiveMarketplaceStatus.last_synced_at).toLocaleString()}` : ""}`}
            >
              <span className="bc-nav-item-left" style={{ minWidth: 0 }}>
                <span
                  aria-hidden="true"
                  className="bc-status-dot"
                  style={{
                    background:
                      marketplaceBadge.tone === "success"
                        ? "var(--bc-success, #22c55e)"
                        : marketplaceBadge.tone === "warn"
                          ? "var(--bc-warn, #f59e0b)"
                          : "var(--bc-text-muted, #94a3b8)",
                  }}
                />
                {!sidebarCollapsed && (
                  <span
                    className="mono"
                    style={{
                      fontSize: 11.5,
                      overflow: "hidden",
                      textOverflow: "ellipsis",
                      whiteSpace: "nowrap",
                    }}
                  >
                    Marketplace · {marketplaceBadge.label}
                  </span>
                )}
              </span>
            </button>
          )}

          {isCloudMode && principal && (
            <div data-testid="cloud-principal-pill">
              {profileUserId && (
                <button type="button" className="bc-nav-item" data-testid="nav-profile"
                  aria-label="Open profile" title="Open profile" onClick={() => openSettings("account")}>
                  <span className="bc-nav-item-left" style={{ minWidth: 0 }}>
                    <span aria-hidden="true" style={{ display: "inline-flex" }}><PersonalAvatar seed={myProfile?.avatar_seed} name={myProfile?.display_name ?? ""} size={24} /></span>
                    {!sidebarCollapsed && (
                      <span style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                        {myProfile?.display_name || myProfile?.email || "My profile"}
                      </span>
                    )}
                  </span>
                </button>
              )}
              {!sidebarCollapsed && (
                <div className="bc-caption mono" style={{ padding: "2px 10px 4px", display: "flex", gap: 6, justifyContent: "space-between" }}>
                  <span title={principal.org_id} style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                    {principal.org_id} · {principal.role}
                  </span>
                  <button type="button" className="bc-link-btn" data-testid="logout-btn" onClick={() => void handleLogout()}>
                    Sign out
                  </button>
                </div>
              )}
            </div>
          )}

          <button
            type="button"
            data-testid="nav-settings"
            className="bc-nav-item"
            onClick={() => openSettings("general")}
            title={availableUpdate ? `Settings · BioCompass ${availableUpdate} is available` : "Settings (⌘,)"}
            aria-label={availableUpdate ? `Settings · BioCompass ${availableUpdate} update available` : "Settings"}
          >
            <span className="bc-nav-item-left">
              <IconSettings size={16} />
              {!sidebarCollapsed && <span>{availableUpdate ? "Settings · Update available" : "Settings"}</span>}
              {sidebarCollapsed && availableUpdate && <span className="bc-status-dot" aria-hidden="true" />}
            </span>
          </button>
        </div>
      </aside>

      {/* ==================== MAIN WORKSPACE ==================== */}
      <main className="bc-main">
        {errorBanner && (
          <div className="bc-error-banner" role="alert">
            <div className="bc-error-banner-body">
              <span className="bc-error-banner-message">{errorBanner.message}</span>
              {errorBanner.hint && (
                <span className="bc-error-banner-hint">{errorBanner.hint}</span>
              )}
            </div>
            <button
              type="button"
              className="bc-icon-btn"
              aria-label="Dismiss error"
              onClick={() => setErrorBanner(null)}
            >
              <IconClose size={14} />
            </button>
          </div>
        )}

        {/* ------------------ PAGE 1: CHAT (+ OPTIONAL SPLIT-SCREEN CANVAS) ------------------ */}
        {activePage === "workbench" && (
          <div
            ref={splitContainerRef}
            className="bc-workbench-split"
            data-docked={String(Boolean(dockedWindow && dockedPlugin))}
            data-resizing={String(isSplitResizing)}
          >
          <div className="bc-chat-layout">
            <div className="bc-chat-scroll">
              {isEmptyChat ? (
                <div className="bc-hero">
                  <div
                    className="bc-hero-heading"
                    onMouseEnter={() => setHeroHovering(true)}
                    onMouseLeave={() => setHeroHovering(false)}
                  >
                    <HeroCompass hovering={heroHovering} />
                    <h1 className="bc-hero-title">What would you like to explore?</h1>
                  </div>

                  <div className="bc-composer-wrap">{renderComposer()}</div>

                  {plugins.length > 0 && (
                    <div className="bc-starter-section">
                      <div className="bc-section-label" style={{ paddingLeft: 2 }}>
                        Quick-Launch Modules
                      </div>
                      <div className="bc-starter-grid">
                        {plugins.slice(0, 4).map((p) => (
                          <button
                            key={p.manifest.id}
                            type="button"
                            data-testid={`quick-prepare-btn-${p.manifest.id}`}
                            className="bc-starter-card"
                            onClick={() => void handlePreparePluginRun(p)}
                          >
                            <div className="bc-starter-card-left">
                              <span className="bc-starter-icon" aria-hidden="true">
                                {p.icon_url ? (
                                  <img src={p.icon_url} alt="" />
                                ) : (
                                  <IconModules size={18} />
                                )}
                              </span>
                              <div style={{ minWidth: 0 }}>
                                <div className="bc-starter-title">{p.manifest.name}</div>
                                <div className="bc-starter-desc">
                                  {p.has_custom_ui
                                    ? "Interactive window + cohort hot-spots"
                                    : "Containerized analysis · charts & CSV"}
                                </div>
                              </div>
                            </div>
                            <IconWindowOpen size={16} />
                          </button>
                        ))}
                      </div>
                    </div>
                  )}

                  <div className="bc-prompt-pills">
                    {SUGGESTED_PROMPTS.map((prompt) => (
                      <button
                        key={prompt}
                        type="button"
                        className="bc-prompt-pill"
                        onClick={() => void submitChatPrompt(prompt)}
                      >
                        {prompt}
                      </button>
                    ))}
                  </div>
                </div>
              ) : (
                <div className="bc-chat-column">
                  {(() => {
                    const toolResultByCallId = new Map<string, SessionEvent>();
                    for (const ev of sessionEvents) {
                      if (ev.kind === "tool_result") {
                        const callId = ev.metadata?.["tool_call_id"];
                        if (typeof callId === "string" && callId) {
                          toolResultByCallId.set(callId, ev);
                        }
                      }
                    }
                    const renderAssistantAvatar = (streaming: boolean) => (
                      <span
                        className="bc-msg-avatar"
                        data-streaming={String(streaming)}
                        title={
                          llmConfig?.configured
                            ? activeProviderLabel
                            : "BioCompass Agent"
                        }
                      >
                        {llmConfig?.configured && llmConfig.active?.provider_id ? (
                          <ProviderLogo
                            provider={llmConfig.active.provider_id}
                            size={16}
                          />
                        ) : streaming ? (
                          <RunningCompass size={17} />
                        ) : (
                          <CompassMark size={17} />
                        )}
                      </span>
                    );

                    return sessionEvents.map((ev) => {
                      if (ev.kind === "user" || ev.kind === "user_message") {
                        const attachedImgs = session
                          ? userImagesByPrompt[`${session.session_id}:${ev.content}`] ?? []
                          : [];
                        return (
                          <div key={ev.event_id} className="bc-msg-user">
                            <div>{renderUserPromptTokens(ev.content)}</div>
                            {attachedImgs.length > 0 && (
                              <div className="bc-msg-user-thumbs">
                                {attachedImgs.map((img, i) => (
                                  <img
                                    key={i}
                                    src={img.src}
                                    alt={img.alt}
                                    className="bc-chat-thumb"
                                    onClick={() => setLightboxImage(img)}
                                  />
                                ))}
                              </div>
                            )}
                          </div>
                        );
                      }

                      if (ev.kind === "tool_call") {
                        const callId =
                          typeof ev.metadata?.["id"] === "string"
                            ? ev.metadata["id"]
                            : "";
                        const args =
                          ev.metadata?.["arguments"] &&
                          typeof ev.metadata["arguments"] === "object"
                            ? (ev.metadata["arguments"] as Record<string, unknown>)
                            : {};
                        const pairedRes = callId ? toolResultByCallId.get(callId) : undefined;
                        let parsedResult: Record<string, unknown> | null = null;
                        if (pairedRes?.content) {
                          try {
                            parsedResult = JSON.parse(pairedRes.content) as Record<
                              string,
                              unknown
                            >;
                          } catch {
                            parsedResult = null;
                          }
                        }

                        const pluginIdArg =
                          typeof args["plugin_id"] === "string"
                            ? args["plugin_id"]
                            : typeof parsedResult?.["plugin_id"] === "string"
                              ? (parsedResult["plugin_id"] as string)
                              : "";
                        const targetPlugin = pluginIdArg
                          ? plugins.find((p) => p.manifest.id === pluginIdArg)
                          : undefined;

                        if (ev.content === "prepare_plugin_run") {
                          const quoteObj =
                            parsedResult?.["quote"] &&
                            typeof parsedResult["quote"] === "object"
                              ? (parsedResult["quote"] as RunQuote)
                              : null;
                          const matchedRun = quoteObj
                            ? runs.find((r) => r.run_id === quoteObj.run_id)
                            : undefined;
                          const isPending = Boolean(
                            quoteObj && pendingQuote?.run_id === quoteObj.run_id,
                          );
                          return (
                            <div
                              key={ev.event_id}
                              className="bc-chat-module-card"
                              data-testid="chat-module-card"
                            >
                              <div className="bc-chat-module-top">
                                <div className="bc-chat-module-title">
                                  <IconModules size={14} />
                                  <span>
                                    {parsedResult?.status === "submitted" ? "Submitted Run" : "Prepared Run"} ·{" "}
                                    {targetPlugin?.manifest.name ?? pluginIdArg}
                                  </span>
                                </div>
                                {quoteObj && typeof quoteObj.estimated_cost_act === "number" && (
                                  <span className="bc-badge bc-badge-accent mono">
                                    {formatMilliAct(quoteObj.estimated_cost_act)}
                                  </span>
                                )}
                              </div>
                              {typeof parsedResult?.message === "string" && (
                                <p className="bc-caption" role="status">{parsedResult.message}</p>
                              )}
                              <div className="bc-chat-module-actions">
                                {isPending && quoteObj && !requiresExternalProviderConsent(quoteObj) && (
                                  <button
                                    type="button"
                                    className="bc-btn bc-btn-primary bc-btn-sm"
                                    disabled={isConfirmingRun}
                                    onClick={() => handleConfirmPendingRun(quoteObj.run_id)}
                                  >
                                    {isConfirmingRun ? "Running…" : `Approve ${formatMilliAct(quoteObj.estimated_cost_act)} run`}
                                  </button>
                                )}
                                {isPending && quoteObj && requiresExternalProviderConsent(quoteObj) && (
                                  <span className="bc-badge bc-badge-warn">
                                    External provider consent required in modal
                                  </span>
                                )}
                                {matchedRun && (
                                  <span
                                    className={`bc-badge ${
                                      matchedRun.status === "succeeded"
                                        ? "bc-badge-success"
                                        : matchedRun.status === "failed"
                                          ? "bc-badge-danger"
                                          : "bc-badge-neutral"
                                    }`}
                                  >
                                    {matchedRun.status}
                                  </span>
                                )}
                                {targetPlugin && (
                                  <>
                                    <button
                                      type="button"
                                      className="bc-btn bc-btn-ghost bc-btn-sm"
                                      onClick={() =>
                                        dockPluginInSplitView(
                                          targetPlugin,
                                          matchedRun ? "results" : "run",
                                          matchedRun?.run_id,
                                        )
                                      }
                                    >
                                      <IconSplitView size={12} />
                                      <span>
                                        {matchedRun ? "Results Split View" : "Split View"}
                                      </span>
                                    </button>
                                    <button
                                      type="button"
                                      className="bc-btn bc-btn-ghost bc-btn-sm"
                                      onClick={() =>
                                        openOrFocusModuleWindow(
                                          targetPlugin,
                                          matchedRun ? "results" : "run",
                                          matchedRun?.run_id,
                                        )
                                      }
                                    >
                                      Open Module
                                    </button>
                                  </>
                                )}
                              </div>
                            </div>
                          );
                        }

                        if (ev.content === "update_module_draft") {
                          const updatedKeys = Array.isArray(parsedResult?.["updated_keys"])
                            ? (parsedResult["updated_keys"] as string[])
                            : Object.keys(
                                (args["params"] as Record<string, unknown> | undefined) ?? {},
                              );
                          return (
                            <div
                              key={ev.event_id}
                              className="bc-chat-module-card"
                              data-testid="chat-module-card"
                            >
                              <div className="bc-chat-module-top">
                                <div className="bc-chat-module-title">
                                  <IconPencil size={13} />
                                  <span>
                                    Updated{" "}
                                    {targetPlugin?.manifest.name ?? pluginIdArg}
                                  </span>
                                </div>
                                {updatedKeys.length > 0 && (
                                  <span className="bc-caption mono">
                                    {updatedKeys.join(", ")}
                                  </span>
                                )}
                              </div>
                              {targetPlugin && (
                                <div className="bc-chat-module-actions">
                                  <button
                                    type="button"
                                    className="bc-btn bc-btn-ghost bc-btn-sm"
                                    onClick={() => dockPluginInSplitView(targetPlugin, "ui")}
                                  >
                                    <IconSplitView size={12} />
                                    <span>Split View</span>
                                  </button>
                                  <button
                                    type="button"
                                    className="bc-btn bc-btn-ghost bc-btn-sm"
                                    onClick={() =>
                                      openOrFocusModuleWindow(targetPlugin, "ui")
                                    }
                                  >
                                    Open Module
                                  </button>
                                </div>
                              )}
                            </div>
                          );
                        }

                        if (ev.content === "query_catalog") {
                          return (
                            <div key={ev.event_id} className="bc-chat-module-pill">
                              <IconDatabase size={12} />
                              <span>Queried HLA Data Catalog</span>
                              <button
                                type="button"
                                className="bc-chat-module-pill-btn"
                                onClick={() => navigateToPage("catalogs")}
                              >
                                Open Data
                              </button>
                            </div>
                          );
                        }

                        return (
                          <div key={ev.event_id} className="bc-chat-module-pill mono">
                            <span>Tool · {ev.content}</span>
                            {targetPlugin && (
                              <button
                                type="button"
                                className="bc-chat-module-pill-btn"
                                onClick={() => dockPluginInSplitView(targetPlugin)}
                              >
                                Open {targetPlugin.manifest.name}
                              </button>
                            )}
                          </div>
                        );
                      }

                      if (ev.kind === "assistant" || ev.kind === "assistant_message") {
                        const persistedThinking =
                          typeof ev.metadata?.["thinking"] === "string"
                            ? ev.metadata["thinking"]
                            : "";
                        return (
                          <div key={ev.event_id} className="bc-msg-assistant">
                            {renderAssistantAvatar(false)}
                            <div className="bc-msg-body">
                              {persistedThinking && (
                                <ThinkingBlock thinking={persistedThinking} streaming={false} />
                              )}
                              <ChatMarkdown
                                content={ev.content}
                                canInsertNotebook={Boolean(notebookPlugin)}
                                onInsertCode={handleInsertCodeIntoNotebook}
                                onOpenImage={(src, alt) => setLightboxImage({ src, alt })}
                              />
                            </div>
                          </div>
                        );
                      }
                      return null;
                    });
                  })()}

                  {pendingUserPrompt &&
                    sessionEvents[sessionEvents.length - 1]?.content !==
                      pendingUserPrompt && (
                      <div className="bc-msg-user">
                        <div>{renderUserPromptTokens(pendingUserPrompt)}</div>
                        {session &&
                          (
                            userImagesByPrompt[
                              `${session.session_id}:${pendingUserPrompt}`
                            ] ?? []
                          ).map((img, i) => (
                            <img
                              key={i}
                              src={img.src}
                              alt={img.alt}
                              className="bc-chat-thumb"
                              onClick={() => setLightboxImage(img)}
                            />
                          ))}
                      </div>
                    )}

                  {liveToolNames.map((toolName) => (
                    <div key={toolName} className="bc-msg-tool mono">
                      Running tool · {toolName}…
                    </div>
                  ))}

                  {(streamingThinking || streamingText || chatBusy) && (
                    <div className="bc-msg-assistant">
                      <span
                        className="bc-msg-avatar"
                        data-streaming="true"
                        title={
                          llmConfig?.configured
                            ? activeProviderLabel
                            : "BioCompass Agent"
                        }
                      >
                        {llmConfig?.configured && llmConfig.active?.provider_id ? (
                          <ProviderLogo
                            provider={llmConfig.active.provider_id}
                            size={16}
                          />
                        ) : (
                          <RunningCompass size={17} />
                        )}
                      </span>
                      <div className="bc-msg-body">
                        {streamingThinking && (
                          <ThinkingBlock
                            thinking={streamingThinking}
                            streaming={chatBusy && !streamingText}
                          />
                        )}
                        {streamingText ? (
                          <ChatMarkdown
                            content={streamingText}
                            canInsertNotebook={Boolean(notebookPlugin)}
                            onInsertCode={handleInsertCodeIntoNotebook}
                            onOpenImage={(src, alt) => setLightboxImage({ src, alt })}
                          />
                        ) : (
                          !streamingThinking && <span className="bc-caption">Thinking…</span>
                        )}
                      </div>
                    </div>
                  )}
                </div>
              )}
            </div>

            {!isEmptyChat && (
              <div className="bc-composer-wrap">{renderComposer()}</div>
            )}
          </div>

          {dockedWindow && dockedPlugin && (
            <>
              <div
                className="bc-split-resizer"
                role="separator"
                aria-orientation="vertical"
                aria-label="Resize split screen canvas"
                title="Drag to resize Chat / Canvas split"
                onPointerDown={handleSplitPointerDown}
                onPointerMove={handleSplitPointerMove}
                onPointerUp={handleSplitPointerUp}
              />
              <aside
                className="bc-canvas-dock"
                data-testid="canvas-dock-pane"
                style={{ width: `${dockWidthPct}%` }}
              >
                <ModuleFloatingWindow
                  key={`${dockedWindow.id}@${dockedPlugin.manifest.version}`}
                  win={dockedWindow}
                  plugin={dockedPlugin}
                  isFocused={true}
                  runs={dockedRuns}
                  runCount={dockedRuns.length}
                  activeGrant={activeGrant}
                  defaultCatalogId={defaultCatalogId}
                  releaseVersion={activePreviewCatalog?.release_version}
                  catalogFacets={defaultCatalogFacets}
                  sessionId={session?.session_id}
                  theme={darkTheme ? "dark" : "light"}
                  pendingQuote={
                    pendingQuote?.plugin_id === dockedPlugin.manifest.id && pendingQuote.plugin_version === dockedPlugin.manifest.version
                      ? pendingQuote
                      : null
                  }
                  pendingQuoteParams={
                    pendingQuote?.plugin_id === dockedPlugin.manifest.id && pendingQuote.plugin_version === dockedPlugin.manifest.version
                      ? pendingQuoteParams
                      : null
                  }
                  isConfirmingRun={
                    isConfirmingRun &&
                    pendingQuote?.plugin_id === dockedPlugin.manifest.id && pendingQuote.plugin_version === dockedPlugin.manifest.version
                  }
                  liveLogs={liveLogs}
                  verifyReport={verifyReport}
                  onFocus={() => handleFocusWindow(dockedWindow.id)}
                  onMoveResize={(patch) =>
                    handleMoveResizeWindow(dockedWindow.id, patch)
                  }
                  onMinimize={() => handleToggleMinimize(dockedWindow.id)}
                  onMaximize={() => handleToggleMaximize(dockedWindow.id)}
                  onDock={() => handleToggleDock(dockedWindow.id)}
                  onClose={() => handleCloseWindow(dockedWindow.id)}
                  onChangeTab={(tab, runId) =>
                    handleChangeWindowTab(dockedWindow.id, tab, runId)
                  }
                  onUpdateParam={(key, val) =>
                    handleUpdateWindowParam(dockedWindow.id, key, val)
                  }
                  onPrepareRun={() =>
                    void handlePreparePluginRun(dockedPlugin, dockedWindow.params)
                  }
                  onRunApiTest={(params, offerId) =>
                    handlePreparePluginRun(dockedPlugin, params, {
                      stayOnActiveTab: true,
                      offerId,
                    })
                  }
                  onQuoteConfirmed={handleQuoteConfirmedFromConsent}
                  onQuotePrepared={handleQuotePrepared}
                  onSendToChat={handleSendModuleToChat}
                  onSyncDraftParams={handleSyncDraftParams}
                  onUpdateRun={updateRun}
                  onReverifyChain={(run, tamper) =>
                    void handleReverifyReceipt(run, tamper)
                  }
                />
              </aside>
            </>
          )}
          </div>
        )}

        {/* ------------------ PAGE 2: MODULES (FLOATING WINDOW LAUNCHER) ------------------ */}
        {activePage === "plugins" && (
          <div className="bc-page-scroll">
            <div className="bc-page-container">
              <div className="bc-page-header">
                <div>
                  <h1 className="bc-page-title">Modules</h1>
                  <p className="bc-page-subtitle">
                    Click any module to open it as a draggable floating window in your
                    workspace.
                  </p>
                </div>

                <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
                  <input
                    type="search"
                    className="bc-input"
                    data-testid="module-search"
                    style={{ minWidth: 230 }}
                    placeholder="Search name, type, or owner..."
                    value={moduleSearch}
                    onChange={(e) => setModuleSearch(e.target.value)}
                  />
                  <button
                    type="button"
                    className="bc-btn bc-btn-secondary"
                    onClick={() => openSettings("developer")}
                  >
                    <IconPlus size={14} />
                    <span>New Module</span>
                  </button>
                </div>
              </div>

              {health?.workspace_hub_url && (
                <WorkspaceModuleDownloads hub={health.workspace_hub_url} bearer={workspaceBearer}
                  onBearer={setWorkspaceBearer} onInstalled={openInstalledRelease} />
              )}

              {moduleUpdate && <p role="status" className="bc-caption">{moduleUpdate}</p>}
              <div className="bc-module-filter-bar">
                <div
                  className="bc-filter-pills"
                  role="tablist"
                  aria-label="Filter modules by execution environment"
                >
                  {(
                    [
                      { id: "all", label: "All", icon: null },
                      { id: "local", label: "Local", icon: <IconLaptop size={13} /> },
                      {
                        id: "alithea_cloud",
                        label: "Cloud",
                        icon: <IconAlitheaCloud size={15} />,
                      },
                      {
                        id: "froglet_node",
                        label: "Network",
                        icon: <IconFrogletNode size={13} />,
                      },
                    ] as const
                  ).map((item) => (
                    <button
                      key={item.id}
                      type="button"
                      role="tab"
                      aria-selected={moduleRuntimeFilter === item.id}
                      data-testid={`module-filter-${item.id}`}
                      data-active={String(moduleRuntimeFilter === item.id)}
                      className="bc-filter-pill"
                      onClick={() => setModuleRuntimeFilter(item.id)}
                    >
                      {item.icon}
                      <span>{item.label}</span>
                      <span className="bc-filter-pill-count mono">
                        {moduleRuntimeCounts[item.id]}
                      </span>
                    </button>
                  ))}
                </div>
              </div>

              {filteredPlugins.length === 0 ? (
                <div className="bc-empty-note">
                  <div>
                    No modules match the active filters
                    {moduleSearch.trim() ? ` for "${moduleSearch.trim()}"` : ""}.
                  </div>
                  {(moduleSearch.trim() || moduleRuntimeFilter !== "all") && (
                    <button
                      type="button"
                      className="bc-btn bc-btn-secondary"
                      style={{ marginTop: 10 }}
                      onClick={() => {
                        setModuleSearch("");
                        setModuleRuntimeFilter("all");
                      }}
                    >
                      Reset Filters
                    </button>
                  )}
                </div>
              ) : (
                <div className="bc-modules-grid">
                {filteredPlugins.map((p) => {
                  const isInstalling = installingPluginId === p.manifest.id;
                  const runtimeIcon =
                    p.execution_target === "alithea_cloud" ? (
                      <IconAlitheaCloud size={15} />
                    ) : p.execution_target === "froglet_node" ? (
                      <IconFrogletNode size={13} />
                    ) : (
                      <IconLaptop size={13} />
                    );

                  return (
                    <article
                      key={p.manifest.id}
                      data-testid={`plugin-card-${p.manifest.id}`}
                      className="bc-module-card"
                      onClick={() => {
                        if (p.installed) {
                          openOrFocusModuleWindow(p, p.has_custom_ui ? "ui" : "run");
                        } else if (!p.detached_versions?.length) {
                          void handleInstallPlugin(p);
                        }
                      }}
                    >
                      <div>
                        <div className="bc-module-card-top">
                          <span className="bc-badge bc-badge-neutral">
                            {p.manifest.category}
                          </span>
                          <span
                            className="bc-module-runtime-pill"
                            data-target={p.execution_target}
                            title={`Runs on ${p.execution_label}`}
                          >
                            {runtimeIcon}
                            <span>{p.execution_label}</span>
                          </span>
                        </div>

                        <div className="bc-module-header-row">
                          <div className="bc-module-icon" aria-hidden="true">
                            {p.icon_url ? (
                              <img src={p.icon_url} alt="" />
                            ) : (
                              <IconModules size={20} />
                            )}
                          </div>
                          <div style={{ minWidth: 0 }}>
                            <h2 className="bc-module-title">{p.manifest.name}</h2>
                            <div className="bc-module-author">
                              By <strong>{p.author}</strong>{" "}
                              <span aria-hidden="true">·</span>{" "}
                              <span className="mono">v{p.manifest.version}</span>
                            </div>
                          </div>
                        </div>

                        <p className="bc-module-desc">{p.manifest.description}</p>
                      </div>

                      <div className="bc-module-card-footer">
                        <span className="bc-caption mono">
                          {formatMilliAct(p.manifest.price_milli_act)} / run
                        </span>
                        {health?.mode === "local" && (
                          <WorkspaceModuleVersions plugin={p} bearer={workspaceBearer} onRemoved={afterModuleRemoval}
                            onRestored={(restored) => {
                              setModuleUpdate(`Restored ${restored.manifest.id} v${restored.manifest.version} from local files.`);
                              openInstalledRelease(restored);
                            }} />
                        )}
                        {p.installed ? (
                          <button
                            type="button"
                            className="bc-btn bc-btn-secondary"
                            onClick={(e) => {
                              e.stopPropagation();
                              openOrFocusModuleWindow(p, p.has_custom_ui ? "ui" : "run");
                            }}
                          >
                            <span>Open</span>
                            <IconWindowOpen size={14} />
                          </button>
                        ) : p.detached_versions?.length ? null : (
                          <button
                            type="button"
                            className="bc-btn bc-btn-primary"
                            disabled={Boolean(installingPluginId)}
                            onClick={(e) => {
                              e.stopPropagation();
                              void handleInstallPlugin(p);
                            }}
                          >
                            {isInstalling ? (
                              <>
                                <RunningCompass size={14} />
                                <span>Installing...</span>
                              </>
                            ) : (
                              <>
                                <IconDownload size={14} />
                                <span>Install</span>
                              </>
                            )}
                          </button>
                        )}
                      </div>
                    </article>
                  );
                })}
                </div>
              )}
            </div>
          </div>
        )}

        {/* ------------------ PAGE 3: RESULTS HISTORY ------------------ */}
        {activePage === "results" && (
          <div className="bc-page-scroll" data-testid="results-page">
            <div className="bc-page-container">
              <div className="bc-page-header">
                <div>
                  <h1 className="bc-page-title">Results</h1>
                  <p className="bc-page-subtitle">
                    Execution history, Froglet receipts, and output artifacts from past
                    module runs.
                  </p>
                </div>

                <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
                  {isCloudMode && principal && (
                    <SharedResultFiles key={approvalScope} orgId={principal.org_id}
                      canPublish={hasMinOrgRole(principal.role, "admin")}
                      open={sharedFilesScope === approvalScope}
                      onOpen={() => setSharedFilesScope(approvalScope)} onClose={() => setSharedFilesScope(null)} />
                  )}
                  <input
                    type="search"
                    className="bc-input"
                    placeholder="Filter results..."
                    value={resultsSearch}
                    onChange={(e) => setResultsSearch(e.target.value)}
                  />
                  <button
                    type="button"
                    className="bc-btn bc-btn-secondary"
                    onClick={() => navigateToPage("plugins")}
                  >
                    <IconModules size={14} />
                    <span>Browse Modules</span>
                  </button>
                </div>
              </div>

              {archivedRunId && (
                <SavedRunDialog key={archivedRunId} runId={archivedRunId} verifyReport={verifyReport}
                  onClose={() => setArchivedRunId(null)} onUpdate={updateRun}
                  onReverify={(run, tamper) => void handleReverifyReceipt(run, tamper)} />
              )}
              {health?.workspace_hub_url && !isCloudMode && (workspaceBearer || workspaceRead) && (
                <InstalledWorkspaceResults key={workspaceRead ?? workspaceBearer} bearer={workspaceBearer} handle={workspaceRead} search={resultsSearch} />
              )}
              {filteredRuns.length === 0 ? (
                <div className="bc-empty-note">
                  {runs.length === 0
                    ? ((workspaceBearer || workspaceRead) && health?.workspace_hub_url && !isCloudMode
                      ? "No module runs executed on this installation."
                      : "No module runs recorded yet. Open a module to run an analysis and inspect its results here.")
                    : ((workspaceBearer || workspaceRead) && health?.workspace_hub_url && !isCloudMode
                      ? `No locally executed runs match "${resultsSearch.trim()}".`
                      : `No runs match "${resultsSearch.trim()}".`)}
                </div>
              ) : (
                <div className="bc-runs-list">
                  {filteredRuns.map((r) => {
                    const runPlugin = plugins.find((p) => p.installed &&
                      p.manifest.id === r.plugin_id && p.manifest.version === r.plugin_version);
                    const badgeClass = r.receipt_verified
                      ? "bc-badge-success"
                      : r.status === "failed"
                        ? "bc-badge-danger"
                        : r.status === "running"
                          ? "bc-badge-accent"
                          : "bc-badge-neutral";
                    return (
                      <div key={r.run_id} className="bc-run-summary-card">
                        <div style={{ minWidth: 0 }}>
                          <div
                            style={{
                              display: "flex",
                              alignItems: "center",
                              flexWrap: "wrap",
                              gap: 8,
                            }}
                          >
                            <strong>{runPlugin?.manifest.name ?? r.plugin_id}</strong>
                            <span className="bc-caption mono">v{r.plugin_version}</span>
                            <span className={`bc-badge ${badgeClass}`}>
                              {r.receipt_verified ? "Verified" : runStatusLabel(r)}
                            </span>
                          </div>
                          <div className="bc-caption mono" style={{ marginTop: 4 }}>
                            {r.run_id.slice(0, 16)} ·{" "}
                            {new Date(r.created_at).toLocaleString()} ·{" "}
                            Quoted {formatMilliAct(r.quote.estimated_cost_act)} ·{" "}
                            {r.artifacts.length} output file(s)
                          </div>
                          <CapacityRequest run={r} onUpdate={updateRun} />
                          {r.quote.dataset_grant_summary && (
                            <div className="bc-caption" style={{ marginTop: 2 }}>
                              Grant: {r.quote.dataset_grant_summary}
                            </div>
                          )}
                          {r.error_message && (
                            <div
                              className="bc-caption"
                              style={{ marginTop: 2, color: "var(--bc-danger)" }}
                            >
                              {r.error_message}
                            </div>
                          )}
                        </div>
                        <div className="bc-run-summary-actions">
                            {isCloudMode && principal && r.org_id === principal.org_id
                              && hasMinOrgRole(principal.role, "admin") && r.status === "succeeded" && r.receipt_verified && (
                              <ShareResultButton key={approvalScope} run={r} />
                            )}
                            {isCloudMode && principal && r.org_id === principal.org_id &&
                              r.status === "succeeded" && r.receipt_verified && <ArchiveResult runId={r.run_id} />}
                            {runPlugin?.has_custom_ui && (
                              <button
                                type="button"
                                className="bc-btn bc-btn-ghost"
                                onClick={() => openOrFocusModuleWindow(runPlugin, "ui")}
                              >
                                <IconWindowOpen size={14} />
                                <span>Open App</span>
                              </button>
                            )}
                            <button
                              type="button"
                              className="bc-btn bc-btn-secondary"
                              data-testid={`open-results-${r.run_id}`}
                              onClick={() => runPlugin
                                ? openOrFocusModuleWindow(runPlugin, "results", r.run_id)
                                : setArchivedRunId(r.run_id)}
                            >
                              <IconWindowOpen size={14} />
                              <span>Open Results</span>
                            </button>
                        </div>
                      </div>
                    );
                  })}
                </div>
              )}
            </div>
          </div>
        )}

        {/* ------------------ PAGE 4: DATA CATALOGS ------------------ */}
        {activePage === "catalogs" && (
          <DataExplorerView
            mode={isCloudMode ? "cloud" : "local"}
            uploadScope={principal?.user_id ? `${principal.org_id}:${principal.user_id}` : undefined}
            defaultNamespace={
              isCloudMode
                ? (principal?.org_namespace ?? "")
                : (principal?.org_id ?? "alithea")
            }
            catalogs={catalogs}
            grants={grantsForActiveCatalog}
            grantUsage={grantUsage}
            activeGrant={activeGrant}
            selectedCatalogIds={selectedCatalogIds}
            activeCatalogId={defaultCatalogId}
            catalogQuery={effectiveCatalogQuery}
            catalogLoading={catalogLoading}
            catalogResult={catalogResult}
            plugins={plugins}
            savedViews={savedViews}
            onSelectPreviewCatalog={(catId) => setPreviewCatalogId(catId)}
            onToggleCatalogSelection={toggleCatalogSelection}
            onGrantChange={(gid) => void handleGrantChange(gid)}
            onUpdateCatalogQuery={(next) => setCatalogQuery(next)}
            onCreateSavedView={async (name, query) => {
              if (!activeGrant) throw new Error("Select a data grant before saving this view.");
              const rec = await createSavedView({
                name,
                catalog_id: defaultCatalogId,
                grant_id: activeGrant.grant_id,
                release_version: activePreviewCatalog?.release_version,
                query: sanitizeQueryForCatalog(query, activePreviewCatalog),
              });
              setSavedViews((prev) => [rec, ...prev]);
              return rec;
            }}
            onDeleteSavedView={async (viewId) => {
              await deleteSavedView(viewId);
              setSavedViews((prev) => prev.filter((v) => v.view_id !== viewId));
            }}
            onVerifiedSavedView={(viewId, report) => {
              setSavedViews((prev) =>
                prev.map((v) =>
                  v.view_id === viewId
                    ? {
                        ...v,
                        last_status: report.status,
                        last_verified_at: report.verified_at,
                      }
                    : v,
                ),
              );
            }}
            onRefreshCatalogs={refreshWorkspaceData}
            onSendToChat={({ catalogId, catalogName, rows, query }) => {
              if (!selectedCatalogIds.includes(catalogId)) {
                toggleCatalogSelection(catalogId);
              }
              const uniqueAlleles = Array.from(
                new Set(rows.map((r: PeptideRecord) => r.hla_allele).filter(Boolean)),
              );
              const uniqueGenes = Array.from(
                new Set(rows.map((r: PeptideRecord) => r.gene_symbol).filter(Boolean)),
              );
              const ic50Vals = rows
                .map((r: PeptideRecord) => r.affinity_nm)
                .filter((n) => Number.isFinite(n));
              const minIc50 = ic50Vals.length > 0 ? Math.min(...ic50Vals) : 0;
              const maxIc50 = ic50Vals.length > 0 ? Math.max(...ic50Vals) : 0;
              const previewParts = [
                uniqueAlleles.slice(0, 3).join(", "),
                uniqueGenes.slice(0, 4).join(", "),
                ic50Vals.length > 0
                  ? `IC50 ${minIc50.toFixed(1)}–${maxIc50.toFixed(1)} nM`
                  : "",
              ].filter(Boolean);

              setAttachedModuleContext({
                pluginId: `@${catalogId}`,
                label: `${catalogName} (${rows.length} selected)`,
                preview: previewParts.join(" · "),
                defaultMessage: `Analyze the ${rows.length} selected record(s) from @${catalogId}.`,
                context: {
                  source: "data_explorer_selection",
                  catalog_id: catalogId,
                  access_level: activeGrant?.level ?? "rows",
                  active_filters: query,
                  selected_count: rows.length,
                  selected_rows: rows.slice(0, 50),
                },
              });
              navigateToPage("workbench");
              window.setTimeout(() => {
                composerTextareaRef.current?.focus();
              }, 40);
            }}
            onRunInModule={(plugin, derivedParams) => {
              handleSyncDraftParams(plugin.manifest.id, derivedParams);
              openOrFocusModuleWindow(plugin, plugin.has_custom_ui ? "ui" : "run");
            }}
          />
        )}

        {/* ------------------ PAGE 5: ORGANIZATION ADMIN (CLOUD) ------------------ */}
        {activePage === "org-admin" && principal && (
          <OrgAdminView
            principal={principal}
            catalogs={catalogs}
            grants={grants}
            grantUsage={grantUsage}
            ledger={ledger}
            onRefreshWorkspace={refreshWorkspaceData}
            onOpenModuleOffer={openOfferedModule}
            initialTab={federatedOfferLanding ? "module-offers" : "overview"}
          />
        )}

        {/* ------------------ PAGE 6: CONTROL TOWER (PLATFORM ADMIN) ------------------ */}
        {activePage === "control-tower" && principal?.is_platform_admin && (
          <ControlTowerView
            principal={principal}
            onRefreshWorkspace={refreshWorkspaceData}
          />
        )}
      </main>

      {/* ==================== FLOATING MODULE WINDOWS + TRAY ==================== */}
      <FloatingWindowsLayer
        windows={windows}
        plugins={plugins}
        runs={runs}
        activeGrant={activeGrant}
        defaultCatalogId={defaultCatalogId}
        releaseVersion={activePreviewCatalog?.release_version}
        catalogFacets={defaultCatalogFacets}
        sessionId={session?.session_id}
        theme={darkTheme ? "dark" : "light"}
        pendingQuote={pendingQuote}
        pendingQuoteParams={pendingQuoteParams}
        isConfirmingRun={isConfirmingRun}
        liveLogs={liveLogs}
        verifyReport={verifyReport}
        onFocusWindow={handleFocusWindow}
        onMoveResizeWindow={handleMoveResizeWindow}
        onToggleMinimize={handleToggleMinimize}
        onToggleMaximize={handleToggleMaximize}
        onToggleDock={handleToggleDock}
        onCloseWindow={handleCloseWindow}
        onChangeTab={handleChangeWindowTab}
        onUpdateParam={handleUpdateWindowParam}
        onPreparePluginRun={(plugin, params) => void handlePreparePluginRun(plugin, params)}
        onRunApiTest={(plugin, params, offerId) =>
          handlePreparePluginRun(plugin, params, { stayOnActiveTab: true, offerId })
        }
        onQuoteConfirmed={handleQuoteConfirmedFromConsent}
        onQuotePrepared={handleQuotePrepared}
        onSendToChat={handleSendModuleToChat}
        onSyncDraftParams={handleSyncDraftParams}
        onUpdateRun={updateRun}
        onReverifyChain={(run, tamper) => void handleReverifyReceipt(run, tamper)}
      />

      {/* ==================== SETTINGS MODAL ==================== */}
      <SettingsModal
        open={settingsOpen}
        initialSection={settingsSection}
        onProfileUpdated={setProfile}
        darkTheme={darkTheme}
        onSetDarkTheme={setDarkTheme}
        health={health}
        workspaceBearer={workspaceBearer}
        onWorkspaceBearer={setWorkspaceBearer}
        initialLlmConfig={llmConfig}
        grants={grants}
        activeGrantId={activeGrant?.grant_id ?? ""}
        onSelectGrant={(gid) => void handleGrantChange(gid)}
        ledger={ledger}
        onScaffoldModule={handleScaffoldModule}
        scaffoldOutput={scaffoldOutput}
        onOpenPluginInIde={handleOpenPluginInIde}
        onLlmConfigChanged={async (updated) => {
          const [freshHealth, freshLlm] = await Promise.all([
            fetchHealth(),
            updated ? Promise.resolve(updated) : fetchLlmConfig(),
          ]);
          setHealth(freshHealth);
          if (freshHealth.marketplace_status) {
            setMarketplaceStatus(freshHealth.marketplace_status);
          }
          setLlmConfig(freshLlm);
        }}
        marketplaceStatus={effectiveMarketplaceStatus}
        onMarketplaceStatusChanged={(updated) => {
          setMarketplaceStatus(updated);
          setHealth((prev) =>
            prev
              ? {
                  ...prev,
                  marketplace_prompt_required: updated.prompt_required,
                  marketplace_status: updated,
                }
              : prev,
          );
        }}
        onClose={() => setSettingsOpen(false)}
      />

      {showMarketplaceConsentModal && effectiveMarketplaceStatus && (
        <MarketplaceConsentModal
          status={effectiveMarketplaceStatus}
          onResolved={(updated) => {
            setMarketplaceStatus(updated);
            setMarketplaceModalDismissed(false);
            setHealth((prev) =>
              prev
                ? {
                    ...prev,
                    marketplace_prompt_required: updated.prompt_required,
                    marketplace_status: updated,
                  }
                : prev,
            );
          }}
          onDismissForSession={() => setMarketplaceModalDismissed(true)}
        />
      )}

      {pendingQuote && <PaidRunConsent key={pendingQuote.run_id} quote={pendingQuote}
        params={pendingQuoteParams} busy={isConfirmingRun} error={paidQuotes[0]?.error}
        onApprove={handleConfirmPendingRun} onDismiss={declinePaidRun} />}

      {lightboxImage && (
        <div
          className="bc-modal-backdrop"
          onMouseDown={(e) => {
            if (e.target === e.currentTarget) setLightboxImage(null);
          }}
        >
          <div className="bc-lightbox" role="dialog" aria-modal="true" aria-label="Image Preview">
            <div className="bc-lightbox-bar">
              <span className="bc-caption">{lightboxImage.alt ?? "Figure preview"}</span>
              <div style={{ display: "flex", gap: 8 }}>
                <a
                  href={lightboxImage.src}
                  download={(lightboxImage.alt || "biocompass-figure").replace(/[^a-z0-9_-]+/gi, "-") + ".png"}
                  className="bc-btn bc-btn-ghost bc-btn-sm"
                >
                  <IconDownload size={13} />
                  <span>Save</span>
                </a>
                <button
                  type="button"
                  className="bc-win-ctrl"
                  aria-label="Close image preview"
                  onClick={() => setLightboxImage(null)}
                >
                  <IconClose size={14} />
                </button>
              </div>
            </div>
            <img
              src={lightboxImage.src}
              alt={lightboxImage.alt ?? "Figure preview"}
              className="bc-lightbox-img"
            />
          </div>
        </div>
      )}
    </div>
  );
}
