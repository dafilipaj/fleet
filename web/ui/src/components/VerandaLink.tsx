import { useState } from 'react'
import { ClockPlusIcon, ExternalLinkIcon, Loader2Icon, MessagesSquareIcon, UnlinkIcon } from 'lucide-react'
import { toast } from 'sonner'

import { api } from '@/api/client'
import type { Session } from '@/api/types'
import { Button } from '@/components/ui/button'
import { useNow } from '@/hooks/useNow'
import { slackLeft } from '@/hooks/useSlack'
import { refreshVeranda, useSessionVerandaLink, useVeranda } from '@/hooks/useVeranda'

const STATE_LABEL = { busy: 'working', idle: 'idle', waiting: 'waiting on you' } as const

/**
 * Details → Veranda (opt-in: only with a `veranda` section in the config): the session's link to a
 * Veranda room — room, time left, Extend (+1h) and Unlink. Linking is `fleet veranda link`, which
 * prints the room's passphrase once.
 */
export function VerandaSection({ host, session: s }: { host: string; session: Session | null }) {
  const veranda = useVeranda()
  const link = useSessionVerandaLink(host, s?.session_id)
  const now = useNow(30 * 1000)
  const [busy, setBusy] = useState<'extend' | 'unlink' | null>(null)

  if (!s?.session_id || !veranda?.configured || !veranda.enabled) return null

  const run = async (what: 'extend' | 'unlink') => {
    if (!link || busy) return
    setBusy(what)
    try {
      if (what === 'extend') {
        const l = await api.verandaExtend(link.room, 60)
        toast.success('Veranda link extended', { description: `room ${l.room}, ${slackLeft(l.until, Date.now())} left` })
      } else {
        await api.verandaUnlink(link.room)
        toast.success('Unlinked from Veranda', { description: `room ${link.room} closed` })
      }
      await refreshVeranda()
    } catch (err) {
      toast.error(what === 'extend' ? 'Extend failed' : 'Unlink failed', { description: (err as Error)?.message })
    } finally {
      setBusy(null)
    }
  }

  return (
    <section className="py-1">
      <h3 className="pt-2 pb-1 text-[0.6875rem] font-semibold tracking-wider text-dimmer uppercase">Veranda</h3>
      {link ? (
        <div className="flex min-h-12 flex-wrap items-center justify-between gap-3 py-1.5">
          <div className="min-w-0">
            <div className="inline-flex items-center gap-1 text-sm">
              <MessagesSquareIcon className="size-3.5 shrink-0 text-primary" />
              Room {link.room} · {slackLeft(link.until, now)} left
              {link.url ? (
                <a href={link.url} target="_blank" rel="noreferrer" aria-label="Open Veranda" className="text-dimmer hover:text-foreground">
                  <ExternalLinkIcon className="size-3" />
                </a>
              ) : null}
            </div>
            <div className="truncate text-xs text-dimmer">
              {[
                link.title,
                link.state ? STATE_LABEL[link.state] : null,
                `${link.delivered} in · ${link.replies} out`,
                link.pending ? `${link.pending} waiting` : null,
                link.note || null,
              ]
                .filter(Boolean)
                .join(' · ')}
            </div>
          </div>
          <div className="flex gap-2">
            <Button variant="outline" size="lg" disabled={busy != null} onClick={() => void run('extend')}>
              {busy === 'extend' ? <Loader2Icon className="animate-spin" /> : <ClockPlusIcon />}
              +1h
            </Button>
            <Button variant="outline" size="lg" disabled={busy != null} onClick={() => void run('unlink')}>
              {busy === 'unlink' ? <Loader2Icon className="animate-spin" /> : <UnlinkIcon />}
              Unlink
            </Button>
          </div>
        </div>
      ) : (
        <div className="flex min-h-12 items-center py-1.5">
          <div className="min-w-0">
            <div className="text-sm">Not linked</div>
            <div className="truncate text-xs text-dimmer">
              Let guests in a Veranda room talk to it: <code>fleet veranda link --room &lt;name&gt;</code>
            </div>
          </div>
        </div>
      )}
    </section>
  )
}
