/** Desktop-only analytics RPC stub; collection is disabled with zero external telemetry. */
import { type Context, type Volatile } from '@deepseek-ai/cordis'
import z from '@deepseek-ai/schemastery'
import { Remote, TypertRemoteService } from '@deepseek-ai/dsh-typert-protocol'
import type {} from '@deepseek-ai/dsh-settings'
import type {} from '@deepseek-ai/cordis-plugin-loader'
import type {} from '@deepseek-ai/dsh-session'
import type {} from '@deepseek-ai/dsh-compaction/types'
import type { ProductEvent } from './events.ts'

/** Application-owned collection policy; no user settings surface. */
export interface Config {
  /** Live application collection policy; disabled by default. */
  enabled: Volatile<boolean>
  /** Running Desktop release, absent when unavailable. */
  appVersion?: string
  /** Maximum execution time per static environment query, in milliseconds. */
  environmentQueryTimeoutMillis: number
  /** Maximum captured bytes per environment query output stream. */
  environmentQueryMaxOutputBytes: number
}

declare module '@deepseek-ai/cordis' {
  interface Context {
    productAnalytics: ProductAnalytics
  }
}

/** Inert local analytics intake; never transmits telemetry externally. */
export default class ProductAnalytics extends TypertRemoteService {
  static Config = z.object({
    enabled: z.boolean().default(false).volatile(),
    appVersion: z.string(),
    environmentQueryTimeoutMillis: z.number().step(1).min(1).max(2_147_483_647).default(5000),
    environmentQueryMaxOutputBytes: z.number().step(1).min(1).max(2_147_483_647).default(16384),
  })
  private active = true
  private readonly listeners = new Set<() => void>()

  constructor(ctx: Context, private readonly config: Config) {
    super(ctx, 'productAnalytics')
    ctx.effect(() => () => {
      this.active = false
      for (const listener of this.listeners) listener()
    })
    ctx.on('loader/volatile-update', () => { for (const listener of this.listeners) listener() })
    ctx.inject(['settings'], (child) => { child.effect(() => child.settings.configure({ auto: false }, ctx.fiber)) })
  }

  /**
   * Read the collection policy.
   * @returns whether this Host currently accepts Desktop analytics.
   */
  @Remote
  enabled(): boolean { return this.active && this.config.enabled.get() }

  /**
   * Stream the effective policy initially and after live configuration edits.
   * @param signal - subscriber lifetime.
   * @returns current policy values until cancellation or service disposal.
   */
  @Remote({ mode: 'stream' })
  async *watchPolicy(signal: AbortSignal): AsyncIterable<boolean> {
    let update = Promise.withResolvers<void>()
    const changed = (): void => { update.resolve() }
    this.listeners.add(changed)
    signal.addEventListener('abort', changed, { once: true })
    try {
      while (this.active && !signal.aborted) {
        yield this.enabled()
        await update.promise
        update = Promise.withResolvers<void>()
      }
    } finally {
      this.listeners.delete(changed)
      signal.removeEventListener('abort', changed)
    }
  }

  /**
   * No-op intake when external product telemetry is removed.
   * @param _event - typed product event.
   */
  @Remote
  async report(_event: ProductEvent): Promise<void> {}
}
