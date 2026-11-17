import type { ReactNode } from 'react'
import { ChevronDownIcon, ChevronRightIcon, LayersIcon } from 'lucide-react'

import type { Session } from '@/api/types'
import { StatusDot } from '@/components/StatusDot'
import { usePersistentState } from '@/hooks/usePersistentState'
import { useStackLabel } from '@/hooks/useStackLabels'
import { openStackSheet } from '@/hooks/useStackUi'
import { isStackLead, statusLabel } from '@/lib/sessions'
import { stackHome } from '@/lib/stacks'
import { sessionTitle } from '@/lib/title'
import { cn } from '@/lib/utils'

/**
 * The adjacent members of one stack in the session list, framed together under a header (layers
 * icon, label, member count) that opens the Stack sheet. The first row is the stack's lead
 * (orchestrator) when it has one — it always comes first (lib/sessions.ts `clusterByStack`) and
 * its row carries a Lead chip — else the most recently active member. The other members hang below it,
 * indented on a guide line, behind a toggle ("N siblings", their status dots): collapsed by
 * default, remembered per stack, and opened while one of them is selected / under the cursor.
 * The rows inside drop their stack chip.
 */
export function StackGroup({
  stackKey,
  sessions,
  renderRow,
  forceOpen = false,
}: {
  stackKey: string
  sessions: Session[]
  renderRow: (s: Session) => ReactNode
  /** Keep it expanded (a hidden member is selected or under the keyboard cursor). */
  forceOpen?: boolean
}) {
  const first = sessions[0]
  const home = stackHome(first)
  const id = first.stack?.id ?? ''
  const { label } = useStackLabel(home, id, first.stack?.label || id)
  const [expanded, setExpanded] = usePersistentState(`fleet.stackOpen.${stackKey}`, false, (raw) => raw === 'true')
  const open = expanded || forceOpen
  const led = isStackLead(first)
  const rest = sessions.slice(1)
  const noun = led ? (rest.length === 1 ? 'sibling' : 'siblings') : 'more'
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
        <span className="shrink-0 text-dimmer tabular-nums">· {sessions.length}</span>
      </button>
      {renderRow(first)}
      {rest.length ? (
        <>
          <button
            type="button"
            aria-expanded={open}
            disabled={forceOpen}
            onClick={() => setExpanded(!expanded)}
            className="mx-1 flex items-center gap-1.5 self-start rounded-md px-1.5 py-0.5 text-[0.6875rem] text-muted-foreground outline-none transition-colors hover:bg-primary/10 hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring/50 disabled:opacity-60"
          >
            {open ? <ChevronDownIcon className="size-3.5" /> : <ChevronRightIcon className="size-3.5" />}
            <span className="tabular-nums">
              {rest.length} {noun}
            </span>
            {open ? null : (
              <span className="flex items-center gap-1">
                {rest.map((s) => (
                  <StatusDot key={`${s.host}/${s.session_id}`} status={s.status} title={`${sessionTitle(s)} — ${statusLabel(s)}`} className="size-2" />
                ))}
              </span>
            )}
          </button>
          {open ? (
            <div className={cn('mb-0.5 ml-3 flex flex-col gap-1 border-l-2 pl-2', led ? 'border-primary/30' : 'border-border')}>{rest.map(renderRow)}</div>
          ) : null}
        </>
      ) : null}
    </div>
  )
}
