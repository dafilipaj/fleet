import assert from 'node:assert/strict';
import { promises as fsp } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';

import {
  busyMessage,
  classify,
  createLoadAlerts,
  createLoadMonitor,
  freshState,
  loadSettings,
  parseDarwin,
  parsePs,
  parsePsi,
  processUse,
  retryWhileBusy,
  step,
} from '../lib/load.mjs';
import { createSpawner } from '../lib/spawn.mjs';

const S = loadSettings({ enabled: true, enterSeconds: 60, exitSeconds: 300 });
const tmp = () => fsp.mkdtemp(path.join(os.tmpdir(), 'fleet-load-'));

test('loadSettings: off by default, defaults for anything malformed', () => {
  assert.equal(loadSettings(undefined).enabled, false);
  const s = loadSettings({ enabled: true, load: { hot: 'x', critical: 6 }, exitSeconds: -1 });
  assert.equal(s.enabled, true);
  assert.deepEqual(s.load, { hot: 2, critical: 6 });
  assert.equal(s.exitSeconds, 300);
});

test('parseDarwin / parsePsi', () => {
  assert.deepEqual(parseDarwin('1\ntotal = 2048.00M  used = 542.31M  free = 1505.69M  (encrypted)\n'), { pressure: 'normal', swapUsedMb: 542 });
  assert.equal(parseDarwin('2\n').pressure, 'warn');
  assert.equal(parseDarwin('4\n').pressure, 'critical');
  assert.equal(parseDarwin('').pressure, null);
  assert.deepEqual(parsePsi('some avg10=1.00 avg60=12.50 avg300=3.00 total=1\nfull avg10=0.00 avg60=0.40 avg300=0.00 total=1\n'), { some: 12.5, full: 0.4 });
});

test('classify: the worse of memory and CPU, with reasons', () => {
  assert.deepEqual(classify({ pressure: 'normal', perCore: 0.9 }, S), { level: 'ok', reasons: [] });
  assert.equal(classify({ pressure: 'warn', perCore: 0.5 }, S).level, 'hot');
  assert.equal(classify({ pressure: 'normal', perCore: 2.5 }, S).level, 'hot');
  const c = classify({ pressure: 'critical', perCore: 2.5 }, S);
  assert.equal(c.level, 'critical');
  assert.deepEqual(c.reasons, ['memory pressure critical', 'load 2.5× cores']);
  assert.equal(classify({ psi: { some: 15, full: 1 }, perCore: 0.1 }, S).level, 'hot');
  assert.equal(classify({ psi: { some: 40, full: 12 }, perCore: 0.1 }, S).level, 'critical');
});

test('step: up only after enterSeconds, down only after exitSeconds; a blip resets the clock', () => {
  const hot = { level: 'hot', reasons: ['load 2.5× cores'] };
  const ok = { level: 'ok', reasons: [] };
  let st = step(null, hot, 0, S);
  assert.equal(st.state, 'ok');
  st = step(st, hot, 30_000, S);
  assert.equal(st.state, 'ok');
  st = step(st, ok, 45_000, S); // a dip: the clock starts again
  st = step(st, hot, 50_000, S);
  st = step(st, hot, 100_000, S);
  assert.equal(st.state, 'ok');
  st = step(st, hot, 110_000, S);
  assert.equal(st.state, 'hot');
  assert.deepEqual(st.reasons, ['load 2.5× cores']);
  st = step(st, ok, 200_000, S);
  assert.equal(st.state, 'hot');
  st = step(st, ok, 499_000, S);
  assert.equal(st.state, 'hot');
  st = step(st, ok, 500_000, S);
  assert.equal(st.state, 'ok');
});

test('monitor: writes the state file; an unreadable sample counts as ok (fail-open)', async () => {
  const dir = await tmp();
  const file = path.join(dir, 'load.json');
  let t = 0;
  let fail = false;
  const m = createLoadMonitor({
    settings: { ...S, enterSeconds: 0 },
    self: 'workstation',
    file,
    now: () => t,
    sample: async () => {
      if (fail) throw new Error('sysctl missing');
      return { pressure: 'critical', perCore: 1 };
    },
  });
  await m.tick();
  t = 1000;
  await m.tick();
  assert.equal(m.current().state, 'critical');
  assert.match(m.busy(), /^workstation is under heavy load \(memory pressure critical\)/);
  const onDisk = JSON.parse(await fsp.readFile(file, 'utf8'));
  assert.equal(onDisk.state, 'critical');
  assert.equal(onDisk.updatedAt, 1000);
  fail = true;
  t = 400_000;
  await m.tick();
  t = 800_000;
  await m.tick();
  assert.equal(m.current().state, 'ok');
  assert.equal(m.busy(), null);
  assert.equal(m.current().error, 'sysctl missing');
});

