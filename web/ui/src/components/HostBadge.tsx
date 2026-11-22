import { GaugeIcon } from 'lucide-react'

import { hostLoad, hostLoadText, isSharedHost } from '@/lib/format'
import { hostBadgeClass, hostDotClass, hostRingClass } from '@/lib/styles'
import { cn } from '@/lib/utils'

/** The mark of a host under heavy load: amber while hot, red when critical. */
function LoadMark({ state, className }: { state: 'hot' | 'critical'; className?: string }) {
  return <GaugeIcon aria-hidden className={cn('size-3 shrink-0', state === 'critical' ? 'text-destructive' : 'text-status-waiting', className)} />
}

/** A host's name in its colour; a shared shed (`hosts.<name>.shared`) is dashed; a gauge while it is under heavy load. */
export function HostBadge({ host, className }: { host: string; className?: string }) {
  const shared = isSharedHost(host)
  const load = hostLoad(host)
  const title = [shared ? `${host} (shared shed)` : null, load ? `${host} ${hostLoadText(load)}` : null].filter(Boolean).join(' — ') || undefined
  return (
    <span
      title={title}
      className={cn(
        'inline-flex h-5 shrink-0 items-center gap-1 rounded-md border px-1.5 text-[0.6875rem] font-semibold tracking-wide whitespace-nowrap',
        hostBadgeClass(host),
        shared && 'border-dashed',
        className,
      )}
    >
      {host}
      {load ? <LoadMark state={load.state} /> : null}
    </span>
  )
}

/** A host's colour mark: a filled dot for the user's own hosts, a hollow diamond for a shared shed; a gauge beside it under heavy load. */
export function HostDot({ host, shared, className }: { host: string; shared?: boolean; className?: string }) {
  const load = hostLoad(host)
  const mark =
    shared ?? isSharedHost(host) ? (
      <span aria-hidden className={cn('inline-block size-2 shrink-0 rotate-45 rounded-[1px] border-[1.5px] bg-transparent', hostRingClass(host), className)} />
    ) : (
      <span aria-hidden className={cn('inline-block size-2 shrink-0 rounded-full', hostDotClass(host), className)} />
    )
  if (!load) return mark
  return (
    <span title={`${host} ${hostLoadText(load)}`} className="inline-flex shrink-0 items-center gap-0.5">
      {mark}
      <LoadMark state={load.state} />
    </span>
  )
}
