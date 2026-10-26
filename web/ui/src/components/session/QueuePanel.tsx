import { ArrowUpIcon, ListOrderedIcon, PauseIcon, PencilIcon, PlayIcon, SendHorizontalIcon, XIcon } from 'lucide-react'

import type { PromptQueue } from '@/api/types'
import { cn } from '@/lib/utils'

export interface QueuePanelProps {
  queue: PromptQueue | null
  /** Who answers ("Claude", "Codex"…), for the status line. */
  agent?: string
  desktop?: boolean
  onSendNow: (itemId: string) => void
  /** Back into the composer (and out of the queue). */
  onEdit: (itemId: string) => void
  onDelete: (itemId: string) => void
  onMoveUp: (itemId: string) => void
  onPause: (paused: boolean) => void
}

const iconBtn =
  'relative inline-flex size-7 shrink-0 items-center justify-center rounded-full text-muted-foreground hover:bg-muted hover:text-foreground after:absolute after:-inset-1 [&_svg]:size-3.5'

/**
 * The prompts waiting for the run to end (lib/queue.mjs on the server): one goes in per run, once
 * the session is idle — never while it asks you something. Pause holds them for this run.
 */
export function QueuePanel({ queue, agent = 'Claude', desktop = false, onSendNow, onEdit, onDelete, onMoveUp, onPause }: QueuePanelProps) {
  if (!queue?.items.length) return null
  const { items, paused, held } = queue
  const status = paused
    ? `Paused — holds when ${agent} finishes this run`
    : held
      ? 'Held — goes in after your next run'
      : `Sends when ${agent} finishes${items.length > 1 ? ', one per run' : ''}`
  return (
    <section aria-label="Queued messages" className="shrink-0 px-3 px-safe">
      <div className={cn('mx-auto mb-2 rounded-xl border border-dashed border-primary/40 bg-accent/40', desktop ? 'max-w-4xl' : 'max-w-3xl')}>
        <div className="flex items-center gap-2 px-3 pt-1.5 text-xs text-muted-foreground">
          <ListOrderedIcon className="size-3.5 shrink-0" aria-hidden />
          <span className="min-w-0 flex-1 truncate" role="status">
            <span className="font-medium text-foreground">Queued {items.length}</span> · {status}
          </span>
          <button
            type="button"
            className={cn(iconBtn, 'w-auto gap-1 px-2 font-medium', (paused || held) && 'text-foreground')}
            aria-pressed={paused}
            title={paused || held ? 'Resume: the next one goes in as soon as the session is idle' : 'Hold the queue when this run ends (clears itself then)'}
            onClick={() => onPause(!(paused || held))}
          >
            {paused || held ? <PlayIcon /> : <PauseIcon />}
            {paused || held ? 'Resume' : 'Pause'}
          </button>
        </div>
        <ol className="flex flex-col gap-0.5 px-1.5 pt-0.5 pb-1.5">
          {items.map((it, i) => (
            <li key={it.id} className="group flex min-w-0 items-start gap-1 rounded-lg px-1.5 py-1 hover:bg-background/60">
              <span className="mt-1 w-4 shrink-0 text-right text-[0.6875rem] text-dimmer tabular-nums">{i + 1}.</span>
              <p className="line-clamp-2 min-w-0 flex-1 pt-0.5 text-sm whitespace-pre-wrap [overflow-wrap:anywhere]" title={it.text}>
                {it.text}
              </p>
              {i > 0 ? (
                <button type="button" className={iconBtn} aria-label="Move up" title="Move up" onClick={() => onMoveUp(it.id)}>
                  <ArrowUpIcon />
                </button>
              ) : null}
              <button type="button" className={iconBtn} aria-label="Edit" title="Edit (back into the composer)" onClick={() => onEdit(it.id)}>
                <PencilIcon />
              </button>
              <button type="button" className={iconBtn} aria-label="Send now" title="Send now (mid-run: Claude reads it after the current step)" onClick={() => onSendNow(it.id)}>
                <SendHorizontalIcon />
              </button>
              <button type="button" className={iconBtn} aria-label="Remove" title="Remove" onClick={() => onDelete(it.id)}>
                <XIcon />
              </button>
            </li>
          ))}
        </ol>
      </div>
    </section>
  )
}
