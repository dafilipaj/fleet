import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

import { configPath, expandHome, ghIdentitiesOf, loadConfig, normalizeConfig, resolveBinary, resolveFleetBin, spawnRoots } from '../lib/config.mjs';

function tmp() {
  return fs.mkdtempSync(path.join(os.tmpdir(), 'fleet-web-cfg-'));
}

const HOME = '/home/tester';
const quiet = { warn() {} };

const SAMPLE = {
  version: 1,
  self: 'laptop',
  defaultHost: 'workstation',
  hosts: {
    laptop: { ssh: null, web: 'http://100.x.y.z:7777/' },
    workstation: { ssh: 'workstation', web: 'http://workstation.example:7777/' },
    headless: { ssh: 'headless' },
  },
  web: { port: 7788, bind: '0.0.0.0', dir: null },
  tmux: null,
  spawnDirs: [
    { label: 'Work', paths: { laptop: '~/Code/app', workstation: '~/Code/app' } },
    { label: 'Notes', paths: { workstation: '~/Code/notes' } },
    { label: 'Shared', path: '/srv/shared' },
  ],
};

test('expandHome expands ~ and ~/ only', () => {
  assert.equal(expandHome('~', HOME), HOME);
  assert.equal(expandHome('~/a/b', HOME), `${HOME}/a/b`);
  assert.equal(expandHome('/abs', HOME), '/abs');
  assert.equal(expandHome('~other/x', HOME), '~other/x');
});

test('configPath: FLEET_CONFIG > XDG_CONFIG_HOME > ~/.config', () => {
  assert.equal(configPath({ FLEET_CONFIG: '~/x.json' }, HOME), `${HOME}/x.json`);
  assert.equal(configPath({ XDG_CONFIG_HOME: '/xdg' }, HOME), '/xdg/fleet/config.json');
  assert.equal(configPath({}, HOME), `${HOME}/.config/fleet/config.json`);
});

test('normalizeConfig maps the shared config to server settings', () => {
  const cfg = normalizeConfig(SAMPLE, { env: { PATH: '' }, home: HOME, webRoot: '/app/web' });
  assert.equal(cfg.self, 'laptop');
  assert.equal(cfg.port, 7788);
  assert.equal(cfg.bind, '0.0.0.0');
  assert.deepEqual(cfg.peers, { workstation: 'http://workstation.example:7777' }, 'self and web-less hosts are not peers');
  assert.deepEqual(cfg.hosts, ['laptop', 'workstation']);
  assert.deepEqual(cfg.spawnDirs, [
    { label: 'Work', path: `${HOME}/Code/app` },
    { label: 'Shared', path: '/srv/shared' },
  ]);
  assert.equal(cfg.uiDir, '/app/web/ui/dist');
  assert.match(cfg.tmux, /(^|\/)tmux$/, 'resolved from PATH/fallbacks or bare');
  assert.equal(cfg.quickReplies, null);
});

test('normalizeConfig: per-host spawnDirs follow self', () => {
  const cfg = normalizeConfig({ ...SAMPLE, self: 'workstation' }, { env: {}, home: HOME });
  assert.deepEqual(cfg.spawnDirs.map((d) => d.label), ['Work', 'Notes', 'Shared']);
  assert.deepEqual(cfg.peers, { laptop: 'http://100.x.y.z:7777' });
});

test('normalizeConfig: hidden spawnDirs are allowed, not offered; only hidden → Home is offered', () => {
  const cfg = normalizeConfig(
    { self: 'laptop', spawnDirs: [{ label: 'Work', paths: { laptop: '~/Code/work' } }, { label: 'Brain', paths: { laptop: '~/brain' }, hidden: true }] },
    { env: {}, home: HOME },
  );
  assert.deepEqual(cfg.spawnDirs, [{ label: 'Work', path: `${HOME}/Code/work` }]);
  assert.deepEqual(cfg.hiddenSpawnDirs, [{ label: 'Brain', path: `${HOME}/brain` }]);
  assert.deepEqual(spawnRoots(cfg), [`${HOME}/Code/work`, `${HOME}/brain`]);

  const only = normalizeConfig({ self: 'laptop', spawnDirs: [{ label: 'Brain', paths: { laptop: '~/brain' }, hidden: true }] }, { env: {}, home: HOME });
  assert.deepEqual(only.spawnDirs, [{ label: 'Home', path: HOME }]);
  assert.deepEqual(spawnRoots(only), [HOME, `${HOME}/brain`]);
  assert.deepEqual(normalizeConfig({}, { env: {}, home: HOME }).hiddenSpawnDirs, []);
});

