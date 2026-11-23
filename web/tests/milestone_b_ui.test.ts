import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import {
  formatFederatedOfferTarget,
  formatMarketplaceSyncBadge,
  getExternalProviderConsentInfo,
  requiresExternalProviderConsent,
  shouldPromptQuoteConsent,
  shouldShowMarketplaceConsentModal,
} from "../src/milestoneBPolicy.ts";
import type { FederationPeerRecord, MarketplaceStatus } from "../src/types.ts";

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

test("[A2] external provider-edge quotes always require explicit buyer consent even when 0 ACT", () => {
  const localFreeQuote = {
    target_node_id: "local-node",
    estimated_cost_act: 0,
    serving_org_id: null,
    external_provider_org: null,
    external_provider_notice: null,
  };
  assert.equal(requiresExternalProviderConsent(localFreeQuote), false);
  assert.equal(shouldPromptQuoteConsent(localFreeQuote), false);
  assert.equal(getExternalProviderConsentInfo(localFreeQuote), null);

  const externalFreeQuote = {
    target_node_id: "provider-edge-zurich-01",
    estimated_cost_act: 0,
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
  assert.equal(info.nodeId, "provider-edge-zurich-01");
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

test("[B1/A3] Org Admin enrolls provider-edge and Control Tower manages federation_peers contract (#42)", () => {
  const orgAdminSrc = readSrc("components/OrgAdminView.tsx");
  assert.match(orgAdminSrc, /mode:\s*"provider-edge"/);

  const controlTowerSrc = readSrc("components/ControlTowerView.tsx");
  assert.match(controlTowerSrc, /<option value="provider-edge">provider-edge<\/option>/);
  assert.match(controlTowerSrc, /<option value="query-provider">query-provider<\/option>/);
  assert.match(controlTowerSrc, /ct-federation-peers-panel/);
  assert.match(controlTowerSrc, /fetchAdminFederationPeers/);
  assert.match(controlTowerSrc, /approveAdminFederationPeer/);
  assert.match(controlTowerSrc, /rejectAdminFederationPeer/);
  assert.match(controlTowerSrc, /suspendAdminFederationPeer/);
  assert.match(controlTowerSrc, /syncAdminFederationPeer/);

  const apiSrc = readSrc("api.ts");
  assert.match(apiSrc, /\/api\/v1\/admin\/federation\/peers/);
  assert.match(apiSrc, /\/approve/);
  assert.match(apiSrc, /\/reject/);
  assert.match(apiSrc, /\/suspend/);
  assert.match(apiSrc, /\/sync/);
  assert.match(apiSrc, /\/api\/v1\/marketplace\/status/);
  assert.match(apiSrc, /\/api\/v1\/marketplace\/consent/);
  assert.match(apiSrc, /\/api\/v1\/marketplace\/sync/);

  // Nullable home_org / destination_org on pending public PeerDto (#50).
  const pendingPeer: FederationPeerRecord = {
    id: "peer-1",
    peer_url: "https://client.hospital.org",
    peer_pubkey: "a".repeat(64),
    org_name: "Hospital Client",
    status: "pending",
    requested_at: "2026-11-23T00:00:00Z",
    enabled: false,
    role: "consumer",
    label: "Hospital Client",
    origin: "https://client.hospital.org",
    backchannel: "https://client.hospital.org",
    home_org: null,
    destination_org: null,
    archive_enabled: false,
  };
  assert.equal(pendingPeer.home_org, null);
  assert.equal(pendingPeer.destination_org, null);

  // Federated dataset offers have plugin_id: null and catalog_id set (#50).
  assert.equal(
    formatFederatedOfferTarget({
      offer_id: "do-1",
      plugin_id: null,
      version: null,
      catalog_id: "alithea/hla-atlas-v2026.1",
    }),
    "alithea/hla-atlas-v2026.1",
  );
  assert.equal(
    formatFederatedOfferTarget({
      offer_id: "mo-1",
      plugin_id: "alithea/hla-motif-enrichment",
      version: "2.0.0",
      catalog_id: null,
    }),
    "alithea/hla-motif-enrichment · 2.0.0",
  );

  const moduleOffersSrc = readSrc("components/ModuleOffersView.tsx");
  assert.match(moduleOffersSrc, /formatFederatedOfferTarget\(offer\)/);
});
