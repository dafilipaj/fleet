import { useState } from 'react'
import { AtSignIcon, CheckIcon, Loader2Icon } from 'lucide-react'
import { toast } from 'sonner'

import type { PullRequest } from '@/api/types'
import { Button } from '@/components/ui/button'
import { useNow } from '@/hooks/useNow'
import { usePrs } from '@/hooks/usePrs'
import { relTime } from '@/lib/format'
import { cn } from '@/lib/utils'

/**
 * A PR's unseen @mention of you: who said what (linked to it on GitHub) and "Mark seen".
 * Renders nothing once it is seen (answered on GitHub, or marked here).
 */
export function PrMention({ pr, className }: { pr: PullRequest; className?: string }) {
  const { markSeen } = usePrs()
  const now = useNow(30_000)
  const [busy, setBusy] = useState(false)
  const m = pr.mention
  if (!m || m.seen) return null

  const seen = async () => {
    setBusy(true)
    try {
      await markSeen(pr)
    } catch (err) {
      toast.error('Could not mark it seen', { description: (err as Error)?.message })
      setBusy(false)
    }
  }

  return (
    <div className={cn('flex items-start gap-2 text-xs', className)}>
      <AtSignIcon className="mt-0.5 size-3.5 shrink-0 text-status-waiting" />
      <a href={m.url} target="_blank" rel="noreferrer" className="min-w-0 flex-1 hover:underline" title="Open it on GitHub">
        <span className="font-medium text-foreground">@{m.author}</span>{' '}
        <span className="text-dimmer tabular-nums" title={new Date(m.at).toLocaleString()}>
          {relTime(m.at, now)}
        </span>
        <span className="mt-0.5 line-clamp-2 text-muted-foreground">{m.excerpt}</span>
      </a>
      <Button variant="ghost" size="xs" className="-my-1" onClick={() => void seen()} disabled={busy} title="Mark seen">
        {busy ? <Loader2Icon className="animate-spin" /> : <CheckIcon />} Seen
      </Button>
    </div>
  )
}
