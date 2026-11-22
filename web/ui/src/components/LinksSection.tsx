import { useState, type ReactNode } from 'react'
import {
  ClockPlusIcon,
  CopyIcon,
  ExternalLinkIcon,
  GitPullRequestIcon,
  KeyRoundIcon,
  Link2Icon,
  ListTodoIcon,
  Loader2Icon,
  type LucideIcon,
  MessagesSquareIcon,
  PencilIcon,
  PlusIcon,
  UnlinkIcon,
  UsersRoundIcon,
  XIcon,
} from 'lucide-react'
import { toast } from 'sonner'

import { api } from '@/api/client'
import type { GithubLink, ProductiveLink, Session, SlackLink, SlackMinutes, VerandaLink } from '@/api/types'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Textarea } from '@/components/ui/textarea'
import { ToggleGroup, ToggleGroupItem } from '@/components/ui/toggle-group'
import { refreshGithub, useGithub } from '@/hooks/useGithub'
import { useNow } from '@/hooks/useNow'
import { refreshProductive, useProductive } from '@/hooks/useProductive'
import { refreshSlack, SLACK_DURATIONS, slackDuration, slackLeft, toSlackMinutes, useSessionSlackLinks, useSlack } from '@/hooks/useSlack'
import { refreshVeranda, useSessionVerandaLink, useVeranda } from '@/hooks/useVeranda'
import { copyText } from '@/lib/clipboard'
import { detectLinkKind, KEEP_OPTIONS, keepLabel, keepValue, LINK_KINDS, LINK_LABEL, ROOM_RE } from '@/lib/links'
import type { LinkKind } from '@/lib/sessions'

const ICON: Record<LinkKind, LucideIcon> = { slack: MessagesSquareIcon, github: GitPullRequestIcon, productive: ListTodoIcon, veranda: UsersRoundIcon }

const PLACEHOLDER: Record<LinkKind, string> = {
  slack: 'Slack thread or channel, @person',
  github: 'GitHub PR',
  productive: 'Productive task',
  veranda: 'Veranda room',
}

/** Veranda rooms always expire (at most 30 days). */
const VERANDA_DURATIONS = [
  { label: '1h', minutes: 60 },
  { label: '2h', minutes: 120 },
  { label: '4h', minutes: 240 },
  { label: '1d', minutes: 24 * 60 },
  { label: '1w', minutes: 7 * 24 * 60 },
]

const VERANDA_STATE = { busy: 'working', idle: 'idle', waiting: 'waiting on you' } as const

const END_LABEL: Record<string, string> = {
  expired: 'expired',
  unlinked: 'unlinked',
  'room-closed': 'room closed',
  'session-gone': 'session ended',
  closed: 'closed',
  merged: 'merged',
}

/** The passphrase of the last Veranda link made here, per session: shown once, kept until dismissed or the page reloads. */
const shown = new Map<string, { room: string; passphrase: string; url: string | null }>()

const chip = 'h-8 rounded-full border border-border bg-card px-3 text-xs data-[state=on]:border-primary/50 data-[state=on]:bg-accent'

/** The form's state: a new link (any kind), or the settings of an active GitHub / Productive link. */
interface Draft {
  kind: LinkKind | null
  /** Set by hand: detection no longer changes it. */
  picked: boolean
  url: string
  editing: string | null
  minutes: number
  slackMinutes: SlackMinutes
  mode: 'mention' | 'proactive'
  title: string
  note: string
  keep: string | null
}

const freshDraft = (): Draft => ({
  kind: null,
  picked: false,
  url: '',
  editing: null,
  minutes: 120,
  slackMinutes: 60,
  mode: 'mention',
  title: '',
  note: '',
  keep: '1d',
})

/**
 * Details → Links: every link of the session in one place — Slack threads, a Veranda room, GitHub
 * PRs and Productive tasks it follows — with what each allows (Unlink, +1h, Edit), the ones it had
 * before ("Link again"), and "Add link": paste a Slack link or @person, a PR, a task or a room name,
 * and the form shows that kind's settings. GitHub and Productive links follow sessions on their
 * bridge's host only; a kind this fleet has no bridge for is not offered.
 */
