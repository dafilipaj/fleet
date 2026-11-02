import { isSharedHost } from '@/lib/format'
import { hostBadgeClass, hostDotClass, hostRingClass } from '@/lib/styles'
import { cn } from '@/lib/utils'

/** A host's name in its colour; a shared shed (`hosts.<name>.shared`) is dashed. */
export function HostBadge({ host, className }: { host: string; className?: string }) {
  const shared = isSharedHost(host)
  return (
    <span
      title={shared ? `${host} (shared shed)` : undefined}
      className={cn(
        'inline-flex h-5 shrink-0 items-center rounded-md border px-1.5 text-[0.6875rem] font-semibold tracking-wide whitespace-nowrap',
        hostBadgeClass(host),
        shared && 'border-dashed',
        className,
      )}
    >
      {host}
    </span>
  )
}

/** A host's colour mark: a filled dot for the user's own hosts, a hollow diamond for a shared shed. */
export function HostDot({ host, shared, className }: { host: string; shared?: boolean; className?: string }) {
  if (shared ?? isSharedHost(host)) {
    return <span aria-hidden className={cn('inline-block size-2 shrink-0 rotate-45 rounded-[1px] border-[1.5px] bg-transparent', hostRingClass(host), className)} />
  }
  return <span aria-hidden className={cn('inline-block size-2 shrink-0 rounded-full', hostDotClass(host), className)} />
}
