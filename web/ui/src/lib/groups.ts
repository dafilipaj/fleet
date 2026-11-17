// Board view logic: join live sessions with the smart groups from /api/groups, the
// client-side fallback (group by repo), per-column status summaries and ordering —
// pure, unit-tested in groups.test.ts.
import type { GroupMember, GroupRun, GroupsResponse, Session, SessionGroup } from '@/api/types'
import type { DormantMember } from '@/lib/dormant'
import { relTime } from '@/lib/format'
import { byLastActivity, leadFirst, statusMeta, type StatusKey } from '@/lib/sessions'

export const UNGROUPED_ID = '__ungrouped__'

export type StatusSummary = Record<StatusKey, number>

export interface BoardColumn {
  id: string
  label: string
  description: string | null
  source: string
  /** The "Ungrouped" column: live sessions no group claims yet. */
  ungrouped: boolean
  /** Waiting first, then most recent activity. */
  sessions: Session[]
  summary: StatusSummary
  /** `source: 'stack'` columns: the session stack they show (on `host`). */
  stack?: { id: string; host: string } | null
  /** Members a reboot left dormant (not live): dimmed cards with Resume, after the live ones. */
  dormant?: DormantMember[]
}

export const STACK_COLUMN_PREFIX = 'stack:'

/** The key a group member and a live session share: `host/session_id` (or `host/pid`). */
export const memberKey = (host: string, id: string) => `${host}/${id}`

function sessionKeys(s: Session): string[] {
  const keys = [memberKey(s.host, s.session_id)]
  if (s.pid != null) keys.push(memberKey(s.host, String(s.pid)))
  return keys
}

const WORKTREE_RE = /\/(?:\.claude\/worktrees|\.worktrees|worktrees)\/[^/]+(?:\/.*)?$/

/**
 * The repo a cwd belongs to: `{ id, label }`, worktree-aware — a cwd under
 * `<repo>/.worktrees/<x>`, `<repo>/worktrees/<x>` or `<repo>/.claude/worktrees/<x>` is `<repo>`.
 */
export function repoOf(cwd: string | null | undefined): { id: string; label: string } | null {
  let p = String(cwd || '').trim().replace(/\/+$/, '')
  if (!p) return null
  p = p.replace(WORKTREE_RE, '') || '/'
  const label = p.split('/').filter(Boolean).pop() || p
  return { id: `repo:${p}`, label }
}

/** Deterministic grouping when the server has none: one group per repo (cwd). */
export function fallbackGroups(sessions: Session[]): SessionGroup[] {
  const groups = new Map<string, SessionGroup>()
  for (const s of sessions) {
    const repo = repoOf(s.cwd)
    if (!repo) continue
    let g = groups.get(repo.id)
    if (!g) {
      g = { id: repo.id, label: repo.label, description: null, source: 'fallback', members: [] }
      groups.set(repo.id, g)
    }
    g.members.push({ host: s.host, id: s.session_id })
  }
  return [...groups.values()]
}

/** The groups to draw: the server's when grouping is on, else the client fallback. */
export function effectiveGroups(resp: GroupsResponse | null | undefined, sessions: Session[]): { groups: SessionGroup[]; fallback: boolean } {
  if (resp?.enabled && Array.isArray(resp.groups)) return { groups: resp.groups, fallback: false }
  return { groups: fallbackGroups(sessions), fallback: true }
}

export function statusSummary(sessions: Session[]): StatusSummary {
  const out: StatusSummary = { waiting: 0, busy: 0, idle: 0, unknown: 0 }
  for (const s of sessions) out[statusMeta(s.status).key] += 1
  return out
}

const byUrgency = (a: Session, b: Session) => {
  const wa = statusMeta(a.status).key === 'waiting' ? 0 : 1
  const wb = statusMeta(b.status).key === 'waiting' ? 0 : 1
  return wa - wb || byLastActivity(a, b)
}