export function LinksSection({ host, session: s }: { host: string; session: Session | null }) {
  const slack = useSlack()
  const veranda = useVeranda()
  const github = useGithub()
  const productive = useProductive()
  const slackLinks = useSessionSlackLinks(host, s?.session_id)
  const verandaLink = useSessionVerandaLink(host, s?.session_id)
  const now = useNow(30 * 1000)
  const [draft, setDraft] = useState<Draft | null>(null)
  const [busy, setBusy] = useState<string | null>(null)
  const [, setTick] = useState(0)

  const sid = s?.session_id ?? null
  const available = LINK_KINDS.filter((k) =>
    k === 'slack'
      ? Boolean(slack?.enabled)
      : k === 'veranda'
        ? Boolean(veranda?.configured && veranda.enabled)
        : k === 'github'
          ? Boolean(github?.enabled && github.host === host)
          : Boolean(productive?.enabled && productive.host === host),
  )
  const mine = <L extends { host: string | null; session: string }>(ls: L[] | undefined, bridge: string | null | undefined) =>
    (ls ?? []).filter((l) => (l.host ?? bridge) === host && l.session === sid)
  const ghLinks = mine(github?.links, github?.host)
  const ptLinks = mine(productive?.links, productive?.host)

  if (!sid || (!available.length && !slackLinks.length && !verandaLink && !ghLinks.length && !ptLinks.length)) return null
  const key = `${host}/${sid}`
  const secret = shown.get(key) ?? null

  // Ended links (newest first), one per place, none it is linked to again already.
  const pastOf = <L extends { host: string | null; session: string }>(ended: L[] | undefined, bridge: string | null | undefined, place: (l: L) => string, active: L[]) => {
    const seen = new Set(active.map(place))
    const out: L[] = []
    for (const l of mine(ended, bridge)) {
      if (seen.has(place(l))) continue
      seen.add(place(l))
      out.push(l)
    }
    return out
  }
  const slackPlace = (l: SlackLink) => `${l.channel}/${l.threadTs ?? ''}`
  const pastSlack = available.includes('slack') ? pastOf(slack?.ended, slack?.host, slackPlace, slackLinks) : []
  const pastVeranda = available.includes('veranda') && !verandaLink ? pastOf(veranda?.ended, veranda?.host, (l: VerandaLink) => l.room, []) : []
  // A merged or closed PR, a closed task: nothing to link again.
  const pastGithub = available.includes('github') ? pastOf(github?.ended, github?.host, (l: GithubLink) => l.url, ghLinks).filter((l) => l.endReason === 'unlinked') : []
  const pastProductive = available.includes('productive')
    ? pastOf(productive?.ended, productive?.host, (l: ProductiveLink) => l.taskId, ptLinks).filter((l) => l.endReason === 'unlinked')
    : []
  // Veranda: one room per session.
  const addable = available.filter((k) => k !== 'veranda' || !verandaLink)

  /** One action at a time; a failure is a toast. */
  const act = async (id: string, what: string, fn: () => Promise<unknown>) => {
    if (busy) return false
    setBusy(id)
    try {
      await fn()
      return true
    } catch (err) {
      toast.error(`${what} failed`, { description: (err as Error)?.message })
      return false
    } finally {
      setBusy(null)
    }
  }

  const unlink = (kind: LinkKind, id: string, room?: string) =>
    void act(id, 'Unlink', async () => {
      if (kind === 'slack') await api.slackUnlink(id).then(refreshSlack)
      else if (kind === 'veranda') await api.verandaUnlink(room!).then(refreshVeranda)
      else if (kind === 'github') await api.githubUnlink(id).then(refreshGithub)
      else await api.productiveUnlink(id).then(refreshProductive)
      toast.success(`Unlinked from ${LINK_LABEL[kind]}`)
    })

  const linkVeranda = async (body: { room: string; minutes: number; title?: string; note?: string }) => {
    const l = await api.verandaLink({ host, session: sid, ...body })
    shown.set(key, { room: l.room, passphrase: l.passphrase, url: l.url })
    setTick((n) => n + 1)
    toast.success('Linked to Veranda', { description: `room ${l.room} until ${clock(l.until)} — pass the passphrase on to the guests` })
    await refreshVeranda()
  }

  const follow = async (kind: 'github' | 'productive', body: { url: string; task?: string; keep?: string | null }) => {
    const l = kind === 'github' ? await api.githubLink({ host, session: sid, ...body }) : await api.productiveLink({ host, session: sid, ...body })
    const what = kind === 'github' ? `${(l as GithubLink).repo}#${l.number}` : `task #${l.number ?? (l as ProductiveLink).taskId}`
    toast.success(l.renewed ? `${LINK_LABEL[kind]} link updated` : `Linked to ${LINK_LABEL[kind]}`, { description: [what, l.title].filter(Boolean).join(' · ') })
    await (kind === 'github' ? refreshGithub() : refreshProductive())
  }

  const submit = async () => {
    if (!draft?.kind || busy) return
    const d = draft
    const url = d.url.trim()
    const note = d.note.trim()
    const ok = await act('link', d.editing ? 'Save' : `Link to ${LINK_LABEL[d.kind!]}`, async () => {
      if (d.kind === 'slack') {
        const l = await api.slackLink({ host, session: sid, url, minutes: d.slackMinutes, mode: d.mode, ...(note ? { task: note } : {}) })
        toast.success(l.renewed ? 'Slack link renewed' : 'Linked to Slack', { description: slackUntil(l.until) })
        await refreshSlack()
      } else if (d.kind === 'veranda') {
        await linkVeranda({ room: url, minutes: d.minutes, ...(d.title.trim() ? { title: d.title.trim() } : {}), ...(note ? { note } : {}) })
      } else {
        await follow(d.kind!, { url, keep: d.keep, ...(note || d.editing ? { task: note } : {}) })
      }
    })
    if (ok) setDraft(null)
  }

  const edit = (kind: 'github' | 'productive', l: GithubLink | ProductiveLink) =>
    setDraft({ ...freshDraft(), kind, picked: true, url: l.url, editing: l.id, note: l.task, keep: keepValue(l.keep) })

  const setUrl = (url: string) =>
    setDraft((d) => {
      if (!d) return d
      const found = detectLinkKind(url, addable)
      return { ...d, url: d.kind === 'veranda' ? url.toLowerCase() : url, kind: d.picked ? d.kind : (found ?? d.kind) }
    })

  const kind = draft?.kind ?? null
  const url = draft?.url.trim() ?? ''
  const valid =
    kind === 'veranda'
      ? ROOM_RE.test(url)
      : kind === 'github'
        ? detectLinkKind(url, ['github']) === 'github'
        : kind === 'productive'
          ? detectLinkKind(url, ['productive']) === 'productive' || /^#?\d+$/.test(url)
          : url !== ''

  return (
    <section className="py-1">
      <h3 className="pt-2 pb-1 text-[0.6875rem] font-semibold tracking-wider text-dimmer uppercase">Links</h3>
      <div className="divide-y divide-border/60">
        {secret ? (
          <LinkRow kind="veranda" icon={KeyRoundIcon} title={<code className="font-mono select-all">{secret.passphrase}</code>} sub={`Passphrase of room ${secret.room} — shown only now; pass it on to the guests`}>
            <Button
              variant="outline"
              size="lg"
              onClick={() => void copyText(secret.url ? `${secret.url}\nRoom: ${secret.room}\nPassphrase: ${secret.passphrase}` : secret.passphrase).then((ok) => ok && toast.success('Copied'))}
            >
              <CopyIcon />
              Copy
            </Button>
            <Button
              variant="ghost"
              size="lg"
              aria-label="Hide the passphrase"
              onClick={() => {
                shown.delete(key)
                setTick((n) => n + 1)
              }}
            >
              <XIcon />
            </Button>
          </LinkRow>
        ) : null}

        {slackLinks.map((l) => (
          <LinkRow
            key={l.id}
            kind="slack"
            href={l.permalink}
            title={`${slackPlaceName(l)} · ${l.until == null ? 'no end' : `${slackLeft(l.until, now)} left`}`}
            sub={[l.mode === 'proactive' ? 'Proactive' : 'When mentioned', l.task || 'the work of this session', `${l.delivered} in · ${l.replies} out`, l.pending ? `${l.pending} waiting` : null]}
          >
            <UnlinkButton busy={busy === l.id} disabled={busy != null} onClick={() => unlink('slack', l.id)} />
          </LinkRow>
        ))}

        {ghLinks.map((l) => (
          <LinkRow key={l.id} kind="github" href={l.url} title={`${l.repo}#${l.number}${l.title ? ` · ${l.title}` : ''}`} sub={followSub(l, `@${l.login}`)}>
            <EditButton disabled={busy != null} onClick={() => edit('github', l)} />
            <UnlinkButton busy={busy === l.id} disabled={busy != null} onClick={() => unlink('github', l.id)} />
          </LinkRow>
        ))}

        {ptLinks.map((l) => (
          <LinkRow key={l.id} kind="productive" href={l.url} title={`#${l.number ?? l.taskId}${l.title ? ` · ${l.title}` : ''}`} sub={followSub(l, l.me?.name ?? null)}>
            <EditButton disabled={busy != null} onClick={() => edit('productive', l)} />
            <UnlinkButton busy={busy === l.id} disabled={busy != null} onClick={() => unlink('productive', l.id)} />
          </LinkRow>
        ))}

        {verandaLink ? (
          <LinkRow
            kind="veranda"
            href={verandaLink.url ?? undefined}
            title={`Room ${verandaLink.room} · ${slackLeft(verandaLink.until, now)} left`}
            sub={[
              verandaLink.title,
              verandaLink.state ? VERANDA_STATE[verandaLink.state] : null,
              `${verandaLink.delivered} in · ${verandaLink.replies} out`,
              verandaLink.pending ? `${verandaLink.pending} waiting` : null,
              verandaLink.note || null,
            ]}
          >
            <Button
              variant="outline"
              size="lg"
              disabled={busy != null}
              onClick={() =>
                void act('extend', 'Extend', async () => {
                  const l = await api.verandaExtend(verandaLink.room, 60)
                  toast.success('Veranda link extended', { description: `room ${l.room}, ${slackLeft(l.until, Date.now())} left` })
                  await refreshVeranda()
                })
              }
            >
              {busy === 'extend' ? <Loader2Icon className="animate-spin" /> : <ClockPlusIcon />}
              +1h
            </Button>
            <UnlinkButton busy={busy === verandaLink.id} disabled={busy != null} onClick={() => unlink('veranda', verandaLink.id, verandaLink.room)} />
          </LinkRow>
        ) : null}

        {pastSlack.map((l) => (
          <LinkRow key={l.id} kind="slack" muted href={l.permalink} title={`${slackPlaceName(l)} · ${l.endReason === 'expired' ? 'expired' : 'unlinked'}`} sub={[l.mode === 'proactive' ? 'Proactive' : 'When mentioned', l.task || 'the work of this session']}>
            <AgainButton
              busy={busy === l.id}
              disabled={busy != null}
              label={`Link again · ${slackDuration(l.minutes)}`}
              onClick={() =>
                void act(l.id, 'Link again', async () => {
                  const n = await api.slackRelink(l.id)
                  toast.success('Linked to Slack again', { description: slackUntil(n.until) })
                  await refreshSlack()
                })
              }
            />
          </LinkRow>
        ))}

        {pastGithub.map((l) => (
          <LinkRow key={l.id} kind="github" muted href={l.url} title={`${l.repo}#${l.number} · ${endLabel(l.endReason)}`} sub={[l.title, l.task || 'the work of this session']}>
            <AgainButton busy={busy === l.id} disabled={busy != null} onClick={() => void act(l.id, 'Link again', () => follow('github', { url: l.url, task: l.task, keep: keepValue(l.keep) }))} />
          </LinkRow>
        ))}

        {pastProductive.map((l) => (
          <LinkRow key={l.id} kind="productive" muted href={l.url} title={`#${l.number ?? l.taskId} · ${endLabel(l.endReason)}`} sub={[l.title, l.task || 'the work of this session']}>
            <AgainButton busy={busy === l.id} disabled={busy != null} onClick={() => void act(l.id, 'Link again', () => follow('productive', { url: l.url, task: l.task, keep: keepValue(l.keep) }))} />
          </LinkRow>
        ))}

        {pastVeranda.map((l) => (
          <LinkRow key={l.id} kind="veranda" muted title={`Room ${l.room} · ${endLabel(l.endReason)}`} sub={[l.title, l.note || 'no instructions']}>
            <AgainButton
              busy={busy === l.id}
              disabled={busy != null}
              label={`Link again · ${slackDuration(l.minutes)}`}
              onClick={() => void act(l.id, 'Link to Veranda', () => linkVeranda({ room: l.room, minutes: l.minutes, ...(l.title ? { title: l.title } : {}), ...(l.note ? { note: l.note } : {}) }))}
            />
          </LinkRow>
        ))}

        {draft ? (
          <div className="flex flex-col gap-2 py-2">
            {draft.editing ? (
              <div className="truncate text-sm">
                {LINK_LABEL[draft.kind!]} · <span className="text-muted-foreground">{draft.url}</span>
              </div>
            ) : (
              <>
                <Input
                  autoFocus
                  value={draft.url}
                  onChange={(e) => setUrl(e.target.value)}
                  onKeyDown={(e) => e.key === 'Enter' && valid && void submit()}
                  placeholder={addable.map((k) => PLACEHOLDER[k]).join(', ')}
                  aria-invalid={kind != null && url !== '' && !valid}
                  className="h-9 text-sm"
                />
                {addable.length > 1 ? (
                  <ToggleGroup
                    type="single"
                    value={kind ?? ''}
                    onValueChange={(v) => v && setDraft((d) => d && { ...d, kind: v as LinkKind, picked: true, url: v === 'veranda' ? d.url.toLowerCase() : d.url })}
                    aria-label="Link to"
                    className="flex-wrap justify-start"
                  >
                    {addable.map((k) => {
                      const Icon = ICON[k]
                      return (
                        <ToggleGroupItem key={k} value={k} className={chip}>
                          <Icon className="size-3.5" />
                          {LINK_LABEL[k]}
                        </ToggleGroupItem>
                      )
                    })}
                  </ToggleGroup>
                ) : null}
              </>
            )}

            {kind === 'slack' ? (
              <>
                <ToggleGroup type="single" value={String(draft.slackMinutes)} onValueChange={(v) => v && setDraft((d) => d && { ...d, slackMinutes: toSlackMinutes(v) })} aria-label="How long" className="flex-wrap justify-start">
                  {SLACK_DURATIONS.map((o) => (
                    <ToggleGroupItem key={o.minutes} value={String(o.minutes)} className={chip}>
                      {o.label}
                    </ToggleGroupItem>
                  ))}
                </ToggleGroup>
                <ToggleGroup type="single" value={draft.mode} onValueChange={(v) => v && setDraft((d) => d && { ...d, mode: v as Draft['mode'] })} aria-label="When it speaks" className="flex-wrap justify-start">
                  <ToggleGroupItem value="mention" title="Woken when someone mentions the app or DMs it, and by messages in threads it has posted in; the rest goes along as context" className={chip}>
                    When mentioned
                  </ToggleGroupItem>
                  <ToggleGroupItem value="proactive" title="Sees every message and steps in when it judges it should (your instructions say how much)" className={chip}>
                    Proactive
                  </ToggleGroupItem>
                </ToggleGroup>
              </>
            ) : kind === 'veranda' ? (
              <>
                <ToggleGroup type="single" value={String(draft.minutes)} onValueChange={(v) => v && setDraft((d) => d && { ...d, minutes: Number(v) })} aria-label="How long" className="flex-wrap justify-start">
                  {VERANDA_DURATIONS.map((o) => (
                    <ToggleGroupItem key={o.minutes} value={String(o.minutes)} className={chip}>
                      {o.label}
                    </ToggleGroupItem>
                  ))}
                </ToggleGroup>
                <Input value={draft.title} onChange={(e) => setDraft((d) => d && { ...d, title: e.target.value })} maxLength={120} placeholder="Title the guests see (optional)" className="h-9 text-sm" />
              </>
            ) : kind === 'github' || kind === 'productive' ? (
              <div className="flex flex-wrap items-center gap-2">
                <span className="text-xs text-dimmer">Sleep after idle</span>
                <ToggleGroup type="single" value={draft.keep ?? 'never'} onValueChange={(v) => v && setDraft((d) => d && { ...d, keep: v === 'never' ? null : v })} aria-label="Sleep after idle" className="flex-wrap justify-start">
                  {KEEP_OPTIONS.map((o) => (
                    <ToggleGroupItem key={o.label} value={o.value ?? 'never'} title={o.value ? `Sleeps after ${o.label} idle; the next event wakes it` : 'Never put to sleep'} className={chip}>
                      {o.label}
                    </ToggleGroupItem>
                  ))}
                </ToggleGroup>
              </div>
            ) : null}

            {kind ? (
              <Textarea
                value={draft.note}
                onChange={(e) => setDraft((d) => d && { ...d, note: e.target.value })}
                rows={3}
                maxLength={1000}
                placeholder={
                  kind === 'veranda'
                    ? 'Instructions: what it may help the guests with, and what not'
                    : kind === 'slack'
                      ? 'Instructions: what it may help with, tone, when to step in (default: the work of this session)'
                      : 'Instructions: what to do with it (default: the work of this session)'
                }
                className="min-h-0 text-sm"
              />
            ) : null}

            <p className="text-xs text-dimmer">{hint(draft, kind, url, valid, addable, github?.host ?? productive?.host ?? null)}</p>

            <div className="flex justify-end gap-2">
              <Button variant="ghost" size="lg" onClick={() => setDraft(null)}>
                Cancel
              </Button>
              <Button size="lg" disabled={!kind || !valid || busy != null} onClick={() => void submit()}>
                {busy === 'link' ? <Loader2Icon className="animate-spin" /> : draft.editing ? <PencilIcon /> : <Link2Icon />}
                {draft.editing ? 'Save' : 'Link'}
              </Button>
            </div>
          </div>
        ) : addable.length ? (
          <div className="flex min-h-12 items-center justify-between gap-3 py-1.5">
            <div className="min-w-0">
              <div className="text-sm">{slackLinks.length || ghLinks.length || ptLinks.length || verandaLink ? 'Another link' : 'Not linked'}</div>
              <div className="truncate text-xs text-dimmer">Let people talk to this session from {joinOr(addable.map((k) => LINK_LABEL[k]))}</div>
            </div>
            <Button variant="outline" size="lg" onClick={() => setDraft(freshDraft())}>
              <PlusIcon />
              Add link
            </Button>
          </div>
        ) : null}
      </div>
    </section>
  )
}

