import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import {
  effectiveRunCostMilli,
  formatWorkspaceAmount,
  negotiatedPriceForRelease,
  unlimitedCeilingWarning,
  unlimitedCeilings,
} from "../src/types.ts";

const here = dirname(fileURLToPath(import.meta.url));
const readSrc = (rel: string) => readFileSync(resolve(here, "../src", rel), "utf8");

test("[review H-ceilings] unlimited ceilings are flagged and warned about", () => {
  assert.deepEqual(
    unlimitedCeilings({ daily_ceiling_milli_act: null, total_ceiling_milli_act: 5000 }),
    { daily: true, total: false },
  );
  // Server flags win over the raw ceilings.
  assert.deepEqual(
    unlimitedCeilings({ unlimited_daily: false, unlimited_total: true, daily_ceiling_milli_act: 1, total_ceiling_milli_act: null }),
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

test("[review pricing] Run Cost is the price the org pays, negotiated when present", () => {
  const plugin = {
    manifest: { price_milli_act: 3000, offers: [{ id: "standard", name: "S", price_milli_act: 3000, base_fee_milli_act: 500, max_input_bytes: 1, max_output_bytes: 1 }] },
  };
  assert.equal(effectiveRunCostMilli(plugin), 3500, "list price includes the base fee");
  const offers = [
    { plugin_id: "a/b", version: "1.0.0", action: "run" as const, negotiated_price: null },
    {
      plugin_id: "a/b",
      version: "1.0.0",
      action: "run" as const,
      negotiated_price: { offer_id: "mo-1", manifest_offer_id: "standard", price_milli_act: 1200, base_fee_milli_act: 100, updated_at: "" },
    },
  ];
  const negotiated = negotiatedPriceForRelease(offers, "a/b", "1.0.0");
  assert.equal(effectiveRunCostMilli(plugin, negotiated), 1300);
  assert.equal(negotiatedPriceForRelease(offers, "a/b", "2.0.0"), null);
  assert.equal(formatWorkspaceAmount(1300, false), "€1.30");
  assert.ok(readSrc("components/ModuleTabs.tsx").includes("effectiveRunCostMilli(plugin, negotiatedPrice)"));
});

test("[review euro] hosted admin views never label amounts as ACT", () => {
  for (const rel of ["components/ControlTowerView.tsx", "components/OrgAdminView.tsx"]) {
    const src = readSrc(rel);
    assert.ok(!/>[^<{]*\bACT\b[^<]*</.test(src), `${rel} still shows ACT in visible text`);
    assert.ok(!src.includes("(milli-ACT)"), `${rel} still asks for milli-ACT`);
  }
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
