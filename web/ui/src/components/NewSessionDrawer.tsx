import { useEffect, useRef, useState } from 'react'
import { ArrowRightIcon, FolderIcon, GitForkIcon, LayersIcon, Loader2Icon, MicIcon, PaperclipIcon, PlayIcon, SquareIcon } from 'lucide-react'
import { toast } from 'sonner'

import { api, ApiError } from '@/api/client'
import { watchForSpawned } from '@/api/spawnWatch'
import { DropOverlay } from '@/components/DropOverlay'
import { HostDot } from '@/components/HostBadge'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Kbd } from '@/components/ui/kbd'
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from '@/components/ui/dialog'
import { Drawer, DrawerContent, DrawerDescription, DrawerHeader, DrawerTitle } from '@/components/ui/drawer'
import { Label } from '@/components/ui/label'
import { Textarea } from '@/components/ui/textarea'
import { ToggleGroup, ToggleGroupItem } from '@/components/ui/toggle-group'
import { useAttach, useFileDrop } from '@/hooks/useAttach'
import { useFleet } from '@/hooks/useFleet'
import { useGhIdentities, useHarnesses } from '@/hooks/useHarnesses'
import { useSettings } from '@/hooks/useSettings'
import { useVoice } from '@/hooks/useVoice'
import type { SiblingTarget } from '@/hooks/useStackUi'
import { withContinueTask } from '@/lib/brief'
import { copyText } from '@/lib/clipboard'
import { shortCwd } from '@/lib/format'
import { harnessLabel, modelKey, offeredHarnesses, pickHarness } from '@/lib/harness'
import { pickModel } from '@/lib/models'
import { storage } from '@/lib/storage'
import { sessionHref, spawnTargets } from '@/lib/sessions'
import { isMacPlatform, isSubmitChord } from '@/lib/shortcuts'
import { stackErrorMessage, stacksMissing } from '@/lib/stacks'
import { cn } from '@/lib/utils'
import { appendTranscript, formatElapsed } from '@/lib/voice'

const HOST_KEY = 'fleet.spawnHost'
const dirKey = (host: string) => `fleet.spawnDirLabel.${host}`
const HARNESS_KEY = 'fleet.spawnHarness'
// The "GitHub as" choice for the user's own identity (no identity name can contain a space).
const ME_IDENTITY = 'me (you)'
// "New session from here": the last mode picked (Hand off / Sibling).
const FROM_MODE_KEY = 'fleet.spawnFromMode'
// Past this many models a harness's picker is a dropdown, not chips (pi lists dozens).
const MODEL_CHIPS_MAX = 8
// The unsent first prompt of a plain New session (not a prefilled "continue"), so closing the form,
// a reload, a crash or a failed spawn never loses it; cleared once the started session registers. Stale after a week.
const DRAFT_KEY = 'fleet.spawnDraft'
const DRAFT_MAX_AGE_MS = 7 * 24 * 60 * 60 * 1000

function loadDraft(): string {
  const d = storage.getJSON<{ prompt?: unknown; at?: unknown }>(DRAFT_KEY)
  if (!d || typeof d.prompt !== 'string' || typeof d.at !== 'number' || Date.now() - d.at > DRAFT_MAX_AGE_MS) return ''
  return d.prompt
}

function saveDraft(prompt: string) {
  if (prompt.trim()) storage.setJSON(DRAFT_KEY, { prompt, at: Date.now() })
  else storage.remove(DRAFT_KEY)
}
/** A started session registered: drop the draft it came from (not a newer one typed since). */
function clearDraftIf(sent: string) {
  if (loadDraft() === sent) storage.remove(DRAFT_KEY)
}

/**
 * A spawn that never showed up: its prompt becomes the draft (a "continue" / sibling one too,
 * unless a newer draft exists) and the toast offers to copy it.
 */
