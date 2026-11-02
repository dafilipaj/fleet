import { CopyIcon, SquareTerminalIcon } from 'lucide-react'

import type { Host } from '@/api/types'
import { HostDot } from '@/components/HostBadge'
import { Button } from '@/components/ui/button'
import { copyWithToast, hostShellCommand, hostShellLink } from '@/lib/clipboard'
import { relTime } from '@/lib/format'
import { cn } from '@/lib/utils'

/**
 * A host's state and the way into its tmux, behind its chip in the top bar (HostRail): name,
 * state, error, terminal. Fleet.app opens an iTerm tab (`fleet://shell` → `fleet -H <host> new`;
 * the Mac it runs on gets a plain iTerm tab); a browser copies the command.
 */
export function HostCard({ host: h, self, now }: { host: Host; self: boolean; now: number }) {
  const down = h.ok === false
  const checked = relTime(h.fetchedAt, now)
  const link = hostShellLink(h.name)
  const cmd = hostShellCommand(h.name)
  const n = h.sessions?.length ?? 0
  return (
    <>
      <div className="flex items-center gap-2">
        <HostDot host={h.name} shared={h.shared === true} />
        <span className="font-semibold">{h.name}</span>
        {self ? <span className="text-xs text-dimmer">this server</span> : null}
        {h.shared ? <span className="text-xs text-dimmer">shared shed</span> : null}
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
