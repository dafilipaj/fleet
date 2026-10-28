// Typed client for the fleet web HTTP API. Same origin (the server serves this UI);
// in dev, Vite proxies /api to a running server (see vite.config.ts).
import type {
  DiffResponse,
  DiffsResponse,
  ReviewAnchor,
  SlackLink,
  SlackStatus,
  ReviewState,
  ReviewThread,
  Memo,
  MemoSegmentResponse,
  MemosResponse,
  AutoNameRun,
  Brief,
  BriefRegenerateResponse,
  CompleteKind,
  CompleteResponse,
  DormantResponse,
  FileOpenResponse,
  FocusStatus,
  ForgetResponse,
  FileStatResponse,
  FleetResponse,
  GroupsResponse,
  InboxResponse,
  InboxSettings,
  HarnessesResponse,
  Job,
  JobEnabledResponse,
  JobRunResponse,
  JobsResponse,
  Health,
  KillResponse,
  MessagesResponse,
  NoteFile,
  NotesSearch,
  NotesTree,
  OkResponse,
  PeekResponse,
  PromptQueue,
  QueuedPrompt,
  QuestionAnswer,
  RegenerateNameResponse,
  RenameResponse,
  ReposResponse,
  ReposSettings,
  ScheduleAddBody,
  ScheduleResponse,
  ScheduledEntry,
  RepoSyncResult,
  RestoreResponse,
  SessionKey,
  Settings,
  SpawnDirEntry,
  SpawnDirsResponse,
  SpawnRequest,
  SpawnResponse,
  StackLinkRequest,
  StackLinkResponse,
  StackSpawnRequest,
  StackSpawnResponse,
  StacksResponse,
  StackView,
  TranscribeResponse,
  OperatorStatus,
  UploadResponse,
  PrSpawnResponse,
  PrWorktree,
  PrsResponse,
  Task,
  TasksResponse,
  TaskSpawnResponse,
  UsageResponse,
} from './types'
import type { GroupEdit } from '@/lib/groups'

/** A failed request. `status` is the HTTP status, or 0 when the network is unreachable. */
export class ApiError extends Error {
  status: number
  /** The parsed JSON error body (e.g. `{ error, retryAfterMs }`), when there was one. */
  data: unknown
  constructor(message: string, status: number, data: unknown = null) {
    super(message)
    this.name = 'ApiError'
    this.status = status
    this.data = data
  }
}

export function isAbortError(err: unknown): boolean {
  return err instanceof DOMException ? err.name === 'AbortError' : (err as { name?: string })?.name === 'AbortError'
}

export interface RequestOptions {
  signal?: AbortSignal
  method?: 'GET' | 'POST' | 'PUT' | 'DELETE'
  body?: unknown
  /** A raw body (a File for uploads), sent as-is with its own type. */
  file?: Blob
  /** Let the request outlive the page (unload, app close) — small bodies only (64 KB). */
  keepalive?: boolean
}

export async function request<T>(path: string, { signal, method = 'GET', body, file, keepalive }: RequestOptions = {}): Promise<T> {
  const init: RequestInit = { method, signal, headers: {} }
  if (keepalive) init.keepalive = true
  if (file !== undefined) {
    init.headers = { 'content-type': file.type || 'application/octet-stream' }
    init.body = file
  } else if (body !== undefined) {
    init.headers = { 'content-type': 'application/json' }
    init.body = JSON.stringify(body)
  }
  let res: Response
  try {
    res = await fetch(path, init)
  } catch (err) {
    if (isAbortError(err)) throw err
    throw new ApiError('network unreachable', 0)
  }
  let data: unknown = null
  try {
    data = await res.json()
  } catch {
    data = null
  }
  if (!res.ok) {
    const msg = (data as { error?: string } | null)?.error
    throw new ApiError(msg || `HTTP ${res.status}`, res.status, data)
  }
  return data as T
}

const enc = encodeURIComponent
const hostPath = (host: string) => `/api/hosts/${enc(host)}`
const sessionPath = (host: string, id: string, suffix: string) => `${hostPath(host)}/sessions/${enc(id)}/${suffix}`

type Opts = { signal?: AbortSignal }