test('freshState: null when stale, malformed or absent', () => {
  assert.equal(freshState(null, 0), null);
  assert.equal(freshState({ state: 'weird', updatedAt: 0 }, 0), null);
  assert.equal(freshState({ state: 'hot', updatedAt: 0 }, 91_000), null);
  assert.equal(freshState({ state: 'hot', updatedAt: 0 }, 60_000).state, 'hot');
});

test('spawner: answers 409 overloaded while busy, unless forced', async () => {
  const s = createSpawner({ run: async () => ({ stdout: '' }), busy: () => busyMessage('workstation', { reasons: ['load 3× cores'] }) });
  await assert.rejects(s.spawn({ name: 'x', dir: '/nonexistent-fleet-test' }), (err) => err.status === 409 && err.overloaded === true && /under heavy load/.test(err.message));
  // forced: past the gate (it then fails on the dir, which shows the gate let it through)
  await assert.rejects(s.spawn({ name: 'x', dir: '/nonexistent-fleet-test', force: true }), (err) => err.status === 400);
});

test('retryWhileBusy: holds while overloaded (onHold once), then runs; other errors pass at once', async () => {
  let t = 0;
  let calls = 0;
  const holds = [];
  const r = await retryWhileBusy(
    async () => {
      calls++;
      if (calls < 3) throw Object.assign(new Error('busy'), { overloaded: true });
      return 'started';
    },
    { onHold: (w) => holds.push(w), sleep: async (ms) => { t += ms; }, now: () => t },
  );
  assert.equal(r, 'started');
  assert.equal(calls, 3);
  assert.deepEqual(holds, ['busy']);
  await assert.rejects(retryWhileBusy(async () => { throw new Error('boom'); }, { sleep: async () => {} }), /boom/);
  // gives up after maxMs
  t = 0;
  await assert.rejects(
    retryWhileBusy(async () => { throw Object.assign(new Error('still busy'), { overloaded: true }); }, { maxMs: 60_000, sleep: async (ms) => { t += ms; }, now: () => t }),
    /still busy/,
  );
});

test('alerts: DM on up, on worse, on recovery — not on critical → hot, not twice, not for silent hosts', async () => {
  const dir = await tmp();
  let t = 1_000_000;
  const states = { workstation: null, laptop: null };
  const sent = [];
  const mk = () =>
    createLoadAlerts({
      hosts: () => Object.keys(states),
      fetchLoad: async (h) => {
        if (h === 'laptop') throw new Error('asleep');
        return states[h];
      },
      notify: async (text) => sent.push(text),
      file: path.join(dir, 'alerts.json'),
      now: () => t,
    });
  const a = mk();
  const at = (state, reasons = []) => ({ state, reasons, since: t, updatedAt: t });
  await a.tick();
  assert.equal(sent.length, 0);
  states.workstation = at('hot', ['load 2.5× cores']);
  await a.tick();
  await a.tick();
  assert.equal(sent.length, 1);
  assert.match(sent[0], /workstation\* is under heavy load \(hot: load 2.5× cores\)/);
  states.workstation = at('critical', ['memory pressure critical']);
  await a.tick();
  assert.equal(sent.length, 2);
  states.workstation = at('hot');
  await a.tick();
  assert.equal(sent.length, 2);
  // a restart remembers what was told
  const b = mk();
  await b.tick();
  assert.equal(sent.length, 2);
  t += 20 * 60_000;
  states.workstation = at('ok');
  await b.tick();
  assert.equal(sent.length, 3);
  assert.match(sent[2], /workstation\* has recovered \(under load for 20 min\)/);
});

test('monitor: off holds nothing and writes no file; configure turns it on live; override lets sessions start', async () => {
  const dir = await tmp();
  const file = path.join(dir, 'load.json');
  let t = 0;
  const m = createLoadMonitor({ settings: { ...S, enabled: false, enterSeconds: 0 }, self: 'workstation', file, now: () => t, sample: async () => ({ pressure: 'warn', perCore: 1 }) });
  await m.tick();
  t = 1000;
  await m.tick();
  assert.equal(m.busy(), null, 'off: never busy');
  await assert.rejects(fsp.stat(file), 'off: no state file');
  assert.equal(m.current().state, 'hot', 'but the numbers are there for Settings');
  t = 2000;
  await m.configure({ ...S, enabled: true, enterSeconds: 0 });
  assert.match(m.busy(), /under heavy load/);
  assert.equal(JSON.parse(await fsp.readFile(file, 'utf8')).state, 'hot');
  await m.override(30);
  assert.equal(m.busy(), null);
  assert.equal(m.current().state, 'ok');
  assert.equal(m.current().override.held, 'hot');
  assert.equal(JSON.parse(await fsp.readFile(file, 'utf8')).state, 'ok', 'the CLI sees the override too');
  t += 31 * 60_000;
  await m.tick();
  assert.match(m.busy(), /under heavy load/, 'the override ends by itself');
  await m.configure({ ...S, enabled: false });
  await assert.rejects(fsp.stat(file), 'turned off: the file is gone');
});