test('normalizeConfig defaults: missing config = single loopback host', () => {
  const cfg = normalizeConfig({}, { env: {}, home: HOME, found: false });
  assert.equal(cfg.self, 'local');
  assert.equal(cfg.port, 7777);
  assert.equal(cfg.bind, '127.0.0.1');
  assert.deepEqual(cfg.peers, {});
  assert.deepEqual(cfg.spawnDirs, [{ label: 'Home', path: HOME }]);
  assert.equal(normalizeConfig({ self: 'a' }, { env: {}, home: HOME, found: true }).bind, '0.0.0.0');
});

test('normalizeConfig env overrides: FLEET_WEB_PORT/PORT, FLEET_WEB_BIND, FLEET_WEB_UI', () => {
  const env = { FLEET_WEB_PORT: '7799', FLEET_WEB_BIND: '127.0.0.1', FLEET_WEB_UI: '~/ui' };
  const cfg = normalizeConfig(SAMPLE, { env, home: HOME });
  assert.equal(cfg.port, 7799);
  assert.equal(cfg.bind, '127.0.0.1');
  assert.equal(cfg.uiDir, `${HOME}/ui`);
  assert.equal(normalizeConfig(SAMPLE, { env: { PORT: '7800' }, home: HOME }).port, 7800);
  assert.equal(normalizeConfig({ web: { ui: '/srv/ui2' } }, { env: {}, home: HOME }).uiDir, '/srv/ui2');
});

test('normalizeConfig quickReplies accepts strings and {label,text}', () => {
  const cfg = normalizeConfig({ web: { quickReplies: ['ok', { label: 'Go', text: 'Go on.' }] } }, { env: {}, home: HOME });
  assert.deepEqual(cfg.quickReplies, [
    { label: 'ok', text: 'ok' },
    { label: 'Go', text: 'Go on.' },
  ]);
  assert.throws(() => normalizeConfig({ web: { quickReplies: [{}] } }, { env: {}, home: HOME }), /quickReplies/);
});

test('normalizeConfig quickReplies accepts { label, kind, value }: text kept, key chips skipped', () => {
  const cfg = normalizeConfig(
    { web: { quickReplies: [{ label: 'Go', kind: 'text', value: 'Go on.' }, { label: 'Esc', kind: 'key', value: 'Escape' }] } },
    { env: {}, home: HOME },
  );
  assert.deepEqual(cfg.quickReplies, [{ label: 'Go', text: 'Go on.' }]);
});

test('normalizeConfig web.autoName: defaults off (opt-in) / 5 min, validated, FLEET_WEB_AUTONAME overrides', () => {
  assert.deepEqual(normalizeConfig({}, { env: {}, home: HOME }).autoName, { enabled: false, intervalMinutes: 5 });
  assert.equal(normalizeConfig({ web: { autoName: { enabled: true } } }, { env: {}, home: HOME }).autoName.enabled, true);
  assert.deepEqual(normalizeConfig({ web: { autoName: { enabled: false, intervalMinutes: 15 } } }, { env: {}, home: HOME }).autoName, {
    enabled: false,
    intervalMinutes: 15,
  });
  assert.equal(normalizeConfig({}, { env: { FLEET_WEB_AUTONAME: '0' }, home: HOME }).autoName.enabled, false);
  assert.equal(normalizeConfig({ web: { autoName: { enabled: false } } }, { env: { FLEET_WEB_AUTONAME: '1' }, home: HOME }).autoName.enabled, true);
  assert.throws(() => normalizeConfig({ web: { autoName: { intervalMinutes: 0 } } }, { env: {}, home: HOME }), /intervalMinutes/);
  assert.throws(() => normalizeConfig({ web: { autoName: { enabled: 'yes' } } }, { env: {}, home: HOME }), /autoName.enabled/);
  assert.throws(() => normalizeConfig({ web: { autoName: true } } , { env: {}, home: HOME }), /autoName/);
});

test('normalizeConfig web.models: default list, { id, label } / bare ids, strict ids', () => {
  const def = normalizeConfig({}, { env: {}, home: '/h', found: false }).models;
  assert.deepEqual(def.map((m) => m.id), ['', 'claude-fable-5-1', 'claude-opus-5-5', 'claude-sonnet-5']);
  const custom = normalizeConfig({ web: { models: [{ id: '', label: 'Auto' }, 'claude-opus-5-5[1m]'] } }, { env: {}, home: '/h' }).models;
  assert.deepEqual(custom, [{ id: '', label: 'Auto' }, { id: 'claude-opus-5-5[1m]', label: 'claude-opus-5-5[1m]' }]);
  assert.throws(() => normalizeConfig({ web: { models: 'opus' } }, { env: {}, home: '/h' }), /web.models must be an array/);
  assert.throws(() => normalizeConfig({ web: { models: [{ id: 'a b' }] } }, { env: {}, home: '/h' }), /web.models entries/);
});