/** Columns with sessions that need you first, then busy ones, then larger, then by label. Ungrouped last. */
export function compareColumns(a: BoardColumn, b: BoardColumn): number {
  if (a.ungrouped !== b.ungrouped) return a.ungrouped ? 1 : -1
  const w = Number(b.summary.waiting > 0) - Number(a.summary.waiting > 0)
  if (w) return w
  const busy = Number(b.summary.busy > 0) - Number(a.summary.busy > 0)
  if (busy) return busy
  return b.sessions.length - a.sessions.length || a.label.localeCompare(b.label)
}

/**
 * Join `sessions` (already filtered by search/host/status) with `groups`: every session
 * lands in exactly one column (the first group that claims it), sessions no group claims
 * go to "Ungrouped", members that are not live are dropped, and empty groups disappear —
 * except a group of the user's with no members at all (`manual`, just made on the board):
 * an empty column to drop sessions into.
 */
export function boardColumns(
  sessions: Session[],
  groups: SessionGroup[],
  /** A member that is not live → its dormant card (null: not dormant / filtered out). */
  dormantOf: (m: GroupMember) => DormantMember | null = () => null,
): BoardColumn[] {
  const owner = new Map<string, number>()
  groups.forEach((g, i) => {
    for (const m of g.members ?? []) {
      const k = memberKey(m.host, m.id)
      if (!owner.has(k)) owner.set(k, i)
    }
  })
  const buckets: Session[][] = groups.map(() => [])
  const ungrouped: Session[] = []
  for (const s of sessions) {
    const idx = sessionKeys(s)
      .map((k) => owner.get(k))
      .find((i) => i !== undefined)
    if (idx === undefined) ungrouped.push(s)
    else buckets[idx].push(s)
  }
  const live = new Set(sessions.flatMap(sessionKeys))
  const placed = new Set<string>()
  const cols: BoardColumn[] = []
  groups.forEach((g, i) => {
    const dormant: DormantMember[] = []
    for (const m of g.members ?? []) {
      const k = memberKey(m.host, m.id)
      if (live.has(k) || placed.has(k)) continue
      const d = dormantOf(m)
      if (!d) continue
      placed.add(k)
      dormant.push(d)
    }
    if (!buckets[i].length && !dormant.length && !isEmptyOwnGroup(g)) return
    const list = buckets[i].sort(byUrgency)
    cols.push({
      id: g.id,
      label: g.label || g.id,
      description: g.description ?? null,
      source: String(g.source ?? 'llm'),
      ungrouped: false,
      sessions: list,
      summary: statusSummary(list),
      ...(dormant.length ? { dormant } : {}),
    })
  })
  if (ungrouped.length) {
    const list = ungrouped.sort(byUrgency)
    cols.push({
      id: UNGROUPED_ID,
      label: 'Ungrouped',
      description: 'Not grouped yet',
      source: 'none',
      ungrouped: true,
      sessions: list,
      summary: statusSummary(list),
    })
  }
  return cols.sort(compareColumns)
}

/**
 * Session stacks as board columns: every stack a session in `sessions` belongs to (`session.stack`)
 * becomes a column `stack:<id>` (source `stack`), placed before the other columns; its members
 * leave the other columns, which vanish when that empties them. `sessions` = what the board shows
 * (already filtered). No-op when no session carries a stack (older servers).
 */
