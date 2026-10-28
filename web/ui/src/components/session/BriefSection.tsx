import { useEffect, useRef, useState, type ReactNode } from 'react'
import {
  ChevronRightIcon,
  CircleAlertIcon,
  CircleDotIcon,
  FileIcon,
  FlagIcon,
  FolderGit2Icon,
  GitBranchIcon,
  GitPullRequestIcon,
  LinkIcon,
  Loader2Icon,
  PencilIcon,
  RefreshCwIcon,
  ScrollTextIcon,
  SparklesIcon,
  SquareCheckBigIcon,
  SquareIcon,
  StickyNoteIcon,
} from 'lucide-react'

import type { BriefResource, BriefWorktree } from '@/api/types'
import { Markdown } from '@/components/Markdown'
import { Button } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import type { BriefState } from '@/hooks/useBrief'
import { FileLinksContext, type FileStats } from '@/hooks/useFileLinks'
import { useNow } from '@/hooks/useNow'
import { usePersistentState } from '@/hooks/usePersistentState'
import { briefBody, briefTime, briefTodos, gitLine, groupResources, todoProgress, type ResourceGroup } from '@/lib/brief'
import { relTime } from '@/lib/format'
import { pathCandidates } from '@/lib/paths'
import { cn } from '@/lib/utils'

const KIND_ICON: Record<string, typeof FileIcon> = {
  Origin: FlagIcon,
  PR: GitPullRequestIcon,
  Issue: CircleDotIcon,
  Artifact: SparklesIcon,
  Spec: ScrollTextIcon,
  File: FileIcon,
  Git: GitBranchIcon,
  Branch: GitBranchIcon,
  Worktree: FolderGit2Icon,
  Link: LinkIcon,
}
/** Resource groups shown as their own parts between the Summary and the Todos. */
const TOP_KINDS = new Set(['Origin', 'PR'])
/** Kinds whose `path` is a file the preview can show. */
const FILE_KINDS = new Set(['File', 'Spec'])
/** The Files group is collapsed until the viewer opens it (remembered per viewer). */
const FILES_OPEN_KEY = 'fleet.briefFilesOpen'

export interface BriefSectionProps {
  host: string
  brief: BriefState
  /** Chat paths on the session's host: file resources open the in-app preview through it. */
  fileLinks: FileStats
  /** Desktop wording (click, ⌘/Ctrl+Enter hints). */
  desktop?: boolean
  /** Filled with "cancel the edit" while editing (the drawer's Esc cancels it instead of closing). */
  cancelEditRef?: React.RefObject<(() => void) | null>
}

/**
 * The session brief, the top of the Details panel: a toolbar (updated / edited, Regenerate,
 * Edit), then Summary, Todos (clickable checkboxes, "3/7"), Resources grouped by kind (files
 * open the in-app preview, URLs a new tab; Files collapsed by default). Edit = the body markdown in a textarea (⌘/Ctrl+Enter saves, Esc cancels).
 */
