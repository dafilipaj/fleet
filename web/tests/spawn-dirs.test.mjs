// Settings → Start directories: GET/PUT /api/hosts/:host/spawn-dirs. Two API servers on
// ephemeral ports (laptop + workstation), each with its own temp config file and home;
// the `fleet` CLI is faked (its `config set` patches the file like the real one).
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { Readable } from 'node:stream';

import { createApi } from '../lib/api.mjs';
import { createHttpServer } from '../lib/app.mjs';
import { createFleet } from '../lib/fleet.mjs';
import { createFleetCli } from '../lib/fleet-cli.mjs';
import { normalizeConfig } from '../lib/config.mjs';
import { canonicalSpawnDirs, createSpawnDirsEditor, validateSpawnDirs, SPAWN_DIRS_LIMITS } from '../lib/spawn-dirs.mjs';

function tmpdir(t, prefix) {
  const d = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), prefix)));
  t.after(() => fs.rmSync(d, { recursive: true, force: true }));
  return d;
}

function listen(server) {
  return new Promise((resolve) => server.listen(0, '127.0.0.1', () => resolve(`http://127.0.0.1:${server.address().port}`)));
}

async function call(url, method = 'GET', body) {
  const res = await fetch(url, { method, headers: body ? { 'content-type': 'application/json' } : {}, body: body ? JSON.stringify(body) : undefined });
  return { status: res.status, body: await res.json() };
}

/** A fake `fleet` binary: `list --json` → [], `--local config set <key> <json>` → patch the file. */
function fakeRun(calls) {
  return async (bin, args, opts = {}) => {
    calls.push({ args, configFile: opts.env?.FLEET_CONFIG });
    if (args[0] === 'list') return { stdout: '[]', stderr: '' };
    if (args[0] === '--local' && args[1] === 'config' && args[2] === 'set') {
      const file = opts.env.FLEET_CONFIG;
      const raw = JSON.parse(fs.readFileSync(file, 'utf8'));
      raw[args[3]] = JSON.parse(args[4]);
      fs.writeFileSync(`${file}.tmp`, `${JSON.stringify(raw, null, 2)}\n`);
      fs.renameSync(`${file}.tmp`, file);
      return { stdout: `${args[3]} = ${args[4]}\n`, stderr: '' };
    }
    throw new Error(`unexpected fleet ${args.join(' ')}`);
  };
}

async function startPair(t) {
  const root = tmpdir(t, 'fleet-spawn-dirs-');
  const hosts = {};
  for (const name of ['laptop', 'workstation']) {
    const home = path.join(root, name, 'home');
    fs.mkdirSync(path.join(home, 'Code', 'work'), { recursive: true });
    fs.mkdirSync(path.join(home, 'brain'), { recursive: true });
    fs.writeFileSync(path.join(home, 'notes.txt'), 'x');
    hosts[name] = { home, configFile: path.join(root, name, 'config.json'), calls: [], spawns: [] };
  }
  const urls = {};
  const handles = {};
  for (const name of Object.keys(hosts)) {
    const server = createHttpServer({ handleApi: (req, url) => handles[name](req, url), uiDir: null });
    urls[name] = await listen(server);
    t.after(() => server.close());
  }
  for (const [name, h] of Object.entries(hosts)) {
    const raw = {
      version: 1,
      self: name,
      somethingNew: { keep: true },
      hosts: { laptop: { ssh: null, web: urls.laptop }, workstation: { ssh: 'workstation', web: urls.workstation } },
      spawnDirs: [
        { label: 'Work', paths: { laptop: '~/Code/work', workstation: '~/Code/work' } },
        { label: 'Brain', paths: { laptop: '~/brain', workstation: '~/brain' } },
      ],
      web: { port: 7777 },
    };
    fs.writeFileSync(h.configFile, `${JSON.stringify(raw, null, 2)}\n`);
    const config = normalizeConfig(raw, { env: {}, home: h.home });
    const cli = createFleetCli({ run: fakeRun(h.calls) });
    const spawner = { spawn: async (req) => (h.spawns.push(req), { name: 'x', dir: req.dir, tmuxSession: 'x', command: 'claude', trusted: false }) };
    const api = createApi({
      config,
      cli,
      fleet: createFleet({ cli, self: name }),
      backend: {},
      transcripts: {},
      spawner,
      spawnDirs: createSpawnDirsEditor({ config, configFile: h.configFile, cli, home: h.home }),
      fleetRefreshMs: 60_000,
    });
    t.after(() => api.stop());
    handles[name] = api;
    h.config = config;
  }
  return { hosts, urls };
}

