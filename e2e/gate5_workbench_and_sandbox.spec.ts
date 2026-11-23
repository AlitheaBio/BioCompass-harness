import fs from "node:fs";
import { expect, test } from "@playwright/test";

function resolveEntryUrl(): { url: string; hasOwnerLink: boolean } {
  const directLink = process.env["BIOCOMPASS_E2E_OWNER_LINK"]?.trim();
  if (directLink) {
    return { url: directLink, hasOwnerLink: true };
  }
  const linkFile = process.env["BIOCOMPASS_E2E_OWNER_LINK_FILE"]?.trim();
  if (linkFile && fs.existsSync(linkFile)) {
    const fromFile = fs.readFileSync(linkFile, "utf-8").trim();
    if (fromFile) {
      return { url: fromFile, hasOwnerLink: true };
    }
  }
  return { url: "/", hasOwnerLink: false };
}

test.describe("Gate 5 — Standalone Binary Browser E2E, Real OCI Execution & Sandbox Exploit Suite", () => {
  test("verifies BrandWordmark, [A1] consent/zero-outbound, DuckLake 3-level Grant governance (zero '***' masking), real OCI Prepare->Confirm + [SEC-20]/[SEC-18] + Schnorr Receipt, and <iframe sandbox='allow-scripts'> exploit blocks", async ({
    page,
    request,
  }) => {
    const pageErrors: string[] = [];
    page.on("pageerror", (err) => {
      pageErrors.push(err.message);
    });

    const initialHealthResp = await request.get("/api/v1/health");
    expect(initialHealthResp.ok()).toBe(true);
    const initialHealth = (await initialHealthResp.json()) as {
      mode: string;
      marketplace_prompt_required?: boolean;
      marketplace_status?: {
        state: string;
        consent: string;
        prompt_required: boolean;
        airgapped: boolean;
        hub_url: string;
        default_hub_url: string;
        feed_offer_count: number;
        last_synced_at?: string | null;
      };
    };
    expect(initialHealth.mode).toBe("local");

    const marketplaceHosts = new Set<string>();
    for (const candidate of [
      initialHealth.marketplace_status?.hub_url,
      initialHealth.marketplace_status?.default_hub_url,
      "https://hub.biocompass.alithea.bio",
    ]) {
      if (!candidate) continue;
      try {
        marketplaceHosts.add(new URL(candidate).host);
      } catch {
        // Ignore malformed candidates
      }
    }

    let allowDeliberateExploitProbes = false;
    const outboundMarketplaceRequests: string[] = [];
    const unexpectedExternalRequests: string[] = [];
    page.on("request", (req) => {
      try {
        const u = new URL(req.url());
        const isHttp = u.protocol === "http:" || u.protocol === "https:";
        const isLoopback =
          u.hostname === "127.0.0.1" ||
          u.hostname === "localhost" ||
          u.hostname === "::1" ||
          u.hostname === "[::1]";
        if (
          marketplaceHosts.has(u.host) ||
          u.pathname === "/v1/feed" ||
          u.pathname.startsWith("/api/v1/marketplace/sync")
        ) {
          outboundMarketplaceRequests.push(`${req.method()} ${req.url()}`);
        } else if (isHttp && !isLoopback && !allowDeliberateExploitProbes) {
          unexpectedExternalRequests.push(`${req.method()} ${req.url()}`);
        }
      } catch {
        // Ignore non-HTTP requests (blob:, data:, about:)
      }
    });

    const entry = resolveEntryUrl();

    // 1. Load standalone single-binary SPA & verify BrandWordmark + live health + [A1] consent + genesis ACT
    await page.goto(entry.url);
    await expect(page.getByTestId("brand-wordmark")).toBeVisible();
    await expect(page.getByTestId("health-pill")).toHaveText("Local");
    await expect(page.getByTestId("health-pill")).toHaveAttribute(
      "title",
      /docker|podman/i,
    );

    // 1b. [A1] First-Run Marketplace Consent Modal:
    // Fresh interactive local boot prompts explicitly; CI/air-gapped boot suppresses cleanly.
    const consentModal = page.getByTestId("marketplace-consent-modal");
    if (initialHealth.marketplace_prompt_required) {
      await expect(consentModal).toBeVisible();
      await expect(consentModal).toContainText(
        "Connect to Global BioCompass Marketplace?",
      );
      expect(outboundMarketplaceRequests).toEqual([]);
      expect(unexpectedExternalRequests).toEqual([]);

      if (entry.hasOwnerLink) {
        const declineRespPromise = page.waitForResponse(
          (resp) =>
            resp.url().includes("/api/v1/marketplace/consent") &&
            resp.request().method() === "PUT",
        );
        await page.getByTestId("marketplace-consent-decline").click();
        const declineResp = await declineRespPromise;
        expect(declineResp.ok()).toBe(true);
        const declinedStatus = (await declineResp.json()) as {
          consent: string;
          prompt_required: boolean;
          state: string;
          feed_offer_count: number;
          last_synced_at?: string | null;
        };
        expect(declinedStatus.consent).toBe("declined");
        expect(declinedStatus.prompt_required).toBe(false);
        expect(declinedStatus.state).toBe("disconnected");
        expect(declinedStatus.feed_offer_count).toBe(0);
        expect(declinedStatus.last_synced_at ?? null).toBeNull();
      } else {
        await consentModal.getByRole("button", { name: "Ask later" }).click();
      }

      await expect(consentModal).toBeHidden();
      expect(outboundMarketplaceRequests).toEqual([]);
      expect(unexpectedExternalRequests).toEqual([]);
    } else {
      expect(
        initialHealth.marketplace_status?.airgapped === true ||
          initialHealth.marketplace_status?.consent !== "unset",
      ).toBe(true);
      await expect(consentModal).toBeHidden();
      expect(outboundMarketplaceRequests).toEqual([]);
      expect(unexpectedExternalRequests).toEqual([]);
    }

    const initialLedgerResp = await request.get("/api/v1/ledger");
    expect(initialLedgerResp.ok()).toBe(true);
    const initialLedger = (await initialLedgerResp.json()) as {
      available_milli_act: number;
    };
    expect(initialLedger.available_milli_act).toBe(250_000);

    // 2. Data Catalogs: verify Full Research Access vs Scoped Partner vs Compute-Only 3-level Grant enforcement
    await page.getByTestId("nav-catalogs").click();
    await expect(page.getByTestId("catalog-access-indicator")).toHaveText(
      "Full Unmasked Row Access",
    );
    const fullTableText = await page.getByTestId("catalog-table").innerText();
    expect(fullTableText).not.toContain("***");
    expect(fullTableText).toMatch(/\b[ACDEFGHIKLMNPQRSTVWY]{8,14}\b/);

    // Switch to Partner Scoped Rows (Lung Cohort · Unmasked Rows)
    const grantSelect = page.getByTestId("grant-select");
    const scopedGrantValue = await grantSelect
      .locator("option", { hasText: /Partner Scoped Rows/i })
      .getAttribute("value");
    expect(scopedGrantValue).toBeTruthy();

    const queryRespPromise = page.waitForResponse(
      (resp) =>
        resp.url().includes("/api/v1/catalogs/alithea/hla-atlas-v2026.1/query") &&
        resp.request().method() === "POST",
    );
    await grantSelect.selectOption(scopedGrantValue!);
    const queryResp = await queryRespPromise;
    expect(queryResp.ok()).toBe(true);

    const scopedPayload = (await queryResp.json()) as {
      access_level: string;
      dataset_digest: string;
      rows: { sequence: string; organ: string }[];
    };
    expect(scopedPayload.access_level).toBe("rows");
    expect(scopedPayload.dataset_digest).toMatch(/^[0-9a-f]{64}$/);
    expect(scopedPayload.rows.length).toBeGreaterThan(0);
    for (const row of scopedPayload.rows) {
      expect(row.sequence).not.toBe("***");
      expect(row.sequence).toMatch(/^[ACDEFGHIKLMNPQRSTVWY]{8,14}$/);
      expect(row.organ).toBe("Lung");
    }

    await expect(page.getByTestId("catalog-access-indicator")).toHaveText(
      "Scoped Unmasked Rows",
    );
    // Open Provenance & Policy drawer to verify typed QueryAst + row_scope
    await page.getByTestId("open-provenance-drawer").click();
    await expect(page.getByTestId("catalog-query-ast")).toContainText('"organ"');
    await expect(page.getByTestId("catalog-query-ast")).toContainText('"Lung"');
    await page.keyboard.press("Escape");

    const scopedTableRows = page.getByTestId("catalog-table").locator("tbody tr");
    const rowCount = await scopedTableRows.count();
    expect(rowCount).toBeGreaterThan(0);
    for (let i = 0; i < rowCount; i++) {
      const cells = scopedTableRows.nth(i).locator("td");
      await expect(cells.nth(2)).not.toHaveText("***");
      await expect(cells.nth(6)).toHaveText("Lung");
    }

    // Switch to Compute-Only Partner Tier: verify SPA does NOT issue a failing queryCatalog call and renders compute-only state
    const computeOnlyGrantValue = await grantSelect
      .locator("option", { hasText: /Compute-Only/i })
      .getAttribute("value");
    expect(computeOnlyGrantValue).toBeTruthy();
    await grantSelect.selectOption(computeOnlyGrantValue!);
    await expect(page.getByTestId("catalog-access-indicator")).toHaveText(
      "Compute-Only (Curated Modules)",
    );
    await expect(page.getByTestId("compute-only-banner")).toBeVisible();

    // Switch back to Full Research Access for Workbench motif run
    const fullGrantValue = await grantSelect
      .locator("option", { hasText: /Full Research Access/i })
      .getAttribute("value");
    expect(fullGrantValue).toBeTruthy();
    await grantSelect.selectOption(fullGrantValue!);

    // 3. Workbench: Prepare & Run real OCI container, stream logs, render SVG views & verify Schnorr Receipt
    await page.getByTestId("nav-workbench").click();
    await page
      .getByTestId("quick-prepare-btn-alithea/hla-motif-enrichment")
      .click();
    await expect(page.getByTestId("paid-run-consent")).toContainText("5.00 credits");
    await page.getByTestId("approve-paid-run").click();

    await expect(
      page.getByTestId("module-window-alithea/hla-motif-enrichment"),
    ).toBeVisible();
    await expect(page.getByTestId("scientific-views")).toBeVisible({
      timeout: 45_000,
    });

    // ACT settled from 250.00 -> 245.00 ACT
    const postMotifLedger = (await (await request.get("/api/v1/ledger")).json()) as {
      available_milli_act: number;
    };
    expect(postMotifLedger.available_milli_act).toBe(245_000);
    await expect(page.getByTestId("live-run-logs")).toContainText(
      "hla-motif-enrichment",
    );

    // Native scientific SVG charts & aggregate-only table rendered from /out/result.json ([SEC-20])
    await expect(page.getByTestId("view-summary-metrics")).toBeVisible();
    await expect(page.getByTestId("view-length-dist")).toBeVisible();
    await expect(page.getByTestId("view-allele-enrichment")).toBeVisible();
    await expect(page.getByTestId("view-organ-breakdown")).toBeVisible();
    await expect(page.getByTestId("view-top-candidates")).toHaveCount(0);
    await expect(page.getByTestId("view-summary-metrics")).toContainText(
      "Aggregate Only (compute_only)",
    );
    const organBreakdownText =
      (await page.getByTestId("view-organ-breakdown").textContent()) ?? "";
    expect(organBreakdownText).toContain("organ");
    expect(organBreakdownText).toContain("condition");
    expect(organBreakdownText).not.toMatch(/\b[ACDEFGHIKLMNPQRSTVWY]{8,14}\b/);
    const scientificViewsText =
      (await page.getByTestId("scientific-views").textContent()) ?? "";
    expect(scientificViewsText).not.toMatch(/\b[ACDEFGHIKLMNPQRSTVWY]{8,14}\b/);

    const motifRunsResp = await request.get("/api/v1/runs");
    expect(motifRunsResp.ok()).toBe(true);
    const motifRunsPayload = (await motifRunsResp.json()) as {
      runs: Array<{
        plugin_id: string;
        result_json?: Record<string, unknown> | null;
      }>;
    };
    const motifRun = motifRunsPayload.runs.find(
      (r) => r.plugin_id === "alithea/hla-motif-enrichment",
    );
    expect(motifRun?.result_json).toBeTruthy();
    expect(motifRun!.result_json!["sequence_policy"]).toBe(
      "aggregate_compute_only",
    );
    expect(motifRun!.result_json!["top_candidates"]).toBeUndefined();
    expect(
      Array.isArray(motifRun!.result_json!["organ_condition_breakdown"]),
    ).toBe(true);
    expect(
      (motifRun!.result_json!["organ_condition_breakdown"] as unknown[]).length,
    ).toBeGreaterThan(0);
    expect(JSON.stringify(motifRun!.result_json)).not.toMatch(
      /\b[ACDEFGHIKLMNPQRSTVWY]{8,14}\b/,
    );

    // [SEC-18] Hardened artifact chip uses <button class="bc-artifact-chip"> authenticated Blob download,
    // and the artifact endpoint enforces sandbox CSP, nosniff, and attachment disposition.
    const runArtifacts = page.getByTestId("run-artifacts");
    await expect(runArtifacts.locator("a")).toHaveCount(0);
    const artifactButton = runArtifacts
      .locator("button.bc-artifact-chip")
      .first();
    await expect(artifactButton).toBeVisible();
    await expect(artifactButton).toContainText("allele_binding_summary.csv");

    const [artifactResp, artifactDownload] = await Promise.all([
      page.waitForResponse(
        (resp) =>
          resp.url().includes("/api/v1/runs/") &&
          resp.url().includes("/artifacts/allele_binding_summary.csv") &&
          resp.request().method() === "GET",
      ),
      page.waitForEvent("download"),
      artifactButton.click(),
    ]);
    expect(artifactResp.ok()).toBe(true);
    expect(artifactDownload.suggestedFilename()).toBe(
      "allele_binding_summary.csv",
    );
    expect(artifactDownload.url()).toMatch(/^blob:/);
    expect(await artifactDownload.failure()).toBeNull();

    const artifactHeaders = artifactResp.headers();
    expect(artifactHeaders["content-security-policy"]).toBe("sandbox");
    expect(artifactHeaders["x-content-type-options"]).toBe("nosniff");
    expect(artifactHeaders["content-disposition"]).toMatch(/^attachment;/i);
    expect(artifactHeaders["content-disposition"]).toContain(
      "allele_binding_summary.csv",
    );

    const csvResp = await request.get(artifactResp.url());
    expect(csvResp.ok()).toBe(true);
    expect(csvResp.headers()["content-security-policy"]).toBe("sandbox");
    expect(csvResp.headers()["x-content-type-options"]).toBe("nosniff");
    expect(csvResp.headers()["content-disposition"]).toMatch(/^attachment;/i);
    const csvBody = await csvResp.text();
    expect(csvBody).toContain("HLA-A*02:01");
    expect(csvBody).not.toMatch(/\b[ACDEFGHIKLMNPQRSTVWY]{8,14}\b/);

    // Froglet v1 5-artifact Schnorr chain inspector + adversarial 1-byte tamper test
    await expect(page.getByTestId("receipt-inspector")).toBeVisible();
    await expect(page.getByTestId("receipt-verified-badge")).toHaveText(
      "SCHNORR CHAIN VALID",
    );

    await page.getByTestId("tamper-test-btn").click();
    await expect(page.getByTestId("receipt-verified-badge")).toHaveText(
      "TAMPER DETECTED / INVALID",
    );

    await page.getByTestId("reverify-chain-btn").click();
    await expect(page.getByTestId("receipt-verified-badge")).toHaveText(
      "SCHNORR CHAIN VALID",
    );
    await expect(
      page.getByTestId("module-window-alithea/hla-motif-enrichment"),
    ).toBeVisible();
    await expect(
      page.getByRole("toolbar", { name: "Open module windows" }),
    ).toBeVisible();

    // 3b. Settings Menu: verify Appearance / Data Governance / Compute & Credits / Developer Studio modal
    await page.getByTestId("nav-settings").click();
    await expect(page.getByTestId("settings-modal")).toBeVisible();
    await page.keyboard.press("Escape");
    await expect(page.getByTestId("settings-modal")).toBeHidden();

    // 4. Sandboxed Custom Vite Plugin UI (<iframe sandbox="allow-scripts">) Exploit & SDK RPC Suite
    // Minimize motif window, switch to Scoped Partner Grant (Lung Cohort) on Data Catalogs, then open sandboxed explorer
    await page
      .getByTestId("module-window-alithea/hla-motif-enrichment")
      .getByRole("button", { name: "Minimize window" })
      .click();
    await page.getByTestId("nav-catalogs").click();
    await grantSelect.selectOption(scopedGrantValue!);
    await page.getByTestId("nav-plugins").click();
    const explorerCard = page.getByTestId(
      "plugin-card-alithea/hla-interactive-explorer",
    );
    const installBtn = explorerCard.getByRole("button", { name: "Install" });
    if (await installBtn.isVisible().catch(() => false)) {
      await installBtn.click();
      await expect(
        explorerCard.getByRole("button", { name: "Open" }),
      ).toBeVisible({ timeout: 60_000 });
    }
    await explorerCard.click();
    await expect(
      page.getByTestId("module-window-alithea/hla-interactive-explorer"),
    ).toBeVisible();

    const iframeEl = page.getByTestId("plugin-sandbox-iframe");
    await expect(iframeEl).toBeVisible();
    await expect(iframeEl).toHaveAttribute("sandbox", "allow-scripts");

    // Verify strict CSP headers on /plugin-ui/alithea/hla-interactive-explorer/index.html
    const pluginHtmlResp = await request.get(
      "/plugin-ui/alithea/hla-interactive-explorer/index.html",
    );
    expect(pluginHtmlResp.ok()).toBe(true);
    const cspHeader = pluginHtmlResp.headers()["content-security-policy"] ?? "";
    expect(cspHeader).toContain("default-src 'none'");
    expect(cspHeader).toContain("connect-src 'none'");

    const frameLocator = page.frameLocator('[data-testid="plugin-sandbox-iframe"]');
    await expect(frameLocator.getByTestId("sandbox-origin-badge")).toHaveText(
      "origin=null",
    );
    await expect(frameLocator.getByTestId("sandbox-access-badge")).toHaveText(
      "Row Access (Unmasked IUPAC)",
    );

    const elementHandle = await iframeEl.elementHandle();
    const childFrame = await elementHandle?.contentFrame();
    expect(childFrame).toBeTruthy();

    // Verify opaque null origin directly inside iframe JS context
    const iframeOrigin = await childFrame!.evaluate(() => window.origin);
    expect(iframeOrigin).toBe("null");

    // Adversarial Exploit 1: Attempt parent DOM theft (window.parent.document)
    const domExploitVerdict = await childFrame!.evaluate(() => {
      try {
        return `ESCAPED:${window.parent.document.title}`;
      } catch (err: unknown) {
        if (err instanceof Error) {
          return `BLOCKED:${err.name}:${err.message}`;
        }
        return `BLOCKED:${String(err)}`;
      }
    });
    expect(domExploitVerdict).toContain("BLOCKED:SecurityError");

    // Adversarial Exploit 2: Attempt external network exfiltration (fetch https://example.com)
    expect(unexpectedExternalRequests).toEqual([]);
    allowDeliberateExploitProbes = true;
    const externalFetchVerdict = await childFrame!.evaluate(async () => {
      try {
        await fetch("https://example.com");
        return "ESCAPED";
      } catch (err: unknown) {
        return `BLOCKED:${err instanceof Error ? err.message : String(err)}`;
      }
    });
    expect(externalFetchVerdict).toMatch(/^BLOCKED:/);

    // Adversarial Exploit 3: Attempt direct fetch to local host API bypassing postMessage bridge
    const hostApiBypassVerdict = await childFrame!.evaluate(async (apiUrl) => {
      try {
        await fetch(apiUrl);
        return "ESCAPED";
      } catch (err: unknown) {
        return `BLOCKED:${err instanceof Error ? err.message : String(err)}`;
      }
    }, new URL("/api/v1/ledger", page.url()).toString());
    expect(hostApiBypassVerdict).toMatch(/^BLOCKED:/);

    // Legitimate @biocompass/plugin-sdk RPC 1: queryCatalog over postMessage JSON-RPC 2.0
    await frameLocator.getByTestId("sdk-query-btn").click();
    const sdkPreviewRows = frameLocator
      .getByTestId("sdk-preview-table")
      .locator("tbody tr");
    await expect(sdkPreviewRows.first()).toBeVisible();
    const sdkRowCount = await sdkPreviewRows.count();
    expect(sdkRowCount).toBeGreaterThan(0);
    for (let i = 0; i < sdkRowCount; i++) {
      await expect(sdkPreviewRows.nth(i).locator("td").nth(1)).not.toHaveText("***");
      await expect(sdkPreviewRows.nth(i).locator("td").nth(4)).toHaveText("Lung");
    }

    // Switch to Compute-Only grant and verify host bridge blocks both queryCatalog RPC and requires_catalog_level="rows" prepareRun!
    await page
      .getByTestId("module-window-alithea/hla-interactive-explorer")
      .getByRole("button", { name: "Minimize window" })
      .click();
    await page.getByTestId("nav-catalogs").click();
    await grantSelect.selectOption(computeOnlyGrantValue!);
    await page
      .getByRole("toolbar", { name: "Open module windows" })
      .getByRole("button", { name: "HLA Antigenic Gene Hotspot Explorer", exact: true })
      .click();
    await expect(frameLocator.getByTestId("sandbox-access-badge")).toHaveText(
      "Compute-Only Grant (Rows Hidden)",
    );
    await frameLocator.getByTestId("sdk-query-btn").click();
    await expect(frameLocator.getByTestId("sandbox-status")).toContainText(
      /Catalog RPC error:.*compute_only/i,
    );
    await frameLocator.getByTestId("sdk-prepare-btn").click();
    await expect(frameLocator.getByTestId("sandbox-status")).toContainText(
      /Prepare RPC error:.*requires.*rows/i,
    );

    // Switch back to Scoped Partner Grant (rows) for legitimate @biocompass/plugin-sdk RPC 2:
    // prepareRun -> Host Auto-Dispatch -> Real OCI Container -> Push Result Back to Iframe over postMessage!
    await page
      .getByTestId("module-window-alithea/hla-interactive-explorer")
      .getByRole("button", { name: "Minimize window" })
      .click();
    await page.getByTestId("nav-catalogs").click();
    await grantSelect.selectOption(scopedGrantValue!);
    await page
      .getByRole("toolbar", { name: "Open module windows" })
      .getByRole("button", { name: "HLA Antigenic Gene Hotspot Explorer", exact: true })
      .click();
    await expect(frameLocator.getByTestId("sandbox-access-badge")).toHaveText(
      "Row Access (Unmasked IUPAC)",
    );
    await frameLocator.getByTestId("sdk-prepare-btn").click();
    await expect(frameLocator.getByTestId("sdk-quote-banner")).toBeVisible();

    await expect(page.getByTestId("paid-run-consent")).toContainText("5.00 credits");
    await page.getByTestId("approve-paid-run").click();
    const hotspotRows = frameLocator
      .getByTestId("sdk-hotspots-table")
      .locator("tbody tr");
    await expect(hotspotRows.first()).toBeVisible({ timeout: 45_000 });
    expect(await hotspotRows.count()).toBeGreaterThan(0);

    const finalLedger = (await (await request.get("/api/v1/ledger")).json()) as {
      available_milli_act: number;
    };
    expect(finalLedger.available_milli_act).toBe(240_000);

    // Zero uncaught page errors and zero outbound marketplace requests across the entire session
    expect(outboundMarketplaceRequests).toEqual([]);
    expect(pageErrors).toEqual([]);
  });
});
