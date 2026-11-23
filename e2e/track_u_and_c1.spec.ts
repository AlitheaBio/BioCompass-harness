import { expect, test } from "@playwright/test";
import { createHash } from "node:crypto";
import http from "node:http";
import type { AddressInfo } from "node:net";

function sha256Hex(buf: Buffer | string): string {
  return createHash("sha256").update(buf).digest("hex");
}

function buildUiBundleArchive(
  files: Record<string, string>,
  entry = "index.html",
): { hash: string; body: string } {
  const sortedPaths = Object.keys(files).sort();
  const inventory: Record<string, [string, number]> = {};
  const entries = sortedPaths.map((p) => {
    const raw = Buffer.from(files[p]!, "utf8");
    const fileSha = sha256Hex(raw);
    inventory[p] = [fileSha, raw.length];
    return {
      path: p,
      sha256: fileSha,
      size_bytes: raw.length,
      data_b64: raw.toString("base64"),
    };
  });
  // Canonical JSON matches BTreeMap<String, (String, u64)> serialization in Rust.
  const inventoryJson = JSON.stringify(inventory);
  const hash = sha256Hex(inventoryJson);
  const body = JSON.stringify({
    kind: "ui_bundle.v1",
    ui_bundle_hash: hash,
    entry,
    total_size_bytes: entries.reduce((sum, f) => sum + f.size_bytes, 0),
    files: entries,
  });
  return { hash, body };
}

interface MockHubHandle {
  url: string;
  archiveHits: () => number;
  close: () => Promise<void>;
}

async function startMockHub(opts: {
  pluginId: string;
  pluginName: string;
  uiHtml: string;
  networkOffer?: {
    offerId: string;
    pluginId: string;
    title: string;
    provider: string;
    uiHtml: string;
  };
  tamperedHash?: string;
}): Promise<MockHubHandle> {
  const mainBundle = buildUiBundleArchive({
    "index.html": opts.uiHtml,
    "app.js": `document.body.dataset.u1Ready = "1";`,
  });
  const netBundle = opts.networkOffer
    ? buildUiBundleArchive({
        "index.html": opts.networkOffer.uiHtml,
        "app.js": `document.body.dataset.netReady = "1";`,
      })
    : null;
  let archiveFetches = 0;

  const server = http.createServer((req, res) => {
    const urlPath = (req.url ?? "/").split("?")[0] ?? "/";
    if (urlPath === "/api/v1/health") {
      res.writeHead(200, { "Content-Type": "application/json" });
      res.end(
        JSON.stringify({
          status: "ok",
          node_id: "mock-hub",
          provider_id: "provider-mock",
          requester_id: "requester-mock",
          container_engine: "kube-runner",
        }),
      );
      return;
    }
    if (urlPath === "/api/v1/plugins") {
      res.writeHead(200, { "Content-Type": "application/json" });
      res.end(
        JSON.stringify({
          plugins: [
            {
              manifest: {
                id: opts.pluginId,
                name: opts.pluginName,
                version: "1.0.0",
                description: `Hosted module from ${opts.pluginName}`,
                category: "Proteomics",
                execution: "cloud_only",
                image: `registry.example.org/mod@sha256:${"a".repeat(64)}`,
                price_milli_act: 2500,
                ui: { entry: "ui/dist/index.html" },
              },
              has_ui_bundle: true,
              source: "org_cloud",
              runs_in: "cloud",
              ui_bundle_hash: mainBundle.hash,
              ui_bundle_size_bytes: Buffer.byteLength(mainBundle.body),
              can_install: false,
              installed_locally: false,
            },
            ...(opts.tamperedHash
              ? [
                  {
                    manifest: {
                      id: "security/tampered-ui",
                      name: "Tampered UI Module",
                      version: "1.0.0",
                      description: "Intentionally corrupted UI archive",
                      category: "Security",
                      execution: "cloud_only",
                      image: `registry.example.org/bad@sha256:${"b".repeat(64)}`,
                      price_milli_act: 0,
                      ui: { entry: "ui/dist/index.html" },
                    },
                    has_ui_bundle: true,
                    source: "org_cloud",
                    runs_in: "cloud",
                    ui_bundle_hash: opts.tamperedHash,
                    can_install: false,
                  },
                ]
              : []),
          ],
        }),
      );
      return;
    }
    if (urlPath === "/v1/feed") {
      const offers = opts.networkOffer && netBundle
        ? [
            {
              id: opts.networkOffer.offerId,
              kind: "run",
              title: opts.networkOffer.title,
              plugin_id: opts.networkOffer.pluginId,
              version: "1.0.0",
              provider_org: opts.networkOffer.provider,
              ui_bundle_hash: netBundle.hash,
              launch_url: `https://provider.example.org/offers/${opts.networkOffer.offerId}`,
              price_milli_act: 3000,
            },
          ]
        : [];
      res.writeHead(200, { "Content-Type": "application/json" });
      res.end(JSON.stringify({ offers }));
      return;
    }
    if (urlPath === `/v1/artifacts/${mainBundle.hash}`) {
      archiveFetches += 1;
      res.writeHead(200, { "Content-Type": "application/json" });
      res.end(mainBundle.body);
      return;
    }
    if (netBundle && urlPath === `/v1/artifacts/${netBundle.hash}`) {
      archiveFetches += 1;
      res.writeHead(200, { "Content-Type": "application/json" });
      res.end(netBundle.body);
      return;
    }
    if (opts.tamperedHash && urlPath === `/v1/artifacts/${opts.tamperedHash}`) {
      res.writeHead(200, { "Content-Type": "application/json" });
      // Return valid JSON whose hash does NOT match opts.tamperedHash -> triggers UI_BUNDLE_TAMPERED!
      res.end(mainBundle.body);
      return;
    }
    res.writeHead(404);
    res.end("not found");
  });

  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const { port } = server.address() as AddressInfo;
  return {
    url: `http://127.0.0.1:${port}`,
    archiveHits: () => archiveFetches,
    close: () => new Promise<void>((resolve) => server.close(() => resolve())),
  };
}

