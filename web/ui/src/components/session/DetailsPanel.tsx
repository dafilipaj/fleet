import { useEffect, useRef, useState, type ReactNode } from 'react'
import {
  ArrowRightIcon,
  CopyIcon,
  FileDiffIcon,
  LayersIcon,
  Link2Icon,
  Loader2Icon,
  PowerIcon,
  RefreshCwIcon,
  SquareTerminalIcon,
} from 'lucide-react'
import { toast } from 'sonner'

import { api, sessionErrorMessage } from '@/api/client'
import type { Session } from '@/api/types'
import { BriefSection, type BriefSectionProps } from '@/components/session/BriefSection'
import { Button } from '@/components/ui/button'
import { Drawer, DrawerContent, DrawerDescription, DrawerHeader, DrawerTitle } from '@/components/ui/drawer'
import { useSessionTitle } from '@/hooks/useTitles'
import { canAttachInApp, copyWithToast, sessionAttachCommand, sessionAttachLink } from '@/lib/clipboard'
import { ctxLevel, ctxSummary, shortCwd } from '@/lib/format'
import { harnessLabel } from '@/lib/harness'
import { CTX_TEXT } from '@/lib/styles'
import { cn } from '@/lib/utils'

export interface DetailsPanelProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  host: string
  id: string
  session: Session | null
  /** Called after a successful close (navigate away). */
  onClosed: () => void
  /** The brief at the top of the panel. */
  brief: Omit<BriefSectionProps, 'desktop' | 'cancelEditRef'>
  /** "New session from here…": the form with Hand off / Sibling (absent: no session row yet). */
  onNewFromHere?: () => void
  /** "Review changes": the diff reviewer (desktop panel; on mobile it is a header button). */
  onReview?: () => void
  /** Session stacks: the label and the StackBrief sheet (when in one), and "Link…" (put another live session in it). */
  stack?: { label: string | null; onOpen?: () => void; onLink: () => void } | null
}

function Row({ label, hint, children }: { label: string; hint?: ReactNode; children: ReactNode }) {
  return (
    <div className="flex min-h-12 items-center justify-between gap-3 py-1.5">
      <div className="min-w-0">
        <div className="text-sm">{label}</div>
        {hint ? <div className="truncate text-xs text-dimmer">{hint}</div> : null}
      </div>
      <div className="flex shrink-0 items-center gap-1.5">{children}</div>
    </div>
  )
}

function Section({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="py-1">
      <h3 className="pt-2 pb-1 text-[0.6875rem] font-semibold tracking-wider text-dimmer uppercase">{title}</h3>
      <div className="divide-y divide-border/60">{children}</div>
    </section>
  )
}

/** ⋯ on mobile: the Details panel as a bottom drawer. Esc while editing the brief cancels the edit, not the drawer. */
export function DetailsDrawer(p: DetailsPanelProps) {
  const cancelEdit = useRef<(() => void) | null>(null)
  // Closing the drawer unmounts the body, which disarms "Close session".
  return (
    <Drawer open={p.open} onOpenChange={p.onOpenChange}>
      <DrawerContent
        className="px-safe"
        onEscapeKeyDown={(e) => {
          if (!cancelEdit.current) return
          e.preventDefault()
          cancelEdit.current()
        }}
      >
        <DetailsPanel {...p} variant="drawer" cancelEditRef={cancelEdit} />
      </DrawerContent>
    </Drawer>
  )
}

/**
 * Details: header (name + regenerate, host · cwd, context), the Brief (summary,
 * todos, resources), New session from here…, Session (attach, ids, close). `drawer` = inside the mobile drawer; `panel` = the desktop
 * details column (click-to-confirm wording, ✕ to close, no drawer chrome).
 */