function LinkRow({
  kind,
  icon,
  title,
  sub,
  href,
  muted = false,
  children,
}: {
  kind: LinkKind
  icon?: LucideIcon
  title: ReactNode
  sub: string | (string | null | undefined | false)[]
  href?: string
  muted?: boolean
  children: ReactNode
}) {
  const Icon = icon ?? ICON[kind]
  const subText = Array.isArray(sub) ? sub.filter(Boolean).join(' · ') : sub
  const head = (
    <>
      <Icon className={muted ? 'size-3.5 shrink-0 text-dimmer' : 'size-3.5 shrink-0 text-primary'} aria-label={LINK_LABEL[kind]} />
      <span className="truncate">{title}</span>
      {href ? <ExternalLinkIcon className="size-3 shrink-0 text-dimmer" /> : null}
    </>
  )
  const cls = `inline-flex max-w-full min-w-0 items-center gap-1 text-sm ${muted ? 'text-muted-foreground' : ''}`
  return (
    <div className="flex min-h-12 flex-wrap items-center justify-between gap-x-3 gap-y-1 py-1.5">
      <div className="min-w-0 flex-1">
        {href ? (
          <a href={href} target="_blank" rel="noreferrer" className={`${cls} hover:underline`}>
            {head}
          </a>
        ) : (
          <div className={cls}>{head}</div>
        )}
        {subText ? <div className="truncate text-xs text-dimmer">{subText}</div> : null}
      </div>
      <div className="flex shrink-0 gap-2">{children}</div>
    </div>
  )
}

