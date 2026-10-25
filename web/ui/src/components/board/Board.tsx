import { useEffect, useRef } from 'react'

import type { Session } from '@/api/types'
import { BoardColumns } from '@/components/board/BoardColumns'
import { DormantSection } from '@/components/DormantSection'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Skeleton } from '@/components/ui/skeleton'
import type { GroupsState } from '@/hooks/useGroups'
import type { SessionListState } from '@/hooks/useSessionList'
import type { BoardColumn } from '@/lib/groups'
import { allSessions } from '@/lib/sessions'
import { sessionKey } from '@/lib/shortcuts'

export interface BoardProps {
  list: SessionListState
  groups: GroupsState
  columns: BoardColumn[]
  /** Reorder: column `id` just before `before` (null: to the end) — a header drag. */
  onMoveColumn: (id: string, before: string | null) => void
  now: number
  cursorKey: string | null
  selectedKey: string | null
  /**
   * Width (px) of the session flyout over the board's right edge, 0 when none. The columns keep
   * their size and place; the board can scroll the last ones out from under it, and the cursor
   * card is scrolled clear of it.
   */
  overlayInset: number
  onOpen: (s: Session) => void
}

/**
 * Desktop board, under the top bar (TopBar): horizontal Kanban columns — one per group, cards =
 * sessions. The same search / status / host filters as the list apply to the cards. Columns can be renamed,
 * reordered and cards dragged between them (BoardColumns).
 */
export function Board({ list, groups, columns, onMoveColumn, now, cursorKey, selectedKey, overlayInset, onOpen }: BoardProps) {
  const { fleet, error, query, host, unreachable } = list
  const boardRef = useRef<HTMLDivElement>(null)

  // The cursor card scrolls into view, clear of the flyout (↑/↓, ⌘K). A click moves the cursor
  // too, but the clicked card is on screen already: it stays put even if the flyout covers it.
  const clickedRef = useRef<string | null>(null)
  const openCard = (s: Session) => {
    clickedRef.current = sessionKey(s)
    onOpen(s)
  }
  useEffect(() => {
    const board = boardRef.current
    const el = cursorKey ? board?.querySelector<HTMLElement>(`[data-session-key="${CSS.escape(cursorKey)}"]`) : null
    const clicked = clickedRef.current === cursorKey
    clickedRef.current = null
    if (!board || !el) return
    if (clicked) {
      const r = el.getBoundingClientRect()
      const b = board.getBoundingClientRect()
      const col = (el.parentElement ?? board).getBoundingClientRect()
      if (r.left >= b.left && r.right <= b.right && r.top >= col.top && r.bottom <= col.bottom) return
    }
    el.scrollIntoView({ block: 'nearest', inline: 'nearest' })
  }, [cursorKey])

  return (
    <div className="flex h-full min-w-0 flex-col">
      {error || unreachable.length ? (
        <div className="shrink-0 space-y-2 px-4 pt-2">
          {error ? (
            <Alert variant="destructive">
              <AlertDescription>
                {fleet ? `Refresh failed: ${error} — showing last known data` : `Could not reach fleet web: ${error}`}
              </AlertDescription>
            </Alert>
          ) : null}
          {unreachable.map((h) => (
            <Alert key={h.name} variant="destructive">
              <AlertDescription className="break-words">
                {h.name} unreachable: {h.error || 'no response'}
              </AlertDescription>
            </Alert>
          ))}
        </div>
      ) : null}

      <div
        ref={boardRef}
        data-session-list
        className="flex min-h-0 flex-1 items-start gap-3 overflow-x-auto overscroll-x-contain p-3"
        style={overlayInset ? { scrollPaddingRight: overlayInset + 12 } : undefined}
      >
        {!fleet ? (
          [0, 1, 2].map((i) => <Skeleton key={i} className="h-64 w-72 shrink-0 rounded-xl" />)
        ) : columns.length === 0 ? (
          <div className="w-full px-4 py-10 text-center text-sm text-dimmer">
            {allSessions(fleet).length > 0 ? 'Nothing matches.' : 'No Claude sessions running.'}
          </div>
        ) : (
          <BoardColumns
            columns={columns}
            now={now}
            cursorKey={cursorKey}
            selectedKey={selectedKey}
            onOpen={openCard}
            editable={groups.editable}
            onEdit={groups.edit}
            onMoveColumn={onMoveColumn}
          />
        )}
        {fleet ? <DormantSection now={now} host={host} query={query} desktop className="max-h-full w-72 shrink-0 overflow-y-auto rounded-xl border border-dashed p-2" /> : null}
        {fleet && columns.length && overlayInset ? <div aria-hidden className="shrink-0" style={{ width: overlayInset }} /> : null}
      </div>
    </div>
  )
}
