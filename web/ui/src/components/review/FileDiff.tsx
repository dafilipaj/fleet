import { ChevronDownIcon, ChevronRightIcon, CopyIcon, MessageSquareIcon } from 'lucide-react'
import { Fragment, memo, useMemo, type ReactNode } from 'react'

import type { DiffFile, DiffHunk, DiffLine, ReviewThread } from '@/api/types'
import { PlainView, type ClauseCommenting } from '@/components/plain/PlainView'
import { Button } from '@/components/ui/button'
import { usePersistentState } from '@/hooks/usePersistentState'
import { copyWithToast } from '@/lib/clipboard'
import { basename } from '@/lib/paths'
import { clauseKey, diffPlain } from '@/lib/plain'
import { allClauses, anchorKey, clauseAt, diffBar, fileAnchor, inSelection, lineNo, placeOnClauses, placeThreads, splitRows, unifiedSide, type LineSelection, type Side } from '@/lib/review'
import { cn } from '@/lib/utils'

export type DiffView = 'unified' | 'split'

const STATUS_LABEL: Record<DiffFile['status'], string> = { added: 'A', deleted: 'D', modified: 'M', renamed: 'R', copied: 'C' }
const STATUS_TONE: Record<DiffFile['status'], string> = {
  added: 'text-diff-add',
  deleted: 'text-diff-del',
  modified: 'text-status-waiting',
  renamed: 'text-primary',
  copied: 'text-primary',
}

export function StatusLetter({ status, className }: { status: DiffFile['status']; className?: string }) {
  return (
    <span title={status} className={cn('w-3 shrink-0 text-center font-mono text-[0.6875rem] font-bold', STATUS_TONE[status], className)}>
      {STATUS_LABEL[status]}
    </span>
  )
}

export function DiffStat({ additions, deletions, bar = true }: { additions: number; deletions: number; bar?: boolean }) {
  const green = diffBar(additions, deletions)
  const total = additions + deletions
  return (
    <span className="inline-flex shrink-0 items-center gap-1.5 font-mono text-xs tabular-nums">
      <span className="text-diff-add">+{additions}</span>
      <span className="text-diff-del">−{deletions}</span>
      {bar ? (
        <span aria-hidden className="flex gap-px">
          {[0, 1, 2, 3, 4].map((i) => (
            <span
              key={i}
              className={cn('size-2 rounded-[2px]', !total ? 'bg-muted' : i < green ? 'bg-diff-add' : i < 5 ? 'bg-diff-del' : 'bg-muted')}
            />
          ))}
        </span>
      ) : null}
    </span>
  )
}

const lineBg = (l: DiffLine | null) => (!l ? 'bg-muted/40' : l.t === '+' ? 'bg-diff-add/10' : l.t === '-' ? 'bg-diff-del/10' : '')
const numBg = (l: DiffLine | null) => (!l ? 'bg-muted/40' : l.t === '+' ? 'bg-diff-add/20' : l.t === '-' ? 'bg-diff-del/20' : '')
const NUM = 'w-12 shrink-0 select-none px-2 text-right text-dimmer tabular-nums'
const CODE = 'min-w-0 flex-1 whitespace-pre-wrap break-all pr-3'

function Text({ l }: { l: DiffLine }) {
  return (
    <>
      {l.text || ' '}
      {l.noEol ? <span className="ml-1 text-dimmer" title="No newline at end of file">⏎̸</span> : null}
    </>
  )
}

function HunkHeader({ hunk }: { hunk: DiffHunk }) {
  return (
    <div className="flex bg-primary/5 font-mono text-xs leading-6 text-muted-foreground">
      <span className="truncate px-3">{hunk.header}</span>
    </div>
  )
}

/** What a row needs to know about comments: selection, the gutter click, what renders under it. */
interface Commenting {
  path: string
  selection: LineSelection | null
  onLine: (path: string, side: Side, n: number, extend: boolean) => void
  /** threads (and the composer) shown under the row holding these anchor keys */
  after: (keys: string[]) => ReactNode
}

