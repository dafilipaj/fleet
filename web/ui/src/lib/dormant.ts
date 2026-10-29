// Dormant sessions (a reboot left them; `fleet restore` brings them back) — pure helpers for the
// Dormant section and the Board's dimmed cards. Unit-tested in dormant.test.ts.
import { ApiError } from '@/api/client'
import type { ClosedView, DormantView, GroupMember, RestoreResponse } from '@/api/types'
import { relTime } from '@/lib/format'

/** One host's answer to GET …/dormant (an empty list when it has none, or predates the route). */
export interface HostDormant {
  host: string
  views: DormantView[]
  /** Recently closed (ended within a boot) — never dormant, never on the Board. */
  closed: ClosedView[]
}

/** The dormant / closed lists of one GET …/dormant answer (old servers send no `closed`). */
export function hostDormant(host: string, r: { dormant?: unknown; closed?: unknown }): HostDormant {
  return { host, views: Array.isArray(r.dormant) ? (r.dormant as DormantView[]) : [], closed: Array.isArray(r.closed) ? (r.closed as ClosedView[]) : [] }
}

/** Whether a host has anything for the Dormant or Recently closed section. */
export const hasEntries = (h: HostDormant) => h.views.length > 0 || h.closed.length > 0

export const closedCount = (hosts: HostDormant[]) => hosts.reduce((n, h) => n + h.closed.length, 0)

/** "tmux · 2 windows · 3 panes · closed 2h ago" ("in tmux api · …" for a Claude session whose pane lives on). */
export function closedMeta(v: ClosedView, now = Date.now()): string {
  const plural = (n: number, w: string) => `${n} ${w}${n === 1 ? '' : 's'}`
  const parts =
    v.kind === 'tmux' ? ['tmux', plural(v.windows ?? 0, 'window'), plural(v.panes ?? 0, 'pane')] : v.tmuxSession ? [`in tmux ${v.tmuxSession}`] : ['not in tmux']
  const cwd = v.sessions?.[0]?.cwd
  if (cwd && v.kind !== 'tmux') parts.push(cwd)
  const ago = relTime(sinceMs(v.closedAt ?? v.since), now)
  if (ago) parts.push(`closed ${ago} ago`)
  return parts.join(' · ')
}

/** A dormant Claude session as the Board shows it: resume it by its session id. */
export interface DormantMember {
  host: string
  /** The session id (also what `fleet restore` takes as a target). */
  id: string
  title: string
  since: number | null
}

const key = (host: string, id: string) => `${host}/${id}`

/** ISO → epoch ms (null when missing / unparsable). */
export function sinceMs(iso: string | null | undefined): number | null {
  const t = iso ? Date.parse(iso) : NaN
  return Number.isFinite(t) ? t : null
}

/** Every dormant Claude session by `host/sessionId` — the Board's fallback when group members carry no `dormant` flag. */
export function dormantIndex(hosts: HostDormant[]): Map<string, DormantMember> {
  const out = new Map<string, DormantMember>()
  for (const h of hosts) {
    for (const v of h.views) {
      for (const s of v.sessions ?? []) {
        if (!s?.sessionId) continue
        out.set(key(h.host, s.sessionId), { host: h.host, id: s.sessionId, title: s.title || s.name || v.name || s.sessionId.slice(0, 8), since: sinceMs(v.since) })
      }
    }
  }
  return out
}

/** The dormant member a group member stands for: the server's `dormant` flag, or the dormant list. */
export function dormantMember(m: GroupMember, index: Map<string, DormantMember>): DormantMember | null {
  const found = index.get(key(m.host, m.id))
  if (found) return found
  return m.dormant === true ? { host: m.host, id: m.id, title: m.id.slice(0, 8), since: null } : null
}

/** The session search over a dormant / closed entry: its name, target, host, tmux session and Claude titles / cwds. */
export function matchesDormant(v: DormantView & { tmuxSession?: string }, host: string, query: string): boolean {
  const q = query.trim().toLowerCase()
  if (!q) return true
  return [v.name, v.target, v.tmuxSession, host, ...(v.sessions ?? []).flatMap((s) => [s?.title, s?.name, s?.cwd])]
    .filter((x): x is string => typeof x === 'string')
    .join('\n')
    .toLowerCase()
    .includes(q)
}

