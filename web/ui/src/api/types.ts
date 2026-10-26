// Types for the fleet web HTTP API (see web/ARCHITECTURE.md → "HTTP API").
// The API is a stable contract: fields are only ever added, so unknown extra
// fields are fine and everything a UI does not strictly need is optional.

export type SessionStatus = 'busy' | 'idle' | 'waiting' | 'unknown'
export type Backend = 'iterm' | 'tmux' | 'unknown'

/** One Claude Code session: a `fleet list --json` row plus the host it runs on. */
export interface Session {
  host: string
  session_id: string
  pid?: number
  name?: string | null
  cwd?: string | null
  /** Unknown strings are possible from newer CLIs — treat as `unknown`. */
  status?: SessionStatus | string | null
  /** Last activity, epoch ms. */
  updated_at?: number | null
  /** Last human prompt, epoch ms; null when unknown (no prompt yet, Codex / pi, older CLI). */
  prompted_at?: number | null
  tty?: string | null
  backend?: Backend | string | null
  /** iTerm session id or tmux pane id (`%87`). */
  handle?: string | null
  tab?: string | null
  tmux_session?: string | null
  name_source?: string | null
  waiting_for?: string | null
  /** First prompt, trimmed to ~300 chars for transport. */
  title?: string | null
  gen_title?: string | null
  /**
   * The session's one title, computed by the CLI (core::title::display_title): the chosen
   * Claude name, else the generated title, else a heuristic. Older CLIs omit it — use
   * `sessionTitle()` (lib/title.ts), never a field directly.
   */
  display_title?: string | null
  /** Context-window usage from the transcript tail; null when unknown (older CLIs omit it). */
  context?: ContextUsage | null
  /** "Open in editor" link for the cwd (like Brief.editorUrl, without the git-root lookup). Older servers omit it. */
  editorUrl?: string | null
  /**
   * The session stack this session belongs to (`fleet list --json` → `stack`); null = in none.
   * Older CLIs omit the field entirely (undefined = this host knows no stacks).
   */
  stack?: StackRef | null
  /** The coding agent running it (`fleet list --json` → `harness`); older CLIs omit it (= claude). */
  harness?: HarnessId | string | null
  /** The model it runs, when known. */
  model?: string | null
  /** Absolute transcript path (Codex / pi only). */
  transcript?: string | null
  /**
   * The scheduled job run that started it (`fleet list --json` → `origin`, docs/cli.md → Jobs);
   * null = started by hand. Older CLIs omit it.
   */
  origin?: SessionOrigin | null
  /**
   * The GitHub identity (a GitHub App) it acts as on GitHub (`fleet list --json` → `gh_identity`,
   * docs/cli.md → GitHub identities); null = the user's own. Older CLIs omit it.
   */
  gh_identity?: string | null
  /** The app whose embedded Fleet panel started it (`fleet list --json` → `embed`, the pane's `@fleet_embed`); null / absent = none. */
  embed?: string | null
  /**
   * The git checkout its cwd is in (`fleet list --json` → `git`): branch (null = detached),
   * root, linked worktree or main checkout, GitHub `owner/name` of origin. Null outside one;
   * older CLIs omit it.
   */
  git?: { branch: string | null; root: string; linked: boolean; repo: string | null } | null
  /** The host's voice operator (web/lib/operator.mjs): kept out of session lists. Older servers omit it. */
  operator?: boolean
}

/** `origin` on a session row: the job and the run id (`<job>/<run>` in its tmux environment). */
export interface SessionOrigin {
  job: string
  run: string
}

/** The coding agents fleet drives — see docs/cli.md → Harnesses. */
export type HarnessId = 'claude' | 'codex' | 'pi'

/** GET /api/hosts/:host/harnesses → `harnesses[]` (`fleet harness list --json`). */
export interface HarnessInfo {
  id: HarnessId | string
  label: string
  command: string
  installed: boolean
  /** The picker; `id: ''` is the harness's own default. */
  models: ModelOption[]
}

export interface HarnessesResponse {
  host: string
  harnesses: HarnessInfo[]
  /** GitHub identities a session spawned there can act as (config `ghIdentities`). Older servers omit it. */
  ghIdentities?: GhIdentity[]
}

/** A GitHub App a spawned session can act as on GitHub instead of the user. */
export interface GhIdentity {
  name: string
  /** The App's GitHub login, e.g. `my-app[bot]`. */
  login: string
}

/** `stack` on a session row. */
export interface StackRef {
  id: string
  label: string
  /** The stack's home host (where its StackBrief lives); older servers omit it (= the session's host). */
  host?: string
}

/** `context` on a session row — see docs/architecture.md → "Context usage". */
export interface ContextUsage {
  /** Prompt tokens of the last main-thread request (input + cache read + cache creation). */
  used: number
  /** Inferred window: 200000 or 1000000. */
  window: number
  /** Whole percent, rounded; may exceed 100. */
  pct: number
  model?: string | null
}

export interface SpawnDir {
  label: string
  path: string
}

/** One Settings → Start directories entry: a label and a directory per host (config `spawnDirs`). */
export interface SpawnDirEntry {
  label: string
  /** host name → directory on that host (absolute or `~/…`); a missing host = not offered there. */
  paths: Record<string, string>
  /** Allowed for spawns (e.g. an app's embedded panel) but not offered in the New session menu. */
  hidden?: boolean
  /** Keys this UI does not know: sent back unchanged. */
  [extra: string]: unknown
}

/** Whether the answering host's own path of one entry is a directory there (null: no path for it). */
export interface SpawnDirCheck {
  path: string
  /** Absolute, `~` expanded. */
  resolved: string
  exists: boolean
  isDir: boolean
}

