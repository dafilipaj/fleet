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
  parsePsi,
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
