import { SquareTerminalIcon } from 'lucide-react'

import { HostDot } from '@/components/HostBadge'
import { Button } from '@/components/ui/button'
import { useFleet } from '@/hooks/useFleet'
import { copyWithToast, hostShellCommand, hostShellLink } from '@/lib/clipboard'
import { cn } from '@/lib/utils'

/**
 * The top bar's ▣ per host: straight into that host's tmux. Fleet.app opens an iTerm tab
 * (`fleet://shell` → `fleet -H <host> new`; the Mac it runs on gets a plain iTerm tab);
 * a browser copies the command.
 */
export function HostShellButtons({ className }: { className?: string }) {
  const { fleet } = useFleet()
  return (
    <>
      {(fleet?.hosts ?? []).map((h) => {
        const link = hostShellLink(h.name)
        const cmd = hostShellCommand(h.name)
        const cls = cn('relative text-dimmer hover:text-foreground', h.ok === false && 'opacity-50', className)
        const body = (
          <>
            <SquareTerminalIcon />
            <HostDot host={h.name} className="absolute right-1 bottom-1 size-1.5 ring-1 ring-background" />
          </>
        )
        return link ? (
          <Button key={h.name} asChild variant="ghost" size="icon-sm" className={cls}>
            <a href={link} aria-label={`Terminal on ${h.name}`} title={`Terminal on ${h.name} (iTerm)`}>
              {body}
            </a>
          </Button>
        ) : (
          <Button key={h.name} variant="ghost" size="icon-sm" aria-label={`Copy the tmux command for ${h.name}`} title={`tmux on ${h.name}: copy ${cmd}`} onClick={() => void copyWithToast(cmd)} className={cls}>
            {body}
          </Button>
        )
      })}
    </>
  )
}
