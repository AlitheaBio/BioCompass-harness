import type {
  HealthStatus,
  LedgerState,
  ModuleRunsIn,
  ModuleSource,
  PluginSummary,
  RunQuote,
} from "./types";

function sanitizeHandoffUrl(raw: string): string | null {
  const trimmed = raw.trim();
  if (!trimmed || /[\u0000-\u001f\u007f]/.test(trimmed)) return null;
  if (trimmed.startsWith("/")) {
    return trimmed.startsWith("//") ? null : trimmed;
  }
  try {
    const parsed = new URL(trimmed);
    return parsed.protocol === "https:" || parsed.protocol === "http:" ? parsed.toString() : null;
  } catch {
    return null;
  }
}

export interface ModuleCardActionModel {
  sourceLabel: string;
  runsInLabel: string;
  source: ModuleSource;
  runsIn: ModuleRunsIn;
  /** Open is ALWAYS available once a module is listed (uses workspace U1 verified cache). */
  canOpen: boolean;
  /** Install copies the container locally; shown ONLY when the offer allows download and is not yet installed locally. */
  canInstall: boolean;
  /** Remove undoes Install (shown only when locally installed removable versions exist). */
  canRemove: boolean;
  /** Network run-only offers show Open + "Runs at <provider>" badge with run handoff until C3. */
  isNetworkRunOnly: boolean;
  runsAtProviderLabel: string | null;
  safeHandoffUrl: string | null;
}

export function inferModuleSource(plugin: Pick<PluginSummary, "source" | "execution_target">): ModuleSource {
  if (plugin.source) return plugin.source;
  if (plugin.execution_target === "local") return "workspace";
  if (plugin.execution_target === "froglet_node") return "network_provider";
  return "org_cloud";
}

export function inferModuleRunsIn(plugin: Pick<PluginSummary, "runs_in" | "execution_target">): ModuleRunsIn {
  if (plugin.runs_in) return plugin.runs_in;
  if (plugin.execution_target === "local") return "local";
  if (plugin.execution_target === "froglet_node") return "provider_edge";
  return "cloud";
}

export function formatModuleSourceLabel(
  plugin: Pick<PluginSummary, "source" | "execution_target" | "provider_label">,
): string {
  const source = inferModuleSource(plugin);
  switch (source) {
    case "workspace":
      return "This workspace";
    case "org_cloud":
      return "My org cloud";
    case "network_provider":
      return plugin.provider_label?.trim()
        ? `Network · ${plugin.provider_label.trim()}`
        : "Network provider";
  }
}

export function formatModuleRunsInLabel(
  plugin: Pick<PluginSummary, "runs_in" | "execution_target" | "provider_label">,
): string {
  const runsIn = inferModuleRunsIn(plugin);
  const provider = plugin.provider_label?.trim();
  switch (runsIn) {
    case "local":
      return "Local";
    case "cloud":
      return "Cloud";
    case "provider_edge":
      return provider ? `Runs at ${provider}` : "Provider node";
    case "network_provider":
      return provider ? `Runs at ${provider}` : "Provider node";
  }
}

export function resolveModuleCardActions(
  plugin: Pick<
    PluginSummary,
    | "source"
    | "runs_in"
    | "execution_target"
    | "provider_label"
    | "can_install"
    | "installed_locally"
    | "installed"
    | "removable_versions"
    | "detached_versions"
    | "launch_url"
  >,
  isLocalWorkspace: boolean,
): ModuleCardActionModel {
  const source = inferModuleSource(plugin);
  const runsIn = inferModuleRunsIn(plugin);
  const installedLocally =
    plugin.installed_locally ?? (plugin.installed && runsIn === "local");
  const isDetachedOnly = !plugin.installed && Boolean(plugin.detached_versions?.length);
  const canOpen = !isDetachedOnly;
  const canInstall =
    isLocalWorkspace &&
    !isDetachedOnly &&
    Boolean(plugin.can_install) &&
    !installedLocally;
  const canRemove =
    isLocalWorkspace &&
    installedLocally &&
    (plugin.removable_versions?.length ?? 0) > 0;
  const isNetworkRunOnly =
    (source === "network_provider" || runsIn === "network_provider") &&
    !installedLocally &&
    !plugin.can_install;
  const provider = plugin.provider_label?.trim() || null;
  const runsAtProviderLabel =
    runsIn === "network_provider" || runsIn === "provider_edge"
      ? provider
        ? `Runs at ${provider}`
        : "Provider node"
      : null;
  const safeHandoffUrl = plugin.launch_url
    ? sanitizeHandoffUrl(plugin.launch_url)
    : null;

  return {
    sourceLabel: formatModuleSourceLabel(plugin),
    runsInLabel: formatModuleRunsInLabel(plugin),
    source,
    runsIn,
    canOpen,
    canInstall,
    canRemove,
    isNetworkRunOnly,
    runsAtProviderLabel,
    safeHandoffUrl,
  };
}

/**
 * Plan Default #3: the laptop's local play ledger is NEVER labelled euros, whereas hosted cloud
 * and network quotes/ledgers display euros to the cent (`€X.XX`).
 */
export function isLocalPlayLedgerContext(
  health: Pick<HealthStatus, "mode"> | null | undefined,
  ledger?: Pick<LedgerState, "is_local_play_ledger"> | null,
  quote?: Pick<RunQuote, "serving_org_id" | "external_provider_org"> | null,
): boolean {
  if (quote && (quote.serving_org_id || quote.external_provider_org)) {
    return false;
  }
  if (ledger?.is_local_play_ledger === true) {
    return true;
  }
  return health?.mode === "local";
}
