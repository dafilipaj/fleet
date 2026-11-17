// Session stacks on the web side: lib/fleet-cli.mjs stack calls, lib/stacks.mjs (errors, sync,
// the post-spawn join) and the /api/hosts/:host/stacks… routes. The `fleet` CLI and the spawner
// are faked; the peer pair runs on 127.0.0.1 ephemeral ports.
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { PassThrough } from 'node:stream';

import { createApi } from '../lib/api.mjs';
import { createHttpServer } from '../lib/app.mjs';
import { createFleet } from '../lib/fleet.mjs';
import { createFleetCli } from '../lib/fleet-cli.mjs';
import { normalizeConfig } from '../lib/config.mjs';
import { HttpError } from '../lib/http.mjs';
import { compactStack, createStacks, resolveStackSpawnDir, stackHttpError, stampForeign, validateStackEdit, withStackEditor } from '../lib/stacks.mjs';

const STACK_ID = 'st-1a2b3c4d';
const SRC_ID = 'aaaaaaaa-0000-0000-0000-000000000001';
const UPDATED = '2026-09-29T12:00:00.000Z';

/** The StackView fixture (docs/architecture.md → Session stacks), rooted at `absCwd`. */
function stackView({ absCwd, host = 'laptop' } = {}) {
  const file = `/home/tester/.local/state/fleet/stacks/${STACK_ID}.md`;
  return {
    host,
    id: STACK_ID,
    label: 'Login redirect fix',
    path: file,
    cwd: '~/Code/project',
    absCwd,
    created: '2026-09-29T10:00:00.000Z',
    updated: UPDATED,
    generatedAt: '2026-09-29T10:00:05.000Z',
    editedAt: null,
    contextLine: `You're running in the session stack with shared context: ${file}.`,
    remoteContextLine: `You're running in the session stack with shared context: stack ${STACK_ID} on host ${host}. Read it with \`fleet -H ${host} stack show ${STACK_ID}\` and update it with \`fleet -H ${host} stack set ${STACK_ID}\` (the whole markdown on stdin; \`fleet -H ${host} stack show ${STACK_ID} --json\` has it under \`markdown\`).`,
    members: [
      { session: SRC_ID, host, name: 'login-redirect', added: '2026-09-29T10:00:00.000Z', closed: null, live: true, status: 'idle', briefPath: `/home/tester/.local/state/fleet/briefs/${SRC_ID}.md`, briefExists: false },
    ],
    markdown: `---\nstack: ${STACK_ID}\nlabel: Login redirect fix\n---\n## Summary\nFixing the login redirect.\n`,
    body: '## Summary\nFixing the login redirect.\n',
    parsed: { summary: 'Fixing the login redirect.', resources: [], notes: '' },
  };
}

const cliError = (code, stderr, stdout = '') => Object.assign(new Error(stderr || `exit ${code}`), { code, stderr, stdout });

/**
 * A fake host: `run` answers `fleet list --json` from `sessions` and `fleet --local stack …`
 * from the fixture; the spawner records the request and makes the new session appear in the
 * list (by tmux name), like a real spawn does a few seconds later.
 */
function fakeHost(self, { dir }) {
  const calls = [];
  const view = stackView({ absCwd: dir, host: self });
  const settings = { leadPrompt: null, leadPromptDefault: 'You lead {label}.', siblingReports: false, reportPrompt: null, reportPromptDefault: 'Report to {lead}.' };
  const sessions = [{ session_id: SRC_ID, name: 'login-redirect', status: 'idle', backend: 'tmux', handle: '%1', tmux_session: 'login', cwd: dir, updated_at: 1, stack: null }];
  let n = 0;
  const run = async (bin, args, opts = {}) => {
    calls.push({ args, opts });
    if (args[0] === 'list') return { stdout: JSON.stringify(sessions), stderr: '' };
    if (args[1] === 'config') {
      settings[args[3].replace(/^stacks\./, '')] = JSON.parse(args[4]);
      return { stdout: '', stderr: '' };
    }
    assert.deepEqual(args.slice(0, 2), ['--local', 'stack'], `unexpected fleet call ${args.join(' ')}`);
    assert.equal(args.at(-1), '--json');
    const [sub, ...rest] = args.slice(2, -1);
    const out = (v) => ({ stdout: JSON.stringify(v), stderr: '' });
    const known = (id) => {
      if (id !== STACK_ID) throw cliError(3, `Error: no stack matches "${id}"`); // the real CLI: exit 3
    };
    switch (sub) {
      case 'list':
        return out({ host: self, stacks: [view], settings });
      case 'lead':
        known(rest[0]);
        assert.equal(opts.timeout, 30 * 1000);
        return out({ ...view, lead: rest[1] === '--clear' ? null : rest[1], sent: rest[1] !== '--clear' && !rest.includes('--quiet') });
      case 'show':
        known(rest[0]);
        return out(view);
      case 'set': {
        known(rest[0]);
        const expect = rest.find((a) => a.startsWith('--expect-updated='))?.split('=')[1];
        if (expect && expect !== UPDATED) {
          const msg = `stack Login redirect fix (${STACK_ID}) changed since it was opened (updated ${UPDATED}) — not saved`;
          throw cliError(3, `Error: ${msg}`, JSON.stringify({ error: msg, id: STACK_ID, updated: UPDATED }));
        }
        return out({ ...view, markdown: opts.input, editedAt: UPDATED });
      }
      case 'rename':
        known(rest[0]);
        assert.equal(rest[1], '--');
        return out({ ...view, label: rest[2], editedAt: UPDATED });
      case 'rm':
        known(rest[0]);
        assert.equal(rest[1], '-f');
        return out({ removed: rest[0] });
      case 'ensure':
        if (rest[0] !== SRC_ID) throw cliError(1, `Error: no live session matches "${rest[0]}"`);
        // The real flat shape: StackView keys + created (bool) / createdAt / generated / warning + stack (nested view).
        return out({ ...view, created: true, createdAt: view.created, generated: true, warning: null, stack: view });
      case 'add': {
        known(rest[0]);
        const on = rest.indexOf('--on');
        const host = on >= 0 ? rest[on + 1] : self;
        if (on >= 0) assert.equal(rest.at(-1), '-', 'a member on another host comes with --input -');
        return out({ ...view, members: [...view.members, { session: rest[1], host, closed: null, live: true }] });
      }
      case 'sync':
        return out({ host: self, changed: [], stacks: [view] });
      default:
        throw cliError(2, `error: unrecognized subcommand '${sub}'`);
    }
  };
  const cli = createFleetCli({ run });
  const fleet = createFleet({ cli, self, ttlMs: 0 });
  const spawner = {
    spawn: async (req) => {
      calls.push({ spawn: req });
      n += 1;
      sessions.push({ session_id: `cccccccc-0000-0000-0000-00000000000${n}`, name: req.name, status: 'busy', backend: 'tmux', handle: `%${n + 10}`, tmux_session: req.name, cwd: req.dir, updated_at: 2, stack: null });
      return { name: req.name, dir: req.dir, tmuxSession: req.name, command: 'claude', trusted: false, model: req.model || null };
    },
  };
  const killer = { kill: async () => ({ process: 'terminated', terminal: 'tmux-session-killed' }) };
  const logs = [];
  const stacks = createStacks({
    cli,
    listSessions: async () => (await fleet.localHost({ force: true })).sessions,
    joinDelaysMs: [0, 0, 0],
    sleep: async () => {},
    log: (l) => logs.push(l),
  });
  const joins = [];
  const join = stacks.join;
  stacks.join = (...a) => {
    const p = join(...a);
    joins.push(p);
    return p;
  };
  const stackCalls = () => calls.filter((c) => c.args?.[1] === 'stack').map((c) => c.args.slice(2, -1));
  return { calls, cli, fleet, spawner, killer, stacks, joins, logs, view, sessions, stackCalls, backend: {}, transcripts: {} };
}