/** GET/PUT /api/hosts/:host/spawn-dirs. */
export interface SpawnDirsResponse {
  host: string
  /** This host + every host in its config. */
  hosts: string[]
  spawnDirs: SpawnDirEntry[]
  /** Index-aligned with `spawnDirs`. */
  checks: (SpawnDirCheck | null)[]
  /** What the host offers now (`Host.spawnDirs`). */
  offered: SpawnDir[]
  /** Its hidden entries now (`Host.hiddenSpawnDirs`); older servers omit it. */
  hiddenSpawnDirs?: SpawnDir[]
  limits: { maxEntries: number; maxLabel: number; maxPath: number }
  /** PUT only: false for a dry run. */
  saved?: boolean
}

/** A PUT spawn-dirs 400: `errors[].index` into the list sent; `host` = that host's path cell. */
export interface SpawnDirsError {
  error: string
  errors: { index: number; field: 'label' | 'paths' | 'hidden' | 'list'; host?: string; error: string }[]
  checks: (SpawnDirCheck | null)[]
}

export interface Host {
  name: string
  ok: boolean
  error?: string
  fetchedAt?: number
  spawnDirs?: SpawnDir[]
  /** Start directories allowed for spawns but not offered (config `hidden: true`); absent when none. */
  hiddenSpawnDirs?: SpawnDir[]
  /** That host's home directory (expands a `~/…` path); older servers omit it. */
  home?: string
  /** Set when that host's server has a notes explorer (`web.notes.root`). */
  notes?: { name: string | null }
  sessions: Session[]
}

/** GET /api/fleet — self first, then peers. */
export interface FleetResponse {
  self: string
  hosts: Host[]
  /** When the merged snapshot was built (epoch ms); absent on `?local=1`. */
  snapshotAt?: number
}

export interface QuickReply {
  label: string
  text: string
}

/** One New session model choice (`web.models`); id '' = no `--model`, Claude's default. */
export interface ModelOption {
  id: string
  label: string
}

/** GET /api/settings */
/** GET/POST /api/hosts/:host/focus: that host's `focus` session (Do Not Disturb + quit apps). */
/** A Slack link (web/lib/slack.mjs): a session follows one Slack thread / conversation until `until`. */
export interface SlackLink {
  id: string
  host: string
  session: string
  sessionName: string | null
  channel: string
  /** null: the whole channel / DM. */
  threadTs: string | null
  permalink: string
  task: string
  createdAt: number
  until: number
  pending: number
  delivered: number
  replies: number
  endedAt: number | null
  endReason: string | null
  renewed?: boolean
}

/** GET /api/slack: the bridge on `slack.host` (proxied there). */
export interface SlackStatus {
  enabled: boolean
  host: string | null
  error?: string
  socket?: { connected: boolean; error: string | null }
  links: SlackLink[]
  ended: SlackLink[]
}

export interface FocusStatus {
  host: string
  active: boolean
  remainingMs: number
  /** Epoch ms (null when off). */
  endsAt: number | null
  apps: string[]
  /** State left behind by a dead watchdog: `focus stop` cleans it up. */
  stale: boolean
}

export interface Settings {
  apiVersion: number
  self: string
  hosts: string[]
  quickReplies: QuickReply[]
  /** `web.uploads.maxMB` of this server (older servers: absent). */
  uploads?: { maxMB: number | null }
  /** Voice prompts (POST /api/transcribe): this server has a transcription key (older servers: absent). */
  transcribe?: { enabled: boolean }
  /** Voice memos (/api/memos): stored on this server, transcribed with its key (older servers: absent). */
  memos?: { enabled: boolean }
  /** The New session model picker (older servers: absent → lib/models.ts defaults). */
  models?: ModelOption[]
  /** This server's notes explorer (older servers: absent). */
  notes?: { enabled: boolean; name?: string | null }
  /** The host whose `focus` CLI the Focus toggle drives (`web.focus.host`; null/absent: no toggle). */
  focus?: { host: string | null }
  /** Session stacks on this server (older servers: absent); `generate` = creating one calls `model`. */
  stacks?: { enabled: boolean; model?: string; generate?: boolean }
  /** Spoken replies (POST /api/speak): this server has the key (older servers: absent). */
  speak?: { enabled: boolean; voice?: string | null }
  /** The voice operator (GET/POST /api/operator) is available on this server (older servers: absent). */
  operator?: { enabled: boolean; hosts?: string[] }
}

/** GET/POST /api/operator — this server's operator session (null until it shows in the fleet). */
export interface OperatorStatus {
  host: string
  dir: string
  session: Session | null
  /** POST: a new operator was started just now. */
  started?: boolean
  tmuxSession?: string | null
}

/** POST /api/transcribe — the recording as text. */
export interface TranscribeResponse {
  text: string
  model: string
}

/** POST /api/hosts/:host/uploads — the stored copy on that host. */
export interface UploadResponse {
  host: string
  /** Absolute path on `host`. */
  path: string
  name: string
  size: number
}

export type FileKind = 'markdown' | 'text' | 'image' | 'pdf' | 'other'

/** Composer autocomplete (GET …/sessions/:id/complete?kind=), listed on the session's host. */
export type CompleteKind = 'skills' | 'files' | 'brain'

export interface CompleteSkill {
  /** Without the slash: `deploy`, `p-dev:review`. */
  name: string
  description: string
  source: 'builtin' | 'user' | 'project' | 'plugin'
  argumentHint?: string
}

export interface CompleteBrainNote {
  /** What goes after `brain:` — the file name, or the path when two notes share it. */
  ref: string
  /** Relative to the brain root. */
  path: string
  title?: string
}

