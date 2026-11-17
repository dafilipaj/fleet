#!/usr/bin/env node
// fleet-web — zero-dependency server for the fleet HTTP API + the bundled PWA.
// Config: the shared fleet config ($FLEET_CONFIG or ~/.config/fleet/config.json).
// See ARCHITECTURE.md.
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { DEFAULT_VOICE_FAILED_DIR, configPath, createSlackSettingsReloader, expandHome, fleetStateDir, loadConfig, ensurePath, ensureUtf8Locale, resolveBinary } from './lib/config.mjs';
import { createPrStates } from './lib/pr-state.mjs';
import { run } from './lib/run.mjs';
import { createFleetCli } from './lib/fleet-cli.mjs';
import { createFocus } from './lib/focus.mjs';
import { createFleet } from './lib/fleet.mjs';
import { createBackend } from './lib/backends.mjs';
import { createTranscriptReader, locateTranscript } from './lib/transcript.mjs';
import { createSpawner } from './lib/spawn.mjs';
import { createKiller } from './lib/kill.mjs';
import { createAutoNamer, createSpawnNamer } from './lib/autoname.mjs';
import { createTitleStore } from './lib/titles.mjs';
import { createGrouper } from './lib/grouping.mjs';
import { createUploader } from './lib/uploads.mjs';
import { SPEECH_KEY_NAME, createKeySource, createSpeaker, createTranscriber } from './lib/transcribe.mjs';
import { createOperator } from './lib/operator.mjs';
import { createCompleter } from './lib/complete.mjs';
import { createDiffer } from './lib/diff.mjs';
import { createReviewPrs } from './lib/review-prs.mjs';
import { createReviews } from './lib/reviews.mjs';
import { createAsks } from './lib/asks.mjs';
import { createPrNotes } from './lib/pr-notes.mjs';
import { createFiles } from './lib/files.mjs';
import { createNotes } from './lib/notes.mjs';
import { createSpawnDirsEditor } from './lib/spawn-dirs.mjs';
import { createRepos } from './lib/repos.mjs';
import { createJobs } from './lib/jobs.mjs';
import { createTouchedIndex } from './lib/touched.mjs';
import { createBriefExtractor } from './lib/brief-extract.mjs';
import { createBeads } from './lib/beads.mjs';
import { createBriefStore, createBriefs, createClaudeAsk, gitInfo } from './lib/briefs.mjs';
import { createStacks } from './lib/stacks.mjs';
import { createAgentAsk, createInbox } from './lib/inbox.mjs';
import { SLACK_USER_TOKEN_NAME } from './lib/inbox-slack.mjs';
import { createPromptQueue } from './lib/queue.mjs';
import { TOKEN_NAMES, createSlackApi, createSlackBridge, createSlackDownload, createSocketClient } from './lib/slack.mjs';
import { createGithubBridge } from './lib/github.mjs';
import { fetchPeerHost, proxyToPeer, setPeerConnects, setPeerProxies, setPeerTokens } from './lib/peers.mjs';
import { createDormant } from './lib/dormant.mjs';
import { createMemos } from './lib/memos.mjs';
import { createApi } from './lib/api.mjs';
import { createHttpServer } from './lib/app.mjs';
import { createRequestGuard, listenAddresses, loopbackFor, machineNames } from './lib/guard.mjs';
import { createAuth, createKeychainSecret, MIN_PASSWORD_LENGTH, MIN_TOKEN_LENGTH } from './lib/auth.mjs';

const ROOT = path.dirname(fileURLToPath(import.meta.url));
const NAME = 'fleet-web';
const VERSION = JSON.parse(fs.readFileSync(path.join(ROOT, 'package.json'), 'utf8')).version;

const log = (line) => process.stdout.write(`${line}\n`);
const logError = (err, context = '') =>
  process.stderr.write(`[error]${context ? ` ${context}` : ''} ${err?.stack ?? String(err)}\n`);

ensurePath(process.env);
ensureUtf8Locale(process.env);

let config;
try {
  config = loadConfig({ env: process.env, webRoot: ROOT, log: console });
} catch (err) {
  process.stderr.write(`[fleet-web] ${err.message}\n`);
  process.exit(2);
}

setPeerProxies(config.peerProxies);
setPeerConnects(config.peerConnects);

