import { expect, test } from "@playwright/test";

test("native Modules removes the selected release and can restore a detached workspace source", async ({ page }) => {
  const id = "fixture/color";
  const active = new Set(["1.0", "2.0"]);
  const detached = new Set<string>();
  const requests: string[] = [];
  let protectedRun = true;
  let mode = "local";
  function summary(version: string) {
    return {
      manifest: { id, name: "Color sample", version, image: "registry/fixture/color@sha256:" + "a".repeat(64),
        category: "utility", price_milli_eur: 0, network: "none", description: "",
        resources: { cpus: 1, memory_mb: 64, timeout_sec: 15 }, inputs: { schema: {} }, views: [] },
      installed: active.has(version), removable_versions: [...active], detached_versions: [...detached],
      author: "Fixture", execution_target: "local", execution_label: "Local", views: [], has_custom_ui: false,
    };
  }
  await page.route("**/api/v1/**", async (route) => {
    const path = new URL(route.request().url()).pathname.slice("/api/v1/".length);
    const json = (body: unknown, status = 200) => route.fulfill({ status, json: body });
    if (path === "health") return json({ status: "ok", mode, workspace_hub_url: "https://hub.example.test" });
    if (path === "plugins") return json({ plugins: [summary(active.has("2.0") ? "2.0" : "1.0")] });
    if (route.request().method() === "DELETE") {
      expect(path).toBe(`plugins/${id}/versions/1.0/install`);
      requests.push("remove 1.0");
      if (protectedRun) return json({ error: "An active run uses this version." }, 409);
      active.delete("1.0"); detached.add("1.0");
      return json({ plugin_id: id, plugin_version: "1.0", removed: true });
    }
    if (path === `plugins/${id}/install` && route.request().method() === "POST") {
      expect(route.request().postDataJSON()).toEqual({ plugin_version: "1.0", source: "workspace" });
      requests.push("restore 1.0");
      active.add("1.0"); detached.delete("1.0");
      return json({ installed: true, plugin_id: id, plugin_version: "1.0", plugin: summary("1.0") });
    }
    const fixtures: Record<string, unknown> = {
      "auth/whoami": { org_id: "local", actor: "tester", role: "owner" },
      catalogs: { catalogs: [] }, grants: { grants: [], grant_usage: [] },
      ledger: { transactions: [], available_milli_eur: 0, reserved_milli_eur: 0 },
      projects: { projects: [] }, runs: { runs: [] }, sessions: { sessions: [] },
      "saved-views": { views: [] }, "llm/config": { configured: false, providers: [] },
    };
    return Object.hasOwn(fixtures, path) ? json(fixtures[path]) : json({ error: "unused fixture" }, 404);
  });
  await page.goto("/");
  await page.getByTestId("nav-plugins").click();
  const manage = page.getByTestId(`manage-installed-${id}`);
  await manage.click();
  const dialog = page.getByRole("dialog", { name: "Remove Color sample from this workspace?" });
  await expect(dialog).toBeVisible();
  await expect(dialog.getByText(/Saved runs and results remain available/)).toBeVisible();
  await expect(page.getByTestId("remove-version")).toHaveValue("2.0");
  await dialog.getByRole("button", { name: "Cancel" }).click();
  expect(requests).toEqual([]);

  await manage.click();
  await page.getByTestId("remove-version").selectOption("1.0");
  await page.getByTestId("confirm-remove-release").click();
  await expect(dialog.getByRole("alert")).toHaveText("An active run uses this version.");
  await expect(page.getByTestId("remove-version")).toHaveValue("1.0");
  protectedRun = false;
  await page.getByTestId("confirm-remove-release").click();
  await expect(dialog).toBeHidden();
  await expect(page.getByRole("status").filter({ hasText: "Removed fixture/color v1.0" })).toBeVisible();
  const card = page.getByTestId(`plugin-card-${id}`);
  await expect(card.getByRole("button", { name: "Open" })).toBeVisible();
  await manage.click();
  await expect(page.getByTestId("remove-version").locator("option")).toHaveText(["2.0"]);
  await page.keyboard.press("Escape");
  await page.getByTestId(`restore-source-${id}`).click();
  await expect(page.getByTestId("restore-version")).toHaveValue("1.0");
  await page.getByTestId("confirm-restore-release").click();
  await expect(page.getByTestId(`module-window-${id}`).locator(".bc-window-version")).toHaveText("v1.0");
  expect(requests).toEqual(["remove 1.0", "remove 1.0", "restore 1.0"]);

  mode = "cloud-web";
  await page.reload();
  await page.getByTestId("nav-plugins").click();
  await expect(card).toBeVisible();
  await expect(manage).toHaveCount(0);
});

