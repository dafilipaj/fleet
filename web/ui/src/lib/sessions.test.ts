import { expect, test } from 'vitest'

import type { FleetResponse, Session } from '@/api/types'
import {
  clusterByStack,
  stackRuns,
  findSession,
  findSpawned,
  setAside,
  isSlackSession,
  slackLinked,
  isEmbedSession,
  isJobSession,
  keepOrder,
  listView,
  matchesSearch,
  sessionHref,
  spawnTargets,
  statusLabel,
  statusMeta,
  statusSummary,
  visibleStatusFilters,
  withoutClosed,
  withoutSession,
  closedKey,
} from './sessions'

const s = (over: Partial<Session>): Session => ({ host: 'laptop', session_id: 'id', status: 'idle', ...over })

const fleet: FleetResponse = {
  self: 'laptop',
  hosts: [
    {
      name: 'laptop',
      ok: true,
      sessions: [
        s({ session_id: 'a', name: 'alpha', status: 'busy', updated_at: 100 }),
        s({ session_id: 'b', name: 'beta', status: 'waiting', updated_at: 300, waiting_for: 'permission' }),
      ],
    },
    {
      name: 'workstation',
      ok: true,
      sessions: [s({ host: 'workstation', session_id: 'c', name: 'gamma', status: 'idle', updated_at: 200, cwd: '/srv/api' })],
    },
    { name: 'down', ok: false, error: 'timeout', sessions: [] },
  ],
}

test('statusMeta / statusLabel', () => {
  expect(statusMeta('BUSY').key).toBe('busy')
  expect(statusMeta('weird').key).toBe('unknown')
  expect(statusMeta(null).label).toBe('unknown')
  expect(statusLabel({ status: 'waiting', waiting_for: ' permission ' })).toBe('needs you · permission')
  const agent = { id: 'a1', description: 'x', type: null, model: null, background: true, started_at: 1, last_activity_at: null, activity: null }
  expect(statusLabel({ status: 'idle', background: { agents: [agent, agent], shells: 1 } })).toBe('idle · 2 agents, 1 shell')
  expect(statusLabel({ status: 'idle', background: { agents: [], shells: 0 } })).toBe('idle')
  expect(statusLabel({ status: 'waiting', waiting_for: null })).toBe('needs you')
  expect(statusLabel({ status: 'busy', waiting_for: 'x' })).toBe('working')
})

test('listView: flat, sorted by last activity, counts per status', () => {
  const v = listView(fleet, {})
  expect(v.sessions.map((x) => x.session_id)).toEqual(['b', 'c', 'a'])
  expect(v.counts).toEqual({ all: 3, waiting: 1, busy: 1, idle: 1, scheduled: 0, slack: 0, embedded: 0 })
})

test('listView: a prompt moves a session, a turn ending does not', () => {
  const f: FleetResponse = {
    self: 'laptop',
    hosts: [
      {
        name: 'laptop',
        ok: true,
        sessions: [
          // Just finished a turn prompted long ago.
          s({ session_id: 'done', updated_at: 900, prompted_at: 100 }),
          s({ session_id: 'asked', status: 'busy', updated_at: 500, prompted_at: 500 }),
          // No prompt known (Codex / pi, or none yet): its activity stands in.
          s({ session_id: 'codex', updated_at: 300 }),
        ],
      },
    ],
  }
  expect(listView(f, {}).sessions.map((x) => x.session_id)).toEqual(['asked', 'codex', 'done'])
})

test('keepOrder: the frozen order holds, new sessions go on top, gone ones drop', () => {
  const now = ['c', 'a', 'new', 'b'].map((id) => s({ session_id: id }))
  const frozen = ['laptop/a', 'laptop/b', 'laptop/gone', 'laptop/c']
  expect(keepOrder(now, frozen).map((x) => x.session_id)).toEqual(['new', 'a', 'b', 'c'])
})

