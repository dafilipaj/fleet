import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, readdirSync, readFileSync, realpathSync, rmSync, symlinkSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { createSpawner, isWithin, launchCommand, paneTail, resolveAllowedDir, sanitizeName, shq, shqTyped, validateSpawnRequest } from '../lib/spawn.mjs';

test('shq quotes like the fleet CLI', () => {
  assert.equal(shq('a b'), "'a b'");
  assert.equal(shq("it's"), "'it'\\''s'");
});

test('sanitizeName mirrors fleet tmux new', () => {
  assert.equal(sanitizeName('  Fix Login Bug! '), 'fix-login-bug');
  assert.equal(sanitizeName('---'), '');
  assert.equal(sanitizeName('x'.repeat(60)).length, 40);
});

test('launchCommand builds claude -n name [prompt]', () => {
  assert.equal(launchCommand({ name: 'job', prompt: '' }), "claude -n 'job'");
  assert.equal(launchCommand({ name: null, prompt: 'go' }), "claude 'go'");
  assert.equal(launchCommand({ launcher: 'cc', name: 'job', prompt: "do it's" }), "cc -n 'job' 'do it'\\''s'");
});

test('launchCommand types a multi-line prompt as one $\'…\' line (raw newlines get dropped)', () => {
  assert.equal(shqTyped('one line'), "'one line'");
  assert.equal(shqTyped("a\nit's \\ b\tc"), "$'a\\nit\\'s \\\\ b\\tc'");
  assert.equal(launchCommand({ name: null, prompt: 'Testiram.\nMulti line.\n\nPrompt.' }), "claude $'Testiram.\\nMulti line.\\n\\nPrompt.'");
});

test('launchCommand adds --model <id> (quoted) before -n', () => {
  assert.equal(launchCommand({ name: 'job', prompt: 'go', model: 'claude-haiku-4-5-20251001' }), "claude --model 'claude-haiku-4-5-20251001' -n 'job' 'go'");
  assert.equal(launchCommand({ launcher: 'cc', model: 'claude-opus-5-5[1m]' }), "cc --model 'claude-opus-5-5[1m]'");
  assert.equal(launchCommand({ name: null, prompt: '', model: '' }), 'claude', "'' = Claude's default, no flag");
});

test('validateSpawnRequest accepts a model id and rejects anything shell-shaped', () => {
  const opts = { spawnDirs: ['/tmp'] };
  assert.equal(validateSpawnRequest({ model: 'claude-sonnet-5' }, opts).model, 'claude-sonnet-5');
  assert.equal(validateSpawnRequest({ model: 'claude-opus-5-5[1m]' }, opts).model, 'claude-opus-5-5[1m]');
  assert.equal(validateSpawnRequest({}, opts).model, '');
  assert.equal(validateSpawnRequest({ model: '' }, opts).model, '');
  for (const bad of ["x'; rm -rf ~", 'a b', '$(id)', 'a;b', '`id`', 'a\nb', 'x'.repeat(101), 42, { id: 'x' }]) {
    const r = validateSpawnRequest({ model: bad }, opts);
    assert.equal(r.ok, false, `rejects ${JSON.stringify(bad)}`);
    assert.match(r.error, /model must be/);
  }
});

test('validateSpawnRequest defaults, sanitizes and rejects bad input', () => {
  const ok = validateSpawnRequest({ name: 'My Job', prompt: 'hi' }, { spawnDirs: ['/tmp'] });
  assert.equal(ok.ok, true);
  assert.equal(ok.name, 'my-job');
  assert.equal(ok.dir, '/tmp');
  const auto = validateSpawnRequest({}, { spawnDirs: ['/tmp'] });
  assert.match(auto.name, /^fw-\d{6}$/);
  assert.equal(auto.nameGiven, false);
  assert.equal(ok.nameGiven, true);
  assert.equal(validateSpawnRequest({ name: '!!!' }, { spawnDirs: ['/tmp'] }).nameGiven, false, 'nothing usable typed');
  assert.equal(validateSpawnRequest({ dir: 'relative' }).ok, false);
  assert.equal(validateSpawnRequest({ prompt: 'x'.repeat(8001) }, { spawnDirs: ['/tmp'] }).ok, false);
  assert.equal(validateSpawnRequest({}, { spawnDirs: [] }).ok, false);
});