function tempTree(t) {
  const base = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'fleet-stacks-')));
  const cwd = path.join(base, 'project');
  fs.mkdirSync(path.join(cwd, 'sub'), { recursive: true });
  fs.mkdirSync(path.join(base, 'other'));
  t.after(() => fs.rmSync(base, { recursive: true, force: true }));
  return { base, cwd };
}

function solo(t, { autoName = false, spawnNamer = null } = {}) {
  const { base, cwd } = tempTree(t);
  const host = fakeHost('laptop', { dir: cwd });
  const config = normalizeConfig({ self: 'laptop', spawnDirs: [{ path: path.join(base, 'other') }], web: { autoName: { enabled: autoName } } }, { env: {}, home: '/home/tester' });
  const api = createApi({ config, ...host, spawnNamer, warmFleet: false });
  const call = async (method, pathname, body) => {
    const req = Object.assign(new PassThrough(), { method, headers: {} });
    req.end(body == null ? '' : JSON.stringify(body));
    try {
      return await api(req, new URL(`http://x${pathname}`));
    } catch (err) {
      if (err instanceof HttpError) return { status: err.status, body: { error: err.message } };
      throw err;
    }
  };
  return { host, api, call, base, cwd };
}

// --- lib/fleet-cli.mjs ------------------------------------------------------------------

test('fleet CLI stack calls: `fleet --local stack <sub> … --json`, markdown on stdin, 150 s for ensure', async () => {
  const calls = [];
  const cli = createFleetCli({ run: async (bin, args, opts) => (calls.push({ args, opts }), { stdout: '{"ok":true}', stderr: '' }), bin: '/x/fleet' });
  await cli.stackList();
  await cli.stackShow(STACK_ID);
  await cli.stackSet(STACK_ID, '# md', UPDATED);
  await cli.stackSet(STACK_ID, '# md');
  await cli.stackRemove(STACK_ID);
  await cli.stackEnsure(SRC_ID, { label: '-odd label' });
  await cli.stackAdd(STACK_ID, SRC_ID);
  await cli.stackSync();
  await cli.stackAdd(STACK_ID, SRC_ID, { host: 'workstation', input: { hosts: [] } });
  await cli.stackSync({ input: { hosts: [] } });
  assert.deepEqual(calls.map((c) => c.args), [
    ['--local', 'stack', 'list', '--json'],
    ['--local', 'stack', 'show', STACK_ID, '--json'],
    ['--local', 'stack', 'set', STACK_ID, `--expect-updated=${UPDATED}`, '--json'],
    ['--local', 'stack', 'set', STACK_ID, '--json'],
    ['--local', 'stack', 'rm', STACK_ID, '-f', '--json'],
    ['--local', 'stack', 'ensure', SRC_ID, '--label=-odd label', '--json'],
    ['--local', 'stack', 'add', STACK_ID, SRC_ID, '--json'],
    ['--local', 'stack', 'sync', '--json'],
    ['--local', 'stack', 'add', STACK_ID, SRC_ID, '--on', 'workstation', '--input', '-', '--json'],
    ['--local', 'stack', 'sync', '--input', '-', '--json'],
  ]);
  assert.equal(calls[2].opts.input, '# md');
  assert.equal(calls[8].opts.input, '{"hosts":[]}');
  assert.equal(calls[9].opts.input, '{"hosts":[]}');
  assert.equal(calls[7].opts.input, undefined);
  assert.equal(calls[5].opts.timeout, 150 * 1000);
  assert.equal(calls[0].opts.timeout, 20 * 1000);
  assert.equal(calls[0].opts.env.NO_COLOR, '1');
});

test('fleet CLI stack errors carry exitCode, the stdout report and `missing` for an old CLI', async () => {
  let err = cliError(3, 'Error: the stack changed', '{"error":"changed","updated":"x"}');
  const cli = createFleetCli({ run: async () => { throw err; } });
  const e1 = await cli.stackSet(STACK_ID, 'x', UPDATED).catch((e) => e);
  assert.equal(e1.exitCode, 3);
  assert.deepEqual(e1.report, { error: 'changed', updated: 'x' });
  assert.equal(e1.message, 'the stack changed');
  err = cliError(2, "error: unrecognized subcommand 'stack'\n\nUsage: fleet …");
  const e2 = await cli.stackList().catch((e) => e);
  assert.equal(e2.missing, true);
  assert.equal(stackHttpError(e2).status, 501);
  err = Object.assign(new Error('killed'), { killed: true });
  assert.equal(stackHttpError(await cli.stackEnsure(SRC_ID).catch((e) => e)).status, 504);
});

// --- lib/stacks.mjs ---------------------------------------------------------------------

test('stackHttpError maps unknown → 404, refusal → 400, ambiguous/conflict → 409, else 502', () => {
  assert.equal(stackHttpError({ message: 'no stack matches "st-00000000"', exitCode: 1 }).status, 404);
  // The real CLI's texts (exit 3): unknown / deleted meanwhile → 404; a member that is not there → 409.
  assert.equal(stackHttpError({ message: 'no stack matches "login"', exitCode: 3 }).status, 404);
  assert.equal(stackHttpError({ message: 'stack st-1a2b3c4d is gone', exitCode: 3 }).status, 404);
  assert.equal(stackHttpError({ message: 'x is not a member of stack Login (st-1a2b3c4d)', exitCode: 3 }).status, 409);
  assert.equal(stackHttpError({ message: 'session is already in stack st-1', exitCode: 1 }).status, 400);
  assert.equal(stackHttpError({ message: 'ambiguous', exitCode: 2 }).status, 409);
  assert.equal(stackHttpError({ message: 'boom', exitCode: null }).status, 502);
});