test('alerts: an override is not a recovery', async () => {
  const dir = await tmp();
  const sent = [];
  let st = { state: 'hot', reasons: [], since: 0, updatedAt: Date.now() };
  const a = createLoadAlerts({ hosts: () => ['workstation'], fetchLoad: async () => st, notify: async (x) => sent.push(x), file: path.join(dir, 'a.json') });
  await a.tick();
  st = { state: 'ok', reasons: [], since: 0, updatedAt: Date.now(), override: { until: Date.now() + 1e6, held: 'hot' } };
  await a.tick();
  assert.equal(sent.length, 1);
});

test('processUse: a session counts its whole process tree; the rest are the heaviest others', () => {
  const ps = parsePs(
    [
      '  100     1  204800  12.5 /usr/local/bin/claude',
      '  101   100  102400  30.0 node',
      '  102   101   10240   0.0 rg',
      '  200     1  512000   1.0 /Applications/Browser.app/Contents/MacOS/Browser',
      '  300     1   20480  50.0 /usr/local/bin/claude',
      'garbage line',
    ].join('\n'),
  );
  assert.equal(ps.length, 5);
  const u = processUse(ps, [
    { session_id: 'a', name: 'big-one', title: 'a long first prompt', status: 'idle', pid: 100 },
    { session_id: 'b', name: 'small', status: 'busy', pid: 300 },
    { session_id: 'c', name: 'gone', pid: 999 },
  ]);
  assert.deepEqual(
    u.sessions.map((s) => [s.id, s.name, s.rssMb, s.cpu, s.processes]),
    [
      ['a', 'big-one', 310, 42.5, 3],
      ['b', 'small', 20, 50, 1],
    ],
  );
  assert.deepEqual(u.others, [{ pid: 200, name: 'Browser', rssMb: 500, cpu: 1 }]);
  assert.equal(u.sessionsMb, 330);
});

test('API: /api/hosts/<self>/load — view, PUT settings through config set (live), override, processes, restart', async () => {
  const { PassThrough } = await import('node:stream');
  const { createApi } = await import('../lib/api.mjs');
  const { normalizeConfig } = await import('../lib/config.mjs');
  const { HttpError } = await import('../lib/http.mjs');
  const dir = await tmp();
  const config = normalizeConfig({ self: 'laptop' }, { env: {}, home: '/home/tester' });
  let t = 0;
  const monitor = createLoadMonitor({ settings: config.resources, self: 'laptop', file: path.join(dir, 'load.json'), now: () => t, sample: async () => ({ pressure: 'normal', perCore: 0.4 }) });
  const sets = [];
  let restarted = 0;
  const api = createApi({
    config,
    fleet: { localHost: async () => ({ name: 'laptop', ok: true, sessions: [] }) },
    backend: {},
    cli: { configSet: async (x) => sets.push(x) },
    loadMonitor: monitor,
    loadProcesses: async () => ({ sessions: [], others: [], totalMb: 1, sessionsMb: 0 }),
    restartSelf: () => restarted++,
    warmFleet: false,
  });
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
  await monitor.tick();
  const got = await call('GET', '/api/hosts/laptop/load');
  assert.equal(got.status, 200);
  assert.equal(got.body.enabled, false);
  assert.equal(got.body.restart, true);
  assert.equal(got.body.load.sample.perCore, 0.4);

  const put = await call('PUT', '/api/hosts/laptop/load', { enabled: true, load: { hot: 3 } });
  assert.equal(put.status, 200);
  assert.equal(put.body.enabled, true);
  assert.equal(monitor.settings().load.hot, 3, 'applied live');
  assert.equal(sets[0].key, 'resources');
  assert.deepEqual(sets[0].value, { enabled: true, load: { hot: 3, critical: 4 }, psi: { hot: 10, critical: 10 }, enterSeconds: 60, exitSeconds: 300 });
  for (const body of [{ enabled: 'yes' }, { load: { hot: -1 } }, { load: { hot: 5, critical: 4 } }, { exitSeconds: 99999 }, []]) {
    assert.equal((await call('PUT', '/api/hosts/laptop/load', body)).status, 400, JSON.stringify(body));
  }

  const ov = await call('POST', '/api/hosts/laptop/load/override', { minutes: 30 });
  assert.equal(ov.status, 200);
  assert.ok(ov.body.load.override.until > 0);
  assert.equal((await call('POST', '/api/hosts/laptop/load/override', { minutes: 1.5 })).status, 400);
  assert.equal((await call('POST', '/api/hosts/laptop/load/override', { minutes: 0 })).body.load.override, undefined);

  assert.equal((await call('GET', '/api/hosts/laptop/load/processes')).body.totalMb, 1);
  assert.equal((await call('POST', '/api/hosts/laptop/load/restart', {})).body.restarting, true);
  assert.equal(restarted, 1);
  assert.equal((await call('DELETE', '/api/hosts/laptop/load')).status, 405);
  assert.equal((await call('GET', '/api/hosts/nowhere/load')).status, 404);
});
