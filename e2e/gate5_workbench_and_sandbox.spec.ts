import { expect, test } from "@playwright/test";

test.describe("Gate 5 — Standalone Binary Browser E2E, Real OCI Execution & Sandbox Exploit Suite", () => {
  test("verifies BrandWordmark, DuckDB Grant masking, real OCI Prepare->Confirm + Schnorr Receipt, and <iframe sandbox='allow-scripts'> exploit blocks", async ({
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
    await expect(page.getByTestId("health-pill")).toContainText(/docker|podman/i);
    await expect(page.getByTestId("act-balance")).toHaveText("250.00 ACT");

    // 2. Data Catalogs: verify Full Research Access vs Partner Lung Masked Grant enforcement
    await page.getByTestId("nav-catalogs").click();
    await expect(page.getByTestId("catalog-mask-indicator")).toHaveText(
      "Unmasked Amino-Acid Sequences",
    );
    const fullTableText = await page.getByTestId("catalog-table").innerText();
    expect(fullTableText).not.toContain("***");
    // Real IUPAC 8-14mer sequences present in Full Access mode
    expect(fullTableText).toMatch(/\b[ACDEFGHIKLMNPQRSTVWY]{8,14}\b/);

    // Switch to Partner Evaluation Tier (Lung & Tumor Only · Sequences Masked '***')
    const grantSelect = page.getByTestId("grant-select");
    const maskedGrantValue = await grantSelect
      .locator("option", { hasText: /Partner Evaluation Tier/i })
      .getAttribute("value");
    expect(maskedGrantValue).toBeTruthy();

    const queryRespPromise = page.waitForResponse(
      (resp) =>
        resp.url().includes("/api/v1/catalogs/alithea/hla-atlas-v2026.1/query") &&
        resp.request().method() === "POST",
    );
    await grantSelect.selectOption(maskedGrantValue!);
    const queryResp = await queryRespPromise;
    expect(queryResp.ok()).toBe(true);

    const maskedPayload = (await queryResp.json()) as {
      sequence_masked: boolean;
      rows: { sequence: string; organ: string }[];
    };
    expect(maskedPayload.sequence_masked).toBe(true);
    expect(maskedPayload.rows.length).toBeGreaterThan(0);
    for (const row of maskedPayload.rows) {
      expect(row.sequence).toBe("***");
      expect(row.organ).toBe("Lung");
    }

    await expect(page.getByTestId("catalog-mask-indicator")).toHaveText(
      "Sequences Masked ('***')",
    );
    await expect(page.getByTestId("catalog-sql-view")).toContainText(
      "'***'::VARCHAR AS sequence",
    );
    await expect(page.getByTestId("catalog-sql-view")).toContainText(
      "organ IN ('Lung')",
    );
    const maskedTableRows = page.getByTestId("catalog-table").locator("tbody tr");
    const rowCount = await maskedTableRows.count();
    expect(rowCount).toBeGreaterThan(0);
    for (let i = 0; i < rowCount; i++) {
      const cells = maskedTableRows.nth(i).locator("td");
      await expect(cells.nth(1)).toHaveText("***");
      await expect(cells.nth(5)).toHaveText("Lung");
    }

    // Switch back to Full Research Access for Workbench motif run
    const fullGrantValue = await grantSelect
      .locator("option", { hasText: /Full Research Access/i })
      .getAttribute("value");
    expect(fullGrantValue).toBeTruthy();
    await grantSelect.selectOption(fullGrantValue!);

    // 3. Workbench: Prepare -> Approve & Run real OCI container, stream logs, render SVG views & verify Schnorr Receipt
    await page.getByTestId("nav-workbench").click();
    await page
      .getByTestId("quick-prepare-btn-alithea/hla-motif-enrichment")
      .click();

    await expect(page.getByTestId("pending-quote-banner")).toBeVisible();
    await expect(page.getByTestId("pending-quote-banner")).toContainText(
      "alithea/hla-motif-enrichment",
    );
    // Consent invariant: preparing a quote MUST NOT deduct ACT before confirmation
    await expect(page.getByTestId("act-balance")).toHaveText("250.00 ACT");

    await page.getByTestId("confirm-run-btn").click();
    await expect(page.getByTestId("pending-quote-banner")).toBeHidden({
      timeout: 45_000,
    });

    // ACT settled from 250.00 -> 245.00 ACT
    await expect(page.getByTestId("act-balance")).toHaveText("245.00 ACT");
    await expect(page.getByTestId("live-run-logs")).toContainText(
      "hla-motif-enrichment",
    );

    // Native scientific SVG charts & table rendered from /out/result.json
    await expect(page.getByTestId("scientific-views")).toBeVisible();
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
    // Activate Partner Masked Grant to prove the sandboxed iframe also receives masked sequences
    await grantSelect.selectOption(maskedGrantValue!);
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
    await expect(frameLocator.getByTestId("sandbox-mask-badge")).toHaveText(
      "Sequences Masked ('***')",
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
      await expect(sdkPreviewRows.nth(i).locator("td").nth(1)).toHaveText("***");
      await expect(sdkPreviewRows.nth(i).locator("td").nth(4)).toHaveText("Lung");
    }

    // Legitimate @biocompass/plugin-sdk RPC 2: prepareRun -> Host Consent Banner -> Real OCI Container -> Push Result Back to Iframe
    await frameLocator.getByTestId("sdk-prepare-btn").click();
    await expect(frameLocator.getByTestId("sdk-quote-banner")).toBeVisible();
    await expect(page.getByTestId("pending-quote-banner")).toBeVisible();
    await expect(page.getByTestId("pending-quote-banner")).toContainText(
      "alithea/hla-interactive-explorer",
    );
    await expect(page.getByTestId("act-balance")).toHaveText("245.00 ACT");

    // User approves in the host UI
    await page.getByTestId("confirm-run-btn").click();
    await expect(page.getByTestId("pending-quote-banner")).toBeHidden({
      timeout: 45_000,
    });
    await expect(page.getByTestId("act-balance")).toHaveText("240.00 ACT");

    // Host automatically pushes completed RunRecord back into the sandboxed iframe over postMessage!
    const hotspotRows = frameLocator
      .getByTestId("sdk-hotspots-table")
      .locator("tbody tr");
    await expect(hotspotRows.first()).toBeVisible();
    expect(await hotspotRows.count()).toBeGreaterThan(0);

    // Zero uncaught page errors across the entire workbench & sandbox session
    expect(pageErrors).toEqual([]);
  });
});