test('validateStackEdit: markdown string ≤ 64 kB, expectUpdated an ISO timestamp', () => {
  assert.deepEqual(validateStackEdit({ markdown: '# x', expectUpdated: UPDATED }), { markdown: '# x', expectUpdated: UPDATED });
  assert.equal(validateStackEdit({ markdown: '' }).expectUpdated, null);
  for (const bad of [{}, { markdown: 42 }, { markdown: 'é'.repeat(33 * 1024) }, { markdown: 'x', expectUpdated: 'yesterday; rm' }]) {
    assert.throws(() => validateStackEdit(bad), (e) => e.status === 400, JSON.stringify(bad).slice(0, 40));
  }
});

test('withStackEditor links absCwd: a local folder here, Remote-SSH for a peer', () => {
  const config = normalizeConfig({ self: 'laptop', hosts: { workstation: { ssh: 'ws', web: 'http://127.0.0.1:1' } } }, { env: {}, home: '/home/tester' });
  const v = stackView({ absCwd: '/home/tester/Code/project' });
  assert.equal(withStackEditor(v, 'laptop', config).editorUrl, 'vscode://file/home/tester/Code/project');
  assert.equal(withStackEditor(v, 'workstation', config).editorUrl, 'vscode://vscode-remote/ssh-remote+ws/home/tester/Code/project');
  assert.deepEqual(withStackEditor({ error: 'x' }, 'laptop', config), { error: 'x' });
});

test('resolveStackSpawnDir: the base, a dir inside it, or a spawn dir — nothing else', async (t) => {
  const { base, cwd } = tempTree(t);
  const roots = [path.join(base, 'other')];
  assert.equal(await resolveStackSpawnDir({ base: cwd, roots }), cwd);
  assert.equal(await resolveStackSpawnDir({ base: cwd, requested: path.join(cwd, 'sub'), roots }), path.join(cwd, 'sub'));
  assert.equal(await resolveStackSpawnDir({ base: cwd, requested: roots[0], roots }), roots[0]);
  await assert.rejects(resolveStackSpawnDir({ base: cwd, requested: base, roots }), (e) => e.status === 400);
  await assert.rejects(resolveStackSpawnDir({ base: path.join(base, 'gone'), roots }), (e) => e.status === 400);
  await assert.rejects(resolveStackSpawnDir({ base: cwd, requested: path.join(cwd, 'nope'), roots }), (e) => e.status === 400);
});

test('sync is de-duplicated, a call made mid-run gets one trailing run, and lastSync records it', async () => {
  let n = 0;
  let release;
  const cli = { stackSync: () => (n += 1, n === 1 ? new Promise((r) => (release = () => r({ changed: ['st-1'] }))) : Promise.resolve({ changed: [] })) };
  const s = createStacks({ cli, listSessions: async () => [] });
  const a = s.sync('kill');
  const b = s.sync('kill');
  const c = s.sync('kill');
  assert.equal(b, c, 'one trailing run for any number of calls');
  release();
  await Promise.all([a, b]);
  assert.equal(n, 2);
  assert.deepEqual(s.lastSync.changed, []);
  assert.equal(s.lastSync.ok, true);
  const failing = createStacks({ cli: { stackSync: async () => { throw new Error('nope'); } }, listSessions: async () => [], log: () => {} });
  await assert.rejects(failing.sync());
  assert.equal(failing.lastSync.ok, false);
  assert.equal(failing.status().lastSync.error, 'nope');
});

test('the background sync: the first tick always runs; then only while a listed session is in a stack or a stack is known; off = never scheduled', async () => {
  let syncs = 0;
  let rows = [{ session_id: 'a', stack: null }];
  let report = { changed: [], stacks: [] };
  const inputs = [];
  const cli = { stackSync: async (o) => (syncs += 1, inputs.push(o?.input ?? null), report) };
  const s = createStacks({ cli, listSessions: async () => assert.fail('the gate uses gateSessions'), gateSessions: async () => rows });
  assert.deepEqual(await s.tick(), { ran: true }, 'the first tick seeds what this host knows');
  assert.equal(syncs, 1);
  assert.deepEqual(await s.tick(), { ran: false });
  rows = [{ session_id: 'a', stack: { id: STACK_ID, label: 'x' } }];
  assert.deepEqual(await s.tick(), { ran: true });
  assert.equal(syncs, 2);
  rows = null; // discovery failed
  assert.deepEqual(await s.tick(), { ran: false });
  // A stack this host knows (its members may run elsewhere) keeps the sync going.
  report = { changed: [], stacks: [stackView({ absCwd: '/x' })] };
  rows = [{ session_id: 'a', stack: { id: STACK_ID, label: 'x' } }];
  await s.tick();
  assert.equal(s.status().known, 1);
  rows = [{ session_id: 'a', stack: null }];
  assert.deepEqual(await s.tick(), { ran: true });
  report = { changed: [], stacks: [] };
  await s.tick();
  assert.equal(s.status().known, 0);
  assert.deepEqual(await s.tick(), { ran: false });
  assert.deepEqual(inputs, [null, null, null, null, null], 'no merged fleet attached: a plain sync');
  // Attached: the merged fleet goes in as --input; a failing source means a plain sync.
  s.attach({ fleetBody: async () => ({ hosts: [{ name: 'laptop', ok: true, sessions: [] }] }) });
  rows = [{ session_id: 'a', stack: { id: STACK_ID, label: 'x' } }];
  await s.tick();
  assert.deepEqual(inputs.at(-1), { hosts: [{ name: 'laptop', ok: true, sessions: [] }] });
  assert.equal(s.status().lastSync.merged, true);
  s.attach({ fleetBody: async () => { throw new Error('peers down'); } });
  await s.tick();
  assert.equal(inputs.at(-1), null);
  assert.equal(s.status().lastSync.merged, false);

  const off = createStacks({ cli, listSessions: async () => rows, syncEnabled: false, initialDelayMs: 1 });
  rows = [{ stack: { id: STACK_ID } }];
  const before = syncs;
  off.start();
  await new Promise((r) => setTimeout(r, 30));
  off.stop();
  assert.equal(syncs, before, 'FLEET_WEB_STACKS=0: no background sync');
  const on = createStacks({ cli, listSessions: async () => rows, initialDelayMs: 1, syncIntervalMs: 60 * 1000 });
  on.start();
  await new Promise((r) => setTimeout(r, 30));
  on.stop();
  assert.equal(syncs, before + 1, 'first run after initialDelayMs');
});

