import { ChevronDownIcon, ChevronRightIcon, ExternalLinkIcon, FolderGitIcon, GitBranchIcon, GitMergeIcon, GitPullRequestClosedIcon, GitPullRequestDraftIcon, GitPullRequestIcon, LoaderIcon, type LucideIcon } from 'lucide-react'
import { useState } from 'react'

import type { DiffRepo, ReviewPr } from '@/api/types'
import { DiffStat } from '@/components/review/FileDiff'
import { entryAnchor } from '@/lib/review'
import { Button } from '@/components/ui/button'
import { cn } from '@/lib/utils'

/** A PR's state as GitHub shows it: icon, colour, label. */
const PR_STATE: Record<NonNullable<ReviewPr['state']>, { icon: LucideIcon; className: string; label: string }> = {
  draft: { icon: GitPullRequestDraftIcon, className: 'text-muted-foreground', label: 'Draft' },
  open: { icon: GitPullRequestIcon, className: 'text-status-busy', label: 'Open' },
  merged: { icon: GitMergeIcon, className: 'text-violet-500', label: 'Merged' },
  closed: { icon: GitPullRequestClosedIcon, className: 'text-status-error', label: 'Closed' },
}

export function PrStateIcon({ pr, className }: { pr: ReviewPr; className?: string }) {
  const s = pr.state ? PR_STATE[pr.state] : null
  const Icon = s?.icon ?? GitPullRequestIcon
  return <Icon role="img" aria-label={s?.label ?? 'Pull request'} className={cn('shrink-0', s?.className ?? 'text-muted-foreground', className)} />
}


/** What a checkout has beyond its PR / upstream, as short badges. */
function LocalBadges({ repo }: { repo: DiffRepo }) {
  const l = repo.local
  const out: { text: string; title: string }[] = []
  if (repo.source === 'pr') out.push({ text: 'no worktree', title: 'Diffed from the PR on GitHub (a local clone): nothing of it is checked out here' })
  if (l?.uncommitted) out.push({ text: `${l.uncommitted} uncommitted`, title: `${l.uncommitted} file${l.uncommitted === 1 ? '' : 's'} changed in the worktree and not committed: not in the PR yet` })
  if (l?.unpushed) out.push({ text: `${l.unpushed} unpushed`, title: `${l.unpushed} commit${l.unpushed === 1 ? '' : 's'} not pushed: not in the PR yet` })
  return (
    <>
      {out.map((b) => (
        <span key={b.text} title={b.title} className="shrink-0 rounded-md border px-1.5 text-[0.6875rem] leading-5 text-muted-foreground">
          {b.text}
        </span>
      ))}
    </>
  )
}

