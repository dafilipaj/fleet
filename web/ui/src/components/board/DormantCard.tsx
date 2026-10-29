import { Loader2Icon, MoonIcon, PlayIcon } from 'lucide-react'

import { HostBadge } from '@/components/HostBadge'
import { Button } from '@/components/ui/button'
import { useDormant } from '@/hooks/useDormant'
import type { DormantMember } from '@/lib/dormant'
import { relTime } from '@/lib/format'
import { cn } from '@/lib/utils'

/**
 * A group member a reboot left dormant, on the Board (and the mobile grouped list): dimmed, with
 * Resume (`fleet restore <session id>` on its host — its whole tmux session comes back).
 */
export function DormantCard({ member: m, now, desktop = false }: { member: DormantMember; now: number; desktop?: boolean }) {
  const { busy, resume } = useDormant()
  const working = busy.has(`${m.host}/${m.id}`) || busy.has(`${m.host}/*`)
  const since = relTime(m.since, now)
  return (
    <div className={cn('flex items-center gap-2 rounded-lg border border-dashed border-border/70 px-3', desktop ? 'py-2' : 'min-h-14 py-2')}>
      <div className="min-w-0 flex-1 opacity-60">
        <div className="flex min-w-0 items-center gap-2">
          <MoonIcon aria-label="inactive" className="size-3 shrink-0 text-dimmer" />
          <span className="min-w-0 truncate text-sm font-semibold">{m.title}</span>
          <HostBadge host={m.host} className="h-4.5 px-1 text-[0.625rem]" />
        </div>
        <p className="mt-0.5 text-[0.6875rem] text-dimmer">{since ? `inactive · down ${since}` : 'inactive'}</p>
      </div>
      <Button
        type="button"
        variant="outline"
        size={desktop ? 'sm' : 'lg'}
        disabled={working}
        onClick={() => void resume(m.host, m.id)}
        title="Recreate its tmux session and resume Claude"
      >
        {working ? <Loader2Icon className="animate-spin" /> : <PlayIcon />}
        Resume
      </Button>
    </div>
  )
}
