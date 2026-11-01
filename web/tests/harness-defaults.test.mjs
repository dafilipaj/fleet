// Settings → Agents: PUT /api/hosts/:host/harness-defaults, the defaults in GET harnesses, and the
// spawn line that uses them. Two API servers (laptop + workstation); the CLI is a fake object.
import test from 'node:test';
import assert from 'node:assert/strict';

import { createApi } from '../lib/api.mjs';
import { createHttpServer } from '../lib/app.mjs';
import { createFleet } from '../lib/fleet.mjs';
import { harnessDefaultsOf, normalizeConfig } from '../lib/config.mjs';
import { applyHarnessDefaults, launchCommand } from '../lib/spawn.mjs';

function listen(server) {
  return new Promise((resolve) => server.listen(0, '127.0.0.1', () => resolve(`http://127.0.0.1:${server.address().port}`)));
}

async function call(url, method = 'GET', body) {
  const res = await fetch(url, { method, headers: body ? { 'content-type': 'application/json' } : {}, body: body ? JSON.stringify(body) : undefined });
  return { status: res.status, body: await res.json() };
}

async function startPair(t) {
  const urls = {};
  const handles = {};
  const hosts = {};
  for (const name of ['laptop', 'workstation']) {
    const server = createHttpServer({ handleApi: (req, url) => handles[name](req, url), uiDir: null });
    urls[name] = await listen(server);
    t.after(() => server.close());
  }
  for (const name of Object.keys(urls)) {
    const raw = {
      self: name,
      hosts: { laptop: { web: urls.laptop }, workstation: { ssh: 'workstation', web: urls.workstation } },
      harnesses: { codex: { command: 'codex', effort: 'low' } },
    };
    const config = normalizeConfig(raw, { env: {}, home: '/home/tester' });
    const sets = [];
    const cli = {
      list: async () => [],
      configSet: async ({ key, value }) => {
        sets.push([key, value]);
      },
      harnesses: async () => [
        { id: 'claude', label: 'Claude Code', command: 'claude', installed: true, models: [], efforts: ['low', 'high'] },
        { id: 'codex', label: 'Codex', command: 'codex', installed: true, models: [] },
      ],
    };
    const api = createApi({ config, cli, fleet: createFleet({ cli, self: name }), backend: {}, transcripts: {}, spawner: {}, fleetRefreshMs: 60_000 });
    t.after(() => api.stop());
    handles[name] = api;
    hosts[name] = { config, sets };
  }
  return { urls, hosts };
}

test('PUT harness-defaults writes both keys through the CLI, locally or on a peer, and GET harnesses shows them', async (t) => {
  const { urls, hosts } = await startPair(t);

  const before = await call(`${urls.laptop}/api/hosts/laptop/harnesses`);
  const codex = before.body.harnesses.find((h) => h.id === 'codex');
  assert.equal(codex.defaultEffort, 'low');
  assert.deepEqual(codex.efforts, ['minimal', 'low', 'medium', 'high', 'xhigh'], 'an older CLI without efforts gets the fallback');
  assert.deepEqual(before.body.harnesses.find((h) => h.id === 'claude').efforts, ['low', 'high']);

  const r = await call(`${urls.laptop}/api/hosts/workstation/harness-defaults`, 'PUT', { harness: 'claude', model: 'claude-opus-5-5', effort: 'high' });
  assert.equal(r.status, 200);
  assert.deepEqual(r.body, { host: 'workstation', harness: 'claude', model: 'claude-opus-5-5', effort: 'high' });
  assert.deepEqual(hosts.workstation.sets, [['harnesses.claude.model', 'claude-opus-5-5'], ['harnesses.claude.effort', 'high']]);
  assert.deepEqual(hosts.laptop.sets, [], 'the laptop only proxied it');
  assert.deepEqual(hosts.workstation.config.harnessDefaults.claude, { model: 'claude-opus-5-5', effort: 'high' });

  const after = await call(`${urls.laptop}/api/hosts/workstation/harnesses`);
  const claude = after.body.harnesses.find((h) => h.id === 'claude');
  assert.equal(claude.defaultModel, 'claude-opus-5-5', 'fresh from the config, not the 10-minute cache');
  assert.equal(claude.defaultEffort, 'high');

  // "" / null clear one
  const cleared = await call(`${urls.laptop}/api/hosts/laptop/harness-defaults`, 'PUT', { harness: 'codex', model: '', effort: null });
  assert.deepEqual(cleared.body, { host: 'laptop', harness: 'codex', model: null, effort: null });
  assert.deepEqual(hosts.laptop.sets, [['harnesses.codex.model', null], ['harnesses.codex.effort', null]]);
});

test('PUT harness-defaults refuses a bad harness, model or effort', async (t) => {
  const { urls, hosts } = await startPair(t);
  for (const body of [{ harness: 'gpt', model: null, effort: null }, { harness: 'claude', model: "x';id'", effort: null }, { harness: 'claude', model: null, effort: 'High!' }, { harness: 'pi', model: 5, effort: null }]) {
    const r = await call(`${urls.laptop}/api/hosts/laptop/harness-defaults`, 'PUT', body);
    assert.equal(r.status, 400, JSON.stringify(body));
  }
  assert.deepEqual(hosts.laptop.sets, []);
});

test('the spawn line takes the defaults unless the pick or the launcher sets them', () => {
  const d = { model: 'claude-opus-5-5', effort: 'high' };
  const line = (launcher, harness, model, defaults) => launchCommand({ launcher, harness, prompt: 'go', ...applyHarnessDefaults(launcher, harness, model, defaults) });
  assert.equal(line('claude', 'claude', '', d), "claude --model 'claude-opus-5-5' --effort high 'go'");
  assert.equal(line('claude', 'claude', 'claude-haiku-5-5', d), "claude --model 'claude-haiku-5-5' --effort high 'go'");
  assert.equal(line('claude --model opus --effort low', 'claude', '', d), "claude --model opus --effort low 'go'");
  assert.equal(line('codex', 'codex', '', { effort: 'xhigh' }), "codex -c model_reasoning_effort=xhigh 'go'");
  assert.equal(line('pi', 'pi', '', { model: null, effort: 'medium' }), "pi --thinking medium 'go'");
  assert.equal(line('claude', 'claude', '', {}), "claude 'go'");
});

test('harnessDefaultsOf reads leniently: bad values are dropped, never fatal', () => {
  assert.deepEqual(harnessDefaultsOf({ harnesses: { claude: { model: ' claude-opus-5-5 ', effort: 'high' }, codex: { model: 'a b', effort: 'X' }, pi: 'nope' } }), {
    claude: { model: 'claude-opus-5-5', effort: 'high' },
    codex: { model: null, effort: null },
    pi: { model: null, effort: null },
  });
});
