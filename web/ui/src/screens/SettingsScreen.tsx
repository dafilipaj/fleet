import { useEffect, useRef, useState, type ReactNode } from 'react'
import { ChevronLeftIcon, KeyboardIcon, MonitorIcon, MoonIcon, SunIcon, XIcon } from 'lucide-react'
import { useLocation } from 'wouter'

import { api } from '@/api/client'
import type { InboxSettings } from '@/api/types'
import { ReposEditor } from '@/components/ReposEditor'
import { ScreenHeader } from '@/components/ScreenHeader'
import { SpawnDirsEditor } from '@/components/SpawnDirsEditor'
import { Button } from '@/components/ui/button'
import { Kbd } from '@/components/ui/kbd'
import { Switch } from '@/components/ui/switch'
import { ToggleGroup, ToggleGroupItem } from '@/components/ui/toggle-group'
import { useInbox } from '@/hooks/useInbox'
import { useIsDesktop } from '@/hooks/useMediaQuery'
import { useNow } from '@/hooks/useNow'
import { usePrefs } from '@/hooks/usePrefs'
import { useSessionList } from '@/hooks/useSessionList'
import { useSwipeBack } from '@/hooks/useSwipeBack'
import { useTheme, type ThemeChoice } from '@/hooks/useTheme'
import { WEEKDAYS } from '@/lib/inbox'
import { SEND_DELAYS, SEND_DELAY_LABELS } from '@/lib/outbox'
import { PALETTES, parsePalette } from '@/lib/palettes'
import { TEXT_SIZES, TEXT_SIZE_LABELS } from '@/lib/prefs'
import { shortcutHint, showShortcuts } from '@/lib/shortcuts'
import { cn } from '@/lib/utils'

const segItem =
  'h-9 flex-1 gap-1.5 rounded-md px-3 text-[0.8125rem] text-muted-foreground data-[state=on]:bg-background data-[state=on]:text-foreground data-[state=on]:shadow-sm dark:data-[state=on]:bg-accent'

/**
 * `#/settings`: the viewer's global preferences — text size (scales the whole app), theme
 * (colour palette), mode (light/dark), progress notes, send delay. Stored in this browser (localStorage).
 * Plus the fleet's Start directories (the New session form's list) and Git repos (`fleet repos`), stored in each host's config. `screen` = the mobile page
 * (back button, swipe back); `pane` = the desktop main pane (✕ / Esc back to `#/`).
 */
