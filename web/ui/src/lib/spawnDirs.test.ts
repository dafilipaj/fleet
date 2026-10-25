import { describe, expect, it } from 'vitest'

import type { FleetResponse, SpawnDirEntry, SpawnDirsResponse } from '@/api/types'

import {
  baseHost,
  checksByPath,
  divergedHosts,
  emptyRow,
  fromDraft,
  hostPathErrors,
  moveRow,
  pathProblem,
  sameList,
  toDraft,
  validateDraft,
  withOffered,
  type DraftRow,
} from './spawnDirs'

const LIST: SpawnDirEntry[] = [
  { label: 'Work', paths: { laptop: '~/Code/work', workstation: '~/Code/work' } },
  { label: 'Notes', paths: { laptop: '~/notes' } },
]

const resp = (host: string, spawnDirs = LIST): SpawnDirsResponse => ({
  host,
  hosts: ['laptop', 'workstation'],
  spawnDirs,
  checks: spawnDirs.map(() => null),
  offered: spawnDirs.filter((d) => d.paths[host]).map((d) => ({ label: d.label, path: `/abs/${d.label}` })),
  limits: { maxEntries: 30, maxLabel: 40, maxPath: 1024 },
})

describe('draft round trip', () => {
  it('keeps entries, trims, drops empty paths and gives every row a key', () => {
    const rows = toDraft(LIST)
    expect(new Set(rows.map((r) => r.key)).size).toBe(2)
    rows[1].label = ' Notes '
    rows[1].paths.workstation = '  '
    expect(fromDraft(rows)).toEqual(LIST)
  })
  it('sameList ignores host order and blank paths, not entry order', () => {
    expect(sameList(LIST, [{ label: 'Work', paths: { workstation: '~/Code/work', laptop: '~/Code/work' } }, { label: 'Notes', paths: { laptop: '~/notes', x: '' } }])).toBe(true)
    expect(sameList(LIST, [LIST[1], LIST[0]])).toBe(false)
    expect(sameList(LIST, [LIST[0], { label: 'Notes', paths: { laptop: '~/other' } }])).toBe(false)
    expect(sameList(LIST, [LIST[0]])).toBe(false)
  })
})

describe('hidden entries', () => {
  const HIDDEN: SpawnDirEntry[] = [LIST[0], { label: 'Brain', paths: { laptop: '~/brain' }, hidden: true, future: { kept: 1 } }]
  it('round-trip `hidden` and unknown keys; false is not sent', () => {
    const rows = toDraft(HIDDEN)
    expect(rows.map((r) => r.hidden)).toEqual([false, true])
    expect(fromDraft(rows)).toEqual(HIDDEN)
    rows[1].hidden = false
    expect(fromDraft(rows)[1]).toEqual({ label: 'Brain', paths: { laptop: '~/brain' }, future: { kept: 1 } })
  })
  it('sameList sees a hide/show toggle as a change', () => {
    expect(sameList(HIDDEN, HIDDEN.map((e) => ({ ...e })))).toBe(true)
    expect(sameList(HIDDEN, [HIDDEN[0], { ...HIDDEN[1], hidden: false }])).toBe(false)
  })
  it('withOffered refreshes the hidden dirs too (dropped when none)', () => {
    const fleet = {
      self: 'laptop',
      hosts: [{ name: 'laptop', ok: true, sessions: [], spawnDirs: [], hiddenSpawnDirs: [{ label: 'Old', path: '/old' }] }],
    } as FleetResponse
    const hidden = withOffered(fleet, { laptop: { ...resp('laptop'), hiddenSpawnDirs: [{ label: 'Brain', path: '/abs/brain' }] } })
    expect(hidden.hosts[0].hiddenSpawnDirs).toEqual([{ label: 'Brain', path: '/abs/brain' }])
    expect('hiddenSpawnDirs' in withOffered(fleet, { laptop: resp('laptop') }).hosts[0]).toBe(false)
  })
})