function notShownUp(name: string, id: string | number, sent: string) {
  const kept = sent.trim() !== ''
  if (kept && !loadDraft().trim()) saveDraft(sent)
  toast.error(`${name} has not shown up yet`, {
    id,
    description: kept ? 'Check the list in a moment. Your prompt is kept in New session.' : 'Check the list in a moment',
    ...(kept ? { action: { label: 'Copy prompt', onClick: () => void copyText(sent) } } : {}),
  })
}

// Radix ToggleGroup treats '' as "nothing selected": the default model ('' = no --model) needs a stand-in.
const DEFAULT_MODEL_VALUE = '__default'

// The extra directory row a prefill adds when its dir is not one of the host's spawnDirs.
const PREFILL_DIR_LABEL = 'Same as the session'

/** "New session from here": start on this host, in this directory, with this first prompt. Nothing is remembered. */
export interface SpawnPrefill {
  host: string
  /** Absolute on `host` (a session's cwd); must be one of its spawnDirs or beneath one, else the spawn is a 400. */
  dir?: string | null
  prompt?: string
  /** Host and dir are given, not chosen (the embed view): no Host / Directory field. */
  fixed?: boolean
  /** The embed view's app: the new session is labelled with it (`embed`, set aside in the Apps filter). */
  embed?: string
  /** Offer Sibling beside Hand off: the same prompt, spawned into this session's stack instead. */
  sibling?: SiblingTarget | null
}

export interface NewSessionDrawerProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  /** Open the new session once it registers (`#/s/…`). */
  onOpenSession: (href: string) => void
  /** Start from these values (the caret goes to the end of the prompt). */
  prefill?: SpawnPrefill | null
  /**
   * Spawn sibling: a new session in the source session's / stack's directory (read-only), in its
   * stack (created first when it has none) — POST …/stack/spawn to the stack's home. The host is
   * the source's unless the user picks another (then one of that host's directories, sent as `host` + `dir`).
   */
  sibling?: SiblingTarget | null
}

const samePath = (a: string, b: string) => a.replace(/\/+$/, '') === b.replace(/\/+$/, '')
/** The same last folder name (`~/Code/app` on one host, `/home/x/Code/app` on another). */
const sameFolderName = (a: string, b: string) => {
  const last = (p: string) => p.replace(/\/+$/, '').split('/').pop() ?? ''
  return !!last(a) && last(a) === last(b)
}

/**
 * `+` in the list header: host, directory (that host's spawnDirs), model and an optional first
 * prompt → POST spawn, then watch the fleet for the new session and open it. No name field:
 * the server's auto-namer names it (tmux `fw-hhmmss` when auto-naming is off).
 */
export function NewSessionDrawer({ open, onOpenChange, onOpenSession, prefill, sibling }: NewSessionDrawerProps) {
  return (
    <Drawer open={open} onOpenChange={onOpenChange}>
      <DrawerContent className="px-safe" onOpenAutoFocus={prefill?.prompt ? (e) => e.preventDefault() : undefined}>
        {open ? <NewSessionForm onDone={() => onOpenChange(false)} onOpenSession={onOpenSession} prefill={prefill} sibling={sibling} /> : null}
      </DrawerContent>
    </Drawer>
  )
}

/** Desktop: the same form in a centred dialog (`c` / `n`, the sidebar's `+`, the palette). */
export function NewSessionDialog({ open, onOpenChange, onOpenSession, prefill, sibling }: NewSessionDrawerProps) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent
        className="max-h-[calc(100dvh-4rem)] overflow-hidden p-0 sm:max-w-lg"
        onOpenAutoFocus={prefill?.prompt ? (e) => e.preventDefault() : undefined}
      >
        {open ? (
          <NewSessionForm variant="dialog" onDone={() => onOpenChange(false)} onOpenSession={onOpenSession} prefill={prefill} sibling={sibling} />
        ) : null}
      </DialogContent>
    </Dialog>
  )
}