test('config: web.stacks.syncMinutes (default 2), FLEET_WEB_STACKS=0, stacks.model passthrough', () => {
  const d = normalizeConfig({}, { env: {}, home: '/home/tester' });
  assert.deepEqual(d.stacks, { sync: true, syncMinutes: 2, generate: true, model: 'sonnet' });
  const c = normalizeConfig({ web: { stacks: { syncMinutes: 5 } }, stacks: { enabled: false, model: 'haiku' } }, { env: { FLEET_WEB_STACKS: '0' }, home: '/home/tester' });
  assert.deepEqual(c.stacks, { sync: false, syncMinutes: 5, generate: false, model: 'haiku' });
  assert.throws(() => normalizeConfig({ web: { stacks: { syncMinutes: 0 } } }, { env: {}, home: '/home/tester' }), /syncMinutes/);
  assert.equal(normalizeConfig({ stacks: { model: 'bad model;' } }, { env: {}, home: '/home/tester' }).stacks.model, 'sonnet', "the CLI's key: bad → default, never a startup error");
});

test('join: waits until the spawned tmux session registers, then `stack add`; refusal stops', async () => {
  const added = [];
  let tries = 0;
  const rows = () => (tries += 1, tries < 3 ? [{ tmux_session: 'job', session_id: null }] : [{ tmux_session: 'job', session_id: 'dddddddd-1' }]);
  const cli = { stackSync: async () => ({}), stackAdd: async (st, sid) => (added.push([st, sid]), {}) };
  const s = createStacks({ cli, listSessions: async () => rows(), joinDelaysMs: [0, 0, 0, 0], sleep: async () => {} });
  const r = await s.join('job', STACK_ID);
  assert.deepEqual(r, { ok: true, reason: 'added', session: 'dddddddd-1', tries: 3 });
  assert.deepEqual(added, [[STACK_ID, 'dddddddd-1']]);

  const member = createStacks({ cli, listSessions: async () => [{ tmux_session: 'job', session_id: 'x', stack: { id: STACK_ID } }], joinDelaysMs: [0], sleep: async () => {} });
  assert.equal((await member.join('job', STACK_ID)).reason, 'already a member');
  const refusing = createStacks({
    cli: { stackSync: async () => ({}), stackAdd: async () => { throw Object.assign(new Error('already in stack st-99999999'), { exitCode: 1 }); } },
    listSessions: async () => [{ tmux_session: 'job', session_id: 'x' }],
    joinDelaysMs: [0, 0, 0],
    sleep: async () => {},
  });
  assert.deepEqual(await refusing.join('job', STACK_ID), { ok: false, reason: 'refused', session: 'x', tries: 1, error: 'already in stack st-99999999' });
  const never = createStacks({ cli, listSessions: async () => [], joinDelaysMs: [0, 0], sleep: async () => {} });
  assert.deepEqual(await never.join('job', STACK_ID), { ok: false, reason: 'not found', tries: 2 });
});

test('join on another host: that peer\'s rows, `stack add --on <host> --input -` with the row', async () => {
  const added = [];
  let tries = 0;
  const peer = async (name) => {
    assert.equal(name, 'workstation');
    tries += 1;
    return tries < 2 ? [{ tmux_session: 'job', session_id: null }] : [{ tmux_session: 'job', session_id: 'dddddddd-1', cwd: '/home/ws/project' }];
  };
  const cli = { stackSync: async () => ({}), stackAdd: async (st, sid, o) => (added.push([st, sid, o]), stackView({ absCwd: '/x' })) };
  const s = createStacks({ cli, self: 'laptop', listSessions: async () => assert.fail('not this host'), joinDelaysMs: [0, 0, 0], sleep: async () => {} });
  assert.deepEqual(await s.join('job', STACK_ID, 'workstation'), { ok: false, reason: 'not found', tries: 3 }, 'no peer source attached yet');
  s.attach({ peerSessions: peer });
  tries = 0;
  const r = await s.join('job', STACK_ID, 'workstation');
  assert.deepEqual(r, { ok: true, reason: 'added', session: 'dddddddd-1', tries: 2 });
  assert.deepEqual(added, [[STACK_ID, 'dddddddd-1', { host: 'workstation', input: { hosts: [{ name: 'workstation', ok: true, sessions: [{ tmux_session: 'job', session_id: 'dddddddd-1', cwd: '/home/ws/project' }] }] } }]]);
  assert.equal(s.status().known, 1, 'the add\'s StackView is remembered');
  // Same name on this host is a different join.
  const local = createStacks({ cli, self: 'laptop', listSessions: async () => [{ tmux_session: 'job', session_id: 'x' }], joinDelaysMs: [0], sleep: async () => {} });
  assert.equal((await local.join('job', STACK_ID)).reason, 'added');
  assert.deepEqual(added.at(-1), [STACK_ID, 'x', undefined]);
});

test('compactStack and stampForeign: members of a stack homed elsewhere get `stack` on their rows', () => {
  const view = { ...stackView({ absCwd: '/x', host: 'laptop' }), members: [
    { session: 'a1', host: 'laptop', closed: null, live: true },
    { session: 'b1', host: 'workstation', closed: null },
    { session: 'b2', host: 'workstation', closed: '2026-01-01T00:00:00.000Z', live: false },
    { session: 'c1', host: 'office', closed: null, live: true },
  ] };
  const c = compactStack(view);
  assert.deepEqual(c, { id: STACK_ID, label: 'Login redirect fix', host: 'laptop', lead: null, members: [
    { session: 'a1', host: 'laptop', live: true },
    { session: 'b1', host: 'workstation', live: true },
    { session: 'b2', host: 'workstation', live: false },
    { session: 'c1', host: 'office', live: true },
  ] });
  assert.equal(compactStack({ id: 'nope' }), null);
  const hosts = [
    { name: 'laptop', ok: true, stacks: [c], sessions: [{ session_id: 'a1', stack: { id: STACK_ID, label: 'Login redirect fix', host: 'laptop' } }] },
    { name: 'workstation', ok: true, stacks: [{ id: 'st-99999999', label: 'Own', host: 'workstation', members: [{ session: 'b3', host: 'workstation', live: true }, { session: 'a1', host: 'laptop', live: true }] }], sessions: [
      { session_id: 'b1', stack: null },
      { session_id: 'b2', stack: null },
      { session_id: 'b3', stack: { id: 'st-99999999', label: 'Own', host: 'workstation' } },
    ] },
    { name: 'office', ok: false, error: 'unreachable', sessions: [] },
  ];
  const out = stampForeign(hosts);
  assert.deepEqual(out[1].sessions[0].stack, { id: STACK_ID, label: 'Login redirect fix', host: 'laptop', lead: null });
  const led = stampForeign([{ ...hosts[0], stacks: [{ ...c, lead: 'b1' }] }, hosts[1]]);
  assert.equal(led[1].sessions[0].stack.lead, 'b1', 'the lead travels with the stamped ref');
  assert.equal(out[1].sessions[1].stack, null, 'a closed member is not stamped');
  assert.deepEqual(out[1].sessions[2].stack, { id: 'st-99999999', label: 'Own', host: 'workstation' });
  assert.deepEqual(out[0].sessions[0].stack, { id: STACK_ID, label: 'Login redirect fix', host: 'laptop' }, "the row's own stack wins");
  assert.equal(hosts[1].sessions[0].stack, null, 'pure: the input is untouched');
  assert.equal(stampForeign(null), null);
});