export function DetailsPanel(p: DetailsPanelProps & { variant: 'drawer' | 'panel'; cancelEditRef?: React.RefObject<(() => void) | null> }) {
  const panel = p.variant === 'panel'
  const [naming, setNaming] = useState(false)

  // Regenerate: a fresh model-picked name for this session (`fleet name <id> --apply --refresh`).
  const regenerate = async () => {
    if (naming) return
    setNaming(true)
    try {
      const r = await api.regenerateName(p.host, p.id)
      toast(r.name ? `Renamed to ${r.name}` : 'Name unchanged')
    } catch (err) {
      toast.error((err as Error)?.message || 'Naming failed')
    } finally {
      setNaming(false)
    }
  }

  // Close: two taps. The first arms (for 5s), the second kills Claude + its terminal.
  const [armed, setArmed] = useState(false)
  const [closing, setClosing] = useState(false)
  const armTimer = useRef<ReturnType<typeof setTimeout>>(undefined)
  useEffect(() => () => clearTimeout(armTimer.current), [])
  const onOpenChange = (open: boolean) => {
    if (!open && !closing) {
      clearTimeout(armTimer.current)
      setArmed(false)
    }
    p.onOpenChange(open)
  }

  const closeSession = async () => {
    if (closing) return
    if (!armed) {
      setArmed(true)
      clearTimeout(armTimer.current)
      armTimer.current = setTimeout(() => setArmed(false), 5000)
      return
    }
    clearTimeout(armTimer.current)
    setClosing(true)
    try {
      const res = await api.kill(p.host, p.id)
      toast.success(`Closed ${res.name || p.id.slice(0, 8)}`, { description: String(res.terminal || 'done').replace(/-/g, ' ') })
      onOpenChange(false)
      p.onClosed()
    } catch (err) {
      toast.error('Close failed', { description: sessionErrorMessage(err) })
      setClosing(false)
      setArmed(false)
    }
  }

  const s = p.session
  const { title } = useSessionTitle(s, `${p.host}/${p.id}`)
  const attachCmd = sessionAttachCommand(s)
  const attachLink = canAttachInApp() ? sessionAttachLink(s) : null
  const Header = panel ? 'div' : DrawerHeader
  const Title = panel ? 'h2' : DrawerTitle
  const Description = panel ? 'p' : DrawerDescription

  return (
        <div
          className={
            panel
              ? 'no-scrollbar h-full w-full overflow-y-auto px-4 pb-4'
              : 'no-scrollbar mx-auto w-full max-w-lg overflow-y-auto px-4 pb-[max(1rem,env(safe-area-inset-bottom))]'
          }
        >
          <Header className={panel ? 'flex flex-row items-start gap-1 pt-3 pb-1 text-left' : 'flex flex-row items-start gap-1 px-0 pt-3 pb-1 text-left'}>
            <div className="flex min-w-0 flex-1 flex-col gap-0.5">
              <div className="flex min-w-0 items-center gap-1">
                <Title className="truncate text-left text-base font-semibold">{s ? title : p.id.slice(0, 8)}</Title>
                {s ? (
                  <Button variant="ghost" size="icon" aria-label="Regenerate name" title="Regenerate name" disabled={naming} onClick={regenerate} className="size-7 shrink-0 rounded-md text-muted-foreground">
                    <RefreshCwIcon className={cn('size-3.5', naming && 'animate-spin')} />
                  </Button>
                ) : null}
              </div>
              <Description className={cn('truncate text-left font-mono text-xs', panel && 'text-muted-foreground')} title={panel ? (s?.cwd ?? undefined) : undefined}>
                {[p.host, s?.cwd ? shortCwd(s.cwd, 60) : null].filter(Boolean).join(' · ')}
              </Description>
              {s?.context ? (
                <p className={cn('truncate text-left text-xs tabular-nums', CTX_TEXT[ctxLevel(s.context.pct)])}>
                  Context {ctxSummary(s.context)}
                  {s.context.model ? <span className="text-dimmer"> · {s.context.model}</span> : null}
                </p>
              ) : null}
            </div>
          </Header>

          <BriefSection {...p.brief} desktop={panel} cancelEditRef={p.cancelEditRef} />

          {p.onReview ? (
            <Button variant="outline" className="mt-1 h-10 w-full rounded-xl" title="Review changes (diff)" onClick={p.onReview}>
              <FileDiffIcon />
              Review changes
            </Button>
          ) : null}

          {p.onNewFromHere ? (
            <Button variant="outline" className="mt-1 h-10 w-full rounded-xl" onClick={p.onNewFromHere}>
              New session from here…
              <ArrowRightIcon />
            </Button>
          ) : null}

          <Section title="Session">
            {p.stack ? (
              <Row label="Session stack" hint={p.stack.label ?? 'Not in a stack'}>
                {p.stack.onOpen ? (
                  <Button variant="outline" className="h-9 px-3" onClick={p.stack.onOpen}>
                    <LayersIcon />
                    StackBrief
                  </Button>
                ) : null}
                <Button variant="outline" className="h-9 px-3" title="Put another live session in this stack (or start one)" onClick={p.stack.onLink}>
                  <Link2Icon />
                  Link…
                </Button>
              </Row>
            ) : null}
            {s ? (
              <div className="py-1.5">
                <Row
                  label={attachLink ? 'Attach' : 'Attach command'}
                  hint={attachCmd ? (attachLink ? 'Opens an iTerm tab attached to its tmux session' : 'Run in a terminal on any fleet host') : 'Only for tmux sessions — this one is an iTerm tab'}
                >
                  <div className="flex gap-1.5">
                    <Button variant={attachLink ? 'ghost' : 'outline'} className="h-9 px-3" disabled={!attachCmd} aria-label="Copy attach command" title="Copy attach command" onClick={() => attachCmd && void copyWithToast(attachCmd)}>
                      <CopyIcon />
                      Copy
                    </Button>
                    {attachLink ? (
                      <Button asChild className="h-9 px-3">
                        <a href={attachLink} title={attachCmd ?? undefined}>
                          <SquareTerminalIcon />
                          Attach
                        </a>
                      </Button>
                    ) : null}
                  </div>
                </Row>
                {attachCmd ? (
                  <code className="block rounded-md bg-muted px-2.5 py-2 font-mono text-xs break-all text-muted-foreground select-all">{attachCmd}</code>
                ) : null}
              </div>
            ) : null}
            {s ? (
              <div className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 py-3 text-xs">
                {(
                  [
                    ['agent', s.harness ? harnessLabel(s.harness) : null],
                    ['model', s.model],
                    ['tmux', s.tmux_session],
                    ['backend', s.backend],
                    ['pid', s.pid],
                    ['session', s.session_id],
                  ] as const
                )
                  .filter(([, v]) => v != null && v !== '')
                  .map(([k, v]) => (
                    <div key={k} className="contents">
                      <span className="text-dimmer">{k}</span>
                      <span className="truncate font-mono text-muted-foreground select-all">{String(v)}</span>
                    </div>
                  ))}
              </div>
            ) : null}
            <div className="py-3">
              <Button
                variant="destructive"
                className={cn('h-11 w-full text-sm', armed && 'bg-destructive text-white hover:bg-destructive/90 dark:bg-destructive dark:hover:bg-destructive/90')}
                disabled={closing}
                onClick={closeSession}
              >
                {closing ? <Loader2Icon className="animate-spin" /> : <PowerIcon />}
                {closing ? 'Closing…' : armed ? (panel ? 'Click again to close' : 'Tap again to close') : 'Close session…'}
              </Button>
              <p className="pt-1.5 text-center text-xs text-dimmer">
                Ends {harnessLabel(s?.harness)} and its {s?.backend === 'iterm' ? 'iTerm tab' : 'tmux window'}.
              </p>
            </div>
          </Section>
        </div>
  )
}
