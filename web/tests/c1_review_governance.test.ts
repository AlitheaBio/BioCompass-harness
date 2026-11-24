import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import {
  effectiveRunCostMilli,
  formatMilliEur,
  unlimitedCeilingWarning,
  unlimitedCeilings,
} from "../src/types.ts";

const here = dirname(fileURLToPath(import.meta.url));
const readSrc = (rel: string) => readFileSync(resolve(here, "../src", rel), "utf8");

test("[review H-ceilings] unlimited ceilings are flagged and warned about", () => {
  assert.deepEqual(
    unlimitedCeilings({ daily_ceiling_milli_eur: null, total_ceiling_milli_eur: 5000 }),
    { daily: true, total: false },
  );
  // Server flags win over the raw ceilings.
  assert.deepEqual(
    unlimitedCeilings({ unlimited_daily: false, unlimited_total: true, daily_ceiling_milli_eur: 1, total_ceiling_milli_eur: null }),
    { daily: false, total: true },
  );
  assert.equal(unlimitedCeilingWarning(false, false), null);
  assert.match(unlimitedCeilingWarning(true, true) ?? "", /whole balance/);
  assert.match(unlimitedCeilingWarning(true, false) ?? "", /No daily spending ceiling/);
  assert.match(unlimitedCeilingWarning(false, true) ?? "", /No total spending ceiling/);
  const panel = readSrc("components/AgentCredentialsPanel.tsx");
  assert.ok(panel.includes("agent-cred-unlimited-warning"), "issue form shows the warning");
  assert.ok(panel.includes("agent-cred-unlimited-"), "credential list shows the warning");
});

test("[review pricing] Run Cost uses EUR offer pricing and local runs show no-charge label", () => {
  const plugin = {
    manifest: {
      price_milli_eur: 3000,
      offers: [{ id: "standard", name: "S", price_milli_eur: 3000, base_fee_milli_eur: 500, max_input_bytes: 1, max_output_bytes: 1 }],
    },
  };
  assert.equal(effectiveRunCostMilli(plugin), 3500, "list price includes the base fee");
  assert.equal(formatMilliEur(3500), "€3.50");
  assert.ok(readSrc("components/ModuleTabs.tsx").includes("effectiveRunCostMilli(plugin)"));
  assert.ok(readSrc("components/ModuleTabs.tsx").includes("Local run (no charge)"));
  assert.ok(readSrc("components/PaidRunConsent.tsx").includes("Local run (no charge)"));
  assert.ok(readSrc("App.tsx").includes("Local run (no charge)"));
});

test("[review euro] hosted admin and workbench views never label amounts as ACT", () => {
  for (const rel of [
    "components/ControlTowerView.tsx",
    "components/OrgAdminView.tsx",
    "components/SettingsModal.tsx",
    "components/PaidRunConsent.tsx",
    "components/ModuleTabs.tsx",
  ]) {
    const src = readSrc(rel);
    assert.ok(!/>[^<{]*\bACT\b[^<]*</.test(src), `${rel} still shows ACT in visible text`);
    assert.ok(!src.includes("(milli-ACT)"), `${rel} still asks for milli-ACT`);
  }
});

test("[stage1] local mode skips ledger fetch and shows local no-billing note", () => {
  const appSrc = readSrc("App.tsx");
  assert.match(appSrc, /isCloudMode \? fetchLedger\(\) : Promise\.resolve\(null\)/);
  assert.match(appSrc, /cloud \? fetchLedger\(\) : Promise\.resolve\(null\)/);

  const settingsSrc = readSrc("components/SettingsModal.tsx");
  assert.match(settingsSrc, /usage-local-note/);
  assert.match(settingsSrc, /settlement_method = &quot;none&quot;/);
});

test("[review webhooks/statements] UI exposes redelivery, replay window, paging and in-flight spend", () => {
  const hooks = readSrc("components/WebhooksPanel.tsx");
  assert.ok(hooks.includes("redeliverWebhookDelivery"));
  assert.ok(hooks.includes("X-BioCompass-Timestamp") && hooks.includes("5 minutes"));
  const stmt = readSrc("components/BillingStatementPanel.tsx");
  assert.ok(stmt.includes("next_cursor") && stmt.includes("statement-load-more"));
  assert.ok(stmt.includes("reserved_in_flight_milli_eur"));
  assert.ok(stmt.includes("statement-ledger-reconciliation"));
});
