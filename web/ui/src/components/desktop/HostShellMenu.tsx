import { SquareTerminalIcon } from 'lucide-react'

import { HostDot } from '@/components/HostBadge'
import { Button } from '@/components/ui/button'
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuLabel, DropdownMenuTrigger } from '@/components/ui/dropdown-menu'
import { useFleet } from '@/hooks/useFleet'
import { copyWithToast, hostShellCommand, hostShellLink } from '@/lib/clipboard'
import { cn } from '@/lib/utils'

/**
 * The top bar's ▣: straight into a host's tmux. Fleet.app opens an iTerm tab
 * (`fleet://shell` → `fleet -H <host> new`); a browser copies that command.
 */
export function HostShellMenu({ className }: { className?: string }) {
  const { fleet } = useFleet()
  const hosts = fleet?.hosts ?? []
  if (!hosts.length) return null
  const inApp = hostShellLink(hosts[0].name) != null
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button variant="ghost" size="icon-sm" aria-label="tmux on a host" title="tmux on a host" className={cn('text-dimmer hover:text-foreground', className)}>
          <SquareTerminalIcon />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="min-w-48">
        <DropdownMenuLabel className="text-xs text-dimmer">{inApp ? 'tmux in iTerm' : 'Copy the tmux command'}</DropdownMenuLabel>
        {hosts.map((h) => {
          const link = hostShellLink(h.name)
          const label = (
            <>
              <HostDot host={h.name} />
              <span className={cn(h.ok === false && 'text-destructive')}>{h.name}</span>
            </>
          )
          return link ? (
            <DropdownMenuItem key={h.name} asChild>
              <a href={link} title={hostShellCommand(h.name)}>
                {label}
              </a>
            </DropdownMenuItem>
          ) : (
            <DropdownMenuItem key={h.name} title={hostShellCommand(h.name)} onSelect={() => void copyWithToast(hostShellCommand(h.name))}>
              {label}
            </DropdownMenuItem>
          )
        })}
      </DropdownMenuContent>
    </DropdownMenu>
  )
}