/** One entry's heading in the diff list: its PR (title, number, state) or branch, the diff size, what is local only. */
export function EntryHeader({ repo, entryKey, onSetUp }: { repo: DiffRepo; entryKey: string; onSetUp?: () => Promise<boolean> }) {
  const pr = repo.pr
  const [busy, setBusy] = useState(false)
  return (
    <div id={entryAnchor(entryKey)} className="scroll-mt-2 rounded-lg border bg-card px-3 py-2" title={repo.root || undefined}>
      <div className="flex min-w-0 items-center gap-2">
        {pr ? <PrStateIcon pr={pr} className="size-4" /> : <GitBranchIcon className="size-4 shrink-0 text-muted-foreground" />}
        {pr ? (
          <a href={pr.url} target="_blank" rel="noreferrer" className="flex min-w-0 items-center gap-1.5 text-sm font-semibold hover:underline" title={`Open ${pr.owner}/${pr.repo}#${pr.number} on GitHub`}>
            <span className="truncate">{pr.title || `Pull request #${pr.number}`}</span>
            <span className="shrink-0 font-normal text-muted-foreground">#{pr.number}</span>
            <ExternalLinkIcon className="size-3 shrink-0 text-muted-foreground" />
          </a>
        ) : (
          <span className="truncate font-mono text-sm font-semibold">{repo.branch ?? 'detached'}</span>
        )}
        <span className="flex-1" />
        {repo.stats ? <DiffStat additions={repo.stats.additions} deletions={repo.stats.deletions} bar={false} /> : null}
      </div>
      <div className="mt-1 flex min-w-0 flex-wrap items-center gap-1.5 text-xs text-muted-foreground">
        {pr ? (
          <span className="flex min-w-0 items-center gap-1">
            <GitBranchIcon className="size-3 shrink-0" />
            <span className="truncate font-mono">{repo.branch ?? pr.headBranch}</span>
          </span>
        ) : null}
        {repo.base?.ref || pr?.baseBranch ? <span className="truncate font-mono text-dimmer">← {pr?.baseBranch ?? repo.base?.ref}</span> : null}
        {repo.source !== 'pr' && repo.label ? (
          <span className="flex min-w-0 items-center gap-1 text-dimmer">
            <FolderGitIcon className="size-3 shrink-0" />
            <span className="truncate">{repo.label}</span>
          </span>
        ) : null}
        <LocalBadges repo={repo} />
        {onSetUp ? (
          <Button
            variant="outline"
            size="xs"
            className="ml-auto"
            disabled={busy}
            title="Check the PR out in a worktree here: its review then follows the worktree live"
            onClick={async () => {
              setBusy(true)
              await onSetUp()
              setBusy(false)
            }}
          >
            {busy ? <LoaderIcon className="animate-spin" /> : <FolderGitIcon />}
            Set up worktree
          </Button>
        ) : null}
      </div>
      {repo.error ? <p className="mt-1.5 text-xs text-destructive">{repo.error}</p> : null}
      {repo.pending ? <p className="mt-1.5 animate-pulse text-xs text-muted-foreground">Fetching the PR from GitHub…</p> : null}
    </div>
  )
}

/** An entry in the file-tree column: compact PR / branch line, click → its heading. */
export function EntryRow({ repo, onOpen, collapsed, onToggle }: { repo: DiffRepo; onOpen: () => void; collapsed?: boolean; onToggle?: () => void }) {
  const pr = repo.pr
  const row = (
    <button
      type="button"
      onClick={onOpen}
      title={pr ? `${pr.owner}/${pr.repo}#${pr.number}: ${pr.title}\n${repo.branch ?? pr.headBranch ?? ''}` : (repo.root ?? undefined)}
      className="flex w-full min-w-0 items-center gap-1.5 rounded-md px-2 py-1 text-left text-xs hover:bg-muted"
    >
      {pr ? <PrStateIcon pr={pr} className="size-3.5" /> : <GitBranchIcon className="size-3.5 shrink-0 text-muted-foreground" />}
      {pr ? (
        <>
          <span className="shrink-0 text-muted-foreground">#{pr.number}</span>
          <span className="truncate font-medium">{pr.title || repo.branch}</span>
        </>
      ) : (
        <span className="truncate font-mono font-medium">{repo.branch ?? repo.label}</span>
      )}
      {repo.source === 'pr' ? <span className="ml-auto shrink-0 text-[0.625rem] text-dimmer">no worktree</span> : repo.local?.uncommitted ? <span className="ml-auto shrink-0 text-[0.625rem] text-status-waiting" title="uncommitted files">●{repo.local.uncommitted}</span> : null}
    </button>
  )
  if (!onToggle) return row
  // The chevron folds the entry's files in the tree; the row itself still jumps to the entry.
  return (
    <div className="flex min-w-0 items-center">
      <button
        type="button"
        aria-expanded={!collapsed}
        aria-label={collapsed ? 'Show its files' : 'Hide its files'}
        title={collapsed ? 'Show its files' : 'Hide its files'}
        onClick={onToggle}
        className="flex size-6 shrink-0 items-center justify-center rounded-md text-dimmer hover:bg-muted hover:text-foreground"
      >
        {collapsed ? <ChevronRightIcon className="size-3.5" /> : <ChevronDownIcon className="size-3.5" />}
      </button>
      <div className="min-w-0 flex-1">{row}</div>
    </div>
  )
}