function Gutter({ l, side, c }: { l: DiffLine | null; side: Side; c: Commenting }) {
  const n = lineNo(l, side)
  // A line commentable on this side: not the other side's change.
  const ok = l != null && n != null && (side === 'new' ? l.t !== '-' : l.t !== '+')
  const selected = ok && inSelection(c.selection, c.path, side, n)
  if (!ok) return <span className={cn(NUM, numBg(l))}>{n ?? ''}</span>
  return (
    <button
      type="button"
      onClick={(e) => c.onLine(c.path, side, n, e.shiftKey)}
      title="Comment on this line (shift-click: a range — or select the code)"
      className={cn(NUM, numBg(l), 'relative cursor-pointer hover:bg-primary/25 hover:text-foreground', selected && 'bg-primary/30 text-foreground')}
    >
      {/* GitHub's "+": shows on the hovered row, says the number is the way in. */}
      <span
        aria-hidden
        className="absolute top-1/2 left-0.5 flex size-4 -translate-y-1/2 items-center justify-center rounded bg-primary text-[0.75rem] leading-none font-bold text-primary-foreground opacity-0 group-hover/row:opacity-100"
      >
        +
      </span>
      {n}
    </button>
  )
}

const keysOf = (l: DiffLine | null, sides: Side[]) =>
  sides.flatMap((side) => {
    const n = lineNo(l, side)
    return l && n != null && (side === 'new' ? l.t !== '-' : l.t !== '+') ? [anchorKey(side, n)] : []
  })

function UnifiedHunk({ hunk, c }: { hunk: DiffHunk; c: Commenting }) {
  return (
    <>
      <HunkHeader hunk={hunk} />
      {hunk.lines.map((l, i) => {
        const sel = inSelection(c.selection, c.path, unifiedSide(l), lineNo(l, unifiedSide(l)))
        return (
          <Fragment key={i}>
            <div data-line="" data-o={l.o ?? undefined} data-n={l.n ?? undefined} data-t={l.t} className={cn('group/row flex font-mono text-xs leading-5', lineBg(l), sel && 'bg-primary/10')}>
              <Gutter l={l} side="old" c={c} />
              <Gutter l={l} side="new" c={c} />
              <span className={cn('w-5 shrink-0 select-none text-center', l.t === '+' ? 'text-diff-add' : l.t === '-' ? 'text-diff-del' : 'text-dimmer')}>
                {l.t === ' ' ? '' : l.t === '-' ? '−' : '+'}
              </span>
              <span className={CODE}>
                <Text l={l} />
              </span>
            </div>
            {c.after(keysOf(l, ['old', 'new']))}
          </Fragment>
        )
      })}
    </>
  )
}

function SplitSide({ l, side, c }: { l: DiffLine | null; side: Side; c: Commenting }) {
  const sel = inSelection(c.selection, c.path, side, lineNo(l, side))
  return (
    <div
      data-line=""
      data-side={side}
      data-o={l?.o ?? undefined}
      data-n={l?.n ?? undefined}
      data-t={l?.t}
      className={cn('group/row flex min-w-0 flex-1 font-mono text-xs leading-5', lineBg(l), sel && 'bg-primary/10')}
    >
      <Gutter l={l} side={side} c={c} />
      <span className={cn('w-5 shrink-0 select-none text-center', l?.t === '+' ? 'text-diff-add' : l?.t === '-' ? 'text-diff-del' : 'text-dimmer')}>
        {l && l.t !== ' ' ? (l.t === '-' ? '−' : '+') : ''}
      </span>
      <span className={CODE}>{l ? <Text l={l} /> : null}</span>
    </div>
  )
}

function SplitHunk({ hunk, c }: { hunk: DiffHunk; c: Commenting }) {
  return (
    <>
      <HunkHeader hunk={hunk} />
      {splitRows(hunk).map((r, i) => (
        <Fragment key={i}>
          <div className="flex divide-x">
            <SplitSide l={r.left} side="old" c={c} />
            <SplitSide l={r.right} side="new" c={c} />
          </div>
          {c.after([...keysOf(r.left, ['old']), ...keysOf(r.right, ['new'])])}
        </Fragment>
      ))}
    </>
  )
}

