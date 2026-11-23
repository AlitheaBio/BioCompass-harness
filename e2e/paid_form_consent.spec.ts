import { test, expect } from "@playwright/test";
import { readFileSync } from "node:fs";
import path from "node:path";

test("paid custom forms wait for consent and confirm only the displayed run", async ({ page }) => {
  const id = "example/paint", pluginVersion = "1.0.0";
  const confirms: Array<{run: string; key?: string}> = [];
  const releases: Record<string, () => void> = {};
  let defaultRequests = 0;
  let willFail = true;
  const sdk = readFileSync(path.resolve("packages/plugin-sdk/dist/index.js"), "utf8");
  const manifest = {id, name:"Color sample", version:pluginVersion, description:"", category:"utility",
    image:"fixture/paint@sha256:"+"a".repeat(64), network:"none", price_milli_act:5000,
    resources:{cpus:1,memory_mb:64,gpu:0,timeout_sec:30,pids_limit:16},inputs:{schema:{}},views:[]};
  const plugin = {manifest, installed:true, views:[], author:"Fixture", has_custom_ui:true,
    ui_entry_url:"/fixture/paint.html", execution_target:"local", execution_label:"Local"};
  const quote = (run: string, price=5000) => ({
    run_id:run,plugin_id:id,plugin_version:pluginVersion,image_ref:manifest.image,target_node_id:"local",
    offer_id:"default",offer_name:"Render",estimated_cost_act:price,workload_hash:"f".repeat(64),
    dataset_grant_summary:"Fixture data grant",froglet_quote:{},expires_at:"2099-01-01T00:00:00Z",
  });
  await page.route("**/api/v1/**",async route=>{
    const name=new URL(route.request().url()).pathname.slice("/api/v1/".length);
    const json=(body:unknown,status=200)=>route.fulfill({json:body,status});
    if (name==="runs/prepare") {
      const body=route.request().postDataJSON();
      const run=body.params.label ?? ["quick-run","api-run","review-run"][defaultRequests++];
      if (run==="late") await new Promise<void>(accept=>{releases[run]=accept;});
      return json(quote(run,run==="free"?0:5000));
    }
    const confirm=/^runs\/([^/]+)\/confirm$/.exec(name);
    if (confirm) {
      const run=confirm[1]!;
      confirms.push({run,key:route.request().postDataJSON().idempotency_key});
      if (run==="retry" && willFail) {willFail=false;return json({error:"Connection interrupted; retry this run."},503);}
      return json({run_id:run,plugin_id:id,plugin_version:pluginVersion,quote:quote(run),created_at:"2026-01-01T00:00:00Z",updated_at:"2026-01-01T00:00:00Z",status:"succeeded",params:{},logs:[],artifacts:[],froglet_chain:[],result_json:{ok:true}});
    }
    if (name.endsWith("/logs")) return route.fulfill({contentType:"text/event-stream",body:""});
    const fixtures:Record<string,unknown>={
      health:{mode:"local",status:"ok"}, "auth/whoami":{org_id:"fixture",actor:"reader",role:"member"},
      catalogs:{catalogs:[]},grants:{grants:[],grant_usage:[]},plugins:{plugins:[plugin]},
      projects:{projects:[]},runs:{runs:[]},sessions:{sessions:[]},"saved-views":{views:[]},
      ledger:{available_milli_act:20000,reserved_milli_act:0,transactions:[]},"llm/config":{configured:false,providers:[]},
    };
    return Object.hasOwn(fixtures,name)?json(fixtures[name]):json({error:"fixture not found"},404);
  });
  await page.route("**/fixture/paint.html",route=>route.fulfill({contentType:"text/html",body:
    '<!doctype html><input aria-label="Label"/><button>Request quote</button><output></output><script type="module">'+sdk+
    '\nconst client=createPluginClient();document.querySelector("button").onclick=async()=>{const label=document.querySelector("input").value;const q=await client.prepareRun({label});document.querySelector("output").textContent=q.run_id;};</script>'}));
  await page.goto("/");
  await page.getByTestId("nav-plugins").click();
  await page.getByTestId("plugin-card-"+id).click();
  const frame=page.getByTestId("module-window-"+id).frameLocator("iframe");
  async function request(label:string, wait=true) {
    await frame.getByRole("textbox").fill(label);
    await frame.getByRole("button",{name:"Request quote"}).click();
    if(wait) await expect(frame.locator("output")).toHaveText(label);
  }
  const consent=page.getByTestId("paid-run-consent");
  const approve=page.getByTestId("approve-paid-run");
  const decline=page.getByTestId("decline-paid-run");
  await request("decline");
  expect(confirms).toEqual([]);
  await expect(consent).toContainText("5.00 ACT");
  await expect(consent).toContainText("decline");
  await expect(decline).toBeFocused();
  await consent.getByText("Run details").click();
  await expect(consent.locator("pre")).toContainText('"label": "decline"');
  await expect(consent).toContainText("Fixture data grant");
  await decline.focus();
  await page.keyboard.press("Tab"); await expect(approve).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(consent).toHaveCount(0);
  await request("decline"); // re-delivery of an already declined quote
  await expect(consent).toHaveCount(0);
  expect(confirms).toEqual([]);
  await request("late",false);
  await expect.poll(()=>Boolean(releases.late)).toBe(true);
  await request("retry");
  releases.late!();
  await expect(frame.locator("output")).toHaveText("late");
  await expect(consent).toContainText("retry");
  await expect(consent).not.toContainText("late");
  await approve.click();
  await expect.poll(()=>confirms.length).toBe(1);
  await expect(approve).toBeEnabled();
  await expect(consent).toContainText("retry");
  await approve.dblclick();
  await expect(consent).toContainText("late");
  await expect(decline).toBeFocused();
  expect(confirms).toEqual([{run:"retry",key:"idem-retry"},{run:"retry",key:"idem-retry"}]);
  await decline.click();
  await expect(consent).toHaveCount(0);
  await request("retry");
  await expect(consent).toHaveCount(0);
  expect(confirms).toHaveLength(2);
  await request("free");
  await expect.poll(()=>confirms.map(x=>x.run)).toEqual(["retry","retry","free"]);
  await expect(consent).toHaveCount(0);
  await page.getByTestId("module-window-"+id).getByRole("button",{name:/close/i}).first().click();
  await page.getByTestId("nav-workbench").click();
  await page.getByTestId("quick-prepare-btn-"+id).click();
  await expect(consent).toContainText("quick-run");
  expect(confirms).toHaveLength(3);
  await decline.click();
  await page.getByTestId("nav-plugins").click();
  await page.getByTestId("plugin-card-"+id).click();
  const win=page.getByTestId("module-window-"+id);
  await win.locator(".bc-window-titlebar").hover();
  await win.getByRole("tab",{name:"API",exact:true}).click();
  await page.getByTestId("api-run-test-btn").click();
  await expect(consent).toContainText("api-run");
  expect(confirms).toHaveLength(3);
  await decline.click();
  await page.getByTestId("review-quote-btn").click();
  await expect(page.getByTestId("quote-consent-card")).toContainText("5.00 ACT");
  expect(confirms).toHaveLength(3);
  await page.getByTestId("confirm-quote-run-btn").click();
  await expect.poll(()=>confirms.at(-1)).toEqual({run:"review-run",key:"idem-review-run"});
});
