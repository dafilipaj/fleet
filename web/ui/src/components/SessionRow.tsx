import { AlarmClockCheckIcon, AlarmClockIcon, CircleCheckBigIcon, CrownIcon } from 'lucide-react'
import type { ReactNode } from 'react'
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
import { StackChip } from '@/components/stack/StackChip'
import { StatusDot } from '@/components/StatusDot'
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip'
import { useLongPress } from '@/hooks/useLongPress'
import { openTitleEditor, useSessionTitle } from '@/hooks/useTitles'
import { relTime, sessionSubtitle, shortCwd } from '@/lib/format'
import { harnessLabel, sessionHarness } from '@/lib/harness'
import { isStackLead, sessionDone, sessionHref, statusLabel, statusMeta } from '@/lib/sessions'
import { sessionKey } from '@/lib/shortcuts'
import { isBack, isSnoozed, snoozeTime } from '@/lib/snooze'
import { echoesTitle } from '@/lib/title'
import { cn } from '@/lib/utils'

const chip = 'inline-flex h-4.5 shrink-0 items-center rounded-[5px] border px-1 text-[0.6875rem] whitespace-nowrap'

/** Each signal chip's own hue, so Back / Lead / Done read at a glance without a label. */
const SIGNAL = {
  back: 'border-sky-500/50 bg-sky-500/15 text-sky-700 dark:text-sky-300',
  lead: 'border-yellow-500/55 bg-yellow-500/15 text-yellow-700 dark:text-yellow-300',
  done: 'border-emerald-500/50 bg-emerald-500/15 text-emerald-700 dark:text-emerald-300',
} as const

/** An icon-only chip (Back, Lead, Done): its explanation in a tooltip and the aria-label. */
function SignalChip({ tone, label, children }: { tone: keyof typeof SIGNAL; label: string; children: ReactNode }) {
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <span role="img" aria-label={label} className={cn(chip, 'w-5 justify-center px-0', SIGNAL[tone])}>
          {children}
        </span>
      </TooltipTrigger>
      <TooltipContent>{label}</TooltipContent>
    </Tooltip>
  )
}

export interface SessionRowProps {
  session: Session
  now: number
  /** Desktop sidebar: this row is the session open in the detail pane. */
  selected?: boolean
  /** Desktop sidebar: the keyboard cursor (↑/↓) is on this row. */
  cursor?: boolean
  /** Shown inside its stack's group (StackGroup): no stack chip of its own. */
  inStack?: boolean
}

/**
 * One session in the list: status dot (its label / waiting_for on hover), title, host, first prompt,
 * stack chip (opens the Stack sheet), Snoozed (until when) chip, the icon-only Back (from a snooze) / Done / Lead chips (each its own hue, explained in a tooltip), the done note in place of the prompt (it ran `fleet done`), job chip (started by a scheduled job), cwd, backend (unless tmux, the default), context, age. The title is editable in place: the pencil on hover
 * (desktop), ⌘E / F2 on the cursor row, or a long press (touch). Clicking the row opens it.
 */
export function SessionRow({ session: s, now, selected, cursor, inStack }: SessionRowProps) {
  const meta = statusMeta(s.status)
  const { title } = useSessionTitle(s)
  const prompt = sessionSubtitle(s)
  // The first prompt, unless the title is just its slug (no generated title yet).
  const subtitle = echoesTitle(title, prompt) ? '' : prompt
  const cwd = shortCwd(s.cwd)
  const backend = String(s.backend || 'unknown')
  const harness = sessionHarness(s)
  const waiting = meta.key === 'waiting'
  const done = sessionDone(s)
  const snoozed = isSnoozed(s, now)
  const back = isBack(s, now)
  const longPress = useLongPress(() => openTitleEditor('row', sessionKey(s)))

  return (
    <Link
      href={sessionHref(s)}
      {...longPress}
      aria-current={selected ? 'page' : undefined}
      data-session-key={selected || cursor ? `${s.host}/${s.session_id}` : undefined}
      data-flip={sessionKey(s)}
      className={cn(
        'group/row block min-h-16 rounded-xl border bg-card px-3.5 py-3 text-card-foreground transition-colors [-webkit-touch-callout:none]',
        'outline-none hover:bg-muted/60 focus-visible:ring-3 focus-visible:ring-ring/50 active:bg-muted',
        waiting ? 'border-status-waiting/35' : back ? 'border-sky-500/50' : done ? 'border-emerald-500/30 bg-card/60' : 'border-border/70',
        selected && 'border-primary/60 bg-accent/60 hover:bg-accent/70',
        cursor && 'border-primary/40 ring-2 ring-primary/40',
      )}
    >
      <div className="flex min-w-0 items-center gap-2">
        {done ? (
          <span role="img" aria-label="done" title="done" className="flex shrink-0">
            <CircleCheckBigIcon className="size-3.5 text-emerald-600 dark:text-emerald-400" aria-hidden />
          </span>
        ) : (
          <StatusDot status={s.status} title={statusLabel(s)} />
        )}
        <EditableTitle session={s} scope="row" className="text-[0.9375rem] font-bold" />
        <HostBadge host={s.host} />
      </div>
      {done?.note ? (
        <p className="mt-1 line-clamp-2 text-[0.8125rem] break-words text-foreground/80">{done.note}</p>
      ) : subtitle ? (
        <p className="mt-1 line-clamp-2 text-[0.8125rem] break-words text-muted-foreground">{subtitle}</p>
      ) : null}
      <div className="mt-1.5 flex min-w-0 items-center gap-1.5 text-[0.6875rem] text-dimmer">
        {back && s.snooze ? (
          <SignalChip tone="back" label={`Back from a snooze (${snoozeTime(s.snooze.until, now)})`}>
            <AlarmClockCheckIcon className="size-3" aria-hidden />
          </SignalChip>
        ) : null}
        {snoozed && s.snooze ? (
          <span className={cn(chip, 'gap-0.5 border-border font-medium text-muted-foreground')} title="Snoozed: it keeps running and comes back to the list then">
            <AlarmClockIcon className="size-3" aria-hidden />
            {snoozeTime(s.snooze.until, now)}
          </span>
        ) : null}
        {done ? (
          <SignalChip tone="done" label="Done: it ran fleet done and waits to be closed">
            <CircleCheckBigIcon className="size-3" aria-hidden />
          </SignalChip>
        ) : null}
        {isStackLead(s) ? (
          <SignalChip tone="lead" label="Lead (orchestrator) of its session stack">
            <CrownIcon className="size-3 fill-current" aria-hidden />
          </SignalChip>
        ) : null}
        {inStack ? null : <StackChip session={s} />}
        <BackgroundChip session={s} />
        <JobChip session={s} />
        <EmbedChip session={s} />
        <GhIdentityChip session={s} />
        <SlackChip session={s} />
        <PrChip session={s} />
        <TaskChip session={s} />
        {cwd ? <span className="min-w-0 truncate font-mono">{cwd}</span> : null}
        {harness !== 'claude' ? (
          <span className={cn(chip, 'border-primary/40 text-primary')} title={s.model ?? undefined}>
            {harnessLabel(harness)}
          </span>
        ) : null}
        {backend !== 'unknown' && backend !== 'tmux' ? <span className={cn(chip, 'border-border')}>{backend}</span> : null}
        <span className="ml-auto flex shrink-0 items-center gap-2">
          <ContextMeter context={s.context} />
          <span className="tabular-nums">{relTime(s.updated_at, now)}</span>
        </span>
      </div>
    </Link>
  )
}
