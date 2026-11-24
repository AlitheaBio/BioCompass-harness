// Minimal OCI distribution endpoint serving image manifests by digest so a compass-hub can
// verify `@sha256`-pinned module images (it never runs them in this test).
// Usage: node fake_registry.mjs <port> <outDigestsJson> <repo>...
import http from "node:http";
import crypto from "node:crypto";
import fs from "node:fs";

const [port, outFile, ...repos] = process.argv.slice(2);
const manifests = new Map();
const digests = {};
for (const repo of repos) {
  const body = Buffer.from(JSON.stringify({
    schemaVersion: 2,
    mediaType: "application/vnd.oci.image.manifest.v1+json",
    config: { mediaType: "application/vnd.oci.image.config.v1+json", size: 2, digest: "sha256:" + crypto.createHash("sha256").update("{}").digest("hex") },
    layers: [],
    annotations: { "org.biocompass.u3": repo },
  }));
  const digest = "sha256:" + crypto.createHash("sha256").update(body).digest("hex");
  manifests.set(`${repo}@${digest}`, body);
  digests[repo] = digest;
}
fs.writeFileSync(outFile, JSON.stringify(digests));
http
  .createServer((req, res) => {
    const m = /^\/v2\/(.+)\/manifests\/(sha256:[0-9a-f]{64})$/.exec(req.url ?? "");
    const body = m && manifests.get(`${m[1]}@${m[2]}`);
    if (req.url === "/v2/" || req.url === "/v2") {
      res.writeHead(200).end("{}");
    } else if (body) {
      res.writeHead(200, {
        "Content-Type": "application/vnd.oci.image.manifest.v1+json",
        "Docker-Content-Digest": m[2],
        "Content-Length": body.length,
      });
      res.end(req.method === "HEAD" ? undefined : body);
    } else {
      res.writeHead(404).end();
    }
  })
  .listen(Number(port), "127.0.0.1");
