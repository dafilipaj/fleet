import { ChevronLeftIcon, Columns2Icon, FoldVerticalIcon, FolderGitIcon, GitBranchIcon, ListTreeIcon, MessageSquarePlusIcon, MessageSquareTextIcon, RowsIcon, UnfoldVerticalIcon } from 'lucide-react'
import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { useLocation } from 'wouter'

import { HostBadge } from '@/components/HostBadge'
import { DiffStat, FileDiff, type DiffView } from '@/components/review/FileDiff'
import { FileTree } from '@/components/review/FileTree'
import { CommentComposer, ReviewThreadCard } from '@/components/review/ReviewThread'
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog'
import { Textarea } from '@/components/ui/textarea'
import { Button } from '@/components/ui/button'
import { ToggleGroup, ToggleGroupItem } from '@/components/ui/toggle-group'
import { useFleet } from '@/hooks/useFleet'
import { useDiff } from '@/hooks/useDiff'
import { useReview } from '@/hooks/useReview'
import { useNow } from '@/hooks/useNow'
import { usePersistentState } from '@/hooks/usePersistentState'
import { useSessionTitle } from '@/hooks/useTitles'
import { relTime } from '@/lib/format'
import { fileAnchor, fileSignature, orphanThreadsByKey, parseReviewLocation, rangeText, repoViews, rowsToRange, selectLine, selectionCode, threadKey, type LineRow, type LineSelection, type Side } from '@/lib/review'
import type { DiffRepo, ReviewThread } from '@/api/types'
import { findSession, sessionHref } from '@/lib/sessions'
import { storage } from '@/lib/storage'
import { cn } from '@/lib/utils'
import { SessionScreen } from '@/screens/SessionScreen'

/** A file whose diff changed in the last update keeps its highlight this long. */
const FRESH_MS = 10_000
const NO_THREADS: ReviewThread[] = []
const parseView = (r: string) => (r === 'split' || r === 'unified' ? r : undefined)
const parseBool = (r: string) => (r === 'true' ? true : r === 'false' ? false : undefined)

/**
 * `#/review/<host>/<id>`: GitHub's "Files changed" over every checkout the session works in
 * (its cwd's, and the worktrees it edits from there — a meta repo's PRs), following it live. Desktop (`pane`): file tree | diff | the session itself (chat + composer), so the
 * review and the conversation sit side by side. Mobile: the diff alone.
 */
