import assert from "node:assert/strict";
import test from "node:test";
import { safeLaunchUrl } from "../src/safeUrl.ts";

test("sec_09_blocks_javascript_data_vbscript_and_protocol_relative_launch_urls", () => {
  for (const malicious of [
    "javascript:alert(1)",
    "  \tJaVaScRiPt:alert(document.cookie)",
    "java\nscript:alert(1)",
    "data:text/html,<script>alert(1)</script>",
    "vbscript:msgbox(1)",
    "file:///etc/passwd",
    "//evil.example/phish",
    "/\\evil.example/phish",
    "",
    "   ",
  ]) {
    assert.equal(
      safeLaunchUrl(malicious),
      null,
      `expected malicious launch_url '${malicious}' to be blocked`,
    );
  }
});

test("sec_09_allows_https_http_and_safe_relative_launch_urls", () => {
  assert.equal(
    safeLaunchUrl("https://provider.biocompass.io/offers/mo-123"),
    "https://provider.biocompass.io/offers/mo-123",
  );
  assert.equal(
    safeLaunchUrl("http://127.0.0.1:18200/offers/mo-123"),
    "http://127.0.0.1:18200/offers/mo-123",
  );
  assert.equal(
    safeLaunchUrl("/api/v1/module-offers/mo-123"),
    "/api/v1/module-offers/mo-123",
  );
});
