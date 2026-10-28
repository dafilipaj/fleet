import { useEffect, useRef, useState } from 'react'
import { CircleCheckBigIcon, Loader2Icon, XIcon } from 'lucide-react'

import type { SessionDone } from '@/api/types'
import { Button } from '@/components/ui/button'
import { clockTime } from '@/lib/format'

/**
 * The end of a conversation whose agent reported its work finished (`fleet done`): its note, when,
 * and Close. Close takes two taps (the first arms it for 5s), like Close in the details panel.
 */
export function DoneCard({ done, now, onClose }: { done: SessionDone; now: number; onClose?: () => Promise<void> }) {
  const [armed, setArmed] = useState(false)
  const [closing, setClosing] = useState(false)
  const timer = useRef<ReturnType<typeof setTimeout>>(undefined)
  useEffect(() => () => clearTimeout(timer.current), [])

  const close = async () => {
    if (!onClose || closing) return
    clearTimeout(timer.current)
    if (!armed) {
      setArmed(true)
      timer.current = setTimeout(() => setArmed(false), 5000)
      return
    }
    setClosing(true)
    try {
      await onClose()
    } catch {
      setClosing(false)
      setArmed(false)
    }
  }

  return (
    <div role="status" className="mt-4 flex items-start gap-2.5 rounded-xl border border-primary/35 bg-primary/8 px-3 py-2.5">
      <CircleCheckBigIcon className="mt-0.5 size-4 shrink-0 text-primary" aria-hidden />
      <div className="min-w-0 flex-1">
        <div className="text-[0.8125rem] font-semibold text-primary">
          Done <span className="font-normal text-dimmer tabular-nums">· {clockTime(done.at, now)}</span>
        </div>
        {done.note ? <p className="mt-0.5 text-[0.8125rem] break-words text-foreground/85">{done.note}</p> : null}
        <p className="mt-0.5 text-[0.6875rem] text-dimmer">Nothing waits on you here. Close it when you have read it, or write to pick it up again.</p>
      </div>
      {onClose ? (
        <Button variant={armed ? 'destructive' : 'outline'} size="sm" className="h-8 shrink-0 rounded-full px-3" disabled={closing} onClick={close}>
          {closing ? <Loader2Icon className="animate-spin" /> : <XIcon />}
          {armed ? 'Tap again' : 'Close'}
        </Button>
      ) : null}
    </div>
  )
}