const cli = createFleetCli({ run, bin: config.fleetBin });
const fleet = createFleet({ cli, self: config.self, ttlMs: 2000 });
const backend = createBackend({ run, tmux: config.tmux });
const autoNamer = createAutoNamer({
  cli,
  intervalMs: config.autoName.intervalMinutes * 60 * 1000,
  log,
});
// After an unnamed spawn with a first prompt: name that one session once it has replied.
const spawnNamer = config.autoName.enabled
  ? createSpawnNamer({
      cli,
      listSessions: async () => {
        const host = await fleet.localHost({ force: true });
        return host.ok ? host.sessions : [];
      },
      log,
    })
  : null;
const uploader = createUploader({
  dir: config.uploads.dir,
  maxBytes: config.uploads.maxMB * 1024 * 1024,
  retentionDays: config.uploads.retentionDays,
  log,
});
// Only the grouping host runs the pass; the fleet accessors are bound once the API exists.
let fleetAccess = null;
const grouper = config.grouping.enabled
  ? createGrouper({
      cli,
      getFleet: () => fleetAccess.buildFleet(),
      peekFleet: () => fleetAccess.peekFleet(),
      self: config.self,
      intervalMs: config.grouping.intervalMinutes * 60 * 1000,
      log,
    })
  : null;
const ghBin = resolveBinary('gh');
const claudeBin = resolveBinary('claude', { extraDirs: [path.join(os.homedir(), '.local', 'bin'), path.join(os.homedir(), '.claude', 'local')] });
// Session briefs: GET/PUT always work; background generation only with web.briefs.enabled.
// What a session made, read from its transcript: the briefs' resources and the session's beads.
const briefExtractor = createBriefExtractor({ locate: (cwd, id) => locateTranscript(cwd, id) });
const briefs = createBriefs({
  settings: config.briefs,
  self: config.self,
  store: createBriefStore({ dir: config.briefs.dir }),
  extractor: briefExtractor,
  listSessions: async () => {
    const host = await fleet.localHost();
    return host.ok ? host.sessions : null;
  },
  ask: createClaudeAsk({ bin: claudeBin, run }),
  git: (cwd) => gitInfo(cwd, { run }),
  prStates: createPrStates({ gh: (args, opts) => run(ghBin, args, opts) }),
  titles: createTitleStore(),
  log,
});
// Session stacks: the routes always work (the CLI owns the files); the background membership
// sync runs every web.stacks.syncMinutes while a session is in a stack (FLEET_WEB_STACKS=0: off).
const stacks = createStacks({
  cli,
  self: config.self,
  listSessions: async () => {
    const host = await fleet.localHost({ force: true });
    if (!host.ok) throw new Error(host.error);
    return host.sessions;
  },
  gateSessions: async () => {
    const host = await fleet.localHost();
    return host.ok ? host.sessions : null;
  },
  syncEnabled: config.stacks.sync,
  syncIntervalMs: config.stacks.syncMinutes * 60 * 1000,
  log,
});
// Inbox: the live to-do list, run by the `inbox.host` server only (the others proxy to it).
// Its Slack poller reads Slack as the user (FLEET_INBOX_SLACK_TOKEN, env or Keychain) — only when
// the token is there at start; without it the connector scan reads Slack too.
const inboxSlackToken = config.inbox.enabled ? await createKeySource({ name: SLACK_USER_TOKEN_NAME, run })() : null;
const inboxSlackApi = inboxSlackToken ? createSlackApi({ getToken: async () => inboxSlackToken }) : null;
const inbox = config.inbox.enabled
  ? createInbox({
      settings: config.inbox,
      cli,
      getFleet: async () => fleetAccess.peekFleet() ?? (await fleetAccess.buildFleet()),
      // The brief: no connectors, no tools — through the JSON runner so its tokens are counted.
      ask: ((agent) => ({ prompt, model }) => agent({ prompt, model, mcp: false }))(createAgentAsk({ bin: claudeBin, run, timeoutMs: 2 * 60 * 1000 })),
      agent: createAgentAsk({ bin: claudeBin, run }),
      slackApi: inboxSlackApi,
      file: path.join(fleetStateDir(process.env), 'inbox.json'),
      log,
    })
  : null;