export function withStackColumns(columns: BoardColumn[], sessions: Session[]): BoardColumn[] {
  const stacks = new Map<string, { id: string; host: string; label: string; sessions: Session[] }>()
  for (const s of sessions) {
    const st = s.stack
    if (!st?.id) continue
    // One column per stack, keyed by its home host: members on other hosts join it.
    const home = st.host || s.host
    const k = memberKey(home, st.id)
    const entry = stacks.get(k)
    if (entry) entry.sessions.push(s)
    else stacks.set(k, { id: st.id, host: home, label: st.label || st.id, sessions: [s] })
  }
  if (!stacks.size) return columns
  const inStack = new Set([...stacks.values()].flatMap((st) => st.sessions.map((s) => memberKey(s.host, s.session_id))))
  const ids = new Set<string>()
  const stackCols: BoardColumn[] = [...stacks.values()]
    .map((st) => {
      // Stack ids are random per host; two hosts sharing one would still get distinct columns.
      let id = `${STACK_COLUMN_PREFIX}${st.id}`
      if (ids.has(id)) id = `${STACK_COLUMN_PREFIX}${st.host}/${st.id}`
      ids.add(id)
      // The lead (orchestrator) first, the rest by urgency.
      const list = leadFirst([...st.sessions].sort(byUrgency))
      return {
        id,
        label: st.label,
        description: null,
        source: 'stack',
        ungrouped: false,
        sessions: list,
        summary: statusSummary(list),
        stack: { id: st.id, host: st.host },
      }
    })
    .sort(compareColumns)
  const rest: BoardColumn[] = []
  for (const c of columns) {
    const left = c.sessions.filter((s) => !inStack.has(memberKey(s.host, s.session_id)))
    if (!left.length && !c.dormant?.length) continue
    rest.push(left.length === c.sessions.length ? c : { ...c, sessions: left, summary: statusSummary(left) })
  }
  return [...stackCols, ...rest]
}

/** How many column ids `fleet.boardOrder` remembers (absent ones are forgotten first). */
export const BOARD_ORDER_MAX = 100

/**
 * Sticky column order: columns already in `prev` keep their place (live status changes never
 * reshuffle the board), new ones are appended in `compareColumns` order (new stack columns are
 * put first instead), Ungrouped stays last.
 * Returns the columns in that order and the updated order to remember — ids of columns that
 * are gone for now (filtered out, no live sessions) keep their slot so they come back in place.
 */
export function stickyColumns(columns: BoardColumn[], prev: readonly string[]): { columns: BoardColumn[]; order: string[] } {
  const known = new Set(prev)
  const freshCols = columns.filter((c) => !c.ungrouped && !known.has(c.id))
  // A new stack column starts at the front (stacks lead the board); other new columns are appended.
  const freshStacks = freshCols.filter((c) => c.source === 'stack').map((c) => c.id)
  const fresh = freshCols.filter((c) => c.source !== 'stack').map((c) => c.id)
  let order = [...freshStacks, ...prev.filter((id) => id !== UNGROUPED_ID), ...fresh]
  if (order.length > BOARD_ORDER_MAX) {
    const present = new Set(columns.map((c) => c.id))
    let drop = order.length - BOARD_ORDER_MAX
    order = order.filter((id) => present.has(id) || drop-- <= 0)
  }
  const rank = new Map(order.map((id, i) => [id, i]))
  const sorted = [...columns].sort((a, b) => {
    if (a.ungrouped !== b.ungrouped) return a.ungrouped ? 1 : -1
    return (rank.get(a.id) ?? 0) - (rank.get(b.id) ?? 0)
  })
  return { columns: sorted, order }
}

/**
 * The remembered column order with `id` moved just before `before` (null: to the end).
 * Ids missing from `order` (a column not remembered yet) are appended first.
 */
export function reorderColumns(order: readonly string[], id: string, before: string | null): string[] {
  if (id === before || id === UNGROUPED_ID) return [...order]
  const out = order.filter((x) => x !== id)
  const at = before == null || before === UNGROUPED_ID ? -1 : out.indexOf(before)
  if (at < 0) out.push(id)
  else out.splice(at, 0, id)
  return out
}

/** A change made on the board — the body of POST /api/groups/edit. */
/** A group the user made on the board that nobody is in yet — shown as an empty column. */
export const isEmptyOwnGroup = (g: SessionGroup) => g.source === 'manual' && !(g.members?.length ?? 0)

export type GroupEdit =
  | { op: 'rename'; id: string; label: string }
  /** An empty group of the user's (it stays until deleted). */
  | { op: 'create'; label: string }
  /** An empty group. */
  | { op: 'delete'; id: string }
  /** Into group `to`, or (with `label`) a new group of that name — an existing one of that name is reused. */
  | { op: 'move'; host: string; session: string; to: string; label?: undefined }
  | { op: 'move'; host: string; session: string; label: string; to?: undefined }

/** The id a group member uses for `s`: its session id, else its pid. */
export const memberId = (s: Session): string => s.session_id || (s.pid != null ? String(s.pid) : '')

