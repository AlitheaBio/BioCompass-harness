import { expect, test, type Page } from "@playwright/test";
import { readFileSync, writeFileSync } from "node:fs";

/**
 * U-3 performance proof against REAL processes, driven by `e2e/u3_real_hubs/run.sh`:
 * two compass-hub processes (hub B is an approved federated provider of hub A), the real
 * `biocompass` harness with an empty UI cache and pre-fetch disabled (true cold path), and a
 * TCP proxy adding latency in front of both hubs. The timer starts at the Open click and stops
 * when the module UI has completed the plugin SDK `biocompass.getContext` handshake.
 *
 * Skipped unless `U3_REAL_HUBS=1` (set by run.sh); the mock-based Track U spec is a UI-flow test.
 */
const enabled = process.env["U3_REAL_HUBS"] === "1";
const CLOUD = "alithea/u3-cloud";
const NETWORK = "alithea/u3-network";
const LOCAL = "alithea/hla-interactive-explorer";

async function installListener(page: Page): Promise<void> {
  await page.evaluate(() => {
    const w = window as unknown as {
      __u3: { kind: string; plugin?: string; t: number }[];
    };
    if (w.__u3) return;
    w.__u3 = [];
    window.addEventListener("message", (e: MessageEvent) => {
      const d = e.data as Record<string, unknown> | null;
      if (d && d["u3ready"] === true) {
        w.__u3.push({ kind: "ready", plugin: String(d["plugin"]), t: performance.now() });
      } else if (d && d["method"] === "biocompass.getContext") {
        w.__u3.push({ kind: "getContext", t: performance.now() });
      }
    });
  });
}

/** Click Open in-page and return ms until the module completed (or, for `local`, issued) getContext. */
async function measureOpen(page: Page, pluginId: string, stopOn: "ready" | "getContext"): Promise<number> {
  await installListener(page);
  return page.evaluate(
    async ({ pluginId, stopOn }) => {
      const w = window as unknown as { __u3: { kind: string; plugin?: string; t: number }[] };
      const btn = document.querySelector<HTMLButtonElement>(`[data-testid="module-open-btn-${pluginId}"]`);
      if (!btn) throw new Error(`no Open button for ${pluginId}`);
      const t0 = performance.now();
      btn.click();
      const deadline = t0 + 15_000;
      for (;;) {
        const hit = w.__u3.find(
          (m) => m.t > t0 && m.kind === stopOn && (stopOn !== "ready" || m.plugin === pluginId),
        );
        if (hit) return hit.t - t0;
        if (performance.now() > deadline) throw new Error(`${pluginId} never completed getContext`);
        await new Promise((r) => setTimeout(r, 2));
      }
    },
    { pluginId, stopOn },
  );
}

async function closeWindow(page: Page, pluginId: string): Promise<void> {
  const win = page.getByTestId(`module-window-${pluginId}`);
  await win.getByRole("button", { name: /close/i }).first().click();
  await expect(win).toHaveCount(0);
}

test.describe("U-3 real two-hub timing", () => {
  test.skip(!enabled, "run via e2e/u3_real_hubs/run.sh (U3_REAL_HUBS=1)");

  test("cloud and federated network module UIs are usable within 1 s, cold and warm", async ({ page, request, baseURL }) => {
    const base = baseURL!;
    const link = readFileSync(process.env["U3_OWNER_LINK_FILE"]!, "utf8").trim();
    const ticket = link.split("#local-owner=")[1];
    const redeem = await request.post(`${base}/api/v1/local/owner-session`, {
      headers: { Origin: base, "Content-Type": "application/json" },
      data: { ticket },
    });
    expect(redeem.status()).toBe(200);
    const owner = (await redeem.json())["owner_session"] as string;

    // Connect the local client to hub A in-app (stored hub token only, U-2).
    const sw = await request.put(`${base}/api/v1/workspace/hub`, {
      headers: { "x-biocompass-local-owner": owner, "Content-Type": "application/json", Origin: base },
      data: { mode: "alithea_cloud", auth_bearer: process.env["U3_HUB_A_KEY"] },
    });
    expect(sw.status(), await sw.text()).toBe(200);
    const status = await sw.json();
    expect(status.hub_key_status).toBe("pinned");

    await page.goto(base);
    const declineBtn = page.getByTestId("marketplace-consent-decline");
    if (await declineBtn.isVisible({ timeout: 2_000 }).catch(() => false)) await declineBtn.click();
    await page.getByTestId("nav-plugins").click();
    await expect(page.getByTestId(`module-open-btn-${CLOUD}`)).toBeVisible({ timeout: 30_000 });
    await expect(page.getByTestId(`module-open-btn-${NETWORK}`)).toBeVisible();
    await expect(page.getByTestId(`module-source-badge-${NETWORK}`)).toHaveAttribute("data-source", "network_provider");

    // The cold network open must not be satisfied by the cloud module's cached bundle.
    const listed = await (await request.get(`${base}/api/v1/plugins`)).json();
    const hashOf = (id: string) =>
      (listed.plugins as { manifest: { id: string }; ui_bundle_hash?: string }[]).find((p) => p.manifest.id === id)?.ui_bundle_hash;
    expect(hashOf(CLOUD)).toBeTruthy();
    expect(hashOf(NETWORK)).toBeTruthy();
    expect(hashOf(CLOUD)).not.toBe(hashOf(NETWORK));

    const results: Record<string, number> = {};
    results["cloud_cold_ms"] = await measureOpen(page, CLOUD, "ready");
    await closeWindow(page, CLOUD);
    results["cloud_warm_ms"] = await measureOpen(page, CLOUD, "ready");
    await closeWindow(page, CLOUD);
    results["network_cold_ms"] = await measureOpen(page, NETWORK, "ready");
    await closeWindow(page, NETWORK);
    results["network_warm_ms"] = await measureOpen(page, NETWORK, "ready");
    await closeWindow(page, NETWORK);
    if (await page.getByTestId(`module-open-btn-${LOCAL}`).count()) {
      results["local_bundled_ms"] = await measureOpen(page, LOCAL, "getContext");
      await closeWindow(page, LOCAL);
    }

    const out = {
      measured_at: new Date().toISOString(),
      one_way_delay_ms: Number(process.env["U3_ONE_WAY_DELAY_MS"] ?? 0),
      stop_condition: "plugin SDK biocompass.getContext reply received by the module UI (local: request issued)",
      ...results,
    };
    console.log(`[U-3 real hubs] ${JSON.stringify(out)}`);
    writeFileSync(process.env["U3_RESULTS_FILE"]!, JSON.stringify(out, null, 2));
    for (const [name, ms] of Object.entries(results)) {
      expect(ms, `${name} = ${ms.toFixed(1)} ms`).toBeLessThan(1000);
    }
  });
});