export interface CompleteResponse {
  host: string
  id: string
  kind: CompleteKind
  skills?: CompleteSkill[]
  /** .md / .plain files, relative to the session cwd. */
  files?: string[]
  /** kind=brain: false when the host has no ~/.brainrc. */
  available?: boolean
  notes?: CompleteBrainNote[]
  truncated?: boolean
}

/** One entry of POST …/sessions/:id/files/stat — a path mentioned in chat, resolved on the session's host. */
export interface FileStat {
  /** What was asked (`docs/a.md:12`). */
  input: string
  /** Absolute path on the host (resolved against the session cwd). */
  path: string
  /** Relative to the cwd when inside it, else `~/…`, else absolute. */
  rel?: string
  line?: number
  col?: number
  exists: boolean
  /** Outside $HOME / the cwd, or a secret store: never served. */
  forbidden?: boolean
  isFile?: boolean
  isDir?: boolean
  size?: number
  mtime?: number
  kind?: FileKind
}

export interface FileStatResponse {
  host: string
  id: string
  cwd: string | null
  home: string
  files: FileStat[]
}

export interface FileOpenResponse {
  ok: true
  host: string
  path: string
  /** A runnable file was revealed in its folder instead of opened. */
  revealed: boolean
  command: string
}

export interface AutoNameRun {
  host?: string
  ok?: boolean
  at?: number
  ms?: number
  reason?: string
  dryRun?: boolean
  renamed?: { from: string; to: string }[]
  tmux?: string[]
  held?: string[]
  errors?: string[]
  error?: string
}

/** GET /api/health */
export interface Health {
  name: string
  version: string
  apiVersion: number
  self: string
  uptime: number
  now: number
  autoName: { enabled: boolean; intervalMinutes: number; lastRun: AutoNameRun | null }
}

/** GET /api/hosts/:host/sessions/:id/peek */
export interface PeekResponse {
  host: string
  id: string
  backend: Backend | string
  lines: number
  text: string
  capturedAt: number
}

export type MessageRole = 'user' | 'assistant' | 'system'
export type MessageKind = 'user' | 'assistant' | 'command' | 'system' | 'question'

/** One question of Claude's AskUserQuestion prompt. */
export interface Question {
  question: string
  header: string
  multiSelect: boolean
  options: { label: string; description: string }[]
  /** No "Type something" row (the terminal's "Review your answers" screen). */
  noOther?: boolean
}

/** POST …/answer: one entry per question, in order (option indexes and/or typed text). */
export interface QuestionAnswer {
  options: number[]
  text?: string | null
}

export interface Message {
  role: MessageRole
  kind: MessageKind
  text: string
  ts?: number | null
  /** Assistant text that ends a turn; `false` = narration between tool calls. */
  final?: boolean
  /** kind 'command': the slash command / skill (`/p-dev:review`), or `!` for a shell command. */
  name?: string
  /** kind 'command': what was typed after the command, when anything was. */
  args?: string
  /** kind 'question': the AskUserQuestion tool call's id (what …/answer names); `screen:…` for a
   * prompt read off the terminal (Claude Code writes it to the transcript only once answered). */
  id?: string
  /** kind 'question': the prompt (`text` is its markdown rendering). */
  questions?: Question[]
  /** kind 'question': the answers, per question, once answered. */
  answers?: (string | null)[]
  /** kind 'question': dismissed (Esc, "Chat about this", interrupted). */
  declined?: boolean
  /** kind 'user': typed while Claude works, still in its queue (always last). */
  queued?: boolean
}

/** GET /api/hosts/:host/sessions/:id/messages */
export interface MessagesResponse {
  host: string
  id: string
  status: SessionStatus | string
  backend: Backend | string
  name?: string | null
  limit: number
  messages: Message[]
  total: number
  truncated: boolean
  updatedAt?: number | null
  capturedAt: number
}

export type SessionKey = 'Enter' | 'Escape' | 'Up' | 'Down'

export interface SpawnRequest {
  /** Still accepted by the server; the UI leaves naming to the auto-namer. */
  name?: string
  dir?: string
  prompt?: string
  /** A `models` id; '' / absent = the harness's default. */
  model?: string
  /** Which coding agent to start; absent = claude. */
  harness?: HarnessId | string
  /** Act on GitHub as this identity (Claude only); absent = the user's own. */
  ghIdentity?: string
  /** Started from this app's embedded panel (the embed view's `app`): a label on the session. */
  embed?: string
}

export interface SpawnResponse {
  ok: boolean
  host: string
  name: string
  dir: string
  tmuxSession: string
  command: string
  trusted: boolean
}

export interface KillResponse {
  ok: true
  host: string
  id: string
  name?: string | null
  process: 'terminated' | 'killed' | 'gone' | 'skipped' | string
  terminal: string
}

/** POST /api/hosts/:host/sessions/:id/autoname → `fleet name <id> --apply --refresh` (409 when held). */
export interface RegenerateNameResponse {
  ok: boolean
  host: string
  id: string
  /** The new name; null when the model came back with the same one. */
  name: string | null
}

/** POST /api/hosts/:host/sessions/:id/rename → `fleet rename --json` (409 when held). */
export interface RenameResponse {
  ok: boolean
  host: string
  id: string
  /** `renamed` (confirmed), `sent` (typed, not reflected yet), `held` (nothing sent). */
  result: 'renamed' | 'sent' | 'held' | string
  title: string
  from?: string
  /** Why nothing was sent: `waiting` (on a permission prompt / question). */
  held?: 'waiting' | string | null
  tmux?: { renamed: boolean; from?: string | null; to?: string | null; note: string } | null
  message?: string
}

