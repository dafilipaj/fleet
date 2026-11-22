// Session list logic: status metadata, filtering, search, sorting — pure, unit-tested.
import type { FleetResponse, Session, SessionStatus } from '@/api/types'
import { isBack, isSnoozed } from '@/lib/snooze'

export type StatusKey = SessionStatus
export interface StatusMeta {
  key: StatusKey
  /** Human label. */
  label: string
}

const STATUS_META: Record<StatusKey, StatusMeta> = {
  waiting: { key: 'waiting', label: 'needs you' },
  busy: { key: 'busy', label: 'working' },
  idle: { key: 'idle', label: 'idle' },
  unknown: { key: 'unknown', label: 'unknown' },
}

export function statusMeta(status: string | null | undefined): StatusMeta {
  const k = String(status || '').toLowerCase() as StatusKey
  return STATUS_META[k] ?? STATUS_META.unknown
}

/**
 * Its `fleet done` mark, while it sits idle on it: a session that works or waits again is not
 * done (the CLI already drops a mark the user prompted past). null = not done.
 */
export function sessionDone(s: Pick<Session, 'status' | 'done'> | null | undefined): Session['done'] {
  return s?.done && statusMeta(s.status).key === 'idle' ? s.done : null
}

/** "needs you · permission" for a waiting session with a reason, else the plain label. */
export function statusLabel(s: Pick<Session, 'status' | 'waiting_for' | 'background'>): string {
  const meta = statusMeta(s.status)
  const why = typeof s.waiting_for === 'string' ? s.waiting_for.trim() : ''
  if (meta.key === 'waiting' && why) return `${meta.label} · ${why}`
  const bg = backgroundLabel(s.background)
  return bg ? `${meta.label} · ${bg}` : meta.label
}

/** "2 agents", "1 agent, 1 shell" — what runs beside the main thread; '' for nothing. */
export function backgroundLabel(bg: Session['background']): string {
  if (!bg) return ''
  const n = bg.agents?.length ?? 0
  return [n ? `${n} agent${n === 1 ? '' : 's'}` : '', bg.shells ? `${bg.shells} shell${bg.shells === 1 ? '' : 's'}` : ''].filter(Boolean).join(', ')
}

export type StatusFilterId = 'all' | 'waiting' | 'busy' | 'idle' | 'scheduled' | 'slack' | 'veranda' | 'github' | 'productive' | 'embedded'

/** The bridges a session can be linked to: people there talk to it (Slack thread, Veranda room, GitHub PR, Productive task). */
export type LinkKind = 'slack' | 'veranda' | 'github' | 'productive'
/** Per bridge, the `host/session_id` keys of the sessions with an active link (`linkedKeys`); a missing one = none. */
export type Linked = Partial<Record<LinkKind, ReadonlySet<string>>>

export interface StatusFilter {
  id: StatusFilterId
  label: string
  match: (s: Session, linked: Linked) => boolean
  /** The chip's tooltip. */
  title?: string
}

/** The filters for sessions set aside from the rest (bots, links, app panels): a chip only when there are some. */
export const SET_ASIDE: StatusFilterId[] = ['scheduled', 'slack', 'veranda', 'github', 'productive', 'embedded']

export const STATUS_FILTERS: StatusFilter[] = [
  { id: 'all', label: 'All', match: () => true },
  { id: 'waiting', label: 'Needs you', match: (s) => statusMeta(s.status).key === 'waiting' },
  { id: 'busy', label: 'Busy', match: (s) => statusMeta(s.status).key === 'busy' },
  { id: 'idle', label: 'Idle', match: (s) => statusMeta(s.status).key === 'idle' },
  // Only the sessions bots started / linked to Slack, Veranda or GitHub / an app's embedded panel
  // started (every other filter leaves them out unless waiting).
  { id: 'scheduled', label: 'Bots', match: (s) => isJobSession(s), title: 'Sessions started by bots' },
  { id: 'slack', label: 'Slack', match: (s, l) => isLinked(s, l.slack), title: 'Sessions linked to Slack: people in a thread talk to them' },
  { id: 'veranda', label: 'Veranda', match: (s, l) => isLinked(s, l.veranda), title: 'Sessions linked to a Veranda room: its guests talk to them' },
  { id: 'github', label: 'GitHub', match: (s, l) => isLinked(s, l.github), title: 'Sessions linked to a GitHub pull request: they follow it and answer there' },
  { id: 'productive', label: 'Productive', match: (s, l) => isLinked(s, l.productive), title: 'Sessions linked to a Productive task: they follow its comments and answer there' },
  { id: 'embedded', label: 'Apps', match: (s) => isEmbedSession(s), title: "Sessions started from an app's Fleet panel (embed view)" },
]