// Slack links: the bridge runs on the `slack.host` server only (the others proxy /api/slack there).
let slack = null;
if (config.slack.enabled) {
  const getBotToken = createKeySource({ name: TOKEN_NAMES.bot, run });
  const call = createSlackApi({ getToken: getBotToken });
  const socket = createSocketClient({
    getAppToken: createKeySource({ name: TOKEN_NAMES.app, run }),
    onEvent: (event) => slack.handleEvent(event),
    log,
  });
  slack = createSlackBridge({
    settings: config.slack,
    call,
    socket,
    self: config.self,
    // A link's session may run on any host: a peer is asked through its web server.
    listSessions: async (host) => {
      if (!host || host === config.self) {
        const local = await fleet.localHost({ force: true });
        return local.ok ? local.sessions : null;
      }
      const peerUrl = config.peers[host];
      if (!peerUrl) return null;
      const h = await fetchPeerHost(host, peerUrl, { timeoutMs: 8000 });
      return h.ok ? h.sessions : null;
    },
    send: async (session, text, host) => {
      if (!host || host === config.self) return backend.send(session, text);
      const peerUrl = config.peers[host];
      if (!peerUrl) throw new Error(`${host} is not a peer with a web url`);
      const r = await proxyToPeer(peerUrl, {
        method: 'POST',
        pathname: `/api/hosts/${encodeURIComponent(host)}/sessions/${encodeURIComponent(session.session_id)}/send`,
        body: JSON.stringify({ text }),
        timeoutMs: 15000,
      });
      if (r.status !== 200) throw new Error(r.body?.error ?? `HTTP ${r.status} from ${host}`);
    },
    // slack.spawn: a mention opens a session on `host` — this server, else that peer's (a spawn
    // linked to Slack answers once the session is linked, ≤ ~1 min).
    spawn: async (host, body) => {
      const local = !host || host === config.self;
      const loopback = loopbackFor(config.bind);
      const base = local ? `http://${loopback.includes(':') ? `[${loopback}]` : loopback}:${config.port}` : config.peers[host];
      if (!base) throw new Error(`${host} is not a peer with a web url`);
      const r = await proxyToPeer(base, {
        method: 'POST',
        pathname: `/api/hosts/${encodeURIComponent(local ? config.self : host)}/spawn`,
        body: JSON.stringify(body),
        timeoutMs: 120000,
      });
      if (r.status !== 200) throw new Error(r.body?.error ?? `HTTP ${r.status} from ${host}`);
      return r.body;
    },
    file: path.join(fleetStateDir(process.env), 'slack-links.json'),
    download: createSlackDownload({ getToken: getBotToken }),
    // The owner's DM about a blocked session links to it in the web UI of the session's host.
    sessionUrl: (host, id) => {
      const base = host === config.self ? config.selfWeb : config.peers[host];
      return base ? `${base}/#/s/${encodeURIComponent(host)}/${encodeURIComponent(id)}` : null;
    },
    log,
  });
}
// GitHub links: the bridge runs on the `github.host` server only, as this host's `gh` account.
// It starts and resumes sessions on this host, through its own POST spawn and the fleet CLI.
const github = config.github.enabled
  ? createGithubBridge({
      settings: config.github,
      self: config.self,
      gh: (args, opts) => run(ghBin, args, opts),
      listSessions: async () => {
        const local = await fleet.localHost({ force: true });
        return local.ok ? local.sessions : null;
      },
      send: (session, text) => backend.send(session, text),
      spawn: async (body) => {
        const loopback = loopbackFor(config.bind);
        const base = `http://${loopback.includes(':') ? `[${loopback}]` : loopback}:${config.port}`;
        const r = await proxyToPeer(base, {
          method: 'POST',
          pathname: `/api/hosts/${encodeURIComponent(config.self)}/spawn`,
          body: JSON.stringify(body),
          timeoutMs: 120000,
        });
        if (r.status !== 200) throw new Error(r.body?.error ?? `HTTP ${r.status} from spawn`);
        return r.body;
      },
      sleep: (sessionId) => cli.sleep({ target: sessionId }),
      restore: (args) => cli.restore(args),
      file: path.join(fleetStateDir(process.env), 'github-links.json'),
      log,
    })
  : null;