export interface OkResponse {
  ok: true
}

/** One member of a group: a session on a host (`id` = session_id, or String(pid) without one). */
export interface GroupMember {
  host: string
  id: string
  /** Set by servers that know session recovery: the session is dormant (a reboot left it, not resumed yet). */
  dormant?: boolean
}

/** A smart group of sessions (`fleet group`). Ids are stable across runs. */
export interface SessionGroup {
  id: string
  label: string
  description?: string | null
  /** `llm` (model-made) or `fallback` (grouped by repo). */
  source?: 'llm' | 'fallback' | string
  members: GroupMember[]
}

export interface GroupRun {
  at: number
  ms: number
  ok: boolean
  reason?: 'scheduled' | 'manual' | 'startup' | 'changes' | string
  mode?: 'noop' | 'incremental' | 'consolidate' | 'full' | 'fallback' | string
  modelCalls?: number
  classified?: number
  error?: string
  note?: string
}

/** GET /api/groups (and POST /api/groups/run). `enabled: false` → the UI groups by repo itself. */
export interface GroupsResponse {
  enabled: boolean
  /** The host that runs the grouping pass. */
  host: string | null
  intervalMinutes: number | null
  running: boolean
  /** Epoch ms of the last applied grouping state. */
  updatedAt: number | null
  lastRun: GroupRun | null
  groups: SessionGroup[]
  error?: string
}

/** A `## Resources` bullet of a session brief. `url` or `path` is set; `kind: null` = a hand-written line. */
export type BriefResourceKind = 'Origin' | 'PR' | 'Issue' | 'Artifact' | 'Spec' | 'File' | 'Git' | 'Branch' | 'Worktree' | 'Link'

export interface BriefResource {
  kind: BriefResourceKind | string | null
  label: string | null
  url: string | null
  /**
   * Relative to the session cwd when inside it, else `~/…`, else absolute (a branch name for the
   * legacy `Branch`; the repo / worktree root for `Git`).
   */
  path: string | null
  /** The bullet as written. */
  text: string
  /** `Git` only (null otherwise; older servers omit it): the branch, null when detached. */
  branch?: string | null
  /** `Git` only: true = a linked worktree, false = the main checkout (null: unknown / not Git). */
  linked?: boolean | null
}

// --- Session stacks (GET/PUT/DELETE /api/hosts/:host/stacks/…) ------------------------------

/** One member of a stack (StackBrief frontmatter `members[]` + live data from discovery). */
export interface StackMember {
  session: string
  host: string
  /** The display title at the last sync (informational). */
  name: string | null
  /** ISO 8601. */
  added: string | null
  /** ISO 8601 once the session is gone; null while live. */
  closed: string | null
  /** Live right now (discovery at render time). */
  live?: boolean
  /** Dormant on its host (a reboot left it; resume it from the Dormant section). */
  dormant?: boolean
  status?: SessionStatus | string | null
  briefPath?: string | null
  briefExists?: boolean
}

/** `fleet stack show --json` (+ the server's `editorUrl`). */
export interface StackView {
  host: string
  id: string
  label: string
  /** Absolute path of the StackBrief on `host`. */
  path: string
  cwd: string | null
  absCwd: string | null
  created: string | null
  updated: string | null
  generatedAt: string | null
  editedAt: string | null
  /** The sentence every sibling's first prompt starts with (on the stack's home host). */
  contextLine: string
  /** The same for a sibling on another host (`fleet -H <home> stack …`); older CLIs omit it. */
  remoteContextLine?: string
  members: StackMember[]
  /** The whole file, frontmatter included (what the editor edits). */
  markdown: string
  /** Without frontmatter. */
  body: string
  parsed: { summary: string; resources: BriefResource[]; notes: string }
  /** Open the stack's directory in the editor; null / absent = hide the button. */
  editorUrl?: string | null
}

/** GET /api/hosts/:host/stacks */
export interface StacksResponse {
  host: string
  stacks: StackView[]
}

/** The body of both stack spawn routes. */
export interface StackSpawnRequest {
  prompt?: string
  name?: string
  model?: string
  harness?: HarnessId | string
  /** Act on GitHub as this identity (Claude only); absent = the user's own. */
  ghIdentity?: string
  /** Label for a stack this spawn creates (the model's otherwise). */
  label?: string
  /** Start the sibling on this host (a peer of the stack's home; default: the home itself). */
  host?: string
  /** Where it starts: required with `host`, a path on that host. */
  dir?: string
}

/** POST …/sessions/:id/stack/spawn and …/stacks/:id/spawn. */
/** POST …/sessions/:id/stack/link and …/stacks/:id/add: the session to put in the stack, and the host it runs on. */
export interface StackLinkRequest {
  session: string
  host?: string
  label?: string
}

export interface StackLinkResponse {
  host: string
  stack: StackView
  /** A new stack was created around the source session (…/stack/link only). */
  created: boolean
  generated: boolean
}

export interface StackSpawnResponse {
  host: string
  stack: StackView
  /** A new stack was created around the source session. */
  created: boolean
  /** The StackBrief came from the model (false: the skeleton fallback). */
  generated: boolean
  spawn: Omit<SpawnResponse, 'host' | 'ok'> & { host?: string; ok?: boolean; model?: string | null }
}

/** A `## Todos` checkbox line. */
export interface BriefTodo {
  done: boolean
  text: string
}

/** @deprecated the section is `## Todos` now — use BriefTodo. */
export type BriefPlanItem = BriefTodo

