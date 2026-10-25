import { useEffect, useRef, useState, type ReactNode } from 'react'
import { ChevronRightIcon, Loader2Icon, MoonIcon, PlayIcon } from 'lucide-react'

import type { DormantView } from '@/api/types'
import { HostBadge } from '@/components/HostBadge'
import { Button } from '@/components/ui/button'
import { useDormant } from '@/hooks/useDormant'
import { dormantMeta, dormantTitles, searchDormant } from '@/lib/dormant'
import { cn } from '@/lib/utils'

/**
 * Two clicks, like Close session / Delete stack: the first arms for 5 s ("Confirm …"), the
 * second runs `onConfirm`. For Forget and Resume all.
 */
export function ArmButton({
  children,
  confirm,
  onConfirm,
  busy = false,
  className,
  variant = 'ghost',
  size = 'sm',
  title,
}: {
  children: ReactNode
  confirm: string
  onConfirm: () => void
  busy?: boolean
  className?: string
  variant?: 'ghost' | 'outline'
  size?: 'sm' | 'lg'
  title?: string
}) {
  const [armed, setArmed] = useState(false)
  const timer = useRef<ReturnType<typeof setTimeout>>(undefined)
  useEffect(() => () => clearTimeout(timer.current), [])
  return (
    <Button
      type="button"
      variant={variant}
      size={size}
      disabled={busy}
      title={title}
      onClick={(e) => {
        e.preventDefault()
        e.stopPropagation()
        clearTimeout(timer.current)
        if (!armed) {
          setArmed(true)
          timer.current = setTimeout(() => setArmed(false), 5000)
          return
        }
        setArmed(false)
        onConfirm()
      }}
      className={cn(armed && 'text-destructive hover:text-destructive', className)}
    >
      {busy ? <Loader2Icon className="animate-spin" /> : null}
      {armed ? confirm : children}
    </Button>
  )
}

/**
 * Sessions a reboot left dormant, per host: what they were (tmux name, Claude titles, panes, how
 * long ago) with Resume (brings the tmux session back and resumes its Claude sessions), Forget
 * (two clicks) and Resume all (two clicks). Nothing when no host has dormant sessions. Collapsed
 * until expanded, like Recently closed; a search shows its matches expanded.
 * `host`: only that host (the list's host filter). `query`: the session search — only matching entries. `desktop`: the denser sidebar / board sizes.
 */
export function DormantSection({ now, host = null, query = '', desktop = false, className }: { now: number; host?: string | null; query?: string; desktop?: boolean; className?: string }) {
  const { hosts, busy, resume, resumeAll, forget } = useDormant()
  const [open, setOpen] = useState(false)
  const shown = (host ? hosts.filter((h) => h.host === host) : hosts).map((h) => searchDormant(h, query)).filter((h) => h.views.length > 0)
  if (!shown.length) return null
  const total = shown.reduce((n, h) => n + h.views.length, 0)
  const size = desktop ? 'sm' : 'lg'
  // A search shows its matches without having to expand the section.
  const expanded = open || !!query.trim()
  return (
    <section aria-label="Dormant sessions" className={cn('flex flex-col gap-2', className)}>
      <button
        type="button"
        aria-expanded={expanded}
        onClick={() => setOpen((o) => !o)}
        title="Stopped by a reboot"
        className="flex min-h-8 items-center gap-1.5 rounded-md px-1 text-left text-xs font-semibold text-muted-foreground outline-none focus-visible:ring-3 focus-visible:ring-ring/50"
      >
        <ChevronRightIcon className={cn('size-3.5 transition-transform', expanded && 'rotate-90')} />
        <MoonIcon className="size-3.5" />
        Dormant
        <span className="font-normal text-dimmer tabular-nums">· {total}</span>
      </button>
      {expanded
        ? shown.map((h) => (
            <div key={h.host} className="flex flex-col gap-1.5">
              <div className="flex items-center gap-2 px-1">
                <HostBadge host={h.host} />
                <span className="text-xs text-dimmer tabular-nums">{h.views.length}</span>
                {h.views.length > 1 ? (
                  <ArmButton
                    confirm="Resume all?"
                    onConfirm={() => void resumeAll(h.host)}
                    busy={busy.has(`${h.host}/*`)}
                    size={size}
                    className="ml-auto text-muted-foreground"
                    title={`Bring back every dormant session on ${h.host}`}
                  >
                    <PlayIcon /> Resume all
                  </ArmButton>
                ) : null}
              </div>
              {h.views.map((v) => (
                <DormantRow
                  key={`${v.kind}/${v.target}`}
                  view={v}
                  now={now}
                  size={size}
                  busy={busy.has(`${h.host}/${v.target}`) || busy.has(`${h.host}/*`)}
                  onResume={() => void resume(h.host, v.target)}
                  onForget={() => void forget(h.host, v.target)}
                />
              ))}
            </div>
          ))
        : null}
    </section>
  )
}

function DormantRow({
  view: v,
  now,
  size,
  busy,
  onResume,
  onForget,
}: {
  view: DormantView
  now: number
  size: 'sm' | 'lg'
  busy: boolean
  onResume: () => void
  onForget: () => void
}) {
  const titles = dormantTitles(v)
  return (
    <div className="rounded-lg border border-dashed border-border/70 bg-card/50 px-3 py-2">
      <div className="min-w-0 opacity-75">
        <p className="truncate text-sm font-semibold" title={v.name}>
          {v.name}
        </p>
        {titles && titles !== v.name ? <p className="line-clamp-2 text-xs break-words text-muted-foreground">{titles}</p> : null}
        <p className="mt-0.5 truncate text-[0.6875rem] text-dimmer tabular-nums">{dormantMeta(v, now)}</p>
      </div>
      <div className="mt-1.5 flex items-center justify-end gap-1">
        <ArmButton confirm="Forget?" onConfirm={onForget} busy={false} size={size} className="text-dimmer" title="Drop it without resuming">
          Forget
        </ArmButton>
        <Button type="button" variant="outline" size={size} disabled={busy} onClick={onResume} title="Recreate the tmux session and resume its Claude sessions">
          {busy ? <Loader2Icon className="animate-spin" /> : <PlayIcon />}
          Resume
        </Button>
      </div>
    </div>
  )
}
