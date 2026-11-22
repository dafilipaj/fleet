import { MessagesSquareIcon } from 'lucide-react'

import type { Session } from '@/api/types'
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip'
import { useNow } from '@/hooks/useNow'
import { slackLatest, slackLeft, useSessionSlackLinks } from '@/hooks/useSlack'
import { cn } from '@/lib/utils'

/**
 * A session linked to Slack (web/lib/slack.mjs): the time left. Nothing for an unlinked one. Not a
 * link — rows and cards are links already.
 */
export function SlackChip({ session: s, className }: { session: Pick<Session, 'host' | 'session_id'>; className?: string }) {
  const links = useSessionSlackLinks(s.host, s.session_id)
  const now = useNow(30 * 1000)
  if (!links.length) return null
  const until = slackLatest(links.map((l) => l.until))
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <span
          aria-label={until == null ? 'Linked to Slack, no end' : `Linked to Slack for ${slackLeft(until, now)}`}
          className={cn(
            'inline-flex h-4.5 shrink-0 items-center gap-1 rounded-[5px] border border-primary/40 px-1 text-[0.6875rem] text-primary tabular-nums',
            className,
          )}
        >
          <MessagesSquareIcon className="size-3 shrink-0" />
          {slackLeft(until, now)}
        </span>
      </TooltipTrigger>
      <TooltipContent>People in a Slack thread can talk to this session ({links.length > 1 ? `${links.length} links` : links[0].permalink})</TooltipContent>
    </Tooltip>
  )
}