const next = (extra = {}) => [
  { label: 'Work', paths: { laptop: '~/Code/work', workstation: '~/Code/work' } },
  { label: 'Fleet', paths: { laptop: '~/brain', workstation: '' }, ...extra },
];

test('validateSpawnDirs: labels, paths, limits, and this host\'s dirs must exist', async (t) => {
  const home = tmpdir(t, 'fleet-sd-home-');
  fs.mkdirSync(path.join(home, 'a'));
  fs.writeFileSync(path.join(home, 'file'), 'x');
  const v = (list) => validateSpawnDirs(list, { self: 'laptop', home });

  const ok = await v([{ label: ' A ', paths: { laptop: '~/a', workstation: '/elsewhere/not/here', other: '' } }]);
  assert.deepEqual(ok.errors, []);
  assert.deepEqual(ok.entries, [{ label: 'A', paths: { laptop: '~/a', workstation: '/elsewhere/not/here' } }]);
  assert.deepEqual(ok.checks, [{ path: '~/a', resolved: path.join(home, 'a'), exists: true, isDir: true }]);

  const bad = await v([
    { label: '', paths: { laptop: '~/a' } },
    { label: 'x'.repeat(SPAWN_DIRS_LIMITS.maxLabel + 1), paths: { laptop: '~/a' } },
    { label: 'Dup', paths: { workstation: '/w' } },
    { label: 'dup', paths: { workstation: '/w' } },
    { label: 'Rel', paths: { workstation: 'Code/x' } },
    { label: 'Nul', paths: { workstation: '/a\u0000b' } },
    { label: 'Missing', paths: { laptop: '~/nope' } },
    { label: 'File', paths: { laptop: '~/file' } },
    { label: 'Empty', paths: { laptop: '', workstation: '  ' } },
    { label: 'Line\nbreak', paths: { workstation: '/w' } },
  ]);
  const at = (i) => bad.errors.filter((e) => e.index === i).map((e) => e.error);
  assert.match(at(0)[0], /label is required/);
  assert.match(at(1)[0], /longer than 40/);
  assert.deepEqual(at(2), []);
  assert.match(at(3)[0], /duplicate label "dup"/);
  assert.match(at(4)[0], /absolute or start with ~/);
  assert.match(at(5)[0], /control character/);
  assert.deepEqual(bad.errors.find((e) => e.index === 6), { index: 6, field: 'paths', host: 'laptop', error: 'no such directory on laptop' });
  assert.match(at(7)[0], /not a directory on laptop/);
  assert.match(at(8)[0], /at least one host/);
  assert.match(at(9)[0], /one line/);

  const many = await v(Array.from({ length: SPAWN_DIRS_LIMITS.maxEntries + 1 }, (_, i) => ({ label: `L${i}`, paths: { w: '/w' } })));
  assert.match(many.errors[0].error, /at most 30/);
  assert.match((await v('nope')).errors[0].error, /must be an array/);
  assert.deepEqual((await v([])).errors, [], 'an empty list is fine (the host then offers Home)');
});

test('canonicalSpawnDirs spells out `{ label, path }` entries for every host', () => {
  assert.deepEqual(canonicalSpawnDirs([{ label: 'Same', path: '~/x' }, '~/Code/app', { paths: { laptop: '~/y ' } }, 7], ['laptop', 'workstation']), [
    { label: 'Same', paths: { laptop: '~/x', workstation: '~/x' } },
    { label: 'app', paths: { laptop: '~/Code/app', workstation: '~/Code/app' } },
    { label: 'y', paths: { laptop: '~/y' } },
  ]);
  assert.deepEqual(canonicalSpawnDirs(null), []);
});

