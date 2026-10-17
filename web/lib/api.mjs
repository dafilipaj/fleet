// The HTTP JSON API (/api/*), independent of any UI. server.mjs mounts it next to the
// static UI; tests mount it on an ephemeral port with fake dependencies.
// Endpoint reference: ARCHITECTURE.md → "HTTP API".
import path from 'node:path';
import { Readable } from 'node:stream';
import { HttpError, readJsonBody } from './http.mjs';
import { clampLines, findSession, resolveHost, validateKey, validateSendText, validateTitle } from './util.mjs';
import { targetOf } from './fleet-cli.mjs';
import { resolveAllowedDir, validateSpawnRequest } from './spawn.mjs';
import {
  fetchPeerHost as defaultFetchPeerHost,
  proxyToPeer as defaultProxyToPeer,
  streamToPeer as defaultStreamToPeer,
  streamFromPeer as defaultStreamFromPeer,
} from './peers.mjs';
import { createSnapshot } from './snapshot.mjs';
import { DISABLED_GROUPS } from './grouping.mjs';
import { DEFAULT_MODELS } from './config.mjs';
import { SESSION_ID_RE } from './briefs.mjs';
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

export const API_VERSION = 1;

const SESSION_ROUTE = /^\/api\/hosts\/([^/]+)\/sessions\/([^/]+)\/(peek|messages|send|keys|answer|kill|rename)$/;
const POST_ACTIONS = new Set(['send', 'keys', 'answer', 'kill', 'rename']);

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
const BRIEF_ROUTE = /^\/api\/hosts\/([^/]+)\/sessions\/([^/]+)\/brief(\/regenerate)?$/;
const MEMO_SEGMENT_ROUTE = /^\/api\/memos\/recordings\/([^/]+)\/segments\/(\d{1,3})$/;
const MEMO_FINISH_ROUTE = /^\/api\/memos\/recordings\/([^/]+)\/finish$/;
const MEMO_ROUTE = /^\/api\/memos\/([^/]+)(\/retry)?$/;
const NOTES_ROUTE = /^\/api\/hosts\/([^/]+)\/notes\/(tree|search|file|raw)$/;
const SPAWN_DIRS_ROUTE = /^\/api\/hosts\/([^/]+)\/spawn-dirs$/;
const STACKS_ROUTE = /^\/api\/hosts\/([^/]+)\/stacks(\/sync)?$/;
const STACK_ROUTE = /^\/api\/hosts\/([^/]+)\/stacks\/([^/]+)(\/spawn|\/rename)?$/;
const SESSION_STACK_SPAWN_ROUTE = /^\/api\/hosts\/([^/]+)\/sessions\/([^/]+)\/stack\/spawn$/;
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
const HARNESSES_ROUTE = /^\/api\/hosts\/([^/]+)\/harnesses$/;
/** `fleet harness list` runs `pi --list-models` (a node start): keep its answer a while. */
const HARNESSES_TTL_MS = 10 * 60 * 1000;
const DORMANT_ROUTE = /^\/api\/hosts\/([^/]+)\/dormant(?:\/(restore|forget))?$/;
/** A restore types launch lines into new tmux panes; `--all` may bring back many sessions. */
const DORMANT_PROXY_MS = 6 * 60 * 1000;
/**
 * A typing action on a peer: longer than its CLI may take there (lib/fleet-cli.mjs: answer 60 s,
 * send 30 s, keys and rename 20 s) plus the busy retry, so the proxy never gives up (504) while the
 * peer still types. Waiting behind the peer's other requests into the same session can add more.
 */
const TYPING_PROXY_MS = { answer: 75 * 1000, send: 45 * 1000, keys: 30 * 1000, rename: 30 * 1000 };
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
 *   backend     lib/backends.mjs instance (peek: reading the screen; typing goes through `cli`)
 *   transcripts lib/transcript.mjs reader
 *   spawner     lib/spawn.mjs spawner
 *   killer      lib/kill.mjs killer (kill action; absent → 501)
 *   cli         lib/fleet-cli.mjs instance — `send`, `keys`, `answer`, `rename`, `usage`, `prs`, `prCheckout` (absent → 501)
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
 *   briefs      lib/briefs.mjs instance (brief, brief/regenerate; absent → 501)
 *   notes       lib/notes.mjs instance (notes/tree|search|file|raw; absent → 501: web.notes.root unset)
 *   spawnDirs   lib/spawn-dirs.mjs#createSpawnDirsEditor (GET/PUT spawn-dirs; absent → 501)
 *   repos       lib/repos.mjs#createRepos (GET/PUT repos, POST repos/sync|service; absent → 501)
 *   jobs        lib/jobs.mjs#createJobs (GET jobs[/:name], POST jobs/:name/run|enabled; absent → 501)
 *   stacks      lib/stacks.mjs#createStacks (…/stacks routes, sibling spawn, sync after kill; absent → 501)
 *   dormant     lib/dormant.mjs#createDormant (…/dormant routes: list / restore / forget; absent → 501)
 *   grouper     lib/grouping.mjs instance when THIS host runs grouping (absent → proxy to the
 *               grouping host, or a disabled response)
 *   warmFleet   keep the merged /api/fleet warm in the background (lib/snapshot.mjs)
 *   name, version, startedAt   for /api/health
 *   fetchPeerHost, proxyToPeer, streamToPeer, streamFromPeer (optional, for tests)
 * @returns {(req, url) => Promise<{status, body}>}  throws HttpError / BackendError.
 *   The function carries `.refreshFleet()` and `.stop()` (clears the background refresh).
 */
