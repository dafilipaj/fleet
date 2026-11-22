import { useState } from 'react'
import { ClockPlusIcon, CopyIcon, ExternalLinkIcon, KeyRoundIcon, Link2Icon, Loader2Icon, MessagesSquareIcon, UnlinkIcon, XIcon } from 'lucide-react'
import { toast } from 'sonner'

import { api } from '@/api/client'
import type { Session, VerandaLink } from '@/api/types'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Textarea } from '@/components/ui/textarea'
import { ToggleGroup, ToggleGroupItem } from '@/components/ui/toggle-group'
import { useNow } from '@/hooks/useNow'
import { slackDuration, slackLeft } from '@/hooks/useSlack'
import { refreshVeranda, useSessionVerandaLink, useVeranda } from '@/hooks/useVeranda'
import { copyText } from '@/lib/clipboard'

const STATE_LABEL = { busy: 'working', idle: 'idle', waiting: 'waiting on you' } as const

/** Veranda rooms always expire (at most 30 days). */
const DURATIONS = [
  { label: '1h', minutes: 60 },
  { label: '2h', minutes: 120 },
  { label: '4h', minutes: 240 },
  { label: '1d', minutes: 24 * 60 },
  { label: '1w', minutes: 7 * 24 * 60 },
]

/** As the bridge checks it (web/lib/veranda.mjs `validRoom`). */
const ROOM_RE = /^[a-z0-9](?:[a-z0-9-]{0,46}[a-z0-9])?$/

const END_LABEL: Record<string, string> = { expired: 'expired', unlinked: 'unlinked', 'room-closed': 'room closed', 'session-gone': 'session ended' }

/** The passphrase of the last link made here, per session: shown once, kept until dismissed or the page reloads. */
const shown = new Map<string, { room: string; passphrase: string; url: string | null }>()

/**
 * Details → Veranda (opt-in: only with a `veranda` section in the config): the session's link to a
 * Veranda room — room, time left, Extend (+1h) and Unlink; when it has none, "Link to Veranda" (room,
 * how long, title, instructions) and "Link again" for the rooms it was linked to before. A link opens
 * the room with a new passphrase, shown here once, to pass on to the guests.
 */