// Prompt queue: messages typed while a session works go in one per run, once it is idle again.
const queue = createPromptQueue({
  file: path.join(fleetStateDir(process.env), 'prompt-queue.json'),
  listSessions: async () => {
    const host = await fleet.localHost({ force: true });
    return host.ok ? host.sessions : null;
  },
  send: (session, text) => backend.send(session, text),
  log,
});
// Session recovery: dormant sessions after a reboot (`fleet --local restore … --json`).
const dormant = createDormant({ cli, log });
// Voice: an OpenAI key for speech-to-text (voice prompts) and one for text-to-speech (the
// operator's replies) — OPENAI_TEXT_TO_SPEECH_API_KEY, else the transcription key.
const transcribeKey = createKeySource({ run });
const ttsKey = createKeySource({ name: SPEECH_KEY_NAME, run });
const speechKey = async () => (await ttsKey()) ?? (await transcribeKey());
const transcriber = createTranscriber({ getKey: transcribeKey, keepDir: expandHome(DEFAULT_VOICE_FAILED_DIR), log });
// Voice memos: segments transcribed with the same key, titled by `claude -p`.
const memos = createMemos({ ...config.memos, transcriber, ask: createClaudeAsk({ bin: claudeBin, run }), log });
const spawner = createSpawner({
  run,
  tmux: config.tmux,
  launcher: config.claude,
  launchers: config.launchers,
  ghIdentities: config.ghIdentities,
  ghIdentityLauncher: (name, launcher) => cli.ghIdentityLauncher(name, launcher),
  harnessDefaults: () => config.harnessDefaults,
});
const operator = createOperator({ config, fleet, spawner, backend, log });
// One transcript reader: the chat and the review threads (their replies) share its cache.
const transcripts = createTranscriptReader();
// Files each session worked on: file links resolve against them, the diff reviewer finds the checkouts it edits.
const touched = createTouchedIndex();

const differ = createDiffer({ run });
const handleApi = createApi({
  config,
  fleet,
  backend,
  transcripts,
  spawner,
  killer: createKiller({ run, tmux: config.tmux, closeIterm: (s) => backend.closeIterm(s) }),
  cli,
  autoNamer,
  spawnNamer,
  uploader,
  focus: createFocus({ run }),
  transcriber,
  memos,
  speaker: createSpeaker({ getKey: speechKey, voice: config.operator.voice ?? undefined, log }),
  operator,
  files: createFiles({ home: os.homedir(), run, touched, roots: config.files.roots }),
  touched,
  completer: createCompleter({ home: os.homedir(), run }),
  differ,
  // The diff reviewer's entries: each checkout's repo + PR, the brief's PRs without a worktree.
  reviewPrs: createReviewPrs({ gh: (args, opts) => run(ghBin, args, opts), differ, clones: () => cli.repos({ all: false }) }),
  // Diff reviewer side questions: a read-only `claude -p` fork of the session, kept in memory only.
  asks: createAsks({ bin: claudeBin }),
  // PR review notes (the reviewer on a PR without a session); posting one runs this host's gh.
  prNotes: createPrNotes({ dir: path.join(fleetStateDir(process.env), 'pr-notes'), gh: (args, opts) => run(ghBin, args, opts) }),
  // Diff reviewer threads: typed into the session like a send, answers read from its transcript.
  reviews: createReviews({
    dir: path.join(fleetStateDir(process.env), 'reviews'),
    send: (session, text) => backend.send(session, text),
    messages: async (session) => (await transcripts.messages(session, 500))?.messages ?? null,
  }),
  notes: createNotes({ config: config.notes, home: os.homedir(), run }),
  // Beads (bd) issues a session works on: on when this host has `bd`.
  beads: (() => {
    const bdBin = resolveBinary('bd');
    return path.isAbsolute(bdBin) ? createBeads({ bd: (args, opts) => run(bdBin, args, opts), extractor: briefExtractor, settings: config.beads }) : null;
  })(),
  // Settings → Start directories: writes through `fleet config set`, hot-reloads config.spawnDirs.
  spawnDirs: createSpawnDirsEditor({ config, configFile: config.configFile ?? configPath(process.env), cli }),
  // Settings → Git repos: `fleet --local repos …` + `fleet config set repos`.
  repos: createRepos({ cli, configFile: config.configFile ?? configPath(process.env), self: config.self }),
  // Jobs screen: `fleet --local jobs …` (list / show / run now / enable|disable).
  jobs: createJobs({ cli, self: config.self }),
  briefs,
  stacks,
  dormant,
  grouper,
  inbox,
  slack,
  github,
  queue,
  name: NAME,
  version: VERSION,
  logError,
});

fleetAccess = handleApi;

