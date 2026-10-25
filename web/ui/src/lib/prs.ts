import type { PrColumn, PrColumnId, PrRotting, PrsResponse, PullRequest, Session } from '@/api/types'

/** Display order and titles when a server sends no `columns` (and the order it should send). */
export const PR_COLUMNS: { id: PrColumnId; title: string }[] = [
  { id: 'unseenMentions', title: 'Unseen mentions' },
  { id: 'waitingOnMe', title: 'Waiting on me' },
  { id: 'reviewMine', title: 'Mine, in review' },
  { id: 'readyToMergeMine', title: 'Mine, ready to merge' },
  { id: 'draftMine', title: 'My drafts' },
  { id: 'waitingOnAuthor', title: 'Waiting on author' },
]

export interface PrGroup extends PrColumn {
  prs: PullRequest[]
}

/**
 * Someone else's draft — what the Drafts toggle hides. Not when it is in Unseen mentions: a
 * mention on a draft is still addressed to you.
 */
export const isOthersDraft = (p: PullRequest, me: string) => p.draft && p.column !== 'unseenMentions' && p.author.toLowerCase() !== me.toLowerCase()

/**
 * The answer's columns, each with its PRs (in the server's order); unknown columns are kept, last.
 * `hideOthersDrafts`: leave out other people's drafts ([isOthersDraft]).
 */
export function groupPrs(
  r: Pick<PrsResponse, 'columns' | 'prs'> & { user?: string } | null,
  filter = '',
  { hideOthersDrafts = false }: { hideOthersDrafts?: boolean } = {},
): PrGroup[] {
  if (!r) return []
  const cols = r.columns?.length ? r.columns : PR_COLUMNS.map((c) => ({ ...c, count: 0 }))
  const q = filter.trim().toLowerCase()
  const hit = (p: PullRequest) =>
    !q || [p.title, p.repo, `#${p.number}`, `${p.repo}#${p.number}`, p.author, p.headBranch].some((s) => s.toLowerCase().includes(q))
  const groups: PrGroup[] = cols.map((c) => ({ ...c, prs: [] }))
  const byId = new Map(groups.map((g) => [g.id as string, g]))
  for (const p of r.prs ?? []) {
    if (!hit(p)) continue
    if (hideOthersDrafts && isOthersDraft(p, r.user ?? '')) continue
    let g = byId.get(p.column)
    if (!g) {
      g = { id: p.column, title: p.column, count: 0, prs: [] }
      byId.set(p.column, g)
      groups.push(g)
    }
    g.prs.push(p)
  }
  return groups
}

/** Since when it has waited (ms): opened, or in Unseen mentions the mention — what `rotting` counts from. */
export const waitingSince = (p: PullRequest) => (p.column === 'unseenMentions' && p.mention ? p.mention.at : p.createdAt)

/** Card accent per rotting stage: `border` (left edge) and `text` (the age). Fresh: none. */
export const ROT_TONE: Record<PrRotting, { border: string; text: string } | null> = {
  fresh: null,
  warming: { border: 'border-l-status-busy', text: 'text-status-busy' },
  stale: { border: 'border-l-status-waiting', text: 'text-status-waiting' },
  rotting: { border: 'border-l-status-rotting', text: 'text-status-rotting' },
  critical: { border: 'border-l-status-error', text: 'font-semibold text-status-error' },
}

export type ChecksTone = 'ok' | 'bad' | 'running' | 'none'

export function checksTone(c: PullRequest['checks']): ChecksTone {
  if (c === 'success') return 'ok'
  if (c === 'failure' || c === 'error') return 'bad'
  if (c === 'pending' || c === 'expected') return 'running'
  return 'none'
}

export const CHECKS_LABEL: Record<ChecksTone, string> = { ok: 'Checks passed', bad: 'Checks failed', running: 'Checks running', none: 'No checks' }

/** Short review state for a card: what this PR is waiting for, from your side. */
export function reviewLabel(p: PullRequest): string | null {
  if (p.myReview?.newCommits) return 'new commits since your review'
  if (p.myReview?.state === 'changesRequested') return 'you requested changes'
  if (p.myReview?.state === 'commented') return 'you commented'
  if (p.reviewDecision === 'changesRequested') return 'changes requested'
  if (p.reviewDecision === 'approved') return 'approved'
  return null
}

