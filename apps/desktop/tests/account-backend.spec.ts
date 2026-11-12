import { expect, it, onTestFinished, vi } from 'vitest'
import type { SignInAttemptId } from '@deepseek-ai/dsh-deepseek-account/types'
import { accountView, desktopAccountBackend } from '../src/account-backend.ts'

it('projects only safe account fields and refuses non-browser authorization schemes', () => {
  const state = { links: { usageUrl: 'http://localhost/usage', topUpUrl: 'http://localhost/top_up' }, status: 'signed-out', token: 'not-for-the-renderer',
    attempt: { id: 'test', phase: 'waiting-browser', authorizeUrl: 'https://platform.deepseek.com/dsh/authorize', codeVerifier: 'private' } }
  expect(accountView(state)).toEqual({ links: { usageUrl: 'http://localhost/usage', topUpUrl: 'http://localhost/top_up' }, status: 'signed-out', attempt: {
    id: 'test', phase: 'waiting-browser', authorizeUrl: 'https://platform.deepseek.com/dsh/authorize',
  } })
  for (const authorizeUrl of ['file:///tmp/example', 'javascript:alert(1)', 'http://example.com/login']) {
    expect(() => accountView({ ...state, attempt: { ...state.attempt, authorizeUrl } })).toThrow()
  }
  expect(() => accountView({ ...state, links: { usageUrl: 'file:///tmp/example', topUpUrl: 'https://example.com/top_up' } })).toThrow()
  expect(() => accountView({ ...state, attempt: { ...state.attempt, expiresAt: 'tomorrow' } })).toThrow()
  expect(() => accountView({ ...state, attempt: { ...state.attempt, errorCode: 'raw-server-message' } })).toThrow()
})

it('accepts a no-response login failure without discarding the account projection', () => {
  const state = { status: 'signed-out', attempt: { id: 'test', phase: 'failed', errorCode: 'no-response' },
    links: { usageUrl: 'https://platform.deepseek.com/usage', topUpUrl: 'https://platform.deepseek.com/top_up' } }
  expect(accountView(state)).toEqual(state)
})

it('returns an inert local signed-out snapshot without invoking Remote commands', async () => {
  const invoke = vi.fn()
  const backend = desktopAccountBackend('http://127.0.0.1:1234', invoke, '')
  const client = { version: '1.2.3', locale: 'en', timezoneOffsetSeconds: 28_800 }
  const expected = { links: { usageUrl: 'https://localhost', topUpUrl: 'https://localhost' }, status: 'signed-out', attempt: null }
  expect(await backend.state()).toEqual(expected)
  expect(await backend.start(client)).toEqual(expected)
  expect(await backend.cancel('attempt-1' as SignInAttemptId)).toEqual(expected)
  expect(await backend.signOut(client)).toEqual(expected)
  expect(invoke).not.toHaveBeenCalled()
})

it('does not replay an expiry reason from an account snapshot', () => {
  const state = { status: 'signed-out', attempt: null,
    links: { usageUrl: 'https://platform.deepseek.com/usage', topUpUrl: 'https://platform.deepseek.com/top_up' } }
  expect(accountView({ ...state, signOutReason: 'expired' })).toEqual(state)
})

it('publishes local signed-out state and disabled analytics policy without opening a socket', async () => {
  const listener = vi.fn()
  const failed = vi.fn()
  const expired = vi.fn()
  const onAnalyticsEnabledChanged = vi.fn()
  const backend = desktopAccountBackend('http://127.0.0.1:1234', vi.fn(), 'dsh-auth-fixture=signed')
  const stop = backend.watch(listener, failed, expired, onAnalyticsEnabledChanged)
  onTestFinished(stop)
  await vi.waitFor(() => { expect(listener).toHaveBeenCalledOnce() })
  expect(listener).toHaveBeenCalledWith({
    status: 'signed-out', attempt: null, links: { usageUrl: 'https://localhost', topUpUrl: 'https://localhost' },
  })
  expect(onAnalyticsEnabledChanged).toHaveBeenCalledWith(false)
  expect(failed).not.toHaveBeenCalled()
  expect(expired).not.toHaveBeenCalled()
  stop()
  expect(onAnalyticsEnabledChanged).toHaveBeenLastCalledWith(false)
})
