import type {
  HealthStatus,
  MarketplaceStatus,
  MarketplaceSyncState,
  RunQuote,
} from "./types";

export interface ExternalProviderConsentInfo {
  providerLabel: string;
  servingOrgId: string | null;
  nodeId: string;
  notice: string;
}

/**
 * Returns true whenever a prepared quote routes execution to an external network provider
 * (`[A2]`), requiring explicit buyer data-boundary consent.
 */
export function requiresExternalProviderConsent(
  quote: Pick<
    RunQuote,
    "serving_org_id" | "external_provider_org" | "external_provider_notice"
  > | null | undefined,
): boolean {
  if (!quote) return false;
  return Boolean(
    quote.external_provider_org?.trim() ||
      quote.serving_org_id?.trim() ||
      quote.external_provider_notice?.trim(),
  );
}

/**
 * Any paid run OR any external provider run (even €0.00) MUST require explicit consent.
 */
export function shouldPromptQuoteConsent(
  quote: Pick<
    RunQuote,
    | "estimated_cost_milli_eur"
    | "serving_org_id"
    | "external_provider_org"
    | "external_provider_notice"
  >,
): boolean {
  return quote.estimated_cost_milli_eur !== 0 || requiresExternalProviderConsent(quote);
}

export function getExternalProviderConsentInfo(
  quote: Pick<
    RunQuote,
    | "target_node_id"
    | "serving_org_id"
    | "external_provider_org"
    | "external_provider_notice"
  >,
): ExternalProviderConsentInfo | null {
  if (!requiresExternalProviderConsent(quote)) return null;
  const providerLabel =
    quote.external_provider_org?.trim() ||
    quote.serving_org_id?.trim() ||
    "External Provider";
  const nodeId = quote.target_node_id || "external-provider";
  const notice =
    quote.external_provider_notice?.trim() ||
    `This run will send your parameters and uploaded inputs to an external provider node (${nodeId}) operated by ${providerLabel}. Protected or compute_only Alithea catalogs are never materialized to external provider nodes.`;
  return {
    providerLabel,
    servingOrgId: quote.serving_org_id?.trim() || null,
    nodeId,
    notice,
  };
}

export interface MarketplaceSyncBadge {
  state: MarketplaceSyncState;
  label: string;
  tone: "success" | "warn" | "neutral" | "danger";
}

export function formatMarketplaceSyncBadge(
  status: MarketplaceStatus | null | undefined,
): MarketplaceSyncBadge {
  if (!status) {
    return { state: "disconnected", label: "Not connected", tone: "neutral" };
  }
  switch (status.state) {
    case "air-gapped":
      return { state: "air-gapped", label: "Air-gapped", tone: "neutral" };
    case "connected":
      return { state: "connected", label: "Connected", tone: "success" };
    case "stale":
      return { state: "stale", label: "Stale", tone: "warn" };
    case "disconnected":
    default:
      return {
        state: "disconnected",
        label: status.consent === "declined" ? "Offline (Declined)" : "Not connected",
        tone: "neutral",
      };
  }
}

export function shouldShowMarketplaceConsentModal(
  health: Pick<HealthStatus, "mode"> | null | undefined,
  status: MarketplaceStatus | null | undefined,
  dismissedInSession: boolean,
): boolean {
  if (dismissedInSession) return false;
  if (health?.mode !== "local") return false;
  if (!status || status.airgapped) return false;
  return Boolean(status.prompt_required);
}
