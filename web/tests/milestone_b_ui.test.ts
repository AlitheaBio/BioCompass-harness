import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import {
  formatMarketplaceSyncBadge,
  getExternalProviderConsentInfo,
  requiresExternalProviderConsent,
  shouldPromptQuoteConsent,
  shouldShowMarketplaceConsentModal,
} from "../src/milestoneBPolicy.ts";
import type { MarketplaceStatus } from "../src/types.ts";

const here = dirname(fileURLToPath(import.meta.url));
const webSrc = resolve(here, "../src");

function readSrc(relPath: string): string {
  return readFileSync(resolve(webSrc, relPath), "utf8");
}

function makeMarketplaceStatus(overrides: Partial<MarketplaceStatus> = {}): MarketplaceStatus {
  return {
    state: "disconnected",
    consent: "unset",
    consent_source: "unset",
    prompt_required: true,
    airgapped: false,
    hub_url: "https://hub.biocompass.alithea.bio",
    default_hub_url: "https://hub.biocompass.alithea.bio",
    feed_offer_count: 0,
    ...overrides,
  };
}

test("[A1] first-run marketplace consent modal shows only in non-airgapped local mode when unset", () => {
  const unsetLocal = makeMarketplaceStatus();
  assert.equal(shouldShowMarketplaceConsentModal({ mode: "local" }, unsetLocal, false), true);
  assert.equal(shouldShowMarketplaceConsentModal({ mode: "local" }, unsetLocal, true), false);

  const airgapped = makeMarketplaceStatus({
    state: "air-gapped",
    airgapped: true,
    prompt_required: false,
    consent: "declined",
  });
  assert.equal(shouldShowMarketplaceConsentModal({ mode: "local" }, airgapped, false), false);

  const declined = makeMarketplaceStatus({
    state: "disconnected",
    consent: "declined",
    prompt_required: false,
  });
  assert.equal(shouldShowMarketplaceConsentModal({ mode: "local" }, declined, false), false);

  assert.equal(shouldShowMarketplaceConsentModal({ mode: "cloud-web" }, unsetLocal, false), false);
});

test("[DEV-07] marketplace sync status badge formats air-gapped, disconnected, connected, and stale states", () => {
  assert.deepEqual(
    formatMarketplaceSyncBadge(
      makeMarketplaceStatus({ state: "air-gapped", airgapped: true, prompt_required: false }),
    ),
    { state: "air-gapped", label: "Air-gapped", tone: "neutral" },
  );
  assert.deepEqual(
    formatMarketplaceSyncBadge(makeMarketplaceStatus({ state: "disconnected", consent: "unset" })),
    { state: "disconnected", label: "Not connected", tone: "neutral" },
  );
  assert.deepEqual(
    formatMarketplaceSyncBadge(
      makeMarketplaceStatus({ state: "disconnected", consent: "declined", prompt_required: false }),
    ),
    { state: "disconnected", label: "Offline (Declined)", tone: "neutral" },
  );
  assert.deepEqual(
    formatMarketplaceSyncBadge(
      makeMarketplaceStatus({ state: "connected", consent: "accepted", prompt_required: false }),
    ),
    { state: "connected", label: "Connected", tone: "success" },
  );
  assert.deepEqual(
    formatMarketplaceSyncBadge(
      makeMarketplaceStatus({ state: "stale", consent: "accepted", prompt_required: false }),
    ),
    { state: "stale", label: "Stale", tone: "warn" },
  );
});

test("[A2] external provider quotes always require explicit buyer consent even when €0.00", () => {
  const localFreeQuote = {
    target_node_id: "local-node",
    estimated_cost_milli_eur: 0,
    serving_org_id: null,
    external_provider_org: null,
    external_provider_notice: null,
  };
  assert.equal(requiresExternalProviderConsent(localFreeQuote), false);
  assert.equal(shouldPromptQuoteConsent(localFreeQuote), false);
  assert.equal(getExternalProviderConsentInfo(localFreeQuote), null);

  const externalFreeQuote = {
    target_node_id: "network-provider-zurich-01",
    estimated_cost_milli_eur: 0,
    serving_org_id: "org-zurich-proteomics",
    external_provider_org: "Zurich Proteomics AG",
    external_provider_notice: "Inputs execute on external provider node.",
  };
  assert.equal(requiresExternalProviderConsent(externalFreeQuote), true);
  assert.equal(shouldPromptQuoteConsent(externalFreeQuote), true);

  const info = getExternalProviderConsentInfo(externalFreeQuote);
  assert.ok(info);
  assert.equal(info.providerLabel, "Zurich Proteomics AG");
  assert.equal(info.servingOrgId, "org-zurich-proteomics");
  assert.equal(info.nodeId, "network-provider-zurich-01");
  assert.match(info.notice, /external provider node/);

  // Verify consent modal & inline quote review gate execution on explicit checkbox ack.
  const paidConsentSrc = readSrc("components/PaidRunConsent.tsx");
  assert.match(paidConsentSrc, /external-provider-consent-banner/);
  assert.match(paidConsentSrc, /external-provider-consent-checkbox/);
  assert.match(paidConsentSrc, /externalProvider !== null && !externalAck/);

  const moduleTabsSrc = readSrc("components/ModuleTabs.tsx");
  assert.match(moduleTabsSrc, /quote-external-provider-banner/);
  assert.match(moduleTabsSrc, /quote-external-provider-checkbox/);

  const appSrc = readSrc("App.tsx");
  assert.match(appSrc, /shouldPromptQuoteConsent\(quote\)/);
  assert.match(appSrc, /!requiresExternalProviderConsent\(quoteObj\)/);
});

test("[Stage 1] Control Tower retains cloud-pool fleet and curated approvals, without federation or tiers", () => {
  const controlTowerSrc = readSrc("components/ControlTowerView.tsx");
  assert.match(controlTowerSrc, /<option value="cloud-pool">cloud-pool<\/option>/);
  assert.match(controlTowerSrc, /curated-approvals-table/);
  assert.doesNotMatch(controlTowerSrc, /ct-federation-peers-panel/);
  assert.doesNotMatch(controlTowerSrc, /tier-definitions-list/);

  const apiSrc = readSrc("api.ts");
  assert.match(apiSrc, /\/api\/v1\/marketplace\/status/);
  assert.match(apiSrc, /\/api\/v1\/marketplace\/consent/);
  assert.match(apiSrc, /\/api\/v1\/marketplace\/sync/);
  assert.doesNotMatch(apiSrc, /\/api\/v1\/admin\/federation\//);
  assert.doesNotMatch(apiSrc, /\/api\/v1\/auth\/federation\//);
  assert.doesNotMatch(apiSrc, /\/api\/v1\/admin\/tiers/);
  assert.doesNotMatch(apiSrc, /\/api\/v1\/result-shares/);
});
