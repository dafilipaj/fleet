import { HostLoadPanel } from '@/components/HostLoadPanel'
import { useEffect, useRef, useState, type ReactNode } from 'react'
import {
  ActivityIcon,
  BotIcon,
  ChevronLeftIcon,
  ChevronRightIcon,
  FolderOpenIcon,
  GaugeIcon,
  GitBranchIcon,
  InboxIcon,
  KeyboardIcon,
  LayersIcon,
  MessageSquareIcon,
  MonitorIcon,
  MoonIcon,
  PaletteIcon,
  SunIcon,
  XIcon,
} from 'lucide-react'
import { useLocation, useRoute } from 'wouter'

import { api } from '@/api/client'
import type { InboxSettings, InboxUsage, InboxUsageSpan, InboxUsageTotals } from '@/api/types'
import { ModelsEditor } from '@/components/ModelsEditor'
import { ReposEditor } from '@/components/ReposEditor'
import { ScreenHeader } from '@/components/ScreenHeader'
import { SlackSpawnEditor } from '@/components/SlackSpawnEditor'
import { SpawnDirsEditor } from '@/components/SpawnDirsEditor'
import { StackSettingsEditor } from '@/components/stack/StackSettingsEditor'
import { Button } from '@/components/ui/button'
import { Kbd } from '@/components/ui/kbd'
import { Switch } from '@/components/ui/switch'
import { Textarea } from '@/components/ui/textarea'
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

type SectionId = 'general' | 'models' | 'start-dirs' | 'repos' | 'stacks' | 'inbox' | 'slack' | 'load' | 'status'

/** Settings' sub-pages, in nav order: `#/settings/<id>`. */
const SECTIONS: { id: SectionId; label: string; hint: string; icon: ReactNode }[] = [
  { id: 'general', label: 'General', hint: 'Text size, theme, chat — this browser', icon: <PaletteIcon /> },
  { id: 'models', label: 'Models', hint: 'Default model and effort per harness', icon: <BotIcon /> },
  { id: 'start-dirs', label: 'Start directories', hint: 'Where new sessions can start', icon: <FolderOpenIcon /> },
  { id: 'repos', label: 'Git repos', hint: 'Repos fleet keeps in sync', icon: <GitBranchIcon /> },
  { id: 'stacks', label: 'Session stacks', hint: 'The lead prompt and sibling reports', icon: <LayersIcon /> },
  { id: 'inbox', label: 'Inbox', hint: 'Scan schedule, model, rules and usage', icon: <InboxIcon /> },
  { id: 'slack', label: 'Slack', hint: 'Sessions started from a mention', icon: <MessageSquareIcon /> },
  { id: 'load', label: 'Host load', hint: 'Busy hosts, what uses them, when new sessions wait', icon: <GaugeIcon /> },
  { id: 'status', label: 'Status', hint: 'Hosts and diagnostics', icon: <ActivityIcon /> },
]

/**
 * `#/settings[/<section>]`: one sub-page per section (`SECTIONS`). General is the viewer's own
 * preferences (localStorage); Models, Start directories, Git repos and Inbox are stored in the
 * hosts' configs. `screen` = the mobile page: `#/settings` lists the sections, a section opens as
 * its own page (back / swipe back to the list). `pane` = the desktop main pane: the sections in a
 * side nav next to the open one (General by default; ✕ / Esc back to `#/`).
 */