/** GET/PUT /api/hosts/:host/sessions/:id/brief (`exists: false` = the empty skeleton, no file yet). */
export interface Brief {
  host: string
  id: string
  exists: boolean
  /** The whole file, frontmatter included. */
  markdown: string
  parsed: {
    summary: string
    resources: BriefResource[]
    /** Older servers omit it — fall back to `plan`. */
    todos?: BriefTodo[]
    /** @deprecated alias of `todos` (same array), kept for one release. */
    plan: BriefTodo[]
  }
  /** ISO 8601 (frontmatter), null when never written. */
  updated: string | null
  editedAt: string | null
  generatedAt: string | null
  generatedThrough: number
  generating: boolean
  /** Background generation is on for that host (`web.briefs.enabled`). */
  enabled: boolean
  /** The first prompt for a new session that continues this one. */
  continuePrompt: string
  /** The session's directory on its host, absolute (no `~`); null when unknown. Older servers omit it. */
  absCwd?: string | null
  /** The git checkout root (repo or linked worktree) containing absCwd, absolute; null outside git. */
  gitRoot?: string | null
  /** `web.editor` of the server that answered: which editor `editorUrl` opens; null = hide the button. */
  editor?: 'vscode' | 'cursor' | null
  /**
   * `vscode://file/<path>` for a session on the answering server's host, or
   * `vscode://vscode-remote/ssh-remote+<alias><path>` for another host (`cursor://…` alike);
   * gitRoot preferred over absCwd. null: no editor, no path, or no ssh alias for that host.
   */
  editorUrl?: string | null
  /** The git checkouts the session works in (its cwd's first, then those of its files). Older servers omit it. */
  worktrees?: BriefWorktree[]
}

/** One checkout in Brief.worktrees. */
export interface BriefWorktree {
  /** The checkout root on the session's host, absolute. */
  path: string
  /** `path` with `~` for the home dir. */
  display: string
  /** null when detached. */
  branch: string | null
  /** true = a linked worktree, false = the main checkout. */
  linked: boolean
  /** "Open in editor" link for `path` (see Brief.editorUrl); null = no button. */
  editorUrl?: string | null
}

/** POST …/brief/regenerate (202). */
export interface BriefRegenerateResponse {
  host: string
  id: string
  started: boolean
  queued: boolean
  generating: true
}

// --- Notes explorer (GET /api/hosts/:host/notes/*) ------------------------------------------

export type NoteKind = 'markdown' | 'text' | 'image'

export interface NoteEntry {
  /** Root-relative, `/`-separated. */
  path: string
  kind: NoteKind
  size: number
  mtime: number
  /** Markdown: frontmatter `title`, else the first `# heading`, else the file name. */
  title?: string
  /** Had age-encrypted blocks (withheld by the server). */
  encrypted?: boolean
}

export interface NotesTree {
  host: string
  name: string
  /** Display path (`~/…`). */
  root: string
  rootAbs: string
  searchEngine: 'builtin' | 'command'
  files: NoteEntry[]
  truncated: boolean
  scannedAt: number
  editorUrl: string | null
}

export interface NoteMatch {
  /** 1-based; null when the search command gave no line numbers. */
  line: number | null
  text: string
  /** [start, end) of each match in `text`. */
  ranges: [number, number][]
}

export interface NoteHit {
  path: string
  kind: NoteKind
  title: string
  mtime: number
  score: number
  matches: NoteMatch[]
  more: number
}

export interface NotesSearch {
  host: string
  q: string
  engine: 'builtin' | 'command'
  /** The search command failed and the built-in search answered instead. */
  fallback?: string
  results: NoteHit[]
  total: number
  ms: number
}

export interface NoteFile {
  host: string
  path: string
  abs: string
  kind: NoteKind
  size: number
  mtime: number
  title: string
  encrypted: boolean
  /** Frontmatter, in file order. */
  meta: [string, string | string[]][]
  body: string
  /** The line `body` starts on. */
  bodyLine: number
  text: string
  editorUrl: string | null
}

// --- Subscription usage (GET /api/hosts/:host/usage → `fleet usage --json`) -------------------

export interface UsageAccount {
  uuid: string | null
  email: string | null
  organization: string | null
  /** `pro`, `max`, `team`, `enterprise`, … */
  plan: string | null
  tier: string | null
  /** e.g. `Team · Max 5x`. */
  plan_label: string | null
}

export interface UsageLimit {
  /** `session`, `weekly_all`, `weekly_scoped`, … */
  kind: string
  /** `session` | `weekly`. */
  group: string
  label: string
  /** The model a scoped limit counts (`Fable`, `Opus`, …). */
  model: string | null
  /** 0–100 (may exceed 100). */
  percent: number
  /** `normal`, `warning`, `critical`, … (open set). */
  severity: string
  resets_at: string | null
  /** The limit currently binding. */
  active: boolean
}

export interface UsageExtra {
  enabled: boolean
  used: number | null
  limit: number | null
  currency: string | null
  percent: number | null
}

export interface UsageResponse {
  host: string
  account: UsageAccount | null
  limits: UsageLimit[]
  extra_usage: UsageExtra | null
  /** RFC 3339: when the numbers were read from Anthropic. */
  fetched_at: string
  /** A refresh failed; these are the last good numbers. */
  stale: boolean
  error: string | null
}

/** A Claude session inside a dormant entry (`fleet restore --json`). */
export interface DormantSession {
  sessionId: string
  name: string | null
  title: string | null
  cwd: string | null
}

/** One dormant tmux session (`kind: tmux`) or lone Claude session (`kind: claude`) a reboot left. */
export interface DormantView {
  kind: 'tmux' | 'claude' | string
  /** What `fleet restore <target>` takes: the tmux name, or the full session id. */
  target: string
  /** The tmux name, or the Claude title. */
  name: string
  /** When it went down (ISO). */
  since: string | null
  windows: number
  panes: number
  sessions: DormantSession[]
}

