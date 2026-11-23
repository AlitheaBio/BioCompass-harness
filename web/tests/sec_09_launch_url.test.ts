import assert from "node:assert/strict";
import test from "node:test";
import { sanitizeFederatedLaunchUrl } from "../src/safeUrl.ts";

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
      sanitizeFederatedLaunchUrl(malicious),
      null,
      `expected malicious launch_url '${malicious}' to be blocked`,
    );
  }
});

test("sec_09_allows_https_http_and_safe_relative_launch_urls", () => {
  assert.equal(
    sanitizeFederatedLaunchUrl("https://partner.biocompass.io/federation/launch?code=abc"),
    "https://partner.biocompass.io/federation/launch?code=abc",
  );
  assert.equal(
    sanitizeFederatedLaunchUrl("http://127.0.0.1:18200/federation/launch"),
    "http://127.0.0.1:18200/federation/launch",
  );
  assert.equal(
    sanitizeFederatedLaunchUrl("/api/v1/auth/federation/launch?offer=mo-123"),
    "/api/v1/auth/federation/launch?offer=mo-123",
  );
});
