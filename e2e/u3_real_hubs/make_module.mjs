// Write a module directory (plugin.toml, SKILL.md, ui/dist) whose UI completes the plugin SDK
// `biocompass.getContext` handshake and then tells the host page `{u3ready}`.
// Usage: node make_module.mjs <dir> <pluginId> <name> <image> [paddingKiB]
import fs from "node:fs";
import path from "node:path";

const [dir, pluginId, name, image, paddingKiB = "256"] = process.argv.slice(2);
const dist = path.join(dir, "ui", "dist");
fs.mkdirSync(path.join(dist, "assets"), { recursive: true });
fs.writeFileSync(
  path.join(dir, "plugin.toml"),
  `id = "${pluginId}"
name = "${name}"
version = "1.0.0"
description = "U-3 timing module"
category = "Timing"
execution = "cloud_only"
image = "${image}"
network = "none"
price_milli_act = 0

[inputs.schema]
type = "object"

[ui]
entry = "ui/dist/index.html"
`,
);
fs.writeFileSync(path.join(dir, "SKILL.md"), `# ${name}\n\nTiming module for the U-3 measurement.\n`);
fs.writeFileSync(
  path.join(dist, "index.html"),
  `<!doctype html><html><head><meta charset="utf-8"><link rel="stylesheet" href="assets/app.css"></head>` +
    `<body><div id="root">loading</div><script src="assets/app.js"></script></body></html>`,
);
fs.writeFileSync(path.join(dist, "assets", "app.css"), "body{font-family:sans-serif}\n".repeat(200));
// Realistic bundle weight (real module bundles are 230-330 KB): incompressible-ish padding.
let pad = "";
// Seed from the plugin ID so every module has a distinct bundle (distinct content hash).
let x = [...pluginId].reduce((h, c) => (h * 31 + c.charCodeAt(0)) & 0x7fffffff, 1234567);
const target = Number(paddingKiB) * 1024;
while (pad.length < target) {
  x = (x * 1103515245 + 12345) & 0x7fffffff;
  pad += x.toString(36);
}
fs.writeFileSync(
  path.join(dist, "assets", "app.js"),
  `/* ${pad} */
(function () {
  var id = "u3_" + Date.now();
  window.addEventListener("message", function (ev) {
    var d = ev.data;
    if (d && d.jsonrpc === "2.0" && d.channel === "biocompass.plugin.rpc.v1" && d.id === id && d.result) {
      document.getElementById("root").textContent = "ready " + d.result.pluginId;
      window.parent.postMessage({ u3ready: true, plugin: d.result.pluginId }, "*");
    }
  });
  window.parent.postMessage({ jsonrpc: "2.0", channel: "biocompass.plugin.rpc.v1", id: id, method: "biocompass.getContext" }, "*");
})();
`,
);