export function VerandaSection({ host, session: s }: { host: string; session: Session | null }) {
  const veranda = useVeranda()
  const link = useSessionVerandaLink(host, s?.session_id)
  const now = useNow(30 * 1000)
  const [busy, setBusy] = useState<string | null>(null)
  const [open, setOpen] = useState(false)
  const [room, setRoom] = useState('')
  const [minutes, setMinutes] = useState(120)
  const [title, setTitle] = useState('')
  const [note, setNote] = useState('')
  const [, setTick] = useState(0)

  if (!s?.session_id || !veranda?.configured || !veranda.enabled) return null
  const key = `${host}/${s.session_id}`
  const secret = shown.get(key) ?? null

  // Ended links of this session (newest first), one per room.
  const past: VerandaLink[] = []
  if (!link) {
    for (const l of veranda.ended) {
      if ((l.host ?? veranda.host) !== host || l.session !== s.session_id || past.some((p) => p.room === l.room)) continue
      past.push(l)
    }
  }

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

  const doLink = async (busyKey: string, body: { room: string; minutes: number; title?: string; note?: string }) => {
    if (busy) return false
    setBusy(busyKey)
    try {
      const l = await api.verandaLink({ host, session: s.session_id!, ...body })
      shown.set(key, { room: l.room, passphrase: l.passphrase, url: l.url })
      setTick((n) => n + 1)
      toast.success('Linked to Veranda', { description: `room ${l.room} until ${clock(l.until)} — pass the passphrase on to the guests` })
      await refreshVeranda()
      return true
    } catch (err) {
      toast.error('Link to Veranda failed', { description: (err as Error)?.message })
      return false
    } finally {
      setBusy(null)
    }
  }

  const roomName = room.trim()
  const roomOk = ROOM_RE.test(roomName)
  const submit = async () => {
    if (!roomOk) return
    const ok = await doLink('link', { room: roomName, minutes, ...(title.trim() ? { title: title.trim() } : {}), ...(note.trim() ? { note: note.trim() } : {}) })
    if (ok) {
      setOpen(false)
      setRoom('')
      setTitle('')
      setNote('')
    }
  }

  const relink = (l: VerandaLink) =>
    void doLink(l.id, { room: l.room, minutes: l.minutes, ...(l.title ? { title: l.title } : {}), ...(l.note ? { note: l.note } : {}) })

  const dismiss = () => {
    shown.delete(key)
    setTick((n) => n + 1)
  }

  const copy = async (text: string) => {
    if (await copyText(text)) toast.success('Copied')
  }

  return (
    <section className="py-1">
      <h3 className="pt-2 pb-1 text-[0.6875rem] font-semibold tracking-wider text-dimmer uppercase">Veranda</h3>
      <div className="divide-y divide-border/60">
        {secret ? (
          <div className="flex min-h-12 items-center justify-between gap-3 py-1.5">
            <div className="min-w-0">
              <div className="inline-flex items-center gap-1 text-sm">
                <KeyRoundIcon className="size-3.5 shrink-0 text-primary" />
                <code className="font-mono select-all">{secret.passphrase}</code>
              </div>
              <div className="truncate text-xs text-dimmer">Passphrase of room {secret.room} — shown only now; pass it on to the guests</div>
            </div>
            <div className="flex gap-2">
              <Button
                variant="outline"
                size="lg"
                onClick={() => void copy(secret.url ? `${secret.url}\nRoom: ${secret.room}\nPassphrase: ${secret.passphrase}` : secret.passphrase)}
              >
                <CopyIcon />
                Copy
              </Button>
              <Button variant="ghost" size="lg" aria-label="Hide the passphrase" onClick={dismiss}>
                <XIcon />
              </Button>
            </div>
          </div>
        ) : null}
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
          <>
            {past.map((l) => (
              <div key={l.id} className="flex min-h-12 items-center justify-between gap-3 py-1.5">
                <div className="min-w-0">
                  <div className="inline-flex items-center gap-1 text-sm text-muted-foreground">
                    <MessagesSquareIcon className="size-3.5 shrink-0 text-dimmer" />
                    Room {l.room} · {END_LABEL[l.endReason ?? ''] ?? l.endReason ?? 'ended'}
                  </div>
                  <div className="truncate text-xs text-dimmer">{[l.title, l.note || 'no instructions'].filter(Boolean).join(' · ')}</div>
                </div>
                <Button variant="outline" size="lg" disabled={busy != null} onClick={() => relink(l)}>
                  {busy === l.id ? <Loader2Icon className="animate-spin" /> : <Link2Icon />}
                  Link again · {slackDuration(l.minutes)}
                </Button>
              </div>
            ))}
            {open ? (
              <div className="flex flex-col gap-2 py-2">
                <Input
                  autoFocus
                  value={room}
                  onChange={(e) => setRoom(e.target.value.toLowerCase())}
                  onKeyDown={(e) => e.key === 'Enter' && void submit()}
                  placeholder="Room (like team-demo)"
                  aria-invalid={roomName !== '' && !roomOk}
                  className="h-9 text-sm"
                />
                <ToggleGroup type="single" value={String(minutes)} onValueChange={(v) => v && setMinutes(Number(v))} aria-label="How long" className="justify-start">
                  {DURATIONS.map((d) => (
                    <ToggleGroupItem
                      key={d.minutes}
                      value={String(d.minutes)}
                      className="h-8 rounded-full border border-border bg-card px-3 text-xs data-[state=on]:border-primary/50 data-[state=on]:bg-accent"
                    >
                      {d.label}
                    </ToggleGroupItem>
                  ))}
                </ToggleGroup>
                <Input value={title} onChange={(e) => setTitle(e.target.value)} maxLength={120} placeholder="Title the guests see (optional)" className="h-9 text-sm" />
                <Textarea
                  value={note}
                  onChange={(e) => setNote(e.target.value)}
                  rows={3}
                  maxLength={1000}
                  placeholder="Instructions: what it may help the guests with, and what not"
                  className="min-h-0 text-sm"
                />
                <p className="text-xs text-dimmer">
                  {roomName !== '' && !roomOk
                    ? 'A room is 1–48 lowercase letters, digits or dashes.'
                    : 'Opens the room with a new passphrase (shown here once). Guests with a Veranda account and the passphrase talk to this session until the link ends.'}
                </p>
                <div className="flex justify-end gap-2">
                  <Button variant="ghost" size="lg" onClick={() => setOpen(false)}>
                    Cancel
                  </Button>
                  <Button size="lg" disabled={!roomOk || busy != null} onClick={() => void submit()}>
                    {busy === 'link' ? <Loader2Icon className="animate-spin" /> : <MessagesSquareIcon />}
                    Link
                  </Button>
                </div>
              </div>
            ) : (
              <div className="flex min-h-12 items-center justify-between gap-3 py-1.5">
                <div className="min-w-0">
                  <div className="text-sm">{past.length ? 'Another room' : 'Not linked'}</div>
                  <div className="truncate text-xs text-dimmer">Let guests in a Veranda room talk to this session for a while</div>
                </div>
                <Button variant="outline" size="lg" onClick={() => setOpen(true)}>
                  <MessagesSquareIcon />
                  Link to Veranda
                </Button>
              </div>
            )}
          </>
        )}
      </div>
    </section>
  )
}

const clock = (t: number) => new Date(t).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })
