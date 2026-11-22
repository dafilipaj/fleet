import assert from 'node:assert/strict';
import { promises as fsp } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';

import { createSpawner } from '../lib/spawn.mjs';
import { DEFAULTS, USAGE_MARK, createUsageAlerts, isUsageHold, usagePatch, usageSettings } from '../lib/usage.mjs';

const tmp = () => fsp.mkdtemp(path.join(os.tmpdir(), 'fleet-usage-'));

test('usageSettings: off by default, lenient like the CLI', () => {
  assert.deepEqual(usageSettings(undefined), { enabled: false, fallback: { ...DEFAULTS.fallback }, limits: { ...DEFAULTS.limits } });
  const s = usageSettings({ enabled: true, fallback: { claude: 'pi', codex: null, pi: 'pi', bogus: 'claude' }, limits: { session: 80, weekly: 500, paceMargin: 0 } });
  assert.equal(s.enabled, true);
  assert.deepEqual(s.fallback, { claude: 'pi', codex: null, pi: 'claude' });
  assert.deepEqual(s.limits, { session: 80, weekly: 95, paceMargin: 0 });
});

test('usagePatch: validates, keeps what it does not touch', () => {
  const cur = usageSettings({});
  const next = usagePatch({ enabled: true, fallback: { codex: null }, limits: { weekly: 97 } }, cur);
  assert.deepEqual(next, { enabled: true, fallback: { claude: 'codex', codex: null, pi: 'claude' }, limits: { session: 90, weekly: 97, paceMargin: 15 } });
  for (const bad of [[], { enabled: 'yes' }, { fallback: { claude: 'claude' } }, { fallback: { gpt: 'claude' } }, { limits: { session: 0 } }, { limits: { paceMargin: 101 } }]) {
    assert.throws(() => usagePatch(bad, cur), (err) => err.status === 400, JSON.stringify(bad));
  }
});

/** A spawner whose tmux is a fake: records what it types; the agent always "starts". */
function fakeSpawner(place) {
  const typed = [];
  const opts = [];
  const run = async (_bin, args) => {
    if (args[0] === 'has-session') throw new Error('no session');
    if (args[0] === 'send-keys' && args.includes('-l')) typed.push(args.at(-1));
    if (args[0] === 'set-option') opts.push(args.slice(-2));
    if (args[0] === 'capture-pane') return { stdout: 'OpenAI Codex › for shortcuts', stderr: '' };
    if (args[0] === 'display-message') return { stdout: 'node', stderr: '' };
    return { stdout: '', stderr: '' };
  };
  const s = createSpawner({ run, place, sleep: async () => {}, promptDir: os.tmpdir() });
  return { s, typed, opts };
}

test('spawner: falls back to the harness the check names, marks the pane, drops the model', async () => {
  const dir = await tmp();
  const asked = [];
  const { s, typed, opts } = fakeSpawner(async (q) => {
    asked.push(q);
    return { enabled: true, fellBack: true, harness: 'codex', note: 'claude → codex: session 95%' };
  });
  const r = await s.spawn({ name: 'fb', dir, prompt: 'hi', model: 'claude-opus-5-5', harness: 'claude' });
  assert.deepEqual(asked, [{ harness: 'claude', model: 'claude-opus-5-5', ctx: 'manual' }]);
  assert.equal(r.harness, 'codex');
  assert.equal(r.model, null);
  assert.deepEqual(r.usage, { primary: 'claude', note: 'claude → codex: session 95%', fellBack: true });
  assert.match(typed[0], /^codex /);
  assert.doesNotMatch(typed[0], /opus/);
  assert.deepEqual(opts.find(([k]) => k === '@fleet_fallback'), ['@fleet_fallback', 'claude → codex: session 95%']);
});

test('spawner: a hold is a 409 overloaded (Slack / GitHub retry), noFallback skips the check, a failing check starts the primary', async () => {
  const dir = await tmp();
  const hold = `shed: claude (session 95%) ${USAGE_MARK} and so is codex — new sessions wait until it recovers`;
  const held = fakeSpawner(async () => ({ enabled: true, hold }));
  await assert.rejects(held.s.spawn({ name: 'h', dir, prompt: '', ctx: 'auto' }), (err) => err.status === 409 && err.overloaded && err.usage && isUsageHold(err));

  let calls = 0;
  const skip = fakeSpawner(async () => (calls++, { fellBack: true, harness: 'codex' }));
  const r = await skip.s.spawn({ name: 'n', dir, prompt: '', noFallback: true });
  assert.equal(calls, 0);
  assert.equal(r.harness, 'claude');

  const broken = fakeSpawner(async () => {
    throw new Error('fleet: no such command');
  });
  assert.equal((await broken.s.spawn({ name: 'b', dir, prompt: '' })).harness, 'claude');
});

test('createUsageAlerts: one DM per change, per host and subscription, survives a restart', async () => {
  const file = path.join(await tmp(), 'usage-alerts.json');
  const sent = [];
  let claude = { enabled: true, primaryState: { provider: 'claude', exhausted: false, reasons: [] } };
  const check = async (host, harness) =>
    harness === 'claude' ? claude : { enabled: true, primaryState: { provider: 'openai', exhausted: false, reasons: [] } };
  const make = () => createUsageAlerts({ hosts: () => ['shed'], check, notify: async (t) => sent.push(t), file });
  const a = make();
  await a.tick();
  assert.equal(sent.length, 0, 'ok from the start: nothing to say');
  claude = {
    enabled: true,
    primaryState: { provider: 'claude', exhausted: true, reasons: ['session 93%'] },
    fallback: { harness: 'codex', usable: true, state: { exhausted: false } },
  };
  await a.tick();
  await a.tick();
  assert.equal(sent.length, 1);
  assert.match(sent[0], /\*shed\*'s Claude subscription is at its limit \(session 93%\)\. New sessions on it start on codex instead/);
  await make().tick();
  assert.equal(sent.length, 1, 'a restart does not repeat it');
  claude = { enabled: true, primaryState: { provider: 'claude', exhausted: false, reasons: [] } };
  await make().tick();
  assert.equal(sent.length, 2);
  assert.match(sent[1], /below its limit again/);
  claude = { enabled: false };
  await make().tick();
  assert.equal(sent.length, 2, 'check off: no message');
});
