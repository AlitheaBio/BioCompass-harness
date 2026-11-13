---
description: "Package map for the web access capability family: the search/fetch service, its provider backends, and the model-facing tools that consume them."
kind: "package-group"
---

# web/ — web access capability family

## Summary

The `web/` packages let models fetch HTTP(S) pages through `web_fetch` (and expose the `ctx.web` search/fetch capability seam for custom search or fetch provider plugins). Use this family for page retrieval and custom web provider integration, not interactive browsing. Models receive consistent tool behavior, cancellation, and error reporting when providers change.

## Table of Contents

- [Packages](#packages)
- [Related documentation](#related-documentation)
- [Dev Note](#dev-note)

-----

<a id="packages"></a>
## Packages

Three packages play the web roles; the subsystem reference owns the exhaustive vocabulary and contracts.

| Package | Role | ctx key |
|---|---|---|
| [`web/`](web/README.md) | Search/fetch service: search and fetch URLs through interchangeable backends, one selection and error policy | `ctx.web` |
| [`web-fetch-http/`](web-fetch-http/README.md) | Fetches HTTP(S) pages anonymously | registers on `ctx.web` |
| [`tool-web/`](tool-web/README.md) | Exposes `web_fetch` (and optional `web_search`) to the model | registers on `ctx.tools` |

-----

<a id="related-documentation"></a>
## Related documentation

Start with the subsystem reference for the shared vocabulary, then the design decision behind the single provider-selection service.

- [Web subsystem](../../docs/subsystems/web.md) — the search/fetch requests and results, provider availability, `WebError`, and the fetch network policy.
- [Web capability seam decision](../../.agents/notes/implemented/architecture/2026-06-24-web-capability-seam.md) — why search and fetch share one provider-selection service.

<a id="dev-note"></a>
## Dev Note

<details>
<summary>Working context for maintainers — click to expand</summary>

None.

</details>