test('GET spawn-dirs: the stored list + this host\'s checks, served locally or proxied once', async (t) => {
  const { hosts, urls } = await startPair(t);
  fs.rmSync(path.join(hosts.workstation.home, 'brain'), { recursive: true });
  const local = await call(`${urls.laptop}/api/hosts/laptop/spawn-dirs`);
  assert.equal(local.status, 200);
  assert.equal(local.body.host, 'laptop');
  assert.deepEqual(local.body.hosts, ['laptop', 'workstation']);
  assert.deepEqual(local.body.spawnDirs.map((d) => d.label), ['Work', 'Brain']);
  assert.deepEqual(local.body.checks.map((c) => c.isDir), [true, true]);
  assert.equal(local.body.offered[0].path, path.join(hosts.laptop.home, 'Code', 'work'));
  assert.equal(local.body.limits.maxEntries, 30);

  const peer = await call(`${urls.laptop}/api/hosts/workstation/spawn-dirs`);
  assert.equal(peer.status, 200);
  assert.equal(peer.body.host, 'workstation');
  assert.deepEqual(peer.body.checks.map((c) => c.exists), [true, false]);
  assert.equal((await call(`${urls.workstation}/api/hosts/laptop/spawn-dirs?local=1`)).status, 404, 'never chained');
  assert.equal((await call(`${urls.laptop}/api/hosts/laptop/spawn-dirs`, 'POST', {})).status, 405);
  assert.equal((await call(`${urls.laptop}/api/hosts/nope/spawn-dirs`)).status, 404);
});

test('PUT spawn-dirs writes through `fleet config set`, keeps other keys and hot-reloads', async (t) => {
  const { hosts, urls } = await startPair(t);
  const lap = hosts.laptop;
  const res = await call(`${urls.laptop}/api/hosts/laptop/spawn-dirs`, 'PUT', { spawnDirs: next() });
  assert.equal(res.status, 200, JSON.stringify(res.body));
  assert.equal(res.body.saved, true);
  assert.deepEqual(res.body.spawnDirs, [next()[0], { label: 'Fleet', paths: { laptop: '~/brain' } }]);
  assert.deepEqual(res.body.offered.map((d) => d.label), ['Work', 'Fleet']);

  const set = lap.calls.find((c) => c.args[1] === 'config');
  assert.deepEqual(set.args.slice(0, 4), ['--local', 'config', 'set', 'spawnDirs']);
  assert.equal(set.configFile, lap.configFile);
  const raw = JSON.parse(fs.readFileSync(lap.configFile, 'utf8'));
  assert.deepEqual(raw.somethingNew, { keep: true });
  assert.deepEqual(Object.keys(raw), ['version', 'self', 'somethingNew', 'hosts', 'spawnDirs', 'web']);
  assert.equal(raw.spawnDirs[1].label, 'Fleet');

  // /api/fleet advertises the new list at once (merged snapshot and ?local=1)
  const fleet = await call(`${urls.laptop}/api/fleet`);
  assert.deepEqual(fleet.body.hosts.find((h) => h.name === 'laptop').spawnDirs.map((d) => d.label), ['Work', 'Fleet']);
  const local = await call(`${urls.laptop}/api/fleet?local=1`);
  assert.deepEqual(local.body.hosts[0].spawnDirs.map((d) => d.label), ['Work', 'Fleet']);

  // the spawn allow-list follows: Brain's old path is still allowed only because Fleet points there
  const brain = path.join(lap.home, 'brain');
  assert.equal((await call(`${urls.laptop}/api/hosts/laptop/spawn`, 'POST', { dir: brain })).status, 200);
  await call(`${urls.laptop}/api/hosts/laptop/spawn-dirs`, 'PUT', { spawnDirs: [next()[0]] });
  const refused = await call(`${urls.laptop}/api/hosts/laptop/spawn`, 'POST', { dir: brain });
  assert.equal(refused.status, 400);
  assert.match(refused.body.error, /spawn dirs/);
});

