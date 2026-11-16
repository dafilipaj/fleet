import { ExternalLinkIcon, RefreshCwIcon, SparklesIcon, ChevronLeftIcon, Columns2Icon, FoldVerticalIcon, FolderGitIcon, GitBranchIcon, ListTreeIcon, MessageSquarePlusIcon, MessageSquareTextIcon, RowsIcon, UnfoldVerticalIcon } from 'lucide-react'
import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from 'react'
import { useLocation } from 'wouter'

import { HostBadge } from '@/components/HostBadge'
import { DiffStat, FileDiff, type DiffView } from '@/components/review/FileDiff'
import { FileTree } from '@/components/review/FileTree'
import { CommentComposer, ReviewThreadCard, type CommentTarget, type ThreadActions } from '@/components/review/ReviewThread'
import { EntryHeader, EntryRow } from '@/components/review/ReviewEntry'
import { SideAsks } from '@/components/review/SideAsks'
import { api, type PrKey } from '@/api/client'
import { watchForSpawned } from '@/api/spawnWatch'
import { toast } from 'sonner'
import { usePrDiff, usePrNotes, useSessionPrNotes } from '@/hooks/usePrReview'
import { useAsks, type AskBody } from '@/hooks/useAsks'
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
import { reviewHref, DEFAULT_KINDS, FILE_KINDS, entryAnchor, entryBucket, entrySections, fileAnchor, fileKey, fileSignature, kindCounts, orphanThreadsByKey, parsePrReviewLocation, parseReviewLocation, prRefOf, rangeText, repoKey, repoViews, rowsToRange, selectLine, selectionCode, threadKey, type EntryBucket, type FileKind, type LineRow, type LineSelection, type Side } from '@/lib/review'
import type { DiffRepo, PrNote, ReviewAnchor, ReviewPr, ReviewThread, SideAsk } from '@/api/types'
import { prHref } from '@/lib/prs'
import { findSession, sessionHref } from '@/lib/sessions'
import { storage } from '@/lib/storage'
import { cn } from '@/lib/utils'
import { SessionScreen } from '@/screens/SessionScreen'

/** A file whose diff changed in the last update keeps its highlight this long. */
const FRESH_MS = 10_000
const NO_THREADS: ReviewThread[] = []
const parseView = (r: string) => (r === 'split' || r === 'unified' ? r : undefined)
const parseBool = (r: string) => (r === 'true' ? true : r === 'false' ? false : undefined)
const parseTarget = (r: string) => (r === 'session' || r === 'note' ? r : undefined)
const SECTION_TITLE: Record<EntryBucket, string> = { pr: 'Pull requests', done: 'Merged or closed', local: 'Without a PR' }

/** A PR note shown in a session's reviewer: which PR it is on (its own actions). */
type NoteThread = ReviewThread & { noteOf: { pr: ReviewPr } }
const noteOf = (t: ReviewThread) => (t as Partial<NoteThread>).noteOf ?? null
const KIND_NAMES = new Set<string>(FILE_KINDS.map((k) => k.kind))
const DEFAULT_SHOWN = FILE_KINDS.filter((k) => DEFAULT_KINDS[k.kind]).map((k) => k.kind).join(',')
/** The shown kinds, stored as `spec,code`; anything unknown falls back to the default. */
const parseKinds = (r: string) => (r === '' || r.split(',').every((k) => KIND_NAMES.has(k)) ? r : undefined)

type Layout = 'screen' | 'pane'