export function SettingsScreen({ layout = 'screen' }: { layout?: 'screen' | 'pane' }) {
  const pane = layout === 'pane'
  const [, navigate] = useLocation()
  const { theme, setTheme } = useTheme()
  const desktop = useIsDesktop()
  const { textSize, setTextSize, hideNotes, setHideNotes, sendDelay, setSendDelay } = usePrefs()

  const back = () => {
    if (!pane && window.history.length > 1) window.history.back()
    else navigate('/', { replace: !pane })
  }
  const screenRef = useRef<HTMLDivElement>(null)
  useSwipeBack(screenRef, back, !pane)

  const content = (
    <div className="mx-auto w-full max-w-lg px-4 pt-2 pb-[max(1.5rem,env(safe-area-inset-bottom))] md:max-w-2xl">
      <Section title="Appearance">
        <Stacked label="Text size" hint="Scales the whole app">
          <ToggleGroup
            type="single"
            value={String(textSize)}
            onValueChange={(v) => v && setTextSize(Number(v))}
            spacing={0}
            aria-label="Text size"
            className="w-full rounded-lg bg-muted p-1"
          >
            {TEXT_SIZES.map((n) => (
              <ToggleGroupItem key={n} value={String(n)} className={segItem}>
                {TEXT_SIZE_LABELS[n] ?? `${n}px`}
              </ToggleGroupItem>
            ))}
          </ToggleGroup>
        </Stacked>
        <Stacked label="Theme" hint="Colours for the whole app">
          <ThemePicker />
        </Stacked>
        <Stacked label="Mode" hint="Light or dark; every theme has both">
          <ToggleGroup
            type="single"
            value={theme}
            onValueChange={(v) => v && setTheme(v as ThemeChoice)}
            spacing={0}
            aria-label="Mode"
            className="w-full rounded-lg bg-muted p-1"
          >
            <ToggleGroupItem value="system" className={segItem}>
              <MonitorIcon /> System
            </ToggleGroupItem>
            <ToggleGroupItem value="dark" className={segItem}>
              <MoonIcon /> Dark
            </ToggleGroupItem>
            <ToggleGroupItem value="light" className={segItem}>
              <SunIcon /> Light
            </ToggleGroupItem>
          </ToggleGroup>
        </Stacked>
      </Section>

      <Section title="Chat">
        <div className="flex min-h-12 items-center justify-between gap-3 py-2">
          <div className="min-w-0">
            <div className="text-sm">Progress notes</div>
            <div className="text-xs text-dimmer">Claude’s narration between tool calls</div>
          </div>
          <Switch checked={!hideNotes} onCheckedChange={(v) => setHideNotes(!v)} aria-label="Show progress notes" />
        </div>
        <Stacked label="Send delay" hint="Time to undo a message (Esc or Undo) before it goes out">
          <ToggleGroup
            type="single"
            value={String(sendDelay)}
            onValueChange={(v) => v && setSendDelay(Number(v))}
            spacing={0}
            aria-label="Send delay"
            className="w-full rounded-lg bg-muted p-1"
          >
            {SEND_DELAYS.map((ms) => (
              <ToggleGroupItem key={ms} value={String(ms)} className={segItem}>
                {SEND_DELAY_LABELS[ms]}
              </ToggleGroupItem>
            ))}
          </ToggleGroup>
        </Stacked>
      </Section>

      {desktop ? (
        <Section title="Keyboard">
          <div className="flex min-h-12 items-center justify-between gap-3 py-2">
            <div className="min-w-0">
              <div className="text-sm">Keyboard shortcuts</div>
              <div className="text-xs text-dimmer">
                Jump to a session with <Kbd>{shortcutHint('palette')}</Kbd>, every shortcut with <Kbd>{shortcutHint('help')}</Kbd>
              </div>
            </div>
            <Button variant="outline" size="sm" onClick={showShortcuts}>
              <KeyboardIcon /> Show
            </Button>
          </div>
        </Section>
      ) : null}

      <p className="pt-4 text-xs text-dimmer">Saved in this browser; every session uses them.</p>
      <Section title="Start directories">
        <SpawnDirsEditor />
      </Section>
      <Section title="Git repos">
        <ReposEditor />
      </Section>
      <InboxSchedule />
      <FleetStatus />
      <ViewportInfo />
    </div>
  )

  if (pane) {
    return (
      <section aria-label="Settings" className="flex h-full min-w-0 flex-1 flex-col bg-background">
        <header
          data-tauri-drag-region="deep"
          className="titlebar titlebar-lead flex shrink-0 items-center gap-1 border-b py-1.5 pr-2 pl-4 [--titlebar-pad:1rem]"
        >
          <h1 className="min-w-0 flex-1 text-[0.9375rem] leading-tight font-bold">Settings</h1>
          <Button variant="ghost" size="icon" aria-label="Close settings" title="Close (Esc)" onClick={back} className="size-11 shrink-0 rounded-xl">
            <XIcon className="size-5" />
          </Button>
        </header>
        <div className="min-h-0 flex-1 overflow-y-auto">{content}</div>
      </section>
    )
  }

  return (
    <div ref={screenRef} className="flex min-h-app flex-col bg-background">
      <ScreenHeader>
        <div className="-ml-2 flex min-h-8 items-center gap-1">
          <Button variant="ghost" size="icon" aria-label="Back" onClick={back} className="-my-1.5 size-11 shrink-0 rounded-xl">
            <ChevronLeftIcon className="size-6" />
          </Button>
          <h1 className="text-xl font-bold tracking-tight">Settings</h1>
        </div>
      </ScreenHeader>
      {content}
    </div>
  )
}

/**
 * Theme (colour palette) picker: a radio group of swatches, each previewing that palette's
 * page / card / primary in the current mode. Arrow keys move between them (radix roving focus).
 */