/**
 * One recently closed entry (ended within a boot: Close, kill, /exit): a DormantView (`since` =
 * `closedAt`) + `tmuxSession` when a Claude session's tmux pane outlived it (it resumes in a new
 * window there). Never part of Resume all, never on the Board.
 */
export interface ClosedView extends DormantView {
  closedAt: string | null
  tmuxSession?: string
}

/** GET /api/hosts/:host/dormant. */
export interface DormantResponse {
  host: string
  bootId: string | null
  dormant: DormantView[]
  /** Recently closed sessions (servers / CLIs that know them). */
  closed?: ClosedView[]
}

export interface RestoredEntry {
  kind: string
  from: string
  /** The tmux session it now lives in. */
  session: string
  renamed: boolean
  windows: number
  panes: number
  launched: { sessionId: string; title: string | null; line: string }[]
  warnings: string[]
  commands: string[]
  dryRun: boolean
}

/** POST /api/hosts/:host/dormant/restore. 409 `{ error, candidates }` when the target is ambiguous. */
export interface RestoreResponse {
  host: string
  restored: RestoredEntry[]
  failed: { target: string; error: string }[]
}

/** POST /api/hosts/:host/dormant/forget. */
export interface ForgetResponse {
  host: string
  forgotten: string[]
}

// --- Git repos (Settings → Git repos; GET/PUT /api/hosts/:host/repos → `fleet repos`) -----------

/** Config `repos` on one host (defaults filled in). Keys of `overrides` / `exclude`: a dir name or a `~/…` path. */
export interface ReposSettings {
  roots: string[]
  /** `30m` / `12h` / `7d`. */
  every: string
  overrides: Record<string, string>
  exclude: string[]
}

export type RepoOutcome = 'updated' | 'current' | 'ahead' | 'diverged' | 'blocked' | 'busy' | 'noUpstream' | 'noRemote' | 'fetchFailed'

/** One row of `fleet repos --json --all`. */
export interface RepoRow {
  name: string
  /** `~/…` */
  path: string
  /** Sync interval in seconds. */
  every: number
  excluded: boolean
  due: boolean
  branch: string | null
  upstream?: string | null
  ahead: number
  behind: number
  dirty: number
  busy?: string | null
  lastAttempt?: number | null
  lastFetch?: number | null
  outcome?: RepoOutcome | null
  detail?: string | null
}

export interface ReposResponse {
  host: string
  settings: ReposSettings
  repos: RepoRow[]
  /** Listing the repos failed (the settings are still there). */
  error: string | null
  service: { installed: boolean; supported: boolean; tickMinutes: number }
  /** PUT only. */
  saved?: boolean
}

/** One result of `fleet repos sync --json`. */
export interface RepoSyncResult {
  name: string
  path: string
  branch?: string | null
  outcome: RepoOutcome
  pulled: number
  defaultBranch?: { branch: string; pulled: number }
  detail?: string
}

/** A `fleet prs` column id (docs/cli.md → Pull requests). */
export type PrColumnId = 'unseenMentions' | 'waitingOnMe' | 'reviewMine' | 'readyToMergeMine' | 'draftMine' | 'waitingOnAuthor'

export interface PrColumn {
  id: PrColumnId
  title: string
  count: number
}

export interface PullRequest {
  column: PrColumnId
  owner: string
  repo: string
  number: number
  title: string
  url: string
  author: string
  draft: boolean
  /** ms epoch */
  createdAt: number
  updatedAt: number
  additions: number
  deletions: number
  changedFiles: number
  size: 'XS' | 'S' | 'M' | 'L' | 'XL'
  baseBranch: string
  headBranch: string
  headOid: string | null
  reviewDecision: 'approved' | 'changesRequested' | 'reviewRequired' | null
  checks: 'success' | 'failure' | 'error' | 'pending' | 'expected' | null
  comments: number
  requestedReviewers: string[]
  myReview: { state: string; at: number | null; newCommits: boolean } | null
  /** The latest @mention of you by someone else; unseen → the PR is in `unseenMentions`. */
  mention?: PrMention
  /** How long it has waited, by its column's thresholds (absent from older servers). */
  rotting?: PrRotting
  /** Where the repo / head branch is checked out on the answering host. */
  /** `editorUrl`: "Open in VS Code / Cursor" for the worktree, from the server the browser asked (null: no link). */
  local?: { repoPath: string; worktree: string | null; editorUrl?: string | null }
  /** Productive task ids its description / title links to (absent from older servers). */
  tasks?: string[]
}

export type PrRotting = 'fresh' | 'warming' | 'stale' | 'rotting' | 'critical'

export interface PrMention {
  /** ms epoch */
  at: number
  author: string
  /** The comment / review it was said in. */
  url: string
  /** The line with the mention, shortened. */
  excerpt: string
  /** You commented or reviewed after it. */
  answered: boolean
  /** Answered, or marked seen after it. */
  seen: boolean
}

/** GET /api/hosts/:host/prs */
export interface PrsResponse {
  host: string
  user: string
  scope: string
  fetchedAt: number
  stale: boolean
  error: string | null
  truncated: boolean
  columns: PrColumn[]
  prs: PullRequest[]
}

/** A `fleet tasks` column id (docs/cli.md → Tasks). */
export type TaskColumnId = 'due' | 'inProgress' | 'toDo'

export interface TaskColumn {
  id: TaskColumnId
  title: string
  count: number
}

export type TaskStatusCategory = 'notStarted' | 'started' | 'closed'

