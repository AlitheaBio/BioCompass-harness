/** Native account commands and state stub; no renderer receives credentials. */
import type { AccountClientMetadata, AccountView, SignInAttemptId } from '@deepseek-ai/dsh-deepseek-account/types'

/** Authenticated unary caller shared with native onboarding. */
export type AccountInvoke = (request: { namespace: string; method: string; args: Record<string, unknown> }) => Promise<unknown>

/** Decode the UI-safe state received across HTTP or WebSocket. @param value - wire value. @returns account projection. */
export function accountView(value: unknown): AccountView {
  if (typeof value !== 'object' || value === null || !('status' in value)
    || !['signed-out', 'credential-stored'].includes(String(value.status)) || !('attempt' in value)) {
    throw new Error('desktop account: invalid state')
  }
  if (!('links' in value) || typeof value.links !== 'object' || value.links === null
    || !('usageUrl' in value.links) || typeof value.links.usageUrl !== 'string'
    || !('topUpUrl' in value.links) || typeof value.links.topUpUrl !== 'string') {
    throw new Error('desktop account: invalid platform links')
  }
  validateBrowserDestination(value.links.usageUrl)
  validateBrowserDestination(value.links.topUpUrl)
  const attempt = value.attempt
  if (attempt !== null && (typeof attempt !== 'object' || !('id' in attempt) || typeof attempt.id !== 'string'
    || !('phase' in attempt) || !['initializing', 'waiting-browser', 'exchanging', 'committing', 'succeeded', 'cancelled', 'expired', 'failed'].includes(String(attempt.phase))
    || ('authorizeUrl' in attempt && typeof attempt.authorizeUrl !== 'string')
    || ('expiresAt' in attempt && (typeof attempt.expiresAt !== 'number' || !Number.isFinite(attempt.expiresAt)))
    || ('errorCode' in attempt && !['no-response', 'network', 'protocol', 'expired', 'storage'].includes(String(attempt.errorCode))))) {
    throw new Error('desktop account: invalid attempt')
  }
  if (attempt !== null && 'authorizeUrl' in attempt) {
    validateBrowserDestination(String(attempt.authorizeUrl))
  }
  const parsed = value as AccountView
  return {
    status: parsed.status,
    links: { usageUrl: parsed.links.usageUrl, topUpUrl: parsed.links.topUpUrl },
    attempt: parsed.attempt === null ? null : {
      id: parsed.attempt.id, phase: parsed.attempt.phase,
      ...parsed.attempt.authorizeUrl === undefined ? {} : { authorizeUrl: parsed.attempt.authorizeUrl },
      ...parsed.attempt.expiresAt === undefined ? {} : { expiresAt: parsed.attempt.expiresAt },
      ...parsed.attempt.errorCode === undefined ? {} : { errorCode: parsed.attempt.errorCode },
    },
  }
}

/** Only HTTP loopback or HTTPS destinations can leave the native app. */
function validateBrowserDestination(value: string): void {
  const url = new URL(value)
  const loopback = ['localhost', '127.0.0.1', '[::1]'].includes(url.hostname)
  if (url.username || url.password || !(url.protocol === 'https:' || (loopback && url.protocol === 'http:'))) {
    throw new Error('desktop account: invalid browser destination')
  }
}

/** Native account operations and explicitly owned stream lifetime. */
export interface DesktopAccountBackend {
  /** @returns current account snapshot. */
  state(): Promise<AccountView>
  /** @param client - this window's identity and current language. @returns new or already-running login attempt. */
  start(client: AccountClientMetadata): Promise<AccountView>
  /** @param id - attempt to cancel. @returns cancellation or completed commit state. */
  cancel(id: SignInAttemptId): Promise<AccountView>
  /** @param client - this window's identity. @returns state after local sign-out. */
  signOut(client: AccountClientMetadata): Promise<AccountView>
  /**
   * @param listener - state recipient.
   * @param failed - stream failure recipient.
   * @param expired - live credential-expiry recipient.
   * @param onAnalyticsEnabledChanged - optional Desktop collection-policy recipient; disconnected streams publish false.
   * @returns stream disposer.
   */
  watch(
    listener: (state: AccountView) => void, failed: () => void, expired: () => void,
    onAnalyticsEnabledChanged?: (enabled: boolean) => void,
  ): () => void
}

const SIGNED_OUT_VIEW: AccountView = {
  status: 'signed-out',
  attempt: null,
  links: { usageUrl: 'https://localhost', topUpUrl: 'https://localhost' },
}

/**
 * Provide local signed-out account state without remote OAuth or WebSocket streams.
 * @param _origin - Host Web origin.
 * @param _invoke - validated unary RPC caller.
 * @param _cookie - Host authentication Cookie header.
 * @returns inert local account operations; watch callers own their subscriptions.
 */
export function desktopAccountBackend(_origin: string, _invoke: AccountInvoke, _cookie: string): DesktopAccountBackend {
  return {
    state: () => Promise.resolve(SIGNED_OUT_VIEW),
    start: () => Promise.resolve(SIGNED_OUT_VIEW),
    cancel: () => Promise.resolve(SIGNED_OUT_VIEW),
    signOut: () => Promise.resolve(SIGNED_OUT_VIEW),
    watch(listener, _failed, _expired, onAnalyticsEnabledChanged) {
      let closed = false
      const timer = setTimeout(() => {
        if (closed) return
        onAnalyticsEnabledChanged?.(false)
        listener(SIGNED_OUT_VIEW)
      }, 0)
      return () => {
        closed = true
        clearTimeout(timer)
        onAnalyticsEnabledChanged?.(false)
      }
    },
  }
}
