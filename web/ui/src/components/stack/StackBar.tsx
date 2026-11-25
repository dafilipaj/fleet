import { useState } from 'react'
import { FileTextIcon, GitForkIcon, LayersIcon, Loader2Icon } from 'lucide-react'

import type { Session } from '@/api/types'
import { InlineEdit } from '@/components/InlineEdit'
import { StackSwitcher } from '@/components/stack/StackSwitcher'
import { Button } from '@/components/ui/button'
import { useFleet } from '@/hooks/useFleet'
import { STACK_BAR_POLL_MS, useStack, useStackOrder } from '@/hooks/useStack'
import { useRenameStack, useStackLabel } from '@/hooks/useStackLabels'
import { openSiblingSpawn, openStackSheet } from '@/hooks/useStackUi'
import { MAX_STACK_LABEL, memberCountsText, stackHome, stackLabelChanged } from '@/lib/stacks'
import { allSessions, stackMembers } from '@/lib/sessions'
import { cn } from '@/lib/utils'

/**
 * Under the session header when the session is in a stack: layers icon + label, "2 live · 1
 * closed" once the stack loaded, a member switcher (StackSwitcher; lead first, then StackBrief
 * order, remembered so it never reshuffles while switching), StackBrief and Spawn sibling.
 * Hidden when the host's server predates stacks. Clicking the label renames the stack (a pointer); a tap on a touch screen, or a
 * click anywhere else on the bar, opens the Stack sheet.
 */
export function StackBar({ session, pane = false }: { session: Session; pane?: boolean }) {
  const ref = session.stack
  // Everything about the stack is served by its home host (the session may run elsewhere).
  const home = stackHome(session)
  const st = useStack(home, ref?.id ?? null, !!ref?.id, STACK_BAR_POLL_MS)
  const [editing, setEditing] = useState(false)
  const { label, saving } = useStackLabel(home, ref?.id, st.stack?.label || ref?.label || ref?.id || '')
  const rename = useRenameStack()
  const { fleet } = useFleet()
  const order = useStackOrder(home, ref?.id, st.stack)
  const members = stackMembers(allSessions(fleet), session, order)
  if (!ref?.id || st.missing) return null
  const open = () => openStackSheet(home, ref.id)
  return (
    <div className="shrink-0 border-b bg-muted/30 px-safe">
      <div className={cn('mx-auto flex w-full items-center gap-1.5 py-1 text-xs', pane ? 'px-4' : 'max-w-3xl px-3')}>
        {/* Not a button: the label inside is one (InlineEdit). Keyboard users open the sheet with StackBrief. */}
        <div
          onClick={editing ? undefined : open}
          title={`Session stack: ${label}`}
          className={cn('flex min-w-0 flex-1 cursor-pointer items-center gap-1.5', pane ? 'min-h-9' : '-my-1 min-h-11')}
        >
          <LayersIcon className="size-3.5 shrink-0 text-primary" />
          <InlineEdit
            value={label}
            editing={editing}
            onEdit={saving ? undefined : () => setEditing(true)}
            onCommit={(text) => {
              setEditing(false)
              if (!stackLabelChanged(text, label)) return
              // The bar polls once a minute: take the renamed StackView now.
              void rename(home, ref.id, text).then((view) => view && st.apply(view))
            }}
            onCancel={() => setEditing(false)}
            className={cn('font-medium text-foreground', saving && 'opacity-70')}
            label="Stack name"
            hint="Rename stack"
            maxLength={MAX_STACK_LABEL}
            tapToEdit={false}
          />
          {saving ? <Loader2Icon aria-label="Saving" className="size-3 shrink-0 animate-spin text-dimmer" /> : null}
          {st.stack ? <span className={cn('shrink-0 text-dimmer tabular-nums', !pane && members.length > 1 && 'max-sm:hidden')}>{memberCountsText(st.stack)}</span> : null}
        </div>
        {members.length > 1 ? <StackSwitcher members={members} current={session} pane={pane} /> : null}
        {/* Mobile: icon buttons, so the label keeps its room. */}
        <Button
          variant="ghost"
          size={pane ? 'sm' : 'icon'}
          aria-label="StackBrief"
          title="StackBrief"
          className={pane ? undefined : '-my-1 size-11'}
          onClick={open}
        >
          <FileTextIcon />
          {pane ? 'StackBrief' : null}
        </Button>
        <Button
          variant="ghost"
          size={pane ? 'sm' : 'icon'}
          aria-label="Spawn sibling"
          title="Spawn sibling"
          className={pane ? undefined : '-my-1 size-11'}
          onClick={() =>
            openSiblingSpawn({
              host: home,
              // A member on the home host spawns from itself (its cwd); one elsewhere from the stack.
              ...(home === session.host ? { sessionId: session.session_id } : { stackId: ref.id }),
              cwd: session.cwd ?? null,
              cwdHost: session.host,
              label,
              creates: false,
            })
          }
        >
          <GitForkIcon />
          {pane ? 'Spawn sibling' : null}
        </Button>
      </div>
    </div>
  )
}