/** What the reviewer needs from where the diff comes from: a session's checkouts, or a PR without a session. */
interface ReviewSource {
  kind: 'session' | 'pr'
  host: string
  /** the Viewed ticks are kept under this */
  storageKey: string
  title: ReactNode
  backLabel: string
  back: () => void
  data: { repos?: DiffRepo[] } | null
  error: string | null
  changedAt: number | null
  rv: {
    review: { threads: ReviewThread[]; drafts: number } | null
    error: string | null
    comment: (anchor: ReviewAnchor, body: string, draft: boolean, base: string | null) => Promise<boolean>
    reply: (thread: string, body: string) => Promise<boolean>
    edit: (thread: string, body: string) => Promise<boolean>
    resolve: (thread: string, resolved: boolean) => Promise<boolean>
    remove: (thread: string) => Promise<boolean>
  }
  askStart: ((body: AskBody) => Promise<SideAsk>) | null
  plainSession: { host: string; id: string } | null
  /** the session beside the diff (desktop) */
  chat: ReactNode | null
  /** the header's submit button and its dialog */
  submit: { label: string; hint: string; title: string; description: (drafts: number) => string; placeholder: string; run: (summary: string) => Promise<boolean> }
  /** PR notes: a single comment is a note; the composer has one button */
  notes?: { post: (thread: string) => Promise<boolean> }
  hint: string
  empty: string
  headerActions?: ReactNode
  /** A session's entries that have a PR: notes on them, and a worktree for one without. */
  prs?: {
    notes: Record<string, PrNote[]>
    create: (pr: ReviewPr, anchor: ReviewAnchor, body: string, base: string | null) => Promise<boolean>
    actionsFor: (pr: ReviewPr, headOid: string | null) => ThreadActions & { post: (thread: string) => Promise<boolean> }
    setUp: (pr: ReviewPr) => Promise<boolean>
  }
}

/**
 * `#/review/<host>/<id>`: GitHub's "Files changed" over every checkout the session works in
 * (its cwd's, and the worktrees it edits from there — a meta repo's PRs), following it live. Desktop (`pane`): file tree | diff | the session itself (chat + composer), so the
 * review and the conversation sit side by side. Mobile: the diff alone.
 * `#/review/pr/<host>/<owner>/<repo>/<n>`: the same over a PR from the host's clone, with no
 * session: comments are notes kept in Fleet (post one to GitHub, or hand them all to a session).
 */
export function ReviewScreen({ layout = 'screen' }: { layout?: Layout }) {
  const [location] = useLocation()
  const prLoc = parsePrReviewLocation(location)
  if (prLoc) return <PrReview key={`${prLoc.host}/${prLoc.owner}/${prLoc.repo}/${prLoc.number}`} host={prLoc.host} pr={prLoc} layout={layout} />
  const loc = parseReviewLocation(location)
  if (!loc) return null
  return <SessionReview key={`${loc.host}/${loc.id}/${loc.root ?? ''}`} host={loc.host} id={loc.id} root={loc.root ?? null} layout={layout} />
}

/** A thread on checkout `root` (one without a root is the cwd's). */
function threadInRoot(t: ReviewThread, root: string, cwd: string | null | undefined): boolean {
  if (t.root) return t.root === root
  return !!cwd && (cwd === root || cwd.startsWith(`${root}/`))
}

/**
 * `root`: the review of that one checkout (a worktree or a PR's, from the Brief) — its diff, its
 * threads, and Submit sends only its drafts. Without it: every checkout the session works in.
 */