export function BriefSection({ host, brief: st, fileLinks, desktop = false, cancelEditRef }: BriefSectionProps) {
  const now = useNow(15_000)
  const b = st.brief
  const [draft, setDraft] = useState<string | null>(null)
  const editing = draft !== null
  const textRef = useRef<HTMLTextAreaElement>(null)

  useEffect(() => {
    if (!cancelEditRef) return
    cancelEditRef.current = editing ? () => setDraft(null) : null
    return () => {
      cancelEditRef.current = null
    }
  }, [cancelEditRef, editing])
  useEffect(() => {
    if (editing) textRef.current?.focus()
  }, [editing])

  // Which file resources / paths in the summary, todos and notes exist on the host (same cache as the chat's links).
  const request = fileLinks.request
  const parsed = b?.parsed
  const summary = parsed?.summary ?? ''
  useEffect(() => {
    const paths: string[] = []
    for (const r of parsed?.resources ?? []) {
      if (r.path && FILE_KINDS.has(String(r.kind))) paths.push(r.path)
      else if (r.kind == null && r.text) paths.push(...pathCandidates(r.text))
    }
    if (parsed?.summary) paths.push(...pathCandidates(parsed.summary))
    for (const t of briefTodos(parsed)) paths.push(...pathCandidates(t.text))
    if (paths.length) request(paths)
  }, [parsed, request])

  const startEdit = () => b && setDraft(briefBody(b.markdown))
  const saveEdit = async () => {
    if (draft == null || st.saving) return
    if (await st.save(draft)) setDraft(null)
  }

  const updated = briefTime(b?.updated)
  const edited = briefTime(b?.editedAt)
  const busy = st.generating || st.regenerating
  const todos = briefTodos(b?.parsed)
  const progress = todoProgress(todos)
  const worktrees = b?.worktrees ?? []
  // The Worktrees list includes the cwd's checkout: the Git line would say it twice.
  const groups = groupResources(parsed?.resources).filter((g) => !(worktrees.length && g.kind === 'Git'))
  // Where the work came from and what it shipped sit under the Summary; the rest goes below the Todos.
  const topGroups = groups.filter((g) => g.kind != null && TOP_KINDS.has(g.kind))
  const restGroups = groups.filter((g) => !(g.kind != null && TOP_KINDS.has(g.kind)))
  const empty = !!b && !b.exists && !summary && !todos.length && !groups.length && !worktrees.length

  const regenerateButton = (label: string, className?: string) => (
    <Button variant="outline" size="lg" className={className} disabled={busy || !b} onClick={() => void st.regenerate()}>
      {busy ? <Loader2Icon className="animate-spin" /> : <SparklesIcon />}
      {busy ? 'Generating…' : label}
    </Button>
  )

  let body: ReactNode
  if (editing) {
    body = (
      <div className="flex flex-col gap-2 pb-2">
        <textarea
          ref={textRef}
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
              e.preventDefault()
              void saveEdit()
            } else if (e.key === 'Escape') {
              e.preventDefault()
              setDraft(null)
            }
          }}
          aria-label="Brief markdown"
          data-vaul-no-drag
          spellCheck={false}
          className={cn(
            'w-full resize-y rounded-lg border border-input bg-card px-3 py-2 font-mono text-[0.8125rem] leading-relaxed outline-none',
            'focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50',
            desktop ? 'h-[60vh] min-h-48' : 'h-[50vh]',
          )}
        />
        <div className="flex items-center gap-2">
          <span className="min-w-0 flex-1 truncate text-xs text-dimmer">{desktop ? '⌘/Ctrl+Enter saves · Esc cancels' : 'Sections: Summary, Resources, Todos'}</span>
          <Button variant="ghost" size="lg" onClick={() => setDraft(null)} disabled={st.saving}>
            Cancel
          </Button>
          <Button size="lg" onClick={() => void saveEdit()} disabled={st.saving}>
            {st.saving ? <Loader2Icon className="animate-spin" /> : null}
            Save
          </Button>
        </div>
      </div>
    )
  } else if (!b) {
    body = st.error ? (
      <p className="flex items-start gap-2 rounded-lg border border-destructive/40 bg-destructive/10 px-3 py-2 text-xs text-destructive">
        <CircleAlertIcon className="mt-px size-3.5 shrink-0" />
        <span className="break-words">{st.error}</span>
      </p>
    ) : (
      <div className="space-y-2 pt-1 pb-2" aria-hidden>
        <Skeleton className="h-4 w-full" />
        <Skeleton className="h-4 w-4/5" />
        <Skeleton className="mt-5 h-4 w-2/3" />
        <Skeleton className="h-4 w-1/2" />
      </div>
    )
  } else if (empty) {
    body = (
      <div className="space-y-3 pb-2 text-sm text-muted-foreground">
        <p>
          No brief yet. A brief is a short summary of what this session is doing, its todos and everything it produced (PRs,
          files, links) — enough to pick the work up in a new session.
        </p>
        {!b.enabled ? (
          <p className="text-xs text-dimmer">
            Automatic updates are off on {host} (<code className="font-mono">web.briefs.enabled</code>); generating one by hand works.
          </p>
        ) : null}
        {regenerateButton('Generate brief', 'w-full')}
      </div>
    )
  } else {
    body = (
      <FileLinksContext.Provider value={fileLinks.api}>
        <div className="space-y-5 pb-2">
          <Part title="Summary">
            {summary ? (
              <Markdown text={summary} className="text-sm leading-relaxed" />
            ) : (
              <div className="space-y-2">
                <p className="text-sm text-dimmer">
                  No summary yet.
                  {!b.enabled ? ' Automatic updates are off on this host (web.briefs.enabled); generating one by hand works.' : ''}
                </p>
                {regenerateButton('Generate summary')}
              </div>
            )}
          </Part>

          {topGroups.map((g) => (
            <Part key={g.kind} title={g.label} aside={g.items.length > 1 ? <span className="tabular-nums">{g.items.length}</span> : null}>
              <ul className="space-y-1">
                {g.items.map((r, i) => (
                  <ResourceRow key={`${r.text}-${i}`} r={r} fileLinks={fileLinks} />
                ))}
              </ul>
            </Part>
          ))}

          {todos.length ? (
            <Part title="Todos" aside={progress ? <span className="tabular-nums">{`${progress.done}/${progress.total}`}</span> : null}>
              {progress ? (
                <div className="mb-2 h-1 overflow-hidden rounded-full bg-muted" aria-hidden>
                  <div className="h-full rounded-full bg-primary/70 transition-[width]" style={{ width: `${(progress.done / progress.total) * 100}%` }} />
                </div>
              ) : null}
              <ul className="space-y-0.5">
                {todos.map((item, i) => (
                  <li key={i} className="flex items-start gap-1">
                    <button
                      type="button"
                      role="checkbox"
                      aria-checked={item.done}
                      aria-label={item.done ? `Mark not done: ${item.text}` : `Mark done: ${item.text}`}
                      onClick={() => void st.toggleTodo(i)}
                      className="-ml-1.5 flex size-8 shrink-0 items-center justify-center rounded-md text-muted-foreground outline-none hover:bg-muted focus-visible:ring-3 focus-visible:ring-ring/50 active:bg-muted"
                    >
                      {item.done ? <SquareCheckBigIcon className="size-[1.125rem] text-primary" /> : <SquareIcon className="size-[1.125rem]" />}
                    </button>
                    <Markdown text={item.text} className={cn('min-w-0 flex-1 pt-1.5 text-sm', item.done && 'text-muted-foreground')} />
                  </li>
                ))}
              </ul>
            </Part>
          ) : null}

          {restGroups.length || worktrees.length ? (
            <Part title="Resources">
              <div className="space-y-3">
                {worktrees.length ? <WorktreesGroup worktrees={worktrees} editor={b.editor} /> : null}
                {restGroups.map((g) => (g.kind === 'File' ? <FilesGroup key="files" g={g} fileLinks={fileLinks} /> : <Group key={g.kind ?? 'notes'} g={g} fileLinks={fileLinks} />))}
              </div>
            </Part>
          ) : null}
        </div>
      </FileLinksContext.Provider>
    )
  }

  return (
    <section aria-label="Brief" className="py-1">
      <div className="flex items-center gap-1 pt-1 pb-1.5">
        <h3 className="text-[0.6875rem] font-semibold tracking-wider text-dimmer uppercase">Brief</h3>
        <span className="min-w-0 flex-1 truncate pl-1 text-xs text-dimmer tabular-nums">
          {busy
            ? 'updating…'
            : !b
              ? st.error
                ? 'could not load'
                : 'loading…'
              : updated
                ? `updated ${relTime(updated, now)} ago${edited ? ' · edited' : ''}`
                : 'not generated yet'}
        </span>
        {!editing ? (
          <>
            <Button
              variant="ghost"
              size="icon"
              aria-label="Regenerate brief"
              title="Regenerate (asks the model)"
              disabled={busy || !b}
              onClick={() => void st.regenerate()}
              className="size-9 shrink-0 rounded-lg"
            >
              {busy ? <Loader2Icon className="animate-spin" /> : <RefreshCwIcon />}
            </Button>
            <Button variant="ghost" size="icon" aria-label="Edit brief" title="Edit" disabled={!b} onClick={startEdit} className="size-9 shrink-0 rounded-lg">
              <PencilIcon />
            </Button>
          </>
        ) : null}
      </div>
      {body}
    </section>
  )
}

