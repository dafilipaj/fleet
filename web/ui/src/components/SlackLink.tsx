import { useState } from 'react'
import { ExternalLinkIcon, Loader2Icon, MessagesSquareIcon, UnlinkIcon } from 'lucide-react'
import { toast } from 'sonner'

import { api } from '@/api/client'
import type { Session } from '@/api/types'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Textarea } from '@/components/ui/textarea'
import { ToggleGroup, ToggleGroupItem } from '@/components/ui/toggle-group'
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip'
import { useNow } from '@/hooks/useNow'
import { refreshSlack, slackLeft, useSessionSlackLinks, useSlack } from '@/hooks/useSlack'
import { cn } from '@/lib/utils'

const DURATIONS = [
  { label: '30m', minutes: 30 },
  { label: '1h', minutes: 60 },
  { label: '2h', minutes: 120 },
  { label: '4h', minutes: 240 },
  { label: '8h', minutes: 480 },
]

/**
 * A session linked to Slack (web/lib/slack.mjs): the time left. Nothing for an unlinked one. Not a
 * link — rows and cards are links already.
 */
export function SlackChip({ session: s, className }: { session: Pick<Session, 'host' | 'session_id'>; className?: string }) {
  const links = useSessionSlackLinks(s.host, s.session_id)
  const now = useNow(30 * 1000)
  if (!links.length) return null
  const until = Math.max(...links.map((l) => l.until))
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <span
          aria-label={`Linked to Slack for ${slackLeft(until, now)}`}
          className={cn(
            'inline-flex h-4.5 shrink-0 items-center gap-1 rounded-[5px] border border-primary/40 px-1 text-[0.6875rem] text-primary tabular-nums',
            className,
          )}
        >
          <MessagesSquareIcon className="size-3 shrink-0" />
          {slackLeft(until, now)}
        </span>
      </TooltipTrigger>
      <TooltipContent>People in a Slack thread can talk to this session ({links.length > 1 ? `${links.length} links` : links[0].permalink})</TooltipContent>
    </Tooltip>
  )
}

/**
 * Details → Slack: the session's links (time left, task, Unlink), and "Link to Slack": a thread,
 * channel or DM link, how long, and what it may help with. Only on the bridge's host (`slack.host`).
 */