function UnlinkButton({ busy, disabled, onClick }: { busy: boolean; disabled: boolean; onClick: () => void }) {
  return (
    <Button variant="outline" size="lg" disabled={disabled} onClick={onClick}>
      {busy ? <Loader2Icon className="animate-spin" /> : <UnlinkIcon />}
      Unlink
    </Button>
  )
}

function EditButton({ disabled, onClick }: { disabled: boolean; onClick: () => void }) {
  return (
    <Button variant="outline" size="lg" aria-label="Edit the link" disabled={disabled} onClick={onClick}>
      <PencilIcon />
    </Button>
  )
}

function AgainButton({ busy, disabled, onClick, label = 'Link again' }: { busy: boolean; disabled: boolean; onClick: () => void; label?: string }) {
  return (
    <Button variant="outline" size="lg" disabled={disabled} onClick={onClick}>
      {busy ? <Loader2Icon className="animate-spin" /> : <Link2Icon />}
      {label}
    </Button>
  )
}

/** A GitHub / Productive link's second line: as whom, instructions, sleep, traffic. */
const followSub = (l: GithubLink | ProductiveLink, as: string | null) => [
  as ? `as ${as}` : null,
  l.task || 'the work of this session',
  keepLabel(l.keep),
  l.asleep ? 'asleep' : null,
  `${l.delivered} in`,
  l.pending ? `${l.pending} waiting` : null,
]

