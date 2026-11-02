import type { Host } from '@/api/types'

/**
 * Which hosts the session views show. A scope follows the config (`hosts.<name>.shared`): `mine`
 * = the user's own hosts, `shared` = shared sheds, `all` = both. A host list is an explicit pick
 * (multi-select). Persisted as `fleet.hostFilter`: `mine` | `shared` | `all` | `h:<name>,<name>`.
 */
export type HostScope = 'mine' | 'shared' | 'all'
export type HostSelection = { scope: HostScope } | { hosts: string[] }

export const DEFAULT_SELECTION: HostSelection = { scope: 'mine' }

export function isShared(h: Pick<Host, 'shared'> | null | undefined): boolean {
  return h?.shared === true
}

/** Stored string → selection. The old single-host values (`*`, a bare name) still read. */
export function parseSelection(raw: string): HostSelection {
  if (raw === 'mine' || raw === 'shared' || raw === 'all') return { scope: raw }
  if (raw === '*' || !raw) return DEFAULT_SELECTION
  const names = (raw.startsWith('h:') ? raw.slice(2) : raw).split(',').filter(Boolean)
  return names.length ? { hosts: names } : DEFAULT_SELECTION
}

export function serializeSelection(sel: HostSelection): string {
  return 'scope' in sel ? sel.scope : `h:${sel.hosts.join(',')}`
}

export function scopeHosts(hosts: Host[], scope: HostScope): string[] {
  return hosts.filter((h) => scope === 'all' || (scope === 'shared') === isShared(h)).map((h) => h.name)
}

/**
 * The host names a selection shows, among the configured `hosts`. Names no longer configured are
 * dropped; an empty pick falls back to `mine`, and `mine` with no own host to `all`.
 */
export function resolveSelection(sel: HostSelection, hosts: Host[]): Set<string> {
  if ('hosts' in sel) {
    const known = new Set(hosts.map((h) => h.name))
    const picked = sel.hosts.filter((n) => known.has(n))
    if (picked.length) return new Set(picked)
    return resolveSelection(DEFAULT_SELECTION, hosts)
  }
  const names = scopeHosts(hosts, sel.scope)
  if (!names.length && sel.scope === 'mine') return new Set(hosts.map((h) => h.name))
  return new Set(names)
}

/** A resolved set back to the simplest selection that means it (a whole group → its scope). */
export function normalizeSelection(names: Iterable<string>, hosts: Host[]): HostSelection {
  const set = new Set(names)
  const same = (list: string[]) => list.length === set.size && list.every((n) => set.has(n))
  if (!set.size) return DEFAULT_SELECTION
  for (const scope of ['mine', 'shared', 'all'] as const) {
    const list = scopeHosts(hosts, scope)
    if (list.length && same(list)) return { scope }
  }
  return { hosts: hosts.map((h) => h.name).filter((n) => set.has(n)) }
}

/**
 * A click on a host chip. Plain: only that host, or back to its group when it already was the
 * only one. `add` (⌘ / Ctrl / Shift): toggle it in the current set (the last one stays).
 */
export function clickHost(current: Set<string>, name: string, hosts: Host[], add: boolean): HostSelection {
  if (add) {
    const next = new Set(current)
    if (next.has(name)) {
      if (next.size > 1) next.delete(name)
    } else next.add(name)
    return normalizeSelection(next, hosts)
  }
  if (current.size === 1 && current.has(name)) {
    return { scope: isShared(hosts.find((h) => h.name === name)) ? 'shared' : 'mine' }
  }
  return { hosts: [name] }
}

/** A click on a group label (`mine` / `shared`). `add`: toggle the whole group in the current set. */
export function clickGroup(current: Set<string>, scope: 'mine' | 'shared', hosts: Host[], add: boolean): HostSelection {
  if (!add) return { scope }
  const group = scopeHosts(hosts, scope)
  const all = group.every((n) => current.has(n))
  const next = new Set(current)
  for (const n of group) {
    if (all) next.delete(n)
    else next.add(n)
  }
  return next.size ? normalizeSelection(next, hosts) : { scope }
}

/** Short text for the current selection, e.g. in the mobile filter summary; null for the default. */
export function selectionLabel(sel: HostSelection, hosts: Host[]): string | null {
  const names = [...resolveSelection(sel, hosts)]
  const norm = normalizeSelection(names, hosts)
  if ('scope' in norm) {
    if (norm.scope === 'mine') return null
    return norm.scope === 'shared' ? 'Shared' : 'All hosts'
  }
  return names.length <= 2 ? names.join(' + ') : `${names.length} hosts`
}