export const api = {
  health: (o: Opts = {}) => request<Health>('/api/health', o),
  settings: (o: Opts = {}) => request<Settings>('/api/settings', o),
  fleet: (o: Opts = {}) => request<FleetResponse>('/api/fleet', o),

  peek: (host: string, id: string, lines = 200, o: Opts = {}) =>
    request<PeekResponse>(`${sessionPath(host, id, 'peek')}?lines=${lines}`, o),
  messages: (host: string, id: string, limit = 60, o: Opts = {}) =>
    request<MessagesResponse>(`${sessionPath(host, id, 'messages')}?limit=${limit}`, o),

  /** The session's worktree against the merge-base with `base` (default branch); `since` = the last fingerprint. */
  diff: (host: string, id: string, { base, since }: { base?: string | null; since?: string | null } = {}, o: Opts = {}) => {
    const q = new URLSearchParams()
    if (base) q.set('base', base)
    if (since) q.set('since', since)
    const qs = q.toString()
    return request<DiffResponse>(`${sessionPath(host, id, 'diff')}${qs ? `?${qs}` : ''}`, o)
  },

  /** Every checkout the session works in (cwd's + those of the files it worked on); `since` = the last fingerprint. */
  diffs: (host: string, id: string, since: string | null = null, o: Opts = {}) =>
    request<DiffsResponse>(`${sessionPath(host, id, 'diffs')}${since ? `?since=${enc(since)}` : ''}`, o),
  // Prompt queue (server-side): sent one per run once the session is idle.
  queueAdd: (host: string, id: string, text: string, o: Opts = {}) =>
    request<PromptQueue & { item: QueuedPrompt }>(sessionPath(host, id, 'queue'), { ...o, method: 'POST', body: { text } }),
  queueEdit: (host: string, id: string, item: string, text: string, o: Opts = {}) =>
    request<PromptQueue>(sessionPath(host, id, `queue/${enc(item)}`), { ...o, method: 'PUT', body: { text } }),
  queueDelete: (host: string, id: string, item: string, o: Opts = {}) =>
    request<PromptQueue>(sessionPath(host, id, `queue/${enc(item)}`), { ...o, method: 'DELETE' }),
  queueSend: (host: string, id: string, item: string, o: Opts = {}) =>
    request<PromptQueue>(sessionPath(host, id, `queue/${enc(item)}/send`), { ...o, method: 'POST', body: {} }),
  queueMove: (host: string, id: string, item: string, to: number, o: Opts = {}) =>
    request<PromptQueue>(sessionPath(host, id, `queue/${enc(item)}/move`), { ...o, method: 'POST', body: { to } }),
  queuePause: (host: string, id: string, paused: boolean, o: Opts = {}) =>
    request<PromptQueue>(sessionPath(host, id, 'queue/pause'), { ...o, method: 'POST', body: { paused } }),
  review: (host: string, id: string, o: Opts = {}) => request<ReviewState>(sessionPath(host, id, 'review'), o),
  /** `draft: false` types the comment into the session now; `true` keeps it for `submitReview`. */
  reviewComment: (host: string, id: string, body: ReviewAnchor & { body: string; draft: boolean; base?: string | null }, o: Opts = {}) =>
    request<ReviewState & { thread: ReviewThread }>(sessionPath(host, id, 'review/threads'), { ...o, method: 'POST', body }),
  reviewReply: (host: string, id: string, thread: string, body: string, o: Opts = {}) =>
    request<ReviewState & { thread: ReviewThread }>(sessionPath(host, id, `review/threads/${enc(thread)}/reply`), { ...o, method: 'POST', body: { body } }),
  reviewUpdate: (host: string, id: string, thread: string, patch: { body?: string; resolved?: boolean }, o: Opts = {}) =>
    request<ReviewState & { thread: ReviewThread }>(sessionPath(host, id, `review/threads/${enc(thread)}`), { ...o, method: 'PUT', body: patch }),
  reviewDelete: (host: string, id: string, thread: string, o: Opts = {}) =>
    request<ReviewState>(sessionPath(host, id, `review/threads/${enc(thread)}`), { ...o, method: 'DELETE' }),
  /** Every draft in one prompt, with an optional overall note. */
  submitReview: (host: string, id: string, body: string, o: Opts = {}) =>
    request<ReviewState>(sessionPath(host, id, 'review/submit'), { ...o, method: 'POST', body: { body } }),

  send: (host: string, id: string, text: string, o: Opts & { keepalive?: boolean } = {}) =>
    request<OkResponse>(sessionPath(host, id, 'send'), { ...o, method: 'POST', body: { text } }),
  keys: (host: string, id: string, key: SessionKey, o: Opts = {}) =>
    request<OkResponse>(sessionPath(host, id, 'keys'), { ...o, method: 'POST', body: { key } }),
  answer: (host: string, id: string, toolUseId: string, answers: QuestionAnswer[], o: Opts = {}) =>
    request<OkResponse>(sessionPath(host, id, 'answer'), { ...o, method: 'POST', body: { toolUseId, answers } }),
  rename: (host: string, id: string, title: string, o: Opts = {}) =>
    request<RenameResponse>(sessionPath(host, id, 'rename'), { ...o, method: 'POST', body: { title } }),
  regenerateName: (host: string, id: string, o: Opts = {}) =>
    request<RegenerateNameResponse>(sessionPath(host, id, 'autoname'), { ...o, method: 'POST', body: {} }),
  /** Ask the session to report itself done once finished (`fleet ask-done`; tmux only). */
  askDone: (host: string, id: string, o: Opts = {}) =>
    request<OkResponse>(sessionPath(host, id, 'ask-done'), { ...o, method: 'POST', body: {} }),
  kill: (host: string, id: string, o: Opts = {}) =>
    request<KillResponse>(sessionPath(host, id, 'kill'), { ...o, method: 'POST', body: {} }),

  spawn: (host: string, body: SpawnRequest, o: Opts = {}) =>
    request<SpawnResponse>(`${hostPath(host)}/spawn`, { ...o, method: 'POST', body }),
  /** The host's Start directories (config `spawnDirs`) and whether its own paths exist. */
  harnesses: (host: string, o: Opts = {}) => request<HarnessesResponse>(`${hostPath(host)}/harnesses`, o),
  spawnDirs: (host: string, o: Opts = {}) => request<SpawnDirsResponse>(`${hostPath(host)}/spawn-dirs`, o),
  /** Replace the host's list (400 `SpawnDirsError` when invalid); `dryRun` validates only. */
  saveSpawnDirs: (host: string, spawnDirs: SpawnDirEntry[], { dryRun = false, ...o }: Opts & { dryRun?: boolean } = {}) =>
    request<SpawnDirsResponse>(`${hostPath(host)}/spawn-dirs`, { ...o, method: 'PUT', body: { spawnDirs, ...(dryRun ? { dryRun } : {}) } }),
  /** The host's scheduled jobs (`fleet jobs --json`; each with its latest run). 501: an older CLI. */
  jobs: (host: string, o: Opts = {}) => request<JobsResponse>(`${hostPath(host)}/jobs`, o),
  /** One job with all its kept runs, newest first (`fleet jobs show <name> --json`). */
  job: (host: string, name: string, o: Opts = {}) => request<Job>(`${hostPath(host)}/jobs/${enc(name)}`, o),
  /** Start a run now (starts an agent). 409 `{ busy: true }` while a session of the job works, unless `force`. */
  runJob: (host: string, name: string, force = false, o: Opts = {}) =>
    request<JobRunResponse>(`${hostPath(host)}/jobs/${enc(name)}/run`, { ...o, method: 'POST', body: force ? { force } : {} }),
  /** Pause / resume a job (`jobs.<name>.enabled`). */
  setJobEnabled: (host: string, name: string, enabled: boolean, o: Opts = {}) =>
    request<JobEnabledResponse>(`${hostPath(host)}/jobs/${enc(name)}/enabled`, { ...o, method: 'POST', body: { enabled } }),
  /** The host's scheduled sessions, pending and recent (`fleet schedule --json`). 404 / 501: an older server / CLI. */
  schedule: (host: string, o: Opts = {}) => request<ScheduleResponse>(`${hostPath(host)}/schedule`, o),
  /** Start a session later on `host` (an agent starts then, not now). */
  scheduleAdd: (host: string, body: ScheduleAddBody, o: Opts = {}) =>
    request<{ host: string; entry: ScheduledEntry }>(`${hostPath(host)}/schedule`, { ...o, method: 'POST', body }),
  /** Cancel a pending scheduled session. */
  cancelScheduled: (host: string, id: string, o: Opts = {}) =>
    request<{ host: string; id: string; output: string }>(`${hostPath(host)}/schedule/${enc(id)}/cancel`, { ...o, method: 'POST', body: {} }),
  /** The host's git repos (`fleet repos --json --all`), its `repos` settings and its sync timer. */
  repos: (host: string, o: Opts = {}) => request<ReposResponse>(`${hostPath(host)}/repos`, o),
  /** Replace the host's `repos` settings (400 `{ error, errors }` when invalid). */
  saveRepos: (host: string, settings: ReposSettings, o: Opts = {}) =>
    request<ReposResponse>(`${hostPath(host)}/repos`, { ...o, method: 'PUT', body: settings }),
  /** Fetch + fast-forward now (every repo, or `names`); 409 while a sync runs there. */
  syncRepos: (host: string, names: string[] = [], o: Opts = {}) =>
    request<{ host: string; results: RepoSyncResult[] }>(`${hostPath(host)}/repos/sync`, { ...o, method: 'POST', body: names.length ? { names } : {} }),
  /** Turn the host's sync timer (launchd) on or off. */
  reposService: (host: string, install: boolean, o: Opts = {}) =>
    request<ReposResponse>(`${hostPath(host)}/repos/service`, { ...o, method: 'POST', body: { install } }),
  autoname: (host: string, o: Opts = {}) =>
    request<AutoNameRun>(`${hostPath(host)}/autoname`, { ...o, method: 'POST', body: {} }),

  /** A voice recording as text (always this server); `hint` = words to expect (session name, repo). */
  transcribe: (audio: Blob, hint: string, o: Opts = {}) =>
    request<TranscribeResponse>(`/api/transcribe?hint=${enc(hint)}`, { ...o, method: 'POST', file: audio }),
  /** Text → spoken mp3 (the voice operator's replies). */
  memos: (o: Opts = {}) => request<MemosResponse>('/api/memos', o),
  memo: (id: string, o: Opts = {}) => request<Memo>(`/api/memos/${enc(id)}`, o),
  putMemoSegment: (rec: string, n: number, audio: Blob, o: Opts = {}) =>
    request<MemoSegmentResponse>(`/api/memos/recordings/${enc(rec)}/segments/${n}`, { ...o, method: 'PUT', file: audio }),
  finishMemo: (rec: string, body: { segments: number; startedAt: number; durationMs: number }, o: Opts = {}) =>
    request<Memo>(`/api/memos/recordings/${enc(rec)}/finish`, { ...o, method: 'POST', body }),
  renameMemo: (id: string, title: string, o: Opts = {}) => request<Memo>(`/api/memos/${enc(id)}`, { ...o, method: 'PUT', body: { title } }),
  deleteMemo: (id: string, o: Opts = {}) => request<{ deleted: string }>(`/api/memos/${enc(id)}`, { ...o, method: 'DELETE' }),
  retryMemo: (id: string, o: Opts = {}) => request<Memo>(`/api/memos/${enc(id)}/retry`, { ...o, method: 'POST', body: {} }),
  speak: async (text: string, o: Opts = {}): Promise<Blob> => {
    let res: Response
    try {
      res = await fetch('/api/speak', { method: 'POST', signal: o.signal, headers: { 'content-type': 'application/json' }, body: JSON.stringify({ text }) })
    } catch (err) {
      if (isAbortError(err)) throw err
      throw new ApiError('network unreachable', 0)
    }
    if (!res.ok) {
      const data = (await res.json().catch(() => null)) as { error?: string } | null
      throw new ApiError(data?.error || `HTTP ${res.status}`, res.status, data)
    }
    return res.blob()
  },
  operator: (o: Opts = {}) => request<OperatorStatus>('/api/operator', o),
  startOperator: (o: Opts = {}) => request<OperatorStatus>('/api/operator', { ...o, method: 'POST', body: {} }),

  /** Store a file on `host`; the response carries its absolute path there. */
  upload: (host: string, file: Blob, name: string, o: Opts = {}) =>
    request<UploadResponse>(`${hostPath(host)}/uploads?name=${enc(name)}`, { ...o, method: 'POST', file }),

  /** What a message to this session can reference: skills, the cwd's .md / .plain files, brain notes. */
  complete: (host: string, id: string, kind: CompleteKind, o: Opts = {}) =>
    request<CompleteResponse>(`${sessionPath(host, id, 'complete')}?kind=${kind}`, o),
  /** Which of these paths (as written in chat, relative to the session cwd) exist on the session's host. */
  fileStat: (host: string, id: string, paths: string[], o: Opts = {}) =>
    request<FileStatResponse>(sessionPath(host, id, 'files/stat'), { ...o, method: 'POST', body: { paths } }),
  /** Open the file with its default app — on `host`, not in this browser. */
  fileOpen: (host: string, id: string, path: string, o: Opts = {}) =>
    request<FileOpenResponse>(sessionPath(host, id, 'files/open'), { ...o, method: 'POST', body: { path } }),
  /** The file's text (preview); 413 when it is too large. */
  fileText: async (host: string, id: string, path: string, o: Opts = {}) => {
    let res: Response
    try {
      res = await fetch(fileRawUrl(host, id, path), { signal: o.signal })
    } catch (err) {
      if (isAbortError(err)) throw err
      throw new ApiError('network unreachable', 0)
    }
    if (!res.ok) {
      const msg = await res.json().then((d: { error?: string }) => d?.error, () => null)
      throw new ApiError(msg || `HTTP ${res.status}`, res.status)
    }
    return res.text()
  },

  /** The session's brief (never calls the model); `exists: false` = none yet. */
  brief: (host: string, id: string, o: Opts = {}) => request<Brief>(sessionPath(host, id, 'brief'), o),
  /** A human edit: the body markdown (the server keeps its frontmatter keys). */
  saveBrief: (host: string, id: string, markdown: string, o: Opts = {}) =>
    request<Brief>(sessionPath(host, id, 'brief'), { ...o, method: 'PUT', body: { markdown } }),
  /** 202 at once; poll `brief` until `generating` is false. 429 `{ retryAfterMs }` at the hourly cap. */
  regenerateBrief: (host: string, id: string, o: Opts = {}) =>
    request<BriefRegenerateResponse>(sessionPath(host, id, 'brief/regenerate'), { ...o, method: 'POST', body: {} }),

  notesTree: (host: string, o: Opts = {}) => request<NotesTree>(`${hostPath(host)}/notes/tree`, o),
  notesSearch: (host: string, q: string, limit = 50, o: Opts = {}) =>
    request<NotesSearch>(`${hostPath(host)}/notes/search?q=${enc(q)}&limit=${limit}`, o),
  noteFile: (host: string, path: string, o: Opts = {}) => request<NoteFile>(`${hostPath(host)}/notes/file?path=${enc(path)}`, o),

  /** Session stacks on `host` (404 / 501 on servers that predate stacks — see lib/stacks.ts `stacksMissing`). */
  stacks: (host: string, o: Opts = {}) => request<StacksResponse>(`${hostPath(host)}/stacks`, o),
  stack: (host: string, id: string, o: Opts = {}) => request<StackView>(`${hostPath(host)}/stacks/${enc(id)}`, o),
  /** A human edit of the whole StackBrief; 409 `{ error, updated }` when `expectUpdated` is stale. */
  saveStack: (host: string, id: string, markdown: string, expectUpdated?: string | null, o: Opts = {}) =>
    request<StackView>(`${hostPath(host)}/stacks/${enc(id)}`, {
      ...o,
      method: 'PUT',
      body: { markdown, ...(expectUpdated ? { expectUpdated } : {}) },
    }),
  /** Rename a stack (400 bad label, 404 unknown stack, 501 / router 404 on servers without stacks). */
  renameStack: (host: string, id: string, label: string, o: Opts = {}) =>
    request<StackView>(`${hostPath(host)}/stacks/${enc(id)}/rename`, { ...o, method: 'POST', body: { label } }),
  deleteStack: (host: string, id: string, o: Opts = {}) =>
    request<{ removed: string }>(`${hostPath(host)}/stacks/${enc(id)}`, { ...o, method: 'DELETE' }),
  /** A sibling of `sessionId`: its stack (created first when it has none — one model call), then a new session in its cwd. */
  spawnSibling: (host: string, sessionId: string, body: StackSpawnRequest, o: Opts = {}) =>
    request<StackSpawnResponse>(sessionPath(host, sessionId, 'stack/spawn'), { ...o, method: 'POST', body }),
  /** A new session in an existing stack (in the stack's directory). */
  spawnInStack: (host: string, stackId: string, body: StackSpawnRequest, o: Opts = {}) =>
    request<StackSpawnResponse>(`${hostPath(host)}/stacks/${enc(stackId)}/spawn`, { ...o, method: 'POST', body }),
  /** Put an existing session (`body.session` on `body.host`) into `sessionId`'s stack — created around it when it has none (one model call). */
  linkToStack: (host: string, sessionId: string, body: StackLinkRequest, o: Opts = {}) =>
    request<StackLinkResponse>(sessionPath(host, sessionId, 'stack/link'), { ...o, method: 'POST', body }),
  /** Put an existing session into a stack (`host` = the stack's home). */
  addToStack: (host: string, stackId: string, body: StackLinkRequest, o: Opts = {}) =>
    request<StackLinkResponse>(`${hostPath(host)}/stacks/${enc(stackId)}/add`, { ...o, method: 'POST', body }),
  syncStacks: (host: string, o: Opts = {}) =>
    request<{ host?: string; changed: string[]; stacks: StackView[] }>(`${hostPath(host)}/stacks/sync`, { ...o, method: 'POST', body: {} }),
  /** That host's focus session; `setFocus` starts (its default length, or `minutes`) or stops one. */
  focus: (host: string, o: Opts = {}) => request<FocusStatus>(`${hostPath(host)}/focus`, o),
  setFocus: (host: string, on: boolean, minutes?: number, o: Opts = {}) =>
    request<FocusStatus>(`${hostPath(host)}/focus`, { ...o, method: 'POST', body: minutes ? { on, minutes } : { on } }),
  /** The Claude subscription limits of the account logged in on that host (`refresh` skips its 60s cache). */
  usage: (host: string, { refresh = false, ...o }: Opts & { refresh?: boolean } = {}) =>
    request<UsageResponse>(`${hostPath(host)}/usage${refresh ? '?refresh=1' : ''}`, o),

  /** The GitHub PRs that need you, as `gh` on that host sees them (`refresh` skips its cache). */
  prs: (host: string, { refresh = false, ...o }: Opts & { refresh?: boolean } = {}) =>
    request<PrsResponse>(`${hostPath(host)}/prs${refresh ? '?refresh=1' : ''}`, o),

  /** Check a PR (`owner/repo#123`) out in a worktree on that host, or find the one it is in. */
  prCheckout: (host: string, pr: string, o: Opts = {}) =>
    request<{ host: string; worktree: PrWorktree }>(`${hostPath(host)}/prs/checkout`, { ...o, method: 'POST', body: { pr } }),
  /** Mark its @mentions of you so far seen (out of the Unseen mentions column). */
  prSeen: (host: string, pr: string, o: Opts = {}) =>
    request<{ host: string; pr: string; seenAt: number | null }>(`${hostPath(host)}/prs/seen`, { ...o, method: 'POST', body: { pr } }),
  /** Check it out, then start a session in the worktree whose first prompt says what the PR is. */
  prSpawn: (host: string, body: { pr: string; prompt?: string; name?: string; model?: string; harness?: string }, o: Opts = {}) =>
    request<PrSpawnResponse>(`${hostPath(host)}/prs/spawn`, { ...o, method: 'POST', body }),

  /** Your open Productive tasks, as that host's token sees them (`refresh` skips its cache). */
  tasks: (host: string, { refresh = false, ...o }: Opts & { refresh?: boolean } = {}) =>
    request<TasksResponse>(`${hostPath(host)}/tasks${refresh ? '?refresh=1' : ''}`, o),
  /** Move a task to another status (its id or name). */
  taskStatus: (host: string, task: string, status: string, o: Opts = {}) =>
    request<{ host: string; task: Task | null }>(`${hostPath(host)}/tasks/status`, { ...o, method: 'POST', body: { task, status } }),
  /** A new session in `dir` (a spawn dir) whose first prompt says what the task is. */
  taskSpawn: (host: string, body: { task: string; dir: string; prompt?: string; name?: string; model?: string; harness?: string }, o: Opts = {}) =>
    request<TaskSpawnResponse>(`${hostPath(host)}/tasks/spawn`, { ...o, method: 'POST', body }),

  /** Sessions a reboot left dormant on `host` (404 / 501 on servers or CLIs that predate session recovery). */
  dormant: (host: string, o: Opts = {}) => request<DormantResponse>(`${hostPath(host)}/dormant`, o),
  /** Bring one back (`target`: tmux name or session id) or every one (`all`). Starts agents. */
  /** `closed: true`: a recently closed session (never with `all`). */
  restoreDormant: (host: string, body: { target: string; closed?: true } | { all: true }, o: Opts = {}) =>
    request<RestoreResponse>(`${hostPath(host)}/dormant/restore`, { ...o, method: 'POST', body }),
  forgetDormant: (host: string, body: { target: string; closed?: true } | { all: true; closed?: true }, o: Opts = {}) =>
    request<ForgetResponse>(`${hostPath(host)}/dormant/forget`, { ...o, method: 'POST', body }),

  groups: (o: Opts = {}) => request<GroupsResponse>('/api/groups', o),
  runGroups: (o: Opts = {}) => request<GroupsResponse>('/api/groups/run', { ...o, method: 'POST', body: {} }),
  /** Rename a group / move a session (409: the id or label was refused). */
  editGroups: (edit: GroupEdit, o: Opts = {}) => request<GroupsResponse>('/api/groups/edit', { ...o, method: 'POST', body: edit }),
  /** Slack links: the bridge's state, from the `slack.host` server (proxied). */
  slack: (o: Opts = {}) => request<SlackStatus>('/api/slack', o),
  /** Link a session to a Slack thread / channel / DM for `minutes`. */
  slackLink: (body: { host: string; session: string; url: string; minutes: number; task?: string; mode?: 'mention' | 'proactive' }, o: Opts = {}) =>
    request<SlackLink>('/api/slack/link', { ...o, method: 'POST', body }),
  slackUnlink: (id: string, o: Opts = {}) => request<SlackLink>(`/api/slack/links/${enc(id)}/unlink`, { ...o, method: 'POST', body: {} }),
  /** The live inbox: brief + everything waiting on you, from the inbox host (proxied). */
  inbox: (o: Opts = {}) => request<InboxResponse>('/api/inbox', o),
  /** Scan now: the Slack & email scan starts in the background, every other source is re-read. */
  refreshInbox: (o: Opts = {}) => request<InboxResponse>('/api/inbox/refresh', { ...o, method: 'POST', body: {} }),
  /** Done: off the list until its source drops it (messages and sessions: or until newer activity). */
  dismissInbox: (id: string, o: Opts = {}) => request<InboxResponse>('/api/inbox/dismiss', { ...o, method: 'POST', body: { id } }),
  /** Change the scan schedule (saved in the inbox host's config). */
  setInboxSettings: (patch: Partial<InboxSettings>, o: Opts = {}) => request<InboxResponse>('/api/inbox/settings', { ...o, method: 'PUT', body: patch }),
}

