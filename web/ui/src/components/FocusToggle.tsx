import { MoonIcon } from 'lucide-react'

import { Button } from '@/components/ui/button'
import { DropdownMenuItem } from '@/components/ui/dropdown-menu'
import { focusLeft, toggleFocus, useFocus, type FocusState } from '@/hooks/useFocus'
import { useNow } from '@/hooks/useNow'
import { cn } from '@/lib/utils'

function label(focus: FocusState, remaining: string | null): string {
  return remaining ? `Focus on — ${remaining} left on ${focus.host} (click to stop)` : `Focus mode on ${focus.host}: Do Not Disturb, distracting apps quit`
}

/** The Focus mode button (the desktop sidebar, the rail): nothing when no host runs `focus`. */
export function FocusToggle({ size = 'icon', className }: { size?: 'icon' | 'icon-sm'; className?: string }) {
  const focus = useFocus()
  const now = useNow(30 * 1000)
  if (!focus.host) return null
  const remaining = focusLeft(focus, now)
  return (
    <Button
      variant="ghost"
      size={remaining ? 'sm' : size}
      aria-label="Focus mode"
      aria-pressed={Boolean(remaining)}
      title={label(focus, remaining)}
      disabled={focus.busy}
      onClick={() => void toggleFocus(focus)}
      className={cn('text-dimmer hover:text-foreground', remaining && 'h-7 gap-1 px-1.5 text-primary hover:text-primary', className)}
    >
      <MoonIcon className={cn(remaining && 'fill-current')} />
      {remaining ? <span className="text-xs tabular-nums">{remaining}</span> : null}
    </Button>
  )
}

/** The same as a menu item (the mobile ⋯ menu). */
export function FocusMenuItem({ className }: { className?: string }) {
  const focus = useFocus()
  const now = useNow(30 * 1000)
  if (!focus.host) return null
  const remaining = focusLeft(focus, now)
  return (
    <DropdownMenuItem disabled={focus.busy} onSelect={() => void toggleFocus(focus)} className={className}>
      <MoonIcon className={cn(remaining && 'fill-current text-primary')} />
      {remaining ? `Focus on · ${remaining}` : 'Focus mode'}
    </DropdownMenuItem>
  )
}