/** The filters worth a chip: Bots / the link kinds / Apps only when there are such sessions, or it is the one picked. */
export function visibleStatusFilters(counts: Record<StatusFilterId, number> | null, current: StatusFilterId): StatusFilter[] {
  return STATUS_FILTERS.filter((f) => !SET_ASIDE.includes(f.id) || current === f.id || (counts?.[f.id] ?? 0) > 0)
}

export function findStatusFilter(id: string | null | undefined): StatusFilter {
  return STATUS_FILTERS.find((f) => f.id === id) ?? STATUS_FILTERS[0]
}

/** Every session across hosts (unreachable hosts contribute none), voice operators left out (they live in #/operator). */
export function allSessions(fleet: FleetResponse | null | undefined): Session[] {
  const out: Session[] = []
  for (const host of fleet?.hosts ?? []) for (const s of host.sessions ?? []) if (!s.operator) out.push({ ...s, host: s.host ?? host.name })
  return out
}

export function matchesSearch(s: Session, query: string): boolean {
  const q = query.trim().toLowerCase()
  if (!q) return true
  return [s.display_title, s.name, s.gen_title, s.title, s.cwd, s.tmux_session, s.host]
    .filter((v): v is string => typeof v === 'string')
    .join('\n')
    .toLowerCase()
    .includes(q)
}

/** Most recent activity first (a flat list across hosts). */
export function byLastActivity(a: Session, b: Session): number {
  return (b.updated_at || 0) - (a.updated_at || 0)
}

/**
 * Most recent prompt first: a session moves when someone talks to it, not on every turn
 * start / end. Without a `prompted_at` (no prompt yet, Codex / pi) its last activity stands in.
 */
export function byLastPrompt(a: Session, b: Session): number {
  return (b.prompted_at || b.updated_at || 0) - (a.prompted_at || a.updated_at || 0)
}

/**
 * `sessions` in the order `prev` (session keys) had them — the list held still while the
 * pointer is on it. Sessions new since then go on top, in their own order.
 */
export function keepOrder(sessions: Session[], prev: readonly string[]): Session[] {
  const at = new Map(prev.map((k, i) => [k, i]))
  const rank = (s: Session) => at.get(`${s.host}/${s.session_id}`) ?? -1
  return sessions
    .map((s, i) => ({ s, i, r: rank(s) }))
    .sort((x, y) => x.r - y.r || x.i - y.i)
    .map((x) => x.s)
}

export interface ListView {
  /** Sessions to show: search + host + status filter applied, most recent prompt first. */
  sessions: Session[]
  /** Per status-filter counts, after search + host scoping (what the chips show). */
  counts: Record<StatusFilterId, number>
}

/** A session a bot started (`origin`, docs/cli.md → Bots). */
export function isJobSession(s: Pick<Session, 'origin'>): boolean {
  return Boolean(s.origin?.job)
}

/** A session an app's embedded Fleet panel started (`embed`, docs/architecture.md → Embed view). */
export function isEmbedSession(s: Pick<Session, 'embed'>): boolean {
  return Boolean(s.embed)
}

/**
 * The `host/session_id` keys of the sessions with an active link (Slack, Veranda, GitHub). A link
 * without a host (Veranda's on the bridge itself) is on `fallbackHost`.
 */
export function linkedKeys(
  links: readonly { host?: string | null; session: string }[] | null | undefined,
  fallbackHost: string | null = null,
): ReadonlySet<string> {
  return new Set((links ?? []).map((l) => `${l.host ?? fallbackHost}/${l.session}`))
}

/** A session in `keys` (`linkedKeys`): people on that bridge talk to it, not you. */
export function isLinked(s: Pick<Session, 'host' | 'session_id'>, keys: ReadonlySet<string> | undefined): boolean {
  return !!keys?.has(`${s.host}/${s.session_id}`)
}

const NOT_LINKED: Linked = {}

/**
 * Job, linked (Slack / Veranda / GitHub) and embed sessions stay out of every filter but their own —
 * except one waiting on you, which always shows (and always counts).
 */