function SessionReview({ host, id, root, layout }: { host: string; id: string; root: string | null; layout: Layout }) {
  const [, navigate] = useLocation()
  const { fleet } = useFleet()
  const session = findSession(fleet, host, id)
  const plainSession = useMemo(() => ({ host, id }), [host, id])
  const { title } = useSessionTitle(session, `${host}/${id}`)
  const useDiffRefresh = useDiff(host, id, root)
  const { data, error, changedAt } = useDiffRefresh
  const fullReview = useReview(host, id, root)
  const cwd = session?.cwd
  const scoped = useMemo(() => {
    if (!root || !fullReview.review) return fullReview.review
    const threads = fullReview.review.threads.filter((t) => threadInRoot(t, root, cwd))
    return { ...fullReview.review, threads, drafts: threads.filter((t) => t.status === 'draft').length }
  }, [fullReview.review, root, cwd])
  const review = root ? { ...fullReview, review: scoped } : fullReview
  const scopeRepo = root ? (data?.repos?.[0] ?? null) : null
  const scopeLabel = root ? (scopeRepo?.branch ?? scopeRepo?.label ?? root.split('/').pop() ?? root) : null
  const askStart = useCallback((body: AskBody) => api.ask(host, id, body).then((r) => r.ask), [host, id])
  const entryPrs = useMemo(() => (data?.repos ?? []).flatMap((r) => (r.pr ? [r.pr] : [])), [data])
  const prNotes = useSessionPrNotes(host, entryPrs)
  const { refresh: refreshDiff } = useDiffRefresh
  const setUp = useCallback(
    async (pr: ReviewPr) => {
      const ref = prRefOf(pr)
      try {
        const r = await api.prCheckout(host, ref)
        toast.success(`${ref} is checked out`, { description: r.worktree.path })
        refreshDiff()
        return true
      } catch (err) {
        toast.error(`Could not set up a worktree for ${ref}`, { description: (err as Error)?.message })
        return false
      }
    },
    [host, refreshDiff],
  )
  const source: ReviewSource = {
    kind: 'session',
    host,
    storageKey: `${host}/${id}`,
    title: scopeLabel ? (
      <>
        Changes · {title} · <span className="font-mono text-muted-foreground" title={root ?? undefined}>{scopeLabel}</span>
      </>
    ) : (
      <>Changes · {title}</>
    ),
    backLabel: 'Back to session',
    back: () => navigate(session ? sessionHref(session) : `/s/${encodeURIComponent(host)}/${encodeURIComponent(id)}`),
    data,
    error,
    changedAt,
    rv: review,
    askStart,
    plainSession,
    chat: <SessionScreen key={`${host}/${id}`} host={host} id={id} layout="pane" embedded />,
    submit: {
      label: 'Submit review',
      hint: 'Send the pending comments to the session in one prompt',
      title: 'Submit review',
      description: (drafts) => `${drafts === 1 ? 'The pending comment goes' : `${drafts} pending comments go`} to the session in one prompt, each with its lines. The answer shows in every thread and in the conversation.`,
      placeholder: 'Overall comment (optional)',
      run: (summary) => review.submit(summary),
    },
    hint: 'To comment: select code with the mouse, or click a line number (shift-click for a range). The comment goes to the session with those lines.',
    prs: {
      notes: prNotes.notes,
      create: (pr, anchor, body, base) => prNotes.create(pr, anchor, body, base),
      actionsFor: prNotes.actionsFor,
      setUp,
    },
    empty: root ? 'This checkout has no changes against its base branch.' : 'This session works in no git checkout yet. Checkouts show up here as the session edits files in them.',
    headerActions: root ? (
      <Button variant="ghost" size="sm" className="shrink-0" title="Review every checkout the session works in" onClick={() => navigate(reviewHref(host, id))}>
        <FolderGitIcon />
        All checkouts
      </Button>
    ) : undefined,
  }
  return <ReviewView layout={layout} source={source} />
}

