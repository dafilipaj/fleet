import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createAutoNamer, createSpawnNamer, parseNameOutput } from '../lib/autoname.mjs';
import { createFleetCli } from '../lib/fleet-cli.mjs';

test('parseNameOutput splits renamed / tmux / held / errors', () => {
  const out = [
    'app-1a  →  fix-login-flow',
    '   ⧉ fix-login → fix-login-flow',
    '⏸ app-4f is mid-turn — nothing sent (--force overrides)',
    '✕ app-01: claude exited 1',
    '⚠ naming model unavailable: `claude -p` failed: Not logged in',
    'app-c4  →  cache-warmup   (llm)',
    '\x1b[2mapp-d5\x1b[0m  →  \x1b[1mrelease-notes\x1b[0m',
  ].join('\n');
  const r = parseNameOutput(out);
  assert.deepEqual(r.renamed, [
    { from: 'app-1a', to: 'fix-login-flow' },
    { from: 'app-c4', to: 'cache-warmup' },
    { from: 'app-d5', to: 'release-notes' },
  ]);
  assert.deepEqual(r.tmux, ['fix-login → fix-login-flow']);
  assert.equal(r.held.length, 1);
  assert.equal(r.errors.length, 1);
  assert.deepEqual(r.warnings, ['naming model unavailable: `claude -p` failed: Not logged in']);
  assert.deepEqual(parseNameOutput('no sessions still carry a Claude-derived name — nothing to rename').renamed, []);
});

test('fleet CLI nameAll runs `fleet name --all --settled --adopt-tmux` (titles only, no /rename) with NO_COLOR (dry run: `-n name --all`)', async () => {
  const calls = [];
  const run = async (bin, args, opts) => (calls.push({ bin, args, opts }), { stdout: 'x', stderr: '' });
  const cli = createFleetCli({ run, bin: '/x/fleet' });
  assert.deepEqual(await cli.nameAll(), { stdout: 'x', stderr: '' });
  assert.deepEqual(calls[0].args, ['name', '--all', '--settled', '--adopt-tmux']);
  assert.equal(calls[0].bin, '/x/fleet');
  assert.equal(calls[0].opts.env.NO_COLOR, '1');
  await cli.nameAll({ dryRun: true });
  assert.deepEqual(calls[1].args, ['-n', 'name', '--all', '--settled']);
  const missing = createFleetCli({ run: async () => { throw Object.assign(new Error('spawn'), { code: 'ENOENT' }); } });
  await assert.rejects(missing.nameAll(), /not found/);
});

test('runOnce calls the CLI once for concurrent runs and records lastRun (tmux lines come from the CLI)', async () => {
  let n = 0;
  const cli = {
    nameAll: async () => {
      n += 1;
      await new Promise((r) => setTimeout(r, 5));
      return { stdout: 'app-1a  →  fix-login-flow\n   ⧉ fw-101010 → fix-login-flow\n', stderr: '' };
    },
  };
  const an = createAutoNamer({ cli });
  const [a, b] = await Promise.all([an.runOnce('manual'), an.runOnce('manual')]);
  assert.equal(a, b);
  assert.equal(n, 1);
  assert.equal(an.lastRun.ok, true);
  assert.equal(an.lastRun.reason, 'manual');
  assert.equal(an.lastRun.renamed[0].to, 'fix-login-flow');
  assert.deepEqual(an.lastRun.tmux, ['fw-101010 → fix-login-flow']);
});

test('runOnce records failures without throwing', async () => {
  const an = createAutoNamer({ cli: { nameAll: async () => { throw new Error('boom'); } } });
  const r = await an.runOnce('scheduled');
  assert.equal(r.ok, false);
  assert.match(r.error, /boom/);
  assert.deepEqual(r.renamed, []);
});

test('start schedules a first run after the initial delay, stop cancels it', async () => {
  let n = 0;
  const an = createAutoNamer({ cli: { nameAll: async () => (n++, { stdout: '', stderr: '' }) }, initialDelayMs: 5, intervalMs: 60000 });
  an.start();
  await new Promise((r) => setTimeout(r, 40));
  an.stop();
  assert.equal(n, 1);
  assert.equal(an.lastRun.reason, 'scheduled');
});

test('fleet CLI nameOne runs `fleet name <id>` (a title, no /rename unless apply) with NO_COLOR', async () => {
  const calls = [];
  const cli = createFleetCli({ run: async (bin, args, opts) => (calls.push([bin, args, opts.env.NO_COLOR]), { stdout: 'a  →  b\n', stderr: '' }) });
  const out = await cli.nameOne({ target: 'abc-123' });
  assert.deepEqual(calls[0], ['fleet', ['name', 'abc-123'], '1']);
  await cli.nameOne({ target: 'abc-123', refresh: true, apply: true });
  assert.deepEqual(calls[1][1], ['name', 'abc-123', '--apply', '--refresh']);
  assert.equal(out.stdout, 'a  →  b\n');
});

test('spawn namer waits for registration and the first reply, then names that one session', async () => {
  const waits = [];
  const frames = [
    [], // not registered yet
    [{ session_id: 'id-1', tmux_session: 'fw-101010', name_source: 'derived', status: 'busy' }], // first turn
    [{ session_id: 'id-1', tmux_session: 'fw-101010', name_source: 'derived', status: 'waiting' }], // held
    [{ session_id: 'id-1', tmux_session: 'fw-101010', name_source: 'derived', status: 'idle' }],
  ];
  let i = 0;
  const named = [];
  const outputs = ['⏸ work-1a is waiting on you — nothing sent\n', 'work-1a  →  reply-ok\n   ⧉ fw-101010 → reply-ok\n'];
  const cli = { nameOne: async ({ target }) => (named.push(target), { stdout: outputs.shift(), stderr: '' }) };
  const logs = [];
  const namer = createSpawnNamer({
    cli,
    listSessions: async () => frames[Math.min(i++, frames.length - 1)],
    delaysMs: [1, 2, 3, 4, 5],
    sleep: async (ms) => void waits.push(ms),
    log: (l) => logs.push(l),
  });
  const p = namer.schedule('fw-101010');
  assert.equal(namer.schedule('fw-101010'), p, 'one pass per session at a time');
  const res = await p;
  assert.deepEqual(res, { ok: true, reason: 'renamed', tries: 4, renamed: 'reply-ok' });
  assert.deepEqual(named, ['id-1', 'id-1'], 'held once, retried');
  assert.deepEqual(waits, [1, 2, 3, 4]);
  assert.deepEqual(namer.pending, []);
  assert.match(logs.at(-1), /fw-101010: → reply-ok tmux: fw-101010 → reply-ok/);
});

test('spawn namer stops when the session is already named, gives up after its tries', async () => {
  const cli = { nameOne: async () => assert.fail('must not name a user-named session') };
  const named = createSpawnNamer({
    cli,
    listSessions: async () => [{ session_id: 'id-2', tmux_session: 'fw-2', name_source: 'user', status: 'idle' }],
    delaysMs: [1, 1],
    sleep: async () => {},
  });
  assert.deepEqual(await named.schedule('fw-2'), { ok: true, reason: 'already named', tries: 1 });

  const never = createSpawnNamer({ cli, listSessions: async () => { throw new Error('discovery down'); }, delaysMs: [1, 1, 1], sleep: async () => {} });
  assert.deepEqual(await never.schedule('fw-3'), { ok: false, reason: 'gave up', tries: 3 });
});
