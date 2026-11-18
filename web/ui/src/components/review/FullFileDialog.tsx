import { ChevronDownIcon, ChevronUpIcon } from 'lucide-react'
import { useCallback, useEffect, useMemo, useRef, useState } from 'react'

import type { DiffFile } from '@/api/types'
import { DiffStat, SplitHunk, StatusLetter, UnifiedHunk, type Commenting, type DiffView } from '@/components/review/FileDiff'
import { Button } from '@/components/ui/button'
import { Dialog, DialogContent, DialogDescription, DialogTitle } from '@/components/ui/dialog'
import { Kbd } from '@/components/ui/kbd'
import { changeBlocks } from '@/lib/review'
import { cn } from '@/lib/utils'

/** Commenting off: plain line numbers (comments stay on the cards). */
const READ_ONLY: Commenting = { path: '', selection: null, onLine: () => {}, after: () => null, readOnly: true }

/** The rows in order: a unified row is its line; a split row holds both sides. */
function rowsOf(el: HTMLElement): HTMLElement[] {
  const rows: HTMLElement[] = []
  el.querySelectorAll<HTMLElement>('[data-line]').forEach((d) => {
    const row = d.dataset.side ? d.parentElement! : d
    if (rows[rows.length - 1] !== row) rows.push(row)
  })
  return rows
}
const CHANGED = '[data-t="+"],[data-t="-"]'
const isChanged = (row: HTMLElement) => row.matches(CHANGED) || !!row.querySelector(CHANGED)
/** The first row of each changed block. */
const blockStarts = (el: HTMLElement) => rowsOf(el).filter((r, i, all) => isChanged(r) && (i === 0 || !isChanged(all[i - 1])))

export interface FullFileTarget {
  fileKey: string
  file: DiffFile
}

/**
 * The whole file in a dialog, its changes marked in place (the diff with every line as context).
 * Read-only. A strip on the right marks where the changes sit (click to jump); n / p or the
 * arrows step through them. Opens at the first change.
 */