function PrReview({ host, pr, layout }: { host: string; pr: PrKey; layout: Layout }) {
  const [, navigate] = useLocation()
  const { applyFleet } = useFleet()
  const { data, error, changedAt, refetch } = usePrDiff(host, pr)
  const notes = usePrNotes(host, pr)
  const info = data?.pr ?? notes.review?.pr ?? null
  const repo = data?.repos?.[0] ?? null
  const head = repo?.head ?? null
  const base = repo?.base?.sha ?? null
  const askStart = useCallback(
    (body: AskBody) => api.prAsk(host, pr, { ...body, head, base }).then((r) => r.ask),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [host, pr.owner, pr.repo, pr.number, head, base],
  )
  const ref = `${pr.owner}/${pr.repo}#${pr.number}`
  const handoff = async (summary: string) => {
    try {
      const res = await api.prHandoff(host, pr, summary)
      const name = res.spawn?.name || res.spawn?.tmuxSession || ref
      const tid = toast.loading(`Starting ${name} on ${host} with your notes…`)
      watchForSpawned(
        { host: res.spawn?.host || host, name, tmuxSession: res.spawn?.tmuxSession ?? name },
        {
          onFleet: applyFleet,
          onFound: (s) => {
            toast.success(`${name} is up`, { id: tid })
            navigate(sessionHref(s))
          },
          onTimeout: () => toast.error(`${name} has not shown up yet`, { id: tid, description: 'Check the list in a moment' }),
        },
      )
      return true
    } catch (err) {
      toast.error('Could not start a session', { description: (err as Error)?.message })
      return false
    }
  }
  const source: ReviewSource = {
    kind: 'pr',
    host,
    storageKey: `pr:${host}/${pr.owner}/${pr.repo}/${pr.number}`.toLowerCase(),
    title: (
      <>
        <span className="font-mono font-semibold text-muted-foreground">{ref}</span> {info?.title ?? ''}
      </>
    ),
    backLabel: 'Back to the PR',
    back: () => navigate(prHref({ owner: pr.owner, repo: pr.repo, number: pr.number })),
    data,
    error,
    changedAt,
    rv: notes,
    askStart,
    plainSession: null,
    chat: null,
    notes: {
      post: (thread) => {
        if (!head) {
          toast.error('The PR head is not known yet — wait for the diff')
          return Promise.resolve(false)
        }
        return notes.post(thread, head)
      },
    },
    submit: {
      label: 'Hand to a session',
      hint: 'Start a session on the PR’s worktree with your notes as its first prompt',
      title: 'Hand the notes to a session',
      description: (drafts) =>
        `A new session checks ${ref} out in a worktree and gets ${drafts === 1 ? 'your note' : `your ${drafts} notes`} (each with its lines) to work through with you. Resolved notes stay out.`,
      placeholder: 'What should it do with them? (optional, e.g. “turn the good ones into review comments”)',
      run: handoff,
    },
    hint: 'Notes stay in Fleet: select code (or click a line number) to write one, post any of them to GitHub, or hand them all to a session. Ask asks Claude about the lines on the side.',
    empty: 'This PR changes no files.',
    headerActions: (
      <>
        <Button variant="ghost" size="icon" className="size-10 shrink-0 rounded-xl" aria-label="Fetch the PR now" title="Fetch the PR now (otherwise at most once a minute)" onClick={refetch}>
          <RefreshCwIcon className="size-5" />
        </Button>
        {info?.url ? (
          <Button asChild variant="ghost" size="icon" className="size-10 shrink-0 rounded-xl" title="Open on GitHub">
            <a href={info.url} target="_blank" rel="noreferrer" aria-label="Open on GitHub">
              <ExternalLinkIcon className="size-5" />
            </a>
          </Button>
        ) : null}
      </>
    ),
  }
  return <ReviewView layout={layout} source={source} />
}