function ThemePicker() {
  const { palette, setPalette, resolved } = useTheme()
  return (
    <ToggleGroup
      type="single"
      value={palette}
      onValueChange={(v) => v && setPalette(parsePalette(v))}
      aria-label="Theme"
      className="grid w-full grid-cols-4 gap-2"
    >
      {PALETTES.map((p) => {
        const sw = p.swatch[resolved]
        return (
          <ToggleGroupItem
            key={p.id}
            value={p.id}
            title={p.hint}
            className="h-auto min-w-0 flex-col items-stretch gap-1.5 rounded-xl border bg-transparent p-1.5 pb-1 text-[0.8125rem] font-normal text-muted-foreground hover:bg-muted/60 aria-checked:border-primary aria-checked:bg-transparent aria-checked:text-foreground aria-checked:ring-2 aria-checked:ring-primary/30 data-[state=on]:bg-transparent"
          >
            <span
              aria-hidden
              className="flex h-12 flex-col justify-between rounded-lg border border-black/10 p-1.5 dark:border-white/10"
              style={{ background: sw.background }}
            >
              <span className="h-3 rounded-sm" style={{ background: sw.card }} />
              <span className="flex items-center gap-1">
                <span className="h-2.5 w-7 rounded-full" style={{ background: sw.primary }} />
                <span className="h-1.5 flex-1 rounded-full opacity-60" style={{ background: sw.card }} />
              </span>
            </span>
            <span className="truncate text-center">{p.label}</span>
          </ToggleGroupItem>
        )
      })}
    </ToggleGroup>
  )
}

const SCAN_EVERY = [15, 30, 60, 120, 240] as const
const SCAN_EVERY_LABELS: Record<number, string> = { 15: '15m', 30: '30m', 60: '1h', 120: '2h', 240: '4h' }
const HOURS = Array.from({ length: 25 }, (_, h) => h)

/** Settings → Inbox: when the Slack & email scan runs (saved in the inbox host's config, live at once). */
function InboxSchedule() {
  const { data, reload } = useInbox()
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const cur = data?.enabled ? data.settings : undefined
  if (!cur) return null
  const save = async (patch: Partial<InboxSettings>) => {
    setSaving(true)
    setError(null)
    try {
      await api.setInboxSettings(patch)
      reload()
    } catch (err) {
      setError((err as Error)?.message || 'could not save')
    } finally {
      setSaving(false)
    }
  }
  const [from, to] = cur.workHours
  const select = 'h-8 rounded-lg border bg-card px-2 text-sm outline-none focus-visible:ring-2 focus-visible:ring-ring/50'
  return (
    <Section title="Inbox">
      <Stacked label="Scan Slack & email every" hint={`On ${data?.host}; costs one small model call per scan`}>
        <ToggleGroup
          type="single"
          value={String(cur.scanMinutes)}
          onValueChange={(v) => v && void save({ scanMinutes: Number(v) })}
          disabled={saving}
          spacing={0}
          aria-label="Scan interval"
          className="w-full rounded-lg bg-muted p-1"
        >
          {(SCAN_EVERY.includes(cur.scanMinutes as (typeof SCAN_EVERY)[number]) ? SCAN_EVERY : [...SCAN_EVERY, cur.scanMinutes].sort((a, b) => a - b)).map((m) => (
            <ToggleGroupItem key={m} value={String(m)} className={segItem}>
              {SCAN_EVERY_LABELS[m] ?? `${m}m`}
            </ToggleGroupItem>
          ))}
        </ToggleGroup>
      </Stacked>
      <Stacked label="Between" hint="Local time on the inbox host; outside it only Scan now scans">
        <div className="flex items-center gap-2 text-sm">
          <select aria-label="From" value={from} disabled={saving} className={select} onChange={(e) => void save({ workHours: [Number(e.target.value), to] })}>
            {HOURS.filter((h) => h < to).map((h) => (
              <option key={h} value={h}>
                {String(h).padStart(2, '0')}:00
              </option>
            ))}
          </select>
          <span className="text-dimmer">–</span>
          <select aria-label="To" value={to} disabled={saving} className={select} onChange={(e) => void save({ workHours: [from, Number(e.target.value)] })}>
            {HOURS.filter((h) => h > from).map((h) => (
              <option key={h} value={h}>
                {String(h).padStart(2, '0')}:00
              </option>
            ))}
          </select>
        </div>
      </Stacked>
      <Stacked label="On">
        <ToggleGroup
          type="multiple"
          value={cur.workDays.map(String)}
          onValueChange={(v) => v.length && void save({ workDays: v.map(Number) })}
          disabled={saving}
          spacing={0}
          aria-label="Scan days"
          className="w-full rounded-lg bg-muted p-1"
        >
          {[1, 2, 3, 4, 5, 6, 0].map((d) => (
            <ToggleGroupItem key={d} value={String(d)} className={segItem}>
              {WEEKDAYS[d]}
            </ToggleGroupItem>
          ))}
        </ToggleGroup>
      </Stacked>
      {error ? <p className="py-2 text-xs text-status-error">{error}</p> : null}
    </Section>
  )
}

