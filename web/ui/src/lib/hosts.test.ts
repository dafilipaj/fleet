import { describe, expect, it } from 'vitest'

import type { Host } from '@/api/types'
import { clickHost, normalizeSelection, parseSelection, resolveSelection, selectionLabel, serializeSelection } from '@/lib/hosts'

const h = (name: string, shared = false): Host => ({ name, ok: true, shared, sessions: [] })
const HOSTS = [h('laptop'), h('workstation'), h('shed-a', true), h('shed-b', true)]
const names = (s: Set<string>) => [...s].sort()

describe('host selection', () => {
  it('reads the old single-host values and round-trips the new ones', () => {
    expect(parseSelection('*')).toEqual({ scope: 'mine' })
    expect(parseSelection('laptop')).toEqual({ hosts: ['laptop'] })
    expect(parseSelection('shared')).toEqual({ scope: 'shared' })
    for (const sel of [{ scope: 'all' as const }, { hosts: ['laptop', 'shed-a'] }]) expect(parseSelection(serializeSelection(sel))).toEqual(sel)
  })

  it('resolves scopes against the hosts, dropping unknown names', () => {
    expect(names(resolveSelection({ scope: 'mine' }, HOSTS))).toEqual(['laptop', 'workstation'])
    expect(names(resolveSelection({ scope: 'shared' }, HOSTS))).toEqual(['shed-a', 'shed-b'])
    expect(resolveSelection({ scope: 'all' }, HOSTS).size).toBe(4)
    expect(names(resolveSelection({ hosts: ['gone'] }, HOSTS))).toEqual(['laptop', 'workstation'])
    // No own host configured: "mine" shows everything.
    expect(resolveSelection({ scope: 'mine' }, [h('shed-a', true)]).size).toBe(1)
  })

  it('a chip click picks one host, a second goes back to its group; ⌘ toggles', () => {
    const mine = resolveSelection({ scope: 'mine' }, HOSTS)
    expect(clickHost(mine, 'shed-a', HOSTS, false)).toEqual({ hosts: ['shed-a'] })
    expect(clickHost(new Set(['shed-a']), 'shed-a', HOSTS, false)).toEqual({ scope: 'shared' })
    expect(clickHost(mine, 'shed-a', HOSTS, true)).toEqual({ hosts: ['laptop', 'workstation', 'shed-a'] })
    expect(clickHost(new Set(['laptop']), 'workstation', HOSTS, true)).toEqual({ scope: 'mine' })
    // The last host stays.
    expect(clickHost(new Set(['laptop']), 'laptop', HOSTS, true)).toEqual({ hosts: ['laptop'] })
  })

  it('labels: none for the default, otherwise short', () => {
    expect(selectionLabel({ scope: 'mine' }, HOSTS)).toBeNull()
    expect(selectionLabel({ hosts: ['laptop', 'workstation'] }, HOSTS)).toBeNull()
    expect(selectionLabel({ scope: 'shared' }, HOSTS)).toBe('Shared')
    expect(selectionLabel({ hosts: ['laptop', 'shed-a'] }, HOSTS)).toBe('laptop + shed-a')
    expect(normalizeSelection(['laptop', 'workstation', 'shed-a', 'shed-b'], HOSTS)).toEqual({ scope: 'all' })
  })
})