/** `owner/repo#123`: how the server names a PR. */
export const prRef = (p: Pick<PullRequest, 'owner' | 'repo' | 'number'>) => `${p.owner}/${p.repo}#${p.number}`

/** Starting prompts offered for a PR, the most likely first. */
export function promptChips(p: Pick<PullRequest, 'column' | 'checks'>): { label: string; text: string }[] {
  const review = {
    label: 'Review',
    text: 'Review this pull request: read the description and the diff, run what is worth running, and list what should change before it merges.',
  }
  const ci = { label: 'Fix CI', text: 'Find out why CI fails on this pull request and fix it.' }
  const comments = { label: 'Address review', text: 'Go through the open review comments on this pull request and address them.' }
  const resume = { label: 'Continue', text: 'Pick up the work on this pull request: see where it stands and what is left.' }
  const chips = p.column === 'waitingOnMe' || p.column === 'waitingOnAuthor' ? [review, ci] : [resume, comments, ci]
  return checksTone(p.checks) === 'bad' ? [ci, ...chips.filter((c) => c !== ci)] : chips
}

/** `#/prs/<owner>/<repo>/<n>`: the PR screen with that PR's dialog open. */
export const prHref = (p: Pick<PullRequest, 'owner' | 'repo' | 'number'>) => `/prs/${encodeURIComponent(p.owner)}/${encodeURIComponent(p.repo)}/${p.number}`

/** The PR a `#/prs/…` location names; null for plain `/prs` (or anything else). */
export function parsePrLocation(location: string): { owner: string; repo: string; number: number } | null {
  const m = /^\/prs\/([^/]+)\/([^/]+)\/([1-9][0-9]*)$/.exec(location)
  if (!m) return null
  try {
    return { owner: decodeURIComponent(m[1]), repo: decodeURIComponent(m[2]), number: Number(m[3]) }
  } catch {
    return null
  }
}

export const isPrsLocation = (location: string) => location === '/prs' || location.startsWith('/prs/')

export interface PrLinks {
  /** `host/sessionId` → the PR whose worktree the session runs in. */
  bySession: Map<string, PullRequest>
  /** PR url → its sessions. */
  byPr: Map<string, Session[]>
}

const under = (cwd: string, dir: string) => cwd === dir || cwd.startsWith(dir.endsWith('/') ? dir : `${dir}/`)

/**
 * Sessions ↔ PRs, on any host: a session in a linked worktree of the PR's repo, on its head
 * branch (`pr-<n>` for a fork's PR, as `fleet pr checkout` names it), works on that PR. A
 * session from an older CLI (no `git`) falls back to the PR host's worktree paths: on `prHost`,
 * its cwd is a PR's worktree or inside it (the deepest one wins). A repo's main checkout never
 * links: it is on some PR's branch now and then, while its sessions do anything.
 */
export function linkSessions(prs: PullRequest[], prHost: string | null, sessions: Session[]): PrLinks {
  const bySession = new Map<string, PullRequest>()
  const byPr = new Map<string, Session[]>()
  if (!prHost) return { bySession, byPr }
  const byBranch = new Map<string, PullRequest>()
  for (const p of prs) {
    const repo = `${p.owner}/${p.repo}`.toLowerCase()
    for (const b of [p.headBranch, `pr-${p.number}`]) if (!byBranch.has(`${repo} ${b}`)) byBranch.set(`${repo} ${b}`, p)
  }
  const withWt = prs.filter((p) => p.local?.worktree && p.local.worktree !== p.local.repoPath).sort((a, b) => b.local!.worktree!.length - a.local!.worktree!.length)
  for (const s of sessions) {
    if (!s.cwd || !s.session_id) continue
    const cwd = s.cwd
    const g = s.git
    const pr =
      g !== undefined
        ? g?.linked && g.repo && g.branch
          ? byBranch.get(`${g.repo.toLowerCase()} ${g.branch}`)
          : undefined
        : s.host === prHost
          ? withWt.find((p) => under(cwd, p.local!.worktree!))
          : undefined
    if (!pr) continue
    bySession.set(`${s.host}/${s.session_id}`, pr)
    byPr.set(pr.url, [...(byPr.get(pr.url) ?? []), s])
  }
  return { bySession, byPr }
}