describe('validateDraft', () => {
  it('passes a good list', () => {
    expect(validateDraft(toDraft(LIST)).ok).toBe(true)
    expect(validateDraft([]).ok).toBe(true)
  })
  it('flags labels (missing, long, duplicate case-insensitively) and paths', () => {
    const rows: DraftRow[] = [
      { ...emptyRow(), paths: { laptop: '~/a' } },
      { ...emptyRow(), label: 'x'.repeat(41), paths: { laptop: '~/a' } },
      { ...emptyRow(), label: 'Work', paths: { laptop: '~/a' } },
      { ...emptyRow(), label: 'work', paths: { laptop: 'relative/dir', workstation: '/ok' } },
      { ...emptyRow(), label: 'Nothing', paths: { laptop: '' } },
    ]
    const { ok, errors } = validateDraft(rows)
    expect(ok).toBe(false)
    expect(errors[rows[0].key].label).toMatch(/name/)
    expect(errors[rows[1].key].label).toMatch(/40/)
    expect(errors[rows[2].key].label).toBeUndefined()
    expect(errors[rows[3].key].label).toMatch(/Another entry/)
    expect(errors[rows[3].key].paths).toEqual({ laptop: 'Must start with / or ~/' })
    expect(errors[rows[4].key].label).toMatch(/at least one host/)
  })
  it('caps the list', () => {
    const rows = Array.from({ length: 31 }, (_, i) => ({ ...emptyRow(), label: `L${i}`, paths: { h: '/x' } }))
    expect(validateDraft(rows).listError).toMatch(/30/)
  })
  it('pathProblem accepts ~, ~/… and absolute paths only', () => {
    expect(pathProblem('~')).toBeNull()
    expect(pathProblem('~/Code')).toBeNull()
    expect(pathProblem('/srv/x')).toBeNull()
    expect(pathProblem('')).toBeNull()
    expect(pathProblem('~other/x')).toMatch(/Must start/)
    expect(pathProblem('/a\u0000b')).toMatch(/control/)
  })
})

describe('moveRow', () => {
  it('swaps neighbours and ignores moves past either end', () => {
    expect(moveRow([1, 2, 3], 0, 1)).toEqual([2, 1, 3])
    expect(moveRow([1, 2, 3], 2, -1)).toEqual([1, 3, 2])
    const same = [1, 2]
    expect(moveRow(same, 0, -1)).toBe(same)
    expect(moveRow(same, 1, 1)).toBe(same)
  })
})

describe('server results', () => {
  it('hostPathErrors keeps only that host’s path errors, keyed by the path sent', () => {
    const sent = [LIST[0], { label: 'X', paths: { laptop: '~/x', workstation: '~/gone' } }]
    const err = {
      error: 'x',
      checks: [],
      errors: [
        { index: 0, field: 'label' as const, error: 'duplicate' },
        { index: 1, field: 'paths' as const, host: 'workstation', error: 'no such directory on workstation' },
        { index: 1, field: 'paths' as const, host: 'laptop', error: 'other host' },
      ],
    }
    expect(hostPathErrors(err, sent, 'workstation')).toEqual({ '~/gone': 'no such directory on workstation' })
    expect(hostPathErrors(null, sent, 'workstation')).toEqual({})
  })
  it('checksByPath maps a host’s checks onto its own paths', () => {
    const check = { path: '~/Code/work', resolved: '/abs/work', exists: true, isDir: true }
    expect(checksByPath({ spawnDirs: LIST, checks: [check, null] }, undefined, 'workstation')).toEqual({ '~/Code/work': check })
    expect(checksByPath({ checks: [null, check] }, [LIST[1], LIST[0]], 'laptop')).toEqual({ '~/Code/work': check })
  })
  it('baseHost prefers this server, divergedHosts names hosts with another list', () => {
    const loaded = { laptop: resp('laptop'), workstation: resp('workstation', [LIST[0]]) }
    expect(baseHost('laptop', loaded, ['laptop', 'workstation'])).toBe('laptop')
    expect(baseHost('gone', loaded, ['workstation', 'laptop'])).toBe('workstation')
    expect(baseHost('laptop', {}, ['laptop'])).toBeNull()
    expect(divergedHosts(LIST, loaded)).toEqual(['workstation'])
  })
  it('withOffered swaps in each saved host’s new spawnDirs', () => {
    const fleet = { self: 'laptop', hosts: [{ name: 'laptop', ok: true, sessions: [], spawnDirs: [] }, { name: 'workstation', ok: true, sessions: [] }] } as FleetResponse
    const next = withOffered(fleet, { laptop: resp('laptop') })
    expect(next.hosts[0].spawnDirs?.map((d) => d.label)).toEqual(['Work', 'Notes'])
    expect(next.hosts[1]).toBe(fleet.hosts[1])
  })
})
