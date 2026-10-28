import type { DragEvent } from 'react'
import { CircleCheckBigIcon } from 'lucide-react'
import { Link } from 'wouter'

import type { Session } from '@/api/types'
import { ContextMeter } from '@/components/ContextMeter'
import { EditableTitle } from '@/components/EditableTitle'
import { GhIdentityChip } from '@/components/GhIdentityChip'
import { BackgroundChip } from '@/components/BackgroundWork'
import { SlackChip } from '@/components/SlackLink'
import { HostBadge } from '@/components/HostBadge'
import { EmbedChip, JobChip } from '@/components/JobChip'
import { PrChip } from '@/components/PrChip'
import { TaskChip } from '@/components/TaskChip'
import { StatusDot } from '@/components/StatusDot'
import { useLongPress } from '@/hooks/useLongPress'
import { openTitleEditor, useEditing, useSessionTitle } from '@/hooks/useTitles'
import { relTime, sessionSubtitle } from '@/lib/format'
import { sessionDone, sessionHref, statusLabel, statusMeta } from '@/lib/sessions'
import { sessionKey } from '@/lib/shortcuts'
import { STATUS_TEXT } from '@/lib/styles'
import { echoesTitle } from '@/lib/title'
import { cn } from '@/lib/utils'

export interface BoardCardProps {
  session: Session
  now: number
  selected?: boolean
  cursor?: boolean
  /** Desktop: open through the shell (cursor + focus handling) instead of plain navigation. */
  onOpen?: (s: Session) => void
  /** Board drag'n'drop: the card can be dragged to another column. */
  drag?: { dragging: boolean; onStart: (e: DragEvent) => void; onEnd: () => void }
}

/** One session on the desktop board: status, title (editable in place, see SessionRow), host, subtitle (a done session: its note), status / waiting_for / done, context, age. */
export function BoardCard({ session: s, now, selected, cursor, onOpen, drag }: BoardCardProps) {
  const meta = statusMeta(s.status)
  const { title } = useSessionTitle(s)
  // Selecting text in the title field must not drag the card.
  const editing = useEditing('card', sessionKey(s))
  const prompt = sessionSubtitle(s)
  // The first prompt, unless the title is just its slug (no generated title yet).
  const subtitle = echoesTitle(title, prompt) ? '' : prompt
  const waiting = meta.key === 'waiting'
  const done = sessionDone(s)
  const longPress = useLongPress(() => openTitleEditor('card', sessionKey(s)))
  return (
    <Link
      href={sessionHref(s)}
      {...longPress}
      draggable={drag && !editing ? true : undefined}
      onDragStart={editing ? undefined : drag?.onStart}
      onDragEnd={drag?.onEnd}
      onClick={(e) => {
        if (!onOpen || e.metaKey || e.ctrlKey || e.shiftKey || e.button !== 0) return
        e.preventDefault()
        onOpen(s)
      }}
      aria-current={selected ? 'page' : undefined}
      data-session-key={sessionKey(s)}
      className={cn(
        'group/row block rounded-lg border bg-card px-3 py-2.5 text-card-foreground transition-colors [-webkit-touch-callout:none]',
        'outline-none hover:bg-muted/60 focus-visible:ring-3 focus-visible:ring-ring/50 active:bg-muted',
        waiting ? 'border-status-waiting/35' : done ? 'border-primary/30 bg-card/60' : 'border-border/70',
        selected && 'border-primary/60 bg-accent/60 hover:bg-accent/70',
        cursor && 'border-primary/40 ring-2 ring-primary/40',
        drag?.dragging && 'opacity-40',
      )}
    >
      <div className="flex min-w-0 items-center gap-2">
        {done ? <CircleCheckBigIcon className="size-3 shrink-0 text-primary" aria-hidden /> : <StatusDot status={s.status} className="size-2" />}
        <EditableTitle session={s} scope="card" className="text-sm font-semibold" />
        <HostBadge host={s.host} className="h-4.5 px-1 text-[0.625rem]" />
      </div>
      {done?.note ? (
        <p className="mt-1 line-clamp-2 text-xs break-words text-foreground/80">{done.note}</p>
      ) : subtitle ? (
        <p className="mt-1 line-clamp-2 text-xs break-words text-muted-foreground">{subtitle}</p>
      ) : null}
      <div className="mt-1.5 flex min-w-0 items-center gap-2 text-[0.6875rem] text-dimmer">
        <BackgroundChip session={s} />
        <PrChip session={s} />
        <TaskChip session={s} />
        <JobChip session={s} />
        <EmbedChip session={s} />
        <GhIdentityChip session={s} />
        <SlackChip session={s} />
        <span className={cn('min-w-0 truncate', done ? 'font-medium text-primary' : STATUS_TEXT[meta.key])}>{done ? 'done' : statusLabel(s)}</span>
        <span className="ml-auto flex shrink-0 items-center gap-2">
          <ContextMeter context={s.context} />
          <span className="tabular-nums">{relTime(s.updated_at, now)}</span>
        </span>
      </div>
    </Link>
  )
}