/** A workflow status (a stage in a project's task workflow). */
export interface TaskStatus {
  id: string
  name: string
  category: TaskStatusCategory | string
  position: number
}

/** One of your open Productive tasks (`fleet tasks --json` → `tasks[]`). */
export interface Task {
  id: string
  /** The project-scoped number (`#123`). */
  number: string
  title: string
  url: string
  column: TaskColumnId
  project: { id: string; name: string }
  taskList: string | null
  status: TaskStatus
  /** Key into `TasksResponse.statuses`: the statuses it can move to. */
  workflowId: string
  /** `YYYY-MM-DD` */
  dueDate: string | null
  startDate: string | null
  /** ms epoch */
  createdAt: number
  updatedAt: number
  lastActivityAt: number | null
  todos: number
  openTodos: number
  tags: string[]
  /** Plain-text excerpt of the description ('' when none). */
  description: string
}

/** GET /api/hosts/:host/tasks */
export interface TasksResponse {
  host: string
  user: { id: string; name: string }
  organization: { id: string; name: string; slug: string }
  fetchedAt: number
  stale: boolean
  error: string | null
  truncated: boolean
  columns: TaskColumn[]
  tasks: Task[]
  /** workflow id → its statuses. */
  statuses: Record<string, TaskStatus[]>
}

/** POST /api/hosts/:host/tasks/spawn */
export interface TaskSpawnResponse {
  host: string
  task: Task & { contextLine: string }
  spawn: SpawnResponse
}

/** A PR checked out in a worktree (`fleet pr checkout --json`). */
export interface PrWorktree {
  owner: string
  repo: string
  number: number
  title?: string
  url?: string
  headBranch?: string
  baseBranch?: string
  /** The local branch (`pr-<n>` for a fork's PR). */
  branch: string
  path: string
  repoPath?: string
  /** Made by this call (false: it was already checked out). */
  created: boolean
  contextLine: string
  /** "Open in VS Code / Cursor" for `path` (null: no link). */
  editorUrl?: string | null
}

/** POST /api/hosts/:host/prs/spawn */
export interface PrSpawnResponse {
  host: string
  worktree: PrWorktree
  spawn: SpawnResponse
}

/** A job run's outcome (docs/cli.md → Jobs). Unknown strings are possible from newer CLIs. */
export type JobOutcome = 'spawned' | 'skipped' | 'missed' | 'failed'

/** A session a run started (`parent`: the tmux session of the job session that spawned it; null = the run's first). */
export interface JobRunSession {
  tmuxSession: string
  name?: string | null
  parent: string | null
  /** Epoch ms. */
  at: number
  closedAt: number | null
}

export interface JobRun {
  id: string
  trigger: 'schedule' | 'manual' | string
  startedAt: number
  slot: number | null
  outcome: JobOutcome | string
  reason: string | null
  promptFile: string | null
  sessions: JobRunSession[]
}

/** A live session of a job (`fleet jobs --json` → `live[]`). */
export interface JobLive {
  run: string | null
  sessionId: string
  tmuxSession: string | null
  status: string
  title: string | null
}

/** One job (`fleet jobs --json` / `fleet jobs show <name> --json`) plus the host it lives on. */
export interface Job {
  host?: string
  name: string
  schedule: string
  /** The schedule in plain English ("Every day at 07:00"); absent from an older CLI. */
  scheduleText?: string
  enabled: boolean
  dir: string
  harness: string
  model: string | null
  command: string | null
  /** Seconds; null = never. */
  keep: number | null
  /**
   * A file prompt's `abs`: the path on the job's host (`~` expanded); `root`: its git checkout,
   * else its directory. `editorUrl` opens the file in the editor, `rootEditorUrl` the root (null
   * without an editor or an ssh alias). All absent from older servers.
   */
  prompt:
    | { inline: string }
    | { file: string; abs?: string; root?: string; editorUrl?: string | null; rootEditorUrl?: string | null }
    | null
  /** Epoch ms; null when paused (or the schedule never fires). */
  nextRun: number | null
  lastSlot: number | null
  /** Newest first: the latest one from the list, every kept run from `show`. */
  runs: JobRun[]
  live: JobLive[]
}

/** GET /api/hosts/:host/jobs */
export interface JobsResponse {
  host: string
  jobs: Job[]
}

/** POST /api/hosts/:host/jobs/:name/run */
export interface JobRunResponse {
  host: string
  name: string
  output: string
}

/** POST /api/hosts/:host/jobs/:name/enabled */
export interface JobEnabledResponse {
  host: string
  name: string
  enabled: boolean
  output: string
  job: Job | null
}

/** One voice memo in GET /api/memos (newest first). */
export interface MemoSummary {
  /** The markdown file's name without `.md`. */
  id: string
  title: string
  /** ISO with offset, when recording started. */
  created: string | null
  durationSec: number | null
  /** `partial`: a segment is not transcribed yet (POST …/retry). */
  status: 'done' | 'partial'
  words: number
  preview: string
}

export interface MemosResponse {
  /** Where the markdown files live on the server. */
  dir: string
  memos: MemoSummary[]
}

/** GET/PUT /api/memos/:id, POST …/finish, POST …/retry. */
export interface Memo extends MemoSummary {
  /** The markdown body (the transcript, plus anything added to the file since). */
  transcript: string
  /** Absolute path of the file on the server. */
  path: string
}

/** PUT /api/memos/recordings/:rec/segments/:n — the audio is stored even when transcription failed. */
export interface MemoSegmentResponse {
  stored: true
  transcribed: boolean
  error?: string
}

// --- diff reviewer (GET …/sessions/:id/diff) -------------------------------------------------

