import type {
  ModuleRunsIn,
  ModuleSource,
  PluginSummary,
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
  /** Remove for an Added (subscribed) network module: unsubscribes the offer. */
  canUnsubscribe: boolean;
  /** Offer ID to unsubscribe when `canUnsubscribe`. */
  subscriptionId: string | null;
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
  if (plugin.execution_target === "froglet_node") return "network_provider";
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
  > & Partial<Pick<PluginSummary, "marketplace_offer_id" | "manifest">>,
  isLocalWorkspace: boolean,
  subscribedOfferIds: ReadonlySet<string> = new Set(),
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
    runsIn === "network_provider"
      ? provider
        ? `Runs at ${provider}`
        : "Provider node"
      : null;
  const safeHandoffUrl = plugin.launch_url
    ? sanitizeHandoffUrl(plugin.launch_url)
    : null;
  const subscriptionId =
    (plugin.marketplace_offer_id && subscribedOfferIds.has(plugin.marketplace_offer_id)
      ? plugin.marketplace_offer_id
      : null) ??
    (plugin.manifest?.id && subscribedOfferIds.has(plugin.manifest.id) ? plugin.manifest.id : null);
  const canUnsubscribe = isLocalWorkspace && source === "network_provider" && subscriptionId !== null;

  return {
    sourceLabel: formatModuleSourceLabel(plugin),
    runsInLabel: formatModuleRunsInLabel(plugin),
    source,
    runsIn,
    canOpen,
    canInstall,
    canRemove,
    isNetworkRunOnly,
    canUnsubscribe,
    subscriptionId,
    runsAtProviderLabel,
    safeHandoffUrl,
  };
}

/**
 * U-4: a network run-only module's UI opens in this workspace, but its runs execute at the
 * provider (brokered runs arrive with C3). Returns the explanation shown to the user and sent
 * back to the module when its UI asks to run, or `null` when the module runs here.
 */
export function networkRunHandoff(
  plugin: Pick<
    PluginSummary,
    | "source"
    | "runs_in"
    | "execution_target"
    | "provider_label"
    | "can_install"
    | "installed_locally"
    | "installed"
    | "launch_url"
  >,
): { message: string; handoffUrl: string | null } | null {
  const source = inferModuleSource(plugin);
  const runsIn = inferModuleRunsIn(plugin);
  const installedLocally = plugin.installed_locally ?? (plugin.installed && runsIn === "local");
  if (!(source === "network_provider" || runsIn === "network_provider") || installedLocally) {
    return null;
  }
  const provider = plugin.provider_label?.trim() || "its provider";
  const handoffUrl = plugin.launch_url ? sanitizeHandoffUrl(plugin.launch_url) : null;
  return {
    message:
      `This module runs at ${provider}. Runs cannot be started from this workspace yet; ` +
      (handoffUrl ? "continue at the provider to run it." : "ask the provider to run it."),
    handoffUrl,
  };
}