test('spawner creates the tmux session then types the launcher', async () => {
  const calls = [];
  const run = async (bin, args) => {
    calls.push([bin, ...args]);
    if (args[0] === 'has-session') throw new Error('no such session');
    return { stdout: '', stderr: '' };
  };
  const sp = createSpawner({ run, tmux: 'tmux', launcher: 'claude', sleep: async () => {} });
  const res = await sp.spawn({ name: 'job', dir: '/tmp', prompt: 'go' });
  assert.equal(res.tmuxSession, 'job');
  assert.deepEqual(calls[0], ['tmux', 'has-session', '-t', '=job']);
  assert.deepEqual(calls[1], ['tmux', 'new-session', '-d', '-s', 'job', '-x', '200', '-y', '50', '-c', '/tmp', '-e', 'DISABLE_AUTO_UPDATE=true']);
  assert.deepEqual(calls[2], ['tmux', 'capture-pane', '-p', '-t', 'job:'], 'waits for the shell prompt');
  const keys = calls.filter((c) => c[1] === 'send-keys');
  assert.deepEqual(keys[0], ['tmux', 'send-keys', '-t', 'job:', '-l', '--', "claude -n 'job' 'go'"]);
  assert.deepEqual(keys[1], ['tmux', 'send-keys', '-t', 'job:', 'Enter']);
  assert.equal(res.trusted, false);
  assert.equal(res.model, null);
});

test('spawner types --model when one was picked', async () => {
  const calls = [];
  const run = async (bin, args) => {
    calls.push([bin, ...args]);
    if (args[0] === 'has-session') throw new Error('no such session');
    return { stdout: '', stderr: '' };
  };
  const sp = createSpawner({ run, tmux: 'tmux', launcher: 'claude', sleep: async () => {} });
  const res = await sp.spawn({ name: 'fw-101010', dir: '/tmp', prompt: 'go', nameGiven: false, model: 'claude-haiku-4-5-20251001' });
  assert.deepEqual(calls.find((c) => c[1] === 'send-keys'), ['tmux', 'send-keys', '-t', 'fw-101010:', '-l', '--', "claude --model 'claude-haiku-4-5-20251001' 'go'"]);
  assert.equal(res.model, 'claude-haiku-4-5-20251001');
});

test('codex and pi: their launcher, --model, -n for pi only; a bad harness is refused', () => {
  assert.equal(launchCommand({ launcher: 'codex', harness: 'codex', name: 'job', model: 'gpt-6', prompt: 'go' }), "codex --model 'gpt-6' 'go'");
  assert.equal(launchCommand({ launcher: 'pi', harness: 'pi', name: 'job', model: 'openai/gpt-5.5:high', prompt: '' }), "pi --model 'openai/gpt-5.5:high' -n 'job'");
  const ok = validateSpawnRequest({ dir: '/tmp', harness: 'pi', model: 'openai/gpt-5.5' });
  assert.equal(ok.ok, true);
  assert.equal(ok.harness, 'pi');
  assert.equal(validateSpawnRequest({ dir: '/tmp' }).harness, 'claude');
  assert.equal(validateSpawnRequest({ dir: '/tmp', harness: 'bash' }).ok, false);
});

test('spawner launches codex with its configured command and accepts its trust prompt', async () => {
  const calls = [];
  let captures = 0;
  const run = async (bin, args) => {
    calls.push([bin, ...args]);
    if (args[0] === 'has-session') throw new Error('no such session');
    if (args[0] === 'capture-pane') {
      captures += 1;
      return { stdout: 'Trust this folder? Codex can read…\n› 1. Trust and continue\n  2. Back' };
    }
    return { stdout: '', stderr: '' };
  };
  const sp = createSpawner({ run, launcher: 'claude', launchers: { codex: 'cx' }, sleep: async () => {} });
  const res = await sp.spawn({ name: 'job', dir: '/tmp', prompt: 'go', harness: 'codex' });
  assert.deepEqual(calls.find((c) => c[1] === 'send-keys'), ['tmux', 'send-keys', '-t', 'job:', '-l', '--', "cx 'go'"]);
  assert.equal(res.harness, 'codex');
  assert.equal(res.trusted, true);
  assert.ok(captures >= 2);
  assert.deepEqual(calls.at(-1), ['tmux', 'send-keys', '-t', 'job:', 'Enter']);
});

