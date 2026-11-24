import { expect, test } from "@playwright/test";

test("a native workspace installs and opens the chosen downloadable release without a dataset grant", async ({ page }) => {
  const pluginId = "fixture/color";
  const digest = (version: string) => `sha256:${version[0]!.repeat(64)}`;
  const versions = ["2.0.0", "1.0.0"];
  const installed: string[] = [];
  let wrongRelease = false;
  let connected = true;

  // The browser consumes the normal API. Keep the fixture independent of a real
  // registry, organization, or Docker daemon, and start with zero installed modules.
  await page.route("**/api/v1/**", async (route) => {
    const endpoint = new URL(route.request().url()).pathname.slice("/api/v1/".length);
    const json = (body: unknown, status = 200) => route.fulfill({ status, json: body });
    if (endpoint === "health") {
      return json({ mode: "local", status: "ok", workspace_hub_url: connected ? "https://hub.example.test" : null });
    }
    if (endpoint === "workspace/modules") {
      if (route.request().headers().authorization !== "Bearer fixture-session") return json({ error: "Sign in" }, 401);
      return json({ plugins: versions.map((version) => ({ name: "Color sample", plugin_id: pluginId, version, image_digest: digest(version) })) });
    }
    if (endpoint === "workspace/auth/login") {
      expect(route.request().postDataJSON().email).toBe("reader@example.test");
      return json({ session_token: "fixture-session" });
    }
    if (endpoint === `plugins/${pluginId}/install`) {
      expect(route.request().headers().authorization).toBe("Bearer fixture-session");
      const { plugin_version } = route.request().postDataJSON();
      installed.push(plugin_version);
      const version = wrongRelease ? "2.0.0" : plugin_version;
      return json({ plugin_id: pluginId, plugin_version, installed: true, plugin: {
        manifest: { id: pluginId, name: "Color sample", version, image: `fixture/color@${digest(version)}`,
          description: "", category: "utility", network: "none", price_milli_eur: 0,
          resources: { cpus: 1, memory_mb: 64, timeout_sec: 15 }, inputs: { schema: {} }, views: [] },
        installed: true, views: [], effective_offers: [], author: "Fixture", execution_target: "local",
        execution_label: "Local", has_custom_ui: true, ui_entry_url: `/test-fixture/form/${version}`,
      } });
    }
    const fixtures: Record<string, unknown> = {
      "auth/whoami": { org_id: "local", actor: "local-user", role: "owner" },
      catalogs: { catalogs: [] }, grants: { grants: [], grant_usage: [] }, plugins: { plugins: [] },
      ledger: { transactions: [], available_milli_eur: 0, reserved_milli_eur: 0 },
      projects: { projects: [] }, runs: { runs: [] }, sessions: { sessions: [] },
      "saved-views": { views: [] }, "llm/config": { configured: false, providers: [] },
    };
    return Object.hasOwn(fixtures, endpoint) ? json(fixtures[endpoint]) : json({ error: "fixture not found" }, 404);
  });
  await page.route("**/test-fixture/form/*", (route) => route.fulfill({
    contentType: "text/html", body: `<h1>${new URL(route.request().url()).pathname.split("/").at(-1)}</h1>`,
  }));
  await page.goto("/");
  await page.getByTestId("nav-plugins").click();
  await page.getByTestId("workspace-marketplace").click();
  await page.getByLabel("Email", { exact: true }).fill("reader@example.test");
  await page.getByLabel("Password", { exact: true }).fill("fixture");
  await page.getByRole("button", { name: "Sign in", exact: true }).click();
  await expect(page.getByTestId("download-module")).toHaveValue(pluginId);
  const win = page.getByTestId(`module-window-${pluginId}`);
  for (const version of versions) {
    await page.getByTestId("download-version").selectOption(version);
    await page.getByTestId("install-release").click();
    await expect(win.locator(".bc-window-version")).toHaveText(`v${version}`);
    await expect(win.frameLocator("iframe").getByRole("heading")).toHaveText(version);
    await win.getByRole("button", { name: /close/i }).first().click();
  }
  wrongRelease = true;
  await page.getByTestId("install-release").click();
  await expect(page.getByRole("alert")).toContainText("does not match the selected release");
  await expect(win).toHaveCount(0);
  expect(installed).toEqual(["2.0.0", "1.0.0", "1.0.0"]);

  connected = false;
  await page.reload();
  await page.getByTestId("nav-plugins").click();
  await expect(page.getByRole("heading", { name: "Modules", exact: true })).toBeVisible();
  await expect(page.getByTestId("workspace-marketplace")).toHaveCount(0);
});