export interface DiffLine {
  /** ' ' context, '+' added, '-' removed */
  t: ' ' | '+' | '-'
  /** old / new line number; null on the side the line is not on */
  o: number | null
  n: number | null
  text: string
  noEol?: boolean
}

export interface DiffHunk {
  header: string
  oldStart: number
  oldLines: number
  newStart: number
  newLines: number
  /** the function / section git names after @@ */
  section: string
  lines: DiffLine[]
}

export interface DiffFile {
  path: string
  oldPath: string | null
  status: 'added' | 'deleted' | 'modified' | 'renamed' | 'copied'
  binary: boolean
  additions: number
  deletions: number
  truncated: boolean
  hunks: DiffHunk[]
}

export interface DiffResponse {
  host: string
  id: string
  root: string
  branch: string | null
  head: string | null
  base: { ref: string | null; sha: string }
  tree: string
  /** pass back as `since`: unchanged → `{ unchanged: true }` without `files` */
  fingerprint: string
  unchanged: boolean
  files?: DiffFile[]
  stats?: { files: number; additions: number; deletions: number }
  truncated?: boolean
}

// --- diff reviewer threads (…/sessions/:id/review) ------------------------------------------

export interface ReviewReply {
  text: string
  ts: number | null
  final: boolean
}

/** One comment or follow-up in a thread and the session's answer to it. */
export interface ReviewEntry {
  body: string
  /** null while a draft */
  sentAt: number | null
  /** draft → sent (typed, not in the transcript yet) → queued → working → done */
  state: 'draft' | 'sent' | 'queued' | 'working' | 'done'
  replies: ReviewReply[]
  turnTs?: number
}

export interface ReviewThread {
  id: string
  /** the checkout the path is in (null: comments made before the reviewer knew several) */
  root?: string | null
  /** how the prompt named the file (relative to the session cwd) */
  display?: string
  path: string
  side: 'new' | 'old'
  start: number
  end: number
  /** the selected lines when the comment was made */
  code: string[]
  base: string | null
  createdAt: number
  status: 'draft' | 'sent'
  resolved: boolean
  reviewId: string | null
  entries: ReviewEntry[]
}

export interface ReviewState {
  host: string
  id: string
  threads: ReviewThread[]
  reviews: { id: string; body: string; sentAt: number; threads: string[] }[]
  drafts: number
}

export interface ReviewAnchor {
  root: string | null
  path: string
  side: 'new' | 'old'
  start: number
  end: number
  code: string[]
}

/** One checkout in GET …/diffs: the session's cwd checkout first, then those of the files it worked on. */
export interface DiffRepo {
  root: string
  /** relative to the session cwd when inside it, else ~/… */
  label: string
  branch?: string | null
  head?: string | null
  base?: { ref: string | null; sha: string }
  tree?: string
  fingerprint?: string
  files?: DiffFile[]
  stats?: { files: number; additions: number; deletions: number }
  truncated?: boolean
  /** the checkout could not be read (a removed worktree) */
  error?: string
}

export interface DiffsResponse {
  host: string
  id: string
  cwd: string | null
  /** pass back as `since`: unchanged → `{ unchanged: true }` without `repos` */
  fingerprint: string
  unchanged: boolean
  repos?: DiffRepo[]
}

/** One source of GET /api/inbox — a missing key in `sources` means that source is off. */
export interface InboxSource {
  /** ms epoch of the last read */
  at: number | null
  ok: boolean
  error: string | null
  count: number
  /** comms: the next Slack & email scan (ms epoch) */
  nextAt?: number | null
  /** comms: a scan is running now */
  running?: boolean
}

export type InboxSection = 'quick' | 'waiting' | 'mine'

/** One thing waiting on you. The list is the server's: items leave it when their source resolves them. */
export interface InboxItem {
  /** stable: `pr:<url>`, `task:<id>`, `session:<host>/<id>`, `slack:…`, `email:…` */
  id: string
  source: 'github' | 'productive' | 'session' | 'slack' | 'email'
  kind: 'review' | 'mention' | 'myPr' | 'task' | 'session' | 'message'
  section: InboxSection
  title: string
  /** one line: what to do */
  ask: string | null
  /** external link (GitHub, Productive, Slack, Gmail) */
  url: string | null
  /** in-app hash route (`#/s/<host>/<id>`) — preferred over `url` */
  href: string | null
  from: string | null
  /** ms epoch, latest activity */
  at: number
  /** ms epoch, when the inbox first saw it */
  firstSeen: number
  effort: 'quick' | 'medium' | 'deep' | null
  meta: {
    repo?: string
    number?: number
    size?: string
    additions?: number
    deletions?: number
    checks?: string | null
    draft?: boolean
    project?: string
    /** YYYY-MM-DD */
    due?: string
    overdue?: boolean
    status?: string
    host?: string
    sessionId?: string
  }
}

/** GET /api/inbox — served by the inbox host (`inbox.host`), proxied from everywhere else. */
/** When the Slack & email scan runs: every `scanMinutes`, on `workDays` (0 = Sunday) within `workHours` [from, to). */
export interface InboxSettings {
  scanMinutes: number
  workHours: [number, number]
  workDays: number[]
}

export interface InboxResponse {
  /** false: no inbox host configured or reachable */
  enabled: boolean
  host: string | null
  error?: string
  updatedAt: number | null
  brief: { text: string; at: number; model: string } | null
  /** the inbox host's scan schedule (Settings → Inbox); absent on older servers / when off */
  settings?: InboxSettings
  sources: { github?: InboxSource; productive?: InboxSource; sessions?: InboxSource; comms?: InboxSource }
  /** sorted: section order, then `at` newest first */
  items: InboxItem[]
}
