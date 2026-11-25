import type { ReactNode } from 'react'
import { ChevronDownIcon, ChevronRightIcon, LayersIcon, MoonIcon } from 'lucide-react'

import type { Session } from '@/api/types'
import { StackMemberDot } from '@/components/stack/StackMemberDot'
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip'
import { usePersistentState } from '@/hooks/usePersistentState'
import { useStackLabel } from '@/hooks/useStackLabels'
import { openStackSheet } from '@/hooks/useStackUi'
import type { DormantMember } from '@/lib/dormant'
import { isStackLead } from '@/lib/sessions'
import { stackHome } from '@/lib/stacks'
import { cn } from '@/lib/utils'

/**
 * The adjacent members of one stack in the session list, framed together under a header (layers
 * icon, label, member count) that opens the Stack sheet. The first row is the stack's lead
 * (orchestrator) when it has one — it always comes first (lib/sessions.ts `clusterByStack`) and
 * its row carries a Lead chip — else the most recently active member. The other members hang below it,
 * indented on a guide line, behind a toggle ("N siblings"): collapsed by default and remembered
 * per stack. Collapsed, the hidden members are status dots that open them (a hover peeks), and a
 * `pinned` member (selected / under the cursor) still shows its row under the lead, so the stack
 * never has to open to be worked in and collapses in one click from any member. Inactive members
 * (`dormant`) are part of the stack too: moons while collapsed, dimmed rows with Resume when open.
 * The rows inside drop their stack chip.
 */
export function StackGroup({
  stackKey,
  sessions,
  renderRow,
  pinned = [],
  dormant = [],
  renderDormant,
}: {
  stackKey: string
  sessions: Session[]
  renderRow: (s: Session) => ReactNode
  /** Keys (`host/session_id`) of hidden members that keep their row while collapsed. */
  pinned?: readonly string[]
  /** Members a reboot left inactive (or put to sleep): after the live ones, dimmed, with Resume. */
  dormant?: readonly DormantMember[]
  renderDormant?: (m: DormantMember) => ReactNode
}) {
  const first = sessions[0]
  const home = stackHome(first)
  const id = first.stack?.id ?? ''
  const { label } = useStackLabel(home, id, first.stack?.label || id)
  const [open, setOpen] = usePersistentState(`fleet.stackOpen.${stackKey}`, false, (raw) => raw === 'true')
  const led = isStackLead(first)
  const rest = sessions.slice(1)
  const others = rest.length + dormant.length
  const noun = led ? (others === 1 ? 'sibling' : 'siblings') : 'more'
  const key = (s: Session) => `${s.host}/${s.session_id}`
  const shown = open ? rest : rest.filter((s) => pinned.includes(key(s)))
  const dots = open ? [] : rest.filter((s) => !pinned.includes(key(s)))
  return (
    <div role="group" aria-label={`Session stack ${label}`} className="flex flex-col gap-1 rounded-2xl border border-primary/25 bg-primary/[0.03] p-1">
      <button
        type="button"
        title={`Session stack: ${label} — open its StackBrief`}
        onClick={() => openStackSheet(home, id)}
        className="ml-1 mt-0.5 flex max-w-[calc(100%-0.5rem)] min-w-0 items-center gap-1.5 self-start rounded-md px-1.5 py-0.5 text-left text-[0.6875rem] font-medium text-primary outline-none transition-colors hover:bg-primary/10 focus-visible:ring-2 focus-visible:ring-ring/50"
      >
        <LayersIcon className="size-3 shrink-0" />
        <span className="min-w-0 truncate">{label}</span>
        <span className="shrink-0 text-dimmer tabular-nums">· {sessions.length + dormant.length}</span>
      </button>
      {renderRow(first)}
      {others ? (
        <>
          <div className="mx-1 flex min-w-0 flex-wrap items-center gap-0.5">
            <button
              type="button"
              aria-expanded={open}
              title={open ? 'Collapse the stack' : 'Show all members'}
              onClick={() => setOpen(!open)}
              className="flex items-center gap-1.5 rounded-md px-1.5 py-0.5 text-[0.6875rem] text-muted-foreground outline-none transition-colors hover:bg-primary/10 hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring/50"
            >
              {open ? <ChevronDownIcon className="size-3.5" /> : <ChevronRightIcon className="size-3.5" />}
              <span className="tabular-nums">
                {others} {noun}
              </span>
            </button>
            {dots.map((s) => (
              <StackMemberDot key={key(s)} session={s} />
            ))}
            {open
              ? null
              : dormant.map((m) => (
                  <Tooltip key={`${m.host}/${m.id}`}>
                    <TooltipTrigger asChild>
                      <button
                        type="button"
                        aria-label={`${m.title} (inactive) — show all members`}
                        onClick={() => setOpen(true)}
                        className="flex size-5 items-center justify-center rounded-full text-dimmer outline-none hover:bg-primary/15 focus-visible:ring-2 focus-visible:ring-ring/50"
                      >
                        <MoonIcon className="size-2.5" />
                      </button>
                    </TooltipTrigger>
                    <TooltipContent side="right" className="flex-col items-start gap-0.5">
                      <span className="font-medium">{m.title}</span>
                      <span className="opacity-70">inactive</span>
                    </TooltipContent>
                  </Tooltip>
                ))}
          </div>
          {shown.length || (open && dormant.length && renderDormant) ? (
            <div className={cn('mb-0.5 ml-3 flex flex-col gap-1 border-l-2 pl-2', led ? 'border-primary/30' : 'border-border')}>
              {shown.map(renderRow)}
              {open && renderDormant ? dormant.map(renderDormant) : null}
            </div>
          ) : null}
        </>
      ) : null}
    </div>
  )
}