test("Results opens the saved run by ID after the exact module version is unavailable", async ({ page }) => {
  const id = "fixture/color";
  const runId = "run-saved-version-one";
  const calls: string[] = [];
  let onlySiblingInstalled = true;
  let canRead = true;
  const record = {
    plugin_id: id, plugin_version: "1.0", run_id: runId, status: "succeeded", params: {},
    quote: { estimated_cost_milli_eur: 0 }, artifacts: [{ name: "marker.bin", size_bytes: 3 }],
    result_json: { exact_value: 17 }, froglet_receipt: { hash: "signed-archive-receipt" },
    froglet_chain: [{ hash: "signed-archive-receipt" }], receipt_verified: true,
    logs: [], created_at: "2026-11-19T08:00:00Z", completed_at: "2026-11-19T08:00:04Z",
  };
  await page.route("**/api/v1/**", async (route) => {
    const path = new URL(route.request().url()).pathname.slice("/api/v1/".length);
    const json = (body: unknown, status = 200) => route.fulfill({ status, json: body });
    if (path === "plugins") return json({ plugins: onlySiblingInstalled ? [{
      manifest: { id, name: "Newer Color", version: "2.0", description: "", category: "utility",
        price_milli_eur: 0, resources: {}, inputs: { schema: {} } },
      installed: true, execution_target: "local", execution_label: "Local", has_custom_ui: true,
      views: [], author: "Fixture",
    }] : [] });
    if (path === "runs") return json({ runs: [{ ...record, result_json: null, froglet_receipt: null }] });
    if (path === "runs/" + runId) {
      calls.push(path);
      return canRead ? json(record) : json({ error: "This run is unavailable." }, 403);
    }
    const settings: Record<string, unknown> = {
      health: { status: "ok", mode: "local" }, "auth/whoami": { org_id: "test", actor: "tester", role: "owner" },
      catalogs: { catalogs: [] }, grants: { grants: [], grant_usage: [] },
      ledger: { transactions: [], available_milli_eur: 0, reserved_milli_eur: 0 },
      projects: { projects: [] }, sessions: { sessions: [] }, "saved-views": { views: [] },
      "llm/config": { configured: false, providers: [] },
    };
    return Object.hasOwn(settings, path) ? json(settings[path]) : json({ error: "unused fixture" }, 404);
  });

  for (const sibling of [true, false]) {
    onlySiblingInstalled = sibling;
    await page.goto("/");
    await page.getByTestId("nav-results").click();
    await expect(page.getByRole("button", { name: "Open App" })).toHaveCount(0);
    await page.getByTestId(`open-results-${runId}`).click();
    const viewer = page.getByTestId("saved-run-dialog");
    await expect(viewer.getByText(id + " v1.0")).toBeVisible();
    await expect(viewer.getByTestId("live-run-logs")).toHaveText("No execution logs were recorded.");
    await expect(viewer.getByTestId("saved-run-json")).toContainText('"exact_value": 17');
    await viewer.getByText("Saved receipt", { exact: true }).click();
    await expect(viewer.getByTestId("saved-run-receipt")).toContainText("signed-archive-receipt");
    await expect(viewer.getByRole("link", { name: /marker.bin/ })).toHaveAttribute(
      "href", `/api/v1/runs/${runId}/artifacts/marker.bin`);
    await viewer.getByRole("button", { name: "Close" }).click();
  }
  for (const status of ["failed", "canceled", "prepared", "running"]) {
    record.status = status;
    await page.getByTestId(`open-results-${runId}`).click();
    const viewer = page.getByTestId("saved-run-dialog");
    await expect(viewer.getByTestId("live-run-logs")).toHaveText(
      status === "prepared" || status === "running" ? "Waiting for container output..." : "No execution logs were recorded.",
    );
    await viewer.getByRole("button", { name: "Close" }).click();
  }
  canRead = false;
  await page.getByTestId(`open-results-${runId}`).click();
  await expect(page.getByTestId("saved-run-dialog").getByRole("alert")).toHaveText("This run is unavailable.");
  await expect(page.getByTestId("saved-run-json")).toHaveCount(0);
  expect(calls.length).toBeGreaterThanOrEqual(3);
});
