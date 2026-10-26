import { describe, expect, it } from 'vitest'

import { ApiError } from '@/api/client'
import type { ClosedView, DormantView } from '@/api/types'
import { byActivity, closedCount, closedMeta, dormantCount, dormantErrorMessage, dormantIndex, dormantMember, dormantMeta, dormantMissing, dormantTitles, hasEntries, hostDormant, matchesDormantMember, restoreSummary, searchDormant } from '@/lib/dormant'

const NOW = Date.parse('2026-10-04T12:00:00Z')
const tmux: DormantView = {
  kind: 'tmux',
  target: 'fix-login',
  name: 'fix-login',
  since: '2026-10-04T09:00:00Z',
  windows: 1,
  panes: 2,
  sessions: [{ sessionId: 'aaaa-1', name: 'fix', title: 'Fix login', cwd: '~/Code/project' }],
}
const lone: DormantView = { kind: 'claude', target: 'bbbb-2', name: 'Review PR', since: null, windows: 0, panes: 0, sessions: [{ sessionId: 'bbbb-2', name: null, title: null, cwd: null }] }

describe('dormant helpers', () => {
  it('indexes dormant Claude sessions by host/sessionId', () => {
    const idx = dormantIndex([{ host: 'workstation', views: [tmux, lone], closed: [] }])
    expect([...idx.keys()]).toEqual(['workstation/aaaa-1', 'workstation/bbbb-2'])
    expect(idx.get('workstation/aaaa-1')).toEqual({ host: 'workstation', id: 'aaaa-1', title: 'Fix login', since: Date.parse('2026-10-04T09:00:00Z') })
    expect(idx.get('workstation/bbbb-2')?.title).toBe('Review PR')
    expect(dormantCount([{ host: 'a', views: [tmux, lone], closed: [] }, { host: 'b', views: [], closed: [] }])).toBe(2)
  })

  it('a group member is dormant by the server flag or the dormant list', () => {
    const idx = dormantIndex([{ host: 'laptop', views: [tmux], closed: [] }])
    expect(dormantMember({ host: 'laptop', id: 'aaaa-1' }, idx)?.title).toBe('Fix login')
    expect(dormantMember({ host: 'laptop', id: 'cccc-3', dormant: true }, idx)).toEqual({ host: 'laptop', id: 'cccc-3', title: 'cccc-3', since: null })
    expect(dormantMember({ host: 'laptop', id: 'cccc-3' }, idx)).toBeNull()
    expect(dormantMember({ host: 'workstation', id: 'aaaa-1' }, idx)).toBeNull()
  })

  it('titles and meta lines', () => {
    expect(dormantTitles(tmux)).toBe('Fix login')
    expect(dormantTitles({ ...tmux, sessions: [] })).toBe('no Claude panes')
    expect(dormantMeta(tmux, NOW)).toBe('tmux · 1 window · 2 panes · down 3h')
    expect(dormantMeta(lone, NOW)).toBe('not in tmux')
  })

  it('errors: old servers are missing, ambiguity lists candidates', () => {
    expect(dormantMissing(new ApiError('not found', 404))).toBe(true)
    expect(dormantMissing(new ApiError('x', 501))).toBe(true)
    expect(dormantMissing(new ApiError('no dormant session matches "x"', 404))).toBe(false)
    expect(dormantErrorMessage(new ApiError('amb', 409, { error: 'amb', candidates: ['a', 'b'] }))).toBe('Ambiguous — matches a, b')
    expect(dormantErrorMessage(new ApiError('no dormant session matches "x"', 404))).toBe('no dormant session matches "x"')
  })

  it('restore summaries', () => {
    const entry = { kind: 'tmux', from: 'fix-login', session: 'fix-login', renamed: false, windows: 1, panes: 2, launched: [{ sessionId: 'aaaa-1', title: 'Fix login', line: '…' }], warnings: [], commands: [], dryRun: false }
    expect(restoreSummary({ host: 'laptop', restored: [entry], failed: [] })).toEqual({ ok: true, title: 'Resumed fix-login', description: '1 Claude session resuming' })
    const partial = restoreSummary({ host: 'laptop', restored: [entry], failed: [{ target: 'x', error: 'boom' }] })
    expect(partial.ok).toBe(false)
    expect(partial.description).toBe('1 Claude session resuming\nx: boom')
    expect(restoreSummary({ host: 'laptop', restored: [], failed: [] }).title).toBe('Nothing resumed')
  })

  it('recently closed: parsed beside dormant, own count and meta, never in the Board index', () => {
    const closed: ClosedView = { ...lone, target: 'cccc-3', name: 'Old work', closedAt: '2026-10-04T10:00:00Z', since: '2026-10-04T10:00:00Z', tmuxSession: 'api', sessions: [{ sessionId: 'cccc-3', name: null, title: 'Old work', cwd: '~/Code/project' }] }
    const h = hostDormant('laptop', { dormant: [], closed: [closed] })
    expect(h).toEqual({ host: 'laptop', views: [], closed: [closed] })
    expect(hostDormant('old', { dormant: [tmux] }).closed).toEqual([])
    expect(hasEntries(h)).toBe(true)
    expect(hasEntries(hostDormant('x', {}))).toBe(false)
    expect(closedCount([h])).toBe(1)
    expect(dormantCount([h])).toBe(0)
    expect(dormantIndex([h]).size).toBe(0)
    expect(closedMeta(closed, NOW)).toBe('in tmux api · ~/Code/project · closed 2h ago')
    expect(closedMeta({ ...closed, tmuxSession: undefined, closedAt: null, since: null }, NOW)).toBe('not in tmux · ~/Code/project')
    expect(closedMeta({ ...tmux, closedAt: '2026-10-04T11:00:00Z' }, NOW)).toBe('tmux · 1 window · 2 panes · closed 1h ago')
  })
})