/** The HTTP status of a typing command's failure, by the CLI's `kind`; anything else is 502. */
const TYPING_STATUS = {
  usage: 400,
  answer_invalid: 400,
  invalid_key: 400,
  target_invalid: 400,
  target_not_found: 404,
  question_not_found: 404,
  transcript_not_found: 404,
  busy: 409,
  human_attached: 409,
  target_ambiguous: 409,
  not_waiting: 409,
  question_answered: 409,
  question_not_active: 409,
  prompt_not_on_screen: 409,
  prompt_incomplete: 409,
  prompt_changed: 409,
  backend_unsupported: 409,
  harness_unsupported: 409,
};

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
  notes = null,
  spawnDirs = null,
  repos = null,
  jobs = null,
  stacks = null,
  dormant = null,
  grouper = null,
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
  typingBusyRetryMs = 2000,
  peerUploadTimeoutMs = 15 * 60 * 1000,
  warmFleet = true,
  fleetRefreshMs = 3000,
  fleetIdleAfterMs = 90 * 1000,
  logError = () => {},
}) {
  // Each host advertises whether it has a notes explorer (like its spawnDirs).
  const notesInfo = () => (notes ? { notes: { name: notes.name ?? null } } : {});
  // …and its session stacks, compact, so a peer can stamp the rows of their members on its side.
  const stacksInfo = () => (stacks ? { stacks: stacks.compact() } : {});
  // …and marks its voice operator's row (`operator: true`), which the UI keeps out of session lists.
  const stampOperator = (host) => (operator ? operator.stamp(host) : host);

  async function buildFleet({ force = false } = {}) {
    const selfHost = stampOperator({ ...(await fleet.localHost({ force })), spawnDirs: config.spawnDirs, ...notesInfo(), ...stacksInfo() });
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
      const selfHost = stampOperator({ ...(await fleet.localHost()), spawnDirs: config.spawnDirs, ...notesInfo(), ...stacksInfo() });
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
      // The exact target, never a fuzzy name: `fleet rename` types into a live agent.
      let report;
      try {
        report = await oneAtATime(session, () => cli.rename({ target: targetOf(session), title: check.title }));
      } catch (err) {
        if (err?.kind && !err.timedOut) {
          // `busy` and the other refusals the CLI names: their status, as for send and keys.
          const error = `rename failed: ${err.message}`;
          return { status: TYPING_STATUS[err.kind] ?? 502, body: { host: config.self, id: session.session_id, ...err.report, ok: false, kind: err.kind, error } };
        }
        throw new HttpError(`rename failed: ${err?.message ?? err}`, err?.timedOut ? 504 : 502);
      }
      if (report.result === 'held') {
        // Waiting on a prompt: nothing was typed. 409 so the UI rolls back and says why.
        return { status: 409, body: { host: config.self, id: session.session_id, error: report.message || 'session is waiting on you — nothing sent', ...report } };
      }
      refreshFleet();
      return { status: 200, body: { host: config.self, id: session.session_id, ...report, ok: true } };
    }

    if (action === 'send') {
      const check = validateSendText(body.text);
      if (!check.ok) throw new HttpError(check.error, 400);
      const session = await resolveLocalSession(id);
      return typeThroughCli('send', session, () => cli.send({ target: targetOf(session), text: check.text }), { retryBusy: true });
    }

    if (action === 'answer') return answerQuestion(id, body);

    // keys
    const check = validateKey(body.key);
    if (!check.ok) throw new HttpError(check.error, 400);
    const session = await resolveLocalSession(id);
    return typeThroughCli('keys', session, () => cli.keys({ target: targetOf(session), keys: [check.key] }), { retryBusy: true });
  }

  /**
   * Run `fn` after every earlier typing request of this server into the same session has
   * finished: the CLI's lock refuses a second command at once (`busy`), so requests arriving
   * together (the outbox's keepalive sends on `pagehide`, quick key taps) would otherwise
   * refuse each other, and a refused message would be lost.
   */
  const typingQueues = new Map();
  function oneAtATime(session, fn) {
    const key = targetOf(session);
    const run = (typingQueues.get(key) ?? Promise.resolve()).then(fn);
    const tail = run.then(
      () => {},
      () => {},
    );
    typingQueues.set(key, tail);
    tail.then(() => {
      if (typingQueues.get(key) === tail) typingQueues.delete(key);
    });
    return run;
  }

  /**
   * Type into a session through the CLI (lib/fleet-cli.mjs), never straight into tmux: the CLI
   * holds the session's typing lock — the one every fleet command takes — so these keys never
   * interleave with another command's (an agent's `fleet answer`, a rename). This server's own
   * requests into one session go one at a time ({@link oneAtATime}). With `retryBusy` (send,
   * keys — never an answer, which checks the screen it saw), `busy` from a command outside this
   * server is retried for up to `typingBusyRetryMs`. A failure the CLI reports keeps its `kind`
   * (`busy`: another command is still typing there; nothing was typed — the UI shows it failed).
   */
  async function typeThroughCli(what, session, call, { retryBusy = false } = {}) {
    if (typeof cli?.[what] !== 'function') throw new HttpError(`${what} is not available on this server`, 501);
    let report;
    try {
      report = await oneAtATime(session, async () => {
        const until = Date.now() + (retryBusy ? typingBusyRetryMs : 0);
        for (let wait = 100; ; wait = Math.min(wait * 2, 500)) {
          try {
            return await call();
          } catch (err) {
            if (err?.kind !== 'busy' || Date.now() + wait > until) throw err;
            await new Promise((r) => setTimeout(r, wait));
          }
        }
      });
    } catch (err) {
      if (err?.timedOut) throw new HttpError(`${what} failed: ${err.message}`, 504);
      if (!err?.kind) throw new HttpError(`${what} failed: ${err?.message ?? err}`, 502);
      return { status: TYPING_STATUS[err.kind] ?? 502, body: { ...err.report, ok: false, kind: err.kind, error: err.message } };
    }
    return { status: 200, body: { ...report, ok: true } };
  }

  /**
   * Answer the session's pending AskUserQuestion prompt by typing what a human would, through
   * `fleet answer --question` (docs/cli.md → Answering prompts): only the prompt named by
   * `toolUseId`, only while it is unanswered, the session waits and the terminal shows it
   * untouched, every key checked on screen — anything else types nothing (409, or 400/404), except
   * `prompt_changed` (409), `io` and `outcome_uncertain` (502), which may come after keys went out.
   */
  async function answerQuestion(id, body) {
    const session = await resolveLocalSession(id);
    const toolUseId = typeof body.toolUseId === 'string' ? body.toolUseId : '';
    if (!toolUseId) throw new HttpError('toolUseId is required', 400);
    const result = await typeThroughCli('answer', session, () => cli.answer({ target: targetOf(session), question: toolUseId, answers: body.answers }));
    refreshFleet();
    return result;
  }

  async function localSpawn(req) {
    const body = await readJsonBody(req);
    const roots = config.spawnDirs.map((d) => d.path);
    const check = validateSpawnRequest(body, { spawnDirs: roots });
    if (!check.ok) throw new HttpError(check.error, 400);
    try {
      check.dir = await resolveAllowedDir(check.dir, roots);
      // No name typed → let Claude derive one and the auto-namer replace it (when it runs).
      const nameGiven = check.nameGiven !== false || !config.autoName?.enabled;
      const result = await spawner.spawn({ ...check, nameGiven });
      refreshFleet();
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
        timeoutMs: TYPING_PROXY_MS[action] ?? peerProxyTimeoutMs,
        local: () => localSessionAction({ action, id, url, req }),
      });
    }

    const s = SPAWN_ROUTE.exec(url.pathname);
    if (s) {
      if (req.method !== 'POST') throw new HttpError('method not allowed', 405);
      return forHost({ req, url, host: decodeURIComponent(s[1]), local: () => localSpawn(req) });
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
      const action = spawn ? 'spawn' : sub === '/rename' ? 'rename' : req.method;
      const r = await forHost({
        req,
        url,
        host,
        timeoutMs: spawn ? STACK_SPAWN_PROXY_MS : peerProxyTimeoutMs,
        bodyLimit: STACK_BODY_LIMIT,
        local: () => (spawn ? localStackSpawn({ req, stackId: id }) : localStacks({ action, id, req })),
      });
      if (r.status !== 200 || action === 'DELETE') return r;
      if (spawn) return { ...r, body: { ...r.body, stack: withStackEditor(r.body?.stack, host, editorViewer(config, req)) } };
      return { ...r, body: withStackEditor(r.body, host, editorViewer(config, req)) };
    }

    const ssp = SESSION_STACK_SPAWN_ROUTE.exec(url.pathname);
    if (ssp) {
      if (req.method !== 'POST') throw new HttpError('method not allowed', 405);
      const host = decodeURIComponent(ssp[1]);
      const id = decodeURIComponent(ssp[2]);
      const r = await forHost({ req, url, host, timeoutMs: STACK_SPAWN_PROXY_MS, local: () => localStackSpawn({ req, sessionId: id }) });
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
      return { status: 200, body: { host: config.self, harnesses: harnessCache.rows } };
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
    return { status: 200, body: { host: config.self, harnesses: rows } };
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
      result = await spawner.spawn({ name: check.name, dir: worktree.path, prompt, nameGiven, model: check.model, harness: check.harness });
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
    const roots = config.spawnDirs.map((d) => d.path);
    const check = validateSpawnRequest(body, { spawnDirs: roots.length ? roots : ['/'] });
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
      result = await spawner.spawn({ name: check.name, dir, prompt, nameGiven, model: check.model, harness: check.harness });
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
