import assert from "node:assert/strict";
import test from "node:test";
import { setTimeout as delay } from "node:timers/promises";
import { createPluginClient, formatMilliEur } from "../dist/index.js";

test("formatMilliEur formats milli-EUR values to cents", () => {
  assert.equal(formatMilliEur(0), "€0.00");
  assert.equal(formatMilliEur(50), "€0.05");
  assert.equal(formatMilliEur(1000), "€1.00");
  assert.equal(formatMilliEur(5000), "€5.00");
  assert.equal(formatMilliEur(12345), "€12.35");
  assert.equal(formatMilliEur(-50), "-€0.05");
  assert.equal(formatMilliEur(-4), "€0.00");
});

test("only the host times out quote preparation", async (t) => {
  const frame = new EventTarget();
  const requests = [];
  frame.parent = {postMessage: request => requests.push(request)};
  globalThis.window = frame;
  const client = createPluginClient({timeoutMs:15});
  t.after(() => {client.dispose(); delete globalThis.window;});
  function reply(index, response) {
    const {id, channel} = requests[index];
    const event = new Event("message");
    Object.assign(event, {source: frame.parent, data: {jsonrpc:"2.0", channel, id, ...response}});
    frame.dispatchEvent(event);
  }
  const quote = client.prepareRun({term:"fixture"});
  const context = client.getContext();
  await assert.rejects(context, /RPC timeout .*getContext/);
  await delay(5);
  reply(0, {result: {run_id:"signed-quote"}});
  assert.deepEqual(await quote, {run_id:"signed-quote"});
  const failed = client.prepareRun({});
  reply(2, {error:{message:"Worker startup deadline exceeded"}});
  await assert.rejects(failed, /Worker startup deadline/);
  const unfinished = client.prepareRun({});
  client.dispose();
  await assert.rejects(unfinished, /disposed/);
});