export function FullFileDialog({ target, view, load, onClose }: { target: FullFileTarget | null; view: DiffView; load: (fileKey: string) => Promise<DiffFile>; onClose: () => void }) {
  const [state, setState] = useState<{ key: string; file?: DiffFile; error?: string } | null>(null)
  const key = target?.fileKey ?? null
  useEffect(() => {
    if (!key) return
    let live = true
    load(key).then(
      (file) => live && setState({ key, file }),
      (err: unknown) => live && setState({ key, error: (err as Error)?.message || 'Could not load the file' }),
    )
    return () => {
      live = false
    }
  }, [key, load])
  const file = state?.key === key ? state.file : undefined
  const error = state?.key === key ? state.error : undefined
  const blocks = useMemo(() => (file ? changeBlocks(file) : []), [file])
  const total = useMemo(() => (file ? file.hunks.reduce((n, h) => n + h.lines.length, 0) : 0), [file])

  const scroller = useRef<HTMLDivElement>(null)
  const [at, setAt] = useState<{ file: DiffFile; i: number } | null>(null)
  const current = at && at.file === file ? at.i : -1
  const go = useCallback(
    (i: number, smooth = true) => {
      const el = scroller.current
      if (!el || !file || !blocks.length) return
      const n = (i + blocks.length) % blocks.length
      const row = blockStarts(el)[n]
      if (!row) return
      el.scrollTo({ top: row.offsetTop - el.clientHeight / 3, behavior: smooth ? 'smooth' : 'instant' })
      setAt({ file, i: n })
    },
    [blocks.length, file],
  )
  // Open at the first change (and again when the view switches).
  useEffect(() => {
    if (!file) return
    const t = requestAnimationFrame(() => go(0, false))
    return () => cancelAnimationFrame(t)
  }, [file, view, go])

  const Hunk = view === 'split' ? SplitHunk : UnifiedHunk
  const shown = file ?? target?.file
  return (
    <Dialog open={!!target} onOpenChange={(o) => !o && onClose()}>
      <DialogContent
        className="flex h-[92dvh] w-[min(96vw,1400px)] max-w-none flex-col gap-0 overflow-hidden p-0 sm:max-w-none"
        onKeyDown={(e) => {
          if (e.metaKey || e.ctrlKey || e.altKey) return
          const step = e.key === 'n' || e.key === 'j' ? 1 : e.key === 'p' || e.key === 'k' ? -1 : 0
          if (!step) return
          e.preventDefault()
          go(current + step)
        }}
      >
        <header className="flex items-center gap-2 border-b bg-card py-2 pr-12 pl-3">
          {shown ? <StatusLetter status={shown.status} /> : null}
          <DialogTitle className="min-w-0 truncate font-mono text-[0.8125rem] font-normal" title={shown?.path}>
            {shown?.oldPath && shown.oldPath !== shown.path ? <span className="text-dimmer">{shown.oldPath} → </span> : null}
            {shown?.path}
          </DialogTitle>
          <DialogDescription className="sr-only">The whole file with its changes marked</DialogDescription>
          <span className="flex-1" />
          {file ? (
            <>
              <span className="shrink-0 text-xs text-muted-foreground tabular-nums max-sm:hidden">
                {blocks.length ? `Change ${current >= 0 ? current + 1 : '–'} of ${blocks.length}` : 'No changes'}
              </span>
              <span className="flex shrink-0 items-center gap-0.5 max-sm:hidden" aria-hidden>
                <Kbd>n</Kbd>
                <Kbd>p</Kbd>
              </span>
              <Button variant="ghost" size="icon" className="size-7" aria-label="Previous change" title="Previous change (p)" disabled={!blocks.length} onClick={() => go(current - 1)}>
                <ChevronUpIcon />
              </Button>
              <Button variant="ghost" size="icon" className="size-7" aria-label="Next change" title="Next change (n)" disabled={!blocks.length} onClick={() => go(current + 1)}>
                <ChevronDownIcon />
              </Button>
              <DiffStat additions={file.additions} deletions={file.deletions} />
            </>
          ) : null}
        </header>
        <div className="flex min-h-0 flex-1">
          <div ref={scroller} className="relative min-w-0 flex-1 overflow-auto">
            {error ? (
              <p className="px-4 py-3 text-sm text-destructive">{error}</p>
            ) : !file ? (
              <p className="animate-pulse px-4 py-3 text-sm text-muted-foreground">Loading the file…</p>
            ) : file.binary ? (
              <p className="px-4 py-3 text-sm text-muted-foreground">Binary file not shown.</p>
            ) : !file.hunks.length ? (
              <p className="px-4 py-3 text-sm text-muted-foreground">No content changes (mode or rename only).</p>
            ) : (
              <>
                {/* One hunk is the whole file; more only from an older server (no `full`): headers between them. */}
                {file.hunks.map((h, i) => (
                  <Hunk key={i} hunk={h} c={READ_ONLY} header={file.hunks.length > 1} />
                ))}
                {file.truncated ? <p className="border-t px-4 py-2 text-xs text-muted-foreground">File cut short — too many lines.</p> : null}
              </>
            )}
          </div>
          {file && total > 0 && blocks.length > 0 ? (
            // Where the changes sit in the file: a mark per block, click to jump.
            <div className="relative w-3 shrink-0 border-l bg-muted/30" aria-hidden>
              {blocks.map((b, i) => (
                <button
                  key={i}
                  type="button"
                  tabIndex={-1}
                  onClick={() => go(i)}
                  className={cn(
                    'absolute inset-x-0.5 min-h-[3px] cursor-pointer rounded-[1px]',
                    b.kind === '+' ? 'bg-diff-add' : b.kind === '-' ? 'bg-diff-del' : 'bg-status-waiting',
                    i === current && 'outline outline-1 outline-foreground',
                  )}
                  style={{ top: `${(b.start / total) * 100}%`, height: `${((b.end - b.start + 1) / total) * 100}%` }}
                />
              ))}
            </div>
          ) : null}
        </div>
      </DialogContent>
    </Dialog>
  )
}