function Section({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="py-1">
      <h2 className="pt-3 pb-1 text-[0.6875rem] font-semibold tracking-wider text-dimmer uppercase">{title}</h2>
      <div className="divide-y divide-border/60">{children}</div>
    </section>
  )
}

function Stacked({ label, hint, children }: { label: string; hint?: string; children: ReactNode }) {
  return (
    <div className="space-y-2 py-3">
      <div>
        <div className="text-sm">{label}</div>
        {hint ? <div className="text-xs text-dimmer">{hint}</div> : null}
      </div>
      {children}
    </div>
  )
}

const PROBES = ['100vh', '100lvh', '100svh', '100dvh', 'var(--app-h)'] as const

/** Viewport numbers (for layout bugs on phones / home-screen apps): what the browser reports. */
function ViewportInfo() {
  const [lines, setLines] = useState<string[]>([])
  const [outline, setOutline] = useState(false)
  useEffect(() => {
    const measure = () => {
      const probe = (css: Partial<CSSStyleDeclaration>) => {
        const el = document.createElement('div')
        Object.assign(el.style, { position: 'fixed', visibility: 'hidden', pointerEvents: 'none', left: '0', top: '0', width: '1px' }, css)
        document.body.appendChild(el)
        const r = el.getBoundingClientRect()
        const cs = getComputedStyle(el)
        el.remove()
        return { h: Math.round(r.height), top: Math.round(r.top), cs }
      }
      const inset = probe({ bottom: '0' })
      const safe = probe({ paddingTop: 'env(safe-area-inset-top)', paddingBottom: 'env(safe-area-inset-bottom)', height: '0' }).cs
      const vv = window.visualViewport
      const nav = navigator as Navigator & { standalone?: boolean }
      setLines([
        `standalone ${nav.standalone === true || window.matchMedia('(display-mode: standalone)').matches}`,
        `screen ${screen.width}×${screen.height} · dpr ${window.devicePixelRatio}`,
        `inner ${window.innerWidth}×${window.innerHeight} · client ${document.documentElement.clientHeight}`,
        `visualViewport ${vv ? `${Math.round(vv.height)} @${Math.round(vv.offsetTop)}` : '—'}`,
        `fixed inset-0 ${inset.h} @${inset.top}`,
        ...PROBES.map((v) => `${v} ${probe({ height: v }).h}`),
        `safe top ${safe.paddingTop} · bottom ${safe.paddingBottom}`,
      ])
    }
    measure()
    window.addEventListener('resize', measure)
    window.visualViewport?.addEventListener('resize', measure)
    return () => {
      window.removeEventListener('resize', measure)
      window.visualViewport?.removeEventListener('resize', measure)
    }
  }, [])
  return (
    <details className="pt-4 text-xs text-dimmer">
      <summary className="cursor-pointer">Viewport diagnostics</summary>
      <pre className="pt-2 font-mono text-[0.6875rem] leading-relaxed whitespace-pre-wrap">{lines.join('\n')}</pre>
      <label className="flex items-center gap-2 pt-1">
        <Switch checked={outline} onCheckedChange={setOutline} aria-label="Show viewport outlines" />
        Outlines: red = fixed inset-0, blue = app height
      </label>
      {outline ? (
        <>
          <div aria-hidden className="pointer-events-none fixed inset-0 z-[100] border-4 border-red-500" />
          <div aria-hidden className="pointer-events-none fixed inset-x-6 top-0 z-[100] h-app border-4 border-blue-500" />
        </>
      ) : null}
    </details>
  )
}

/** Connection status: how fresh the list is and which hosts answer (the list header only shows a ⚠ on trouble). */
function FleetStatus() {
  const now = useNow(5_000)
  const { note, hosts } = useSessionList(now)
  return (
    <Section title="Status">
      <div className="flex min-h-12 items-center justify-between gap-3 py-2">
        <div className="text-sm">Session list</div>
        <div className={cn('text-right text-xs tabular-nums', note.error ? 'text-destructive' : 'text-dimmer')} aria-live="polite">
          {note.text || '—'}
        </div>
      </div>
      {hosts.map((h) => (
        <div key={h.name} className="flex min-h-12 items-center justify-between gap-3 py-2">
          <div className="text-sm">{h.name}</div>
          <div className={cn('min-w-0 truncate text-right text-xs', h.ok === false ? 'text-destructive' : 'text-dimmer')}>
            {h.ok === false ? `unreachable: ${h.error || 'no response'}` : `${h.sessions?.length ?? 0} sessions`}
          </div>
        </div>
      ))}
    </Section>
  )
}
