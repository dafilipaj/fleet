import { CopyIcon, SquareTerminalIcon } from 'lucide-react'

import type { Host } from '@/api/types'
import { HostDot } from '@/components/HostBadge'
import { Button } from '@/components/ui/button'
import { Popover, PopoverContent, PopoverTrigger } from '@/components/ui/popover'
import { useFleet } from '@/hooks/useFleet'
import { copyWithToast, hostShellCommand, hostShellLink } from '@/lib/clipboard'
import { relTime } from '@/lib/format'
import { cn } from '@/lib/utils'

/**
 * The top bar's ▣ per host: the host's state at a glance (red when unreachable) and a popover
 * with its name, state, error and the way into its tmux. Fleet.app opens an iTerm tab
 * (`fleet://shell` → `fleet -H <host> new`; the Mac it runs on gets a plain iTerm tab);
 * a browser copies the command.
 */
export function HostButtons({ now, className }: { now: number; className?: string }) {
  const { fleet } = useFleet()
  return (
    <>
      {(fleet?.hosts ?? []).map((h) => (
        <HostButton key={h.name} host={h} self={h.name === fleet?.self} now={now} className={className} />
      ))}
    </>
  )
}

function HostButton({ host: h, self, now, className }: { host: Host; self: boolean; now: number; className?: string }) {
  const down = h.ok === false
  const label = down ? `${h.name}: unreachable` : `${h.name}: ${h.sessions?.length ?? 0} sessions`
  return (
    <Popover>
      <PopoverTrigger asChild>
        <Button
          variant="ghost"
          size="icon-sm"
          aria-label={label}
          title={label}
          className={cn('relative', down ? 'text-destructive hover:text-destructive' : 'text-dimmer hover:text-foreground', className)}
        >
          <SquareTerminalIcon />
          <HostDot host={h.name} className="absolute right-1 bottom-1 size-1.5 ring-1 ring-background" />
          {down ? <span aria-hidden className="absolute top-1 right-1 size-1.5 rounded-full bg-destructive ring-1 ring-background" /> : null}
        </Button>
      </PopoverTrigger>
      <PopoverContent align="end" className="w-80">
        <HostCard host={h} self={self} now={now} />
      </PopoverContent>
    </Popover>
  )
}

function HostCard({ host: h, self, now }: { host: Host; self: boolean; now: number }) {
  const down = h.ok === false
  const checked = relTime(h.fetchedAt, now)
  const link = hostShellLink(h.name)
  const cmd = hostShellCommand(h.name)
  const n = h.sessions?.length ?? 0
  return (
    <>
      <div className="flex items-center gap-2">
        <HostDot host={h.name} />
        <span className="font-semibold">{h.name}</span>
        {self ? <span className="text-xs text-dimmer">this server</span> : null}
        <span className={cn('ml-auto text-xs font-medium', down ? 'text-destructive' : 'text-status-busy')}>{down ? 'Unreachable' : 'Connected'}</span>
      </div>
      <div className="text-xs text-muted-foreground tabular-nums">
        {down ? 'No sessions from this host' : `${n} session${n === 1 ? '' : 's'}`}
        {checked ? ` · checked ${checked} ago` : ''}
      </div>
      {down ? (
        <div className="max-h-40 overflow-y-auto rounded-md bg-destructive/10 px-2 py-1.5 text-xs break-words text-destructive">
          {h.error?.replace(/^(unreachable:\s*)+/i, '') || 'no response'}
        </div>
      ) : null}
      {link ? (
        <Button asChild size="sm" variant="outline" className="w-full">
          <a href={link}>
            <SquareTerminalIcon />
            Open terminal
          </a>
        </Button>
      ) : (
        <Button size="sm" variant="outline" className="w-full" title={cmd} onClick={() => void copyWithToast(cmd)}>
          <CopyIcon />
          Copy tmux command
        </Button>
      )}
    </>
  )
}