test('spawner passes a prompt too long to type via a file', async (t) => {
  const promptDir = mkdtempSync(path.join(os.tmpdir(), 'fleet-prompts-'));
  t.after(() => rmSync(promptDir, { recursive: true, force: true }));
  const calls = [];
  const run = async (bin, args) => {
    calls.push(args);
    if (args[0] === 'has-session') throw new Error('no such session');
    return { stdout: '', stderr: '' };
  };
  const prompt = `Skeniraj šta je radila.\n\n${'č'.repeat(600)}\nit's done`;
  const sp = createSpawner({ run, sleep: async () => {}, promptDir });
  const res = await sp.spawn({ name: 'job', dir: '/tmp', prompt });
  const [file] = readdirSync(promptDir);
  assert.match(file, /^\d{8}-\d{6}-job\.md$/);
  const full = path.join(promptDir, file);
  assert.equal(readFileSync(full, 'utf8'), prompt);
  assert.equal(res.command, `claude -n 'job' "$(cat '${full}')"`);
  assert.deepEqual(calls.find((a) => a[0] === 'send-keys'), ['send-keys', '-t', 'job:', '-l', '--', res.command]);
});

test('spawner accepts the folder-trust prompt when it appears', async () => {
  const calls = [];
  let captures = 0;
  const run = async (bin, args) => {
    calls.push(args);
    if (args[0] === 'has-session') throw new Error('no such session');
    if (args[0] === 'capture-pane') {
      captures += 1;
      if (captures < 2) return { stdout: 'starting…', stderr: '' };
      const downs = calls.filter((a) => a[0] === 'send-keys' && a[a.length - 1] === 'Down').length;
      return { stdout: downs === 0 ? '❯ No, exit\n  Yes, I trust this folder\n' : '  No, exit\n❯ Yes, I trust this folder\n', stderr: '' };
    }
    return { stdout: '', stderr: '' };
  };
  const sp = createSpawner({ run, sleep: async () => {} });
  const res = await sp.spawn({ name: 'job', dir: '/tmp', prompt: '' });
  assert.equal(res.trusted, true);
  const keys = calls.filter((a) => a[0] === 'send-keys').map((a) => a[a.length - 1]);
  assert.deepEqual(keys, ["claude -n 'job'", 'Enter', 'Down', 'Enter']);
});

test('spawner fails with the pane tail and kills the session when Claude never starts', async () => {
  const calls = [];
  const run = async (bin, args) => {
    calls.push(args);
    if (args[0] === 'has-session') throw new Error('no such session');
    if (args[0] === 'display-message') return { stdout: '-zsh\n', stderr: '' };
    if (args[0] === 'capture-pane') return { stdout: "➜  work claude 'go\nquote> \n\n", stderr: '' };
    return { stdout: '', stderr: '' };
  };
  const sp = createSpawner({ run, sleep: async () => {} });
  await assert.rejects(sp.spawn({ name: 'job', dir: '/tmp', prompt: 'go' }), (e) => {
    assert.equal(e.status, 502);
    assert.equal(e.message, "Claude did not start:\n➜  work claude 'go\nquote>");
    return true;
  });
  assert.deepEqual(calls.at(-1), ['kill-session', '-t', '=job']);
});

test('spawner leaves a session alone once something other than the shell runs in it', async () => {
  const calls = [];
  const run = async (bin, args) => {
    calls.push(args);
    if (args[0] === 'has-session') throw new Error('no such session');
    if (args[0] === 'display-message') return { stdout: '2.1.285\n', stderr: '' };
    return { stdout: 'loading…', stderr: '' };
  };
  const sp = createSpawner({ run, sleep: async () => {} });
  await sp.spawn({ name: 'job', dir: '/tmp', prompt: 'go' });
  assert.equal(calls.some((a) => a[0] === 'kill-session'), false);
});