test('listView: host + status + search filters', () => {
  expect(listView(fleet, { host: 'workstation' }).sessions.map((x) => x.session_id)).toEqual(['c'])
  expect(listView(fleet, { host: 'workstation' }).counts.all).toBe(1)
  expect(listView(fleet, { status: 'waiting' }).sessions.map((x) => x.session_id)).toEqual(['b'])
  // counts ignore the status filter (the chips show every status)
  expect(listView(fleet, { status: 'waiting' }).counts.all).toBe(3)
  expect(listView(fleet, { query: 'srv/API' }).sessions.map((x) => x.session_id)).toEqual(['c'])
  expect(listView(null, {}).sessions).toEqual([])
})

test('matchesSearch covers name, titles, cwd, tmux session, host', () => {
  const x = s({ name: 'n', gen_title: 'Gen', title: 'T', cwd: '/c', tmux_session: 'tm', host: 'hh' })
  for (const q of ['n', 'gen', 't', '/c', 'tm', 'hh', '  ']) expect(matchesSearch(x, q)).toBe(true)
  expect(matchesSearch(x, 'zzz')).toBe(false)
})

test('findSession / sessionHref', () => {
  expect(findSession(fleet, 'workstation', 'c')?.name).toBe('gamma')
  expect(findSession(fleet, 'laptop', 'c')).toBeNull()
  expect(sessionHref({ host: 'a b', session_id: 'x/y' })).toBe('/s/a%20b/x%2Fy')
})

test('spawnTargets: reachable hosts with their dirs', () => {
  const fleet = {
    self: 'a',
    hosts: [
      { name: 'a', ok: true, sessions: [], spawnDirs: [{ label: 'Work', path: '/w' }] },
      { name: 'b', ok: false, sessions: [] },
      { name: 'c', ok: true, sessions: [] },
    ],
  }
  expect(spawnTargets(fleet)).toEqual([
    { name: 'a', dirs: [{ label: 'Work', path: '/w' }] },
    { name: 'c', dirs: [] },
  ])
})

test('findSpawned matches tmux session or name on that host', () => {
  const fleet = {
    self: 'a',
    hosts: [
      { name: 'a', ok: true, sessions: [{ host: 'a', session_id: '1', tmux_session: 'fw-1', name: 'x' }] },
      { name: 'b', ok: true, sessions: [{ host: 'b', session_id: '2', tmux_session: 'fw-2', name: 'y' }] },
    ],
  }
  expect(findSpawned(fleet, 'a', { tmuxSession: 'fw-1' })?.session_id).toBe('1')
  expect(findSpawned(fleet, 'a', { tmuxSession: 'fw-2' })).toBeNull()
  expect(findSpawned(fleet, 'b', { tmuxSession: 'nope', name: 'y' })?.session_id).toBe('2')
})

test('withoutSession drops only that session', () => {
  const fleet = {
    self: 'a',
    hosts: [
      { name: 'a', ok: true, sessions: [{ host: 'a', session_id: '1' }, { host: 'a', session_id: '2' }] },
      { name: 'b', ok: true, sessions: [{ host: 'b', session_id: '1' }] },
    ],
  }
  const out = withoutSession(fleet, 'a', '1')
  expect(out.hosts[0].sessions.map((s) => s.session_id)).toEqual(['2'])
  expect(out.hosts[1].sessions).toHaveLength(1)
})

test('withoutClosed hides recently closed sessions until their deadline', () => {
  const fleet = {
    self: 'a',
    hosts: [{ name: 'a', ok: true, sessions: [{ host: 'a', session_id: '1' }, { host: 'a', session_id: '2' }] }],
  }
  const closed = new Map([[closedKey('a', '1'), 1000]])
  expect(withoutClosed(fleet, closed, 999).hosts[0].sessions.map((s) => s.session_id)).toEqual(['2'])
  expect(withoutClosed(fleet, closed, 1000)).toBe(fleet)
  expect(withoutClosed(fleet, new Map([[closedKey('b', '1'), 1000]]), 0)).toBe(fleet)
})