export function SettingsScreen({ layout = 'screen' }: { layout?: 'screen' | 'pane' }) {
  const pane = layout === 'pane'
  const [, navigate] = useLocation()
  const [, params] = useRoute('/settings/:section')
  const active = SECTIONS.find((s) => s.id === params?.section) ?? null
  const current = active ?? (pane ? SECTIONS[0] : null)

  const back = () => {
    if (!pane && active) navigate('/settings')
    else if (!pane && window.history.length > 1) window.history.back()
    else navigate('/', { replace: !pane })
  }
  const screenRef = useRef<HTMLDivElement>(null)
  useSwipeBack(screenRef, back, !pane)

  const body = current ? (
    <div className="mx-auto w-full max-w-lg px-4 pt-2 pb-[max(1.5rem,env(safe-area-inset-bottom))] md:max-w-2xl">
      <SectionBody id={current.id} />
    </div>
  ) : (
    <SectionList />
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
        <div className="flex min-h-0 flex-1">
          <nav aria-label="Settings sections" className="w-52 shrink-0 overflow-y-auto border-r p-2">
            {SECTIONS.map((s) => (
              <a
                key={s.id}
                href={`#/settings/${s.id}`}
                aria-current={s.id === current?.id ? 'page' : undefined}
                className={cn(
                  'flex h-9 items-center gap-2.5 rounded-lg px-2.5 text-sm text-muted-foreground hover:bg-muted/60 hover:text-foreground [&_svg]:size-4 [&_svg]:shrink-0',
                  s.id === current?.id && 'bg-muted font-medium text-foreground',
                )}
              >
                {s.icon}
                <span className="truncate">{s.label}</span>
              </a>
            ))}
          </nav>
          <div className="min-h-0 min-w-0 flex-1 overflow-y-auto">{body}</div>
        </div>
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
          <h1 className="text-xl font-bold tracking-tight">{active ? active.label : 'Settings'}</h1>
        </div>
      </ScreenHeader>
      {body}
    </div>
  )
}

/** Mobile `#/settings`: one row per section. */
function SectionList() {
  return (
    <nav aria-label="Settings sections" className="mx-auto w-full max-w-lg px-4 pt-2 pb-[max(1.5rem,env(safe-area-inset-bottom))]">
      <div className="divide-y divide-border/60">
        {SECTIONS.map((s) => (
          <a key={s.id} href={`#/settings/${s.id}`} className="flex min-h-14 items-center gap-3 py-2 [&>svg:first-child]:size-5 [&>svg:first-child]:text-dimmer">
            {s.icon}
            <span className="min-w-0 flex-1">
              <span className="block text-sm">{s.label}</span>
              <span className="block truncate text-xs text-dimmer">{s.hint}</span>
            </span>
            <ChevronRightIcon className="size-4 shrink-0 text-dimmer" />
          </a>
        ))}
      </div>
    </nav>
  )
}

function SectionBody({ id }: { id: SectionId }) {
  switch (id) {
    case 'general':
      return <GeneralSettings />
    case 'models':
      return (
        <Section title="Default model & effort">
          <ModelsEditor />
        </Section>
      )
    case 'start-dirs':
      return (
        <Section title="Start directories">
          <SpawnDirsEditor />
        </Section>
      )
    case 'repos':
      return (
        <Section title="Git repos">
          <ReposEditor />
        </Section>
      )
    case 'stacks':
      return (
        <Section title="Session stacks">
          <StackSettingsEditor />
        </Section>
      )
    case 'inbox':
      return <InboxSchedule />
    case 'slack':
      return (
        <Section title="Sessions from Slack">
          <SlackSpawnEditor />
        </Section>
      )
    case 'load':
      return (
        <Section title="Host load">
          <HostLoadPanel />
        </Section>
      )
    case 'status':
      return (
        <>
          <FleetStatus />
          <ViewportInfo />
        </>
      )
  }
}

