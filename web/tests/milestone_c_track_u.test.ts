import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import {
  formatModuleRunsInLabel,
  formatModuleSourceLabel,
  networkRunHandoff,
  resolveModuleCardActions,
} from "../src/moduleListPolicy.ts";
import { formatMilliEur } from "../src/types.ts";

const here = dirname(fileURLToPath(import.meta.url));
const webSrc = resolve(here, "../src");

function readSrc(relPath: string): string {
  return readFileSync(resolve(webSrc, relPath), "utf8");
}

test("[H3] formatMilliEur formats milli-EUR values as euros to the cent", () => {
  assert.equal(formatMilliEur(0), "€0.00");
  assert.equal(formatMilliEur(50), "€0.05");
  assert.equal(formatMilliEur(1000), "€1.00");
  assert.equal(formatMilliEur(5000), "€5.00");
  assert.equal(formatMilliEur(12340), "€12.34");
  assert.equal(formatMilliEur(12345), "€12.35");
  assert.equal(formatMilliEur(-50), "-€0.05");
  assert.equal(formatMilliEur(-4), "€0.00");
  assert.equal(formatMilliEur(250000), "€250.00");
});

test("[U2] unified module list semantics separate source from runs_in with exact Open/Install/Remove buttons", () => {
  // 1. Local workspace installed module: Open + Remove (no Install)
  const localMod = resolveModuleCardActions(
    {
      source: "workspace",
      runs_in: "local",
      execution_target: "local",
      can_install: false,
      installed_locally: true,
      installed: true,
      removable_versions: ["1.0.0"],
    },
    true,
  );
  assert.equal(localMod.sourceLabel, "This workspace");
  assert.equal(localMod.runsInLabel, "Local");
  assert.equal(localMod.canOpen, true);
  assert.equal(localMod.canInstall, false);
  assert.equal(localMod.canRemove, true);
  assert.equal(localMod.isNetworkRunOnly, false);

  // 2. My org cloud run-only module: Open ONLY (Install hidden because can_install=false)
  const cloudRunOnly = resolveModuleCardActions(
    {
      source: "org_cloud",
      runs_in: "cloud",
      execution_target: "alithea_cloud",
      can_install: false,
      installed_locally: false,
      installed: false,
    },
    true,
  );
  assert.equal(cloudRunOnly.sourceLabel, "My org cloud");
  assert.equal(cloudRunOnly.runsInLabel, "Cloud");
  assert.equal(cloudRunOnly.canOpen, true);
  assert.equal(cloudRunOnly.canInstall, false);
  assert.equal(cloudRunOnly.canRemove, false);

  // 3. My org cloud download-enabled module in local workspace: Open + Install!
  const cloudDownloadable = resolveModuleCardActions(
    {
      source: "org_cloud",
      runs_in: "cloud",
      execution_target: "alithea_cloud",
      can_install: true,
      installed_locally: false,
      installed: false,
    },
    true,
  );
  assert.equal(cloudDownloadable.canOpen, true);
  assert.equal(cloudDownloadable.canInstall, true);
  assert.equal(cloudDownloadable.canRemove, false);

  // 4. Network provider run-only module: Open + "Runs at <provider>" + safe handoff URL, Install hidden!
  const networkRunOnly = resolveModuleCardActions(
    {
      source: "network_provider",
      runs_in: "network_provider",
      execution_target: "froglet_node",
      provider_label: "Zurich Proteomics",
      can_install: false,
      installed_locally: false,
      installed: false,
      launch_url: "https://zurich.example.org/offers/mo-1",
    },
    true,
  );
  assert.equal(networkRunOnly.sourceLabel, "Network · Zurich Proteomics");
  assert.equal(networkRunOnly.runsInLabel, "Runs at Zurich Proteomics");
  assert.equal(networkRunOnly.canOpen, true);
  assert.equal(networkRunOnly.canInstall, false);
  assert.equal(networkRunOnly.isNetworkRunOnly, true);
  assert.equal(networkRunOnly.runsAtProviderLabel, "Runs at Zurich Proteomics");
  assert.equal(networkRunOnly.safeHandoffUrl, "https://zurich.example.org/offers/mo-1");

  // Unsafe javascript: launch URL must be stripped cleanly.
  const unsafeNet = resolveModuleCardActions(
    {
      source: "network_provider",
      runs_in: "network_provider",
      execution_target: "froglet_node",
      provider_label: "Untrusted",
      can_install: false,
      installed_locally: false,
      installed: false,
      launch_url: "javascript:alert(1)",
    },
    true,
  );
  assert.equal(unsafeNet.safeHandoffUrl, null);
  assert.equal(
    formatModuleSourceLabel({ source: "network_provider", execution_target: "froglet_node" }),
    "Network provider",
  );
  assert.equal(
    formatModuleRunsInLabel({ runs_in: "network_provider", execution_target: "froglet_node" }),
    "Provider node",
  );
});