function hint(d: Draft, kind: LinkKind | null, url: string, valid: boolean, addable: LinkKind[], bridge: string | null) {
  if (!kind) return url ? `Not a link this session can have — paste ${joinOr(addable.map((k) => PLACEHOLDER[k]))}.` : `Paste ${joinOr(addable.map((k) => PLACEHOLDER[k]))}.`
  if (url && !valid) {
    if (kind === 'veranda') return 'A room is 1–48 lowercase letters, digits or dashes.'
    if (kind === 'github') return 'A pull request: https://github.com/<owner>/<repo>/pull/<n> or <owner>/<repo>#<n>.'
    if (kind === 'productive') return 'A task: its URL from Productive, or its id.'
  }
  if (kind === 'slack') {
    const how = d.mode === 'proactive' ? 'It sees every message there and posts when it has something useful, as the Slack app.' : 'It answers when someone mentions the app or DMs it, or, in a thread it has posted in, talks to it.'
    return `${how} ${url.startsWith('http') ? 'Invite the app first.' : url ? 'A person: the app DMs with them.' : ''}`.trim()
  }
  if (kind === 'veranda') return 'Opens the room with a new passphrase (shown here once). Guests with a Veranda account and the passphrase talk to this session until the link ends.'
  const who = kind === 'github' ? 'reviews, comments and pushes on the PR' : 'comments and changes on the task'
  const ends = kind === 'github' ? 'the PR is merged or closed' : 'the task is closed'
  return `${d.editing ? 'New instructions and sleep for this link. ' : ''}It gets the ${who} (throttled) and answers there as the bridge's account${bridge ? ` on ${bridge}` : ''}, until ${ends} or you unlink it.`
}

const slackPlaceName = (l: SlackLink) => (l.dm ? `DM with ${l.dm.name}` : l.threadTs ? 'Thread' : 'Conversation')

const slackUntil = (until: number | null) => (until == null ? 'until you unlink it' : `until ${clock(until)}`)

const endLabel = (r: string | null | undefined) => END_LABEL[r ?? ''] ?? r ?? 'ended'

const clock = (t: number) => new Date(t).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })

const joinOr = (xs: string[]) => (xs.length <= 1 ? (xs[0] ?? '') : `${xs.slice(0, -1).join(', ')} or ${xs.at(-1)}`)