test('hidden spawn dirs: kept on rewrite, allowed for spawns, not offered in /api/fleet', async (t) => {
  const { hosts, urls } = await startPair(t);
  const lap = hosts.laptop;
  const brain = path.join(lap.home, 'brain');
  const list = [
    { label: 'Work', paths: { laptop: '~/Code/work', workstation: '~/Code/work' } },
    { label: 'Brain', paths: { laptop: '~/brain', workstation: '~/brain' }, hidden: true, extra: { kept: 1 } },
  ];
  const res = await call(`${urls.laptop}/api/hosts/laptop/spawn-dirs`, 'PUT', { spawnDirs: list });
  assert.equal(res.status, 200, JSON.stringify(res.body));
  assert.deepEqual(res.body.spawnDirs, list, 'hidden and unknown keys round-trip');
  assert.deepEqual(res.body.offered.map((d) => d.label), ['Work']);
  assert.deepEqual(res.body.hiddenSpawnDirs, [{ label: 'Brain', path: brain }]);
  assert.deepEqual(JSON.parse(fs.readFileSync(lap.configFile, 'utf8')).spawnDirs, list);
  assert.deepEqual((await call(`${urls.laptop}/api/hosts/laptop/spawn-dirs`)).body.spawnDirs, list);

  // /api/fleet: the menu (`spawnDirs`) leaves it out; `hiddenSpawnDirs` lists it (merged and ?local=1)
  const fleet = await call(`${urls.laptop}/api/fleet`);
  const self = fleet.body.hosts.find((h) => h.name === 'laptop');
  assert.deepEqual(self.spawnDirs.map((d) => d.label), ['Work']);
  assert.deepEqual(self.hiddenSpawnDirs, [{ label: 'Brain', path: brain }]);
  assert.equal(fleet.body.hosts.find((h) => h.name === 'workstation').hiddenSpawnDirs, undefined, 'none hidden there');

  // still on the allow-list (an embed panel's spawn), beneath it too
  assert.equal((await call(`${urls.laptop}/api/hosts/laptop/spawn`, 'POST', { dir: brain, embed: 'brain' })).status, 200);
  // no dir → the first offered one, never a hidden one
  assert.equal((await call(`${urls.laptop}/api/hosts/laptop/spawn`, 'POST', {})).status, 200);
  assert.equal(lap.spawns.at(-1).dir, path.join(lap.home, 'Code', 'work'));

  // only hidden entries → Home is offered (and allowed), the hidden dir stays allowed
  const onlyHidden = [list[1]];
  assert.equal((await call(`${urls.laptop}/api/hosts/laptop/spawn-dirs`, 'PUT', { spawnDirs: onlyHidden })).status, 200);
  const local = await call(`${urls.laptop}/api/fleet?local=1`);
  assert.deepEqual(local.body.hosts[0].spawnDirs, [{ label: 'Home', path: lap.home }]);
  assert.equal((await call(`${urls.laptop}/api/hosts/laptop/spawn`, 'POST', { dir: brain })).status, 200);
  assert.equal((await call(`${urls.laptop}/api/hosts/laptop/spawn`, 'POST', { dir: lap.home })).status, 200);

  // un-hiding it puts it back in the menu
  await call(`${urls.laptop}/api/hosts/laptop/spawn-dirs`, 'PUT', { spawnDirs: [{ ...list[1], hidden: false }] });
  assert.deepEqual(lap.config.spawnDirs.map((d) => d.label), ['Brain']);
  assert.deepEqual(lap.config.hiddenSpawnDirs, []);
  assert.equal(JSON.parse(fs.readFileSync(lap.configFile, 'utf8')).spawnDirs[0].hidden, undefined, 'false is not stored');

  const bad = await call(`${urls.laptop}/api/hosts/laptop/spawn-dirs`, 'PUT', { spawnDirs: [{ ...list[0], hidden: 'yes' }] });
  assert.equal(bad.status, 400);
  assert.deepEqual(bad.body.errors.map((e) => e.field), ['hidden']);
});

