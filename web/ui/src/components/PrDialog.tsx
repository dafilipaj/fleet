import { useState } from 'react'
import { CodeXmlIcon, ExternalLinkIcon, FolderGit2Icon, GitBranchIcon, Loader2Icon, PlayIcon } from 'lucide-react'
import { toast } from 'sonner'
import { useLocation } from 'wouter'

import { api } from '@/api/client'
import type { PullRequest, Session } from '@/api/types'
import { watchForSpawned } from '@/api/spawnWatch'
import { HostDot } from '@/components/HostBadge'
import { PrSessionLink } from '@/components/PrChip'
import { PrMention } from '@/components/PrMention'
import { PrTaskChips } from '@/components/TaskChip'
import { usePrTasks } from '@/hooks/useTasks'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from '@/components/ui/dialog'
import { Kbd } from '@/components/ui/kbd'
import { Label } from '@/components/ui/label'
import { Textarea } from '@/components/ui/textarea'
import { useFleet } from '@/hooks/useFleet'
import { editorLabel } from '@/lib/brief'
import { copyText } from '@/lib/clipboard'
import { shortCwd } from '@/lib/format'
import { CHECKS_LABEL, checksTone, prRef, promptChips } from '@/lib/prs'
import { sessionHref } from '@/lib/sessions'
import { isMacPlatform, isSubmitChord } from '@/lib/shortcuts'

/**
 * One PR's actions on `host`: start a session on it (checked out in a worktree first, the
 * session told what the PR is and where), just check it out, or open it on GitHub. Key it by
 * the PR so a new PR starts with a fresh form.
 */