export function setAside(s: Session, linked: Linked = NOT_LINKED): boolean {
  if (statusMeta(s.status).key === 'waiting') return false
  return isJobSession(s) || isEmbedSession(s) || isLinked(s, linked.slack) || isLinked(s, linked.veranda) || isLinked(s, linked.github) || isLinked(s, linked.productive)
}

/**
 * `host` null = every host. Bots / Slack / Veranda / GitHub / Apps show only their own sessions;
 * the other filters show the rest, plus those waiting on you. Snoozed sessions (their time still
 * ahead of `now`) are left out — they live in Snoozed (`snoozedView`); one back from a snooze
 * goes on top.
 */
export function listView(
  fleet: FleetResponse | null | undefined,
  {
    query = '',
    hosts = null,
    status = 'all',
    linked = NOT_LINKED,
    now = Date.now(),
  }: { query?: string; hosts?: ReadonlySet<string> | null; status?: StatusFilterId; linked?: Linked; now?: number },
): ListView {
  const matched = allSessions(fleet).filter((s) => (hosts == null || hosts.has(s.host)) && !isSnoozed(s, now) && matchesSearch(s, query))
  const visible = matched.filter((s) => !setAside(s, linked))
  const scope = (f: StatusFilter) => (SET_ASIDE.includes(f.id) ? matched : visible).filter((s) => f.match(s, linked))
  const counts = Object.fromEntries(STATUS_FILTERS.map((f) => [f.id, scope(f).length])) as Record<StatusFilterId, number>
  const backFirst = (a: Session, b: Session) => Number(isBack(b, now)) - Number(isBack(a, now)) || byLastPrompt(a, b)
  return { sessions: clusterByStack(scope(findStatusFilter(status)).sort(backFirst)), counts }
}

/** The snoozed sessions (time still ahead of `now`) the search + host scope match, soonest back first. */
export function snoozedView(
  fleet: FleetResponse | null | undefined,
  { query = '', hosts = null, now = Date.now() }: { query?: string; hosts?: ReadonlySet<string> | null; now?: number },
): Session[] {
  return allSessions(fleet)
    .filter((s) => (hosts == null || hosts.has(s.host)) && isSnoozed(s, now) && matchesSearch(s, query))
    .sort((a, b) => (a.snooze?.until ?? 0) - (b.snooze?.until ?? 0))
}

/** Every session by status, job sessions included, snoozed ones left out — the header's counters. `hosts`: only those. */
export function statusSummary(fleet: FleetResponse | null | undefined, hosts: ReadonlySet<string> | null = null, now = Date.now()): Record<'waiting' | 'busy' | 'idle', number> {
  const out = { waiting: 0, busy: 0, idle: 0 }
  for (const s of allSessions(fleet)) {
    if ((hosts && !hosts.has(s.host)) || isSnoozed(s, now)) continue
    const k = statusMeta(s.status).key
    if (k === 'waiting' || k === 'busy' || k === 'idle') out[k] += 1
  }
  return out
}

/** A stack's identity across hosts: its home host + id; null when the session is in none. */
function stackKey(s: Session): string | null {
  return s.stack?.id ? `${s.stack.host || s.host}/${s.stack.id}` : null
}

/** Whether `s` is its stack's lead (orchestrator). */
export function isStackLead(s: Session): boolean {
  return !!s.stack?.lead && s.stack.lead === s.session_id
}

/** One stack's members with its lead first; the others keep their order. */
export function leadFirst(members: Session[]): Session[] {
  const i = members.findIndex(isStackLead)
  return i <= 0 ? members : [members[i], ...members.slice(0, i), ...members.slice(i + 1)]
}

/**
 * Keep the sessions of one stack adjacent: every stack moves up to the position of its first
 * member, its lead first and the other members in their relative order; everything else stays
 * where it was. Stable, pure; rows without `stack` (older servers) come back unchanged.
 */
export function clusterByStack(sessions: Session[]): Session[] {
  // Keyed by the stack's home host: members on other hosts cluster with it.
  const key = stackKey
  const members = new Map<string, Session[]>()
  for (const s of sessions) {
    const k = key(s)
    if (!k) continue
    const list = members.get(k)
    if (list) list.push(s)
    else members.set(k, [s])
  }
  if (![...members.values()].some((l) => l.length > 1)) return sessions
  const out: Session[] = []
  const done = new Set<string>()
  for (const s of sessions) {
    const k = key(s)
    if (!k) out.push(s)
    else if (!done.has(k)) {
      done.add(k)
      out.push(...leadFirst(members.get(k) ?? [s]))
    }
  }
  return out
}

