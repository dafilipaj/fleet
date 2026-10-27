import { AppWindowIcon, BotIcon, MessagesSquareIcon } from 'lucide-react'

import type { Session } from '@/api/types'
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip'
import type { StatusFilter } from '@/lib/sessions'
import { cn } from '@/lib/utils'

/**
 * A session started by a bot (`origin`): bot icon + the bot name, the run id in a
 * tooltip. Nothing for a session started by hand. Not a link — rows and cards are links already.
 */
export function JobChip({ session: s, className }: { session: Pick<Session, 'origin'>; className?: string }) {
  const o = s.origin
  if (!o?.job) return null
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <span
          aria-label={`Started by bot ${o.job}, run ${o.run}`}
          className={cn(
            'inline-flex h-4.5 max-w-[40%] min-w-0 shrink items-center gap-1 rounded-[5px] border border-border px-1 text-[0.6875rem] text-muted-foreground',
            className,
          )}
        >
          <BotIcon className="size-3 shrink-0" />
          <span className="min-w-0 truncate">{o.job}</span>
        </span>
      </TooltipTrigger>
      <TooltipContent>
        Bot {o.job} · run {o.run}
      </TooltipContent>
    </Tooltip>
  )
}

/**
 * A session an app's embedded Fleet panel started (`embed`): app icon + the app's name. Nothing
 * for any other session.
 */
export function EmbedChip({ session: s, className }: { session: Pick<Session, 'embed'>; className?: string }) {
  if (!s.embed) return null
  return (
    <span
      title={`Started from ${s.embed}'s Fleet panel`}
      className={cn(
        'inline-flex h-4.5 max-w-[40%] min-w-0 shrink items-center gap-1 rounded-[5px] border border-border px-1 text-[0.6875rem] text-muted-foreground',
        className,
      )}
    >
      <AppWindowIcon className="size-3 shrink-0" aria-hidden />
      <span className="min-w-0 truncate">{s.embed}</span>
    </span>
  )
}

/**
 * A status filter chip's label: the Bots / Slack / Apps filters are just their icon (the chip row is
 * crowded), every other filter its name.
 */
export function StatusFilterLabel({ filter }: { filter: StatusFilter }) {
  const Icon = filter.id === 'scheduled' ? BotIcon : filter.id === 'slack' ? MessagesSquareIcon : filter.id === 'embedded' ? AppWindowIcon : null
  if (!Icon) return <>{filter.label}</>
  return (
    <>
      <Icon className="size-3.5" aria-hidden />
      <span className="sr-only">{filter.label}</span>
    </>
  )
}