test('clusterByStack: a stack moves up to its first member, everything else keeps its place', () => {
  const st = { id: 'st-1', label: 'Login fix' }
  const rows = [
    s({ session_id: 'a', stack: st }),
    s({ session_id: 'b', stack: null }),
    s({ session_id: 'c' }),
    s({ session_id: 'd', stack: st }),
    s({ session_id: 'e', stack: { id: 'st-2', label: 'x' } }),
  ]
  expect(clusterByStack(rows).map((x) => x.session_id)).toEqual(['a', 'd', 'b', 'c', 'e'])
  // Same id on another host: a different stack — unless it names the same home host.
  const hosts = [s({ session_id: 'a', stack: st }), s({ session_id: 'b' }), s({ session_id: 'c', host: 'workstation', stack: st })]
  expect(clusterByStack(hosts).map((x) => x.session_id)).toEqual(['a', 'b', 'c'])
  const home = { ...st, host: 'laptop' }
  const shared = [s({ session_id: 'a', stack: home }), s({ session_id: 'b' }), s({ session_id: 'c', host: 'workstation', stack: home })]
  expect(clusterByStack(shared).map((x) => x.session_id)).toEqual(['a', 'c', 'b'])
  // Nothing to move: the same array back.
  const plain = [s({ session_id: 'a' }), s({ session_id: 'b' })]
  expect(clusterByStack(plain)).toBe(plain)
})

test('listView keeps stacks adjacent after sorting by activity', () => {
  const st = { id: 'st-1', label: 'Login fix' }
  const f: FleetResponse = {
    self: 'laptop',
    hosts: [
      {
        name: 'laptop',
        ok: true,
        sessions: [
          s({ session_id: 'a', updated_at: 3, stack: st }),
          s({ session_id: 'b', updated_at: 2 }),
          s({ session_id: 'c', updated_at: 1, stack: st }),
        ],
      },
    ],
  }
  expect(listView(f, {}).sessions.map((x) => x.session_id)).toEqual(['a', 'c', 'b'])
})

test('listView: job sessions only under Scheduled, except waiting ones', () => {
  const origin = { job: 'triage', run: '20260101-0700' }
  const f: FleetResponse = {
    self: 'laptop',
    hosts: [
      {
        name: 'laptop',
        ok: true,
        sessions: [
          s({ session_id: 'mine', status: 'busy', updated_at: 100 }),
          s({ session_id: 'job-busy', status: 'busy', updated_at: 200, origin }),
          s({ session_id: 'job-idle', status: 'idle', updated_at: 300, origin }),
          s({ session_id: 'job-wait', status: 'waiting', updated_at: 400, origin }),
        ],
      },
    ],
  }
  expect(isJobSession(s({ origin }))).toBe(true)
  expect(isJobSession(s({ origin: null }))).toBe(false)
  expect(setAside(s({ origin, status: 'waiting' }))).toBe(false)

  const all = listView(f, {})
  expect(all.sessions.map((x) => x.session_id)).toEqual(['job-wait', 'mine'])
  expect(all.counts).toEqual({ all: 2, waiting: 1, busy: 1, idle: 0, scheduled: 3, slack: 0, embedded: 0 })

  // "Scheduled" shows only job sessions, waiting ones included.
  const sched = listView(f, { status: 'scheduled' })
  expect(sched.sessions.map((x) => x.session_id).sort()).toEqual(['job-busy', 'job-idle', 'job-wait'])
  // The count follows search / host scoping.
  expect(listView(f, { query: 'nothing-matches' }).counts.scheduled).toBe(0)

  // The header counts every session.
  expect(statusSummary(f)).toEqual({ waiting: 1, busy: 2, idle: 1 })

  // The chip shows only when there is something to show, or it is picked.
  expect(visibleStatusFilters(all.counts, 'all').map((x) => x.id)).toContain('scheduled')
  expect(visibleStatusFilters({ ...all.counts, scheduled: 0 }, 'all').map((x) => x.id)).not.toContain('scheduled')
  expect(visibleStatusFilters({ ...all.counts, scheduled: 0 }, 'scheduled').map((x) => x.id)).toContain('scheduled')
})