/** A .plain spec's card body: the module drawn with its clause diff; a clause is a comment target. */
function SpecBody({ file, fileKey, selection, composing, onLine, threads, renderThread, renderComposer }: Pick<FileDiffProps, 'file' | 'fileKey' | 'selection' | 'composing' | 'onLine' | 'threads' | 'renderThread' | 'renderComposer'>) {
  const diff = useMemo(() => diffPlain(file.plain?.old ?? null, file.plain?.new ?? ''), [file.plain])
  const clauses = useMemo(() => allClauses(diff), [diff])
  const placed = useMemo(() => placeOnClauses(file, clauses, threads), [file, clauses, threads])
  const composerKey = composing && selection ? clauseAt(clauses, selection.side, selection.end) : null
  const commenting = useMemo<ClauseCommenting>(
    () => ({
      selected: (c) => !!selection && selection.side === c.side && selection.start <= c.end && selection.end >= c.start,
      onClause: (c, extend) => {
        onLine(fileKey, c.side, c.start, extend)
        if (c.end !== c.start) onLine(fileKey, c.side, c.end, true)
      },
      after: (c) => {
        const key = clauseKey(c)
        const ts = placed.at.get(key) ?? []
        const withComposer = composerKey === key
        if (!ts.length && !withComposer) return null
        return (
          <div className="-ml-3 overflow-hidden rounded-md border bg-background font-sans text-sm">
            {ts.map((t) => (
              <Fragment key={t.id}>{renderThread(t, placed.outdated.has(t.id))}</Fragment>
            ))}
            {withComposer ? renderComposer() : null}
          </div>
        )
      },
    }),
    [selection, onLine, fileKey, placed, composerKey, renderThread, renderComposer],
  )
  return (
    <>
      {placed.unplaced.length ? (
        <div className="border-b">
          <p className="bg-muted/40 px-3 py-1 text-xs text-muted-foreground">Comments on lines before the first clause</p>
          {placed.unplaced.map((t) => (
            <Fragment key={t.id}>{renderThread(t, placed.outdated.has(t.id))}</Fragment>
          ))}
        </div>
      ) : null}
      <div className="px-4 py-4 pl-10 sm:px-8 sm:pl-12">
        <PlainView diff={diff} name={basename(file.path).replace(/\.plain$/i, '')} diffed commenting={commenting} />
      </div>
    </>
  )
}

const parsePlainView = (r: string) => (r === 'spec' || r === 'diff' ? r : undefined)

export interface FileDiffProps {
  /** unique across checkouts (lib/review fileKey): what the callbacks and the selection carry */
  fileKey: string
  file: DiffFile
  view: DiffView
  collapsed: boolean
  viewed: boolean
  /** changed since the last poll that showed it */
  fresh: boolean
  onToggleCollapsed: (path: string) => void
  onToggleViewed: (path: string) => void
  /** this file's selection (null when the selection is on another file) */
  selection: LineSelection | null
  /** the composer is open under the selection */
  composing: boolean
  onLine: (path: string, side: Side, n: number, extend: boolean) => void
  /** this file's threads */
  threads: ReviewThread[]
  renderThread: (thread: ReviewThread, outdated: boolean) => ReactNode
  renderComposer: () => ReactNode
}