test('spawner types only once the shell prompt has drawn and settled', async () => {
  const calls = [];
  const screens = ['', '', 'Last login: …', '➜  work', '➜  work'];
  const run = async (bin, args) => {
    calls.push(args[0]);
    if (args[0] === 'has-session') throw new Error('no such session');
    if (args[0] === 'capture-pane' && !calls.includes('send-keys')) return { stdout: screens.shift() ?? '➜  work', stderr: '' };
    return { stdout: '', stderr: '' };
  };
  const sp = createSpawner({ run, sleep: async () => {} });
  await sp.spawn({ name: 'job', dir: '/tmp', prompt: 'go' });
  assert.equal(calls.indexOf('send-keys'), 2 + 5, 'five polls: blank, blank, login banner, prompt, prompt again');
});

test('paneTail keeps the last non-empty lines, capped', () => {
  assert.equal(paneTail('a\n\nb\nc\nd\ne\n\n'), 'b\nc\nd\ne');
  assert.equal(paneTail('x'.repeat(250), { width: 10 }), `${'x'.repeat(10)}…`);
});

test('spawner refuses an existing tmux session and a missing dir', async () => {
  const run = async () => ({ stdout: '', stderr: '' });
  const sp = createSpawner({ run, sleep: async () => {} });
  await assert.rejects(sp.spawn({ name: 'job', dir: '/tmp', prompt: '' }), (e) => e.status === 409);
  await assert.rejects(sp.spawn({ name: 'job', dir: '/definitely/not/here', prompt: '' }), (e) => e.status === 400);
});

test('isWithin is path-segment aware', () => {
  assert.equal(isWithin('/a/b', '/a/b'), true);
  assert.equal(isWithin('/a/b/c', '/a/b'), true);
  assert.equal(isWithin('/a/bc', '/a/b'), false);
  assert.equal(isWithin('/a', '/a/b'), false);
});

test('resolveAllowedDir allows roots and subdirs, rejects .. and symlink escapes', async (t) => {
  const base = realpathSync(mkdtempSync(path.join(os.tmpdir(), 'fleet-spawn-')));
  t.after(() => rmSync(base, { recursive: true, force: true }));
  const root = path.join(base, 'root');
  const outside = path.join(base, 'outside');
  mkdirSync(path.join(root, 'sub'), { recursive: true });
  mkdirSync(outside);
  symlinkSync(outside, path.join(root, 'link-out'));
  symlinkSync(path.join(root, 'sub'), path.join(base, 'link-in'));

  assert.equal(await resolveAllowedDir(root, [root]), root);
  assert.equal(await resolveAllowedDir(path.join(root, 'sub'), [root]), path.join(root, 'sub'));
  assert.equal(await resolveAllowedDir(path.join(base, 'link-in'), [root]), path.join(root, 'sub'), 'symlink into a root is fine');
  const bad = (e) => e.status === 400;
  await assert.rejects(resolveAllowedDir(outside, [root]), bad);
  await assert.rejects(resolveAllowedDir(path.join(root, '..', 'outside'), [root]), bad);
  await assert.rejects(resolveAllowedDir(path.join(root, 'link-out'), [root]), bad);
  await assert.rejects(resolveAllowedDir(path.join(root, 'missing'), [root]), bad);
  await assert.rejects(resolveAllowedDir(root, [path.join(base, 'nope')]), bad, 'a missing root allows nothing');
  // No spawnDirs: $HOME subtree only.
  assert.equal(await resolveAllowedDir(path.join(root, 'sub'), [], { home: base }), path.join(root, 'sub'));
  await assert.rejects(resolveAllowedDir('/', [], { home: base }), bad);
});

test('spawner omits -n for an auto-generated name', async () => {
  const calls = [];
  const run = async (bin, args) => {
    calls.push(args);
    if (args[0] === 'has-session') throw new Error('no such session');
    return { stdout: '', stderr: '' };
  };
  const sp = createSpawner({ run, sleep: async () => {} });
  const res = await sp.spawn({ name: 'fw-101010', dir: '/tmp', prompt: 'go', nameGiven: false });
  const typed = calls.find((a) => a[0] === 'send-keys' && a.includes('-l'));
  assert.equal(typed.at(-1), "claude 'go'");
  assert.equal(res.tmuxSession, 'fw-101010');
});