function Part({ title, aside, children }: { title: string; aside?: ReactNode; children: ReactNode }) {
  return (
    <section>
      <h4 className="flex items-baseline justify-between gap-2 pb-1.5 text-xs font-semibold text-muted-foreground">
        {title}
        {aside ? <span className="text-xs font-medium text-dimmer">{aside}</span> : null}
      </h4>
      {children}
    </section>
  )
}

function GroupHeading({ g }: { g: ResourceGroup }) {
  return (
    <>
      {g.label}
      {g.items.length > 1 ? <span className="pl-1 text-dimmer tabular-nums">{g.items.length}</span> : null}
    </>
  )
}

function Group({ g, fileLinks }: { g: ResourceGroup; fileLinks: FileStats }) {
  return (
    <div>
      <h5 className="pb-1 text-xs font-medium text-muted-foreground">
        <GroupHeading g={g} />
      </h5>
      <ul className="space-y-1">
        {g.items.map((r, i) => (
          <ResourceRow key={`${r.text}-${i}`} r={r} fileLinks={fileLinks} />
        ))}
      </ul>
    </div>
  )
}

/** Files: often long, so collapsed by default; the open state is remembered per viewer. */
function FilesGroup({ g, fileLinks }: { g: ResourceGroup; fileLinks: FileStats }) {
  const [openRaw, setOpenRaw] = usePersistentState<string>(FILES_OPEN_KEY, '0', (r) => (r === '1' || r === '0' ? r : undefined))
  const open = openRaw === '1'
  return (
    <div>
      <h5>
        <button
          type="button"
          aria-expanded={open}
          onClick={() => setOpenRaw(open ? '0' : '1')}
          className="-ml-1 flex min-h-8 items-center gap-1 rounded-md pr-2 pl-1 text-xs font-medium text-muted-foreground outline-none hover:text-foreground focus-visible:ring-3 focus-visible:ring-ring/50"
        >
          <ChevronRightIcon className={cn('size-3.5 transition-transform', open && 'rotate-90')} />
          {g.label}
          <span className="text-dimmer tabular-nums">{g.items.length}</span>
        </button>
      </h5>
      {open ? (
        <ul className="space-y-1">
          {g.items.map((r, i) => (
            <ResourceRow key={`${r.text}-${i}`} r={r} fileLinks={fileLinks} />
          ))}
        </ul>
      ) : null}
    </div>
  )
}

