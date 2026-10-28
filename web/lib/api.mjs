// The HTTP JSON API (/api/*), independent of any UI. server.mjs mounts it next to the
// static UI; tests mount it on an ephemeral port with fake dependencies.
// Endpoint reference: ARCHITECTURE.md → "HTTP API".
import os from 'node:os';
import path from 'node:path';
import { Readable } from 'node:stream';
import { HttpError, readJsonBody } from './http.mjs';
import { clampLines, findSession, resolveHost, sleep as defaultSleep, validateKey, validateSendText, validateTitle } from './util.mjs';
import { answerSteps, parseScreenPrompt, questionsText, screenShowsPrompt, validateAnswer } from './questions.mjs';
import { resolveAllowedDir, validateSpawnRequest } from './spawn.mjs';
import {
  fetchPeerHost as defaultFetchPeerHost,
  proxyToPeer as defaultProxyToPeer,
  streamToPeer as defaultStreamToPeer,
  streamFromPeer as defaultStreamFromPeer,
} from './peers.mjs';
import { createSnapshot } from './snapshot.mjs';
import { DISABLED_GROUPS } from './grouping.mjs';
import { checkInboxSettings } from './inbox.mjs';
import { DEFAULT_MODELS, spawnRoots } from './config.mjs';
import { parseNameOutput } from './autoname.mjs';
import { SESSION_ID_RE } from './briefs.mjs';
import { validThreadId } from './reviews.mjs';
import { validItemId } from './queue.mjs';
import { editorUrl, editorViewer, withBriefEditor, withSessionEditors } from './editor.mjs';
import {
  STACK_BODY_LIMIT,
  STACK_ID_RE,
  resolveStackSpawnDir,
  stampForeign,
  validateStackEdit,
  validateStackLabel,
  withStackEditor,
  withStacksEditor,
} from './stacks.mjs';
import { DEFAULT_STACKS_MODEL } from './config.mjs';
import { validateDormantRequest } from './dormant.mjs';
import { validJobName } from './jobs.mjs';
import { MAX_FIRST, MAX_MINUTES, MAX_TASK, MIN_MINUTES, MODES, SlackError, isPersonTarget, parseMinutes, parseSlackUrl, validLinkId } from './slack.mjs';

export const API_VERSION = 1;

const SESSION_ROUTE = /^\/api\/hosts\/([^/]+)\/sessions\/([^/]+)\/(peek|messages|send|keys|answer|kill|rename|autoname)$/;
const POST_ACTIONS = new Set(['send', 'keys', 'answer', 'kill', 'rename', 'autoname']);

const GROUP_ID_RE = /^[A-Za-z0-9._:-]{1,120}$/;
const MEMBER_HOST_RE = /^[^/\s]{1,128}$/;
const MEMBER_ID_RE = /^[A-Za-z0-9._-]{1,128}$/;
const MAX_GROUP_LABEL = 200;

/**
 * The body of POST /api/groups/edit, validated: `{ op: "rename", id, label }`,
 * `{ op: "move", host, session, to }` / `{ op: "move", host, session, label }` (a new group),
 * `{ op: "create", label }` (an empty group) or `{ op: "delete", id }` (an empty group).
 */