test('embed sessions: set aside like bot sessions, in their own Apps filter', () => {
  const f: FleetResponse = {
    self: 'laptop',
    hosts: [
      {
        name: 'laptop',
        ok: true,
        sessions: [
          s({ session_id: 'mine', status: 'busy', updated_at: 100 }),
          s({ session_id: 'app-idle', status: 'idle', updated_at: 200, embed: 'fin' }),
          s({ session_id: 'app-wait', status: 'waiting', updated_at: 300, embed: 'fin' }),
        ],
      },
    ],
  }
  expect(isEmbedSession(s({ embed: 'fin' }))).toBe(true)
  expect(isEmbedSession(s({ embed: null }))).toBe(false)
  const all = listView(f, {})
  expect(all.sessions.map((x) => x.session_id)).toEqual(['app-wait', 'mine'])
  expect(all.counts.embedded).toBe(2)
  expect(listView(f, { status: 'embedded' }).sessions.map((x) => x.session_id)).toEqual(['app-wait', 'app-idle'])
  expect(visibleStatusFilters(all.counts, 'all').map((x) => x.id)).toEqual(['all', 'waiting', 'busy', 'idle', 'embedded'])
})

test('Slack-linked sessions: set aside like bot sessions, in their own Slack filter', () => {
  const f: FleetResponse = {
    self: 'laptop',
    hosts: [
      {
        name: 'workstation',
        ok: true,
        sessions: [
          s({ host: 'workstation', session_id: 'mine', status: 'busy', updated_at: 100 }),
          s({ host: 'workstation', session_id: 'linked-idle', status: 'idle', updated_at: 200 }),
          s({ host: 'workstation', session_id: 'linked-wait', status: 'waiting', updated_at: 300 }),
        ],
      },
    ],
  }
  const slack = slackLinked([
    { host: 'workstation', session: 'linked-idle' },
    { host: 'workstation', session: 'linked-wait' },
    { host: 'laptop', session: 'mine' },
  ])
  expect(isSlackSession(s({ host: 'workstation', session_id: 'linked-idle' }), slack)).toBe(true)
  expect(isSlackSession(s({ host: 'workstation', session_id: 'mine' }), slack)).toBe(false)
  // Without the links (Slack off, not loaded yet) nothing is set aside.
  expect(listView(f, {}).sessions.map((x) => x.session_id)).toEqual(['linked-wait', 'linked-idle', 'mine'])
  const all = listView(f, { slack })
  expect(all.sessions.map((x) => x.session_id)).toEqual(['linked-wait', 'mine'])
  expect(all.counts.slack).toBe(2)
  expect(listView(f, { status: 'slack', slack }).sessions.map((x) => x.session_id)).toEqual(['linked-wait', 'linked-idle'])
  expect(visibleStatusFilters(all.counts, 'all').map((x) => x.id)).toEqual(['all', 'waiting', 'busy', 'idle', 'slack'])
})

test('stackRuns: 2+ adjacent members of one stack form a group; a lone member stays a row', () => {
  const st = { id: 'st-1', label: 'Login fix' }
  const rows = [s({ session_id: 'a', stack: st }), s({ session_id: 'b', stack: st }), s({ session_id: 'c' }), s({ session_id: 'd', stack: { id: 'st-2', label: 'x' } })]
  const runs = stackRuns(rows)
  expect(runs.map((r) => (r.kind === 'stack' ? r.sessions.map((x) => x.session_id).join('+') : r.session.session_id))).toEqual(['a+b', 'c', 'd'])
  expect(runs[0]).toMatchObject({ kind: 'stack', key: 'laptop/st-1' })
})