export function PrDialog({
  pr,
  host,
  sessions = [],
  onClose,
  onChanged,
}: {
  pr: PullRequest | null
  host: string | null
  /** Sessions already working in its worktree. */
  sessions?: Session[]
  onClose: () => void
  onChanged: () => void
}) {
  const [, navigate] = useLocation()
  const { applyFleet } = useFleet()
  const [prompt, setPrompt] = useState('')
  const [busy, setBusy] = useState<null | 'spawn' | 'checkout'>(null)
  const [error, setError] = useState<string | null>(null)
  const tasks = usePrTasks(pr ?? { url: '' })
  if (!pr || !host) return null
  const ref = prRef(pr)
  const worktree = pr.local?.worktree ?? null
  const editorUrl = worktree ? (pr.local?.editorUrl ?? null) : null
  const editorText = editorLabel(null, editorUrl)
  const tone = checksTone(pr.checks)

  const checkout = async () => {
    setBusy('checkout')
    setError(null)
    try {
      const { worktree: wt } = await api.prCheckout(host, ref)
      const open = editorLabel(null, wt.editorUrl)
      toast.success(wt.created ? `Checked out ${ref}` : `${ref} was already checked out`, {
        description: shortCwd(wt.path),
        action:
          open && wt.editorUrl
            ? { label: open.replace(/^Open in /, ''), onClick: () => window.location.assign(wt.editorUrl!) }
            : { label: 'Copy path', onClick: () => void copyText(wt.path) },
      })
      onChanged()
      setBusy(null)
    } catch (err) {
      setError((err as Error)?.message || 'Checkout failed')
      setBusy(null)
    }
  }

  const spawn = async () => {
    if (busy) return
    setBusy('spawn')
    setError(null)
    try {
      const res = await api.prSpawn(host, {
        pr: ref,
        prompt: prompt.trim() || undefined,
      })
      const name = res.spawn?.name || res.spawn?.tmuxSession || ref
      onClose()
      onChanged()
      const id = toast.loading(`Starting ${name} on ${host}…`, {
        description: shortCwd(res.worktree.path),
      })
      watchForSpawned(
        {
          host: res.spawn?.host || host,
          name,
          tmuxSession: res.spawn?.tmuxSession ?? name,
        },
        {
          onFleet: applyFleet,
          onFound: (s) => {
            toast.success(`${name} is up`, { id, description: undefined })
            navigate(sessionHref(s))
          },
          onTimeout: () =>
            toast.error(`${name} has not shown up yet`, {
              id,
              description: 'Check the list in a moment',
            }),
        },
      )
    } catch (err) {
      setError((err as Error)?.message || 'Failed to start')
      setBusy(null)
    }
  }

  return (
    <Dialog open onOpenChange={(o) => (o ? null : onClose())}>
      <DialogContent className="max-h-[90dvh] overflow-y-auto sm:max-w-lg">
        <DialogHeader>
          <DialogDescription className="flex items-center gap-1.5 font-mono text-xs">
            {pr.owner}/{pr.repo}#{pr.number}
            {pr.draft ? <span className="rounded border px-1 font-sans text-[0.625rem]">draft</span> : null}
          </DialogDescription>
          <DialogTitle className="text-base leading-snug">{pr.title}</DialogTitle>
        </DialogHeader>

        <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 text-sm">
          <dt className="text-muted-foreground">Branch</dt>
          <dd className="min-w-0 truncate font-mono text-xs leading-5" title={`${pr.headBranch} → ${pr.baseBranch}`}>
            <GitBranchIcon className="mr-1 inline size-3.5 align-[-2px]" />
            {pr.headBranch} <span className="text-muted-foreground">→ {pr.baseBranch}</span>
          </dd>
          <dt className="text-muted-foreground">By</dt>
          <dd>
            @{pr.author} · +{pr.additions} −{pr.deletions} in {pr.changedFiles} files
          </dd>
          {tasks.length ? (
            <>
              <dt className="text-muted-foreground">Tasks</dt>
              <dd className="flex flex-wrap items-center gap-1.5" onClickCapture={onClose}>
                <PrTaskChips pr={pr} />
                <span className="min-w-0 truncate text-xs text-muted-foreground">{tasks.map((t) => t.title).join(' · ')}</span>
              </dd>
            </>
          ) : null}
          <dt className="text-muted-foreground">Checks</dt>
          <dd>{CHECKS_LABEL[tone]}</dd>
          <dt className="text-muted-foreground">Worktree</dt>
          <dd className="flex min-w-0 items-center gap-1.5">
            <HostDot host={host} />
            {worktree ? (
              <>
                <button
                  type="button"
                  className="min-w-0 truncate text-left font-mono text-xs hover:underline"
                  title="Copy path"
                  onClick={() => void copyText(worktree)}
                >
                  {shortCwd(worktree)}
                </button>
                {/* A desktop editor link makes no sense on a phone: hidden on touch-first screens. */}
                {editorText && editorUrl ? (
                  <Button asChild variant="outline" size="xs" className="-my-1 ml-auto pointer-coarse:hidden">
                    <a href={editorUrl} title={editorText}>
                      <CodeXmlIcon /> {editorText.replace(/^Open in /, '')}
                    </a>
                  </Button>
                ) : null}
              </>
            ) : pr.local ? (
              <span className="text-muted-foreground">not checked out on {host}</span>
            ) : (
              <span className="text-muted-foreground">
                no clone of {pr.repo} on {host}
              </span>
            )}
          </dd>
        </dl>

        <PrMention pr={pr} className="rounded-lg border p-2.5" />

        {sessions.length ? (
          <div className="space-y-1">
            <Label>Sessions on it</Label>
            <div className="flex flex-col gap-0.5" onClickCapture={onClose}>
              {sessions.map((s) => (
                <PrSessionLink key={`${s.host}/${s.session_id}`} session={s} className="py-0.5 text-sm" />
              ))}
            </div>
          </div>
        ) : null}

        <div className="space-y-2">
          <Label htmlFor="pr-prompt">{sessions.length ? 'Start another session' : 'Start a session on it'}</Label>
          <div className="flex flex-wrap gap-1.5">
            {promptChips(pr).map((c) => (
              <button
                key={c.label}
                type="button"
                onClick={() => setPrompt(c.text)}
                className="rounded-full border px-2.5 py-0.5 text-xs text-muted-foreground hover:bg-muted/60 hover:text-foreground"
              >
                {c.label}
              </button>
            ))}
          </div>
          <Textarea
            id="pr-prompt"
            value={prompt}
            onChange={(e) => setPrompt(e.target.value)}
            onKeyDown={(e) => {
              if (isSubmitChord(e)) {
                e.preventDefault()
                void spawn()
              }
            }}
            rows={3}
            placeholder="What should it do? (optional)"
          />
          <p className="text-xs text-dimmer">
            {worktree ? 'Runs in the worktree above' : `Checks ${pr.headBranch} out in a worktree on ${host} first`}; the session is told the PR and the
            worktree path.
          </p>
        </div>

        {error ? (
          <Alert variant="destructive">
            <AlertDescription>{error}</AlertDescription>
          </Alert>
        ) : null}

        <div className="flex flex-wrap items-center gap-2">
          <Button asChild variant="ghost" size="sm" className="text-muted-foreground">
            <a href={pr.url} target="_blank" rel="noreferrer">
              <ExternalLinkIcon /> GitHub
            </a>
          </Button>
          {!worktree && pr.local ? (
            <Button variant="outline" size="sm" onClick={() => void checkout()} disabled={!!busy}>
              {busy === 'checkout' ? <Loader2Icon className="animate-spin" /> : <FolderGit2Icon />} Check out
            </Button>
          ) : null}
          <Button size="sm" className="ml-auto" onClick={() => void spawn()} disabled={!!busy || !pr.local}>
            {busy === 'spawn' ? <Loader2Icon className="animate-spin" /> : <PlayIcon />} Start session
            <Kbd className="ml-1 hidden bg-primary-foreground/20 text-primary-foreground sm:inline-flex">{isMacPlatform() ? '⌘↵' : 'Ctrl↵'}</Kbd>
          </Button>
        </div>
      </DialogContent>
    </Dialog>
  )
}