test("[C1/U3] API client and Enterprise UI panels wire retained Milestone C1 routes", () => {
  const apiSrc = readSrc("api.ts");
  for (const route of [
    "/api/v1/workspace/hub",
    "/api/v1/workspace/hub/login",
    "/api/v1/workspace/network-offers",
    "/api/v1/agent-credentials",
    "/api/v1/billing/statement",
    "/api/v1/webhooks",
    "/api/v1/documents",
    "/provenance",
    "/placement",
  ]) {
    assert.ok(apiSrc.includes(route), `api.ts missing route ${route}`);
  }

  const orgAdminSrc = readSrc("components/OrgAdminView.tsx");
  assert.match(orgAdminSrc, /AgentCredentialsPanel/);
  assert.match(orgAdminSrc, /BillingStatementPanel/);
  assert.match(orgAdminSrc, /WebhooksPanel/);
  assert.match(orgAdminSrc, /DocumentsPanel/);
  assert.match(orgAdminSrc, /ClientCloudImportPanel/);

  const docsSrc = readSrc("components/DocumentsPanel.tsx");
  assert.match(docsSrc, /DOC_VERSION_CONFLICT/);
  assert.match(docsSrc, /document-version-conflict-banner/);
  assert.doesNotMatch(docsSrc, /content_json/);

  const stmtSrc = readSrc("components/BillingStatementPanel.tsx");
  assert.match(stmtSrc, /g\.plugin_id/);
  assert.match(stmtSrc, /g\.catalog_id/);
  assert.match(stmtSrc, /g\.dataset_fee_milli_eur/);
  assert.match(stmtSrc, /entry\.settled_milli_eur/);

  assert.match(apiSrc, /toRfc3339StatementBound/);
  assert.match(apiSrc, /data\.webhooks/);
  assert.match(apiSrc, /data\.webhook/);
  assert.match(apiSrc, /data\.document/);

  const provSrc = readSrc("components/ProvenancePanel.tsx");
  assert.match(provSrc, /bc\.provenance\.v1/);

  const appSrc = readSrc("App.tsx");
  assert.match(appSrc, /WorkspaceHubChooser/);
  assert.match(appSrc, /resolveModuleCardActions/);
  assert.match(appSrc, /network-offers-discovery-section/);
});

test("[U-4] an Added network module offers Remove (unsubscribe) and only for subscribed offers", () => {
  const networkModule = {
    source: "network_provider" as const,
    runs_in: "network_provider" as const,
    execution_target: "froglet_node",
    provider_label: "Provider B",
    can_install: false,
    installed_locally: false,
    installed: true,
    removable_versions: [],
    detached_versions: [],
    launch_url: "https://hub-b.example/module-offers/mo-1",
    marketplace_offer_id: "mo-1",
  };
  const subscribed = resolveModuleCardActions(networkModule, true, new Set(["mo-1"]));
  assert.equal(subscribed.canUnsubscribe, true);
  assert.equal(subscribed.subscriptionId, "mo-1");
  assert.equal(resolveModuleCardActions(networkModule, true, new Set()).canUnsubscribe, false);
  assert.equal(resolveModuleCardActions(networkModule, false, new Set(["mo-1"])).canUnsubscribe, false);
});

test("[U-4] a network run-only module's in-iframe run is intercepted with an explanation and handoff", () => {
  const runOnly = {
    source: "network_provider" as const,
    runs_in: "network_provider" as const,
    execution_target: "froglet_node",
    provider_label: "Provider B",
    can_install: false,
    installed_locally: false,
    installed: true,
    launch_url: "https://hub-b.example/module-offers/mo-1",
  };
  const handoff = networkRunHandoff(runOnly);
  assert.ok(handoff);
  assert.match(handoff.message, /runs at Provider B/);
  assert.equal(handoff.handoffUrl, "https://hub-b.example/module-offers/mo-1");
  assert.equal(networkRunHandoff({ ...runOnly, launch_url: "javascript:alert(1)" })?.handoffUrl, null);
  // A module that runs here (or was installed locally) is not intercepted.
  assert.equal(networkRunHandoff({ ...runOnly, source: "org_cloud", runs_in: "cloud" }), null);
  assert.equal(networkRunHandoff({ ...runOnly, installed_locally: true }), null);
  // The sandbox bridge must route prepareRun through the interception.
  const frame = readSrc("components/SandboxedPluginFrame.tsx");
  assert.match(frame, /networkRunHandoff\(plugin\)/);
  assert.match(frame, /RUN_AT_PROVIDER/);
});

test("[U-1] the hub chooser shows the pinned hub key and can trust a key explicitly", () => {
  const chooser = readSrc("components/WorkspaceHubChooser.tsx");
  assert.match(chooser, /workspace-hub-key-pinned/);
  assert.match(chooser, /workspace-hub-key-trust-btn/);
  assert.match(chooser, /hub_key: hubKey/);
});