test('PUT spawn-dirs: 400 with per-entry errors, dry runs write nothing', async (t) => {
  const { hosts, urls } = await startPair(t);
  const before = fs.readFileSync(hosts.laptop.configFile, 'utf8');
  const bad = await call(`${urls.laptop}/api/hosts/laptop/spawn-dirs`, 'PUT', {
    spawnDirs: [...next(), { label: 'work', paths: { laptop: '~/missing' } }],
  });
  assert.equal(bad.status, 400);
  assert.match(bad.body.error, /^work: duplicate label/);
  assert.deepEqual(bad.body.errors.map((e) => [e.index, e.field, e.host ?? null]), [[2, 'label', null], [2, 'paths', 'laptop']]);
  assert.equal(bad.body.checks[2].exists, false);

  const dry = await call(`${urls.laptop}/api/hosts/laptop/spawn-dirs`, 'PUT', { spawnDirs: next(), dryRun: true });
  assert.equal(dry.status, 200);
  assert.equal(dry.body.saved, false);
  assert.deepEqual(dry.body.checks.map((c) => c.isDir), [true, true]);
  assert.equal(fs.readFileSync(hosts.laptop.configFile, 'utf8'), before, 'nothing written');
  assert.equal(hosts.laptop.calls.filter((c) => c.args[1] === 'config').length, 0);
  assert.equal((await call(`${urls.laptop}/api/hosts/laptop/spawn-dirs`, 'PUT', { nope: 1 })).status, 400);
});

test('PUT spawn-dirs to a peer writes the peer\'s config and our merged fleet shows it', async (t) => {
  const { hosts, urls } = await startPair(t);
  // The workstation checks its own paths: Fleet has none there, so only Work is validated.
  const res = await call(`${urls.laptop}/api/hosts/workstation/spawn-dirs`, 'PUT', { spawnDirs: next() });
  assert.equal(res.status, 200, JSON.stringify(res.body));
  assert.equal(res.body.host, 'workstation');
  assert.deepEqual(res.body.offered.map((d) => d.label), ['Work']);
  assert.equal(JSON.parse(fs.readFileSync(hosts.workstation.configFile, 'utf8')).spawnDirs.length, 2);
  assert.equal(JSON.parse(fs.readFileSync(hosts.laptop.configFile, 'utf8')).spawnDirs[1].label, 'Brain', 'laptop untouched');
  const fleet = await call(`${urls.laptop}/api/fleet`);
  assert.deepEqual(fleet.body.hosts.find((h) => h.name === 'workstation').spawnDirs.map((d) => d.label), ['Work']);

  // a path that does not exist on the peer is rejected by the peer
  const bad = await call(`${urls.laptop}/api/hosts/workstation/spawn-dirs`, 'PUT', { spawnDirs: [{ label: 'X', paths: { workstation: '~/nope' } }] });
  assert.equal(bad.status, 400);
  assert.match(bad.body.error, /no such directory on workstation/);
});

test('PUT spawn-dirs reports a CLI failure (502) and keeps the old list', async (t) => {
  const home = tmpdir(t, 'fleet-sd-cli-');
  const configFile = path.join(home, 'config.json');
  fs.writeFileSync(configFile, '{"self":"solo","spawnDirs":[]}\n');
  const config = normalizeConfig({ self: 'solo' }, { env: {}, home });
  const cli = { configSet: async () => { throw new Error('fleet config set: not rewriting a config that doesn\'t parse'); } };
  const editor = createSpawnDirsEditor({ config, configFile, cli, home });
  const api = createApi({ config, cli, fleet: { localHost: async () => ({ name: 'solo', ok: true, sessions: [] }) }, spawnDirs: editor, warmFleet: false });
  const body = JSON.stringify({ spawnDirs: [{ label: 'Home', paths: { solo: '~' } }] });
  const req = Object.assign(Readable.from([Buffer.from(body)]), { method: 'PUT', headers: { 'content-type': 'application/json' } });
  await assert.rejects(api(req, new URL('http://x/api/hosts/solo/spawn-dirs')), (err) => err.status === 502 && /doesn't parse/.test(err.message));
  assert.deepEqual(config.spawnDirs, [{ label: 'Home', path: home }]);
});
