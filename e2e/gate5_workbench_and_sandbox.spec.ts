import { expect, test } from "@playwright/test";

test.describe("Gate 5 — Standalone Binary Browser E2E, Real OCI Execution & Sandbox Exploit Suite", () => {
  test("verifies BrandWordmark, DuckLake 3-level Grant governance (zero '***' masking), real OCI Prepare->Confirm + Schnorr Receipt, and <iframe sandbox='allow-scripts'> exploit blocks", async ({
    page,
    request,
  }) => {
    const pageErrors: string[] = [];
    page.on("pageerror", (err) => {
      pageErrors.push(err.message);
    });

    // 1. Load standalone single-binary SPA & verify BrandWordmark + live health + genesis ACT
    await page.goto("/");
    await expect(page.getByTestId("brand-wordmark")).toBeVisible();
    await expect(page.getByTestId("health-pill")).toHaveText("Local");
    await expect(page.getByTestId("health-pill")).toHaveAttribute(
      "title",
      /docker|podman/i,
    );
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
    await expect(page.getByTestId("paid-run-consent")).toContainText("5.00 ACT");
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

    // Native scientific SVG charts & table rendered from /out/result.json
    await expect(page.getByTestId("view-summary-metrics")).toBeVisible();
    await expect(page.getByTestId("view-length-dist")).toBeVisible();
    await expect(page.getByTestId("view-allele-enrichment")).toBeVisible();
    await expect(page.getByTestId("view-top-candidates")).toBeVisible();

    // Artifact download endpoint serves real CSV output from BlobStore
    const artifactLink = page.getByTestId("run-artifacts").locator("a").first();
    await expect(artifactLink).toContainText("allele_binding_summary.csv");
    const artifactHref = await artifactLink.getAttribute("href");
    expect(artifactHref).toBeTruthy();
    const csvResp = await request.get(artifactHref!);
    expect(csvResp.ok()).toBe(true);
    const csvBody = await csvResp.text();
    expect(csvBody).toContain("HLA-A*02:01");

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
    await page
      .getByTestId("plugin-card-alithea/hla-interactive-explorer")
      .click();
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

    await expect(page.getByTestId("paid-run-consent")).toContainText("5.00 ACT");
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

    // Zero uncaught page errors across the entire workbench & sandbox session
    expect(pageErrors).toEqual([]);
  });
});