/** One file's card: the GitHub "Files changed" header (path, +/−, Viewed) over its hunks. */
export const FileDiff = memo(function FileDiff({
  fileKey,
  file,
  view,
  collapsed,
  viewed,
  fresh,
  onToggleCollapsed,
  onToggleViewed,
  selection,
  composing,
  onLine,
  threads,
  renderThread,
  renderComposer,
}: FileDiffProps) {
  const placed = useMemo(() => placeThreads(file, threads), [file, threads])
  const composerKey = composing && selection ? anchorKey(selection.side, selection.end) : null
  const c: Commenting = {
    path: fileKey,
    selection,
    onLine,
    after: (keys) => {
      const ts = keys.flatMap((k) => placed.at.get(k) ?? [])
      const withComposer = composerKey != null && keys.includes(composerKey)
      if (!ts.length && !withComposer) return null
      return (
        <>
          {ts.map((t) => (
            <Fragment key={t.id}>{renderThread(t, placed.outdated.has(t.id))}</Fragment>
          ))}
          {withComposer ? renderComposer() : null}
        </>
      )
    },
  }
  const Hunk = view === 'split' ? SplitHunk : UnifiedHunk
  const open = !collapsed && !viewed
  // A .plain spec reads as a spec by default; Diff shows its lines.
  const hasSpec = !!file.plain && (file.plain.new != null || file.plain.old != null)
  const [plainView, setPlainView] = usePersistentState<'spec' | 'diff'>('fleet.plainView', 'spec', parsePlainView)
  const spec = hasSpec && plainView === 'spec'
  return (
    <section
      id={fileAnchor(fileKey)}
      data-file-key={fileKey}
      aria-label={file.path}
      className={cn('scroll-mt-3 overflow-hidden rounded-lg border bg-background [content-visibility:auto]', fresh && 'ring-2 ring-primary/40')}
    >
      <header className="sticky top-0 z-10 flex items-center gap-2 border-b bg-card px-2 py-1.5">
        <Button variant="ghost" size="icon" className="size-7" aria-label={open ? 'Collapse' : 'Expand'} aria-expanded={open} onClick={() => onToggleCollapsed(fileKey)}>
          {open ? <ChevronDownIcon /> : <ChevronRightIcon />}
        </Button>
        <StatusLetter status={file.status} />
        <span className="min-w-0 truncate font-mono text-[0.8125rem]" title={file.path}>
          {file.oldPath && file.oldPath !== file.path ? (
            <>
              <span className="text-dimmer">{file.oldPath} → </span>
              {file.path}
            </>
          ) : (
            file.path
          )}
        </span>
        <Button variant="ghost" size="icon" className="size-7 shrink-0 text-dimmer" aria-label="Copy path" title="Copy path" onClick={() => void copyWithToast(file.path)}>
          <CopyIcon className="size-3.5" />
        </Button>
        <span className="flex-1" />
        {hasSpec ? (
          <span role="group" aria-label="Show as" className="flex shrink-0 rounded-md bg-muted p-0.5 text-xs">
            {(['spec', 'diff'] as const).map((v) => (
              <button
                key={v}
                type="button"
                aria-pressed={plainView === v}
                onClick={() => setPlainView(v)}
                title={v === 'spec' ? 'The spec, readable, its changes marked by clause' : 'The changed lines'}
                className={cn('cursor-pointer rounded px-2 py-0.5 capitalize', plainView === v ? 'bg-background font-medium text-foreground shadow-sm' : 'text-muted-foreground hover:text-foreground')}
              >
                {v}
              </button>
            ))}
          </span>
        ) : null}
        {threads.length ? (
          <span className="flex shrink-0 items-center gap-1 text-xs text-muted-foreground" title={`${threads.length} comment thread${threads.length === 1 ? '' : 's'}`}>
            <MessageSquareIcon className="size-3.5" />
            {threads.length}
          </span>
        ) : null}
        <DiffStat additions={file.additions} deletions={file.deletions} />
        <label className={cn('ml-1 flex shrink-0 cursor-pointer items-center gap-1.5 rounded-md border px-2 py-0.5 text-xs', viewed && 'border-primary/40 bg-primary/10')}>
          <input type="checkbox" checked={viewed} onChange={() => onToggleViewed(fileKey)} className="accent-primary" />
          Viewed
        </label>
      </header>
      {open && spec ? (
        <SpecBody
          file={file}
          fileKey={fileKey}
          selection={selection}
          composing={composing}
          onLine={onLine}
          threads={threads}
          renderThread={renderThread}
          renderComposer={renderComposer}
        />
      ) : null}
      {open && !spec && placed.unplaced.length ? (
        <div className="border-b">
          <p className="bg-muted/40 px-3 py-1 text-xs text-muted-foreground">Comments on lines no longer in the diff</p>
          {placed.unplaced.map((t) => (
            <Fragment key={t.id}>{renderThread(t, true)}</Fragment>
          ))}
        </div>
      ) : null}
      {open && !spec ? (
        <div className="overflow-x-auto">
          {file.binary ? (
            <p className="px-4 py-3 text-sm text-muted-foreground">Binary file not shown.</p>
          ) : !file.hunks.length ? (
            <p className="px-4 py-3 text-sm text-muted-foreground">{file.truncated ? 'Diff too large to show.' : 'No content changes (mode or rename only).'}</p>
          ) : (
            file.hunks.map((h, i) => <Hunk key={i} hunk={h} c={c} />)
          )}
          {file.truncated && file.hunks.length ? <p className="border-t px-4 py-2 text-xs text-muted-foreground">Diff cut short — too many lines.</p> : null}
        </div>
      ) : null}
    </section>
  )
})