/** GET …/files/raw — for <img>, <iframe>, fetch and download links. */
export function fileRawUrl(host: string, id: string, path: string, { download = false } = {}): string {
  return `${sessionPath(host, id, 'files/raw')}?path=${enc(path)}${download ? '&download=1' : ''}`
}

/** GET …/notes/raw — an image inside the notes root. */
export function noteRawUrl(host: string, path: string): string {
  return `${hostPath(host)}/notes/raw?path=${enc(path)}`
}

/** A short, human message for a failed session request (peek/messages/send…). */
export function sessionErrorMessage(err: unknown): string {
  if (!(err instanceof ApiError)) return (err as Error)?.message || 'request failed'
  switch (err.status) {
    case 409:
      return "This session's backend can't be controlled from here"
    case 404:
      return 'Session gone'
    case 502:
    case 504:
      return 'host unreachable'
    case 503:
      return 'session discovery failed on that host'
    case 0:
      return 'network unreachable'
    default:
      return err.message || 'request failed'
  }
}

/**
 * A failed send, for the chat's inline error: a 409 carries the server's reason (waiting on a
 * prompt, no usable handle) when it gave one; anything else is sessionErrorMessage().
 */
export function sendErrorMessage(err: unknown): string {
  if (err instanceof ApiError && err.status === 409 && err.message && !/^HTTP \d+$/.test(err.message)) return err.message
  return sessionErrorMessage(err)
}

/** 404 "unknown session: …" — the session no longer exists (vs. 404 "transcript not found"). */
export function isSessionGone(err: unknown): boolean {
  return err instanceof ApiError && err.status === 404 && /^unknown session/i.test(err.message)
}