/** Id of a group made optimistically on the board, until the server answers with the real one. */
export const PENDING_PREFIX = 'pending:'

/**
 * `resp` with `edit` applied locally — the optimistic board while the server saves it
 * (mirrors `core::grouping::{rename_group, move_session}`: a moved-from group left empty goes).
 */
export function applyGroupEdit(resp: GroupsResponse, edit: GroupEdit): GroupsResponse {
  if (edit.op === 'rename') {
    return { ...resp, groups: resp.groups.map((g) => (g.id === edit.id ? { ...g, label: edit.label } : g)) }
  }
  if (edit.op === 'create') {
    const g: SessionGroup = { id: `${PENDING_PREFIX}${edit.label}`, label: edit.label, description: null, source: 'manual', members: [] }
    return { ...resp, groups: [...resp.groups, g] }
  }
  if (edit.op === 'delete') return { ...resp, groups: resp.groups.filter((g) => g.id !== edit.id) }
  const member = { host: edit.host, id: edit.session }
  const same = (m: { host: string; id: string }) => m.host === member.host && m.id === member.id
  let groups = resp.groups.map((g) => ({ ...g, members: (g.members ?? []).filter((m) => !same(m)) }))
  const label = edit.label ?? ''
  let target = edit.to ?? groups.find((g) => g.label.toLowerCase() === label.toLowerCase())?.id
  if (!target) {
    target = `${PENDING_PREFIX}${label}`
    groups.push({ id: target, label, description: null, source: 'manual', members: [] })
  }
  groups = groups
    .map((g) => (g.id === target ? { ...g, members: [...g.members, member] } : g))
    .filter((g) => g.members.length > 0 || g.source === 'manual')
  return { ...resp, groups }
}

/** Sessions in board order (columns left→right, cards top→bottom) — the j/k cursor order. */
export function boardOrder(columns: BoardColumn[]): Session[] {
  return columns.flatMap((c) => c.sessions)
}

/** "grouped 3m ago · 1 model call", "fallback: by repo", "grouping failed 2m ago". */
export function groupsStatusText(resp: GroupsResponse | null | undefined, now = Date.now()): string {
  if (!resp || !resp.enabled) return 'fallback: by repo'
  if (resp.running) return 'grouping…'
  const run: GroupRun | null = resp.lastRun
  if (!run) return resp.updatedAt ? `grouped ${relTime(resp.updatedAt, now)} ago` : 'not grouped yet'
  const ago = relTime(run.at, now)
  if (!run.ok) return `grouping failed ${ago} ago`
  const calls = run.modelCalls ?? 0
  const at = resp.updatedAt && resp.updatedAt > run.at ? relTime(resp.updatedAt, now) : ago
  const suffix = run.mode === 'fallback' ? ' · fallback' : ` · ${calls} model call${calls === 1 ? '' : 's'}`
  return `grouped ${at} ago${suffix}`
}

/** One line for the toast after "Regroup now". */
export function regroupToast(resp: GroupsResponse): string {
  const run = resp.lastRun
  if (!run) return 'Grouping done'
  if (!run.ok) return `Grouping failed: ${run.error || 'unknown error'}`
  const n = resp.groups?.length ?? 0
  if (run.mode === 'noop') return `No changes · ${n} group${n === 1 ? '' : 's'}`
  const calls = run.modelCalls ?? 0
  const classified = run.classified ?? 0
  return `Grouped ${classified} session${classified === 1 ? '' : 's'} into ${n} group${n === 1 ? '' : 's'} · ${calls} model call${calls === 1 ? '' : 's'}`
}

/** `fleet.groupsCollapsed` (collapsed group ids, mobile board) and `fleet.boardOrder`: a list of ids. */
export function parseIdList(raw: unknown): string[] {
  return Array.isArray(raw) ? raw.filter((x): x is string => typeof x === 'string') : []
}

export type ViewMode = 'list' | 'board'
export const parseViewMode = (raw: string): ViewMode | undefined => (raw === 'list' || raw === 'board' ? raw : undefined)