test('normalizeConfig rejects broken shapes with a pointed message', () => {
  const env = {};
  assert.throws(() => normalizeConfig({ version: 2 }, { env, home: HOME }), /version/);
  assert.throws(() => normalizeConfig({ self: '' }, { env, home: HOME }), /self/);
  assert.throws(() => normalizeConfig({ hosts: [] }, { env, home: HOME }), /hosts/);
  assert.throws(() => normalizeConfig({ hosts: { b: { web: 'nope' } } }, { env, home: HOME }), /hosts\.b\.web/);
  assert.throws(() => normalizeConfig({ web: { port: 0 } }, { env, home: HOME }), /port/);
  assert.throws(() => normalizeConfig({ spawnDirs: {} }, { env, home: HOME }), /spawnDirs/);
  assert.throws(() => normalizeConfig({ spawnDirs: [{ path: 'relative' }] }, { env, home: HOME }), /absolute/);
});

test('resolveBinary: explicit > PATH > fallbacks > bare name', () => {
  const dir = tmp();
  try {
    const bin = path.join(dir, 'fleet');
    fs.writeFileSync(bin, '#!/bin/sh\n', { mode: 0o755 });
    assert.equal(resolveBinary('fleet', { explicit: '~/bin/fleet', env: {}, home: HOME }), `${HOME}/bin/fleet`);
    assert.equal(resolveBinary('fleet', { env: { PATH: dir } }), bin);
    assert.equal(resolveBinary('fleet', { env: { PATH: '' }, extraDirs: [dir] }), bin);
    assert.equal(resolveBinary('definitely-not-a-binary-xyz', { env: { PATH: dir } }), 'definitely-not-a-binary-xyz');
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test('resolveFleetBin checks ~/.local/bin and ~/.cargo/bin, FLEET_BIN wins', () => {
  const home = tmp();
  try {
    const cargo = path.join(home, '.cargo', 'bin');
    fs.mkdirSync(cargo, { recursive: true });
    fs.writeFileSync(path.join(cargo, 'fleet'), '#!/bin/sh\n', { mode: 0o755 });
    assert.equal(resolveFleetBin({}, { env: { PATH: '' }, home }), path.join(cargo, 'fleet'));
    assert.equal(resolveFleetBin({ fleetBin: '/x/fleet' }, { env: { PATH: '' }, home }), '/x/fleet');
    assert.equal(resolveFleetBin({ fleetBin: '/x/fleet' }, { env: { PATH: '', FLEET_BIN: '/y/fleet' }, home }), '/y/fleet');
  } finally {
    fs.rmSync(home, { recursive: true, force: true });
  }
});

test('loadConfig reads $FLEET_CONFIG, tolerates a missing file, rejects bad JSON', () => {
  const dir = tmp();
  try {
    const file = path.join(dir, 'config.json');
    fs.writeFileSync(file, JSON.stringify(SAMPLE));
    const cfg = loadConfig({ env: { FLEET_CONFIG: file, PATH: '' }, home: HOME, log: quiet });
    assert.equal(cfg.configFile, file);
    assert.equal(cfg.self, 'laptop');

    const missing = loadConfig({ env: { XDG_CONFIG_HOME: dir, PATH: '' }, home: HOME, log: quiet });
    assert.equal(missing.configFile, null);
    assert.equal(missing.self, 'local');

    fs.writeFileSync(file, '{ nope');
    assert.throws(() => loadConfig({ env: { FLEET_CONFIG: file }, home: HOME, log: quiet }), /not valid JSON/);
    fs.writeFileSync(file, JSON.stringify({ hosts: 1 }));
    assert.throws(() => loadConfig({ env: { FLEET_CONFIG: file }, home: HOME, log: quiet }), /invalid fleet config .*hosts/);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test('config.example.json is valid and has placeholders only', () => {
  const file = new URL('../config.example.json', import.meta.url);
  const raw = JSON.parse(fs.readFileSync(file, 'utf8'));
  const cfg = normalizeConfig(raw, { env: {}, home: HOME });
  assert.equal(cfg.self, 'laptop');
  assert.ok(Object.keys(cfg.peers).includes('workstation'));
});

test('ghIdentitiesOf keeps entries with a usable name and a command', () => {
  assert.deepEqual(ghIdentitiesOf({}), []);
  assert.deepEqual(ghIdentitiesOf({ ghIdentities: [] }), []);
  assert.deepEqual(
    ghIdentitiesOf({ ghIdentities: { bot: { command: 'gh-bot', login: 'my-app[bot]' }, plain: { command: 'x' }, nocmd: {}, 'a b': { command: 'x' } } }),
    [{ name: 'bot', login: 'my-app[bot]' }, { name: 'plain', login: 'plain' }],
  );
});