// A browser can reach this server by these names (lib/guard.mjs); anything else is refused.
const guard = createRequestGuard({
  names: [
    ...config.hosts,
    ...Object.values(config.hostAddrs),
    ...[config.selfWeb, ...Object.values(config.peers)].filter(Boolean).map((u) => new URL(u).hostname.replace(/^\[|\]$/g, '')),
    ...machineNames(),
  ],
  allowedHosts: config.allowedHosts,
});

// web.bind: one address, or a specific one (a tailnet IP) plus loopback (lib/guard.mjs).
const addresses = listenAddresses(config.bind);
// web.auth: a token on every request, loopback included (lib/auth.mjs); off unless configured. The
// token and the phone password live in the Keychain only.
const auth = createAuth({
  enabled: Boolean(config.authKeychain),
  getToken: createKeychainSecret({ service: config.authKeychain, run, minLength: MIN_TOKEN_LENGTH, log }),
  getPassword: createKeychainSecret({ service: config.authPasswordKeychain, run, minLength: MIN_PASSWORD_LENGTH, log }),
  log,
});
// Peers whose web.auth wants a token (hosts.<name>.webTokenKeychain): read now and, while one is
// missing (a locked Keychain), again every 30 s.
const peerTokenSources = Object.entries(config.peerTokenKeychains).map(([origin, service]) => [origin, createKeychainSecret({ service, run, minLength: MIN_TOKEN_LENGTH, log })]);
async function loadPeerTokens() {
  const entries = await Promise.all(peerTokenSources.map(async ([origin, get]) => [origin, await get()]));
  setPeerTokens(Object.fromEntries(entries.filter(([, t]) => t)));
  if (entries.some(([, t]) => !t)) setTimeout(loadPeerTokens, 30 * 1000).unref();
}
if (peerTokenSources.length) loadPeerTokens().catch((err) => logError(err, 'peer tokens'));
const servers = addresses.map(() => createHttpServer({ handleApi, uiDir: config.uiDir, guard, auth, log, logError }));
for (const s of servers) {
  s.on('error', (err) => {
    // A tailnet address that is not up yet (EADDRNOTAVAIL): exit, launchd starts us again.
    logError(err, 'server');
    process.exit(1);
  });
}
for (const [i, s] of servers.entries()) if (i > 0) s.listen(config.port, addresses[i]);