// --- routes (createApi, local) ------------------------------------------------------------

test('GET stacks / stack: the CLI JSON with editorUrl; unknown → 404; a bad id → 400', async (t) => {
  const { call, cwd } = solo(t);
  const list = await call('GET', '/api/hosts/laptop/stacks');
  assert.equal(list.status, 200);
  assert.equal(list.body.host, 'laptop');
  assert.equal(list.body.stacks[0].id, STACK_ID);
  assert.equal(list.body.stacks[0].editorUrl, `vscode://file${cwd}`);
  const one = await call('GET', `/api/hosts/laptop/stacks/${STACK_ID}`);
  assert.equal(one.status, 200);
  assert.equal(one.body.contextLine.startsWith("You're running in the session stack with shared context: "), true);
  assert.equal(one.body.editorUrl, `vscode://file${cwd}`);
  assert.equal((await call('GET', '/api/hosts/laptop/stacks/st-00000000')).status, 404);
  assert.equal((await call('GET', '/api/hosts/laptop/stacks/nope')).status, 400);
  assert.equal((await call('POST', `/api/hosts/laptop/stacks/${STACK_ID}`, {})).status, 405);
  assert.equal((await call('GET', '/api/hosts/laptop/stacks/sync')).status, 405);
  assert.equal((await call('GET', '/api/hosts/nope/stacks')).status, 404);
});

test('PUT stack: markdown on stdin, expectUpdated passed, conflict → 409 { error, updated }, bad body → 400', async (t) => {
  const { call, host } = solo(t);
  const ok = await call('PUT', `/api/hosts/laptop/stacks/${STACK_ID}`, { markdown: '## Summary\nnew', expectUpdated: UPDATED });
  assert.equal(ok.status, 200);
  assert.equal(ok.body.markdown, '## Summary\nnew');
  assert.deepEqual(host.stackCalls().at(-1), ['set', STACK_ID, `--expect-updated=${UPDATED}`]);
  const stale = await call('PUT', `/api/hosts/laptop/stacks/${STACK_ID}`, { markdown: 'x', expectUpdated: '2026-01-01T00:00:00.000Z' });
  assert.equal(stale.status, 409);
  assert.deepEqual(stale.body, { error: `stack Login redirect fix (${STACK_ID}) changed since it was opened (updated ${UPDATED}) — not saved`, id: STACK_ID, updated: UPDATED });
  assert.equal((await call('PUT', `/api/hosts/laptop/stacks/${STACK_ID}`, { markdown: 42 })).status, 400);
  assert.equal((await call('PUT', `/api/hosts/laptop/stacks/${STACK_ID}`, { markdown: 'x'.repeat(64 * 1024 + 1) })).status, 400);
  assert.equal((await call('PUT', `/api/hosts/laptop/stacks/${STACK_ID}`, { markdown: 'x'.repeat(64 * 1024) })).status, 200, 'up to 64 kB (the body limit is raised)');
  assert.equal((await call('PUT', '/api/hosts/laptop/stacks/st-00000000', { markdown: 'x' })).status, 404);
});

test('POST stacks/:id/rename: the label as a positional after `--`; bad label → 400; unknown → 404', async (t) => {
  const { call, host } = solo(t);
  const ok = await call('POST', `/api/hosts/laptop/stacks/${STACK_ID}/rename`, { label: '  Auth rework  ' });
  assert.equal(ok.status, 200);
  assert.equal(ok.body.label, 'Auth rework');
  assert.equal(ok.body.editorUrl.startsWith('vscode://file/'), true, 'the editor link is added like GET');
  assert.deepEqual(host.stackCalls().at(-1), ['rename', STACK_ID, '--', 'Auth rework']);
  for (const body of [{}, { label: 42 }, { label: '   ' }, { label: 'a\nb' }, { label: 'x'.repeat(81) }]) {
    assert.equal((await call('POST', `/api/hosts/laptop/stacks/${STACK_ID}/rename`, body)).status, 400, JSON.stringify(body));
  }
  assert.equal((await call('POST', '/api/hosts/laptop/stacks/st-00000000/rename', { label: 'x' })).status, 404);
  assert.equal((await call('GET', `/api/hosts/laptop/stacks/${STACK_ID}/rename`)).status, 405);
});

test('DELETE stack → { removed }; POST stacks/sync → the sync report', async (t) => {
  const { call, host } = solo(t);
  const del = await call('DELETE', `/api/hosts/laptop/stacks/${STACK_ID}`);
  assert.deepEqual(del, { status: 200, body: { removed: STACK_ID } });
  assert.deepEqual(host.stackCalls().at(-1), ['rm', STACK_ID, '-f']);
  const sync = await call('POST', '/api/hosts/laptop/stacks/sync', {});
  assert.equal(sync.status, 200);
  assert.deepEqual(sync.body.changed, []);
  assert.equal(sync.body.stacks[0].editorUrl.startsWith('vscode://file/'), true);
});

test('sibling spawn from a session: ensure, spawn in its cwd with the context line, then add in the background', async (t) => {
  const { call, host, cwd } = solo(t);
  const res = await call('POST', '/api/hosts/laptop/sessions/aaaaaaaa/stack/spawn', { prompt: '  Write the tests\n', name: 'login-tests', model: 'claude-sonnet-5' });
  assert.equal(res.status, 200);
  assert.equal(res.body.host, 'laptop');
  assert.equal(res.body.created, true);
  assert.equal(res.body.generated, true);
  assert.equal(res.body.stack.id, STACK_ID);
  assert.equal(res.body.stack.created, host.view.created, 'stack.created is the timestamp (the nested view)');
  assert.equal('createdAt' in res.body.stack, false);
  assert.equal(res.body.stack.editorUrl, `vscode://file${cwd}`);
  assert.deepEqual(res.body.spawn, { ok: true, host: 'laptop', name: 'login-tests', dir: cwd, tmuxSession: 'login-tests', command: 'claude', trusted: false, model: 'claude-sonnet-5' });
  const spawned = host.calls.find((c) => c.spawn).spawn;
  assert.equal(spawned.prompt, `${host.view.contextLine} Write the tests`, 'like core::stack::stack_prompt: the prompt trimmed');
  assert.equal(spawned.dir, cwd);
  assert.deepEqual(host.stackCalls()[0], ['ensure', SRC_ID]);
  const joined = await host.joins[0];
  assert.deepEqual(joined, { ok: true, reason: 'added', session: 'cccccccc-0000-0000-0000-000000000001', tries: 1 });
  assert.deepEqual(host.stackCalls().at(-1), ['add', STACK_ID, 'cccccccc-0000-0000-0000-000000000001']);
});

