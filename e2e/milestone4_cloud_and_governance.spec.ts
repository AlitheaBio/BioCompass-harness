import { execFileSync } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { expect, test } from "@playwright/test";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const CLOUD_COMPOSE_FILE = path.resolve(
  __dirname,
  "../../biocompass-cloud/docker-compose.cloud.yml",
);
const CLOUD_WEB_URL =
  process.env["BIOCOMPASS_CLOUD_WEB_URL"] ?? "http://127.0.0.1:18180";
const BOOTSTRAP_API_KEY =
  process.env["COMPASS_HUB_BOOTSTRAP_API_KEY"] ??
  "bc_live_bootstrap_cloud_web_key_0123456789abcdef";

test.describe("Milestone 4 — Shared SPA Governance, Saved Views, DuckLake Import, Quote Consent & Cloud Control Tower", () => {
  test("Local Mode: Saved Views ('Verified locally on this machine'), CSV DuckLake Import + Generic Columns, Snapshot Pin/Rollback, and Signed Quote Consent Card", async ({
    page,
  }) => {
    const pageErrors: string[] = [];
    page.on("pageerror", (err) => {
      pageErrors.push(err.message);
    });

    await page.goto("/");
    await expect(page.getByTestId("brand-wordmark")).toBeVisible();
    await expect(page.getByTestId("health-pill")).toHaveText("Local");

    // 1. Navigate to Data Catalogs and ensure Full Research Access grant on default HLA catalog
    await page.getByTestId("nav-catalogs").click();
    const defaultCatalogCard = page.getByTestId(
      "catalog-card-alithea/hla-atlas-v2026.1",
    );
    if (await defaultCatalogCard.isVisible()) {
      await defaultCatalogCard.click();
    }
    const grantSelect = page.getByTestId("grant-select");
    const fullGrantValue = await grantSelect
      .locator("option", { hasText: /Full Research Access/i })
      .first()
      .getAttribute("value");
    expect(fullGrantValue).toBeTruthy();
    await grantSelect.selectOption(fullGrantValue!);

    await expect(page.getByTestId("catalog-access-indicator")).toHaveText(
      "Full Unmasked Row Access",
    );
    const initialTableText = await page.getByTestId("catalog-table").innerText();
    expect(initialTableText).not.toContain("***");

    // 2. Create a reproducible Saved View and verify local provenance badge + on-demand PK digest check
    await page.getByTestId("save-view-btn").click();
    await expect(page.getByTestId("save-view-modal")).toBeVisible();
    await page
      .getByTestId("save-view-name-input")
      .fill("E2E Baseline HLA Snapshot");
    await page.getByTestId("save-view-submit-btn").click();
    await expect(page.getByTestId("save-view-modal")).toBeHidden();

    const savedViewsPanel = page.getByTestId("saved-views-panel");
    await expect(savedViewsPanel).toBeVisible();
    await expect(
      savedViewsPanel.locator('[data-testid^="saved-view-local-badge-"]').first(),
    ).toHaveText("Verified locally on this machine");

    await savedViewsPanel
      .locator('[data-testid^="verify-saved-view-btn-"]')
      .first()
      .click();
    const verifyBanner = page.getByTestId("saved-view-verification-banner");
    await expect(verifyBanner).toBeVisible();
    await expect(verifyBanner).toContainText("Verified");
    await expect(verifyBanner).toContainText("Verified locally on this machine");

    // 3. Local DuckLake Table Import (CSV -> replace) into a brand-new generic catalog
    await page.getByTestId("open-import-wizard-btn").click();
    await expect(page.getByTestId("catalog-import-wizard")).toBeVisible();

    await page.getByTestId("import-file-input").setInputFiles({
      name: "targets.csv",
      mimeType: "text/csv",
      buffer: Buffer.from(
        "gene_id,symbol,score\nG1,EGFR,9.4\nG2,KRAS,8.7\n",
        "utf-8",
      ),
    });
    await page.getByTestId("import-mode-select").selectOption("replace");
    await page
      .getByTestId("import-catalog-id")
      .fill("alithea/e2e-imported-targets");
    await page.getByTestId("import-table-name").fill("targets");
    await page.getByTestId("import-primary-key").fill("gene_id");
    await page
      .getByTestId("import-catalog-name")
      .fill("E2E Imported Targets");
    await page.getByTestId("import-submit-btn").click();
    await expect(page.getByTestId("catalog-import-wizard")).toBeHidden({
      timeout: 30_000,
    });

    await expect(
      page.getByTestId("catalog-card-alithea/e2e-imported-targets"),
    ).toBeVisible();
    await expect(page.getByTestId("catalog-table")).toContainText("EGFR");
    await expect(page.getByTestId("catalog-table")).toContainText("KRAS");

    // 4. Append a 3rd row (snapshot #2), then Pin snapshot #1 & Rollback Head in Provenance Drawer
    await page.getByTestId("open-import-wizard-btn").click();
    await expect(page.getByTestId("catalog-import-wizard")).toBeVisible();
    await page.getByTestId("import-file-input").setInputFiles({
      name: "targets_append.csv",
      mimeType: "text/csv",
      buffer: Buffer.from("gene_id,symbol,score\nG3,TP53,9.9\n", "utf-8"),
    });
    await page.getByTestId("import-mode-select").selectOption("append");
    await page
      .getByTestId("import-catalog-id")
      .fill("alithea/e2e-imported-targets");
    await page.getByTestId("import-table-name").fill("targets");
    await page.getByTestId("import-primary-key").fill("gene_id");
    await page.getByTestId("import-submit-btn").click();
    await expect(page.getByTestId("catalog-import-wizard")).toBeHidden({
      timeout: 30_000,
    });

    await expect(page.getByTestId("catalog-table")).toContainText("TP53");

    await page.getByTestId("open-provenance-drawer").click();
    await expect(page.getByTestId("catalog-provenance-drawer")).toBeVisible();
    await page.getByTestId("pin-snapshot-btn-1").click();
    await page.getByTestId("rollback-snapshot-btn-1").click();
    await page.keyboard.press("Escape");
    await expect(page.getByTestId("catalog-provenance-drawer")).toBeHidden();

    await expect(page.getByTestId("catalog-table")).toContainText("EGFR");
    await expect(page.getByTestId("catalog-table")).not.toContainText("TP53");

    // 5. Switch back to default HLA catalog and verify Signed Froglet Quote Consent Card flow
    await page
      .getByTestId("catalog-card-alithea/hla-atlas-v2026.1")
      .click();
    await expect(page.getByTestId("catalog-table")).toContainText("HLA-");

    await page.getByTestId("nav-plugins").click();
    await page
      .getByTestId("plugin-card-alithea/hla-motif-enrichment")
      .click();
    const motifWindow = page.getByTestId(
      "module-window-alithea/hla-motif-enrichment",
    );
    await expect(motifWindow).toBeVisible();

    await motifWindow.getByRole("tab", { name: "API" }).click({ force: true });
    await expect(page.getByTestId("offer-select")).toBeVisible();
    await page.getByTestId("review-quote-btn").click();

    const consentCard = page.getByTestId("quote-consent-card");
    await expect(consentCard).toBeVisible();
    await expect(consentCard).toContainText("Local run (no charge)");
    await expect(consentCard).toContainText(/workload sha256:[0-9a-f]+/i);
    await expect(consentCard).toContainText("Enforced Input Limit");
    await expect(consentCard).toContainText("Enforced Output Limit");

    await page.getByTestId("confirm-quote-run-btn").click();
    await expect(page.getByTestId("scientific-views")).toBeVisible({
      timeout: 45_000,
    });
    await expect(page.getByTestId("receipt-verified-badge")).toHaveText(
      "SCHNORR CHAIN VALID",
    );

    expect(pageErrors).toEqual([]);
  });

  test("Cloud Mode: AuthGate Free Demo Sign-Up, Cloud Run, Invitation + Quarantine S3 Multipart Ingest + Cloud Saved View + Platform Admin Control Tower", async ({
    browser,
    request,
  }) => {
    let cloudHealthy = false;
    try {
      const healthResp = await request.get(`${CLOUD_WEB_URL}/api/v1/health`, {
        timeout: 3_000,
      });
      if (healthResp.ok()) {
        const body = (await healthResp.json()) as { mode?: string };
        cloudHealthy = body.mode === "cloud-web" || body.mode === "cloud";
      }
    } catch {
      cloudHealthy = false;
    }

    if (process.env["BIOCOMPASS_REQUIRE_CLOUD_E2E"] === "1") {
      expect(
        cloudHealthy,
        `Expected cloud-web to be healthy at ${CLOUD_WEB_URL}`,
      ).toBe(true);
    } else {
      test.skip(
        !cloudHealthy,
        `cloud-web is not running at ${CLOUD_WEB_URL}; skipping Cloud E2E`,
      );
    }

    const cloudContext = await browser.newContext({ baseURL: CLOUD_WEB_URL });
    const page = await cloudContext.newPage();
    const pageErrors: string[] = [];
    page.on("pageerror", (err) => {
      pageErrors.push(err.message);
    });

    try {
      // 1. Unauthenticated Cloud visit renders AuthGate -> Self-serve Free Demo Registration
      await page.goto("/");
      await expect(page.getByTestId("auth-gate")).toBeVisible();

      const suffix = `${Date.now()}`;
      const demoEmail = `demo.e2e.${suffix}@biotech.bio`;
      await page.getByTestId("auth-tab-register").click();
      await page.getByTestId("auth-reg-name").fill("Dr. Cloud Demo");
      await page
        .getByTestId("auth-reg-org-name")
        .fill(`E2E Demo Org ${suffix}`);
      await page.getByTestId("auth-reg-email").fill(demoEmail);
      await page
        .getByTestId("auth-reg-password")
        .fill("BioCompass#CloudDemo2026!");
      await page.getByTestId("auth-reg-submit").click();

      await expect(page.getByTestId("auth-gate")).toBeHidden({
        timeout: 20_000,
      });
      await expect(page.getByTestId("health-pill")).toHaveText("Cloud");
      await expect(page.getByTestId("cloud-principal-pill")).toBeVisible();

      // 2. Verify physical Demo catalog (alithea/hla-public-demo-v1), daily row budget pill & zero '***' masking
      await page.getByTestId("nav-catalogs").click();
      await expect(
        page.getByTestId("catalog-card-alithea/hla-public-demo-v1"),
      ).toBeVisible();
      await expect(page.getByTestId("daily-row-budget-pill")).toBeVisible();
      const demoTableText = await page.getByTestId("catalog-table").innerText();
      expect(demoTableText).not.toContain("***");
      expect(demoTableText).toMatch(/\b[ACDEFGHIKLMNPQRSTVWY]{8,14}\b/);

      // 3. Verify Organization Admin overview shows the initial EUR balance and daily breaker
      await page.getByTestId("nav-org-admin").click();
      await expect(page.getByTestId("org-admin-view")).toBeVisible();
      const ledgerResponse = await cloudContext.request.get(`${CLOUD_WEB_URL}/api/v1/ledger`);
      expect(ledgerResponse.ok()).toBe(true);
      const demoLedger = await ledgerResponse.json();
      expect(demoLedger.available_milli_eur).toBe(0);
      expect(demoLedger.transactions).toEqual([]);
      await expect(page.getByTestId("org-available-eur")).toHaveText("€0.00");
      await expect(page.getByTestId("org-eur-left-today")).toBeVisible();
      await expect(page.getByTestId("org-grant-budget-list")).toBeVisible();

      // Fund this fixture explicitly before its paid module run.
      const credit = await request.post(`${CLOUD_WEB_URL}/api/v1/billing/invoices/credit`, {
        headers: { "X-API-Key": BOOTSTRAP_API_KEY },
        data: {
          org_id: demoLedger.org_id,
          amount_milli_eur: 25_000,
          reference: `e2e-demo-${demoLedger.org_id}`,
        },
      });
      expect(credit.ok()).toBe(true);

      // 4. Execute Cloud Module Run via Quote Consent Card (operator_ledger.v1 settlement)
      await page.getByTestId("nav-catalogs").click();
      await page
        .getByTestId("catalog-card-alithea/hla-public-demo-v1")
        .click();
      await page.getByTestId("nav-plugins").click();
      await page
        .getByTestId("plugin-card-alithea/hla-motif-enrichment")
        .click();
      const motifWin = page.getByTestId(
        "module-window-alithea/hla-motif-enrichment",
      );
      await expect(motifWin).toBeVisible();
      await motifWin.getByRole("tab", { name: "API" }).click({ force: true });
      await page.getByTestId("review-quote-btn").click();
      await expect(page.getByTestId("quote-consent-card")).toBeVisible();
      await page.getByTestId("confirm-quote-run-btn").click();

      await expect(page.getByTestId("scientific-views")).toBeVisible({
        timeout: 60_000,
      });
      await expect(page.getByTestId("receipt-verified-badge")).toHaveText(
        "SCHNORR CHAIN VALID",
      );
      await motifWin.getByRole("button", { name: /Close window/i }).click();

      // 5. Sign out back to AuthGate
      await page.getByTestId("logout-btn").click();
      await expect(page.getByTestId("auth-gate")).toBeVisible();

      // 6. Accept Invitation into Enterprise org-alithea-cloud & promote to Platform Admin
      const adminEmail = `admin.e2e.${suffix}@alithea.bio`;
      const invResp = await cloudContext.request.post(
        `${CLOUD_WEB_URL}/api/v1/orgs/invitations`,
        {
          headers: { "X-API-Key": BOOTSTRAP_API_KEY },
          data: { email: adminEmail, role: "admin" },
        },
      );
      expect(invResp.ok()).toBe(true);
      const invJson = (await invResp.json()) as { invite_token: string };
      expect(invJson.invite_token).toBeTruthy();

      await page.getByTestId("auth-tab-invite").click();
      await page.getByTestId("auth-inv-token").fill(invJson.invite_token);
      await page.getByTestId("auth-inv-email").fill(adminEmail);
      await page
        .getByTestId("auth-inv-password")
        .fill("BioCompass#Admin2026!");
      await page.getByTestId("auth-inv-submit").click();

      await expect(page.getByTestId("auth-gate")).toBeHidden({
        timeout: 20_000,
      });

      // Promote accepted user to is_platform_admin = TRUE for ControlTowerView E2E
      execFileSync(
        "docker",
        [
          "compose",
          "-f",
          CLOUD_COMPOSE_FILE,
          "exec",
          "-T",
          "postgres",
          "psql",
          "-U",
          "biocompass",
          "-d",
          "biocompass_cloud",
          "-c",
          `UPDATE users SET is_platform_admin = TRUE WHERE email = '${adminEmail}';`,
        ],
        { stdio: "pipe" },
      );

      await page.reload();
      await expect(page.getByTestId("nav-control-tower")).toBeVisible();

      // 7. Cloud Quarantine S3 Multipart CSV Upload + Airgapped Validator + Cloud Saved View
      const cloudCatalogSlug = `alithea/e2e-targets-${suffix}`;
      await page.getByTestId("nav-catalogs").click();
      await page.getByTestId("open-import-wizard-btn").click();
      await expect(page.getByTestId("catalog-import-wizard")).toBeVisible();
      await page.getByTestId("import-file-input").setInputFiles({
        name: "cloud_targets.csv",
        mimeType: "text/csv",
        buffer: Buffer.from(
          "gene_id,symbol,score\nG1,ERBB2,9.8\nG2,MET,8.9\n",
          "utf-8",
        ),
      });
      await page.getByTestId("import-mode-select").selectOption("replace");
      await page.getByTestId("import-catalog-id").fill(cloudCatalogSlug);
      await page.getByTestId("import-table-name").fill("targets");
      await page.getByTestId("import-primary-key").fill("gene_id");
      await page
        .getByTestId("import-catalog-name")
        .fill("Cloud Quarantine E2E Targets");
      await page.getByTestId("import-submit-btn").click();
      await expect(page.getByTestId("catalog-import-wizard")).toBeHidden({
        timeout: 60_000,
      });

      await expect(
        page.getByTestId(`catalog-card-${cloudCatalogSlug}`),
      ).toBeVisible();
      await expect(page.getByTestId("catalog-table")).toContainText("ERBB2");
      await expect(page.getByTestId("catalog-table")).toContainText("MET");

      await page.getByTestId("save-view-btn").click();
      await page
        .getByTestId("save-view-name-input")
        .fill("Cloud Quarantine Saved View");
      await page.getByTestId("save-view-submit-btn").click();
      await expect(page.getByTestId("save-view-modal")).toBeHidden();

      const cloudViewsPanel = page.getByTestId("saved-views-panel");
      await expect(cloudViewsPanel).toBeVisible();
      await expect(
        cloudViewsPanel.locator('[data-testid^="saved-view-local-badge-"]'),
      ).toHaveCount(0);
      await cloudViewsPanel
        .locator('[data-testid^="verify-saved-view-btn-"]')
        .first()
        .click();
      await expect(
        page.getByTestId("saved-view-verification-banner"),
      ).toContainText("Verified");

      // 8. Settings -> Developer Studio Cloud Publish Panel
      await page.getByTestId("nav-settings").click();
      await expect(page.getByTestId("settings-modal")).toBeVisible();
      await page.getByRole("button", { name: "Developer Studio" }).click();
      await expect(page.getByTestId("developer-publish-panel")).toBeVisible();
      await expect(page.getByTestId("developer-publish-panel")).toContainText(
        "biocompass publish-plugin",
      );
      await page.keyboard.press("Escape");
      await expect(page.getByTestId("settings-modal")).toBeHidden();

      // 9. OrgAdminView Security: Scoped API Key + TOTP Enrollment
      await page.getByTestId("nav-org-admin").click();
      await page.getByTestId("org-tab-security").click();
      await page.getByTestId("api-key-label-input").fill("e2e-automation-key");
      await page.getByTestId("create-api-key-btn").click();
      await expect(page.getByTestId("created-api-key-banner")).toContainText(
        "bc_live_",
      );
      await page.getByTestId("enroll-totp-btn").click();
      await expect(page.getByTestId("totp-secret-display")).toBeVisible();

      // 10. Platform Admin ControlTowerView across governance tabs
      await page.getByTestId("nav-control-tower").click();
      await expect(page.getByTestId("control-tower-view")).toBeVisible();

      await page.getByTestId("ct-tab-curated").click();
      await expect(page.getByTestId("curated-approvals-table")).toBeVisible();

      await page.getByTestId("ct-tab-plugins").click();
      await expect(page.getByTestId("plugin-review-queue")).toBeVisible();

      await page.getByTestId("ct-tab-nodes").click();
      await expect(page.getByTestId("ct-worker-nodes-table")).toContainText(
        "froglet-cloud-pool-01",
      );

      await page.getByTestId("ct-tab-billing").click();
      await page
        .getByTestId("invoice-org-id-input")
        .fill("org-alithea-cloud");
      await page.getByTestId("invoice-amount-eur-input").fill("10");
      await page
        .getByTestId("invoice-reference-input")
        .fill(`INV-M4-E2E-${suffix}`);
      await page.getByTestId("invoice-credit-submit").click();
      await expect(page.getByTestId("control-tower-status")).toBeVisible();

      await page.getByTestId("ct-tab-monitor").click();
      await expect(page.getByTestId("enumeration-monitor-table")).toBeVisible();

      expect(pageErrors).toEqual([]);
    } finally {
      await cloudContext.close();
    }
  });
});