servers[0].listen(config.port, addresses[0], () => {
  log(`[fleet-web] self=${config.self} listening on ${addresses.map((a) => `http://${a.includes(':') ? `[${a}]` : a}:${config.port}`).join(', ')}`);
  if (config.allowedHosts.length) log(`[fleet-web] allowedHosts=${config.allowedHosts.join(',')}`);
  log(`[fleet-web] auth ${auth.enabled ? `on (Keychain ${config.authKeychain}${config.authPasswordKeychain ? `, password ${config.authPasswordKeychain}` : ''})` : 'off'}`);
  log(`[fleet-web] config=${config.configFile ?? '(none, defaults)'} peers=${Object.keys(config.peers).join(',') || '(none)'}`);
  log(`[fleet-web] fleet=${config.fleetBin} tmux=${config.tmux} ui=${config.uiDir ?? '(none)'}`);
  if (config.autoName.enabled) {
    autoNamer.start();
    log(`[fleet-web] autoname every ${config.autoName.intervalMinutes}m (fleet name --all --apply)`);
  } else {
    log('[fleet-web] autoname off (web.autoName.enabled = false)');
  }
  uploader.start();
  // A running operator keeps its workspace current (its ./api helper names this server's port).
  operator.prepare().catch((err) => logError(err, 'operator workspace'));
  operator.start();
  if (config.operator.clearIdleMinutes) log(`[fleet-web] operator: /clear after ${config.operator.clearIdleMinutes}m idle`);
  log(`[fleet-web] memos ${config.memos.dir} (audio ${config.memos.audioDir})`);
  log(`[fleet-web] uploads ${config.uploads.dir} (max ${config.uploads.maxMB} MB, kept ${config.uploads.retentionDays || '∞'} days)`);
  if (grouper) {
    grouper.start();
    log(`[fleet-web] grouping every ${config.grouping.intervalMinutes}m (fleet group, all hosts)`);
  } else if (config.grouping.host && config.grouping.host !== config.self) {
    log(`[fleet-web] grouping: served by ${config.grouping.host}`);
  } else {
    log('[fleet-web] grouping off (web.grouping.enabled = false)');
  }
  if (config.briefs.enabled) {
    briefs.start();
    const b = config.briefs;
    log(`[fleet-web] briefs on (${b.dir}; ${b.model}, idle ${b.idleMs / 1000}s, every ≥ ${b.minIntervalMs / 60000}m per session, ≤ ${b.maxCallsPerHour}/h)`);
  } else {
    log(`[fleet-web] briefs: background generation off (web.briefs.enabled = false); ${config.briefs.dir}`);
  }
  if (inbox) {
    inbox.start();
    const ib = config.inbox;
    log(`[fleet-web] inbox on (${inboxSlackApi ? `Slack poller every ${ib.slackMinutes}m, ${ib.slackModel}; ` : ''}${ib.comms ? `${inboxSlackApi ? 'mail' : 'Slack + mail'} every ${ib.scanMinutes}m in work hours, ${ib.model}` : 'no connector scan'}; brief ${ib.briefModel}, ≥ ${ib.briefMinutes}m apart)`);
  } else if (config.inbox.host) {
    log(`[fleet-web] inbox: served by ${config.inbox.host}`);
  }
  queue.start();
  if (slack || github) {
    // slack.owner / slack.spawn / github.spawn: live from the config file (`fleet config set`), no restart.
    const reloadSlack = createSlackSettingsReloader({ config, log });
    setInterval(() => {
      if (reloadSlack()) log(`[fleet-web] settings reloaded: slack.owner=${config.slack.owner ?? '(none)'}`);
    }, 5000).unref();
  }
  if (github) {
    github.start();
    const from = config.github.spawn?.from ?? [];
    log(`[fleet-web] github links on (notifications of this host's gh; review requests start sessions: ${config.github.spawn ? (from.length ? from.join(', ') : 'nobody') : 'off'})`);
  } else if (config.github.host) {
    log(`[fleet-web] github links: served by ${config.github.host}`);
  }
  if (slack) {
    slack.start();
    log(`[fleet-web] slack links on (Socket Mode; ${TOKEN_NAMES.bot} + ${TOKEN_NAMES.app} from env or Keychain)`);
  } else if (config.slack.host) {
    log(`[fleet-web] slack links: served by ${config.slack.host}`);
  }
  // The sync judges members on other hosts with the merged fleet (the warm snapshot when someone
  // is watching, else one fresh build); a sibling started on a peer is found in that peer's list.
  stacks.attach({
    fleetBody: async () => handleApi.peekFleet() ?? (await handleApi.buildFleet()),
    peerSessions: async (name) => {
      const url = config.peers[name];
      if (!url) return null;
      const h = await fetchPeerHost(name, url, { timeoutMs: 6000 });
      return h.ok ? h.sessions : null;
    },
  });
  stacks.start();
  log(config.stacks.sync
    ? `[fleet-web] stacks: sync every ${config.stacks.syncMinutes}m while a session is in a stack (fleet stack sync)`
    : '[fleet-web] stacks: background sync off (FLEET_WEB_STACKS=0)');
  if (config.restore.onBoot) {
    log('[fleet-web] restore.onBoot: bringing back dormant sessions (fleet restore --all)');
    dormant.restoreOnBoot({ markerFile: config.restore.markerFile }).then((r) => {
      if (r?.restored?.length) handleApi.refreshFleet();
    });
  }
});

let shuttingDown = false;
function shutdown(signal) {
  if (shuttingDown) return;
  shuttingDown = true;
  log(`[fleet-web] ${signal} received, shutting down`);
  autoNamer.stop();
  spawnNamer?.stop();
  grouper?.stop();
  briefs.stop();
  inbox?.stop();
  queue.stop();
  slack?.stop();
  github?.stop();
  stacks.stop();
  operator.stop();
  uploader.stop();
  handleApi.stop();
  const timer = setTimeout(() => process.exit(0), 3000);
  timer.unref?.();
  let open = servers.length;
  for (const s of servers) {
    s.close(() => {
      if (--open > 0) return;
      log('[fleet-web] closed');
      process.exit(0);
    });
    s.closeIdleConnections?.();
  }
}

process.on('SIGTERM', () => shutdown('SIGTERM'));
process.on('SIGINT', () => shutdown('SIGINT'));
process.on('uncaughtException', (err) => logError(err, 'uncaughtException'));
process.on('unhandledRejection', (err) => logError(err, 'unhandledRejection'));
