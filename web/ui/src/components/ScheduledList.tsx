import { useState } from 'react'
import { CalendarClockIcon, Loader2Icon, XIcon } from 'lucide-react'
import { toast } from 'sonner'

import { api } from '@/api/client'
import { HostDot } from '@/components/HostBadge'
import { Button } from '@/components/ui/button'
import { shortCwd } from '@/lib/format'
import { type HostedEntry, scheduleWhat, scheduleWhen } from '@/lib/schedule'
import { cn } from '@/lib/utils'

/** Pending scheduled sessions (`fleet schedule`), each with its host, when, what and Cancel. */
export function ScheduledList({ entries, now, onChanged, className }: { entries: HostedEntry[]; now: number; onChanged: () => void; className?: string }) {
  const [busy, setBusy] = useState<string | null>(null)
  if (!entries.length) return null
  const cancel = async (e: HostedEntry) => {
    setBusy(e.id)
    try {
      await api.cancelScheduled(e.host, e.id)
      toast.success(`Cancelled — ${scheduleWhat(e)}`)
      onChanged()
    } catch (err) {
      toast.error('Could not cancel', { description: (err as Error)?.message })
    } finally {
      setBusy(null)
    }
  }
  return (
    <ul className={cn('divide-y rounded-xl border bg-card', className)}>
      {entries.map((e) => (
        <li key={`${e.host}/${e.id}`} className="flex min-w-0 items-center gap-2.5 py-1.5 pr-1 pl-3">
          <CalendarClockIcon className="size-4 shrink-0 text-muted-foreground" />
          <div className="min-w-0 flex-1">
            <div className="truncate text-sm">
              {e.task ? (
                <a href={e.task.url} target="_blank" rel="noreferrer" className="hover:underline">
                  {scheduleWhat(e)}
                </a>
              ) : (
                scheduleWhat(e)
              )}
            </div>
            <div className="flex min-w-0 items-center gap-1.5 text-xs text-muted-foreground">
              <HostDot host={e.host} />
              <span className="shrink-0 font-medium">{scheduleWhen(e, now)}</span>
              <span className="min-w-0 truncate text-dimmer">
                {e.host} · {shortCwd(e.dir)}
                {e.task && e.prompt ? ` · ${e.prompt}` : ''}
              </span>
            </div>
          </div>
          <Button
            variant="ghost"
            size="icon"
            aria-label={`Cancel ${scheduleWhat(e)}`}
            title="Cancel"
            disabled={busy === e.id}
            onClick={() => void cancel(e)}
            className="size-9 shrink-0 rounded-lg text-muted-foreground"
          >
            {busy === e.id ? <Loader2Icon className="animate-spin" /> : <XIcon />}
          </Button>
        </li>
      ))}
    </ul>
  )
}
