import { test, expect } from "@playwright/test";

test("a tab recovers its caller's lost quote and gives the next prepare a new key", async ({ page }) => {
  const module = "example/paint";
  const version = "1.0.0";
  const image = "fixture/paint@sha256:" + "a".repeat(64);
  const manifest = {id:module, name:"Paint", version, description:"", category:"utility", image,
    network:"none", price_milli_act:5000, resources:{cpus:1,memory_mb:64,gpu:0,timeout_sec:30,pids_limit:16},
    inputs:{schema:{}},views:[]};
  const plugin = {manifest,installed:true,views:[],author:"Fixture",has_custom_ui:false,
    execution_target:"cloud",execution_label:"Cloud"};
  let caller = "a";
  const received: {caller:string;key:string}[] = [];
  const durable = new Map<string,string>();
  await page.route("**/api/v1/**", async route => {
    const path = new URL(route.request().url()).pathname.slice("/api/v1/".length);
    if (path === "runs/prepare") {
      const key = route.request().postDataJSON().request_key;
      expect(key).toMatch(/^[a-f\d-]{36}$/);
      received.push({caller,key});
      const unique = caller + ":" + key;
      const run = durable.get(unique) ?? "run-" + (durable.size + 1);
      durable.set(unique,run);
      // The response is lost only after the server recorded A's original request.
      if (received.length === 1) return route.abort("connectionreset");
      return route.fulfill({json:{run_id:run,plugin_id:module,plugin_version:version,image_ref:image,
        target_node_id:"fixture",offer_id:"default",offer_name:"Default",estimated_cost_act:5000,
        workload_hash:"f".repeat(64),froglet_quote:{},expires_at:"2099-01-01T00:00:00Z"}});
    }
    const fixtures: Record<string,unknown> = {
      health:{mode:"cloud-web",status:"ok"},
      "auth/whoami":{org_id:"fixture-org",actor:caller,user_id:caller,auth_method:"session",role:"member"},
      catalogs:{catalogs:[]},grants:{grants:[],grant_usage:[]},plugins:{plugins:[plugin]},
      projects:{projects:[]},runs:{runs:[]},sessions:{sessions:[]},"saved-views":{views:[]},
      ledger:{available_milli_act:20000,reserved_milli_act:0,transactions:[]},"llm/config":{configured:false,providers:[]},
    };
    return route.fulfill({status:path in fixtures ? 200 : 404,json:fixtures[path] ?? {error:"fixture not found"}});
  });
  const prepare = async () => {
    await page.getByTestId("nav-workbench").click();
    await page.getByTestId("quick-prepare-btn-" + module).click();
  };
  await page.goto("/");
  await prepare();
  await expect.poll(() => received.length).toBe(1);
  caller = "b";
  await page.reload();
  await prepare();
  await expect(page.getByTestId("paid-run-consent")).toContainText("run-2");
  expect(received[1]!.key).not.toBe(received[0]!.key);
  caller = "a";
  await page.reload();
  await prepare();
  await expect(page.getByTestId("paid-run-consent")).toContainText("run-1");
  expect(received[2]!.key).toBe(received[0]!.key);
  await page.reload();
  await prepare();
  await expect(page.getByTestId("paid-run-consent")).toContainText("run-3");
  expect(new Set(received.map(r=>r.key)).size).toBe(3);
});