export function ReviewScreen({ layout = 'screen' }: { layout?: 'screen' | 'pane' }) {
  const pane = layout === 'pane'
  const [location, navigate] = useLocation()
  const loc = parseReviewLocation(location)
  const host = loc?.host ?? ''
  const id = loc?.id ?? ''
  const { fleet } = useFleet()
  const session = findSession(fleet, host, id)
  const plainSession = useMemo(() => (host && id ? { host, id } : null), [host, id])
  const { title } = useSessionTitle(session, `${host}/${id}`)
  const now = useNow(5_000)
  const { data, error, changedAt } = useDiff(host, id)

  const [view, setView] = usePersistentState<DiffView>('fleet.diffView', pane ? 'split' : 'unified', parseView)
  const [showTree, setShowTree] = usePersistentState<boolean>('fleet.reviewTree', true, parseBool)
  const [showChat, setShowChat] = usePersistentState<boolean>('fleet.reviewChat', true, parseBool)

  const views = useMemo(() => repoViews(data?.repos ?? []), [data])
  const clean = useMemo(() => (data?.repos ?? []).filter((r) => !r.error && !r.files?.length), [data])
  // Every file across checkouts, by key (root + path).
  const entries = useMemo(() => new Map(views.flatMap((v) => v.files.map((f) => [f.key, { ...f, repo: v.repo }] as const))), [views])
  const files = useMemo(() => [...entries.values()], [entries])
  const signatures = useMemo(() => new Map(files.map((f) => [f.key, fileSignature(f.file)])), [files])
  const stats = useMemo(
    () => views.reduce((a, v) => ({ files: a.files + (v.repo.stats?.files ?? 0), additions: a.additions + (v.repo.stats?.additions ?? 0), deletions: a.deletions + (v.repo.stats?.deletions ?? 0) }), { files: 0, additions: 0, deletions: 0 }),
    [views],
  )

  // Viewed: per session, path → the diff's signature when ticked (a new change unticks it).
  const viewedKey = `fleet.reviewViewed:${host}/${id}`
  const [viewedMap, setViewedMap] = useState<Record<string, string>>(() => storage.getJSON<Record<string, string>>(viewedKey) ?? {})
  const isViewed = useCallback((path: string) => viewedMap[path] != null && viewedMap[path] === signatures.get(path), [viewedMap, signatures])
  const toggleViewed = useCallback(
    (path: string) => {
      setViewedMap((m) => {
        const next = { ...m }
        if (next[path] != null && next[path] === signatures.get(path)) delete next[path]
        else next[path] = signatures.get(path) ?? ''
        storage.set(viewedKey, JSON.stringify(next))
        return next
      })
    },
    [signatures, viewedKey],
  )
  const viewedCount = files.filter((f) => isViewed(f.key)).length

  // Fresh: the files whose diff changed in the latest update (not on the first load).
  const prevSigs = useRef<Map<string, string> | null>(null)
  const [fresh, setFresh] = useState<{ paths: Set<string>; at: number }>({ paths: new Set(), at: 0 })
  useEffect(() => {
    if (!data) return // nothing loaded yet: the first answer is not a change
    const prev = prevSigs.current
    prevSigs.current = signatures
    if (!prev) return
    const changed = new Set([...signatures].filter(([p, s]) => prev.get(p) !== s).map(([p]) => p))
    if (changed.size) setFresh({ paths: changed, at: Date.now() })
  }, [data, signatures])
  const isFresh = useCallback((path: string) => fresh.paths.has(path) && now - fresh.at < FRESH_MS, [fresh, now])

  const [collapsed, setCollapsed] = useState<Set<string>>(() => new Set())
  const toggleCollapsed = useCallback(
    (path: string) => {
      // A viewed file shows collapsed; expanding it unticks Viewed like GitHub does.
      if (isViewed(path)) return toggleViewed(path)
      setCollapsed((c) => {
        const next = new Set(c)
        if (next.has(path)) next.delete(path)
        else next.add(path)
        return next
      })
    },
    [isViewed, toggleViewed],
  )
  const allCollapsed = files.length > 0 && files.every((f) => collapsed.has(f.key) || isViewed(f.key))

  const [active, setActive] = useState<string | null>(null)
  const openFile = (path: string) => {
    setActive(path)
    setCollapsed((c) => {
      if (!c.has(path)) return c
      const next = new Set(c)
      next.delete(path)
      return next
    })
    requestAnimationFrame(() => document.getElementById(fileAnchor(path))?.scrollIntoView({ block: 'start', behavior: 'smooth' }))
  }

  // --- comments --------------------------------------------------------------
  const rv = useReview(host, id)
  const threads = useMemo(() => rv.review?.threads ?? [], [rv.review])
  const drafts = rv.review?.drafts ?? 0
  // Threads made before the reviewer knew several checkouts belong to the session's own (the first).
  const defaultRoot = data?.repos?.[0]?.root ?? ''
  const byKey = useMemo(() => {
    const m = new Map<string, ReviewThread[]>()
    for (const t of threads) {
      const k = threadKey(t, defaultRoot)
      m.set(k, [...(m.get(k) ?? []), t])
    }
    return m
  }, [threads, defaultRoot])
  const orphans = useMemo(() => (data ? orphanThreadsByKey(new Set(entries.keys()), threads, defaultRoot) : []), [data, entries, threads, defaultRoot])
  const [selection, setSelection] = useState<LineSelection | null>(null)
  const [composing, setComposing] = useState(false)
  const onLine = useCallback((path: string, side: Side, n: number, extend: boolean) => {
    setSelection((prev) => selectLine(prev, path, side, n, extend))
    setComposing(true)
  }, [])
  const closeComposer = useCallback(() => {
    setComposing(false)
    setSelection(null)
  }, [])
  const renderThread = useCallback((t: ReviewThread, outdated: boolean) => <ReviewThreadCard thread={t} outdated={outdated} actions={rv} />, [rv])
  // Select code with the mouse → a "Comment on lines …" button at the selection's end.
  const bodyRef = useRef<HTMLDivElement>(null)
  const [pick, setPick] = useState<{ sel: LineSelection; x: number; y: number } | null>(null)
  const readSelection = useCallback(() => {
    const s = window.getSelection()
    if (!s || s.isCollapsed || !s.rangeCount) return setPick(null)
    const rowOf = (node: Node | null) => (node instanceof Element ? node : node?.parentElement)?.closest<HTMLElement>('[data-line]') ?? null
    const a = rowOf(s.anchorNode)
    const f = rowOf(s.focusNode)
    const card = a?.closest<HTMLElement>('[data-file-key]')
    if (!a || !f || !card || card !== f.closest('[data-file-key]')) return setPick(null)
    const all = [...card.querySelectorAll<HTMLElement>('[data-line]')]
    let i = all.indexOf(a)
    let j = all.indexOf(f)
    if (i > j) [i, j] = [j, i]
    const num = (v: string | undefined) => (v == null || v === '' ? null : Number(v))
    const rows: LineRow[] = all.slice(i, j + 1).map((el) => ({ side: el.dataset.side as Side | undefined, o: num(el.dataset.o), n: num(el.dataset.n), t: el.dataset.t ?? ' ' }))
    const range = rowsToRange(rows, f.dataset.side as Side | undefined)
    const key = card.dataset.fileKey
    if (!range || !key) return setPick(null)
    const rect = s.getRangeAt(0).getBoundingClientRect()
    // Inside the diff column (the session pane sits to its right).
    const box = bodyRef.current?.getBoundingClientRect()
    const right = (box?.right ?? window.innerWidth) - 230
    setPick({ sel: { path: key, ...range }, x: Math.max(box?.left ?? 0, Math.min(rect.right, right)), y: Math.min(rect.bottom + 6, window.innerHeight - 48) })
  }, [])
  const commentOnPick = () => {
    if (!pick) return
    setSelection(pick.sel)
    setComposing(true)
    setPick(null)
    window.getSelection()?.removeAllRanges()
  }

  // selection.path is a file key (root + path).
  const sel = selection ? entries.get(selection.path) ?? null : null
  const renderComposer = () =>
    selection && sel ? (
      <CommentComposer
        key={`${selection.path}:${selection.side}:${selection.start}`}
        label={rangeText(selection)}
        drafts={drafts}
        onCancel={closeComposer}
        onSubmit={(body, draft) =>
          rv.comment(
            { root: sel.repo.root, path: sel.file.path, side: selection.side, start: selection.start, end: selection.end, code: selectionCode(sel.file, selection) },
            body,
            draft,
            sel.repo.base?.sha ?? null,
          )
        }
      />
    ) : null
  const [submitOpen, setSubmitOpen] = useState(false)
  const [summary, setSummary] = useState('')
  const [submitting, setSubmitting] = useState(false)
  const submit = async () => {
    setSubmitting(true)
    const ok = await rv.submit(summary)
    setSubmitting(false)
    if (ok) {
      setSubmitOpen(false)
      setSummary('')
    }
  }

  const back = () => navigate(session ? sessionHref(session) : `/s/${encodeURIComponent(host)}/${encodeURIComponent(id)}`)

  const header = (
    <header
      data-tauri-drag-region={pane ? 'deep' : undefined}
      className="z-20 shrink-0 border-b bg-background/90 pt-safe px-safe backdrop-blur-md backdrop-saturate-150"
    >
      <div className={cn('flex w-full items-center gap-1 py-1.5 pr-2', pane ? 'titlebar titlebar-lead pl-2 [--titlebar-pad:0.5rem]' : 'pl-1')}>
        <Button variant="ghost" size="icon" aria-label="Back to session" title="Back to session" onClick={back} className="size-11 shrink-0 rounded-xl">
          <ChevronLeftIcon className="size-6" />
        </Button>
        <div className="min-w-0 flex-1">
          <div className="truncate text-[0.9375rem] leading-tight font-bold">Changes · {title}</div>
          <div className="mt-0.5 flex min-w-0 items-center gap-1.5 overflow-hidden text-xs whitespace-nowrap text-muted-foreground">
            <HostBadge host={host} className="h-4 px-1 text-[0.625rem]" />
            {data ? (
              <>
                {views.length === 1 ? (
                  <>
                    <GitBranchIcon className="size-3 shrink-0" />
                    <BranchText repo={views[0].repo} />
                  </>
                ) : (
                  <>
                    <FolderGitIcon className="size-3 shrink-0" />
                    <span className="shrink-0">{views.length} checkouts</span>
                  </>
                )}
                <span className="text-dimmer">·</span>
                <span className="shrink-0">{stats.files} files</span>
                <DiffStat additions={stats.additions} deletions={stats.deletions} bar={false} />
                {changedAt ? <span className="truncate text-dimmer tabular-nums">· changed {relTime(changedAt, now)} ago</span> : null}
              </>
            ) : null}
          </div>
        </div>
        {files.length ? (
          <span className="mr-1 hidden shrink-0 text-xs text-muted-foreground tabular-nums sm:inline">
            {viewedCount} / {files.length} viewed
          </span>
        ) : null}
        <Button size="sm" variant={drafts ? 'default' : 'outline'} className="mr-1 shrink-0" disabled={!drafts} onClick={() => setSubmitOpen(true)} title={drafts ? 'Send the pending comments to the session in one prompt' : 'Start a review from a comment to submit it'}>
          Submit review{drafts ? ` (${drafts})` : ''}
        </Button>
        <Button
          variant="ghost"
          size="icon"
          className="size-10 shrink-0 rounded-xl"
          aria-label={allCollapsed ? 'Expand all' : 'Collapse all'}
          title={allCollapsed ? 'Expand all' : 'Collapse all'}
          onClick={() => setCollapsed(allCollapsed ? new Set() : new Set(files.map((f) => f.key)))}
        >
          {allCollapsed ? <UnfoldVerticalIcon className="size-5" /> : <FoldVerticalIcon className="size-5" />}
        </Button>
        <ToggleGroup type="single" value={view} onValueChange={(v) => v && setView(v as DiffView)} spacing={0} aria-label="Diff layout" className="shrink-0 rounded-xl bg-muted p-0.5">
          <ToggleGroupItem value="unified" aria-label="Unified" title="Unified" className={toggleItem}>
            <RowsIcon />
          </ToggleGroupItem>
          <ToggleGroupItem value="split" aria-label="Split" title="Split" className={toggleItem}>
            <Columns2Icon />
          </ToggleGroupItem>
        </ToggleGroup>
        {pane ? (
          <>
            <Button variant="ghost" size="icon" aria-label="File tree" title="File tree" aria-pressed={showTree} onClick={() => setShowTree(!showTree)} className={cn('size-10 shrink-0 rounded-xl', showTree && 'bg-muted text-foreground')}>
              <ListTreeIcon className="size-5" />
            </Button>
            <Button variant="ghost" size="icon" aria-label="Session" title="Session" aria-pressed={showChat} onClick={() => setShowChat(!showChat)} className={cn('size-10 shrink-0 rounded-xl', showChat && 'bg-muted text-foreground')}>
              <MessageSquareTextIcon className="size-5" />
            </Button>
          </>
        ) : null}
      </div>
    </header>
  )

  const body = (
    <div
      ref={bodyRef}
      className="min-w-0 flex-1 space-y-3 overflow-y-auto p-3"
      // After the browser settles the selection.
      onMouseUp={() => setTimeout(readSelection, 0)}
      onKeyUp={(e) => e.shiftKey && readSelection()}
      onScroll={() => pick && setPick(null)}
    >
      {files.length && !threads.length ? (
        <p className="px-1 text-xs text-muted-foreground">
          To comment: select code with the mouse, or click a line number (shift-click for a range). The comment goes to the session with those lines.
        </p>
      ) : null}
      {error ? <p className="rounded-lg border border-destructive/40 bg-destructive/5 px-3 py-2 text-sm text-destructive">{error}</p> : null}
      {!data && !error ? <p className="px-1 py-6 text-center text-sm text-muted-foreground">Loading the diff…</p> : null}
      {data && !files.length ? (
        <div className="px-1 py-12 text-center">
          <p className="text-base font-semibold">No changes</p>
          <p className="mt-1 text-sm text-dimmer">
            {clean.length ? `Nothing changed in ${clean.map((r) => r.label).join(', ')}.` : 'This session works in no git checkout yet.'} Checkouts show up here as the session edits files in them.
          </p>
        </div>
      ) : null}
      {views.map((v) => (
        <section key={v.repo.root} aria-label={v.repo.label} className="space-y-3">
          {views.length > 1 || v.repo.error ? <RepoHeader repo={v.repo} /> : null}
          {v.repo.error ? <p className="rounded-lg border border-destructive/40 px-3 py-2 text-sm text-destructive">{v.repo.error}</p> : null}
          {v.repo.truncated ? <p className="text-xs text-muted-foreground">The diff is very large: later files are listed without their lines.</p> : null}
          {v.files.map(({ key, file }) => (
            <FileDiff
              key={key}
              fileKey={key}
              file={file}
              view={view}
              collapsed={collapsed.has(key)}
              viewed={isViewed(key)}
              fresh={isFresh(key)}
              onToggleCollapsed={toggleCollapsed}
              onToggleViewed={toggleViewed}
              selection={selection?.path === key ? selection : null}
              composing={composing && selection?.path === key}
              onLine={onLine}
              threads={byKey.get(key) ?? NO_THREADS}
              renderThread={renderThread}
              renderComposer={renderComposer}
              session={plainSession}
            />
          ))}
        </section>
      ))}
      {orphans.length ? (
        <section aria-label="Comments on files no longer changed" className="overflow-hidden rounded-lg border">
          <p className="border-b bg-card px-3 py-1.5 text-xs text-muted-foreground">Comments on files no longer in the diff</p>
          {orphans.map((t) => (
            <div key={t.id}>
              <p className="px-3 pt-2 font-mono text-xs">{t.display ?? t.path}</p>
              {renderThread(t, true)}
            </div>
          ))}
        </section>
      ) : null}
      {rv.error ? <p className="text-xs text-destructive">Comments: {rv.error}</p> : null}
      {pick ? (
        <Button
          size="sm"
          className="fixed z-30 shadow-lg"
          style={{ left: pick.x, top: pick.y }}
          // Keep the text selection while clicking.
          onMouseDown={(e) => e.preventDefault()}
          onClick={commentOnPick}
        >
          <MessageSquarePlusIcon />
          Comment on {rangeText(pick.sel)}
        </Button>
      ) : null}
    </div>
  )

  const dialog = (
    <Dialog open={submitOpen} onOpenChange={setSubmitOpen}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>Submit review</DialogTitle>
          <DialogDescription>
            {drafts === 1 ? 'The pending comment goes' : `${drafts} pending comments go`} to the session in one prompt, each with its lines. The answer shows in every thread and in the conversation.
          </DialogDescription>
        </DialogHeader>
        <Textarea
          value={summary}
          onChange={(e) => setSummary(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
              e.preventDefault()
              void submit()
            }
          }}
          placeholder="Overall comment (optional)"
          className="min-h-24 text-sm"
        />
        <DialogFooter>
          <Button variant="ghost" onClick={() => setSubmitOpen(false)}>
            Cancel
          </Button>
          <Button disabled={!drafts || submitting} onClick={() => void submit()}>
            Submit review
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )

  if (!loc) return null
  if (!pane) {
    return (
      <div className="flex h-dvh flex-col bg-background">
        {header}
        {body}
        {dialog}
      </div>
    )
  }
  return (
    <div className="flex min-w-0 flex-1">
      <div className="flex min-w-0 flex-1 flex-col">
        {header}
        <div className="flex min-h-0 flex-1">
          {showTree && files.length ? (
            <aside aria-label="Changed files" className="w-64 shrink-0 overflow-y-auto border-r px-1.5">
              {views.map((v) => (
                <div key={v.repo.root}>
                  {views.length > 1 ? (
                    <p className="mt-2 flex items-center gap-1.5 px-2 text-xs font-semibold text-muted-foreground" title={v.repo.root}>
                      <FolderGitIcon className="size-3.5 shrink-0" />
                      <span className="truncate">{v.repo.label}</span>
                    </p>
                  ) : null}
                  <FileTree nodes={v.tree} viewed={isViewed} fresh={isFresh} active={active} onOpen={openFile} />
                </div>
              ))}
            </aside>
          ) : null}
          {body}
        </div>
      </div>
      {showChat ? (
        <aside aria-label="Session" className="flex w-[28rem] shrink-0 border-l xl:w-[32rem]">
          <SessionScreen key={`${host}/${id}`} host={host} id={id} layout="pane" embedded />
        </aside>
      ) : null}
      {dialog}
    </div>
  )
}