function ReviewView({ layout, source }: { layout: Layout; source: ReviewSource }) {
  const pane = layout === 'pane'
  const { host, data, error, changedAt, plainSession } = source
  const now = useNow(5_000)

  const [view, setView] = usePersistentState<DiffView>('fleet.diffView', pane ? 'split' : 'unified', parseView)
  const [showTree, setShowTree] = usePersistentState<boolean>('fleet.reviewTree', true, parseBool)
  const [showChat, setShowChat] = usePersistentState<boolean>('fleet.reviewChat', true, parseBool)

  const [shownRaw, setShownRaw] = usePersistentState<string>('fleet.reviewKinds', DEFAULT_SHOWN, parseKinds)
  const shown = useMemo(() => {
    const on = new Set(shownRaw.split(','))
    return Object.fromEntries(FILE_KINDS.map((k) => [k.kind, on.has(k.kind)])) as Record<FileKind, boolean>
  }, [shownRaw])
  const toggleKind = (kind: FileKind) => setShownRaw(FILE_KINDS.filter((k) => (k.kind === kind ? !shown[k.kind] : shown[k.kind])).map((k) => k.kind).join(','))
  const counts = useMemo(() => kindCounts(data?.repos ?? []), [data])
  const views = useMemo(() => repoViews(data?.repos ?? [], shown), [data, shown])
  // Every changed file's key, shown or not: a thread on a hidden file is not an orphan.
  const allKeys = useMemo(() => new Set((data?.repos ?? []).flatMap((r) => (r.files ?? []).map((f) => fileKey(repoKey(r), f.path)))), [data])
  const hiddenCount = allKeys.size - views.reduce((n, v) => n + v.files.length, 0)
  // Entries by section (PRs, merged / closed, without a PR), each by repository. With several
  // sections, all but the first start folded: the PRs come first, local-only changes wait below.
  const sections = useMemo(() => entrySections(views), [views])
  const grouped = source.kind === 'session' || views.length > 1
  const [sectionOpen, setSectionOpen] = useState<Partial<Record<EntryBucket, boolean>>>({})
  const isOpen = (bucket: EntryBucket) => sectionOpen[bucket] ?? (sections.length < 2 || sections[0].bucket === bucket)
  const openEntry = (key: string, bucket: EntryBucket) => {
    setSectionOpen((o) => ({ ...o, [bucket]: true }))
    requestAnimationFrame(() => document.getElementById(entryAnchor(key))?.scrollIntoView({ block: 'start', behavior: 'smooth' }))
  }
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
  const viewedKey = `fleet.reviewViewed:${source.storageKey}`
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
    const owner = entries.get(path)?.repo
    if (owner) setSectionOpen((o) => ({ ...o, [entryBucket(owner)]: true }))
    setCollapsed((c) => {
      if (!c.has(path)) return c
      const next = new Set(c)
      next.delete(path)
      return next
    })
    requestAnimationFrame(() => document.getElementById(fileAnchor(path))?.scrollIntoView({ block: 'start', behavior: 'smooth' }))
  }

  // --- comments --------------------------------------------------------------
  const rv = source.rv
  const threads = useMemo(() => rv.review?.threads ?? [], [rv.review])
  const drafts = rv.review?.drafts ?? 0
  // What the header's button sends: a session's pending comments, or a PR's open notes.
  const toSubmit = source.notes ? threads.filter((t) => !t.resolved).length : drafts
  // Threads made before the reviewer knew several checkouts belong to the session's own (the first).
  const defaultRoot = data?.repos?.[0]?.root ?? ''
  // Notes on the session's PRs sit with the entry of their PR (a worktree on its branch, or the PR itself).
  const prNotesByRef = source.prs?.notes
  const noteThreads = useMemo(() => {
    if (!prNotesByRef) return [] as NoteThread[]
    return (data?.repos ?? []).flatMap((r) => {
      if (!r.pr) return []
      const ref = prRefOf(r.pr)
      return (prNotesByRef[ref.toLowerCase()] ?? []).map((n): NoteThread => ({ ...n, root: r.root, pr: r.source === 'pr' ? ref : undefined, noteOf: { pr: r.pr! } }))
    })
  }, [data, prNotesByRef])
  const allThreads = useMemo(() => (noteThreads.length ? [...threads, ...noteThreads] : threads), [threads, noteThreads])
  const byKey = useMemo(() => {
    const m = new Map<string, ReviewThread[]>()
    for (const t of allThreads) {
      const k = threadKey(t, defaultRoot)
      m.set(k, [...(m.get(k) ?? []), t])
    }
    return m
  }, [allThreads, defaultRoot])
  const orphans = useMemo(() => (data ? orphanThreadsByKey(allKeys, allThreads, defaultRoot) : []), [data, allKeys, allThreads, defaultRoot])
  const [selection, setSelection] = useState<LineSelection | null>(null)
  const [composing, setComposing] = useState(false)
  // Opened from the selection's "Ask": the composer asks on the side by default.
  const [asking, setAsking] = useState(false)
  const onLine = useCallback((path: string, side: Side, n: number, extend: boolean) => {
    setSelection((prev) => selectLine(prev, path, side, n, extend))
    setComposing(true)
    setAsking(false)
  }, [])
  const closeComposer = useCallback(() => {
    setComposing(false)
    setSelection(null)
    setAsking(false)
  }, [])

  // --- side questions: a read-only fork of the session answers; nothing reaches its thread ---
  const side = useAsks(host, source.askStart)
  const askFiles = useRef(new Map<string, string>()) // ask id → file key, to jump back to its lines
  const notes = source.notes
  const prActions = source.prs?.actionsFor
  const renderThread = useCallback(
    (t: ReviewThread, outdated: boolean) => {
      const of = noteOf(t)
      if (of && prActions) {
        const a = prActions(of.pr, of.pr.headOid)
        return <ReviewThreadCard thread={t} outdated={outdated} actions={a} note={{ posted: (t as PrNote).posted ?? null, onPost: () => a.post(t.id) }} />
      }
      return <ReviewThreadCard thread={t} outdated={outdated} actions={rv} note={notes ? { posted: (t as PrNote).posted ?? null, onPost: () => notes.post(t.id) } : undefined} />
    },
    [rv, notes, prActions],
  )
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
    const right = (box?.right ?? window.innerWidth) - 300
    setPick({ sel: { path: key, ...range }, x: Math.max(box?.left ?? 0, Math.min(rect.right, right)), y: Math.min(rect.bottom + 6, window.innerHeight - 48) })
  }, [])
  const commentOnPick = (ask = false) => {
    if (!pick) return
    setSelection(pick.sel)
    setComposing(true)
    setAsking(ask)
    setPick(null)
    window.getSelection()?.removeAllRanges()
  }

  // selection.path is a file key (root + path).
  const sel = selection ? entries.get(selection.path) ?? null : null
  // On an entry with a PR, a comment can go to the session or be a note on the PR.
  const [target, setTarget] = usePersistentState<CommentTarget>('fleet.reviewCommentTarget', 'session', parseTarget)
  const selPr = sel?.repo.pr && source.prs ? sel.repo.pr : null
  const renderComposer = () =>
    selection && sel ? (
      <CommentComposer
        key={`${selection.path}:${selection.side}:${selection.start}`}
        label={rangeText(selection)}
        drafts={notes ? 0 : drafts}
        submitLabel={notes ? 'Add note' : undefined}
        target={selPr ? { value: target, onChange: setTarget, note: `Note on #${selPr.number}` } : undefined}
        onCancel={closeComposer}
        onSubmit={(body, draft) => {
          const anchor: ReviewAnchor = { root: sel.repo.root, path: sel.file.path, side: selection.side, start: selection.start, end: selection.end, code: selectionCode(sel.file, selection) }
          if (selPr && target === 'note' && source.prs) return source.prs.create(selPr, anchor, body, sel.repo.base?.sha ?? null)
          if (sel.repo.source === 'pr' && sel.repo.pr) anchor.pr = prRefOf(sel.repo.pr)
          return rv.comment(anchor, body, draft, sel.repo.base?.sha ?? null)
        }}
        asking={asking}
        onAsk={async (question) => {
          const a = await side.ask({ root: sel.repo.root, path: sel.file.path, side: selection.side, start: selection.start, end: selection.end, code: selectionCode(sel.file, selection), question })
          if (a) askFiles.current.set(a.id, selection.path)
          return !!a
        }}
      />
    ) : null
  const [submitOpen, setSubmitOpen] = useState(false)
  const [summary, setSummary] = useState('')
  const [submitting, setSubmitting] = useState(false)
  const submit = async () => {
    setSubmitting(true)
    const ok = await source.submit.run(summary)
    setSubmitting(false)
    if (ok) {
      setSubmitOpen(false)
      setSummary('')
    }
  }

  const header = (
    <header
      data-tauri-drag-region={pane ? 'deep' : undefined}
      className="z-20 shrink-0 border-b bg-background/90 pt-safe px-safe backdrop-blur-md backdrop-saturate-150"
    >
      <div className={cn('flex w-full items-center gap-1 py-1.5 pr-2', pane ? 'titlebar titlebar-lead pl-2 [--titlebar-pad:0.5rem]' : 'pl-1')}>
        <Button variant="ghost" size="icon" aria-label={source.backLabel} title={source.backLabel} onClick={source.back} className="size-11 shrink-0 rounded-xl">
          <ChevronLeftIcon className="size-6" />
        </Button>
        <div className="min-w-0 flex-1">
          <div className="truncate text-[0.9375rem] leading-tight font-bold">{source.title}</div>
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
                    <span className="shrink-0">{sections.reduce((n, x) => n + x.groups.length, 0) > 1 ? `${new Set(views.map((v) => (v.repo.remote || v.repo.label).toLowerCase())).size} repos · ` : ''}{views.length} branches</span>
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
        {source.headerActions}
        <Button size="sm" variant={toSubmit ? 'default' : 'outline'} className="mr-1 shrink-0" disabled={!toSubmit} onClick={() => setSubmitOpen(true)} title={toSubmit ? source.submit.hint : source.notes ? 'Write a note first' : 'Start a review from a comment to submit it'}>
          {source.submit.label}
          {toSubmit ? ` (${toSubmit})` : ''}
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
            {source.chat ? (
              <Button variant="ghost" size="icon" aria-label="Session" title="Session" aria-pressed={showChat} onClick={() => setShowChat(!showChat)} className={cn('size-10 shrink-0 rounded-xl', showChat && 'bg-muted text-foreground')}>
                <MessageSquareTextIcon className="size-5" />
              </Button>
            ) : null}
          </>
        ) : null}
      </div>
      {data && allKeys.size ? (
        <div className="px-2 pb-1.5">
          <div role="group" aria-label="Show files" className="flex w-fit max-w-full items-center gap-0.5 overflow-x-auto rounded-xl bg-muted p-0.5">
            {FILE_KINDS.map(({ kind, label }) => (
              <button
                key={kind}
                type="button"
                aria-pressed={shown[kind]}
                disabled={!counts[kind]}
                title={`${shown[kind] ? 'Hide' : 'Show'} ${label.toLowerCase()}`}
                onClick={() => toggleKind(kind)}
                className={cn(
                  'h-8 rounded-[10px] px-2.5 text-xs font-medium text-muted-foreground tabular-nums disabled:opacity-40',
                  shown[kind] && 'bg-background text-foreground shadow-sm dark:bg-accent',
                )}
              >
                {label} <span className="text-dimmer">{counts[kind]}</span>
              </button>
            ))}
          </div>
        </div>
      ) : null}
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
          {source.hint}
        </p>
      ) : null}
      {error ? <p className="rounded-lg border border-destructive/40 bg-destructive/5 px-3 py-2 text-sm text-destructive">{error}</p> : null}
      {!data && !error ? <p className="px-1 py-6 text-center text-sm text-muted-foreground">Loading the diff…</p> : null}
      {data && !files.length && hiddenCount ? (
        <div className="px-1 py-12 text-center">
          <p className="text-base font-semibold">Everything is filtered out</p>
          <p className="mt-1 text-sm text-dimmer">
            {hiddenCount === 1 ? '1 changed file is' : `${hiddenCount} changed files are`} hidden. Turn on Specs, Docs, Code or Tests above.
          </p>
        </div>
      ) : null}
      {data && !files.length && !hiddenCount ? (
        <div className="px-1 py-12 text-center">
          <p className="text-base font-semibold">No changes</p>
          <p className="mt-1 text-sm text-dimmer">
            {clean.length && source.kind === 'session' ? `Nothing changed in ${clean.map((r) => r.label).join(', ')}. Checkouts show up here as the session edits files in them.` : source.empty}
          </p>
        </div>
      ) : null}
      {sections.map(({ bucket, groups }) => {
        const open = isOpen(bucket)
        const count = groups.reduce((n, g) => n + g.views.length, 0)
        return (
          <section key={bucket} aria-label={SECTION_TITLE[bucket]} className={cn('space-y-4', bucket !== 'pr' && sections.length > 1 && 'pt-2')}>
            {sections.length > 1 ? (
              <button
                type="button"
                aria-expanded={open}
                onClick={() => setSectionOpen((o) => ({ ...o, [bucket]: !open }))}
                className="flex w-full items-center gap-2 border-b pb-1 text-left text-xs font-semibold tracking-wide text-muted-foreground uppercase hover:text-foreground"
              >
                <ChevronLeftIcon className={cn('size-3.5 transition-transform', open ? '-rotate-90' : 'rotate-180')} />
                {SECTION_TITLE[bucket]} <span className="font-normal text-dimmer">{count}</span>
              </button>
            ) : null}
            {open
              ? groups.map((g) => (
                  <div key={g.name} className={cn('space-y-3', bucket !== 'pr' && sections.length > 1 && 'opacity-90')}>
                    {grouped ? (
                      <h2 className="flex items-center gap-2 px-1 pt-1 text-base font-bold" title={g.views[0].repo.root || undefined}>
                        <FolderGitIcon className="size-4 shrink-0 text-muted-foreground" />
                        <span className="truncate">{g.name}</span>
                      </h2>
                    ) : null}
                    {g.views.map((v) => {
                      const k = repoKey(v.repo)
                      const pr = v.repo.pr
                      return (
                        <div key={k} className="space-y-3">
                          {grouped || v.repo.error ? <EntryHeader repo={v.repo} entryKey={k} onSetUp={v.repo.source === 'pr' && pr && source.prs ? () => source.prs!.setUp(pr) : undefined} /> : null}
                          {!v.files.length && !v.repo.error ? <p className="px-1 text-xs text-dimmer">All changed files here are filtered out.</p> : null}
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
                        </div>
                      )
                    })}
                  </div>
                ))
              : null}
          </section>
        )
      })}
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
        <div className="fixed z-30 flex gap-1" style={{ left: pick.x, top: pick.y }}>
          <Button
            size="sm"
            className="shadow-lg"
            // Keep the text selection while clicking.
            onMouseDown={(e) => e.preventDefault()}
            onClick={() => commentOnPick()}
          >
            <MessageSquarePlusIcon />
            Comment on {rangeText(pick.sel)}
          </Button>
          <Button
            size="sm"
            variant="outline"
            className="bg-background shadow-lg"
            title="Ask Claude about these lines on the side — the answer stays out of the session"
            onMouseDown={(e) => e.preventDefault()}
            onClick={() => commentOnPick(true)}
          >
            <SparklesIcon />
            Ask
          </Button>
        </div>
      ) : null}
      <SideAsks
        asks={side.asks}
        onClose={(qid) => {
          askFiles.current.delete(qid)
          side.close(qid)
        }}
        onOpen={(a) => {
          const key = askFiles.current.get(a.id)
          if (key) openFile(key)
        }}
      />
    </div>
  )

  const dialog = (
    <Dialog open={submitOpen} onOpenChange={setSubmitOpen}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{source.submit.title}</DialogTitle>
          <DialogDescription>{source.submit.description(toSubmit)}</DialogDescription>
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
          placeholder={source.submit.placeholder}
          className="min-h-24 text-sm"
        />
        <DialogFooter>
          <Button variant="ghost" onClick={() => setSubmitOpen(false)}>
            Cancel
          </Button>
          <Button disabled={!toSubmit || submitting} onClick={() => void submit()}>
            {source.submit.label}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )

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
              {sections.map(({ bucket, groups }) => {
                const open = isOpen(bucket)
                return (
                  <div key={bucket} className="pb-1">
                    {sections.length > 1 ? (
                      <button
                        type="button"
                        aria-expanded={open}
                        onClick={() => setSectionOpen((o) => ({ ...o, [bucket]: !open }))}
                        className="mt-3 flex w-full items-center gap-1 px-1.5 text-left text-[0.6875rem] font-semibold tracking-wide text-muted-foreground uppercase hover:text-foreground"
                      >
                        <ChevronLeftIcon className={cn('size-3 transition-transform', open ? '-rotate-90' : 'rotate-180')} />
                        {SECTION_TITLE[bucket]}
                        <span className="font-normal text-dimmer">{groups.reduce((n, g) => n + g.views.length, 0)}</span>
                      </button>
                    ) : null}
                    {open
                      ? groups.map((g) =>
                          grouped ? (
                            <div key={g.name} className="mt-2 overflow-hidden rounded-lg border bg-card/40">
                              <p className="flex items-center gap-1.5 border-b bg-muted/50 px-2 py-1.5 text-xs font-bold" title={g.views[0].repo.root || undefined}>
                                <FolderGitIcon className="size-3.5 shrink-0 text-muted-foreground" />
                                <span className="truncate">{g.name}</span>
                              </p>
                              {g.views.map((v) => (
                                <div key={repoKey(v.repo)} className="border-b px-0.5 py-1 last:border-b-0">
                                  <EntryRow repo={v.repo} onOpen={() => openEntry(repoKey(v.repo), bucket)} />
                                  <FileTree nodes={v.tree} viewed={isViewed} fresh={isFresh} active={active} onOpen={openFile} />
                                </div>
                              ))}
                            </div>
                          ) : (
                            g.views.map((v) => <FileTree key={repoKey(v.repo)} nodes={v.tree} viewed={isViewed} fresh={isFresh} active={active} onOpen={openFile} />)
                          ),
                        )
                      : null}
                  </div>
                )
              })}
            </aside>
          ) : null}
          {body}
        </div>
      </div>
      {showChat && source.chat ? (
        <aside aria-label="Session" className="flex w-[28rem] shrink-0 border-l xl:w-[32rem]">
          {source.chat}
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


