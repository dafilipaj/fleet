// Session stacks (a StackBrief shared by N sibling sessions): pure helpers for the Stack sheet,
// the session screen's stack bar and the sibling form — unit-tested in stacks.test.ts. The list /
// board placement lives next to its peers: clusterByStack() (lib/sessions.ts) and
// withStackColumns() (lib/groups.ts).
import { ApiError } from '@/api/client'
import type { Session, StackMember, StackView } from '@/api/types'

/** A member is live while `closed` is null (the server's `live` wins when present). */
export function memberLive(m: Pick<StackMember, 'live' | 'closed'>): boolean {
  return typeof m.live === 'boolean' ? m.live : m.closed == null
}

/** Live and closed member counts. */
export function memberCounts(stack: Pick<StackView, 'members'> | null | undefined): { live: number; closed: number } {
  let live = 0
  let closed = 0
  for (const m of stack?.members ?? []) {
    if (memberLive(m)) live += 1
    else closed += 1
  }
  return { live, closed }
}

/** "2 live · 1 closed" ("2 live" when none closed). */
export function memberCountsText(stack: Pick<StackView, 'members'> | null | undefined): string {
  const { live, closed } = memberCounts(stack)
  return closed ? `${live} live · ${closed} closed` : `${live} live`
}

/** Live members first (the lead, then stack order), then closed ones, most recently closed first. */
export function sortedMembers(members: readonly StackMember[] | null | undefined, lead?: string | null): StackMember[] {
  const list = [...(members ?? [])]
  const live = list.filter(memberLive).sort((a, b) => Number(b.session === lead) - Number(a.session === lead))
  const closed = list.filter((m) => !memberLive(m)).sort((a, b) => String(b.closed ?? '').localeCompare(String(a.closed ?? '')))
  return [...live, ...closed]
}

/**
 * The server (or that host's peer) predates stacks: a 501, or the router's generic 404
 * "not found" (an unknown stack is a 404 with its own message).
 */
export function stacksMissing(err: unknown): boolean {
  return err instanceof ApiError && (err.status === 501 || (err.status === 404 && (err.message === 'not found' || /^HTTP 404$/.test(err.message))))
}

/** Does this session's host know stacks at all? Newer CLIs send `stack: null` on every row; older ones omit it. */
export function stacksKnown(s: Pick<Session, 'stack'> | null | undefined): boolean {
  return !!s && s.stack !== undefined
}

/** One line for a failed stack request (toasts / the sheet). */
export function stackErrorMessage(err: unknown): string {
  if (stacksMissing(err)) return 'This host’s fleet web server predates session stacks — update and restart it'
  if (err instanceof ApiError) {
    if (err.status === 0) return 'network unreachable'
    if (err.status === 502 || err.status === 504) return 'host unreachable'
    return err.message || `HTTP ${err.status}`
  }
  return (err as Error)?.message || 'request failed'
}

/** The stored `updated` a 409 carries (`{ error, updated }`), if any. */
export function conflictUpdated(err: unknown): string | null {
  if (!(err instanceof ApiError) || err.status !== 409) return null
  const u = (err.data as { updated?: unknown } | null)?.updated
  return typeof u === 'string' ? u : null
}

/** The host a session's stack is homed on (its StackBrief): `stack.host`, else — older servers — the session's own. */
export function stackHome(s: Pick<Session, 'host' | 'stack'>): string {
  return s.stack?.host || s.host
}

/** The Stack sheet's member → a session route (`#/s/<host>/<id>`). */
export function memberSession(m: Pick<StackMember, 'host' | 'session'>, stackHost: string): { host: string; session_id: string } {
  return { host: m.host || stackHost, session_id: m.session }
}

/** Same cap as the server's stack rename (`POST …/stacks/:id/rename`). */
export const MAX_STACK_LABEL = 80

/** Is `next` (as typed) a label to save over `prev`? Trimmed, non-empty and different. */
export function stackLabelChanged(next: string, prev: string | null | undefined): boolean {
  const t = next.trim()
  return t !== '' && t !== (prev ?? '').trim()
}

/** How long a saved rename's optimistic label may outlive the server's (a slow fleet snapshot). */
export const STACK_LABEL_TTL_MS = 30_000

/** An optimistic stack label (hooks/useStackLabels.ts). */
export interface PendingStackLabel {
  label: string
  /** Epoch ms of the rename request. */
  at: number
  /** The POST is still in flight. */
  saving: boolean
}

/**
 * The label to draw: the pending one while its rename is in flight, and after it succeeded until
 * the server's label catches up (or the TTL runs out); otherwise the server's.
 */
export function shownStackLabel(server: string, pending: PendingStackLabel | null | undefined, now = Date.now()): string {
  if (!pending) return server
  if (pending.saving) return pending.label
  return pending.label !== server && now - pending.at < STACK_LABEL_TTL_MS ? pending.label : server
}

/** What linking session `a` with `b` into one stack takes (see linkPlan). */
export type LinkPlan =
  /** Neither is in a stack: one is created around `a` (on its host — one model call), `b` joins. */
  | { kind: 'create'; host: string; sessionId: string; other: { session: string; host: string } }
  /** One of them is in a stack: the other joins it (the request goes to the stack's home host). */
  | { kind: 'join'; host: string; stackId: string; label: string; other: { session: string; host: string } }
  /** Already in the same stack: nothing to do. */
  | { kind: 'same' }
  /** Each is in a different stack: refused (leave one first). */
  | { kind: 'conflict'; label: string }

type Linkable = Pick<Session, 'host' | 'session_id' | 'stack'>

/** Link two live sessions into one stack after the fact: join the one that has a stack, else create one around `a`. */
export function linkPlan(a: Linkable, b: Linkable): LinkPlan {
  const ka = a.stack?.id ? `${stackHome(a)}/${a.stack.id}` : null
  const kb = b.stack?.id ? `${stackHome(b)}/${b.stack.id}` : null
  if (ka && kb) return ka === kb ? { kind: 'same' } : { kind: 'conflict', label: b.stack?.label || b.stack?.id || '' }
  if (a.stack?.id) return { kind: 'join', host: stackHome(a), stackId: a.stack.id, label: a.stack.label || a.stack.id, other: { session: b.session_id, host: b.host } }
  if (b.stack?.id) return { kind: 'join', host: stackHome(b), stackId: b.stack.id, label: b.stack.label || b.stack.id, other: { session: a.session_id, host: a.host } }
  return { kind: 'create', host: a.host, sessionId: a.session_id, other: { session: b.session_id, host: b.host } }
}