export function groupEditOp(body) {
  const b = body && typeof body === 'object' ? body : {};
  const label = () => {
    const l = typeof b.label === 'string' ? b.label.trim() : '';
    if (!l || l.length > MAX_GROUP_LABEL) throw new HttpError(`label must be 1-${MAX_GROUP_LABEL} characters`, 400);
    return l;
  };
  const groupId = (v, what) => {
    if (typeof v !== 'string' || !GROUP_ID_RE.test(v)) throw new HttpError(`invalid ${what}`, 400);
    return v;
  };
  if (b.op === 'rename') return { op: 'rename', id: groupId(b.id, 'group id'), label: label() };
  if (b.op === 'move') {
    if (typeof b.host !== 'string' || !MEMBER_HOST_RE.test(b.host)) throw new HttpError('invalid host', 400);
    if (typeof b.session !== 'string' || !MEMBER_ID_RE.test(b.session)) throw new HttpError('invalid session', 400);
    const op = { op: 'move', host: b.host, session: b.session };
    return b.to != null ? { ...op, to: groupId(b.to, 'target group id') } : { ...op, label: label() };
  }
  if (b.op === 'create') return { op: 'create', label: label() };
  if (b.op === 'delete') return { op: 'delete', id: groupId(b.id, 'group id') };
  throw new HttpError('op must be "rename", "move", "create" or "delete"', 400);
}
const SPAWN_ROUTE = /^\/api\/hosts\/([^/]+)\/spawn$/;
const AUTONAME_ROUTE = /^\/api\/hosts\/([^/]+)\/autoname$/;
const UPLOAD_ROUTE = /^\/api\/hosts\/([^/]+)\/uploads$/;
const FILES_ROUTE = /^\/api\/hosts\/([^/]+)\/sessions\/([^/]+)\/files\/(stat|raw|open)$/;
const COMPLETE_ROUTE = /^\/api\/hosts\/([^/]+)\/sessions\/([^/]+)\/complete$/;
const COMPLETE_KINDS = new Set(['skills', 'files', 'brain']);
const DIFF_ROUTE = /^\/api\/hosts\/([^/]+)\/sessions\/([^/]+)\/(diffs?)$/;
const REVIEW_ROUTE = /^\/api\/hosts\/([^/]+)\/sessions\/([^/]+)\/review(?:\/(submit)|\/(threads)(?:\/([^/]+)(?:\/(reply))?)?)?$/;
/** The prompt queue: …/queue (GET list, POST add), …/queue/pause, …/queue/:item (PUT edit, DELETE), …/queue/:item/send|move. */
const QUEUE_ROUTE = /^\/api\/hosts\/([^/]+)\/sessions\/([^/]+)\/queue(?:\/(pause)|\/([^/]+)(?:\/(send|move))?)?$/;
/** A review comment quotes the selected code: room for a big selection. */
const REVIEW_BODY_LIMIT = 1024 * 1024;
const BRIEF_ROUTE = /^\/api\/hosts\/([^/]+)\/sessions\/([^/]+)\/brief(\/regenerate)?$/;
const MEMO_SEGMENT_ROUTE = /^\/api\/memos\/recordings\/([^/]+)\/segments\/(\d{1,3})$/;
const MEMO_FINISH_ROUTE = /^\/api\/memos\/recordings\/([^/]+)\/finish$/;
const MEMO_ROUTE = /^\/api\/memos\/([^/]+)(\/retry)?$/;
const NOTES_ROUTE = /^\/api\/hosts\/([^/]+)\/notes\/(tree|search|file|raw)$/;
const SPAWN_DIRS_ROUTE = /^\/api\/hosts\/([^/]+)\/spawn-dirs$/;
const STACKS_ROUTE = /^\/api\/hosts\/([^/]+)\/stacks(\/sync)?$/;
const STACK_ROUTE = /^\/api\/hosts\/([^/]+)\/stacks\/([^/]+)(\/spawn|\/rename|\/add)?$/;
const SESSION_STACK_SPAWN_ROUTE = /^\/api\/hosts\/([^/]+)\/sessions\/([^/]+)\/stack\/(spawn|link)$/;
/** A sibling spawn may create a stack first (`stack ensure` → one model call, ≤ 150 s). */
const STACK_SPAWN_PROXY_MS = 180 * 1000;
const REPOS_ROUTE = /^\/api\/hosts\/([^/]+)\/repos(?:\/(sync|service))?$/;
const JOBS_ROUTE = /^\/api\/hosts\/([^/]+)\/jobs(?:\/([^/]+)(?:\/(run|enabled))?)?$/;
/** `fleet jobs run` starts a session (tmux + the agent): give a peer longer than the CLI's 60 s. */
const JOB_RUN_PROXY_MS = 90 * 1000;
const USAGE_ROUTE = /^\/api\/hosts\/([^/]+)\/usage$/;
const PRS_ROUTE = /^\/api\/hosts\/([^/]+)\/prs(?:\/(checkout|spawn|seen))?$/;
/** `owner/repo#123` — what the PR screen sends; the CLI parses it again. */
const PR_REF_RE = /^[A-Za-z0-9_-][A-Za-z0-9._-]{0,99}\/[A-Za-z0-9_-][A-Za-z0-9._-]{0,99}#[1-9][0-9]{0,9}$/;
/** A checkout fetches the PR's head (a big repo, a slow link): the peer gets as long as the CLI. */
const PR_CHECKOUT_PROXY_MS = 5 * 60 * 1000;
/** A PR fetch is four `gh` searches, up to 4 pages each: give a peer as long as its CLI gets. */
const PRS_PROXY_MS = 130 * 1000;
const TASKS_ROUTE = /^\/api\/hosts\/([^/]+)\/tasks(?:\/(status|spawn))?$/;
/** A Productive task: its numeric id. */
const TASK_ID_RE = /^[1-9][0-9]{0,14}$/;
/** Scheduled sessions: GET list, POST add, POST :id/cancel (`fleet schedule`). */
const SCHEDULE_ROUTE = /^\/api\/hosts\/([^/]+)\/schedule(?:\/([a-z0-9]{1,24})\/(cancel))?$/;
/** A `--at` as the UI sends it (`2026-10-29T07:30`, `tomorrow 09:00`, `in 2h`): passed to the CLI as one flag value. */
const SCHEDULE_AT_RE = /^[A-Za-z0-9 :+-]{1,40}$/;
const SCHEDULE_TIME_RE = /^\d{1,2}:\d{2}$/;
/** A Productive task link: `https://app.productive.io/<org>-<slug>/tasks/<id>`. */
const TASK_URL_RE = /^https?:\/\/app\.productive\.io\/\d+-[\w-]+\/tasks?\/([1-9][0-9]{0,14})(?:[/?#].*)?$/;
/** A task fetch is a few API calls; status changes and spawns one more. */
const TASKS_PROXY_MS = 130 * 1000;
const FOCUS_ROUTE = /^\/api\/hosts\/([^/]+)\/focus$/;
/** `focus start` runs Shortcuts and quits apps: a few seconds. */
const FOCUS_PROXY_MS = 45 * 1000;
const HARNESSES_ROUTE = /^\/api\/hosts\/([^/]+)\/harnesses$/;
/** `fleet harness list` runs `pi --list-models` (a node start): keep its answer a while. */
const HARNESSES_TTL_MS = 10 * 60 * 1000;
const SLACK_UNLINK_ROUTE = /^\/api\/slack\/links\/([^/]+)\/unlink$/;
const DORMANT_ROUTE = /^\/api\/hosts\/([^/]+)\/dormant(?:\/(restore|forget))?$/;
/** A restore types launch lines into new tmux panes; `--all` may bring back many sessions. */
const DORMANT_PROXY_MS = 6 * 60 * 1000;
/** Restored sessions register in `fleet list` within seconds: refresh the list and stacks then. */
const DORMANT_SETTLE_MS = [3000, 10000];

export const DEFAULT_QUICK_REPLIES = [
  { label: 'Continue', text: 'Continue.' },
  { label: 'Yes', text: 'Yes' },
  { label: 'No', text: 'No' },
  { label: '1', text: '1' },
  { label: '2', text: '2' },
];

/**
 * @param {object} deps
 *   config      normalized config (lib/config.mjs)
 *   fleet       lib/fleet.mjs instance (local discovery)
 *   backend     lib/backends.mjs instance (peek/send/keys)
 *   transcripts lib/transcript.mjs reader
 *   spawner     lib/spawn.mjs spawner
 *   killer      lib/kill.mjs killer (kill action; absent → 501)
 *   cli         lib/fleet-cli.mjs instance — `rename`, `nameOne`, `usage`, `prs`, `prCheckout`, `tasks`, `taskShow`, `taskStatus`, `schedule`, `scheduleAdd`, `scheduleCancel` (absent → 501)
 *   autoNamer   lib/autoname.mjs instance (autoname route + health; absent → 501)
 *   spawnNamer  lib/autoname.mjs#createSpawnNamer — a targeted naming pass after an unnamed
 *               spawn with a first prompt (only when web.autoName is enabled; absent → none)
 *   uploader    lib/uploads.mjs instance (uploads route; absent → 501)
 *   transcriber lib/transcribe.mjs instance (POST /api/transcribe, settings.transcribe; absent → off)
 *   speaker     lib/transcribe.mjs#createSpeaker (POST /api/speak, settings.speak; absent → off)
 *   memos       lib/memos.mjs#createMemos (/api/memos…, settings.memos; needs the transcriber's key)
 *   operator    lib/operator.mjs instance (GET/POST /api/operator; absent → 501)
 *   files       lib/files.mjs instance (files/stat|raw|open; absent → 501)
 *   completer   lib/complete.mjs instance (…/complete: composer autocomplete; absent → 501)
 *   differ      lib/diff.mjs#createDiffer (…/diff, …/diffs: the diff reviewer; absent → 501)
 *   touched     lib/touched.mjs index — the files a session worked on (…/diffs finds their checkouts)
 *   reviews     lib/reviews.mjs#createReviews (…/review: comment threads sent to the session; absent → 501)
 *   queue       lib/queue.mjs#createPromptQueue (…/queue: prompts that wait for the run to end; absent → 501)
 *   briefs      lib/briefs.mjs instance (brief, brief/regenerate; absent → 501)
 *   notes       lib/notes.mjs instance (notes/tree|search|file|raw; absent → 501: web.notes.root unset)
 *   spawnDirs   lib/spawn-dirs.mjs#createSpawnDirsEditor (GET/PUT spawn-dirs; absent → 501)
 *   repos       lib/repos.mjs#createRepos (GET/PUT repos, POST repos/sync|service; absent → 501)
 *   jobs        lib/jobs.mjs#createJobs (GET jobs[/:name], POST jobs/:name/run|enabled; absent → 501)
 *   stacks      lib/stacks.mjs#createStacks (…/stacks routes, sibling spawn, sync after kill; absent → 501)
 *   focus       lib/focus.mjs instance (/api/hosts/:host/focus; absent → 501)
 *   dormant     lib/dormant.mjs#createDormant (…/dormant routes: list / restore / forget; absent → 501)
 *   grouper     lib/grouping.mjs instance when THIS host runs grouping (absent → proxy to the
 *               grouping host, or a disabled response)
 *   inbox       lib/inbox.mjs instance when THIS host runs the inbox (absent → proxy to
 *               config.inbox.host, or a disabled response)
 *   warmFleet   keep the merged /api/fleet warm in the background (lib/snapshot.mjs)
 *   name, version, startedAt   for /api/health
 *   fetchPeerHost, proxyToPeer, streamToPeer, streamFromPeer (optional, for tests)
 * @returns {(req, url) => Promise<{status, body}>}  throws HttpError / BackendError.
 *   The function carries `.refreshFleet()` and `.stop()` (clears the background refresh).
 */
export function createApi({
  config,
  fleet,
  backend,
  transcripts,
  spawner,
  killer = null,
  cli = null,
  autoNamer = null,
  spawnNamer = null,
  uploader = null,
  transcriber = null,
  memos = null,
  speaker = null,
  operator = null,
  files = null,
  completer = null,
  briefs = null,
  differ = null,
  touched = null,
  reviews = null,
  queue = null,
  notes = null,
  spawnDirs = null,
  repos = null,
  jobs = null,
  stacks = null,
  dormant = null,
  focus = null,
  grouper = null,
  inbox = null,
  slack = null,
  groupingDiscoveryMs = 5 * 60 * 1000,
  name = 'fleet-web',
  version = '0.0.0',
  startedAt = Date.now(),
  fetchPeerHost = defaultFetchPeerHost,
  proxyToPeer = defaultProxyToPeer,
  streamToPeer = defaultStreamToPeer,
  streamFromPeer = defaultStreamFromPeer,
  peerFleetTimeoutMs = 6000,
  peerProxyTimeoutMs = 20000,
  peerUploadTimeoutMs = 15 * 60 * 1000,
  warmFleet = true,
  fleetRefreshMs = 3000,
  fleetIdleAfterMs = 90 * 1000,
  logError = () => {},
  sleep = defaultSleep,
  answerKeyGapMs = 80,
  spawnLinkWaitMs = 45 * 1000,
}) {
  // Each host advertises its start directories: `spawnDirs` (the New session menu) and, when it
  // has any, `hiddenSpawnDirs` (allowed for spawns, e.g. an app's embedded panel, but not offered).
  const spawnDirsInfo = () => ({
    spawnDirs: config.spawnDirs,
    ...(config.hiddenSpawnDirs?.length ? { hiddenSpawnDirs: config.hiddenSpawnDirs } : {}),
  });
  // Each host advertises whether it has a notes explorer (like its spawnDirs).
  const notesInfo = () => (notes ? { notes: { name: notes.name ?? null } } : {});
  // …and its session stacks, compact, so a peer can stamp the rows of their members on its side.
  const stacksInfo = () => (stacks ? { stacks: stacks.compact() } : {});
  // …and marks its voice operator's row (`operator: true`), which the UI keeps out of session lists.
  const stampOperator = (host) => (operator ? operator.stamp(host) : host);

  // `home`: a `~/…` path from the UI (the embed view's `dir`) is matched against the absolute
  // spawnDirs and session cwds.
  async function buildFleet({ force = false } = {}) {
    const selfHost = stampOperator({ ...(await fleet.localHost({ force })), ...spawnDirsInfo(), home: os.homedir(), ...notesInfo(), ...stacksInfo() });
    const peerNames = Object.keys(config.peers);
    const peerHosts = await Promise.all(
      peerNames.map((n) => fetchPeerHost(n, config.peers[n], { timeoutMs: peerFleetTimeoutMs })),
    );
    // Editor links are added per request (handleFleet): they depend on where the browser is.
    // A member of a stack homed on another host gets that stack on its row here.
    return { self: config.self, hosts: stampForeign([selfHost, ...peerHosts]) };
  }

  // The merged fleet, served stale-while-revalidate so a page load never waits on
  // discovery or the slowest peer (see lib/snapshot.mjs).
  const snapshot = warmFleet
    ? createSnapshot({ build: () => buildFleet({ force: true }), refreshMs: fleetRefreshMs, idleAfterMs: fleetIdleAfterMs, logError })
    : null;

  /**
   * After a mutation: drop the local cache and rebuild the merged snapshot behind it.
   * `wait`: resolve once a build that started after this call is done (capped at `waitMs`),
   * so the next /api/fleet already shows the change (a settings save, not a spawn).
   */
  function refreshFleet({ wait = false, waitMs = 3000 } = {}) {
    fleet.invalidate?.();
    const p = snapshot?.refresh({ fresh: wait }).catch(() => {});
    if (!wait || !p) return Promise.resolve();
    return new Promise((resolve) => {
      const timer = setTimeout(resolve, waitMs);
      timer.unref?.();
      p.then(() => (clearTimeout(timer), resolve()));
    });
  }

  async function handleFleet(req, url) {
    // Editor links are this server's to build: its `web.editor` and its ssh aliases, for the
    // machine the browser is on.
    const ec = editorViewer(config, req);
    const withEditors = (body) => ({ ...body, hosts: body.hosts.map((h) => withSessionEditors(h, ec)) });
    if (url.searchParams.get('local') === '1') {
      // Peers poll this: the local host (2s TTL cache), never the merged snapshot.
      const selfHost = stampOperator({ ...(await fleet.localHost()), ...spawnDirsInfo(), home: os.homedir(), ...notesInfo(), ...stacksInfo() });
      return { status: 200, body: withEditors({ self: config.self, hosts: [selfHost] }) };
    }
    if (snapshot) return { status: 200, body: withEditors(await snapshot.get()) };
    return { status: 200, body: withEditors({ ...(await buildFleet()), snapshotAt: Date.now() }) };
  }

  async function resolveLocalSession(id) {
    const host = await fleet.localHost();
    if (!host.ok) throw new HttpError(`session discovery failed: ${host.error}`, 503);
    const session = findSession(host.sessions, id);
    if (!session) throw new HttpError(`unknown session: ${id}`, 404);
    return session;
  }

  async function localSessionAction({ action, id, url, req }) {
    if (action === 'peek') {
      const lines = clampLines(url.searchParams.get('lines'));
      const session = await resolveLocalSession(id);
      const text = await backend.peek(session, lines);
      return {
        status: 200,
        body: { host: config.self, id: session.session_id, backend: session.backend, lines, text, capturedAt: Date.now() },
      };
    }

    if (action === 'messages') {
      const limit = clampLines(url.searchParams.get('limit'), 60, 1, 500);
      const session = await resolveLocalSession(id);
      const result = await transcripts.messages(session, limit);
      if (!result) throw new HttpError('transcript not found for this session', 404);
      const onScreen = await screenQuestion(session, result.messages);
      if (onScreen) result.messages.push(onScreen);
      return {
        status: 200,
        body: {
          host: config.self,
          id: session.session_id,
          status: session.status,
          backend: session.backend,
          name: session.name,
          limit,
          messages: result.messages,
          // The prompts waiting for this run to end (lib/queue.mjs), null when none.
          queue: (await queue?.peek(session).catch(() => null)) ?? null,
          total: result.total,
          truncated: result.truncated,
          updatedAt: result.updatedAt,
          capturedAt: Date.now(),
        },
      };
    }

    const body = await readJsonBody(req);
    if (action === 'kill') {
      if (!killer) throw new HttpError('kill is not available on this server', 501);
      const session = await resolveLocalSession(id);
      const result = await killer.kill(session);
      refreshFleet();
      // The StackBrief marks it closed; best effort (logged by lib/stacks.mjs), never fails the kill.
      stacks?.sync('kill').catch(() => {});
      return {
        status: 200,
        body: { ok: true, host: config.self, id: session.session_id, name: session.name ?? null, ...result },
      };
    }

    if (action === 'rename') {
      if (typeof cli?.rename !== 'function') throw new HttpError('rename is not available on this server', 501);
      const check = validateTitle(body.title);
      if (!check.ok) throw new HttpError(check.error, 400);
      const session = await resolveLocalSession(id);
      // The session id, never a fuzzy name: `fleet rename` types into a live agent.
      const target = session.session_id && !session.session_id.startsWith('pid-') ? session.session_id : String(session.pid);
      let report;
      try {
        report = await cli.rename({ target, title: check.title });
      } catch (err) {
        throw new HttpError(`rename failed: ${err?.message ?? err}`, err?.timedOut ? 504 : 502);
      }
      if (report.result === 'held') {
        // Waiting on a prompt: nothing was typed. 409 so the UI rolls back and says why.
        return { status: 409, body: { host: config.self, id: session.session_id, error: report.message || 'session is waiting on you — nothing sent', ...report } };
      }
      refreshFleet();
      return { status: 200, body: { host: config.self, id: session.session_id, ...report, ok: true } };
    }

    if (action === 'autoname') {
      if (typeof cli?.nameOne !== 'function') throw new HttpError('naming is not available on this server', 501);
      const session = await resolveLocalSession(id);
      const target = session.session_id && !session.session_id.startsWith('pid-') ? session.session_id : String(session.pid);
      let out;
      try {
        out = await cli.nameOne({ target, refresh: true });
      } catch (err) {
        throw new HttpError(`naming failed: ${err?.message ?? err}`, err?.timedOut ? 504 : 502);
      }
      const { renamed, held, errors } = parseNameOutput(`${out.stdout}\n${out.stderr}`);
      const base = { host: config.self, id: session.session_id };
      if (held.length) return { status: 409, body: { ...base, error: `session is waiting on you — nothing sent (${held[0]})` } };
      if (errors.length && !renamed.length) return { status: 502, body: { ...base, error: `naming failed: ${errors[0]}` } };
      refreshFleet();
      return { status: 200, body: { ...base, ok: true, name: renamed[0]?.to ?? null } };
    }

    if (action === 'send') {
      const check = validateSendText(body.text);
      if (!check.ok) throw new HttpError(check.error, 400);
      const session = await resolveLocalSession(id);
      await backend.send(session, check.text);
      return { status: 200, body: { ok: true } };
    }

    if (action === 'answer') return answerQuestion(id, body);

    // keys
    const check = validateKey(body.key);
    if (!check.ok) throw new HttpError(check.error, 400);
    const session = await resolveLocalSession(id);
    await backend.keys(session, check.key);
    return { status: 200, body: { ok: true } };
  }

  /**
   * The AskUserQuestion prompt the terminal shows, as a question message, when the transcript
   * has none open: Claude Code writes the call there only once it is answered. Null otherwise.
   */
  async function screenQuestion(session, messages) {
    if (session.status !== 'waiting' || (session.harness && session.harness !== 'claude')) return null;
    const last = messages.findLast((m) => m.kind === 'question');
    if (last && !last.answers && !last.declined) return null;
    let screen;
    try {
      screen = await backend.peek(session, 80);
    } catch {
      return null;
    }
    const prompt = parseScreenPrompt(screen);
    if (!prompt) return null;
    const { id, questions } = prompt;
    return { role: 'assistant', kind: 'question', id, questions, text: questionsText(questions), ts: Date.now(), final: true };
  }

  /**
   * Answer the session's pending AskUserQuestion prompt by typing what a human would. Only the
   * prompt named by `toolUseId`, only while it is unanswered, the session waits, and the
   * terminal shows it untouched — anything else is a 409 and nothing is typed.
   */
  async function answerQuestion(id, body) {
    const session = await resolveLocalSession(id);
    const toolUseId = typeof body.toolUseId === 'string' ? body.toolUseId : '';
    if (!toolUseId) throw new HttpError('toolUseId is required', 400);
    if (toolUseId.startsWith('screen:')) return answerScreenQuestion(session, toolUseId, body);
    const result = await transcripts.messages(session, 500);
    const prompt = result?.messages.find((m) => m.kind === 'question' && m.id === toolUseId);
    if (!prompt) throw new HttpError('no such question in this session', 404);
    if (prompt.answers || prompt.declined) throw new HttpError('this question has already been answered', 409);
    if (session.status !== 'waiting') throw new HttpError('the session is not waiting on a question', 409);
    const check = validateAnswer(body, prompt.questions);
    if (!check.ok) throw new HttpError(check.error, 400);
    const screen = await backend.peek(session, 80);
    if (!screenShowsPrompt(screen, prompt.questions)) {
      throw new HttpError('the terminal is not showing this question untouched — answer it there', 409);
    }
    for (const step of answerSteps(prompt.questions, check.answers)) {
      if (step.text) await backend.type(session, step.text);
      else await backend.keys(session, step.key);
      await sleep(answerKeyGapMs);
    }
    refreshFleet();
    return { status: 200, body: { ok: true } };
  }

  /** The same for a prompt read off the screen: one tab at a time, each its own `screen:` id. */
  async function answerScreenQuestion(session, toolUseId, body) {
    if (session.status !== 'waiting') throw new HttpError('the session is not waiting on a question', 409);
    const prompt = parseScreenPrompt(await backend.peek(session, 80));
    if (prompt?.id !== toolUseId) throw new HttpError('the terminal is no longer showing this question', 409);
    const check = validateAnswer(body, prompt.questions);
    if (!check.ok) throw new HttpError(check.error, 400);
    if (!prompt.untouched) throw new HttpError('the terminal is not showing this question untouched — answer it there', 409);
    for (const step of answerSteps(prompt.questions, check.answers)) {
      if (step.text) await backend.type(session, step.text);
      else await backend.keys(session, step.key);
      await sleep(answerKeyGapMs);
    }
    refreshFleet();
    return { status: 200, body: { ok: true } };
  }

  /**
   * `slack` on a spawn: `{ url, minutes, mode?, task? }` — the session starts linked. Checked
   * before anything opens; only the bridge's own host links its sessions.
   */
  function spawnSlackRequest(raw, prompt) {
    if (raw == null) return null;
    if (typeof raw !== 'object' || Array.isArray(raw)) throw new HttpError('slack must be { url, minutes, mode?, task? }', 400);
    if (!slack) {
      const host = config.slack?.host;
      throw new HttpError(host && host !== config.self ? `only sessions on ${host} can be linked to Slack` : 'no server runs Slack links (slack.host)', 400);
    }
    const url = typeof raw.url === 'string' ? raw.url.trim() : '';
    if (!url) throw new HttpError('slack.url is required: a Slack thread or channel link, or @person', 400);
    if (!isPersonTarget(url)) {
      try {
        parseSlackUrl(url);
      } catch (err) {
        throw new HttpError(err.message, 400);
      }
    }
    const minutes = parseMinutes(raw.minutes);
    if (minutes == null || minutes < MIN_MINUTES || minutes > MAX_MINUTES) throw new HttpError(`slack.minutes must be ${MIN_MINUTES}–${MAX_MINUTES} min (or like 30m, 2h, 2d)`, 400);
    if (raw.mode != null && !MODES.includes(raw.mode)) throw new HttpError(`slack.mode must be one of: ${MODES.join(', ')}`, 400);
    if (raw.task != null && (typeof raw.task !== 'string' || raw.task.trim().length > MAX_TASK)) throw new HttpError(`slack.task must be a string of at most ${MAX_TASK} chars`, 400);
    if (prompt.trim().length > MAX_FIRST) throw new HttpError(`the prompt is too long for a session linked to Slack (max ${MAX_FIRST} chars)`, 400);
    return { url, minutes, mode: raw.mode ?? undefined, task: raw.task?.trim() || undefined };
  }

  /** Link a just-spawned session once it has a session id; its prompt goes in with the intro. */
  async function linkSpawned(tmuxSession, link, first) {
    const deadline = Date.now() + spawnLinkWaitMs;
    for (;;) {
      try {
        return await slack.link({ ...link, tmuxSession, first, host: config.self });
      } catch (err) {
        // 404: not listed yet, 409: no session id yet.
        if (!(err instanceof SlackError) || ![404, 409].includes(err.status) || Date.now() >= deadline) throw err;
      }
      await sleep(1000);
    }
  }

  async function localSpawn(req) {
    const body = await readJsonBody(req);
    const roots = spawnRoots(config); // hidden spawn dirs are allowed too
    const check = validateSpawnRequest(body, { spawnDirs: config.spawnDirs.map((d) => d.path) });
    if (!check.ok) throw new HttpError(check.error, 400);
    const link = spawnSlackRequest(body?.slack, check.prompt);
    try {
      check.dir = await resolveAllowedDir(check.dir, roots);
      // No name typed → let Claude derive one and the auto-namer replace it (when it runs).
      const nameGiven = check.nameGiven !== false || !config.autoName?.enabled;
      // Linked: it starts with no prompt, so nothing runs before it knows it is in Slack — the
      // prompt goes in with the link's intro, as its first prompt.
      const result = await spawner.spawn({ ...check, ...(link ? { prompt: '' } : {}), nameGiven });
      refreshFleet();
      if (link) {
        try {
          result.slack = await linkSpawned(result.tmuxSession, link, check.prompt);
        } catch (err) {
          const why = err?.message ?? String(err);
          throw new HttpError(`${result.name} started, but linking it to Slack failed: ${why} — its prompt was not sent`, err?.status === 400 ? 400 : 502);
        }
      }
      // Name it as soon as it has answered, not at the next periodic pass. Codex titles its
      // own threads and pi keeps the first prompt; the namer drives Claude's `/rename`.
      if (!nameGiven && check.prompt.trim() && spawnNamer && check.harness === 'claude') {
        spawnNamer.schedule(result.tmuxSession).catch(() => {});
      }
      return { status: 200, body: { ok: true, host: config.self, ...result } };
    } catch (err) {
      if (err?.status) throw new HttpError(err.message, err.status);
      throw err;
    }
  }

  // --- /api/groups: served by the grouping host, proxied from everywhere else ----------
  let discovered = null; // { at, name } — the peer found running grouping (name null: none)

  async function groupsTarget(url) {
    if (grouper) return { kind: 'self' };
    if (url.searchParams.get('local') === '1') return { kind: 'none' }; // never chain
    const host = config.grouping?.host ?? null;
    if (host === config.self) return { kind: 'none' };
    if (host) {
      const peerUrl = config.peers[host];
      if (!peerUrl) return { kind: 'none', error: `grouping.host "${host}" is not a peer with a web URL` };
      return { kind: 'peer', name: host, url: peerUrl };
    }
    // No grouping.host: the first peer whose server runs grouping (cached).
    if (!discovered || Date.now() - discovered.at > groupingDiscoveryMs) {
      let found = null;
      for (const [peer, peerUrl] of Object.entries(config.peers)) {
        const r = await proxyToPeer(peerUrl, { method: 'GET', pathname: '/api/groups', timeoutMs: 3000 });
        if (r.status === 200 && r.body?.enabled === true) {
          found = peer;
          break;
        }
      }
      discovered = { at: Date.now(), name: found };
    }
    return discovered.name ? { kind: 'peer', name: discovered.name, url: config.peers[discovered.name] } : { kind: 'none' };
  }

  async function handleGroups(req, url, action) {
    const target = await groupsTarget(url);
    if (action === 'get') {
      if (target.kind === 'self') return { status: 200, body: grouper.response() };
      if (target.kind === 'peer') {
        const r = await proxyToPeer(target.url, { method: 'GET', pathname: '/api/groups', timeoutMs: peerProxyTimeoutMs });
        if (r.status === 200) return { status: 200, body: r.body };
        discovered = null;
        return { status: 200, body: { ...DISABLED_GROUPS, host: target.name, error: r.body?.error ?? `HTTP ${r.status}` } };
      }
      return { status: 200, body: { ...DISABLED_GROUPS, ...(target.error ? { error: target.error } : {}) } };
    }
    // run
    await readJsonBody(req);
    if (target.kind === 'self') {
      const body = await grouper.runOnce('manual');
      refreshFleet();
      return { status: body.lastRun?.ok === false ? 502 : 200, body };
    }
    if (target.kind === 'peer') {
      const r = await proxyToPeer(target.url, {
        method: 'POST',
        pathname: '/api/groups/run',
        body: '{}',
        timeoutMs: 6 * 60 * 1000,
      });
      return { status: r.status, body: r.body };
    }
    throw new HttpError(target.error ?? 'smart grouping is not enabled on this fleet (web.grouping.enabled)', 501);
  }

  // --- /api/inbox: served by the inbox host (config inbox.host), proxied from everywhere else
  // action: get · refresh (Scan now) · dismiss `{ id }` · settings `{ scanMinutes?, workHours?, workDays? }`
  const INBOX_PATHS = { get: ['GET', '/api/inbox'], refresh: ['POST', '/api/inbox/refresh'], dismiss: ['POST', '/api/inbox/dismiss'], settings: ['PUT', '/api/inbox/settings'] };
  async function handleInbox(req, url, action) {
    const body = action === 'get' ? null : await readJsonBody(req);
    if (inbox) {
      if (action === 'get') return { status: 200, body: inbox.response() };
      if (action === 'refresh') return { status: 200, body: await inbox.refresh() };
      if (action === 'dismiss') {
        if (typeof body?.id !== 'string' || !body.id) throw new HttpError('id is required', 400);
        const r = inbox.dismiss(body.id);
        if (!r) throw new HttpError(`no inbox item ${body.id}`, 404);
        return { status: 200, body: r };
      }
      const checked = checkInboxSettings(body);
      if (checked.error) throw new HttpError(checked.error, 400);
      // The CLI owns the config file: one `fleet config set inbox.<key>` per knob, then live.
      if (typeof cli?.configSet !== 'function') throw new HttpError('config writes are not available on this server', 501);
      try {
        for (const [key, value] of Object.entries(checked.ok)) await cli.configSet({ key: `inbox.${key}`, value, configFile: config.configFile ?? null });
      } catch (err) {
        throw new HttpError(`could not save the inbox settings: ${err?.message ?? err}`, 502);
      }
      return { status: 200, body: inbox.updateSettings(checked.ok) };
    }
    const host = config.inbox?.host ?? null;
    const off = (error) => ({ status: 200, body: { enabled: false, host, ...(error ? { error } : {}), updatedAt: null, brief: null, sources: {}, items: [] } });
    const fail = (error) => (action === 'get' ? off(error) : { status: 409, body: { error: error ?? 'no server runs the inbox (inbox.host)' } });
    if (!host) return fail(null);
    if (host === config.self) return fail('the inbox is off on this server (FLEET_WEB_INBOX)');
    if (url.searchParams.get('local') === '1') return fail(null); // never chain
    const peerUrl = config.peers[host];
    if (!peerUrl) return fail(`inbox.host "${host}" is not a peer with a web URL`);
    const [method, pathname] = INBOX_PATHS[action];
    const r = await proxyToPeer(peerUrl, {
      method,
      pathname,
      ...(body == null ? {} : { body: JSON.stringify(body) }),
      timeoutMs: action === 'refresh' ? 3 * 60 * 1000 : peerProxyTimeoutMs,
    });
    if (action !== 'get' || r.status === 200) return { status: r.status, body: r.body };
    return off(r.body?.error ?? `HTTP ${r.status} from ${host}`);
  }

  /**
   * Slack links (lib/slack.mjs), on the `slack.host` server; the others proxy there. GET status
   * → `{ enabled, host, socket, bot, links, ended }`; POST link `{ host, session | pane, url,
   * minutes, task? }`; POST reply `{ link, text, thread?, pane? }`; POST links/:id/unlink.
   */
  async function handleSlack(req, url, action, id = null) {
    const body = action === 'status' ? null : await readJsonBody(req);
    const host = config.slack?.host ?? null;
    if (slack) {
      try {
        if (action === 'status') return { status: 200, body: { host, ...(await slack.status()) } };
        if (action === 'unlink') {
          if (!validLinkId(id)) throw new HttpError('link must look like sl-xxxxxx', 400);
          return { status: 200, body: await slack.unlink(id) };
        }
        if (action === 'reply') return { status: 200, body: await slack.reply(body ?? {}) };
        const target = typeof body?.host === 'string' && body.host ? body.host : config.self;
        if (target !== config.self) throw new HttpError(`only sessions on ${config.self} can be linked to Slack for now`, 400);
        const link = await slack.link({ ...body, host: config.self });
        refreshFleet();
        return { status: 200, body: link };
      } catch (err) {
        if (err instanceof SlackError) throw new HttpError(err.message, err.status);
        throw err;
      }
    }
    const off = (error) => (action === 'status'
      ? { status: 200, body: { enabled: false, host, ...(error ? { error } : {}), links: [], ended: [] } }
      : { status: 409, body: { error: error ?? 'no server runs Slack links (slack.host)' } });
    if (!host) return off(null);
    if (host === config.self) return off('Slack links are off on this server (FLEET_WEB_SLACK)');
    if (url.searchParams.get('local') === '1') return off(null); // never chain
    const peerUrl = config.peers[host];
    if (!peerUrl) return off(`slack.host "${host}" is not a peer with a web URL`);
    const r = await proxyToPeer(peerUrl, {
      method: action === 'status' ? 'GET' : 'POST',
      pathname: url.pathname,
      ...(body == null ? {} : { body: JSON.stringify(body) }),
      timeoutMs: peerProxyTimeoutMs,
    });
    return { status: r.status, body: r.body };
  }

  /** POST /api/groups/edit — rename a group / move a session, on the grouping host. */
  async function handleGroupEdit(req, url) {
    const op = groupEditOp(await readJsonBody(req));
    const target = await groupsTarget(url);
    if (target.kind === 'self') {
      try {
        return { status: 200, body: await grouper.edit(op) };
      } catch (err) {
        throw new HttpError(`group edit failed: ${err?.message ?? err}`, err?.refused ? 409 : err?.timedOut ? 504 : 502);
      }
    }
    if (target.kind === 'peer') {
      const r = await proxyToPeer(target.url, { method: 'POST', pathname: '/api/groups/edit', body: JSON.stringify(op), timeoutMs: peerProxyTimeoutMs });
      return { status: r.status, body: r.body };
    }
    throw new HttpError(target.error ?? 'smart grouping is not enabled on this fleet (web.grouping.enabled)', 501);
  }

  /** Serve locally when `host` is self, proxy (once, with ?local=1) when it is a peer. */
  async function forHost({ req, url, host, local, stream = false, streamResponse = false, timeoutMs = peerProxyTimeoutMs, bodyLimit }) {
    const target = resolveHost(host, config);
    if (target.kind === 'unknown') throw new HttpError(`unknown host: ${host}`, 404);
    if (target.kind === 'self') return local();
    if (url.searchParams.get('local') === '1') throw new HttpError(`unknown host: ${host}`, 404); // never chain
    if (streamResponse) {
      // files/raw: the peer's body (any type, any size) comes back unbuffered.
      return streamFromPeer(target.url, {
        pathname: url.pathname,
        search: url.search.replace(/^\?/, ''),
        timeoutMs: peerProxyTimeoutMs,
      });
    }
    if (stream) {
      // Uploads: the raw body goes through unbuffered.
      const proxied = await streamToPeer(target.url, {
        method: req.method,
        pathname: url.pathname,
        search: url.search.replace(/^\?/, ''),
        req,
        timeoutMs: peerUploadTimeoutMs,
      });
      return { status: proxied.status, body: proxied.body };
    }
    const rawBody = req.method === 'POST' || req.method === 'PUT' ? JSON.stringify(await readJsonBody(req, bodyLimit)) : null;
    const proxied = await proxyToPeer(target.url, {
      method: req.method,
      pathname: url.pathname,
      search: url.search.replace(/^\?/, ''),
      body: rawBody,
      timeoutMs,
    });
    return { status: proxied.status, body: proxied.body };
  }

  /** This server's own operator: GET status, POST find-or-start. */
  async function localOperator(req) {
    if (!operator) throw new HttpError('the operator is not available on this server', 501);
    if (req.method === 'GET') return { status: 200, body: await operator.status() };
    try {
      const r = await operator.ensure();
      if (r.started) refreshFleet();
      return { status: 200, body: r };
    } catch (err) {
      if (err?.status) throw new HttpError(err.message, err.status);
      throw err;
    }
  }

  async function handleMemos(req, url) {
    if (!memos) throw new HttpError('voice memos are not available on this server', 501);
    const method = req.method;
    if (url.pathname === '/api/memos') {
      if (method !== 'GET') throw new HttpError('method not allowed', 405);
      return { status: 200, body: await memos.list() };
    }
    let m = MEMO_SEGMENT_ROUTE.exec(url.pathname);
    if (m) {
      if (method !== 'PUT') throw new HttpError('method not allowed', 405);
      if (!(await transcriber?.enabled())) throw new HttpError('voice memos are off: no transcription key on this server', 501);
      return { status: 200, body: await memos.putSegment(decodeURIComponent(m[1]), Number(m[2]), req) };
    }
    m = MEMO_FINISH_ROUTE.exec(url.pathname);
    if (m) {
      if (method !== 'POST') throw new HttpError('method not allowed', 405);
      const body = await readJsonBody(req);
      return { status: 200, body: await memos.finish(decodeURIComponent(m[1]), body ?? {}) };
    }
    m = MEMO_ROUTE.exec(url.pathname);
    if (!m) throw new HttpError('not found', 404);
    const id = decodeURIComponent(m[1]);
    if (m[2]) {
      if (method !== 'POST') throw new HttpError('method not allowed', 405);
      return { status: 200, body: await memos.retry(id) };
    }
    if (method === 'GET') return { status: 200, body: await memos.get(id) };
    if (method === 'PUT') return { status: 200, body: await memos.rename(id, (await readJsonBody(req))?.title) };
    if (method === 'DELETE') return { status: 200, body: await memos.remove(id) };
    throw new HttpError('method not allowed', 405);
  }

  async function handleApi(req, url) {
    if (url.pathname === '/api/health') {
      return {
        status: 200,
        body: {
          name,
          version,
          apiVersion: API_VERSION,
          self: config.self,
          uptime: Math.round((Date.now() - startedAt) / 1000),
          now: Date.now(),
          autoName: { ...(config.autoName ?? { enabled: false }), lastRun: autoNamer?.lastRun ?? null },
          grouping: {
            enabled: Boolean(grouper),
            host: grouper ? config.self : (config.grouping?.host ?? null),
            lastRun: grouper?.lastRun ?? null,
          },
          briefs: briefs?.status() ?? { enabled: false },
          inbox: inbox?.status() ?? { enabled: false, host: config.inbox?.host ?? null },
          stacks: stacks?.status() ?? { sync: false, lastSync: null },
        },
      };
    }

    if (url.pathname === '/api/settings') {
      if (req.method !== 'GET') throw new HttpError('method not allowed', 405);
      return {
        status: 200,
        body: {
          apiVersion: API_VERSION,
          self: config.self,
          hosts: config.hosts,
          quickReplies: config.quickReplies ?? DEFAULT_QUICK_REPLIES,
          models: config.models ?? DEFAULT_MODELS,
          uploads: { maxMB: config.uploads?.maxMB ?? null },
          transcribe: { enabled: Boolean(await transcriber?.enabled()) },
          speak: { enabled: Boolean(await speaker?.enabled()), voice: speaker?.voice ?? null },
          memos: { enabled: Boolean(memos && (await transcriber?.enabled())) },
          operator: { enabled: Boolean(operator), hosts: config.operator?.hosts ?? [config.self] },
          notes: notes ? { enabled: true, name: notes.name ?? null } : { enabled: false },
          focus: { host: config.focus?.host ?? null },
          // `generate`: whether creating a stack calls the model (the CLI's `stacks.enabled`).
          stacks: { enabled: Boolean(stacks), model: config.stacks?.model ?? DEFAULT_STACKS_MODEL, generate: config.stacks?.generate !== false },
        },
      };
    }

    // Voice prompts: always this server (the UI's origin), whichever host the session is on.
    if (url.pathname === '/api/transcribe') {
      if (req.method !== 'POST') throw new HttpError('method not allowed', 405);
      if (!transcriber) throw new HttpError('voice input is not available on this server', 501);
      return { status: 200, body: await transcriber.transcribe(req, url.searchParams.get('hint')) };
    }

    // Voice memos: always this server, like /api/transcribe.
    if (url.pathname === '/api/memos' || url.pathname.startsWith('/api/memos/')) return handleMemos(req, url);

    // Spoken replies (the voice operator): always this server, like /api/transcribe.
    if (url.pathname === '/api/speak') {
      if (req.method !== 'POST') throw new HttpError('method not allowed', 405);
      if (!speaker) throw new HttpError('spoken replies are not available on this server', 501);
      const body = await readJsonBody(req);
      if (typeof body?.text !== 'string' || !body.text.trim()) throw new HttpError('text is required', 400);
      const audio = await speaker.speak(body.text);
      return {
        status: 200,
        headers: { 'content-type': 'audio/mpeg', 'content-length': audio.length, 'cache-control': 'no-store' },
        stream: Readable.from([audio]),
      };
    }

    // The voice operator (GET: is it running; POST: find or start it) — on the first host of
    // web.operator.hosts that answers: this server's own, or a peer's (proxied, ?local=1).
    if (url.pathname === '/api/operator') {
      if (req.method !== 'GET' && req.method !== 'POST') throw new HttpError('method not allowed', 405);
      if (url.searchParams.get('local') === '1') return localOperator(req);
      const tried = [];
      for (const h of config.operator?.hosts ?? [config.self]) {
        if (h === config.self) return localOperator(req);
        if (!config.peers[h]) {
          tried.push(`${h}: not a peer with a web url`);
          continue;
        }
        const r = await proxyToPeer(config.peers[h], {
          method: req.method,
          pathname: '/api/operator',
          search: '',
          body: req.method === 'POST' ? '{}' : null,
          timeoutMs: req.method === 'POST' ? 45000 : 5000,
        });
        // Unreachable (or too old to have an operator): the next host takes over.
        if (r.status === 502 || r.status === 504 || r.status === 404 || r.status === 501) {
          tried.push(`${h}: ${r.body?.error ?? `HTTP ${r.status}`}`);
          continue;
        }
        return r;
      }
      throw new HttpError(`no operator host answered (${tried.join('; ')})`, 503);
    }

    if (url.pathname === '/api/fleet') {
      if (req.method !== 'GET') throw new HttpError('method not allowed', 405);
      return handleFleet(req, url);
    }

    if (url.pathname === '/api/groups') {
      if (req.method !== 'GET') throw new HttpError('method not allowed', 405);
      return handleGroups(req, url, 'get');
    }

    if (url.pathname === '/api/inbox') {
      if (req.method !== 'GET') throw new HttpError('method not allowed', 405);
      return handleInbox(req, url, 'get');
    }

    if (url.pathname === '/api/inbox/refresh') {
      if (req.method !== 'POST') throw new HttpError('method not allowed', 405);
      return handleInbox(req, url, 'refresh');
    }

    if (url.pathname === '/api/inbox/dismiss') {
      if (req.method !== 'POST') throw new HttpError('method not allowed', 405);
      return handleInbox(req, url, 'dismiss');
    }

    if (url.pathname === '/api/inbox/settings') {
      if (req.method !== 'PUT') throw new HttpError('method not allowed', 405);
      return handleInbox(req, url, 'settings');
    }

    if (url.pathname === '/api/slack') {
      if (req.method !== 'GET') throw new HttpError('method not allowed', 405);
      return handleSlack(req, url, 'status');
    }

    if (url.pathname === '/api/slack/link' || url.pathname === '/api/slack/reply') {
      if (req.method !== 'POST') throw new HttpError('method not allowed', 405);
      return handleSlack(req, url, url.pathname.slice('/api/slack/'.length));
    }

    const su = SLACK_UNLINK_ROUTE.exec(url.pathname);
    if (su) {
      if (req.method !== 'POST') throw new HttpError('method not allowed', 405);
      return handleSlack(req, url, 'unlink', decodeURIComponent(su[1]));
    }

    if (url.pathname === '/api/groups/edit') {
      if (req.method !== 'POST') throw new HttpError('method not allowed', 405);
      return handleGroupEdit(req, url);
    }

    if (url.pathname === '/api/groups/run') {
      if (req.method !== 'POST') throw new HttpError('method not allowed', 405);
      return handleGroups(req, url, 'run');
    }

    const m = SESSION_ROUTE.exec(url.pathname);
    if (m) {
      const [, rawHost, rawId, action] = m;
      const wantPost = POST_ACTIONS.has(action);
      if (req.method !== (wantPost ? 'POST' : 'GET')) throw new HttpError('method not allowed', 405);
      const id = decodeURIComponent(rawId);
      return forHost({
        req,
        url,
        host: decodeURIComponent(rawHost),
        local: () => localSessionAction({ action, id, url, req }),
      });
    }

    const s = SPAWN_ROUTE.exec(url.pathname);
    if (s) {
      if (req.method !== 'POST') throw new HttpError('method not allowed', 405);
      // A spawn linked to Slack waits for the session to register before it answers.
      const timeoutMs = Math.max(peerProxyTimeoutMs, spawnLinkWaitMs + 30 * 1000);
      return forHost({ req, url, host: decodeURIComponent(s[1]), local: () => localSpawn(req), timeoutMs });
    }

    const a = AUTONAME_ROUTE.exec(url.pathname);
    if (a) {
      if (req.method !== 'POST') throw new HttpError('method not allowed', 405);
      return forHost({
        req,
        url,
        host: decodeURIComponent(a[1]),
        local: async () => {
          await readJsonBody(req);
          if (!autoNamer) throw new HttpError('auto-naming is not available on this server', 501);
          const result = await autoNamer.runOnce('manual');
          refreshFleet();
          return { status: result.ok ? 200 : 502, body: { host: config.self, ...result } };
        },
      });
    }

    const sd = SPAWN_DIRS_ROUTE.exec(url.pathname);
    if (sd) {
      if (req.method !== 'GET' && req.method !== 'PUT') throw new HttpError('method not allowed', 405);
      const r = await forHost({ req, url, host: decodeURIComponent(sd[1]), local: () => localSpawnDirs(req) });
      // A peer saved its list: rebuild our merged snapshot so it advertises the new one.
      if (req.method === 'PUT' && r.status === 200 && r.body?.saved && decodeURIComponent(sd[1]) !== config.self) {
        await refreshFleet({ wait: true });
      }
      return r;
    }

    const rp = REPOS_ROUTE.exec(url.pathname);
    if (rp) {
      const action = rp[2] ?? null;
      const allowed = action ? ['POST'] : ['GET', 'PUT'];
      if (!allowed.includes(req.method)) throw new HttpError('method not allowed', 405);
      // A sync of a stale checkout can take minutes: the peer gets as long as the CLI does.
      const timeoutMs = action === 'sync' ? 16 * 60 * 1000 : peerProxyTimeoutMs;
      return forHost({ req, url, host: decodeURIComponent(rp[1]), timeoutMs, local: () => localRepos(req, action) });
    }

    const jb = JOBS_ROUTE.exec(url.pathname);
    if (jb) {
      let jobName = null;
      try {
        jobName = jb[2] == null ? null : decodeURIComponent(jb[2]);
      } catch {
        throw new HttpError('bad job name', 400);
      }
      const action = jb[3] ?? null;
      if (req.method !== (action ? 'POST' : 'GET')) throw new HttpError('method not allowed', 405);
      if (jobName != null && !validJobName(jobName)) throw new HttpError('bad job name', 400);
      const host = decodeURIComponent(jb[1]);
      const r = await forHost({
        req,
        url,
        host,
        timeoutMs: action === 'run' ? JOB_RUN_PROXY_MS : peerProxyTimeoutMs,
        local: () => localJobs(req, jobName, action),
      });
      // A run started a session: show it in the next /api/fleet without waiting a poll.
      if (action === 'run' && r.status === 200) refreshFleet();
      // Prompt-file editor links are built by the server the browser asked, like the notes'.
      if (!action && r.status === 200 && r.body) {
        const viewer = editorViewer(config, req);
        const withLink = (j) =>
          typeof j?.prompt?.abs === 'string'
            ? {
                ...j,
                prompt: {
                  ...j.prompt,
                  editorUrl: editorUrl(viewer, host, j.prompt.abs, { line: 1 }),
                  rootEditorUrl: editorUrl(viewer, host, j.prompt.root ?? null),
                },
              }
            : j;
        return { ...r, body: jobName == null && Array.isArray(r.body.jobs) ? { ...r.body, jobs: r.body.jobs.map(withLink) } : withLink(r.body) };
      }
      return r;
    }

    const fo = FOCUS_ROUTE.exec(url.pathname);
    if (fo) {
      if (req.method !== 'GET' && req.method !== 'POST') throw new HttpError('method not allowed', 405);
      const host = decodeURIComponent(fo[1]);
      return forHost({ req, url, host, timeoutMs: FOCUS_PROXY_MS, local: () => localFocus(req) });
    }

    const hr = HARNESSES_ROUTE.exec(url.pathname);
    if (hr) {
      if (req.method !== 'GET') throw new HttpError('method not allowed', 405);
      return forHost({ req, url, host: decodeURIComponent(hr[1]), local: () => localHarnesses(url) });
    }

    const us = USAGE_ROUTE.exec(url.pathname);
    if (us) {
      if (req.method !== 'GET') throw new HttpError('method not allowed', 405);
      return forHost({ req, url, host: decodeURIComponent(us[1]), local: () => localUsage(url) });
    }

    const pr = PRS_ROUTE.exec(url.pathname);
    if (pr) {
      const action = pr[2] ?? null;
      if (req.method !== (action ? 'POST' : 'GET')) throw new HttpError('method not allowed', 405);
      const host = decodeURIComponent(pr[1]);
      const r = action
        ? await forHost({ req, url, host, timeoutMs: PR_CHECKOUT_PROXY_MS, local: () => localPrAction(req, action) })
        : await forHost({ req, url, host, timeoutMs: PRS_PROXY_MS, local: () => localPrs(url) });
      return withPrEditors(r, editorViewer(config, req), host);
    }

    const sc = SCHEDULE_ROUTE.exec(url.pathname);
    if (sc) {
      const [, rawHost, id, action] = sc;
      if (!(action ? ['POST'] : ['GET', 'POST']).includes(req.method)) throw new HttpError('method not allowed', 405);
      return forHost({
        req,
        url,
        host: decodeURIComponent(rawHost),
        timeoutMs: TASKS_PROXY_MS,
        local: () => (action ? localScheduleCancel(id) : req.method === 'POST' ? localScheduleAdd(req) : localSchedule()),
      });
    }

    const tk = TASKS_ROUTE.exec(url.pathname);
    if (tk) {
      const action = tk[2] ?? null;
      if (req.method !== (action ? 'POST' : 'GET')) throw new HttpError('method not allowed', 405);
      const host = decodeURIComponent(tk[1]);
      return forHost({ req, url, host, timeoutMs: TASKS_PROXY_MS, local: () => (action ? localTaskAction(req, action) : localTasks(url)) });
    }

    const u = UPLOAD_ROUTE.exec(url.pathname);
    if (u) {
      if (req.method !== 'POST') throw new HttpError('method not allowed', 405);
      return forHost({
        req,
        url,
        host: decodeURIComponent(u[1]),
        stream: true,
        local: async () => {
          if (!uploader) throw new HttpError('uploads are not available on this server', 501);
          const stored = await uploader.store(req, url.searchParams.get('name'));
          return { status: 200, body: { host: config.self, ...stored } };
        },
      });
    }

    const f = FILES_ROUTE.exec(url.pathname);
    if (f) {
      const [, rawHost, rawId, action] = f;
      if (req.method !== (action === 'raw' ? 'GET' : 'POST')) throw new HttpError('method not allowed', 405);
      const id = decodeURIComponent(rawId);
      return forHost({
        req,
        url,
        host: decodeURIComponent(rawHost),
        streamResponse: action === 'raw',
        local: () => localFiles({ action, id, url, req }),
      });
    }

    const c = COMPLETE_ROUTE.exec(url.pathname);
    if (c) {
      const [, rawHost, rawId] = c;
      if (req.method !== 'GET') throw new HttpError('method not allowed', 405);
      const kind = url.searchParams.get('kind') ?? '';
      if (!COMPLETE_KINDS.has(kind)) throw new HttpError('kind must be "skills", "files" or "brain"', 400);
      const id = decodeURIComponent(rawId);
      return forHost({
        req,
        url,
        host: decodeURIComponent(rawHost),
        local: async () => {
          if (!completer) throw new HttpError('autocomplete is not available on this server', 501);
          const session = await resolveLocalSession(id);
          const cwd = typeof session.cwd === 'string' && session.cwd ? session.cwd : null;
          return { status: 200, body: { host: config.self, id: session.session_id, kind, ...(await completer.list(kind, cwd)) } };
        },
      });
    }

    const d = DIFF_ROUTE.exec(url.pathname);
    if (d) {
      if (req.method !== 'GET') throw new HttpError('method not allowed', 405);
      const id = decodeURIComponent(d[2]);
      return forHost({
        req,
        url,
        host: decodeURIComponent(d[1]),
        local: async () => {
          if (!differ) throw new HttpError('the diff reviewer is not available on this server', 501);
          const session = await resolveLocalSession(id);
          const cwd = typeof session.cwd === 'string' && session.cwd ? session.cwd : null;
          if (d[3] === 'diffs') {
            // Every checkout the session works in: its cwd's, then those of the files it worked on.
            const worked = (await touched?.get(session).catch(() => null))?.worked ?? [];
            const roots = await differ.roots(cwd, worked);
            const all = await differ.diffAll(roots, { since: url.searchParams.get('since') });
            const home = os.homedir();
            const label = (root) => {
              const rel = cwd ? path.relative(cwd, root) : '';
              if (cwd && rel && !rel.startsWith('..') && !path.isAbsolute(rel)) return rel;
              return root === home ? '~' : root.startsWith(`${home}/`) ? `~/${root.slice(home.length + 1)}` : root;
            };
            if (all.repos) all.repos = all.repos.map((r) => ({ ...r, label: label(r.root) }));
            return { status: 200, body: { host: config.self, id: session.session_id, cwd, ...all } };
          }
          if (!cwd) throw new HttpError('this session has no working directory', 409);
          try {
            const body = await differ.diff(cwd, { base: url.searchParams.get('base'), since: url.searchParams.get('since') });
            return { status: 200, body: { host: config.self, id: session.session_id, ...body } };
          } catch (err) {
            if (err?.status) throw new HttpError(err.message, err.status);
            throw err;
          }
        },
      });
    }

    const rv = REVIEW_ROUTE.exec(url.pathname);
    if (rv) {
      const [, rawHost, rawId, submit, threads, rawTid, reply] = rv;
      const tid = rawTid == null ? null : decodeURIComponent(rawTid);
      const op = submit ? 'submit' : reply ? 'reply' : tid ? (req.method === 'DELETE' ? 'remove' : 'update') : threads ? 'create' : 'list';
      const method = { list: 'GET', create: 'POST', submit: 'POST', reply: 'POST', remove: 'DELETE', update: 'PUT' }[op];
      if (req.method !== method) throw new HttpError('method not allowed', 405);
      if (tid != null && !validThreadId(tid)) throw new HttpError('invalid thread id', 400);
      const id = decodeURIComponent(rawId);
      return forHost({
        req,
        url,
        host: decodeURIComponent(rawHost),
        bodyLimit: REVIEW_BODY_LIMIT,
        local: async () => {
          if (!reviews) throw new HttpError('review comments are not available on this server', 501);
          const body = method === 'POST' || method === 'PUT' ? await readJsonBody(req, REVIEW_BODY_LIMIT) : null;
          const session = await resolveLocalSession(id);
          if (!session.session_id || !SESSION_ID_RE.test(session.session_id)) throw new HttpError('this session has no session id yet', 409);
          try {
            const r =
              op === 'list'
                ? await reviews.list(session)
                : op === 'create'
                  ? await reviews.create(session, body)
                  : op === 'submit'
                    ? await reviews.submit(session, body)
                    : op === 'reply'
                      ? await reviews.reply(session, tid, body)
                      : op === 'remove'
                        ? await reviews.remove(session, tid)
                        : await reviews.update(session, tid, body);
            return { status: 200, body: { host: config.self, id: session.session_id, ...r } };
          } catch (err) {
            if (err?.status) throw new HttpError(err.message, err.status);
            throw err;
          }
        },
      });
    }

    const qm = QUEUE_ROUTE.exec(url.pathname);
    if (qm) {
      const [, rawHost, rawId, pause, rawItem, sub] = qm;
      const itemId = rawItem == null ? null : decodeURIComponent(rawItem);
      const op = pause ? 'pause' : itemId == null ? (req.method === 'GET' ? 'get' : 'add') : sub ?? (req.method === 'DELETE' ? 'remove' : 'edit');
      const method = { get: 'GET', add: 'POST', pause: 'POST', send: 'POST', move: 'POST', remove: 'DELETE', edit: 'PUT' }[op];
      if (req.method !== method) throw new HttpError('method not allowed', 405);
      if (itemId != null && !validItemId(itemId)) throw new HttpError('invalid queue item id', 400);
      const id = decodeURIComponent(rawId);
      return forHost({
        req,
        url,
        host: decodeURIComponent(rawHost),
        local: async () => {
          if (!queue) throw new HttpError('the prompt queue is not available on this server', 501);
          const body = method === 'POST' || method === 'PUT' ? await readJsonBody(req) : null;
          const session = await resolveLocalSession(id);
          try {
            const r =
              op === 'get'
                ? await queue.get(session)
                : op === 'add'
                  ? await queue.add(session, body)
                  : op === 'pause'
                    ? await queue.pause(session, body)
                    : op === 'send'
                      ? await queue.sendNow(session, itemId)
                      : op === 'move'
                        ? await queue.move(session, itemId, body)
                        : op === 'remove'
                          ? await queue.remove(session, itemId)
                          : await queue.edit(session, itemId, body);
            return { status: 200, body: { host: config.self, id: session.session_id, ...r } };
          } catch (err) {
            if (err?.status) throw new HttpError(err.message, err.status);
            throw err;
          }
        },
      });
    }

    const b = BRIEF_ROUTE.exec(url.pathname);
    if (b) {
      const [, rawHost, rawId, regen] = b;
      const allowed = regen ? ['POST'] : ['GET', 'PUT'];
      if (!allowed.includes(req.method)) throw new HttpError('method not allowed', 405);
      const id = decodeURIComponent(rawId);
      const host = decodeURIComponent(rawHost);
      const r = await forHost({
        req,
        url,
        host,
        local: () => localBrief({ action: regen ? 'regenerate' : req.method, id, req }),
      });
      // The serving host knows only its absolute paths; the editor link (local folder vs
      // Remote-SSH with our alias for that host) is built here, for proxied bodies too.
      if (!regen && r.status === 200) return { ...r, body: withBriefEditor(r.body, host, editorViewer(config, req)) };
      return r;
    }

    const sl = STACKS_ROUTE.exec(url.pathname);
    if (sl) {
      const [, rawHost, sync] = sl;
      if (req.method !== (sync ? 'POST' : 'GET')) throw new HttpError('method not allowed', 405);
      const host = decodeURIComponent(rawHost);
      const r = await forHost({ req, url, host, local: () => localStacks({ action: sync ? 'sync' : 'list', req }) });
      return r.status === 200 ? { ...r, body: withStacksEditor(r.body, host, editorViewer(config, req)) } : r;
    }

    const sk = STACK_ROUTE.exec(url.pathname);
    if (sk) {
      const [, rawHost, rawId, sub] = sk;
      const spawn = sub === '/spawn';
      const allowed = sub ? ['POST'] : ['GET', 'PUT', 'DELETE'];
      if (!allowed.includes(req.method)) throw new HttpError('method not allowed', 405);
      const id = decodeURIComponent(rawId);
      if (!STACK_ID_RE.test(id)) throw new HttpError(`invalid stack id: ${id}`, 400);
      const host = decodeURIComponent(rawHost);
      const action = spawn ? 'spawn' : sub === '/rename' ? 'rename' : sub === '/add' ? 'add' : req.method;
      const r = await forHost({
        req,
        url,
        host,
        timeoutMs: spawn ? STACK_SPAWN_PROXY_MS : peerProxyTimeoutMs,
        bodyLimit: STACK_BODY_LIMIT,
        local: () => (spawn ? localStackSpawn({ req, stackId: id }) : action === 'add' ? localStackLink({ req, stackId: id }) : localStacks({ action, id, req })),
      });
      if (r.status !== 200 || action === 'DELETE') return r;
      if (spawn || action === 'add') return { ...r, body: { ...r.body, stack: withStackEditor(r.body?.stack, host, editorViewer(config, req)) } };
      return { ...r, body: withStackEditor(r.body, host, editorViewer(config, req)) };
    }

    const ssp = SESSION_STACK_SPAWN_ROUTE.exec(url.pathname);
    if (ssp) {
      if (req.method !== 'POST') throw new HttpError('method not allowed', 405);
      const host = decodeURIComponent(ssp[1]);
      const id = decodeURIComponent(ssp[2]);
      const local = ssp[3] === 'link' ? () => localStackLink({ req, sessionId: id }) : () => localStackSpawn({ req, sessionId: id });
      const r = await forHost({ req, url, host, timeoutMs: STACK_SPAWN_PROXY_MS, local });
      return r.status === 200 ? { ...r, body: { ...r.body, stack: withStackEditor(r.body?.stack, host, editorViewer(config, req)) } } : r;
    }

    const dm = DORMANT_ROUTE.exec(url.pathname);
    if (dm) {
      const action = dm[2] ?? 'list';
      if (req.method !== (action === 'list' ? 'GET' : 'POST')) throw new HttpError('method not allowed', 405);
      const host = decodeURIComponent(dm[1]);
      return forHost({ req, url, host, timeoutMs: action === 'restore' ? DORMANT_PROXY_MS : peerProxyTimeoutMs, local: () => localDormant(action, req) });
    }

    const n = NOTES_ROUTE.exec(url.pathname);
    if (n) {
      const [, rawHost, action] = n;
      if (req.method !== 'GET') throw new HttpError('method not allowed', 405);
      const host = decodeURIComponent(rawHost);
      const r = await forHost({ req, url, host, streamResponse: action === 'raw', local: () => localNotes(action, url) });
      // Editor links are built by the server the browser asked (its `web.editor` and ssh aliases).
      if (r.status === 200 && r.body && !r.stream) {
        if (action === 'tree') return { ...r, body: { ...r.body, editorUrl: editorUrl(editorViewer(config, req), host, r.body.rootAbs) } };
        if (action === 'file') return { ...r, body: { ...r.body, editorUrl: editorUrl(editorViewer(config, req), host, r.body.abs) } };
      }
      return r;
    }

    throw new HttpError('not found', 404);
  }

  async function localNotes(action, url) {
    if (!notes) throw new HttpError('notes are not configured on this host (web.notes.root)', 501);
    if (action === 'tree') return { status: 200, body: { host: config.self, ...(await notes.tree()) } };
    if (action === 'search') {
      const limit = clampLines(url.searchParams.get('limit'), 50, 1, 200);
      return { status: 200, body: { host: config.self, ...(await notes.search(url.searchParams.get('q'), { limit })) } };
    }
    if (action === 'file') return { status: 200, body: { host: config.self, ...(await notes.file(url.searchParams.get('path'))) } };
    return notes.raw(url.searchParams.get('path'));
  }

  /**
   * Dormant sessions (lib/dormant.mjs): the CLI's JSON passed through. A restore starts agents,
   * like a spawn: an ambiguous target is a 409 with `candidates`, never a guess.
   */
  async function localDormant(action, req) {
    if (!dormant) throw new HttpError('session recovery is not available on this server', 501);
    try {
      if (action === 'list') return { status: 200, body: await dormant.list() };
      const check = validateDormantRequest(await readJsonBody(req), { allowDryRun: action === 'restore', allowAll: action === 'forget' });
      if (action === 'forget') return { status: 200, body: await dormant.forget(check) };
      const report = await dormant.restore(check);
      const restored = Array.isArray(report?.restored) ? report.restored : [];
      const failed = Array.isArray(report?.failed) ? report.failed : [];
      if (!check.dryRun && restored.length) afterRestore();
      if (!restored.length && failed.length) {
        return { status: 502, body: { error: String(failed[0]?.error ?? 'restore failed'), ...report } };
      }
      return { status: 200, body: report };
    } catch (err) {
      if (err instanceof HttpError && err.body) return { status: err.status, body: err.body };
      throw err;
    }
  }

  /** After a restore: the list now (tmux sessions), again as the agents register, then a stack sync. */
  function afterRestore() {
    refreshFleet();
    DORMANT_SETTLE_MS.forEach((ms, i) => {
      const timer = setTimeout(() => {
        refreshFleet();
        if (i === DORMANT_SETTLE_MS.length - 1) stacks?.sync('restore').catch(() => {});
      }, ms);
      timer.unref?.();
    });
  }

  /**
   * GET harnesses: what this host can spawn (`fleet harness list --json`), cached
   * HARNESSES_TTL_MS (`?refresh=1` skips it). A CLI without `harness` (older fleet) answers
   * Claude alone, with the configured model list, so the New session form still works.
   */
  let harnessCache = null; // { at, rows }
  async function localHarnesses(url) {
    const refresh = url.searchParams.get('refresh') === '1';
    if (!refresh && harnessCache && Date.now() - harnessCache.at < HARNESSES_TTL_MS) {
      return { status: 200, body: { host: config.self, harnesses: harnessCache.rows, ghIdentities: config.ghIdentities ?? [] } };
    }
    let rows;
    try {
      if (typeof cli?.harnesses !== 'function') throw new Error('no harness support');
      rows = await cli.harnesses();
    } catch {
      rows = [{ id: 'claude', label: 'Claude Code', command: config.claude ?? 'claude', installed: true, models: [] }];
    }
    // Claude's picker is the configured one (`web.models`), like before harnesses.
    rows = rows.map((r) => (r.id === 'claude' ? { ...r, models: config.models ?? DEFAULT_MODELS } : r));
    harnessCache = { at: Date.now(), rows };
    return { status: 200, body: { host: config.self, harnesses: rows, ghIdentities: config.ghIdentities ?? [] } };
  }

  /**
   * Editor links for worktrees in a PR answer (each PR's `local.editorUrl`, a checkout's
   * `worktree.editorUrl`), built by the server the browser asked — like the notes and briefs.
   */
  function withPrEditors(r, viewer, host) {
    if (r?.status !== 200 || !r.body || r.stream) return r;
    const body = { ...r.body };
    if (Array.isArray(body.prs)) {
      body.prs = body.prs.map((p) => (p?.local?.worktree ? { ...p, local: { ...p.local, editorUrl: editorUrl(viewer, host, p.local.worktree) } } : p));
    }
    if (body.worktree?.path) body.worktree = { ...body.worktree, editorUrl: editorUrl(viewer, host, body.worktree.path) };
    return { ...r, body };
  }

  /** GET usage: this host's Claude subscription limits (`fleet usage --json`). */
  /** `fleet --local prs --json` on this host (its `gh` login, its checkouts). */
  async function localPrs(url) {
    if (typeof cli?.prs !== 'function') throw new HttpError('pull requests are not available on this server', 501);
    try {
      const prs = await cli.prs({ refresh: url.searchParams.get('refresh') === '1' });
      return { status: 200, body: { host: config.self, ...prs } };
    } catch (err) {
      if (err?.missing) throw new HttpError('this host’s fleet CLI has no `prs` command — update it', 501);
      throw new HttpError(String(err?.message ?? err), err?.timedOut ? 504 : 502);
    }
  }

  /**
   * POST prs/checkout `{ pr }` → `{ host, worktree }`; POST prs/spawn `{ pr, prompt?, name?,
   * model?, harness? }` → `{ host, worktree, spawn }`: the PR's worktree (made when missing),
   * then a new session there whose first prompt is the PR's context line + `prompt`.
   */
  async function localPrAction(req, action) {
    if (typeof cli?.prCheckout !== 'function') throw new HttpError('pull requests are not available on this server', 501);
    const body = await readJsonBody(req);
    const ref = typeof body?.pr === 'string' ? body.pr.trim() : '';
    if (!PR_REF_RE.test(ref)) throw new HttpError('pr must look like owner/repo#123', 400);
    if (action === 'seen') return localPrSeen(ref);
    let check = null;
    if (action === 'spawn') {
      // Validate before anything is fetched; the real dir is the worktree.
      check = validateSpawnRequest({ ...body, dir: '/' }, { spawnDirs: [] });
      if (!check.ok) throw new HttpError(check.error, 400);
    }
    let worktree;
    try {
      worktree = await cli.prCheckout(ref);
    } catch (err) {
      if (err?.missing) throw new HttpError('this host’s fleet CLI has no `pr` command — update it', 501);
      throw new HttpError(String(err?.message ?? err), err?.timedOut ? 504 : 502);
    }
    if (typeof worktree?.path !== 'string' || !path.isAbsolute(worktree.path) || typeof worktree.contextLine !== 'string') {
      throw new HttpError('fleet pr checkout returned no worktree path — update fleet on this host', 502);
    }
    if (action === 'checkout') return { status: 200, body: { host: config.self, worktree } };

    const userPrompt = check.prompt.trim();
    const prompt = userPrompt ? `${worktree.contextLine} ${userPrompt}` : worktree.contextLine;
    const nameGiven = check.nameGiven !== false || !config.autoName?.enabled;
    let result;
    try {
      result = await spawner.spawn({ name: check.name, dir: worktree.path, prompt, nameGiven, model: check.model, harness: check.harness, ghIdentity: check.ghIdentity });
    } catch (err) {
      if (err?.status) throw new HttpError(err.message, err.status);
      throw err;
    }
    refreshFleet();
    if (!nameGiven && spawnNamer && check.harness === 'claude') spawnNamer.schedule(result.tmuxSession).catch(() => {});
    return { status: 200, body: { host: config.self, worktree, spawn: { ok: true, host: config.self, ...result } } };
  }

  /** POST prs/seen `{ pr }` → `{ host, pr, seenAt }`: its @mentions of you so far are seen. */
  async function localPrSeen(ref) {
    if (typeof cli?.prSeen !== 'function') throw new HttpError('pull requests are not available on this server', 501);
    try {
      const r = await cli.prSeen(ref);
      return { status: 200, body: { host: config.self, pr: r?.pr ?? ref, seenAt: r?.seenAt ?? null } };
    } catch (err) {
      if (err?.missing) throw new HttpError('this host’s fleet CLI has no `pr seen` command — update it', 501);
      throw new HttpError(String(err?.message ?? err), err?.timedOut ? 504 : 502);
    }
  }

  /** `fleet --local tasks --json` on this host (its Productive token and organization). */
  async function localTasks(url) {
    if (typeof cli?.tasks !== 'function') throw new HttpError('tasks are not available on this server', 501);
    try {
      const tasks = await cli.tasks({ refresh: url.searchParams.get('refresh') === '1' });
      return { status: 200, body: { host: config.self, ...tasks } };
    } catch (err) {
      if (err?.missing) throw new HttpError('this host’s fleet CLI has no `tasks` command — update it', 501);
      throw new HttpError(String(err?.message ?? err), err?.timedOut ? 504 : 502);
    }
  }

  /**
   * POST tasks/status `{ task, status }` → `{ host, task }`; POST tasks/spawn `{ task, dir,
   * prompt?, name?, model?, harness? }` → `{ host, task, spawn }`: a new session in `dir` (a
   * spawn dir, like a plain spawn) whose first prompt is the task's context line + `prompt`.
   */
  async function localTaskAction(req, action) {
    if (typeof cli?.taskShow !== 'function' || typeof cli?.taskStatus !== 'function') {
      throw new HttpError('tasks are not available on this server', 501);
    }
    const body = await readJsonBody(req);
    const id = taskId(body?.task);
    if (!id) throw new HttpError('task must be a Productive task id or link', 400);
    const cliError = (err) => {
      if (err?.missing) return new HttpError('this host’s fleet CLI has no `task` command — update it', 501);
      return new HttpError(String(err?.message ?? err), err?.timedOut ? 504 : 502);
    };
    if (action === 'status') {
      const status = typeof body?.status === 'string' ? body.status.trim() : '';
      if (!status || status.length > 200) throw new HttpError('status must be a status id or name', 400);
      try {
        const r = await cli.taskStatus(id, status);
        return { status: 200, body: { host: config.self, task: r?.task ?? null } };
      } catch (err) {
        throw cliError(err);
      }
    }

    const roots = spawnRoots(config); // hidden spawn dirs are allowed too
    const check = validateSpawnRequest(body, { spawnDirs: config.spawnDirs.map((d) => d.path) });
    if (!check.ok) throw new HttpError(check.error, 400);
    let task;
    try {
      task = await cli.taskShow(id);
    } catch (err) {
      throw cliError(err);
    }
    if (typeof task?.contextLine !== 'string' || !task.contextLine.trim()) {
      throw new HttpError('fleet task show returned no context line — update fleet on this host', 502);
    }
    const userPrompt = check.prompt.trim();
    const prompt = userPrompt ? `${task.contextLine} ${userPrompt}` : task.contextLine;
    const nameGiven = check.nameGiven !== false || !config.autoName?.enabled;
    let result;
    try {
      const dir = await resolveAllowedDir(check.dir, roots);
      result = await spawner.spawn({ name: check.name, dir, prompt, nameGiven, model: check.model, harness: check.harness, ghIdentity: check.ghIdentity });
    } catch (err) {
      if (err?.status) throw new HttpError(err.message, err.status);
      throw err;
    }
    refreshFleet();
    if (!nameGiven && spawnNamer && check.harness === 'claude') spawnNamer.schedule(result.tmuxSession).catch(() => {});
    return { status: 200, body: { host: config.self, task, spawn: { ok: true, host: config.self, ...result } } };
  }

  function scheduleError(err) {
    if (err?.missing) return new HttpError('this host’s fleet CLI has no `schedule` command — update it', 501);
    return new HttpError(String(err?.message ?? err), err?.timedOut ? 504 : 502);
  }

  /** GET schedule → `{ host, entries }` (pending and recent finished; docs/cli.md → Scheduled sessions). */
  async function localSchedule() {
    if (typeof cli?.schedule !== 'function') throw new HttpError('scheduled sessions are not available on this server', 501);
    try {
      return { status: 200, body: { ...(await cli.schedule()), host: config.self } };
    } catch (err) {
      throw scheduleError(err);
    }
  }

  /**
   * POST schedule `{ at | task (+ time), dir, prompt?, name?, model?, harness?, ghIdentity? }` →
   * `{ host, entry }`: a session started later in `dir` (a spawn dir, like a plain spawn) — at `at`,
   * or on the task's start date at `time`.
   */
  async function localScheduleAdd(req) {
    if (typeof cli?.scheduleAdd !== 'function') throw new HttpError('scheduled sessions are not available on this server', 501);
    const body = await readJsonBody(req);
    const at = typeof body?.at === 'string' ? body.at.trim() : '';
    const task = body?.task != null && body.task !== '' ? taskId(body.task) : null;
    const time = typeof body?.time === 'string' ? body.time.trim() : '';
    if (body?.task != null && body.task !== '' && !task) throw new HttpError('task must be a Productive task id or link', 400);
    if (!at === !task) throw new HttpError('say when: at (a time) or task (its start date), not both', 400);
    if (at && !SCHEDULE_AT_RE.test(at)) throw new HttpError('at must be like 2026-10-29T07:30, tomorrow 09:00 or in 2h', 400);
    if (time && (!task || !SCHEDULE_TIME_RE.test(time))) throw new HttpError('time (HH:MM) goes with a task', 400);
    const check = validateSpawnRequest(body, { spawnDirs: config.spawnDirs.map((d) => d.path) });
    if (!check.ok) throw new HttpError(check.error, 400);
    let dir;
    try {
      dir = await resolveAllowedDir(check.dir, spawnRoots(config));
    } catch (err) {
      if (err?.status) throw new HttpError(err.message, err.status);
      throw err;
    }
    try {
      const entry = await cli.scheduleAdd({
        at: at || null,
        task,
        time: time || null,
        dir,
        prompt: check.prompt.trim(),
        name: check.nameGiven ? check.name : null,
        model: check.model || null,
        harness: check.harness,
        ghIdentity: check.ghIdentity,
      });
      return { status: 200, body: { host: config.self, entry } };
    } catch (err) {
      throw scheduleError(err);
    }
  }

  /** POST schedule/:id/cancel → `{ host, id, output }`. */
  async function localScheduleCancel(id) {
    if (typeof cli?.scheduleCancel !== 'function') throw new HttpError('scheduled sessions are not available on this server', 501);
    try {
      return { status: 200, body: { host: config.self, id, output: await cli.scheduleCancel(id) } };
    } catch (err) {
      throw scheduleError(err);
    }
  }

  /** A task id from `123` or its Productive link; null when it is neither. */
  function taskId(v) {
    const s = typeof v === 'string' || typeof v === 'number' ? String(v).trim() : '';
    if (TASK_ID_RE.test(s)) return s;
    return TASK_URL_RE.exec(s)?.[1] ?? null;
  }

  /** GET → this host's focus status; POST `{ on, minutes? }` starts or stops a session. */
  async function localFocus(req) {
    if (!focus) throw new HttpError('focus mode is not available on this server', 501);
    let on = null;
    let minutes = null;
    if (req.method === 'POST') {
      const body = await readJsonBody(req);
      if (typeof body?.on !== 'boolean') throw new HttpError('on must be true or false', 400);
      on = body.on;
      if (body.minutes != null) {
        minutes = Number(body.minutes);
        if (!Number.isInteger(minutes) || minutes < 1 || minutes > 600) throw new HttpError('minutes must be a whole number from 1 to 600', 400);
      }
    }
    try {
      const status = on === null ? await focus.status() : on ? await focus.start(minutes) : await focus.stop();
      return { status: 200, body: { host: config.self, ...status } };
    } catch (err) {
      if (err?.missing) throw new HttpError('the focus CLI is not installed on this host', 501);
      throw new HttpError(String(err?.message ?? err), err?.timedOut ? 504 : 502);
    }
  }

  async function localUsage(url) {
    if (typeof cli?.usage !== 'function') throw new HttpError('usage is not available on this server', 501);
    try {
      const usage = await cli.usage({ refresh: url.searchParams.get('refresh') === '1' });
      return { status: 200, body: { host: config.self, ...usage } };
    } catch (err) {
      throw new HttpError(String(err?.message ?? err), err?.timedOut ? 504 : 502);
    }
  }

  async function localSpawnDirs(req) {
    if (!spawnDirs) throw new HttpError('editing spawn directories is not available on this server', 501);
    try {
      if (req.method === 'GET') return { status: 200, body: await spawnDirs.get() };
      const r = await spawnDirs.put(await readJsonBody(req));
      if (r.body?.saved) await refreshFleet({ wait: true }); // the next /api/fleet shows the new list
      return r;
    } catch (err) {
      if (err instanceof HttpError) throw err;
      throw new HttpError(err?.message ?? String(err), err?.status ?? 502);
    }
  }

  async function localRepos(req, action) {
    if (!repos) throw new HttpError('git repos are not available on this server', 501);
    try {
      if (action === 'sync') return await repos.sync(await readJsonBody(req));
      if (action === 'service') return await repos.setService(await readJsonBody(req));
      if (req.method === 'GET') return { status: 200, body: await repos.get() };
      return await repos.put(await readJsonBody(req));
    } catch (err) {
      if (err instanceof HttpError) throw err;
      throw new HttpError(err?.message ?? String(err), err?.status ?? 502);
    }
  }

  async function localJobs(req, name, action) {
    if (!jobs) throw new HttpError('jobs are not available on this server', 501);
    if (action === 'run') return jobs.runNow(name, await readJsonBody(req));
    if (action === 'enabled') return jobs.setEnabled(name, await readJsonBody(req));
    return name == null ? jobs.list() : jobs.show(name);
  }

  /** A live session, or — for GET/PUT — a gone one whose brief file is still there (exact id). */
  async function briefSession(id, { allowGone }) {
    try {
      return { session: await resolveLocalSession(id), id: null };
    } catch (err) {
      if (!allowGone || err?.status !== 404 || !SESSION_ID_RE.test(id)) throw err;
      const text = await briefs.store?.read(id).catch(() => null);
      if (text == null) throw err;
      return { session: null, id };
    }
  }

  async function localBrief({ action, id, req }) {
    if (!briefs) throw new HttpError('session briefs are not available on this server', 501);
    const body = action === 'GET' ? null : await readJsonBody(req);
    const found = await briefSession(id, { allowGone: action !== 'regenerate' });
    const sid = found.session?.session_id ?? found.id;
    if (!sid || !SESSION_ID_RE.test(sid)) throw new HttpError('this session has no session id yet', 409);
    try {
      if (action === 'GET') return { status: 200, body: await briefs.get(sid, found.session) };
      if (action === 'PUT') return { status: 200, body: await briefs.put(sid, body.markdown, found.session) };
      const r = await briefs.regenerate(found.session);
      return { status: 202, body: { host: config.self, id: sid, started: r.started, queued: r.queued, generating: r.generating } };
    } catch (err) {
      if (err?.status === 429) return { status: 429, body: { error: err.message, retryAfterMs: err.retryAfterMs ?? null } };
      if (err?.status) throw new HttpError(err.message, err.status);
      throw err;
    }
  }

  // --- session stacks (lib/stacks.mjs): the CLI's JSON passed through ----------------------
  async function localStacks({ action, id = null, req }) {
    if (!stacks) throw new HttpError('session stacks are not available on this server', 501);
    if (action === 'list') return { status: 200, body: await stacks.list() };
    if (action === 'sync') {
      await readJsonBody(req);
      const body = await stacks.syncNow('manual');
      refreshFleet();
      return { status: 200, body };
    }
    if (action === 'GET') return { status: 200, body: await stacks.show(id) };
    if (action === 'DELETE') {
      const body = await stacks.remove(id);
      refreshFleet();
      return { status: 200, body };
    }
    if (action === 'rename') {
      const label = validateStackLabel((await readJsonBody(req))?.label);
      if (!label) throw new HttpError('label must be one non-empty line', 400);
      const body = await stacks.rename(id, label);
      refreshFleet(); // the label shows on session rows
      return { status: 200, body };
    }
    // PUT
    const edit = validateStackEdit(await readJsonBody(req, STACK_BODY_LIMIT));
    try {
      const body = await stacks.set(id, edit.markdown, edit.expectUpdated);
      refreshFleet(); // the label shows on session rows
      return { status: 200, body };
    } catch (err) {
      if (err?.status === 409 && err.body) return { status: 409, body: err.body };
      throw err;
    }
  }

  /**
   * A sibling: from a session (`sessionId`: `stack ensure` first — it may create the stack) or
   * from a stack (`stackId`). Spawns with this server's spawner in the source's directory, the
   * stack's context line prepended to the prompt, then adds the new session to the stack in the
   * background once it shows up in `fleet list` (lib/stacks.mjs#join). With `host` (a peer):
   * the peer's `/spawn` route starts it in `dir` (a path there, required) with the stack's
   * `remoteContextLine`, and the join watches that peer's list; the stack stays on this host.
   */
  async function localStackSpawn({ req, sessionId = null, stackId = null }) {
    if (!stacks) throw new HttpError('session stacks are not available on this server', 501);
    const body = await readJsonBody(req);
    const roots = spawnRoots(config);
    const offered = config.spawnDirs.map((d) => d.path);
    const check = validateSpawnRequest(body, { spawnDirs: offered.length ? offered : ['/'] });
    if (!check.ok) throw new HttpError(check.error, 400);
    const requested = typeof body.dir === 'string' && body.dir.trim() ? check.dir : null;
    const label = validateStackLabel(body.label);
    const spawnHost = typeof body.host === 'string' && body.host.trim() ? body.host.trim() : config.self;
    const remote = spawnHost !== config.self;
    if (remote && !config.peers[spawnHost]) throw new HttpError(`unknown host: ${spawnHost} (not a peer with a web url)`, 400);
    if (remote && !requested) throw new HttpError(`dir is required for a sibling on ${spawnHost}`, 400);

    let stack;
    let created = false;
    let generated = false;
    let dir;
    if (sessionId != null) {
      const source = await resolveLocalSession(sessionId);
      if (!source.session_id || !SESSION_ID_RE.test(source.session_id)) throw new HttpError('this session has no session id yet', 409);
      if (typeof source.cwd !== 'string' || !source.cwd) throw new HttpError('this session has no working directory', 409);
      if (!remote) dir = await resolveStackSpawnDir({ base: source.cwd, requested, roots }); // before any model call
      const out = await stacks.ensure(source.session_id, { label });
      // `ensure --json` is flat: the StackView keys plus `created` (bool: made by this call),
      // `createdAt`, `generated`, `warning` and `stack` (the untouched StackView, whose own
      // `created` is the timestamp). Prefer the nested view; fall back to the flat keys.
      created = Boolean(out?.created);
      generated = Boolean(out?.generated);
      if (out?.stack && typeof out.stack === 'object') stack = out.stack;
      else {
        const { created: _c, generated: _g, createdAt, warning: _w, ...rest } = out ?? {};
        stack = createdAt != null ? { ...rest, created: createdAt } : rest;
      }
    } else {
      stack = await stacks.show(stackId);
      if (!remote) dir = await resolveStackSpawnDir({ base: stack?.absCwd ?? stack?.cwd, requested, roots });
    }
    if (!stack?.id || typeof stack.contextLine !== 'string') throw new HttpError('fleet stack returned no stack id / contextLine — update fleet on this host', 502);
    if (remote && typeof stack.remoteContextLine !== 'string') throw new HttpError('fleet stack returned no remoteContextLine — update fleet on this host', 502);

    // Exactly `core::stack::stack_prompt`: the context line, a space, the trimmed prompt.
    const userPrompt = check.prompt.trim();
    const line = remote ? stack.remoteContextLine : stack.contextLine;
    const prompt = userPrompt ? `${line} ${userPrompt}` : line;
    if (remote) {
      // The peer validates `dir` against its own spawn dirs and names / auto-names the session.
      const proxied = await proxyToPeer(config.peers[spawnHost], {
        method: 'POST',
        pathname: `/api/hosts/${encodeURIComponent(spawnHost)}/spawn`,
        body: JSON.stringify({
          dir: requested,
          prompt,
          ...(check.nameGiven ? { name: check.name } : {}),
          ...(check.model ? { model: check.model } : {}),
          harness: check.harness,
          ...(check.ghIdentity ? { ghIdentity: check.ghIdentity } : {}),
        }),
        timeoutMs: peerProxyTimeoutMs,
      });
      if (proxied.status !== 200) {
        const why = proxied.body?.error ?? `HTTP ${proxied.status}`;
        throw new HttpError(`${spawnHost}: ${why}`, proxied.status >= 500 ? 502 : proxied.status);
      }
      refreshFleet();
      if (typeof proxied.body?.tmuxSession === 'string') stacks.join(proxied.body.tmuxSession, stack.id, spawnHost);
      return {
        status: 200,
        body: { host: config.self, stack, created: Boolean(created), generated: Boolean(generated), spawn: { ok: true, ...proxied.body, host: spawnHost } },
      };
    }
    const nameGiven = check.nameGiven !== false || !config.autoName?.enabled;
    let result;
    try {
      result = await spawner.spawn({ name: check.name, dir, prompt, nameGiven, model: check.model, harness: check.harness, ghIdentity: check.ghIdentity });
    } catch (err) {
      if (err?.status) throw new HttpError(err.message, err.status);
      throw err;
    }
    refreshFleet();
    if (!nameGiven && check.prompt.trim() && spawnNamer && check.harness === 'claude') spawnNamer.schedule(result.tmuxSession).catch(() => {});
    stacks.join(result.tmuxSession, stack.id);
    return {
      status: 200,
      body: { host: config.self, stack, created: Boolean(created), generated: Boolean(generated), spawn: { ok: true, host: config.self, ...result } },
    };
  }

  /**
   * Put an existing session into a stack after the fact. Body `{ session, host? }`: the session to
   * add (a full session id; `host`: the peer it runs on, default this one). Into a stack
   * (`stackId`, …/stacks/:id/add), or around a session of this host (`sessionId`,
   * …/sessions/:id/stack/link: `stack ensure` first — it may create the stack, `label` naming it).
   */
  async function localStackLink({ req, sessionId = null, stackId = null }) {
    if (!stacks) throw new HttpError('session stacks are not available on this server', 501);
    const body = await readJsonBody(req);
    const other = typeof body?.session === 'string' ? body.session.trim() : '';
    if (!SESSION_ID_RE.test(other)) throw new HttpError('session must be a session id', 400);
    const onHost = typeof body.host === 'string' && body.host.trim() ? body.host.trim() : config.self;
    if (onHost !== config.self && !config.peers[onHost]) throw new HttpError(`unknown host: ${onHost} (not a peer with a web url)`, 400);
    const label = validateStackLabel(body.label);
    let id = stackId;
    let created = false;
    let generated = false;
    if (sessionId != null) {
      const source = await resolveLocalSession(sessionId);
      if (!source.session_id || !SESSION_ID_RE.test(source.session_id)) throw new HttpError('this session has no session id yet', 409);
      if (source.session_id === other && onHost === config.self) throw new HttpError('a session cannot be linked to itself', 400);
      const out = await stacks.ensure(source.session_id, { label });
      created = Boolean(out?.created);
      generated = Boolean(out?.generated);
      id = out?.stack?.id ?? out?.id;
      if (!id) throw new HttpError('fleet stack returned no stack id — update fleet on this host', 502);
    }
    const stack = await stacks.add(id, other, onHost === config.self ? undefined : onHost);
    refreshFleet(); // the stack shows on both rows
    return { status: 200, body: { host: config.self, stack, created, generated } };
  }

  async function localFiles({ action, id, url, req }) {
    if (!files) throw new HttpError('file preview is not available on this server', 501);
    const body = action === 'raw' ? null : await readJsonBody(req);
    const session = await resolveLocalSession(id);
    const cwd = typeof session.cwd === 'string' && session.cwd ? session.cwd : null;
    if (action === 'stat') return { status: 200, body: { host: config.self, id: session.session_id, ...(await files.stat(body.paths, cwd, { session })) } };
    if (action === 'open') return { status: 200, body: { host: config.self, ...(await files.open(body.path, cwd, { session })) } };
    const download = ['1', 'true'].includes(url.searchParams.get('download') ?? '');
    return files.raw(url.searchParams.get('path'), cwd, { download, session });
  }

  handleApi.refreshFleet = refreshFleet;
  /** A fresh merged fleet, built once — what the grouper feeds `fleet group`. Doesn't warm the snapshot. */
  handleApi.buildFleet = () => buildFleet({ force: true });
  /** The warm snapshot if someone is watching, else null — for cheap change checks. */
  handleApi.peekFleet = () => snapshot?.peek() ?? null;
  handleApi.stop = () => snapshot?.stop();
  return handleApi;
}
