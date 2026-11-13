# Product telemetry

The [Desktop analytics seam](../../packages/client/product-analytics/README.md) provides an inert local no-op intake (`ctx.productAnalytics`) so Desktop and client UI consumers can invoke `track` and read collection policy without transmitting any telemetry externally. Ordinary Web clients do not mount the service.

<!-- BEGIN GENERATED cordis-surface (gen-cordis-catalog.ts) — do not edit between markers -->

<a id="cordis-surface"></a>

## Cordis API

Generated from source by `scripts/gen-cordis-catalog.ts` (verified fresh by `pnpm run verify-cordis-catalog` in doc-sync; regenerate with `pnpm run gen-cordis-catalog`) — the language sides differ only in locale-specific paired document paths. Signature blocks use a `ts cordis-catalog` fence and keep the original source JSDoc; dispatch modes are defined in the [primer](../cordis-primer.md#dispatch-modes), and the framework-inherited `ctx` API lives in [cordis-api/inherited.md](../cordis-api/inherited.md).

<a id="ctxproductanalytics--productanalytics"></a>

### `ctx.productAnalytics` — `ProductAnalytics`

Inert local analytics intake; never transmits telemetry externally.

```ts cordis-catalog
/**
 * Read the collection policy.
 * @returns whether this Host currently accepts Desktop analytics.
 */
@Remote enabled(): boolean

/**
 * Stream the effective policy initially and after live configuration edits.
 * @param signal - subscriber lifetime.
 * @returns current policy values until cancellation or service disposal.
 */
@Remote({ mode: 'stream' }) async *watchPolicy(signal: AbortSignal): AsyncIterable<boolean>

/**
 * No-op intake when external product telemetry is removed.
 * @param _event - typed product event.
 */
@Remote async report(_event: ProductEvent): Promise<void>
```

Source: [`packages/client/product-analytics/src/index.ts`](../../packages/client/product-analytics/src/index.ts)
<!-- END GENERATED cordis-surface -->
