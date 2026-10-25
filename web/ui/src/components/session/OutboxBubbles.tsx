import { useState } from 'react'
import { CheckIcon, CircleAlertIcon, Loader2Icon, PencilIcon, RotateCcwIcon, Undo2Icon } from 'lucide-react'

import { useNow } from '@/hooks/useNow'
import { secondsLeft, type OutboxItem } from '@/lib/outbox'
import { cn } from '@/lib/utils'

export interface OutboxBubblesProps {
  items: OutboxItem[]
  /** Desktop wording ("⌫ to cancel"). */
  desktop?: boolean
  onUndo: () => void
  onRetry: (id: number) => void
  onEdit: (id: number) => void
}

const RING_R = 6
const RING_LEN = 2 * Math.PI * RING_R

/** The countdown ring: drains over the send delay (CSS animation; hidden with reduced motion). */
function Ring({ item }: { item: OutboxItem }) {
  const total = Math.max(1, (item.dueAt ?? item.createdAt) - item.createdAt)
  // Fixed at mount: a changing (negative) delay would make the running animation jump.
  const [elapsed] = useState(() => Math.min(total, Math.max(0, Date.now() - item.createdAt)))
  return (
    <svg viewBox="0 0 16 16" className="size-3.5 shrink-0 -rotate-90 motion-reduce:hidden" aria-hidden>
      <circle cx="8" cy="8" r={RING_R} fill="none" stroke="currentColor" strokeOpacity={0.25} strokeWidth={2} />
      <circle
        cx="8"
        cy="8"
        r={RING_R}
        fill="none"
        stroke="currentColor"
        strokeWidth={2}
        strokeLinecap="round"
        strokeDasharray={RING_LEN}
        style={{ ['--ring-len' as string]: `${RING_LEN}`, animation: `fleet-countdown ${total}ms linear -${elapsed}ms forwards` }}
      />
    </svg>
  )
}

function Countdown({ item }: { item: OutboxItem }) {
  const now = useNow(250)
  return <span className="tabular-nums">Sending in {secondsLeft(item.dueAt, now)}s</span>
}

const actionClass =
  'relative inline-flex h-7 items-center gap-1 rounded-full px-2 font-medium text-foreground/80 hover:bg-muted hover:text-foreground after:absolute after:inset-x-0 after:-inset-y-2 [&_svg]:size-3.5'

function Status({ item, desktop, onUndo, onRetry, onEdit }: Omit<OutboxBubblesProps, 'items'> & { item: OutboxItem }) {
  switch (item.state) {
    case 'pending':
      return (
        <>
          <Ring item={item} />
          <Countdown item={item} />
          {desktop ? <span className="text-dimmer">· ⌫ to cancel</span> : null}
          <button type="button" className={actionClass} onClick={onUndo}>
            <Undo2Icon /> Undo
          </button>
        </>
      )
    case 'queued':
    case 'sending':
      return (
        <>
          <Loader2Icon className="size-3 animate-spin motion-reduce:animate-none" aria-hidden />
          <span>Sending…</span>
        </>
      )
    case 'sent':
      return (
        <>
          <CheckIcon className="size-3" aria-hidden />
          <span>Sent</span>
        </>
      )
    case 'failed':
      return (
        <>
          <CircleAlertIcon className="size-3.5 shrink-0 text-destructive" aria-hidden />
          <span className="min-w-0 truncate text-destructive" role="alert">
            Not sent: {item.error || 'request failed'}
          </span>
          <button type="button" className={actionClass} onClick={() => onRetry(item.id)}>
            <RotateCcwIcon /> Retry
          </button>
          <button type="button" className={actionClass} onClick={() => onEdit(item.id)}>
            <PencilIcon /> Edit
          </button>
        </>
      )
  }
}

/**
 * Optimistic user bubbles for messages in the outbox: pending (countdown ring + "Sending in 3s",
 * Backspace / Undo), sending…, sent (until the transcript shows the real one), failed (Retry / Edit).
 */
export function OutboxBubbles({ items, desktop = false, onUndo, onRetry, onEdit }: OutboxBubblesProps) {
  const pending = items.find((it) => it.state === 'pending')
  const announce = pending
    ? `Sending in ${Math.round(((pending.dueAt ?? pending.createdAt) - pending.createdAt) / 1000)} seconds, ${desktop ? 'press Backspace' : 'tap Undo'} to cancel`
    : ''
  const live = (
    <div className="sr-only" aria-live="polite" aria-atomic>
      {announce}
    </div>
  )
  if (!items.length) return live
  return (
    <div className="flex flex-col gap-2.5 pt-2.5">
      {live}
      {items.map((it) => (
        <div key={it.id} className="flex w-full flex-col items-end" data-outbox={it.state}>
          <div
            className={cn(
              'w-full rounded-2xl rounded-br-md border px-3 py-2 whitespace-pre-wrap text-accent-foreground transition-opacity [overflow-wrap:anywhere]',
              it.state === 'failed'
                ? 'border-destructive/50 bg-accent/60'
                : it.state === 'sent'
                  ? 'border-primary/25 bg-accent'
                  : 'border-dashed border-primary/40 bg-accent/60 opacity-80',
            )}
          >
            {it.text}
          </div>
          <div className="flex max-w-full min-w-0 items-center gap-1.5 px-1 pt-0.5 text-[0.6875rem] text-dimmer">
            <Status item={it} desktop={desktop} onUndo={onUndo} onRetry={onRetry} onEdit={onEdit} />
          </div>
        </div>
      ))}
    </div>
  )
}