test.describe("Track U (U1, U2, U3) & Milestone C1 Enterprise Web UI", () => {
  test("U1 <1s verified UI cache (local, cloud, network + tamper refusal), U2 button semantics, U3 hub switch, H3 play credits", async ({
    page,
  }) => {
    const tamperedHash = "e".repeat(64);
    const hubA = await startMockHub({
      pluginId: "hub-a/proteomics-cloud",
      pluginName: "Hub A Proteomics Cloud",
      uiHtml:
        '<!doctype html><html><body><main data-testid="u1-ui-ready">Hub A Verified UI Loaded</main><script src="app.js"></script></body></html>',
      networkOffer: {
        offerId: "mo-zurich-net-01",
        pluginId: "zurich/net-motif",
        title: "Zurich Network Motif",
        provider: "Zurich Proteomics",
        uiHtml:
          '<!doctype html><html><body><main data-testid="u1-net-ready">Zurich Network UI Loaded</main><script src="app.js"></script></body></html>',
      },
      tamperedHash,
    });
    const hubB = await startMockHub({
      pluginId: "hub-b/genomics-cloud",
      pluginName: "Hub B Genomics Cloud",
      uiHtml:
        '<!doctype html><html><body><main data-testid="u1-hub-b-ready">Hub B Verified UI Loaded</main><script src="app.js"></script></body></html>',
    });

    try {
      const ownerLink = process.env["BIOCOMPASS_E2E_OWNER_LINK"] ?? "/";
      await page.goto(ownerLink);
      // Dismiss first-run marketplace consent modal if present.
      const declineBtn = page.getByTestId("marketplace-consent-decline");
      if (await declineBtn.isVisible({ timeout: 2_000 }).catch(() => false)) {
        await declineBtn.click();
      }

      // 1. Verify H3 local play ledger is NOT labelled euros.
      const ledgerResp = await page.request.get("/api/v1/ledger");
      expect(ledgerResp.ok()).toBe(true);
      const ledger = (await ledgerResp.json()) as {
        available_display: string;
        is_local_play_ledger?: boolean;
      };
      expect(ledger.is_local_play_ledger).toBe(true);
      expect(ledger.available_display).not.toContain("€");
      expect(ledger.available_display.toUpperCase()).not.toContain("EUR");
      expect(ledger.available_display).toMatch(/credits$/);

      await page.getByTestId("nav-plugins").click();
      await expect(page.getByTestId("workspace-hub-chooser")).toBeVisible();

      // 2. Measure local bundled module click-to-usable UI (< 1000 ms).
      const localCard = page.getByTestId("plugin-card-alithea/hla-interactive-explorer");
      if (await localCard.isVisible().catch(() => false)) {
        await expect(
          page.getByTestId("module-source-badge-alithea/hla-interactive-explorer"),
        ).toHaveText("This workspace");
        await expect(
          page.getByTestId("module-runs-in-badge-alithea/hla-interactive-explorer"),
        ).toHaveText("Local");
        const localStart = performance.now();
        await page.getByTestId("module-open-btn-alithea/hla-interactive-explorer").click();
        const localWin = page.getByTestId("module-window-alithea/hla-interactive-explorer");
        await expect(localWin.locator("iframe")).toBeVisible();
        await expect(localWin.frameLocator("iframe").locator("body")).not.toBeEmpty();
        expect(performance.now() - localStart).toBeLessThan(1000);
        await localWin.getByRole("button", { name: /close/i }).first().click();
      }

      // 3. U3: Switch to Hub A in-app via WorkspaceHubChooser!
      await page.getByTestId("workspace-hub-mode-select").selectOption("custom_cloud");
      await page.getByTestId("workspace-hub-custom-url").fill(hubA.url);
      await page.getByTestId("workspace-hub-connect-btn").click();

      const modACard = page.getByTestId("plugin-card-hub-a/proteomics-cloud");
      await expect(modACard).toBeVisible({ timeout: 10_000 });
      await expect(page.getByTestId("module-source-badge-hub-a/proteomics-cloud")).toHaveText(
        "My org cloud",
      );
      await expect(page.getByTestId("module-runs-in-badge-hub-a/proteomics-cloud")).toHaveText(
        "Cloud",
      );
      // U2: Install button MUST NOT show for a non-download cloud offer; Open MUST show!
      await expect(page.getByTestId("module-install-btn-hub-a/proteomics-cloud")).toHaveCount(0);
      await expect(page.getByTestId("module-open-btn-hub-a/proteomics-cloud")).toBeVisible();

      // U1 Cold/Prefetched Open (< 1000 ms click-to-usable UI!)
      const cloudOpenStart = performance.now();
      await page.getByTestId("module-open-btn-hub-a/proteomics-cloud").click();
      const modAWin = page.getByTestId("module-window-hub-a/proteomics-cloud");
      await expect(
        modAWin.frameLocator("iframe").getByTestId("u1-ui-ready"),
      ).toHaveText("Hub A Verified UI Loaded");
      expect(performance.now() - cloudOpenStart).toBeLessThan(1000);
      await modAWin.getByRole("button", { name: /close/i }).first().click();

      // U1 Warm Open (< 1000 ms, 0 extra archive fetches!)
      const hitsBeforeWarm = hubA.archiveHits();
      const warmStart = performance.now();
      await page.getByTestId("module-open-btn-hub-a/proteomics-cloud").click();
      await expect(
        modAWin.frameLocator("iframe").getByTestId("u1-ui-ready"),
      ).toHaveText("Hub A Verified UI Loaded");
      expect(performance.now() - warmStart).toBeLessThan(1000);
      expect(hubA.archiveHits()).toBe(hitsBeforeWarm);
      await modAWin.getByRole("button", { name: /close/i }).first().click();

      // U1 Tampered UI bundle MUST be refused with 409 UI_BUNDLE_TAMPERED!
      const tamperResp = await page.request.get(
        "/plugin-ui-version/security/tampered-ui/v1.0.0/index.html",
        { headers: { "Sec-Fetch-Dest": "iframe" } },
      );
      expect(tamperResp.status()).toBe(409);
      const tamperJson = (await tamperResp.json()) as { code: string };
      expect(tamperJson.code).toBe("UI_BUNDLE_TAMPERED");

      // U2: Network provider run-only module shows Open + "Runs at Zurich Proteomics" + run handoff!
      const netCard = page.getByTestId("plugin-card-zurich/net-motif");
      await expect(netCard).toBeVisible();
      await expect(page.getByTestId("module-runs-in-badge-zurich/net-motif")).toHaveText(
        "Runs at Zurich Proteomics",
      );
      await expect(page.getByTestId("module-install-btn-zurich/net-motif")).toHaveCount(0);
      const netStart = performance.now();
      await page.getByTestId("module-open-btn-zurich/net-motif").click();
      const netWin = page.getByTestId("module-window-zurich/net-motif");
      await expect(netWin.getByTestId("network-run-handoff-banner")).toContainText(
        "Runs at Zurich Proteomics",
      );
      await expect(
        netWin.frameLocator("iframe").getByTestId("u1-net-ready"),
      ).toHaveText("Zurich Network UI Loaded");
      expect(performance.now() - netStart).toBeLessThan(1000);
      await netWin.getByRole("button", { name: /close/i }).first().click();

      // 4. U3: Switch in-app to Hub B -> Hub A module disappears, Hub B module appears and opens in < 1000 ms!
      await page.getByTestId("workspace-hub-custom-url").fill(hubB.url);
      await page.getByTestId("workspace-hub-connect-btn").click();
      await expect(page.getByTestId("plugin-card-hub-b/genomics-cloud")).toBeVisible({
        timeout: 10_000,
      });
      await expect(page.getByTestId("plugin-card-hub-a/proteomics-cloud")).toHaveCount(0);

      const hubBStart = performance.now();
      await page.getByTestId("module-open-btn-hub-b/genomics-cloud").click();
      const modBWin = page.getByTestId("module-window-hub-b/genomics-cloud");
      await expect(
        modBWin.frameLocator("iframe").getByTestId("u1-hub-b-ready"),
      ).toHaveText("Hub B Verified UI Loaded");
      expect(performance.now() - hubBStart).toBeLessThan(1000);
    } finally {
      await hubA.close();
      await hubB.close();
    }
  });

  test("Milestone C1 Enterprise Web UI: EUR display, Agent Credentials, Statements, Webhooks, Documents conflict, Provenance, U4 Import", async ({
    page,
  }) => {
    const credentials: Array<Record<string, unknown>> = [];
    const webhooks: Array<Record<string, unknown>> = [];
    let docVersion = 1;
    let simulateConflictOnce = true;

    await page.route("**/api/v1/**", async (route) => {
      const req = route.request();
      const method = req.method();
      const pathname = new URL(req.url()).pathname.slice("/api/v1/".length);
      const json = (body: unknown, status = 200) => route.fulfill({ json: body, status });

      if (pathname === "health") {
        return json({
          status: "ok",
          version: "0.2.0",
          mode: "cloud-web",
          node_id: "hub-c1",
          provider_id: "alithea",
          requester_id: "alithea",
          container_engine: "kube-runner",
          llm_configured: false,
          llm_model: null,
          catalog_count: 1,
          plugin_count: 0,
        });
      }
      if (pathname === "auth/whoami") {
        return json({
          mode: "cloud-web",
          org_id: "org-pharma",
          user_id: "user-1",
          actor: "admin@pharma.bio",
          email: "admin@pharma.bio",
          role: "owner",
          tier_id: "enterprise",
          is_platform_admin: true,
          auth_method: "session",
        });
      }
      if (pathname === "ledger") {
        return json({
          org_id: "org-pharma",
          tier_id: "enterprise",
          available_milli_act: 42_500,
          available_display: "€42.50",
          daily_act_spent_milli: 5_000,
          daily_act_breaker_milli: 50_000,
          transactions: [],
        });
      }
      if (pathname === "agent-credentials" && method === "GET") {
        return json({ credentials });
      }
      if (pathname === "agent-credentials" && method === "POST") {
        const body = req.postDataJSON() as Record<string, unknown>;
        const cred = {
          credential_id: "cred-1",
          org_id: "org-pharma",
          label: body.label,
          role: body.role ?? "developer",
          token_prefix: "bc_agent_abc123",
          allowed_catalogs: body.allowed_catalogs ?? null,
          allowed_modules: body.allowed_modules ?? null,
          daily_ceiling_milli_act: body.daily_ceiling_milli_act ?? null,
          total_ceiling_milli_act: body.total_ceiling_milli_act ?? null,
          spent_total_milli_act: 0,
          burst_per_minute: body.burst_per_minute ?? 60,
          expires_at: "2027-01-01T00:00:00Z",
          created_by: "admin@pharma.bio",
          created_at: "2026-11-23T00:00:00Z",
          revoked_at: null,
          last_used_at: null,
        };
        credentials.push(cred);
        return json({ credential: cred, raw_token: "bc_agent_abc123_secret_demo_token" }, 201);
      }
      if (pathname === "agent-credentials/cred-1" && method === "DELETE") {
        if (credentials[0]) credentials[0].revoked_at = "2026-11-23T01:00:00Z";
        return json({ revoked: true, credential_id: "cred-1" });
      }
      if (pathname === "billing/statement") {
        return json({
          org_id: "org-pharma",
          currency: "EUR",
          totals: {
            run_count: 3,
            total_milli_eur: 12_500,
            module_fee_milli_eur: 10_000,
            dataset_fee_milli_eur: 2_500,
            refunded_milli_eur: 0,
          },
          by_module: [{ key: "alithea/hla-motif-enrichment", run_count: 3, total_milli_eur: 10_000 }],
          by_dataset: [{ key: "alithea/hla-atlas-v2026.1", run_count: 3, total_milli_eur: 2_500 }],
          by_credential: [{ key: "cred-1", label: "Research MCP Agent", run_count: 3, total_milli_eur: 12_500 }],
          entries: [
            {
              run_id: "run-c1-001",
              plugin_id: "alithea/hla-motif-enrichment",
              agent_credential_id: "cred-1",
              status: "succeeded",
              total_milli_eur: 5_000,
              module_fee_milli_eur: 4_000,
              dataset_fee_milli_eur: 1_000,
              receipt_hash: "a".repeat(64),
              created_at: "2026-11-23T12:00:00Z",
            },
          ],
        });
      }
      if (pathname === "webhooks" && method === "GET") {
        return json({ endpoints: webhooks });
      }
      if (pathname === "webhooks" && method === "POST") {
        const body = req.postDataJSON() as Record<string, unknown>;
        const ep = {
          endpoint_id: "wh-1",
          org_id: "org-pharma",
          label: body.label,
          url: body.url,
          event_types: body.event_types,
          enabled: true,
          created_by: "admin@pharma.bio",
          created_at: "2026-11-23T00:00:00Z",
          updated_at: "2026-11-23T00:00:00Z",
        };
        webhooks.push(ep);
        return json({ endpoint: ep, signing_secret: "whsec_demo_hmac_key" }, 201);
      }
      if (pathname === "webhooks/wh-1/deliveries") {
        return json({
          deliveries: [
            {
              delivery_id: "del-1",
              event_id: 1,
              endpoint_id: "wh-1",
              event_type: "run.succeeded",
              status: "delivered",
              attempt_count: 1,
              max_attempts: 8,
              last_http_status: 200,
              delivered_at: "2026-11-23T12:01:00Z",
              created_at: "2026-11-23T12:01:00Z",
              updated_at: "2026-11-23T12:01:00Z",
            },
          ],
        });
      }
      if (pathname === "documents" && method === "GET") {
        return json({
          documents: [
            {
              id: "doc-nb-1",
              org_id: "org-pharma",
              plugin_id: "alithea/bc-jupyter-notebook",
              title: "Cohort Analysis Notebook",
              latest_version: docVersion,
              created_by: "admin@pharma.bio",
              created_at: "2026-11-23T00:00:00Z",
              updated_at: "2026-11-23T00:00:00Z",
            },
          ],
        });
      }
      if (pathname === "documents/doc-nb-1" && method === "GET") {
        return json({
          document: {
            id: "doc-nb-1",
            org_id: "org-pharma",
            plugin_id: "alithea/bc-jupyter-notebook",
            title: "Cohort Analysis Notebook",
            latest_version: docVersion,
            created_by: "admin@pharma.bio",
            created_at: "2026-11-23T00:00:00Z",
            updated_at: "2026-11-23T00:00:00Z",
          },
          version: {
            doc_id: "doc-nb-1",
            version: docVersion,
            content_json: { cells: [{ source: `version ${docVersion}` }] },
            content_sha256: "c".repeat(64),
            base_version: docVersion - 1,
            created_at: "2026-11-23T00:00:00Z",
          },
        });
      }
      if (pathname === "documents/doc-nb-1/versions") {
        return json({
          versions: [
            {
              doc_id: "doc-nb-1",
              version: docVersion,
              content_json: { cells: [] },
              content_sha256: "c".repeat(64),
              base_version: docVersion - 1,
              created_at: "2026-11-23T00:00:00Z",
            },
          ],
        });
      }
      if (pathname === "documents/doc-nb-1" && method === "PUT") {
        if (simulateConflictOnce) {
          simulateConflictOnce = false;
          docVersion = 2;
          return json(
            { error: "document version conflict", code: "DOC_VERSION_CONFLICT" },
            409,
          );
        }
        docVersion += 1;
        return json({
          document: {
            id: "doc-nb-1",
            org_id: "org-pharma",
            plugin_id: "alithea/bc-jupyter-notebook",
            title: "Cohort Analysis Notebook",
            latest_version: docVersion,
            created_by: "admin@pharma.bio",
            created_at: "2026-11-23T00:00:00Z",
            updated_at: "2026-11-23T00:00:00Z",
          },
          version: {
            doc_id: "doc-nb-1",
            version: docVersion,
            content_json: req.postDataJSON().content,
            content_sha256: "d".repeat(64),
            base_version: docVersion - 1,
            created_at: "2026-11-23T00:00:00Z",
          },
        });
      }
      if (pathname === "admin/federation/register-outbound") {
        return json({ status: "pending", message: "Outbound registration submitted to Alithea hub." });
      }
      if (pathname === "catalogs/org-pharma/study-1/placement" && method === "PATCH") {
        return json({ catalog_id: "org-pharma/study-1", data_placement: "licensable_copy" });
      }

      const defaults: Record<string, unknown> = {
        catalogs: {
          catalogs: [
            {
              id: "org-pharma/study-1",
              name: "Study 1",
              version: "1.0.0",
              description: "Protected cohort",
              latest_snapshot_id: 1,
              dataset_digest: "a".repeat(64),
              row_count: 100,
              tables: [],
              facets: {},
              snapshots: [],
            },
          ],
        },
        grants: { grants: [], grant_usage: [] },
        plugins: { plugins: [] },
        projects: { projects: [] },
        runs: { runs: [] },
        sessions: { sessions: [] },
        "saved-views": { views: [] },
        "llm/config": { configured: false, discovered_backends: [] },
        "auth/profile": {
          user_id: "user-1",
          display_name: "Admin",
          email: "admin@pharma.bio",
          avatar_seed: null,
          email_verification: "unavailable",
          requested_email: null,
        },
      };
      return Object.hasOwn(defaults, pathname) ? json(defaults[pathname]) : json({});
    });

    await page.goto("/");
    await page.getByTestId("nav-org-admin").click();

    // 1. H3 EUR display in Org Overview
    await expect(page.getByTestId("org-available-act")).toHaveText("€42.50");
    await expect(page.getByTestId("org-act-left-today")).toContainText("€45.00 / €50.00");

    // 2. H1 Agent Credentials
    await page.getByTestId("org-tab-agent-credentials").click();
    await page.getByTestId("agent-cred-label-input").fill("Research MCP Agent");
    await page.getByTestId("agent-cred-daily-cap-input").fill("5.00");
    await page.getByTestId("agent-cred-create-btn").click();
    await expect(page.getByTestId("agent-credential-raw-token")).toHaveText(
      "bc_agent_abc123_secret_demo_token",
    );
    await expect(page.getByTestId("agent-cred-row-cred-1")).toContainText("€5.00");
    await page.getByTestId("agent-cred-revoke-cred-1").click();
    await expect(page.getByTestId("agent-cred-row-cred-1")).toContainText("Revoked");

    // 3. H4 Statements in EUR
    await page.getByTestId("org-tab-statements").click();
    await expect(page.getByTestId("statement-totals-grid")).toContainText("€12.50");
    await expect(page.getByTestId("billing-statement-panel")).toContainText("Research MCP Agent");

    // 4. H4 Outbound Webhooks + Delivery logs
    await page.getByTestId("org-tab-webhooks").click();
    await page.getByTestId("webhook-label-input").fill("LIMS Hook");
    await page.getByTestId("webhook-url-input").fill("https://lims.pharma.example/hook");
    await page.getByTestId("webhook-create-btn").click();
    await expect(page.getByTestId("webhook-secret-banner")).toContainText("whsec_demo_hmac_key");
    await page.getByTestId("webhook-row-wh-1").getByRole("button", { name: "Deliveries" }).click();
    await expect(page.getByTestId("webhook-deliveries-panel")).toContainText("delivered");

    // 5. H5 Stored Documents with 409 DOC_VERSION_CONFLICT handling
    await page.getByTestId("org-tab-documents").click();
    await page.getByTestId("document-row-doc-nb-1").getByRole("button", { name: "Open" }).click();
    await page.getByTestId("document-save-btn").click();
    await expect(page.getByTestId("document-version-conflict-banner")).toContainText(
      "DOC_VERSION_CONFLICT",
    );
    await page.getByTestId("document-reload-latest-btn").click();
    await page.getByTestId("document-save-btn").click();
    await expect(page.getByTestId("document-editor-card")).toContainText("v3");

    // 6. U4 Client Cloud Registration & Data Placement
    await page.getByTestId("org-tab-client-cloud-import").click();
    await page.getByTestId("u4-register-outbound-btn").click();
    await expect(page.getByTestId("client-cloud-import-panel")).toContainText(
      "Outbound registration submitted",
    );
    await page.getByTestId("u4-placement-mode-select").selectOption("licensable_copy");
    await page.getByTestId("u4-placement-save-btn").click();
    await expect(page.getByTestId("client-cloud-import-panel")).toContainText("licensable_copy");
  });
});