/** A host's dormant / closed lists narrowed to the entries the session search matches. */
export function searchDormant(h: HostDormant, query: string): HostDormant {
  if (!query.trim()) return h
  return { ...h, views: h.views.filter((v) => matchesDormant(v, h.host, query)), closed: h.closed.filter((v) => matchesDormant(v, h.host, query)) }
}

/** The session search over a Board dormant card (its title, host and session id). */
export function matchesDormantMember(m: DormantMember, query: string): boolean {
  const q = query.trim().toLowerCase()
  return !q || [m.title, m.host, m.id].join('\n').toLowerCase().includes(q)
}

/**
 * Every host's entries in one list, most recently active first (closed: `closedAt`, dormant:
 * `since` — when it went down); entries without a time go last.
 */
export function byActivity<V extends DormantView & { closedAt?: string | null }>(hosts: HostDormant[], pick: (h: HostDormant) => V[]): { host: string; view: V }[] {
  const at = (v: V) => sinceMs(v.closedAt ?? v.since) ?? -Infinity
  return hosts.flatMap((h) => pick(h).map((view) => ({ host: h.host, view }))).sort((a, b) => at(b.view) - at(a.view))
}

export const dormantCount = (hosts: HostDormant[]) => hosts.reduce((n, h) => n + h.views.length, 0)

/** The Claude titles inside a dormant entry, or what it is when it has none. */
export function dormantTitles(v: DormantView): string {
  const titles = (v.sessions ?? []).map((s) => s.title || s.name || s.sessionId.slice(0, 8))
  if (titles.length) return titles.join(' · ')
  return v.kind === 'tmux' ? 'no Claude panes' : ''
}

/** "tmux · 2 windows · 3 panes · down 3h" (lone Claude sessions: "iTerm · down 3h"). */
export function dormantMeta(v: DormantView, now = Date.now()): string {
  const plural = (n: number, w: string) => `${n} ${w}${n === 1 ? '' : 's'}`
  const lone = v.slept ? (v.sessions?.[0]?.cwd ?? 'claude') : 'not in tmux'
  const parts = v.kind === 'tmux' ? ['tmux', plural(v.windows ?? 0, 'window'), plural(v.panes ?? 0, 'pane')] : [lone]
  const since = relTime(sinceMs(v.since), now)
  if (since) parts.push(`${v.slept ? 'deactivated' : 'down'} ${since}`)
  return parts.join(' · ')
}

/** The host's server (or CLI) predates session recovery: hide its section, no error. */
export function dormantMissing(err: unknown): boolean {
  return err instanceof ApiError && (err.status === 501 || (err.status === 404 && (err.message === 'not found' || /^HTTP 404$/.test(err.message))))
}

/** A failed restore / forget, for a toast: an ambiguous target lists the candidates. */
export function dormantErrorMessage(err: unknown): string {
  if (err instanceof ApiError) {
    const c = (err.data as { candidates?: unknown } | null)?.candidates
    if (err.status === 409 && Array.isArray(c) && c.length) return `Ambiguous — matches ${c.join(', ')}`
    if (err.status === 0) return 'network unreachable'
    return err.message || `HTTP ${err.status}`
  }
  return (err as Error)?.message || 'request failed'
}

/** A restore's result as a toast: { ok, title, description }. */
export function restoreSummary(r: RestoreResponse): { ok: boolean; title: string; description: string } {
  const restored = r.restored ?? []
  const failed = r.failed ?? []
  const names = restored.map((x) => x.session).join(', ')
  const launched = restored.reduce((n, x) => n + (x.launched?.length ?? 0), 0)
  const warnings = restored.flatMap((x) => x.warnings ?? [])
  const lines = [
    launched ? `${launched} Claude session${launched === 1 ? '' : 's'} resuming` : null,
    ...failed.map((f) => `${f.target}: ${f.error}`),
    ...warnings,
  ].filter(Boolean)
  const title = restored.length ? `Resumed ${names}` : 'Nothing resumed'
  return { ok: failed.length === 0 && restored.length > 0, title, description: lines.join('\n') }
}