export function findSession(fleet: FleetResponse | null | undefined, host: string, id: string): Session | null {
  const entry = fleet?.hosts?.find((h) => h.name === host)
  return entry?.sessions?.find((s) => s.session_id === id) ?? null
}

/** Hash route of a session's detail screen: `#/s/<host>/<session_id>`. */
export function sessionHref(s: Pick<Session, 'host' | 'session_id'>): string {
  return `/s/${encodeURIComponent(s.host)}/${encodeURIComponent(s.session_id)}`
}

/** May `host` start agent sessions? False only for a host with config `spawn: false` (unknown hosts: yes). */
export function canSpawnOn(fleet: FleetResponse | null | undefined, host: string | null | undefined): boolean {
  return fleet?.hosts?.find((h) => h.name === host)?.spawn !== false
}

/** Hosts a session can be spawned on: reachable ones that run agent sessions, with the directories each advertises. */
/** `hiddenDirs`: the host's hidden start directories (allowed, offered only on request in New session). */
export function spawnTargets(
  fleet: FleetResponse | null | undefined,
): { name: string; dirs: { label: string; path: string }[]; hiddenDirs: { label: string; path: string }[]; shared: boolean }[] {
  const all = (fleet?.hosts ?? [])
    .filter((h) => h.ok !== false && h.spawn !== false)
    .map((h) => ({
      name: h.name,
      dirs: Array.isArray(h.spawnDirs) ? h.spawnDirs : [],
      hiddenDirs: Array.isArray(h.hiddenSpawnDirs) ? h.hiddenSpawnDirs : [],
      shared: h.shared === true,
    }))
  // The user's own hosts first (the default pick), shared sheds last.
  return [...all.filter((h) => !h.shared), ...all.filter((h) => h.shared)]
}

/** The freshly spawned session, once Claude registered it: its tmux session (or name) matches. */
export function findSpawned(
  fleet: FleetResponse | null | undefined,
  host: string,
  spawned: { tmuxSession?: string; name?: string },
): Session | null {
  const entry = fleet?.hosts?.find((h) => h.name === host)
  const tmux = spawned.tmuxSession || spawned.name
  return (
    entry?.sessions?.find((s) => (tmux && s.tmux_session === tmux) || (spawned.name && s.name === spawned.name)) ?? null
  )
}

/** The fleet minus one session (optimistic update after closing it). */
export function withoutSession(fleet: FleetResponse, host: string, id: string): FleetResponse {
  return {
    ...fleet,
    hosts: fleet.hosts.map((h) => (h.name === host ? { ...h, sessions: h.sessions.filter((s) => s.session_id !== id) } : h)),
  }
}

/** The key `withoutClosed` matches on: host + session id. */
export const closedKey = (host: string, id: string) => `${host}\u0000${id}`

/** The fleet minus the sessions closed recently (`closed`: closedKey → hide-until ms); same object when nothing matches. */
export function withoutClosed(fleet: FleetResponse, closed: ReadonlyMap<string, number>, now = Date.now()): FleetResponse {
  const hidden = (h: string, id: string) => (closed.get(closedKey(h, id)) ?? 0) > now
  if (!fleet.hosts.some((h) => h.sessions.some((s) => hidden(h.name, s.session_id)))) return fleet
  return { ...fleet, hosts: fleet.hosts.map((h) => ({ ...h, sessions: h.sessions.filter((s) => !hidden(h.name, s.session_id)) })) }
}

/** A list entry: one session, or the adjacent members of one stack (2 or more) shown as a group. */
export type ListEntry = { kind: 'session'; session: Session } | { kind: 'stack'; key: string; sessions: Session[] }

/**
 * Split a (clustered) list into entries: each run of 2+ adjacent members of one stack becomes a
 * `stack` entry, a lone member stays a plain `session`. Order is kept as given.
 */
export function stackRuns(sessions: Session[]): ListEntry[] {
  const out: ListEntry[] = []
  for (let i = 0; i < sessions.length; ) {
    const k = stackKey(sessions[i])
    let j = i + 1
    while (k && j < sessions.length && stackKey(sessions[j]) === k) j += 1
    if (k && j - i > 1) out.push({ kind: 'stack', key: k, sessions: sessions.slice(i, j) })
    else out.push({ kind: 'session', session: sessions[i] })
    i = j
  }
  return out
}