/** The checkouts the session works in, each with an "open in editor" button (hidden on touch-first screens). */
function WorktreesGroup({ worktrees, editor }: { worktrees: BriefWorktree[]; editor: string | null | undefined }) {
  const label = editor === 'cursor' ? 'Cursor' : 'VSC'
  return (
    <div>
      <h5 className="pb-1 text-xs font-medium text-muted-foreground">
        Worktrees
        {worktrees.length > 1 ? <span className="pl-1 text-dimmer tabular-nums">{worktrees.length}</span> : null}
      </h5>
      <ul className="space-y-1">
        {worktrees.map((w) => (
          <li key={w.path} className={cn(rowClass, 'items-center')}>
            <FolderGit2Icon className="size-4 shrink-0 text-dimmer" />
            <span className="min-w-0 flex-1 font-mono text-[0.8125rem] [overflow-wrap:anywhere]" title={w.path}>
              <span className={cn('select-all', w.branch ? 'text-foreground' : 'text-dimmer')}>{w.branch ?? 'detached'}</span>
              <span className="text-muted-foreground"> · {w.linked ? 'worktree' : 'repo'} </span>
              <span className="text-muted-foreground select-all">{w.display}</span>
            </span>
            {w.editorUrl ? (
              <Button asChild variant="outline" size="xs" className="pointer-coarse:hidden">
                <a href={w.editorUrl} title={`Open ${w.display} in ${editor === 'cursor' ? 'Cursor' : 'VS Code'}`}>
                  {label}
                </a>
              </Button>
            ) : null}
          </li>
        ))}
      </ul>
    </div>
  )
}

const rowClass = 'flex min-w-0 items-start gap-2 rounded-md py-1 text-sm'
const linkClass = 'min-w-0 break-words text-primary underline decoration-primary/40 underline-offset-2 [overflow-wrap:anywhere] active:opacity-70'

function ResourceRow({ r, fileLinks }: { r: BriefResource; fileLinks: FileStats }) {
  const Icon = (r.kind && KIND_ICON[r.kind]) || StickyNoteIcon
  const icon = <Icon className="mt-0.5 size-4 shrink-0 text-dimmer" />
  if (r.url) {
    return (
      <li className={rowClass}>
        {icon}
        <a href={r.url} target="_blank" rel="noopener noreferrer" className={linkClass} title={r.url}>
          {r.label || r.url}
        </a>
      </li>
    )
  }
  if (r.kind == null) {
    return (
      <li className={rowClass}>
        {icon}
        <Markdown text={r.text} className="min-w-0 flex-1 text-sm" />
      </li>
    )
  }
  if (r.kind === 'Git') {
    const { branch, where } = gitLine(r)
    return (
      <li className={rowClass}>
        {icon}
        <span className="min-w-0 font-mono text-[0.8125rem] [overflow-wrap:anywhere]">
          <span className={cn('select-all', branch ? 'text-foreground' : 'text-dimmer')}>{branch ?? 'detached'}</span>
          {where ? <span className="text-muted-foreground"> · {where}</span> : null}
        </span>
      </li>
    )
  }
  const path = r.path ?? r.label ?? r.text
  const previewable = FILE_KINDS.has(r.kind) && !!r.path && fileLinks.api.isLink(r.path, 'code')
  return (
    <li className={rowClass}>
      {icon}
      {previewable ? (
        <button
          type="button"
          onClick={() => fileLinks.api.open(r.path as string, 'code')}
          title={`Preview ${path}`}
          className="min-w-0 cursor-pointer text-left font-mono text-[0.8125rem] break-all text-foreground underline decoration-primary/35 decoration-dotted underline-offset-[3px] outline-none hover:decoration-primary hover:decoration-solid focus-visible:rounded-sm focus-visible:ring-2 focus-visible:ring-ring"
        >
          {path}
        </button>
      ) : (
        <span className="min-w-0 font-mono text-[0.8125rem] break-all text-muted-foreground select-all">{path}</span>
      )}
    </li>
  )
}