const toggleItem =
  'size-9 rounded-[10px] text-muted-foreground data-[state=on]:bg-background data-[state=on]:text-foreground data-[state=on]:shadow-sm dark:data-[state=on]:bg-accent [&_svg:not([class*=size-])]:size-[18px]'

function BranchText({ repo }: { repo: DiffRepo }) {
  return (
    <span className="truncate font-mono">
      {repo.branch ?? 'detached'}
      {repo.base?.ref && repo.base.ref !== repo.branch ? <span className="text-dimmer"> ← {repo.base.ref}</span> : null}
    </span>
  )
}

/** A checkout's heading in the diff list (several checkouts, or one that failed). */
function RepoHeader({ repo }: { repo: DiffRepo }) {
  return (
    <div className="flex items-center gap-2 px-1 pt-2 text-sm" title={repo.root}>
      <FolderGitIcon className="size-4 shrink-0 text-muted-foreground" />
      <span className="truncate font-semibold">{repo.label}</span>
      <span className="flex min-w-0 items-center gap-1 text-xs text-muted-foreground">
        <GitBranchIcon className="size-3 shrink-0" />
        <BranchText repo={repo} />
      </span>
      {repo.stats ? <DiffStat additions={repo.stats.additions} deletions={repo.stats.deletions} bar={false} /> : null}
    </div>
  )
}