test('sibling spawn: empty prompt = just the context line; dir inside the cwd or a spawn dir; label passed to ensure', async (t) => {
  const { call, host, cwd, base } = solo(t);
  const a = await call('POST', `/api/hosts/laptop/sessions/${SRC_ID}/stack/spawn`, { dir: path.join(cwd, 'sub'), label: 'Login work' });
  assert.equal(a.status, 200);
  const spawnReqs = host.calls.filter((c) => c.spawn).map((c) => c.spawn);
  assert.equal(spawnReqs[0].prompt, host.view.contextLine);
  assert.equal(spawnReqs[0].dir, path.join(cwd, 'sub'));
  assert.deepEqual(host.stackCalls()[0], ['ensure', SRC_ID, '--label=Login work']);
  const b = await call('POST', `/api/hosts/laptop/sessions/${SRC_ID}/stack/spawn`, { dir: path.join(base, 'other') });
  assert.equal(b.status, 200, 'a spawn dir is fine too');
  await Promise.all(host.joins);
  const before = host.stackCalls().length;
  for (const body of [{ dir: base }, { dir: '/' }, { dir: 'relative' }, { model: "x'; id" }, { prompt: 7 }, { label: 'a\nb' }]) {
    const r = await call('POST', `/api/hosts/laptop/sessions/${SRC_ID}/stack/spawn`, body);
    assert.equal(r.status, 400, JSON.stringify(body));
  }
  assert.equal(host.stackCalls().length, before, 'a bad request never reaches ensure (no model call)');
  assert.equal((await call('POST', '/api/hosts/laptop/sessions/zzzzzzzzzz/stack/spawn', {})).status, 404);
  assert.equal((await call('GET', `/api/hosts/laptop/sessions/${SRC_ID}/stack/spawn`)).status, 405);
});

test('link after the fact: …/stack/link ensures the stack and adds the other session; …/stacks/:id/add adds into one', async (t) => {
  const { call, host, cwd } = solo(t);
  const other = 'bbbbbbbb-0000-0000-0000-000000000002';
  host.sessions.push({ session_id: other, name: 'login-tests', status: 'idle', backend: 'tmux', handle: '%2', tmux_session: 'tests', cwd, updated_at: 1, stack: null });
  const res = await call('POST', `/api/hosts/laptop/sessions/${SRC_ID}/stack/link`, { session: other, label: 'Login work' });
  assert.equal(res.status, 200, JSON.stringify(res.body));
  assert.equal(res.body.created, true);
  assert.equal(res.body.stack.id, STACK_ID);
  assert.equal(res.body.stack.editorUrl, `vscode://file${cwd}`);
  assert.deepEqual(host.stackCalls(), [['ensure', SRC_ID, '--label=Login work'], ['add', STACK_ID, other]]);

  const into = await call('POST', `/api/hosts/laptop/stacks/${STACK_ID}/add`, { session: other });
  assert.equal(into.status, 200);
  assert.equal(into.body.stack.members.at(-1).session, other);
  assert.equal(into.body.stack.editorUrl, `vscode://file${cwd}`);
  assert.equal(host.stackCalls().at(-1)[0], 'add');

  // Already a member → the stack as is; in another stack → 409; not live → 404; bad body → 400.
  host.sessions[1].stack = { id: STACK_ID, label: 'Login redirect fix' };
  const again = await call('POST', `/api/hosts/laptop/stacks/${STACK_ID}/add`, { session: other });
  assert.equal(again.status, 200);
  assert.equal(host.stackCalls().at(-1)[0], 'show');
  host.sessions[1].stack = { id: 'st-99999999', label: 'Elsewhere' };
  const taken = await call('POST', `/api/hosts/laptop/stacks/${STACK_ID}/add`, { session: other });
  assert.equal(taken.status, 409);
  assert.match(taken.body.error, /Elsewhere/);
  assert.equal((await call('POST', `/api/hosts/laptop/stacks/${STACK_ID}/add`, { session: 'cccccccc-0000-0000-0000-000000000009' })).status, 404);
  const before = host.stackCalls().length;
  for (const body of [{}, { session: 7 }, { session: 'a b' }, { session: other, host: 'office' }]) {
    assert.equal((await call('POST', `/api/hosts/laptop/sessions/${SRC_ID}/stack/link`, body)).status, 400, JSON.stringify(body));
  }
  assert.equal((await call('POST', `/api/hosts/laptop/sessions/${SRC_ID}/stack/link`, { session: SRC_ID })).status, 400, 'not with itself');
  assert.equal(host.stackCalls().length, before, 'a bad request never reaches ensure (no model call)');
  assert.equal((await call('GET', `/api/hosts/laptop/stacks/${STACK_ID}/add`)).status, 405);
});

test('spawn from a stack: no ensure, dir = the stack absCwd, unknown stack → 404, missing absCwd → 400', async (t) => {
  const { call, host, cwd } = solo(t);
  const res = await call('POST', `/api/hosts/laptop/stacks/${STACK_ID}/spawn`, { prompt: 'Review it' });
  assert.equal(res.status, 200);
  assert.equal(res.body.created, false);
  assert.equal(res.body.generated, false);
  assert.equal(res.body.spawn.dir, cwd);
  assert.equal(host.calls.find((c) => c.spawn).spawn.prompt, `${host.view.contextLine} Review it`);
  assert.equal(host.stackCalls().some((c) => c[0] === 'ensure'), false);
  await host.joins[0];
  assert.equal(host.stackCalls().at(-1)[0], 'add');
  assert.equal((await call('POST', '/api/hosts/laptop/stacks/st-00000000/spawn', {})).status, 404);
  host.view.absCwd = path.join(cwd, 'gone');
  const gone = await call('POST', `/api/hosts/laptop/stacks/${STACK_ID}/spawn`, {});
  assert.equal(gone.status, 400);
  assert.match(gone.body.error, /not a directory/);
});

test('sibling spawn keeps the spawn-namer parity: unnamed + a prompt + auto-naming on → scheduled', async (t) => {
  const scheduled = [];
  const { call, host } = solo(t, { autoName: true, spawnNamer: { schedule: async (tmux) => void scheduled.push(tmux) } });
  const r = await call('POST', `/api/hosts/laptop/sessions/${SRC_ID}/stack/spawn`, { prompt: 'go' });
  assert.deepEqual(scheduled, [r.body.spawn.tmuxSession]);
  assert.equal(host.calls.find((c) => c.spawn).spawn.nameGiven, false);
  await call('POST', `/api/hosts/laptop/sessions/${SRC_ID}/stack/spawn`, {}); // only the context line: nothing to name from
  assert.equal(scheduled.length, 1);
});

