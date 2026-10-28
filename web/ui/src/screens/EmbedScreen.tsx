import { useState } from 'react'
import { CircleAlertIcon, FolderIcon, Loader2Icon, PlusIcon } from 'lucide-react'
import { Redirect, Route, Switch, useLocation } from 'wouter'

import type { Session } from '@/api/types'
import { HostBadge } from '@/components/HostBadge'
import { NewSessionDrawer } from '@/components/NewSessionDrawer'
import { StatusDot } from '@/components/StatusDot'
import { Button } from '@/components/ui/button'
import { useFleet } from '@/hooks/useFleet'
import { useNow } from '@/hooks/useNow'
import { useSessionTitle } from '@/hooks/useTitles'
import { embedHash, resolveEmbedDir, sessionsIn, type EmbedParams } from '@/lib/embed'
import { relTime, sessionSubtitle, shortCwd } from '@/lib/format'
import { sessionHref, statusLabel, statusMeta } from '@/lib/sessions'
import { STATUS_TEXT } from '@/lib/styles'
import { echoesTitle } from '@/lib/title'
import { cn } from '@/lib/utils'
import { SessionScreen } from '@/screens/SessionScreen'

/**
 * `#/embed?host=…&dir=…` — the sessions working in one directory and their chat, for another app
 * to frame (docs/architecture.md → Embed view). No Fleet navigation: the list, a session
 * (`#/s/…`, the mobile session screen without Review / stacks), back. Every move replaces the
 * history entry, so the framing page's Back button stays its own.
 */
export function EmbedScreen({ params }: { params: EmbedParams }) {
  const [, navigate] = useLocation()
  const home = embedHash(params)
  const toList = () => navigate(home, { replace: true })
  return (
    <Switch>
      <Route path="/embed">
        <EmbedList params={params} />
      </Route>
      <Route path="/s/:host/:id">
        {(p) =>
          p.host === params.host ? (
            <SessionScreen key={`${p.host}/${p.id}`} host={p.host} id={p.id} embedded onBack={toList} />
          ) : (
            <Redirect to={home} replace />
          )
        }
      </Route>
      <Route>
        <Redirect to={home} replace />
      </Route>
    </Switch>
  )
}

function EmbedList({ params }: { params: EmbedParams }) {
  const { fleet, error } = useFleet()
  const [, navigate] = useLocation()
  const now = useNow(5000)
  const [spawning, setSpawning] = useState(false)
  const hostEntry = fleet?.hosts.find((h) => h.name === params.host) ?? null
  const dir = hostEntry ? resolveEmbedDir(params.dir, hostEntry) : null
  const sessions = dir?.ok ? sessionsIn(fleet, params.host, dir.path) : []
  const name = params.dir.replace(/\/+$/, '').split('/').pop() || params.dir

  const problem = !fleet
    ? error
    : !hostEntry
      ? `${params.host} is not in this Fleet.`
      : hostEntry.ok === false
        ? `${params.host} unreachable: ${hostEntry.error || 'no response'}`
        : dir && !dir.ok
          ? dir.error
          : null

  return (
    <div className="fixed-app flex flex-col overflow-hidden bg-background">
      <header className="z-20 shrink-0 border-b bg-background/90 pt-safe px-safe backdrop-blur-md">
        <div className="flex items-center gap-2 py-2 pr-2 pl-4">
          <FolderIcon className="size-4 shrink-0 text-dimmer" />
          <div className="min-w-0 flex-1">
            <div className="truncate text-[0.9375rem] leading-tight font-bold">{name}</div>
            <div className="mt-0.5 flex min-w-0 items-center gap-1.5 text-xs text-dimmer">
              <HostBadge host={params.host} className="h-4 px-1 text-[0.625rem]" />
              <span className="truncate font-mono text-[0.6875rem]">{dir?.ok ? shortCwd(dir.path, 60) : params.dir}</span>
            </div>
          </div>
          <Button size="lg" disabled={!dir?.ok} onClick={() => setSpawning(true)}>
            <PlusIcon /> New session
          </Button>
        </div>
      </header>

      <main className="no-scrollbar min-h-0 flex-1 overflow-y-auto px-safe">
        <div className="mx-auto w-full max-w-3xl space-y-2 p-3 pb-[max(0.75rem,env(safe-area-inset-bottom))]">
          {problem ? (
            <div className="flex items-start gap-2 rounded-lg border border-destructive/40 bg-destructive/10 px-3 py-2 text-xs text-destructive">
              <CircleAlertIcon className="mt-px size-3.5 shrink-0" />
              <span className="break-words">{problem}</span>
            </div>
          ) : !fleet || !dir ? (
            <div className="flex items-center justify-center gap-2 py-10 text-sm text-dimmer">
              <Loader2Icon className="size-4 animate-spin" /> Loading sessions…
            </div>
          ) : sessions.length === 0 ? (
            <div className="flex flex-col items-center gap-3 py-10 text-center text-sm text-dimmer">
              <p>No sessions in {shortCwd(dir.ok ? dir.path : params.dir, 60)} yet.</p>
              <Button variant="outline" size="lg" onClick={() => setSpawning(true)}>
                <PlusIcon /> Start one
              </Button>
            </div>
          ) : (
            sessions.map((s) => (
              <EmbedRow key={s.session_id} session={s} now={now} onOpen={() => navigate(sessionHref(s), { replace: true })} />
            ))
          )}
        </div>
      </main>

      {dir?.ok ? (
        <NewSessionDrawer
          open={spawning}
          onOpenChange={setSpawning}
          onOpenSession={(href) => navigate(href, { replace: true })}
          prefill={{ host: params.host, dir: dir.path, fixed: true, embed: params.app }}
        />
      ) : null}
    </div>
  )
}

function EmbedRow({ session: s, now, onOpen }: { session: Session; now: number; onOpen: () => void }) {
  const meta = statusMeta(s.status)
  const { title } = useSessionTitle(s)
  const prompt = sessionSubtitle(s)
  const subtitle = echoesTitle(title, prompt) ? '' : prompt
  return (
    <button
      type="button"
      onClick={onOpen}
      className="block w-full rounded-xl border bg-card px-3.5 py-3 text-left text-card-foreground transition-colors outline-none hover:bg-muted/60 focus-visible:ring-3 focus-visible:ring-ring/50 active:bg-muted"
    >
      <div className="flex min-w-0 items-center gap-2">
        <StatusDot status={s.status} className="size-2" />
        <span className="min-w-0 flex-1 truncate text-sm font-semibold">{title}</span>
        <span className="shrink-0 text-xs text-dimmer tabular-nums">{relTime(s.updated_at, now)}</span>
      </div>
      <div className="mt-1 flex min-w-0 items-center gap-1.5 text-xs">
        <span className={cn('shrink-0', STATUS_TEXT[meta.key])}>{statusLabel(s)}</span>
        {subtitle ? <span className="min-w-0 truncate text-muted-foreground">· {subtitle}</span> : null}
      </div>
    </button>
  )
}
