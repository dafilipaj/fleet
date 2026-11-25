import { CheckIcon, ChevronsUpDownIcon, CrownIcon } from 'lucide-react'
import { useLocation } from 'wouter'

import type { Session } from '@/api/types'
import { StatusDot } from '@/components/StatusDot'
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from '@/components/ui/dropdown-menu'
import { isStackLead, sessionHref, statusLabel } from '@/lib/sessions'
import { sessionTitle } from '@/lib/title'
import { cn } from '@/lib/utils'

const same = (a: Session, b: Session) => a.host === b.host && a.session_id === b.session_id

/**
 * The stack bar's member switcher: a select-like button with the current member, its menu one item
 * per live member (in `members` order, which the caller keeps stable) that opens it — without
 * opening the stack in the session list.
 */
export function StackSwitcher({ members, current, pane = false }: { members: Session[]; current: Session; pane?: boolean }) {
  const [, navigate] = useLocation()
  const label = (s: Session, active = false) => (
    <>
      <StatusDot status={s.status} className="size-2" />
      {isStackLead(s) ? <CrownIcon aria-label="Lead" className="size-3 shrink-0 fill-current text-yellow-600 dark:text-yellow-300" /> : null}
      <span className={cn('min-w-0 truncate', active && 'font-medium')}>{sessionTitle(s)}</span>
    </>
  )
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <button
          type="button"
          aria-label={`Switch stack member (${members.length} live)`}
          title="Switch to another session of this stack"
          className={cn(
            'flex min-w-0 max-w-[50%] shrink-0 items-center sm:max-w-56 gap-1.5 rounded-md border bg-background/60 px-2 text-xs text-foreground outline-none transition-colors hover:bg-primary/10 focus-visible:ring-2 focus-visible:ring-ring/50',
            pane ? 'h-7' : 'h-8',
          )}
        >
          {label(current)}
          <span className="shrink-0 text-dimmer tabular-nums">
            {members.findIndex((m) => same(m, current)) + 1}/{members.length}
          </span>
          <ChevronsUpDownIcon className="size-3.5 shrink-0 text-dimmer" />
        </button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="max-w-80 min-w-56">
        {members.map((m) => {
          const active = same(m, current)
          return (
            <DropdownMenuItem key={`${m.host}/${m.session_id}`} title={`${sessionTitle(m)} — ${statusLabel(m)}`} onSelect={() => !active && navigate(sessionHref(m))}>
              {label(m, active)}
              {active ? <CheckIcon className="ml-auto size-3.5 shrink-0" /> : null}
            </DropdownMenuItem>
          )
        })}
      </DropdownMenuContent>
    </DropdownMenu>
  )
}
