import { useEffect, useRef } from 'react'
import { NotebookTextIcon } from 'lucide-react'

import { ClosedSection } from '@/components/ClosedSection'
import { DormantSection } from '@/components/DormantSection'
import { SessionListSkeleton } from '@/components/SessionListSkeleton'
import type { Session } from '@/api/types'
import { SessionRow } from '@/components/SessionRow'
import { StackGroup } from '@/components/stack/StackGroup'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { useNotesHosts } from '@/hooks/useNotes'
import { useFlip } from '@/hooks/useFlip'
import type { SessionListState } from '@/hooks/useSessionList'
import { allSessions, stackRuns } from '@/lib/sessions'
import { sessionKey, withHint } from '@/lib/shortcuts'
import { cn } from '@/lib/utils'

export interface SidebarProps {
  list: SessionListState
  now: number
  cursorKey: string | null
  selectedKey: string | null
}

/** Desktop left column under the top bar (TopBar): the session list, dormant and recently closed. */
export function Sidebar({ list, now, cursorKey, selectedKey }: SidebarProps) {
  const { fleet, error, query, host, view, unreachable, setFrozen } = list
  const listRef = useRef<HTMLDivElement>(null)
  useFlip(listRef, view.sessions.map(sessionKey).join(' '))

  // Keep the cursor row visible (j/k past the edge).
  useEffect(() => {
    if (!cursorKey) return
    const el = listRef.current?.querySelector<HTMLElement>(`[data-session-key="${CSS.escape(cursorKey)}"]`)
    el?.scrollIntoView({ block: 'nearest' })
  }, [cursorKey])

  return (
    <div className="flex h-full min-w-0 flex-col">
      <div
        ref={listRef}
        data-session-list
        onPointerEnter={(e) => e.pointerType === 'mouse' && setFrozen(true)}
        onPointerLeave={() => setFrozen(false)}
        className="min-h-0 flex-1 overflow-y-auto overscroll-contain px-2 py-2"
      >
        {error ? (
          <Alert variant="destructive" className="mb-2">
            <AlertDescription>
              {fleet ? `Refresh failed: ${error} — showing last known data` : `Could not reach fleet web: ${error}`}
            </AlertDescription>
          </Alert>
        ) : null}
        {unreachable.map((h) => (
          <Alert key={h.name} variant="destructive" className="mb-2">
            <AlertDescription className="break-words">
              {h.name} unreachable: {h.error || 'no response'}
            </AlertDescription>
          </Alert>
        ))}
        {!fleet ? (
          <SessionListSkeleton />
        ) : view.sessions.length > 0 ? (
          <nav aria-label="Sessions" className="flex flex-col gap-1.5">
            {stackRuns(view.sessions).map((e) => {
              const row = (s: Session, inStack?: boolean) => {
                const key = sessionKey(s)
                return <SessionRow key={key} session={s} now={now} selected={key === selectedKey} cursor={key === cursorKey && key !== selectedKey} inStack={inStack} />
              }
              return e.kind === 'stack' ? (
                <StackGroup
                  key={e.key}
                  stackKey={e.key}
                  sessions={e.sessions}
                  renderRow={(s) => row(s, true)}
                  forceOpen={e.sessions.slice(1).some((s) => [selectedKey, cursorKey].includes(sessionKey(s)))}
                />
              ) : (
                row(e.session)
              )
            })}
          </nav>
        ) : unreachable.length === 0 ? (
          <div className="px-4 py-10 text-center text-sm text-dimmer">
            {allSessions(fleet).length > 0 ? 'Nothing matches.' : 'No Claude sessions running.'}
          </div>
        ) : null}
        {fleet ? <DormantSection now={now} host={host} query={query} desktop className="mt-4" /> : null}
        {fleet ? <ClosedSection now={now} host={host} query={query} desktop className="mt-3" /> : null}
      </div>

    </div>
  )
}

/** Link to the notes explorer; nothing when no host has notes. */
export function NotesLink({ className, size = 'icon' }: { className?: string; size?: 'icon' | 'icon-sm' }) {
  const hosts = useNotesHosts()
  if (!hosts.length) return null
  return (
    <Button asChild variant="ghost" size={size} className={cn('text-dimmer hover:text-foreground', className)}>
      <a href="#/notes" aria-label="Notes" title={withHint('Notes', 'notes')}>
        <NotebookTextIcon />
      </a>
    </Button>
  )
}

