import { describe, expect, test } from 'vitest'

import type { UsageResponse } from '@/api/types'
import { groupByAccount, money, resetsLabel, usageErrors, usageLevel } from '@/lib/usage'

const usage = (over: Partial<UsageResponse> = {}): UsageResponse => ({
  host: 'laptop',
  account: { uuid: 'u1', email: 'me@example.com', organization: null, plan: 'max', tier: null, plan_label: 'Max 5x' },
  limits: [],
  extra_usage: null,
  fetched_at: '2026-09-29T20:00:00Z',
  stale: false,
  error: null,
  ...over,
})

describe('usage', () => {
  test('bands: severity wins, then percent', () => {
    expect(usageLevel({ percent: 10, severity: 'normal' })).toBe('low')
    expect(usageLevel({ percent: 75, severity: 'normal' })).toBe('warn')
    expect(usageLevel({ percent: 10, severity: 'warning' })).toBe('warn')
    expect(usageLevel({ percent: 95, severity: 'normal' })).toBe('hot')
    expect(usageLevel({ percent: 10, severity: 'critical' })).toBe('hot')
  })

  test('reset labels', () => {
    const now = Date.parse('2026-09-29T20:00:00Z')
    expect(resetsLabel('2026-09-29T20:45:00Z', now)).toBe('in 45m')
    expect(resetsLabel('2026-09-29T22:20:00Z', now)).toBe('in 2h 20m')
    expect(resetsLabel('2026-10-02T12:00:00Z', now)).toMatch(/\d{2}:\d{2}$/)
    expect(resetsLabel(null, now)).toBeNull()
    expect(resetsLabel('nope', now)).toBeNull()
  })

  test('money', () => {
    expect(money(null, 'USD')).toBeNull()
    expect(money(12.5, 'USD')).toMatch(/12[.,]50/)
    expect(money(3, null)).toBe('3.00')
  })

  test('hosts on one account collapse into one, keeping the freshest read', () => {
    const accounts = groupByAccount([
      { host: 'laptop', usage: usage({ stale: true }), error: null },
      { host: 'workstation', usage: usage({ host: 'workstation', fetched_at: '2026-09-29T19:00:00Z' }), error: null },
      { host: 'other', usage: usage({ host: 'other', account: null }), error: null },
      { host: 'down', usage: null, error: 'unreachable' },
    ])
    expect(accounts.map((a) => a.hosts)).toEqual([['laptop', 'workstation'], ['other']])
    expect((accounts[0].usage as UsageResponse).host).toBe('workstation')
  })

  test('ChatGPT reads become their own cards, after Claude; failed reads become errors', () => {
    const gpt = { ...usage(), host: undefined, provider: 'openai', account: { uuid: 'acct', email: 'me@example.com', organization: null, plan: 'plus', tier: null, plan_label: 'ChatGPT Plus' } }
    const results = [
      { host: 'laptop', usage: usage({ openai: gpt }), error: null },
      { host: 'workstation', usage: usage({ host: 'workstation', openai: gpt }), error: null },
      { host: 'codex-only', usage: usage({ host: 'codex-only', account: null, error: 'no Claude Code login found', openai: gpt }), error: null },
    ]
    const accounts = groupByAccount(results)
    expect(accounts.map((a) => [a.provider, a.hosts])).toEqual([
      ['claude', ['laptop', 'workstation']],
      ['openai', ['laptop', 'workstation', 'codex-only']],
    ])
    expect(usageErrors(results)).toEqual([{ key: 'codex-only:claude', host: 'codex-only', error: 'Claude: no Claude Code login found' }])
  })
})