export function SlackLinkSection({ host, session: s }: { host: string; session: Session | null }) {
  const slack = useSlack()
  const links = useSessionSlackLinks(host, s?.session_id)
  const now = useNow(30 * 1000)
  const [open, setOpen] = useState(false)
  const [url, setUrl] = useState('')
  const [minutes, setMinutes] = useState(60)
  const [task, setTask] = useState('')
  const [mode, setMode] = useState<'mention' | 'proactive'>('mention')
  const [busy, setBusy] = useState<string | null>(null)

  if (!s?.session_id || !slack?.enabled || slack.host !== host) return null

  const link = async () => {
    if (busy || !url.trim()) return
    setBusy('link')
    try {
      const l = await api.slackLink({ host, session: s.session_id!, url: url.trim(), minutes, mode, ...(task.trim() ? { task: task.trim() } : {}) })
      toast.success(l.renewed ? 'Slack link renewed' : 'Linked to Slack', { description: `until ${new Date(l.until).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })}` })
      setOpen(false)
      setUrl('')
      setTask('')
      await refreshSlack()
    } catch (err) {
      toast.error('Link to Slack failed', { description: (err as Error)?.message })
    } finally {
      setBusy(null)
    }
  }

  const unlink = async (id: string) => {
    if (busy) return
    setBusy(id)
    try {
      await api.slackUnlink(id)
      toast.success('Unlinked from Slack')
      await refreshSlack()
    } catch (err) {
      toast.error('Unlink failed', { description: (err as Error)?.message })
    } finally {
      setBusy(null)
    }
  }

  return (
    <section className="py-1">
      <h3 className="pt-2 pb-1 text-[0.6875rem] font-semibold tracking-wider text-dimmer uppercase">Slack</h3>
      <div className="divide-y divide-border/60">
        {links.map((l) => (
          <div key={l.id} className="flex min-h-12 items-center justify-between gap-3 py-1.5">
            <div className="min-w-0">
              <a href={l.permalink} target="_blank" rel="noreferrer" className="inline-flex items-center gap-1 text-sm hover:underline">
                <MessagesSquareIcon className="size-3.5 shrink-0 text-primary" />
                {l.dm ? `DM with ${l.dm.name}` : l.threadTs ? 'Thread' : 'Conversation'} · {slackLeft(l.until, now)} left
                <ExternalLinkIcon className="size-3 text-dimmer" />
              </a>
              <div className="truncate text-xs text-dimmer">
                {[l.mode === 'proactive' ? 'Proactive' : 'When mentioned', l.task || 'the work of this session', `${l.delivered} in · ${l.replies} out`, l.pending ? `${l.pending} waiting` : null].filter(Boolean).join(' · ')}
              </div>
            </div>
            <Button variant="outline" className="h-9 shrink-0 px-3" disabled={busy === l.id} onClick={() => void unlink(l.id)}>
              {busy === l.id ? <Loader2Icon className="animate-spin" /> : <UnlinkIcon />}
              Unlink
            </Button>
          </div>
        ))}
        {open ? (
          <div className="flex flex-col gap-2 py-2">
            <Input
              autoFocus
              value={url}
              onChange={(e) => setUrl(e.target.value)}
              onKeyDown={(e) => e.key === 'Enter' && void link()}
              placeholder="Slack link (a message's thread, a channel) or @person for a DM"
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
            <ToggleGroup type="single" value={mode} onValueChange={(v) => v && setMode(v as 'mention' | 'proactive')} aria-label="When it speaks" className="justify-start">
              {(
                [
                  ['mention', 'When mentioned', 'Woken when someone mentions the app or DMs it, and by messages in threads it has posted in (it judges whether they are for it); the rest goes along as context'],
                  ['proactive', 'Proactive', 'Sees every message and steps in when it judges it should (your instructions say how much)'],
                ] as const
              ).map(([v, label, hint]) => (
                <ToggleGroupItem
                  key={v}
                  value={v}
                  title={hint}
                  className="h-8 rounded-full border border-border bg-card px-3 text-xs data-[state=on]:border-primary/50 data-[state=on]:bg-accent"
                >
                  {label}
                </ToggleGroupItem>
              ))}
            </ToggleGroup>
            <Textarea
              value={task}
              onChange={(e) => setTask(e.target.value)}
              rows={3}
              maxLength={1000}
              placeholder="Instructions: what it may help with, tone, when to step in (default: the work of this session)"
              className="min-h-0 text-sm"
            />
            <p className="text-xs text-dimmer">
              {mode === 'proactive'
                ? 'It sees every message there and posts when it has something useful, as the Slack app.'
                : 'It follows quietly and answers when someone mentions the app or DMs it, or, in a thread it has posted in, talks to it.'}{' '}
              {url.trim().startsWith('http') ? 'Everyone there can talk to it until the link ends. Invite the app first.' : 'A person: the app DMs with them — tell them to write to the app, or ask it in the instructions to start.'}
            </p>
            <div className="flex justify-end gap-2">
              <Button variant="ghost" className="h-9 px-3" onClick={() => setOpen(false)}>
                Cancel
              </Button>
              <Button className="h-9 px-3" disabled={!url.trim() || busy === 'link'} onClick={() => void link()}>
                {busy === 'link' ? <Loader2Icon className="animate-spin" /> : <MessagesSquareIcon />}
                Link
              </Button>
            </div>
          </div>
        ) : (
          <div className="flex min-h-12 items-center justify-between gap-3 py-1.5">
            <div className="min-w-0">
              <div className="text-sm">{links.length ? 'Another conversation' : 'Not linked'}</div>
              <div className="truncate text-xs text-dimmer">Let people in a Slack thread talk to this session for a while</div>
            </div>
            <Button variant="outline" className="h-9 shrink-0 px-3" onClick={() => setOpen(true)}>
              <MessagesSquareIcon />
              Link to Slack
            </Button>
          </div>
        )}
      </div>
    </section>
  )
}
