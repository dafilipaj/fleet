import { useEffect, useRef, useState } from 'react'
import { GaugeIcon } from 'lucide-react'

import { Button } from '@/components/ui/button'
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog'
import { setLoadConfirm } from '@/lib/loadConfirm'

/**
 * Answers `confirmLoad`: a new session on a host under heavy load starts only once you say so.
 * One question at a time; a second one while it is open is a no.
 */
export function LoadConfirmHost() {
  const [message, setMessage] = useState<string | null>(null)
  const resolver = useRef<((ok: boolean) => void) | null>(null)

  useEffect(() => {
    setLoadConfirm((msg) => {
      if (resolver.current) return Promise.resolve(false)
      return new Promise<boolean>((resolve) => {
        resolver.current = resolve
        setMessage(msg)
      })
    })
    return () => setLoadConfirm(null)
  }, [])

  const answer = (ok: boolean) => {
    resolver.current?.(ok)
    resolver.current = null
    setMessage(null)
  }

  return (
    <Dialog open={message != null} onOpenChange={(o) => (o ? null : answer(false))}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <GaugeIcon className="size-5 text-status-waiting" />
            Host under heavy load
          </DialogTitle>
          <DialogDescription>{message}. Start the session anyway?</DialogDescription>
        </DialogHeader>
        <DialogFooter>
          <Button variant="outline" onClick={() => answer(false)}>
            Cancel
          </Button>
          <Button onClick={() => answer(true)}>Start anyway</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
