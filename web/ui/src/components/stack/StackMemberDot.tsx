import { Link } from 'wouter'

import type { Session } from '@/api/types'
import { StatusDot } from '@/components/StatusDot'
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip'
import { sessionSubtitle } from '@/lib/format'
import { sessionHref, statusLabel } from '@/lib/sessions'
import { sessionTitle } from '@/lib/title'
import { cn } from '@/lib/utils'

/**
 * A hidden stack member as its status dot in a collapsed stack: a link that opens it (the stack
 * stays collapsed, the member shows under its lead while selected). Hovering peeks at its title,
 * status and first prompt.
 */
export function StackMemberDot({ session: s, className }: { session: Session; className?: string }) {
  const title = sessionTitle(s)
  const prompt = sessionSubtitle(s)
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <Link
          href={sessionHref(s)}
          aria-label={`Open ${title} (${statusLabel(s)})`}
          className={cn('flex size-5 items-center justify-center rounded-full outline-none hover:bg-primary/15 focus-visible:ring-2 focus-visible:ring-ring/50', className)}
        >
          <StatusDot status={s.status} className="size-2" />
        </Link>
      </TooltipTrigger>
      <TooltipContent side="right" className="flex-col items-start gap-0.5">
        <span className="font-medium">{title}</span>
        <span className="opacity-70">{statusLabel(s)}</span>
        {prompt && prompt !== title ? <span className="line-clamp-3 opacity-80">{prompt}</span> : null}
      </TooltipContent>
    </Tooltip>
  )
}