test('kill runs `stack sync` afterwards; a failing sync never fails the kill', async (t) => {
  const { call, host } = solo(t);
  const r = await call('POST', `/api/hosts/laptop/sessions/${SRC_ID}/kill`, {});
  assert.equal(r.status, 200);
  await new Promise((res) => setImmediate(res));
  assert.deepEqual(host.stackCalls().at(-1), ['sync']);
  host.cli.stackSync = async () => { throw new Error('boom'); };
  assert.equal((await call('POST', `/api/hosts/laptop/sessions/${SRC_ID}/kill`, {})).status, 200);
  await new Promise((res) => setImmediate(res));
  assert.equal(host.stacks.lastSync.ok, false);
});

test('health reports stacks.lastSync; settings reports stacks { enabled, model }; no stacks dep → 501', async (t) => {
  const { call, host } = solo(t);
  await call('POST', '/api/hosts/laptop/stacks/sync', {});
  const h = await call('GET', '/api/health');
  assert.equal(h.body.stacks.lastSync.ok, true);
  assert.equal(h.body.stacks.sync, true);
  const s = await call('GET', '/api/settings');
  assert.deepEqual(s.body.stacks, { enabled: true, model: 'sonnet', generate: true });
  const bare = createApi({ config: normalizeConfig({ self: 'laptop' }, { env: {}, home: '/home/tester' }), fleet: host.fleet, warmFleet: false });
  await assert.rejects(bare({ method: 'GET', headers: {} }, new URL('http://x/api/hosts/laptop/stacks')), (e) => e.status === 501);
});

// --- proxied to a peer --------------------------------------------------------------------

function listen(server) {
  return new Promise((resolve) => server.listen(0, '127.0.0.1', () => resolve(`http://127.0.0.1:${server.address().port}`)));
}

test('stack routes are proxied once to the peer; the asking server builds the editor link', async (t) => {
  const { cwd } = tempTree(t);
  const hosts = { laptop: fakeHost('laptop', { dir: cwd }), workstation: fakeHost('workstation', { dir: cwd }) };
  const urls = {};
  const handles = {};
  for (const name of Object.keys(hosts)) {
    const server = createHttpServer({ handleApi: (req, url) => handles[name](req, url) });
    urls[name] = await listen(server);
    t.after(() => server.close());
  }
  for (const [name, host] of Object.entries(hosts)) {
    const config = normalizeConfig(
      { self: name, hosts: { laptop: { web: urls.laptop }, workstation: { web: urls.workstation, ssh: 'ws' } }, spawnDirs: [{ path: cwd }] },
      { env: {}, home: '/home/tester' },
    );
    handles[name] = createApi({ config, ...host, warmFleet: false });
  }
  const json = async (method, url, body) => {
    const res = await fetch(url, { method, headers: { 'content-type': 'application/json' }, ...(body ? { body: JSON.stringify(body) } : {}) });
    return { status: res.status, body: await res.json() };
  };
  const base = `${urls.laptop}/api/hosts/workstation`;
  const list = await json('GET', `${base}/stacks`);
  assert.equal(list.status, 200);
  assert.equal(list.body.host, 'workstation');
  assert.equal(list.body.stacks[0].editorUrl, `vscode://vscode-remote/ssh-remote+ws${cwd}`);
  assert.equal(hosts.laptop.stackCalls().length, 0, 'served by the peer');

  const big = 'x'.repeat(60 * 1024);
  const put = await json('PUT', `${base}/stacks/${STACK_ID}`, { markdown: big, expectUpdated: UPDATED });
  assert.equal(put.status, 200, 'a large edit passes the proxy');
  assert.equal(put.body.markdown, big);
  const conflict = await json('PUT', `${base}/stacks/${STACK_ID}`, { markdown: 'x', expectUpdated: '2026-01-01T00:00:00.000Z' });
  assert.equal(conflict.status, 409);
  assert.equal(conflict.body.updated, UPDATED);

  const sib = await json('POST', `${base}/sessions/aaaaaaaa/stack/spawn`, { prompt: 'hi' });
  assert.equal(sib.status, 200);
  assert.equal(sib.body.host, 'workstation');
  assert.equal(sib.body.stack.editorUrl, `vscode://vscode-remote/ssh-remote+ws${cwd}`);
  assert.equal(hosts.workstation.calls.find((c) => c.spawn).spawn.prompt, `${hosts.workstation.view.contextLine} hi`);

  assert.deepEqual((await json('DELETE', `${base}/stacks/${STACK_ID}`)).body, { removed: STACK_ID });
  assert.equal((await json('GET', `${urls.laptop}/api/hosts/workstation/stacks?local=1`)).status, 404, 'never chained');
});

