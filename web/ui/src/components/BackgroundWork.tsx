import { BotIcon, TerminalIcon } from 'lucide-react'

import type { Session } from '@/api/types'
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip'
import { useNow } from '@/hooks/useNow'
import { relTime } from '@/lib/format'
import { backgroundLabel } from '@/lib/sessions'
import { cn } from '@/lib/utils'

/**
 * What a session runs beside its main thread (`background`, core/background.rs): its subagents
 * and background shells. Nothing when none. Not a link — rows and cards are links already.
 */
export function BackgroundChip({ session: s, className }: { session: Pick<Session, 'background'>; className?: string }) {
  const label = backgroundLabel(s.background)
  if (!label) return null
  const agents = s.background?.agents ?? []
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <span
          aria-label={`Runs ${label}`}
          className={cn('inline-flex h-4.5 shrink-0 items-center gap-1 rounded-[5px] border border-status-busy/40 px-1 text-[0.6875rem] text-status-busy tabular-nums', className)}
        >
          {agents.length ? <BotIcon className="size-3 shrink-0" /> : <TerminalIcon className="size-3 shrink-0" />}
          {label}
        </span>
      </TooltipTrigger>
      <TooltipContent>{agents.length ? agents.map((a) => a.description || a.type || 'agent').join(' · ') : 'Background shells running'}</TooltipContent>
    </Tooltip>
  )
}

/** Details → Background: each subagent (what, which type and model, how long, its last step) and the shells. */
export function BackgroundSection({ session: s }: { session: Pick<Session, 'background'> | null }) {
  const now = useNow(15 * 1000)
  const bg = s?.background
  if (!bg || (!bg.agents?.length && !bg.shells)) return null
  return (
    <section className="py-1">
      <h3 className="pt-2 pb-1 text-[0.6875rem] font-semibold tracking-wider text-dimmer uppercase">Background</h3>
      <div className="divide-y divide-border/60">
        {bg.agents.map((a) => (
          <div key={a.id || `${a.description}-${a.started_at}`} className="flex min-h-12 items-start gap-2 py-1.5">
            <BotIcon className="mt-0.5 size-3.5 shrink-0 text-status-busy" />
            <div className="min-w-0 flex-1">
              <div className="truncate text-sm">{a.description || 'Subagent'}</div>
              <div className="truncate text-xs text-dimmer">
                {[
                  a.background ? null : 'foreground',
                  a.type,
                  a.model?.replace(/^claude-/, ''),
                  a.started_at ? `${relTime(a.started_at, now)} running` : null,
                  a.last_activity_at ? `active ${relTime(a.last_activity_at, now)} ago` : null,
                ]
                  .filter(Boolean)
                  .join(' · ')}
              </div>
              {a.activity ? <div className="truncate font-mono text-xs text-muted-foreground">{a.activity}</div> : null}
            </div>
          </div>
        ))}
        {bg.shells ? (
          <div className="flex min-h-10 items-center gap-2 py-1.5 text-sm">
            <TerminalIcon className="size-3.5 shrink-0 text-status-busy" />
            {bg.shells} background shell{bg.shells === 1 ? '' : 's'}
          </div>
        ) : null}
      </div>
    </section>
  )
}