describe('searchDormant', () => {
  const view = (name: string, extra: Partial<ClosedView> = {}): ClosedView => ({
    kind: 'tmux',
    target: name,
    name,
    since: null,
    closedAt: null,
    windows: 1,
    panes: 1,
    sessions: [{ sessionId: 'abc12345', name: null, title: 'Fix login bug', cwd: '~/Code/api' }],
    ...extra,
  })
  const h = { host: 'workstation', views: [view('web'), view('docs', { sessions: [] })], closed: [view('old', { tmuxSession: 'infra', sessions: [] })] }

  it('keeps everything without a query', () => {
    expect(searchDormant(h, '  ')).toBe(h)
  })

  it('matches names, Claude titles, cwds, tmux sessions and the host', () => {
    expect(searchDormant(h, 'login').views.map((v) => v.name)).toEqual(['web'])
    expect(searchDormant(h, 'code/api').views.map((v) => v.name)).toEqual(['web'])
    expect(searchDormant(h, 'DOCS').views.map((v) => v.name)).toEqual(['docs'])
    expect(searchDormant(h, 'infra').closed.map((v) => v.name)).toEqual(['old'])
    expect(searchDormant(h, 'workstation').views).toHaveLength(2)
    expect(searchDormant(h, 'nothing')).toMatchObject({ views: [], closed: [] })
  })
})

describe('matchesDormantMember', () => {
  const m = { host: 'laptop', id: 'abc12345-0000', title: 'Refactor groups', since: null }

  it('matches title, host and id', () => {
    expect(matchesDormantMember(m, '')).toBe(true)
    expect(matchesDormantMember(m, 'refactor')).toBe(true)
    expect(matchesDormantMember(m, 'laptop')).toBe(true)
    expect(matchesDormantMember(m, 'abc123')).toBe(true)
    expect(matchesDormantMember(m, 'zzz')).toBe(false)
  })
})

describe('byActivity', () => {
  it('merges hosts, most recent first, undated last', () => {
    const old: DormantView = { ...tmux, target: 'old', since: '2026-10-04T08:00:00Z' }
    const fresh: ClosedView = { ...tmux, target: 'fresh', since: null, closedAt: '2026-10-04T11:00:00Z' }
    const hosts = [
      { host: 'laptop', views: [old, lone], closed: [] },
      { host: 'workstation', views: [tmux], closed: [fresh] },
    ]
    expect(byActivity(hosts, (h) => h.views).map((e) => `${e.host}/${e.view.target}`)).toEqual(['workstation/fix-login', 'laptop/old', 'laptop/bbbb-2'])
    expect(byActivity(hosts, (h) => h.closed).map((e) => e.view.target)).toEqual(['fresh'])
  })
})