test('a sibling on another host: the peer spawns it with the remote context line, the home host adds it, every merged fleet shows it in the stack', async (t) => {
  const { cwd } = tempTree(t);
  const hosts = { laptop: fakeHost('laptop', { dir: cwd }), workstation: fakeHost('workstation', { dir: cwd }) };
  const urls = {};
  const handles = {};
  for (const name of Object.keys(hosts)) {
    const server = createHttpServer({ handleApi: (req, url) => handles[name](req, url) });
    urls[name] = await listen(server);
    t.after(() => server.close());
  }
  const peerSessions = (name) => fetch(`${urls[name]}/api/fleet?local=1`).then((r) => r.json()).then((b) => b.hosts[0].sessions);
  for (const [name, host] of Object.entries(hosts)) {
    const config = normalizeConfig(
      { self: name, hosts: { laptop: { web: urls.laptop }, workstation: { web: urls.workstation, ssh: 'ws' } }, spawnDirs: [{ path: cwd }] },
      { env: {}, home: '/home/tester' },
    );
    handles[name] = createApi({ config, ...host, warmFleet: false });
    host.stacks.attach({ fleetBody: () => handles[name].buildFleet(), peerSessions });
  }
  const json = async (method, url, body) => {
    const res = await fetch(url, { method, headers: { 'content-type': 'application/json' }, ...(body ? { body: JSON.stringify(body) } : {}) });
    return { status: res.status, body: await res.json() };
  };
  const home = `${urls.laptop}/api/hosts/laptop`;
  // A peer without a web url, or no dir, is refused before any model call.
  assert.equal((await json('POST', `${home}/stacks/${STACK_ID}/spawn`, { host: 'office', dir: cwd })).status, 400);
  const noDir = await json('POST', `${home}/stacks/${STACK_ID}/spawn`, { host: 'workstation' });
  assert.equal(noDir.status, 400);
  assert.match(noDir.body.error, /dir is required/);
  assert.equal(hosts.laptop.stackCalls().filter((c) => c[0] === 'ensure').length, 0);

  const res = await json('POST', `${home}/sessions/${SRC_ID}/stack/spawn`, { host: 'workstation', dir: cwd, prompt: 'Run the e2e suite', name: 'login-e2e' });
  assert.equal(res.status, 200, JSON.stringify(res.body));
  assert.equal(res.body.host, 'laptop');
  assert.equal(res.body.created, true);
  assert.equal(res.body.spawn.host, 'workstation');
  assert.equal(res.body.spawn.name, 'login-e2e');
  assert.equal(res.body.spawn.tmuxSession, 'login-e2e');
  assert.equal(hosts.laptop.calls.some((c) => c.spawn), false, 'not spawned here');
  const spawned = hosts.workstation.calls.find((c) => c.spawn).spawn;
  assert.equal(spawned.dir, cwd);
  assert.equal(spawned.prompt, `${hosts.laptop.view.remoteContextLine} Run the e2e suite`);
  // The join: the laptop found the session in the workstation's list and added it there.
  assert.equal(hosts.laptop.joins.length, 1);
  const joined = await hosts.laptop.joins[0];
  assert.equal(joined.ok, true, JSON.stringify(joined));
  const add = hosts.laptop.calls.find((c) => c.args?.[2] === 'add');
  assert.deepEqual(add.args.slice(2, -1), ['add', STACK_ID, joined.session, '--on', 'workstation', '--input', '-']);
  const input = JSON.parse(add.opts.input);
  assert.equal(input.hosts[0].name, 'workstation');
  assert.equal(input.hosts[0].sessions[0].session_id, joined.session);
  assert.equal(hosts.workstation.stackCalls().length, 0, 'the workstation never touches the stack');
  // Both merged fleets show the workstation session in the laptop's stack.
  for (const name of ['laptop', 'workstation']) {
    const fleet = (await json('GET', `${urls[name]}/api/fleet`)).body;
    const ws = fleet.hosts.find((h) => h.name === 'workstation');
    const row = ws.sessions.find((s) => s.session_id === joined.session);
    assert.deepEqual(row.stack, { id: STACK_ID, label: 'Login redirect fix', host: 'laptop', lead: null }, `${name}'s view`);
    const lp = fleet.hosts.find((h) => h.name === 'laptop');
    assert.equal(lp.stacks.find((s) => s.id === STACK_ID).members.some((m) => m.session === joined.session && m.host === 'workstation'), true);
  }
  // The laptop's sync goes out with the merged fleet (so the workstation member is judged).
  await hosts.laptop.stacks.syncNow('test');
  const sync = hosts.laptop.calls.filter((c) => c.args?.[2] === 'sync').at(-1);
  assert.deepEqual(sync.args.slice(2, -1), ['sync', '--input', '-']);
  assert.deepEqual(JSON.parse(sync.opts.input).hosts.map((h) => h.name), ['laptop', 'workstation']);
});

test('POST stacks/:id/lead: a member leads (told unless quiet), null clears; bad session → 400', async (t) => {
  const { call, host } = solo(t);
  const ok = await call('POST', `/api/hosts/laptop/stacks/${STACK_ID}/lead`, { session: SRC_ID });
  assert.equal(ok.status, 200);
  assert.equal(ok.body.lead, SRC_ID);
  assert.equal(ok.body.sent, true);
  assert.deepEqual(host.stackCalls().at(-1), ['lead', STACK_ID, SRC_ID]);
  await call('POST', `/api/hosts/laptop/stacks/${STACK_ID}/lead`, { session: SRC_ID, quiet: true });
  assert.deepEqual(host.stackCalls().at(-1), ['lead', STACK_ID, SRC_ID, '--quiet']);
  const cleared = await call('POST', `/api/hosts/laptop/stacks/${STACK_ID}/lead`, { session: null });
  assert.equal(cleared.body.lead, null);
  assert.deepEqual(host.stackCalls().at(-1), ['lead', STACK_ID, '--clear']);
  for (const body of [{ session: 42 }, { session: '-x' }, { session: 'a b' }]) {
    assert.equal((await call('POST', `/api/hosts/laptop/stacks/${STACK_ID}/lead`, body)).status, 400, JSON.stringify(body));
  }
  assert.equal((await call('GET', `/api/hosts/laptop/stacks/${STACK_ID}/lead`)).status, 405);
});

test('stacks/settings: GET from `stack list --json`; PUT writes stacks.* through `config set`, "" → null', async (t) => {
  const { call, host } = solo(t);
  const got = await call('GET', '/api/hosts/laptop/stacks/settings');
  assert.equal(got.status, 200);
  assert.deepEqual(got.body, { host: 'laptop', leadPrompt: null, leadPromptDefault: 'You lead {label}.', siblingReports: false, reportPrompt: null, reportPromptDefault: 'Report to {lead}.' });
  const put = await call('PUT', '/api/hosts/laptop/stacks/settings', { leadPrompt: '  Lead {label}  ', siblingReports: true, reportPrompt: '' });
  assert.equal(put.status, 200);
  assert.equal(put.body.leadPrompt, 'Lead {label}');
  assert.equal(put.body.siblingReports, true);
  assert.equal(put.body.reportPrompt, null);
  const sets = host.calls.filter((c) => c.args?.[1] === 'config').map((c) => c.args.slice(2));
  assert.deepEqual(sets, [['set', 'stacks.leadPrompt', '"Lead {label}"'], ['set', 'stacks.reportPrompt', 'null'], ['set', 'stacks.siblingReports', 'true']]);
  for (const body of [{}, { leadPrompt: 1 }, { siblingReports: 'yes' }, { reportPrompt: 'x'.repeat(8001) }]) {
    assert.equal((await call('PUT', '/api/hosts/laptop/stacks/settings', body)).status, 400, JSON.stringify(body));
  }
  assert.equal((await call('POST', '/api/hosts/laptop/stacks/settings', {})).status, 405);
});

test('sibling spawn into a stack with a lead: the leadLine follows the context line', async (t) => {
  const { call, host } = solo(t);
  host.view.leadLine = 'Report to aaaaaaaa.';
  const res = await call('POST', `/api/hosts/laptop/stacks/${STACK_ID}/spawn`, { prompt: 'Go', name: 'sib' });
  assert.equal(res.status, 200, JSON.stringify(res.body));
  assert.equal(host.calls.find((c) => c.spawn).spawn.prompt, `${host.view.contextLine} Report to aaaaaaaa. Go`);
});