/** Settings → General: this viewer's preferences, stored in this browser (localStorage). */
function GeneralSettings() {
  const { theme, setTheme } = useTheme()
  const desktop = useIsDesktop()
  const { textSize, setTextSize, hideNotes, setHideNotes, sendDelay, setSendDelay } = usePrefs()
  return (
    <>
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
    </>
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
const BRIEF_EVERY = [15, 30, 45, 60, 120] as const
const INBOX_MODELS = ['haiku', 'sonnet', 'opus'] as const
const HOURS = Array.from({ length: 25 }, (_, h) => h)

/** Settings → Inbox: what the Slack & email scan does and when, its model, and what the inbox spends (saved in the inbox host's config, live at once). */
function InboxSchedule() {
  const { data, reload } = useInbox()
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const cur = data?.enabled ? data.settings : undefined
  if (!cur) {
    return (
      <Section title="Inbox">
        <p className="py-3 text-sm text-dimmer">{data ? 'The inbox is not set up on this fleet.' : 'Loading…'}</p>
      </Section>
    )
  }
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
  const withCurrent = <T extends number | string>(opts: readonly T[], v: T | undefined) =>
    v == null || opts.includes(v) ? [...opts] : [...opts, v]
  const comms = cur.comms !== false
  const poller = Boolean(data?.sources.slack)
  return (
    <>
      {poller ? (
        <Section title="Slack">
          <p className="pt-2 text-xs text-dimmer">
            Read as you with the Slack user token. A conversation leaves the list as soon as you answer it or react to it, with no model; only conversations with new messages from others go to the model.
          </p>
          <Stacked label="Check every" hint="Any time of day; free when nothing is new">
            <ToggleGroup
              type="single"
              value={String(cur.slackMinutes ?? 2)}
              onValueChange={(v) => v && void save({ slackMinutes: Number(v) })}
              disabled={saving}
              spacing={0}
              aria-label="Slack interval"
              className="w-full rounded-lg bg-muted p-1"
            >
              {withCurrent([2, 5, 10, 30] as readonly number[], cur.slackMinutes ?? 2)
                .sort((a, b) => a - b)
                .map((m) => (
                  <ToggleGroupItem key={m} value={String(m)} className={segItem}>
                    {SCAN_EVERY_LABELS[m] ?? `${m}m`}
                  </ToggleGroupItem>
                ))}
            </ToggleGroup>
          </Stacked>
          <Stacked label="Model" hint="Sorts only the conversations with new messages, from a short digest (no tools)">
            <ModelPick value={cur.slackModel ?? 'haiku'} options={withCurrent(INBOX_MODELS as readonly string[], cur.slackModel ?? 'haiku')} disabled={saving} onChange={(slackModel) => void save({ slackModel })} label="Slack model" />
          </Stacked>
          <RulesEditor
            key={`slack:${cur.slackRules ?? ''}`}
            label="Your rules"
            hintOwn="Added to the model's instructions (they win over its defaults)"
            hintDefault="Optional: e.g. which channels never need you, who always does"
            value={cur.slackRules ?? null}
            fallback=""
            disabled={saving}
            onSave={(slackRules) => save({ slackRules })}
          />
        </Section>
      ) : null}
      <Section title={poller ? 'Email scan' : 'Slack & email scan'}>
        {cur.comms != null ? (
          <div className="flex items-center justify-between gap-4 py-3">
            <div>
              <div className="text-sm">{poller ? 'Scan email' : 'Scan Slack & email'}</div>
              <div className="text-xs text-dimmer">Through the claude.ai connectors; off: no connector scan</div>
            </div>
            <Switch checked={comms} disabled={saving} onCheckedChange={(v) => void save({ comms: v })} aria-label="Scan Slack and email" />
          </div>
        ) : null}
        <Stacked label="Every" hint={`On ${data?.host}; one model call with tools per scan`}>
          <ToggleGroup
            type="single"
            value={String(cur.scanMinutes)}
            onValueChange={(v) => v && void save({ scanMinutes: Number(v) })}
            disabled={saving || !comms}
            spacing={0}
            aria-label="Scan interval"
            className="w-full rounded-lg bg-muted p-1"
          >
            {withCurrent(SCAN_EVERY as readonly number[], cur.scanMinutes)
              .sort((a, b) => a - b)
              .map((m) => (
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
        {cur.model ? (
          <Stacked label="Model" hint="It reads with tools, turn by turn: Sonnet costs several times Haiku here">
            <ModelPick value={cur.model} options={withCurrent(INBOX_MODELS as readonly string[], cur.model)} disabled={saving} onChange={(model) => void save({ model })} label="Scan model" />
          </Stacked>
        ) : null}
        {cur.defaultScanRules ? (
          <RulesEditor
            key={`scan:${cur.scanRules ?? ''}`}
            label="Rules"
            hintOwn="Your own rules (the read-only rule, the time window and the answer format stay fixed)"
            hintDefault="The default rules; edit to change what counts and how answered threads are told apart"
            value={cur.scanRules ?? null}
            fallback={cur.defaultScanRules}
            disabled={saving}
            onSave={(scanRules) => save({ scanRules })}
          />
        ) : null}
      </Section>
      {cur.briefModel ? (
        <Section title="Brief">
          <Stacked label="Model">
            <ModelPick value={cur.briefModel} options={withCurrent(INBOX_MODELS as readonly string[], cur.briefModel)} disabled={saving} onChange={(briefModel) => void save({ briefModel })} label="Brief model" />
          </Stacked>
          <Stacked label="At most every" hint="Only when the list changed; outside work hours once a day">
            <ToggleGroup
              type="single"
              value={String(cur.briefMinutes)}
              onValueChange={(v) => v && void save({ briefMinutes: Number(v) })}
              disabled={saving}
              spacing={0}
              aria-label="Brief interval"
              className="w-full rounded-lg bg-muted p-1"
            >
              {withCurrent(BRIEF_EVERY as readonly number[], cur.briefMinutes)
                .sort((a, b) => a - b)
                .map((m) => (
                  <ToggleGroupItem key={m} value={String(m)} className={segItem}>
                    {SCAN_EVERY_LABELS[m] ?? `${m}m`}
                  </ToggleGroupItem>
                ))}
            </ToggleGroup>
          </Stacked>
        </Section>
      ) : null}
      {error ? <p className="py-2 text-xs text-status-error">{error}</p> : null}
      {data?.usage ? <InboxUsagePanel usage={data.usage} /> : null}
    </>
  )
}

function ModelPick({ value, options, disabled, onChange, label }: { value: string; options: string[]; disabled: boolean; onChange: (v: string) => void; label: string }) {
  return (
    <ToggleGroup
      type="single"
      value={value}
      onValueChange={(v) => v && v !== value && onChange(v)}
      disabled={disabled}
      spacing={0}
      aria-label={label}
      className="w-full rounded-lg bg-muted p-1"
    >
      {options.map((m) => (
        <ToggleGroupItem key={m} value={m} className={cn(segItem, 'capitalize')}>
          {m}
        </ToggleGroupItem>
      ))}
    </ToggleGroup>
  )
}

/** Editable model rules (the scan's, or the Slack sort's extra ones). Empty or the fallback = the default (null). */
function RulesEditor({
  label,
  hintOwn,
  hintDefault,
  value,
  fallback,
  disabled,
  onSave,
}: {
  label: string
  hintOwn: string
  hintDefault: string
  value: string | null
  fallback: string
  disabled: boolean
  onSave: (v: string | null) => Promise<void>
}) {
  const [draft, setDraft] = useState(value ?? fallback)
  const dirty = draft.trim() !== (value ?? fallback).trim()
  return (
    <Stacked label={label} hint={value ? hintOwn : hintDefault}>
      <Textarea value={draft} onChange={(e) => setDraft(e.target.value)} disabled={disabled} className="max-h-96 font-mono text-xs" aria-label={label} />
      <div className="flex gap-2">
        <Button size="sm" disabled={disabled || !dirty} onClick={() => void onSave(draft)}>
          Save
        </Button>
        {dirty ? (
          <Button size="sm" variant="ghost" disabled={disabled} onClick={() => setDraft(value ?? fallback)}>
            Cancel
          </Button>
        ) : null}
        {value ? (
          <Button size="sm" variant="ghost" disabled={disabled} onClick={() => void onSave(null)}>
            {fallback ? 'Reset to default' : 'Clear'}
          </Button>
        ) : null}
      </div>
    </Stacked>
  )
}

const fmtTokens = (n: number) => (n >= 1e6 ? `${(n / 1e6).toFixed(1)}M` : n >= 1e3 ? `${Math.round(n / 1e3)}k` : String(n))
const fmtUsd = (n: number) => `$${n < 1 ? n.toFixed(3) : n.toFixed(2)}`
const allTokens = (t: InboxUsageTotals) => t.input + t.output + t.cacheRead + t.cacheWrite

/** What the inbox's own model calls spent: today, 7 and 30 days, and the last calls. */
function InboxUsagePanel({ usage }: { usage: InboxUsage }) {
  const spans: [string, InboxUsageSpan][] = [
    ['Today', usage.today],
    ['7 days', usage.week],
    ['30 days', usage.month],
  ]
  const hasSlack = Boolean(usage.month.slack?.runs)
  return (
    <Section title="Usage">
      <p className="pt-2 text-xs text-dimmer">Tokens as Claude Code reports them (cache reads included); cost is its API-price estimate — on a subscription it counts against your limits instead.</p>
      <table className="my-2 w-full text-sm tabular-nums">
        <thead>
          <tr className="text-left text-xs text-dimmer">
            <th className="py-1 font-normal" />
            <th className="py-1 text-right font-normal">Scans</th>
            {hasSlack ? <th className="py-1 text-right font-normal">Slack</th> : null}
            <th className="py-1 text-right font-normal">Briefs</th>
            <th className="py-1 text-right font-normal">Tokens</th>
            <th className="py-1 text-right font-normal">Output</th>
            <th className="py-1 text-right font-normal">Cost</th>
          </tr>
        </thead>
        <tbody>
          {spans.map(([label, s]) => (
            <tr key={label} className="border-t border-border/60">
              <td className="py-1.5">{label}</td>
              <td className="py-1.5 text-right">{s.scan.runs}</td>
              {hasSlack ? <td className="py-1.5 text-right">{s.slack?.runs ?? 0}</td> : null}
              <td className="py-1.5 text-right">{s.brief.runs}</td>
              <td className="py-1.5 text-right">{fmtTokens(allTokens(s.total))}</td>
              <td className="py-1.5 text-right">{fmtTokens(s.total.output)}</td>
              <td className="py-1.5 text-right">{fmtUsd(s.total.costUsd)}</td>
            </tr>
          ))}
        </tbody>
      </table>
      {usage.runs.length ? (
        <details className="py-2 text-xs">
          <summary className="cursor-pointer text-dimmer">Last {usage.runs.length} calls</summary>
          <ul className="mt-2 space-y-1 tabular-nums">
            {usage.runs.map((r, i) => (
              <li key={`${r.at}-${i}`} className="flex gap-2">
                <span className="w-24 shrink-0 text-dimmer">{new Date(r.at).toLocaleString([], { weekday: 'short', hour: '2-digit', minute: '2-digit' })}</span>
                <span className="w-10 shrink-0">{r.kind}</span>
                <span className="w-14 shrink-0 text-dimmer">{r.model}</span>
                <span className="w-14 shrink-0 text-right">{fmtTokens((r.input ?? 0) + (r.output ?? 0) + (r.cacheRead ?? 0) + (r.cacheWrite ?? 0))}</span>
                <span className="w-14 shrink-0 text-right">{r.costUsd != null ? fmtUsd(r.costUsd) : '–'}</span>
                <span className="text-dimmer">{r.durationMs != null ? `${Math.round(r.durationMs / 1000)}s` : ''}{r.turns ? ` · ${r.turns} turns` : ''}{r.ok ? '' : ' · failed'}</span>
              </li>
            ))}
          </ul>
        </details>
      ) : null}
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