function NewSessionForm({
  onDone,
  onOpenSession,
  variant = 'drawer',
  prefill = null,
  sibling: siblingProp = null,
}: {
  onDone: () => void
  onOpenSession: (href: string) => void
  variant?: 'drawer' | 'dialog'
  prefill?: SpawnPrefill | null
  sibling?: SiblingTarget | null
}) {
  const dialog = variant === 'dialog'
  const Header = dialog ? DialogHeader : DrawerHeader
  const Title = dialog ? DialogTitle : DrawerTitle
  const Description = dialog ? DialogDescription : DrawerDescription
  const { fleet, applyFleet } = useFleet()
  const hosts = spawnTargets(fleet)
  // From a session: Hand off (a separate session) or Sibling (in its stack), same prompt either way.
  const offersSibling = !siblingProp && !!prefill?.sibling
  const [fromMode, setFromMode] = useState<'handoff' | 'sibling'>(() => (storage.get(FROM_MODE_KEY) === 'sibling' ? 'sibling' : 'handoff'))
  const sibling = siblingProp ?? (offersSibling && fromMode === 'sibling' ? (prefill?.sibling ?? null) : null)
  const pickFromMode = (m: 'handoff' | 'sibling') => {
    setFromMode(m)
    storage.set(FROM_MODE_KEY, m)
    setError(null)
  }

  const [host, setHost] = useState(() => {
    // A sibling starts where the user is looking (the source session's host); the stack's home may differ.
    if (sibling) return sibling.cwdHost ?? sibling.host
    if (prefill && hosts.some((h) => h.name === prefill.host)) return prefill.host
    const remembered = storage.get(HOST_KEY)
    return hosts.find((h) => h.name === remembered)?.name ?? hosts[0]?.name ?? ''
  })
  const hostDirs = hosts.find((h) => h.name === host)?.dirs ?? []
  // A prefilled dir (the session's cwd) is offered first when it is not one of the spawnDirs, and preselected.
  const prefillDir = prefill?.dir && host === prefill.host ? prefill.dir : null
  const fixed = !!prefill?.fixed && !!prefillDir
  const dirs = fixed && prefillDir
    ? [{ label: prefillDir.replace(/\/+$/, '').split('/').pop() || prefillDir, path: prefillDir }]
    : prefillDir && !hostDirs.some((d) => samePath(d.path, prefillDir)) ? [{ label: PREFILL_DIR_LABEL, path: prefillDir }, ...hostDirs] : hostDirs
  const [dirLabels, setDirLabels] = useState<Record<string, string | null>>({})
  // A sibling on the source's host starts in its directory (no choice); on another host the user
  // picks one of that host's directories — the same repo (by folder name) is preselected.
  const siblingSameHost = !!sibling && host === (sibling.cwdHost ?? sibling.host)
  const siblingMatch = sibling && !siblingSameHost && sibling.cwd ? (hostDirs.find((d) => sameFolderName(d.path, sibling.cwd ?? ''))?.label ?? null) : null
  const rememberedLabel =
    dirLabels[host] ?? (prefillDir ? dirs.find((d) => samePath(d.path, prefillDir))?.label : null) ?? siblingMatch ?? storage.get(dirKey(host))
  const dir = dirs.find((d) => d.label === rememberedLabel) ?? dirs[0] ?? null

  const { models: claudeModels, voice: voiceEnabled } = useSettings()
  const offered = offeredHarnesses(useHarnesses(host), claudeModels)
  const [harnessChoice, setHarnessChoice] = useState<string | null>(() => storage.get(HARNESS_KEY))
  const harness = pickHarness(offered, harnessChoice)
  const who = harnessLabel(harness)
  const models = offered.find((h) => h.id === harness)?.models ?? claudeModels
  const [modelChoices, setModelChoices] = useState<Record<string, string | null>>({})
  const model = pickModel(models, modelChoices[harness] ?? storage.get(modelKey(harness)))
  // GitHub identity (a GitHub App the session acts as instead of you): chosen per spawn, never
  // remembered — a forgotten choice must not start the next session as a bot. Claude only.
  const ghIdentities = useGhIdentities(host)
  const [ghIdentityChoice, setGhIdentityChoice] = useState('')
  const ghIdentity = harness === 'claude' && ghIdentities.some((i) => i.name === ghIdentityChoice) ? ghIdentityChoice : ''
  const [prompt, setPrompt] = useState(() => prefill?.prompt ?? (sibling ? '' : loadDraft()))
  // Keep the draft on every change (attachments typed in included); a prefill / sibling is never a draft.
  const draftable = !prefill && !sibling
  useEffect(() => {
    if (draftable) saveDraft(prompt)
  }, [draftable, prompt])
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  // Files dropped on the form / pasted / picked: uploaded to the chosen host, paths go into the prompt.
  const promptRef = useRef<HTMLTextAreaElement>(null)
  const picker = useRef<HTMLInputElement>(null)
  const { attach, onPaste, progress } = useAttach({ host, textarea: promptRef, setValue: setPrompt })
  // Voice: the transcript is appended to the prompt (the repo name helps the spelling).
  const voice = useVoice({
    hint: dir?.path.split('/').pop() ?? '',
    onText: (t) => {
      setPrompt((cur) => appendTranscript(cur, t))
      requestAnimationFrame(() => {
        const el = promptRef.current
        if (!el) return
        el.focus()
        el.setSelectionRange(el.value.length, el.value.length)
        el.scrollTop = el.scrollHeight
      })
    },
  })
  const recording = voice.state === 'recording'
  const [now, setNow] = useState(() => Date.now())
  useEffect(() => {
    if (!recording) return
    const t = window.setInterval(() => setNow(Date.now()), 500)
    return () => window.clearInterval(t)
  }, [recording])
  const drop = useFileDrop((files) => void attach(files), !busy)

  // Prefilled: the caret goes after the prompt, where the user types what comes next.
  const prefilled = !!prefill?.prompt
  useEffect(() => {
    const el = promptRef.current
    if (!prefilled || !el) return
    el.focus()
    el.setSelectionRange(el.value.length, el.value.length)
    el.scrollTop = el.scrollHeight
  }, [prefilled])

  const pickHost = (h: string) => {
    setHost(h)
    if (!sibling) storage.set(HOST_KEY, h) // a sibling's host is a one-off choice
    setError(null)
  }
  const chooseModel = (id: string) => {
    setModelChoices((m) => ({ ...m, [harness]: id }))
    storage.set(modelKey(harness), id)
  }
  const chooseHarness = (id: string) => {
    setHarnessChoice(id)
    storage.set(HARNESS_KEY, id)
  }
  const pickDir = (label: string) => {
    setDirLabels((m) => ({ ...m, [host]: label }))
    if (label !== PREFILL_DIR_LABEL) storage.set(dirKey(host), label)
  }

  // ⌘/Ctrl+Enter submits from any field (the prompt included; plain Enter there stays a newline).
  const canStart = !busy && (sibling ? siblingSameHost || !!dir : !!dir) && progress == null && voice.state === 'idle'

  const startSibling = async (sib: SiblingTarget) => {
    setBusy(true)
    setError(null)
    try {
      const sent = withContinueTask(prompt)
      const body = {
        prompt: sent.trim() ? sent : undefined,
        model: model || undefined,
        harness: harness === 'claude' ? undefined : harness,
        ghIdentity: ghIdentity || undefined,
        // Another host than the source's: say where, and in which of its directories.
        ...(siblingSameHost ? {} : { host, dir: dir?.path }),
      }
      const res = sib.sessionId ? await api.spawnSibling(sib.host, sib.sessionId, body) : await api.spawnInStack(sib.host, sib.stackId ?? '', body)
      onDone()
      const label = res.stack?.label || sib.label || 'the stack'
      if (res.created)
        toast.success(`Stack created: ${label}`, {
          description: res.generated ? 'StackBrief written from this session.' : 'StackBrief skeleton — the model call was skipped or failed.',
        })
      const spawnHost = res.spawn?.host || res.host || sib.host
      const name = res.spawn?.name || res.spawn?.tmuxSession || 'sibling'
      const id = toast.loading(`Starting ${name} in ${label}…`, { description: `Waiting for ${who} to register` })
      watchForSpawned(
        { host: spawnHost, name, tmuxSession: res.spawn?.tmuxSession ?? name },
        {
          onFleet: applyFleet,
          onFound: (s) => {
            toast.success(`${name} is up`, { id, description: undefined })
            onOpenSession(sessionHref(s))
          },
          onTimeout: () => notShownUp(name, id, sent),
        },
      )
    } catch (err) {
      setError(stacksMissing(err) ? stackErrorMessage(err) : (err as Error)?.message || 'Failed to start')
      setBusy(false)
    }
  }

  const start = async () => {
    if (busy || progress) return
    if (sibling) return startSibling(sibling)
    if (!host) return setError('No reachable host to start a session on.')
    if (!dir) return setError('This host advertises no directories.')
    setBusy(true)
    setError(null)
    try {
      const sent = withContinueTask(prompt)
      const res = await api.spawn(host, {
        dir: dir.path,
        prompt: sent.trim() ? sent : undefined,
        model: model || undefined,
        harness: harness === 'claude' ? undefined : harness,
        ghIdentity: ghIdentity || undefined,
        embed: prefill?.embed || undefined,
      })
      // The draft stays until the session registers: a spawn that never shows up must not lose the prompt.
      onDone()
      const id = toast.loading(`Starting ${res.name} on ${host}…`, { description: `Waiting for ${who} to register` })
      watchForSpawned(
        { host, name: res.name, tmuxSession: res.tmuxSession },
        {
          onFleet: applyFleet,
          onFound: (s) => {
            clearDraftIf(sent)
            toast.success(`${res.name} is up`, { id, description: undefined })
            onOpenSession(sessionHref(s))
          },
          onTimeout: () => notShownUp(res.name, id, sent),
        },
      )
    } catch (err) {
      const status = err instanceof ApiError ? err.status : 0
      const msg = (err as Error)?.message || 'Failed to start'
      setError(status === 409 ? `${msg} — try again.` : msg)
      setBusy(false)
    }
  }

  const harnessField =
    offered.length > 1 ? (
      <Field label="Agent">
        <ToggleGroup
          type="single"
          value={harness}
          onValueChange={(v) => v && chooseHarness(v)}
          aria-label="Agent"
          className="no-scrollbar w-full justify-start overflow-x-auto"
        >
          {offered.map((h) => (
            <ToggleGroupItem
              key={h.id}
              value={h.id}
              title={h.command}
              className="h-10 shrink-0 rounded-full border border-border bg-card px-4 text-sm data-[state=on]:border-primary/50 data-[state=on]:bg-accent"
            >
              {h.label}
            </ToggleGroupItem>
          ))}
        </ToggleGroup>
      </Field>
    ) : null
  const ghIdentityField =
    harness === 'claude' && ghIdentities.length > 0 ? (
      <Field label="GitHub as">
        <ToggleGroup
          type="single"
          value={ghIdentity || ME_IDENTITY}
          onValueChange={(v) => v && setGhIdentityChoice(v === ME_IDENTITY ? '' : v)}
          aria-label="GitHub identity"
          className="no-scrollbar w-full justify-start overflow-x-auto"
        >
          {[{ name: ME_IDENTITY, login: 'You' }, ...ghIdentities].map((i) => (
            <ToggleGroupItem
              key={i.name}
              value={i.name}
              title={i.name === ME_IDENTITY ? 'Acts on GitHub as you' : `Acts on GitHub as ${i.login} (uses its gh wrapper)`}
              className="h-10 shrink-0 rounded-full border border-border bg-card px-4 text-sm data-[state=on]:border-primary/50 data-[state=on]:bg-accent"
            >
              {i.login}
            </ToggleGroupItem>
          ))}
        </ToggleGroup>
      </Field>
    ) : null
  const modelField =
    models.length > MODEL_CHIPS_MAX ? (
      <Field label="Model">
        <select
          value={model}
          onChange={(e) => chooseModel(e.target.value)}
          aria-label="Model"
          className="h-10 w-full rounded-xl border border-border bg-card px-3 text-sm outline-none focus-visible:ring-3 focus-visible:ring-ring/50"
        >
          {models.map((m) => (
            <option key={m.id || DEFAULT_MODEL_VALUE} value={m.id}>
              {m.label}
            </option>
          ))}
        </select>
      </Field>
    ) : models.length > 1 ? (
      <Field label="Model">
        <ToggleGroup
          type="single"
          value={model || DEFAULT_MODEL_VALUE}
          onValueChange={(v) => v && chooseModel(v === DEFAULT_MODEL_VALUE ? '' : v)}
          aria-label="Model"
          className="no-scrollbar w-full justify-start overflow-x-auto"
        >
          {models.map((m) => (
            <ToggleGroupItem
              key={m.id || DEFAULT_MODEL_VALUE}
              value={m.id || DEFAULT_MODEL_VALUE}
              title={m.id || `${who}'s default model`}
              className="h-10 shrink-0 rounded-full border border-border bg-card px-4 text-sm data-[state=on]:border-primary/50 data-[state=on]:bg-accent"
            >
              {m.label}
            </ToggleGroupItem>
          ))}
        </ToggleGroup>
      </Field>
    ) : null
  const promptField = (
    <Field label="First prompt" hint="optional">
      <Textarea
        ref={promptRef}
        value={prompt}
        onChange={(e) => setPrompt(e.target.value)}
        onPaste={onPaste}
        rows={3}
        placeholder={`What should ${who} work on?`}
        className="max-h-48 min-h-20 rounded-xl bg-card text-base md:text-base"
      />
      <div className="flex min-w-0 items-center gap-2">
        <input
          ref={picker}
          type="file"
          multiple
          hidden
          tabIndex={-1}
          onChange={(e) => {
            const files = Array.from(e.target.files ?? [])
            e.target.value = ''
            void attach(files)
          }}
        />
        <Button
          type="button"
          variant="ghost"
          size="sm"
          disabled={!host || progress != null}
          onClick={() => picker.current?.click()}
          className="-ml-2 h-9 shrink-0 rounded-lg px-2 text-muted-foreground"
        >
          {progress ? <Loader2Icon className="animate-spin" /> : <PaperclipIcon />}
          Attach files
        </Button>
        {voiceEnabled ? (
          <Button
            type="button"
            variant={recording ? 'destructive' : 'ghost'}
            size="sm"
            aria-pressed={recording}
            aria-label={recording ? 'Stop recording and transcribe' : 'Record a voice prompt'}
            title={recording ? 'Stop and transcribe' : 'Voice input: speak, the text is added to the prompt'}
            disabled={voice.state === 'transcribing'}
            onClick={() => (recording ? voice.stop() : void voice.start())}
            className={cn('h-9 shrink-0 rounded-lg px-2', !recording && 'text-muted-foreground', recording && 'animate-pulse')}
          >
            {voice.state === 'transcribing' ? <Loader2Icon className="animate-spin" /> : recording ? <SquareIcon className="fill-current" /> : <MicIcon />}
            {recording ? formatElapsed(voice.startedAt ? now - voice.startedAt : 0) : 'Voice'}
          </Button>
        ) : null}
        <span className="min-w-0 truncate text-xs text-dimmer" role="status">
          {voice.state !== 'idle' ? (
            <>
              {recording ? 'Recording… ' : 'Transcribing… '}
              <button type="button" className="underline-offset-2 hover:text-foreground hover:underline" onClick={voice.cancel}>
                Cancel
              </button>
            </>
          ) : progress
            ? `Uploading ${progress.name}${progress.total > 1 ? ` (${progress.index}/${progress.total})` : ''}…`
            : dialog
              ? `or drop / paste them — stored on ${host}`
              : `stored on ${host}`}
        </span>
      </div>
    </Field>
  )
  const hostField =
    hosts.length > 1 ? (
      <Field label="Host">
        <ToggleGroup
          type="single"
          value={host}
          onValueChange={(v) => v && pickHost(v)}
          aria-label="Host"
          className="no-scrollbar w-full overflow-x-auto"
        >
          {hosts.map((h) => (
            <ToggleGroupItem
              key={h.name}
              value={h.name}
              className="h-10 gap-2 rounded-full border border-border bg-card px-4 text-sm data-[state=on]:border-primary/50 data-[state=on]:bg-accent"
            >
              <HostDot host={h.name} />
              {h.name}
            </ToggleGroupItem>
          ))}
        </ToggleGroup>
      </Field>
    ) : null
  const dirField = (
    <Field label="Directory">
      {dirs.length ? (
        <ToggleGroup
          type="single"
          value={dir?.label ?? ''}
          onValueChange={(v) => v && pickDir(v)}
          aria-label="Directory"
          className="no-scrollbar w-full justify-start overflow-x-auto"
        >
          {dirs.map((d) => (
            <ToggleGroupItem
              key={d.label}
              value={d.label}
              title={shortCwd(d.path, 60)}
              className="group h-10 shrink-0 gap-2 rounded-full border border-border bg-card px-4 text-sm data-[state=on]:border-primary/50 data-[state=on]:bg-accent"
            >
              <FolderIcon className="size-4 shrink-0 text-dimmer group-data-[state=on]:text-primary" />
              {d.label}
            </ToggleGroupItem>
          ))}
        </ToggleGroup>
      ) : (
        <p className="text-sm text-dimmer">{host} advertises no directories.</p>
      )}
    </Field>
  )
  const errorBox =
    error ? (
      <Alert variant="destructive">
        <AlertDescription className="break-words whitespace-pre-line">{error}</AlertDescription>
      </Alert>
    ) : null
  const submitButton = (
    <Button type="submit" className="h-11 w-full rounded-xl text-[0.9375rem]" disabled={!canStart}>
      {busy ? <Loader2Icon className="animate-spin" /> : sibling ? <GitForkIcon /> : <PlayIcon />}
      {busy ? (sibling?.creates ? 'Creating stack…' : 'Starting…') : sibling ? 'Spawn sibling' : `Start on ${host}`}
      {dialog && !busy ? (
        <Kbd aria-hidden className="ml-1 bg-primary-foreground/15 text-primary-foreground/80">
          {isMacPlatform() ? '⌘↵' : 'Ctrl↵'}
        </Kbd>
      ) : null}
    </Button>
  )

  return (
    <form
      className={cn(
        'relative',
        dialog
          ? 'no-scrollbar max-h-[calc(100dvh-4rem)] w-full overflow-y-auto px-5 pt-2 pb-5'
          : 'no-scrollbar mx-auto min-h-0 w-full max-w-lg overflow-y-auto px-4 pb-[max(1rem,env(safe-area-inset-bottom))]',
      )}
      {...drop.bind}
      onSubmit={(e) => {
        e.preventDefault()
        void start()
      }}
      onKeyDown={(e) => {
        if (!isSubmitChord(e.nativeEvent)) return
        e.preventDefault()
        if (canStart) void start()
      }}
    >
      <Header className="px-0 pt-3 pb-2 text-left">
        <Title className="text-left text-base font-semibold">{offersSibling ? 'New session from here' : sibling ? 'Spawn sibling' : fixed ? 'New session' : prefill ? 'Continue in new session' : 'New session'}</Title>
        <Description className="text-left text-xs">
          {sibling
            ? sibling.creates
              ? `This starts a new stack around this session (one Sonnet call), then a sibling session${siblingSameHost ? ' in the same directory' : ` on ${host}`} that reads the shared StackBrief first.`
              : `A new session in the stack ${sibling.label ? `“${sibling.label}”` : ''}${siblingSameHost ? ', in the same directory' : ` on ${host}`}. It reads the shared StackBrief first${siblingSameHost ? '' : ` (through fleet, from ${sibling.host})`}.`
            : fixed
              ? `Starts ${who} in ${shortCwd(dir?.path ?? '', 60)} on ${host}.`
              : offersSibling
              ? `A separate ${who} session that picks up where this one is, on its own.`
              : `Starts ${who} in a new tmux session. A first-run folder trust prompt is accepted for you.`}
        </Description>
      </Header>

      {offersSibling ? (
        <ToggleGroup
          type="single"
          value={fromMode}
          onValueChange={(v) => v && pickFromMode(v as 'handoff' | 'sibling')}
          spacing={0}
          aria-label="How"
          className="mt-1 mb-3 w-full rounded-lg bg-muted p-1"
        >
          {(
            [
              ['handoff', 'Hand off', ArrowRightIcon],
              ['sibling', 'Sibling', GitForkIcon],
            ] as const
          ).map(([v, label, Icon]) => (
            <ToggleGroupItem
              key={v}
              value={v}
              className="h-9 flex-1 gap-1.5 rounded-md px-3 text-[0.8125rem] text-muted-foreground data-[state=on]:bg-background data-[state=on]:text-foreground data-[state=on]:shadow-sm dark:data-[state=on]:bg-accent"
            >
              <Icon /> {label}
            </ToggleGroupItem>
          ))}
        </ToggleGroup>
      ) : null}

      {sibling ? (
        <div className="space-y-4 pt-1">
          <Field label={sibling.creates ? 'From' : 'Stack'}>
            <div className="flex min-h-12 items-center gap-3 rounded-xl border border-border bg-card px-3 py-2">
              {sibling.creates ? <GitForkIcon className="size-4 shrink-0 text-dimmer" /> : <LayersIcon className="size-4 shrink-0 text-primary" />}
              <span className="min-w-0 flex-1">
                <span className="block truncate text-sm font-medium">{sibling.label || (sibling.creates ? 'this session' : 'Session stack')}</span>
                <span className="flex min-w-0 items-center gap-1.5 text-[0.6875rem] text-dimmer">
                  <HostDot host={sibling.cwdHost ?? sibling.host} />
                  <span className="shrink-0">{sibling.cwdHost ?? sibling.host}</span>
                  {sibling.cwd ? <span className="min-w-0 truncate font-mono">· {shortCwd(sibling.cwd, 60)}</span> : null}
                </span>
              </span>
            </div>
          </Field>
          {hostField}
          {siblingSameHost ? null : dirField}
          {harnessField}
          {modelField}
          {ghIdentityField}
          {promptField}
          {errorBox}
          {submitButton}
        </div>
      ) : hosts.length === 0 ? (
        <Alert variant="destructive" className="my-2">
          <AlertDescription>No reachable host to start a session on.</AlertDescription>
        </Alert>
      ) : (
        <div className="space-y-4 pt-1">
          {fixed ? null : hostField}

          {fixed ? null : dirField}

          {harnessField}

          {modelField}

          {ghIdentityField}

          {promptField}

          {errorBox}

          {submitButton}
        </div>
      )}
      <DropOverlay show={drop.dragging} hint={host ? `Uploaded to ${host}; the path goes into the first prompt` : undefined} />
    </form>
  )
}

function Field({ label, hint, children }: { label: string; hint?: string; children: React.ReactNode }) {
  return (
    <div className="space-y-1.5">
      <Label className="flex items-baseline gap-2 text-xs font-semibold tracking-wide text-muted-foreground uppercase">
        {label}
        {hint ? <span className="font-normal tracking-normal text-dimmer normal-case">{hint}</span> : null}
      </Label>
      {children}
    </div>
  )
}
