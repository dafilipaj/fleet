// Session briefs: the file format (lib/brief-format.mjs), model-free extraction from a transcript
// (lib/brief-extract.mjs), the service's budget / debounce gates with a fake clock and a fake
// model (lib/briefs.mjs), the `claude -p` runner against a fake binary, the config keys and the
// HTTP routes (incl. a peer proxy). The real `claude` is never run.
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import {
  addDismissed,
  classifyUrl,
  formatResource,
  mergeGit,
  continuePrompt,
  emptyBrief,
  mergeResources,
  parseBrief,
  parseFrontmatter,
  parseModelOutput,
  parseResourceLine,
  removedResourceKeys,
  serializeBrief,
} from '../lib/brief-format.mjs';
import { createBriefExtractor, extractUrls, ingestLine, readDelta } from '../lib/brief-extract.mjs';
import { buildPrompt, createBriefStore, createBriefs, createClaudeAsk, displayPath, gitInfo } from '../lib/briefs.mjs';
import { normalizeConfig, DEFAULT_BRIEFS, briefsDir } from '../lib/config.mjs';
import { editorUrl, editorViewer, withBriefEditor } from '../lib/editor.mjs';
import { run } from '../lib/run.mjs';
import { createApi } from '../lib/api.mjs';
import { createHttpServer } from '../lib/app.mjs';
import { createFleet } from '../lib/fleet.mjs';
import { createFleetCli } from '../lib/fleet-cli.mjs';

const SID = 'aaaaaaaa-1111-2222-3333-444444444444';

function tmpdir(t, prefix = 'fleet-briefs-') {
  const d = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), prefix)));
  t.after(() => fs.rmSync(d, { recursive: true, force: true }));
  return d;
}

// ------------------------------------------------------------------ transcript fixtures

const line = (o) => `${JSON.stringify(o)}\n`;
const user = (text) => line({ type: 'user', message: { role: 'user', content: text } });
const assistantText = (text, id = `m${Math.random()}`) => line({ type: 'assistant', message: { id, role: 'assistant', stop_reason: 'end_turn', content: [{ type: 'text', text }] } });
const toolUse = (id, name, input) => line({ type: 'assistant', message: { id: `m-${id}`, role: 'assistant', stop_reason: 'tool_use', content: [{ type: 'tool_use', id, name, input }] } });
const toolResult = (id, text, extra = {}) => line({ type: 'user', message: { role: 'user', content: [{ type: 'tool_result', tool_use_id: id, content: text, ...extra }] } });

// ------------------------------------------------------------------ format

const SAMPLE = `---
session: ${SID}
host: laptop
cwd: ~/Code/project
updated: 2026-01-02T03:04:05.000Z
generatedThrough: 1234
editedAt: 2026-01-02T03:00:00.000Z
dismissed: ["https://github.com/o/r/pull/9"]
custom: kept
---
## Summary
Fixing the login flow in ~/Code/project on branch fix-login.

## Resources
- PR: [o/r#12](https://github.com/o/r/pull/12)
- File: \`src/login.ts\`
- my own note, no kind

## Todos
- [x] reproduce
- [ ] fix
`;

test('format: parse → serialise round-trips the canonical form', () => {
  const b = parseBrief(SAMPLE);
  assert.equal(b.meta.session, SID);
  assert.equal(b.meta.generatedThrough, 1234);
  assert.deepEqual(b.meta.dismissed, ['https://github.com/o/r/pull/9']);
  assert.equal(b.meta.custom, 'kept');
  assert.equal(b.summary, 'Fixing the login flow in ~/Code/project on branch fix-login.');
  assert.deepEqual(
    b.resources.map((r) => [r.kind, r.label, r.url, r.path]),
    [
      ['PR', 'o/r#12', 'https://github.com/o/r/pull/12', null],
      ['File', 'src/login.ts', null, 'src/login.ts'],
      [null, 'my own note, no kind', null, 'my own note, no kind'],
    ],
  );
  assert.deepEqual(b.todos, [{ done: true, text: 'reproduce' }, { done: false, text: 'fix' }]);
  assert.equal(serializeBrief(b), SAMPLE);
  // A legacy `## Plan` section reads as Todos and is written back as `## Todos`.
  const legacy = parseBrief(SAMPLE.replace('## Todos', '## Plan'));
  assert.deepEqual(legacy.todos, b.todos);
  assert.equal(serializeBrief(legacy), SAMPLE);
});

test('format: tolerates hand edits — no frontmatter, odd order, extra sections, * bullets, [X]', () => {
  const b = parseBrief('Some preamble\n\n## plan\n* [X] Done it\n* [ ] next\nnot a checkbox\n\n## Notes\nfree text\n\n## Summary\nhello\n');
  assert.deepEqual(b.meta, {});
  assert.equal(b.summary, 'hello');
  assert.deepEqual(b.todos, [{ done: true, text: 'Done it' }, { done: false, text: 'next' }]);
  const out = serializeBrief({ ...b, meta: { session: SID } });
  assert.match(out, /^---\nsession: [^\n]+\n---\nSome preamble\n\n## Summary\nhello\n\n## Resources\n\n## Todos\n\* \[X\] Done it\n\* \[ \] next\nnot a checkbox\n\n## Notes\nfree text\n$/);
  // The skeleton has all three sections.
  assert.equal(serializeBrief(emptyBrief(SID)), `---\nsession: ${SID}\n---\n## Summary\n\n## Resources\n\n## Todos\n`);
});

test('format: frontmatter values that would read back as another type are quoted', () => {
  const text = serializeBrief({ meta: { session: SID, cwd: '~/a #b', todos: '123', note: '[x]' } });
  const { meta } = parseFrontmatter(text);
  assert.equal(meta.cwd, '~/a #b');
  assert.equal(meta.todos, '123');
  assert.equal(meta.note, '[x]');
});

test('format: resources — classify, merge keeps hand lines, de-dupes, honours dismissed', () => {
  assert.deepEqual(classifyUrl('https://github.com/o/r/pull/12/files#diff'), { kind: 'PR', label: 'o/r#12', url: 'https://github.com/o/r/pull/12' });
  assert.equal(classifyUrl('https://github.com/o/r/issues/3').kind, 'Issue');
  assert.equal(classifyUrl('https://claude.ai/code/artifact/0123456789abcdef').kind, 'Artifact');
  assert.equal(classifyUrl('https://docs.example.dev/guide').kind, 'Link');
  assert.equal(parseResourceLine('see https://github.com/o/r/pull/5.').kind, 'PR');

  const existing = '- PR: [o/r#12](https://github.com/o/r/pull/12)\n- hand-written line';
  const { text, added } = mergeResources(
    existing,
    [
      { kind: 'PR', label: 'o/r#12', url: 'https://github.com/o/r/pull/12' },
      { kind: 'PR', label: 'o/r#9', url: 'https://github.com/o/r/pull/9' },
      { kind: 'File', path: 'src/a.ts' },
    ],
    ['https://github.com/o/r/pull/9'],
  );
  assert.equal(added, 1);
  assert.equal(text, `${existing}\n- File: \`src/a.ts\``);

  const before = parseBrief(`## Resources\n${text}\n`);
  const after = parseBrief('## Resources\n- hand-written line\n- File: `src/a.ts`\n');
  assert.deepEqual(removedResourceKeys(before, after), ['https://github.com/o/r/pull/12']);
  assert.deepEqual(addDismissed(['a', 'b'], ['a', 'c']), ['b', 'a', 'c']);
});

test('format: model output — valid, fenced, and garbage', () => {
  assert.deepEqual(parseModelOutput('## Summary\nDoing X in repo Y.\n\n## Todos\n- [x] a\n- [ ] b\nchatter\n'), {
    summary: 'Doing X in repo Y.',
    todos: [{ done: true, text: 'a' }, { done: false, text: 'b' }],
  });
  assert.deepEqual(parseModelOutput('## Summary\nOld heading.\n\n## Plan\n- [ ] a\n').todos, [{ done: false, text: 'a' }], '`## Plan` still accepted');
  assert.deepEqual(parseModelOutput('```markdown\n## Summary\nOnly this.\n```'), { summary: 'Only this.', todos: null });
  assert.equal(parseModelOutput('Sorry, I cannot help with that.'), null);
  assert.equal(parseModelOutput('## Summary\n\n## Plan\n- [ ] x'), null, 'empty summary');
  assert.equal(parseModelOutput(`## Summary\n${'x'.repeat(1300)}`), null, 'too long');
  assert.equal(parseModelOutput('## Summary\nok\n### nested heading\n'), null);
});

test('format: continue prompt carries where, the summary, origin, PRs and the worktree, then a separator', () => {
  const b = parseBrief(
    SAMPLE.replace('## Resources\n', '## Resources\n- Origin: [Task 7](https://app.example.dev/tasks/7)\n- Git: `fix-login` · worktree `~/Code/project-wt`\n- Artifact: [Report](https://claude.ai/artifact/abcdef123456)\n'),
  );
  assert.equal(
    continuePrompt(b),
    `Continue the work of session ${SID} on laptop in ~/Code/project. Its brief:\n\n` +
      'Fixing the login flow in ~/Code/project on branch fix-login.\n\n' +
      'Origin:\n- [Task 7](https://app.example.dev/tasks/7)\n\n' +
      'PRs:\n- [o/r#12](https://github.com/o/r/pull/12)\n\n' +
      'Worktree: `~/Code/project-wt` (branch `fix-login`)\n\n---',
  );
  // No origin, no PRs, the main checkout: none of those blocks.
  const plain = continuePrompt(parseBrief('## Summary\nx\n\n## Resources\n- Git: `main` · repo `~/Code/project`\n- File: `a.ts`\n'));
  assert.doesNotMatch(plain, /Origin:|PRs:|Worktree:|a\.ts/);
  assert.equal(continuePrompt(emptyBrief(SID), { host: 'workstation' }), `Continue the work of session ${SID} on workstation. Its brief:\n\n---`);
});

test('format: an Origin item turns its auto Link / Issue line into an Origin line', () => {
  const text = '- Link: [app.example.dev/tasks/7](https://app.example.dev/tasks/7)\n- Issue: hand-written https://github.com/o/r/issues/3';
  const items = [
    { kind: 'Origin', label: 'app.example.dev/tasks/7', url: 'https://app.example.dev/tasks/7' },
    { kind: 'Origin', label: 'o/r#3', url: 'https://github.com/o/r/issues/3' },
  ];
  const r = mergeResources(text, items);
  assert.equal(r.text, '- Origin: [app.example.dev/tasks/7](https://app.example.dev/tasks/7)\n- Issue: hand-written https://github.com/o/r/issues/3');
  assert.equal(r.added, 1);
});

// Shared with the Rust `core::brief` tests: both implementations must produce these bytes.
test('format: shared fixtures in testdata/briefs (pinned by the Rust tests too)', async () => {
  const dir = fileURLToPath(new URL('../../testdata/briefs/', import.meta.url));
  const read = (f) => fs.readFileSync(path.join(dir, f), 'utf8');
  const { parsedShape, humanEdit } = await import('../../testdata/briefs/gen.mjs');
  const b = parseBrief(read('sample.md'));
  assert.equal(serializeBrief(b), read('sample.canonical.md'));
  assert.deepEqual(parsedShape(b), JSON.parse(read('sample.parsed.json')));
  assert.equal(continuePrompt(b), read('sample.continue.txt'));
  assert.equal(await humanEdit(read('sample.md'), read('edit.md')), read('sample.edited.md'));
  assert.equal(await humanEdit(read('sample.md'), read('edit-frontmatter.md')), read('sample.edited-frontmatter.md'));
  assert.equal(await humanEdit(null, read('edit.md')), read('empty.edited.md'));
});

// ------------------------------------------------------------------ extraction

function transcriptFixture(t) {
  const root = tmpdir(t);
  const home = path.join(root, 'home');
  const cwd = path.join(home, 'Code', 'project');
  fs.mkdirSync(path.join(cwd, 'src'), { recursive: true });
  fs.mkdirSync(path.join(cwd, 'specs'), { recursive: true });
  for (const f of ['src/a.ts', 'src/b.ts', 'out.txt', 'specs/SPEC.md']) fs.writeFileSync(path.join(cwd, f), 'x');
  const file = path.join(root, 'session.jsonl');
  const body = [
    user('Please fix the bug from https://github.com/o/r/issues/7 and see http://localhost:3000/x\n\nContinue the work of session abc on laptop. Its brief:\n\nResources:\n- PR: https://github.com/o/r/pull/5\n- Artifact: https://claude.ai/artifact/HandedOver1234'),
    toolUse('t1', 'Read', { file_path: path.join(cwd, 'README.md') }),
    toolResult('t1', 'history: https://github.com/o/r/pull/1 https://github.com/o/r/pull/2'),
    toolUse('t2', 'Edit', { file_path: path.join(cwd, 'src', 'a.ts'), old_string: 'a', new_string: 'b' }),
    toolUse('t3', 'Write', { file_path: path.join(cwd, 'specs', 'SPEC.md'), content: '#' }),
    toolUse('t4', 'Write', { file_path: '/tmp/scratch.txt', content: 'x' }),
    toolUse('t5', 'Bash', { command: `cd ${cwd} && echo hi > out.txt` }),
    toolUse('t6', 'Bash', { command: 'gh pr create --title x --body y' }),
    toolResult('t6', 'https://github.com/o/r/pull/12\n'),
    toolUse('t7', 'Bash', { command: 'gh pr list' }),
    toolResult('t7', 'https://github.com/o/r/pull/99'),
    toolUse('t8q', 'Artifact', { action: 'quickstart', intent: 'other' }),
    toolResult('t8q', 'Start from the Dashboard type: publish with type_url "https://claude.ai/artifact/TypeDashboard1"; slides https://claude.ai/artifact/TypeSlides12'),
    toolUse('t8', 'Artifact', { file_path: '/x/report.html', title: 'Login report' }),
    toolResult('t8', 'Published: https://claude.ai/code/artifact/0123456789abcdef0123 (private)'),
    toolUse('t9', 'TodoWrite', { todos: [{ content: 'reproduce', status: 'completed' }, { content: 'fix', status: 'in_progress' }, { content: 'test', status: 'pending' }] }),
    assistantText(`Opened https://github.com/o/r/pull/12 — docs at https://docs.example.dev/auth. Schema https://json-schema.org/draft/x\n\n${'Details. '.repeat(300)}`),
  ].join('');
  fs.writeFileSync(file, body);
  return { root, home, cwd, file };
}

test('extract: resources come from what the session made, not what it read', async (t) => {
  const { home, cwd, file } = transcriptFixture(t);
  const ex = createBriefExtractor({ locate: async () => file, userHome: home, skipPath: /^\/tmp\// });
  const r = await ex.refresh({ session_id: SID, cwd });
  const got = r.resources.map((x) => `${x.kind} ${x.url ?? x.path}${x.label && x.kind === 'Artifact' ? ` (${x.label})` : ''}`).sort();
  assert.deepEqual(got, [
    'Artifact https://claude.ai/code/artifact/0123456789abcdef0123 (Login report)',
    `File ${path.join(cwd, 'out.txt')}`,
    `File ${path.join(cwd, 'src', 'a.ts')}`,
    'Link https://docs.example.dev/auth',
    'Origin https://github.com/o/r/issues/7',
    'PR https://github.com/o/r/pull/12',
    'PR https://github.com/o/r/pull/5',
    `Spec ${path.join(cwd, 'specs', 'SPEC.md')}`,
  ]);
  assert.deepEqual(r.todos, [
    { done: true, text: 'reproduce' },
    { done: false, text: 'fix (in progress)' },
    { done: false, text: 'test' },
  ]);
  assert.equal(r.offset, fs.statSync(file).size);

  // Incremental: only the growth is read; a task list replaces the todo list.
  fs.appendFileSync(
    file,
    toolUse('t10', 'TaskCreate', { subject: 'Ship it' }) + toolResult('t10', 'Task #3 created successfully') + toolUse('t11', 'TaskUpdate', { taskId: '3', status: 'completed' }) + '{"partial":',
  );
  const r2 = await ex.refresh({ session_id: SID, cwd });
  assert.deepEqual(r2.todos, [{ done: true, text: 'Ship it' }]);
  assert.ok(r2.offset < fs.statSync(file).size, 'an unfinished last line waits');
});

test('extract: origin comes from the first prompt and carries down a continue chain; the task below the separator is own text', async (t) => {
  const dir = tmpdir(t);
  const file = path.join(dir, 's.jsonl');
  const handed =
    'Continue the work of session abc on laptop. Its brief:\n\nDid things, see https://github.com/o/r/pull/3.\n\n' +
    'Origin:\n- [Task 7](https://app.example.dev/tasks/7)\n- https://chat.example.dev/archives/C1/p2\n\n' +
    'PRs:\n- [o/r#5](https://github.com/o/r/pull/5)\n\n' +
    'Worktree: `~/Code/project-wt` (branch `x`)\n\n---\n\n' +
    'Now review https://github.com/o/r/pull/8 against https://app.example.dev/tasks/9';
  fs.writeFileSync(file, user(handed) + user('also see https://chat.example.dev/archives/C1/p9') + assistantText('Mentioned https://app.example.dev/tasks/7 again.'));
  const ex = createBriefExtractor({ locate: async () => file, userHome: dir });
  const r = await ex.refresh({ session_id: SID, cwd: dir });
  const got = r.resources.map((x) => `${x.kind} ${x.url}`).sort();
  assert.deepEqual(got, [
    'Origin https://app.example.dev/tasks/7',
    'Origin https://app.example.dev/tasks/9',
    'Origin https://chat.example.dev/archives/C1/p2',
    'PR https://github.com/o/r/pull/3',
    'PR https://github.com/o/r/pull/5',
    'PR https://github.com/o/r/pull/8',
  ]);
});

test('extract: URL filter drops local, templated and schema links', () => {
  assert.deepEqual(extractUrls('a http://127.0.0.1:7777/api b https://x.io/{id} c https://www.w3.org/2000/svg d https://ok.dev/p).').map((u) => u.url), ['https://ok.dev/p']);
  const lan = 'http://192.0.2.1:7777 http://laptop:7777 http://laptop.example-tailnet.ts.net:7777 http://nas.local/x https://github.com/…/pull/1';
  assert.deepEqual(extractUrls(lan), [], 'private-network servers and truncated URLs');
});

test('readDelta: only the new conversation, clipped to maxChars, newest kept', async (t) => {
  const dir = tmpdir(t);
  const file = path.join(dir, 's.jsonl');
  fs.writeFileSync(file, user('old prompt') + assistantText('old answer'));
  const from = fs.statSync(file).size;
  fs.appendFileSync(file, user('first new') + toolUse('x', 'Bash', { command: 'ls' }) + toolResult('x', 'noise') + assistantText('A'.repeat(5000)) + user('second new') + assistantText('done'));
  const d = await readDelta(file, from, { maxChars: 12000 });
  assert.equal(d.userTurns, 2);
  assert.equal(d.messages, 4);
  assert.doesNotMatch(d.text, /old prompt|noise/);
  assert.match(d.text, /\[user\] first new[\s\S]*\[assistant\] A+ \[…\] A+[\s\S]*\[user\] second new[\s\S]*\[assistant\] done$/);
  assert.equal(d.end, fs.statSync(file).size);

  const small = await readDelta(file, from, { maxChars: 60 });
  assert.match(small.text, /^\[… \d+ earlier message\(s\) omitted\]/);
  assert.match(small.text, /\[assistant\] done$/);
  assert.ok(small.text.length < 200);
  // A shrunk / replaced transcript starts over.
  assert.equal((await readDelta(file, 10 ** 9)).userTurns, 3);
});

// ------------------------------------------------------------------ the service

function setup(t, { settings = {}, answer, sessions } = {}) {
  const fx = transcriptFixture(t);
  const dir = path.join(fx.root, 'briefs');
  let clock = Date.parse('2026-01-01T10:00:00Z');
  const logs = [];
  const asks = [];
  const state = {
    sessions: sessions ?? [{ session_id: SID, cwd: fx.cwd, status: 'idle', updated_at: clock - 5 * 60 * 1000 }],
    git: () => ({ branch: 'fix-login', toplevel: fx.cwd, linked: false, worktree: null }),
  };
  const svc = createBriefs({
    settings: { ...DEFAULT_BRIEFS, enabled: true, ...settings },
    self: 'laptop',
    store: createBriefStore({ dir }),
    extractor: createBriefExtractor({ locate: async () => fx.file, userHome: fx.home, skipPath: /^\/tmp\// }),
    listSessions: async () => state.sessions,
    ask: async (req) => {
      asks.push(req);
      return answer ? answer(req, asks.length) : '## Summary\nFixing a login bug in project on main.\n\n## Todos\n- [x] reproduce\n- [ ] fix\n';
    },
    git: async (cwd) => state.git(cwd),
    now: () => clock,
    log: (l) => logs.push(l),
    home: fx.home,
  });
  const read = () => fs.readFileSync(path.join(dir, `${SID}.md`), 'utf8');
  const grow = (text) => fs.appendFileSync(fx.file, text);
  return { ...fx, svc, dir, logs, asks, state, read, grow, advance: (ms) => (clock += ms), now: () => clock };
}

test('briefs: an idle session gets resources, its todos and one model summary', async (t) => {
  const s = setup(t);
  await s.svc.tick();
  assert.equal(s.asks.length, 1);
  assert.equal(s.asks[0].model, 'haiku');
  assert.match(s.asks[0].prompt, /Where the session runs: host laptop, directory .*project, git branch fix-login\./);
  assert.match(s.asks[0].prompt, /Answer with ONLY/);
  assert.doesNotMatch(s.asks[0].prompt, /## Todos\n- \[x\] <done todo>/, 'the session\'s todos win: the model is asked for the summary only');
  const b = parseBrief(s.read());
  assert.equal(b.summary, 'Fixing a login bug in project on main.');
  assert.equal(b.meta.host, 'laptop');
  assert.equal(b.meta.generatedThrough, fs.statSync(s.file).size);
  assert.ok(b.meta.generatedAt && b.meta.updated);
  assert.deepEqual(b.todos.map((p) => p.text), ['reproduce', 'fix (in progress)', 'test'], 'todos from TodoWrite, not the model');
  assert.ok(b.resourcesText.startsWith(`- Git: \`fix-login\` · repo \`${displayPath(s.cwd, null, s.home)}\`\n`), b.resourcesText);
  assert.match(b.resourcesText, /- File: `src\/a\.ts`/, 'paths relative to the cwd');
  assert.match(b.resourcesText, /- PR: \[o\/r#12\]\(https:\/\/github\.com\/o\/r\/pull\/12\)/);
  assert.match(b.resourcesText, /- Artifact: \[Login report\]\(https:\/\/claude\.ai\/code\/artifact\//);
  assert.ok(s.logs.some((l) => /^\[briefs\] idle aaaaaaaa: claude -p --model haiku, \d+ msg\(s\) \/ 1 user turn\(s\), \d+ chars in \(1\/12 this hour\)$/.test(l)), s.logs.join('\n'));

  // Nothing new → no call, no rewrite.
  s.advance(60 * 60 * 1000);
  const before = s.read();
  await s.svc.tick();
  assert.equal(s.asks.length, 1);
  assert.equal(s.read(), before);
});

test('briefs: gates — busy / not idle long enough, too little new, min interval, hourly cap', async (t) => {
  const s = setup(t, { settings: { maxCallsPerHour: 2 } });
  s.state.sessions[0].status = 'busy';
  await s.svc.tick();
  assert.equal(s.asks.length, 0, 'busy');
  s.state.sessions[0] = { ...s.state.sessions[0], status: 'idle', updated_at: s.now() - 30 * 1000 };
  await s.svc.tick();
  assert.equal(s.asks.length, 0, 'idle for less than idleMs');
  s.advance(31 * 1000);
  await s.svc.tick();
  assert.equal(s.asks.length, 1);

  // One more user turn and a short answer: under minNewTurns and minNewChars.
  s.advance(20 * 60 * 1000);
  s.grow(user('tiny') + assistantText('ok'));
  await s.svc.tick();
  assert.equal(s.asks.length, 0 + 1, 'too little new content');

  // Enough new content, but within minIntervalMs of the last call.
  s.grow(user('another prompt') + assistantText('x'.repeat(3000)));
  const t0 = s.now();
  s.advance(1000);
  await s.svc.tick();
  assert.equal(s.asks.length, 2, 'past the interval since the first call (20m) → called');
  s.grow(user('more') + user('and more') + assistantText('y'.repeat(3000)));
  s.advance(5 * 60 * 1000);
  await s.svc.tick();
  assert.equal(s.asks.length, 2, 'within 15 min of the last call');
  s.advance(11 * 60 * 1000);
  await s.svc.tick();
  assert.equal(s.asks.length, 2, 'hourly cap (2) reached — pending, not dropped');
  assert.ok(s.logs.some((l) => /hourly cap reached \(2\/h\)/.test(l)));
  s.advance(60 * 60 * 1000 - (s.now() - t0) + 1000);
  await s.svc.tick();
  assert.equal(s.asks.length, 3, 'the pending update runs once the window frees, with no new growth needed');
  assert.match(s.asks[2].prompt, /\[user\] more[\s\S]*\[user\] and more/);
  assert.doesNotMatch(s.asks[2].prompt, /another prompt/, 'only the delta since generatedThrough');
});

test('briefs: garbage from the model keeps the old brief; a failed call is logged', async (t) => {
  let mode = 'garbage';
  const s = setup(t, {
    answer: () => {
      if (mode === 'fail') throw new Error('`claude -p` failed: not logged in');
      return 'I am sorry, I cannot do that.';
    },
  });
  await s.svc.tick();
  const b = parseBrief(s.read());
  assert.equal(b.summary, '');
  assert.equal(b.meta.generatedThrough, undefined, 'not advanced: the content is retried later');
  assert.ok(s.logs.some((l) => /unusable answer/.test(l)));
  mode = 'fail';
  s.advance(16 * 60 * 1000);
  s.grow(user('again') + assistantText('z'.repeat(2500)));
  await s.svc.tick();
  assert.ok(s.logs.some((l) => /failed: `claude -p` failed: not logged in/.test(l)));
  assert.equal(s.svc.status().lastRun.ok, false);
});

test('briefs: human edits are authoritative — dismissed resources stay gone, summary fed back', async (t) => {
  const s = setup(t);
  await s.svc.tick();
  const b = parseBrief(s.read());
  const edited = serializeBrief({
    ...b,
    summary: 'MY OWN WORDS',
    resourcesText: b.resourcesText.split('\n').filter((l) => !l.includes('pull/12')).concat('- my hand-added line').join('\n'),
  });
  const v = await s.svc.put(SID, edited, s.state.sessions[0]);
  assert.equal(v.parsed.summary, 'MY OWN WORDS');
  assert.ok(v.editedAt);
  const stored = parseBrief(s.read());
  assert.deepEqual(stored.meta.dismissed, ['https://github.com/o/r/pull/12']);
  assert.equal(stored.meta.generatedThrough, b.meta.generatedThrough, 'machine keys kept');

  // New activity that mentions the dismissed PR again: it doesn't come back; the hand line stays.
  s.advance(16 * 60 * 1000);
  s.grow(user('status?') + user('and?') + assistantText('PR https://github.com/o/r/pull/12 is green'));
  await s.svc.tick();
  assert.equal(s.asks.length, 2);
  assert.match(s.asks[1].prompt, /The user edited it by hand/);
  assert.match(s.asks[1].prompt, /MY OWN WORDS/);
  const after = s.read();
  assert.doesNotMatch(after, /pull\/12\)/);
  assert.match(after, /- my hand-added line/);
});

test('briefs: an edit that lands while the model runs wins', async (t) => {
  let s;
  s = setup(t, {
    answer: async () => {
      s.advance(1000);
      await s.svc.put(SID, '## Summary\nedited mid-flight\n');
      return '## Summary\nmodel text\n';
    },
  });
  await s.svc.tick();
  assert.equal(parseBrief(s.read()).summary, 'edited mid-flight');
  assert.ok(s.logs.some((l) => /edited while generating/.test(l)));
});

test('briefs: gone sessions are forgotten; one call at a time; manual regenerate bypasses gates but not the cap', async (t) => {
  let release;
  const gate = new Promise((r) => (release = r));
  const s = setup(t, {
    settings: { maxCallsPerHour: 2 },
    answer: async (req, n) => {
      if (n === 1) await gate;
      return `## Summary\ncall ${n}\n\n## Plan\n- [ ] step\n`;
    },
  });
  // A second idle session in another place; both are eligible.
  const other = { session_id: 'bbbbbbbb-1111-2222-3333-444444444444', cwd: s.cwd, status: 'idle', updated_at: s.now() - 10 * 60 * 1000 };
  s.state.sessions.push(other);
  const tick = s.svc.tick();
  await new Promise((r) => setTimeout(r, 30));
  assert.equal(s.asks.length, 1);
  assert.equal(s.svc.status().generating, SID);
  // Manual regenerate of the same session while it runs: no second call.
  assert.deepEqual((({ job, ...r }) => r)(await s.svc.regenerate(s.state.sessions[0])), { started: false, queued: false, generating: true });
  release();
  await tick;
  assert.equal(s.asks.length, 2, 'the other session ran after the first finished, not alongside');

  // Right after its auto call (min interval, no new content): manual still runs … until the cap.
  await assert.rejects(s.svc.regenerate(s.state.sessions[0]), (e) => e.status === 429 && e.retryAfterMs > 0);
  s.advance(61 * 60 * 1000);
  const r = await s.svc.regenerate(s.state.sessions[0]);
  assert.equal(r.started, true);
  assert.equal((await s.svc.get(SID, s.state.sessions[0])).generating, true);
  await r.job;
  assert.equal(s.asks.length, 3);
  assert.match(s.asks[2].prompt, /The recent conversation/, 'nothing new → recap of the recent conversation');
  assert.equal(parseBrief(s.read()).summary, 'call 3');

  // Gone: dropped from memory, never processed again.
  s.state.sessions = [];
  s.grow(user('x') + user('y') + assistantText('w'.repeat(3000)));
  s.advance(60 * 60 * 1000);
  await s.svc.tick();
  assert.equal(s.asks.length, 3);
});

test('briefs: GET serves an empty skeleton, then no-model extraction', async (t) => {
  const s = setup(t);
  const empty = await s.svc.get('cccccccc-0000-0000-0000-000000000000');
  assert.equal(empty.exists, false);
  assert.equal(empty.markdown, serializeBrief(emptyBrief('cccccccc-0000-0000-0000-000000000000')));
  assert.deepEqual(empty.parsed, { summary: '', resources: [], todos: [], plan: [] });
  assert.equal(empty.absCwd, null);
  assert.equal(empty.gitRoot, null);
  assert.equal(empty.generating, false);

  const live = await s.svc.get(SID, s.state.sessions[0]);
  assert.equal(live.exists, true);
  assert.equal(s.asks.length, 0, 'GET never calls the model');
  assert.ok(live.parsed.resources.some((r) => r.kind === 'PR' && r.label === 'o/r#12'));
  assert.equal(live.parsed.todos.length, 3);
  assert.deepEqual(live.parsed.plan, live.parsed.todos, 'deprecated alias');
  assert.equal(live.absCwd, s.cwd);
  assert.equal(live.gitRoot, s.cwd);
  const git = live.parsed.resources.find((r) => r.kind === 'Git');
  assert.deepEqual([git.branch, git.linked, git.label], ['fix-login', false, 'fix-login']);
  assert.match(live.continuePrompt, /^Continue the work of session aaaaaaaa-.* on laptop in /);
  assert.deepEqual(Object.keys(live).sort(), ['absCwd', 'continuePrompt', 'editedAt', 'enabled', 'exists', 'generatedAt', 'generatedThrough', 'generating', 'gitRoot', 'host', 'id', 'markdown', 'parsed', 'updated', 'worktrees']);
  assert.deepEqual(live.worktrees, [{ path: s.cwd, display: displayPath(s.cwd, null, s.home), branch: 'fix-login', linked: false }]);
});

test('briefs: worktrees — the cwd checkout first, then where it worked and the files it wrote, deduped', async (t) => {
  const s = setup(t, { settings: { enabled: false } });
  const wt = path.join(s.cwd, '.worktrees', 'feat');
  const nested = path.join(s.cwd, 'repos', 'lib');
  const other = path.join(s.home, 'Code', 'other');
  for (const d of [path.join(wt, 'src'), path.join(nested, 'src'), other, path.join(s.cwd, '.git')]) fs.mkdirSync(d, { recursive: true });
  fs.writeFileSync(path.join(wt, '.git'), 'gitdir: ../../.git/worktrees/feat\n');
  fs.mkdirSync(path.join(nested, '.git'));
  fs.mkdirSync(path.join(other, '.git'));
  const calls = [];
  s.state.git = (dir) => {
    calls.push(dir);
    if (dir.startsWith(wt)) return { branch: 'feat', toplevel: wt, linked: true, worktree: wt };
    if (dir.startsWith(nested)) return { branch: 'main', toplevel: nested, linked: false, worktree: null };
    if (dir.startsWith(other)) return { branch: null, toplevel: other, linked: false, worktree: null };
    return { branch: 'main', toplevel: s.cwd, linked: false, worktree: null };
  };
  // A nested repo it only cd'd into (dirs), a worktree it wrote files in, a spec elsewhere.
  const md = `---\nsession: ${SID}\ncwd: ${s.cwd}\ndirs: ${JSON.stringify([s.cwd, path.join(nested, 'src')])}\n---\n## Summary\nx\n\n## Resources\n- File: \`src/a.ts\`\n- File: \`.worktrees/feat/src/b.ts\`\n- File: \`.worktrees/feat/c.ts\`\n- Spec: \`~/Code/other/SPEC.md\`\n- File: \`~/gone/x.ts\`\n`;
  const view = await s.svc.put(SID, md);
  assert.deepEqual(
    view.worktrees.map((w) => [w.display, w.branch, w.linked]),
    [
      [displayPath(s.cwd, null, s.home), 'main', false],
      [displayPath(nested, null, s.home), 'main', false],
      [displayPath(wt, null, s.home), 'feat', true],
      ['~/Code/other', null, false],
    ],
  );
  assert.equal(view.worktrees[2].path, wt);
  assert.deepEqual(calls.slice(1).sort(), [nested, other, wt].sort(), 'one git call per checkout, on its root');

  const cfg = { self: 'laptop', editor: 'vscode', sshHosts: {} };
  const body = withBriefEditor(view, 'laptop', cfg);
  assert.equal(body.worktrees[2].editorUrl, `vscode://file${wt}`);
  assert.equal(withBriefEditor(view, 'nas', cfg).worktrees[2].editorUrl, null, 'no ssh alias');
});

test('brief extraction: the working directories the transcript records, first-seen order', () => {
  const st = { items: new Map(), dirs: new Set(), pending: new Map(), tasks: new Map(), seq: 0, skipPath: /^\/tmp\// };
  for (const cwd of ['/w/a', '/w/a/wt/x', '/w/a', '/tmp/scratch', 'relative']) ingestLine(st, JSON.stringify({ type: 'user', cwd, message: { content: 'hi' } }));
  ingestLine(st, JSON.stringify({ type: 'assistant', cwd: '/w/side', isSidechain: true, message: { content: [] } }));
  assert.deepEqual([...st.dirs], ['/w/a', '/w/a/wt/x']);
});

test('briefs: buildPrompt caps the fed-back brief; displayPath', () => {
  const brief = { summary: 'S'.repeat(5000), todosText: '- [ ] a', meta: {} };
  const p = buildPrompt({ brief, delta: { text: 'conv' }, host: 'h', cwd: '/w', branch: null, todosFromSession: false, maxBriefChars: 300 });
  assert.ok(p.length < 2000);
  assert.match(p, /S \[…\]\n<\/brief>/);
  assert.match(p, /## Todos\n- \[x\] <done todo>/);
  assert.equal(displayPath('/h/u/Code/p/src/x', '/h/u/Code/p', '/h/u'), 'src/x');
  assert.equal(displayPath('/h/u/other/y', '/h/u/Code/p', '/h/u'), '~/other/y');
  assert.equal(displayPath('/opt/z', '/h/u/Code/p', '/h/u'), '/opt/z');
});

test('briefs: store writes atomically and rejects ids that are not file names', async (t) => {
  const dir = path.join(tmpdir(t), 'nested', 'briefs');
  const store = createBriefStore({ dir });
  assert.equal(await store.read(SID), null);
  await store.write(SID, 'one');
  await store.write(SID, 'two');
  assert.equal(await store.read(SID), 'two');
  assert.deepEqual(fs.readdirSync(dir), [`${SID}.md`], 'no tmp files left behind');
  assert.throws(() => store.file('../etc/passwd'), /not a session id/);
});

// ------------------------------------------------------------------ claude -p runner

function fakeClaude(t, script) {
  const dir = tmpdir(t, 'fleet-fake-claude-');
  const bin = path.join(dir, 'claude');
  fs.writeFileSync(bin, `#!/bin/sh\n${script}\n`, { mode: 0o755 });
  return { bin, dir };
}

test('claude runner: flags, prompt on stdin, neutral cwd', async (t) => {
  const { bin, dir } = fakeClaude(t, `echo "ARGS $*" > "$(dirname "$0")/args"; pwd > "$(dirname "$0")/cwd"; cat > "$(dirname "$0")/stdin"; printf '## Summary\\nok\\n'`);
  const neutral = tmpdir(t, 'fleet-neutral-');
  const ask = createClaudeAsk({ bin, run, cwd: neutral });
  assert.equal(await ask({ prompt: 'hello brief', model: 'haiku' }), '## Summary\nok\n');
  assert.equal(fs.readFileSync(path.join(dir, 'args'), 'utf8').trim(), 'ARGS -p --model haiku --no-session-persistence --strict-mcp-config --disallowed-tools Bash,Edit,Write,Read,WebFetch,WebSearch,Task');
  assert.equal(fs.readFileSync(path.join(dir, 'stdin'), 'utf8'), 'hello brief');
  assert.equal(fs.realpathSync(fs.readFileSync(path.join(dir, 'cwd'), 'utf8').trim()), fs.realpathSync(neutral));
});

test('claude runner: failure, timeout, and a claude without --no-session-persistence', async (t) => {
  const failing = fakeClaude(t, 'echo "Invalid API key · Please run /login" >&2; exit 1');
  await assert.rejects(createClaudeAsk({ bin: failing.bin, run })({ prompt: 'x' }), /`claude -p` failed: Invalid API key · Please run \/login/);

  const slow = fakeClaude(t, 'sleep 5');
  await assert.rejects(createClaudeAsk({ bin: slow.bin, run, timeoutMs: 300 })({ prompt: 'x' }), (e) => e.timedOut && /did not answer within 0s/.test(e.message));

  const old = fakeClaude(t, 'for a in "$@"; do [ "$a" = --no-session-persistence ] && { echo "error: unknown option \'--no-session-persistence\'" >&2; exit 1; }; done; cat >/dev/null; echo fine');
  const ask = createClaudeAsk({ bin: old.bin, run });
  assert.equal((await ask({ prompt: 'x' })).trim(), 'fine');
  assert.equal((await ask({ prompt: 'y' })).trim(), 'fine', 'the flag stays off');
});

test('gitInfo: branch and checkout root from one rev-parse', async () => {
  const fakeRun = (out) => async () => ({ stdout: out, stderr: '' });
  assert.deepEqual(await gitInfo('/w/p', { run: fakeRun('main\n/w/p\n.git\n.git\n') }), { branch: 'main', toplevel: '/w/p', linked: false, worktree: null });
  // In a subdirectory git prints absolute paths; relative ones are relative to the cwd.
  assert.equal((await gitInfo('/w/p/src', { run: fakeRun('main\n/w/p\n/w/p/.git\n../.git\n') })).linked, false);
  assert.deepEqual(await gitInfo('/w/p/.worktrees/x', { run: fakeRun('feat-x\n/w/p/.worktrees/x\n/w/p/.git/worktrees/x\n/w/p/.git\n') }), { branch: 'feat-x', toplevel: '/w/p/.worktrees/x', linked: true, worktree: '/w/p/.worktrees/x' });
  assert.equal((await gitInfo('/w', { run: fakeRun('HEAD\n/w\n.git\n.git\n') })).branch, null, 'detached');
  assert.equal(await gitInfo('/nope', { run: async () => { throw new Error('not a git repository'); } }), null);
});

// ------------------------------------------------------------------ config

test('config: web.briefs defaults (off), overrides, validation, env, dir', () => {
  const home = '/home/tester';
  const d = normalizeConfig({}, { env: {}, home });
  assert.deepEqual(d.briefs, { ...DEFAULT_BRIEFS, dir: '/home/tester/.local/state/fleet/briefs' });
  assert.equal(d.briefs.enabled, false);
  const c = normalizeConfig({ web: { briefs: { enabled: true, model: 'claude-haiku-4-5', minIntervalMs: 60000, maxCallsPerHour: 3 } } }, { env: {}, home });
  assert.equal(c.briefs.enabled, true);
  assert.equal(c.briefs.model, 'claude-haiku-4-5');
  assert.equal(c.briefs.minIntervalMs, 60000);
  assert.equal(c.briefs.maxCallsPerHour, 3);
  assert.equal(c.briefs.maxDeltaChars, 12000);
  assert.throws(() => normalizeConfig({ web: { briefs: { enabled: 'yes' } } }, { env: {}, home }), /briefs\.enabled/);
  assert.throws(() => normalizeConfig({ web: { briefs: { model: 'haiku; rm -rf' } } }, { env: {}, home }), /briefs\.model/);
  assert.throws(() => normalizeConfig({ web: { briefs: { maxCallsPerHour: -1 } } }, { env: {}, home }), /maxCallsPerHour/);
  assert.equal(normalizeConfig({}, { env: { FLEET_WEB_BRIEFS: '1' }, home }).briefs.enabled, true);
  assert.equal(normalizeConfig({ web: { briefs: { enabled: true } } }, { env: { FLEET_WEB_BRIEFS: 'off' }, home }).briefs.enabled, false);
  assert.equal(briefsDir({ FLEET_BRIEFS_DIR: '~/b' }, home), '/home/tester/b');
  assert.equal(briefsDir({ XDG_STATE_HOME: '/state' }, home), '/state/fleet/briefs');
});

// ------------------------------------------------------------------ HTTP

function listen(server) {
  return new Promise((resolve) => server.listen(0, '127.0.0.1', () => resolve(`http://127.0.0.1:${server.address().port}`)));
}

test('api: GET / PUT / regenerate, locally and through a peer; gone sessions keep their brief', async (t) => {
  const fx = transcriptFixture(t);
  const sessions = [{ session_id: SID, cwd: fx.cwd, status: 'idle', backend: 'tmux', handle: '%1', updated_at: 1 }];
  const asks = [];
  const briefs = createBriefs({
    settings: { ...DEFAULT_BRIEFS },
    self: 'workstation',
    store: createBriefStore({ dir: path.join(fx.root, 'briefs') }),
    extractor: createBriefExtractor({ locate: async () => fx.file, userHome: fx.home, skipPath: /^\/tmp\// }),
    ask: async (req) => (asks.push(req), '## Summary\nfrom the model\n'),
  });
  const hosts = {};
  for (const name of ['laptop', 'workstation']) {
    let handle = null;
    const server = createHttpServer({ handleApi: (req, url) => handle(req, url) });
    hosts[name] = { url: await listen(server), setHandle: (h) => (handle = h) };
    t.after(() => server.close());
  }
  for (const name of ['laptop', 'workstation']) {
    const config = normalizeConfig(
      { self: name, hosts: { laptop: { ssh: null, web: hosts.laptop.url }, workstation: { ssh: 'workstation', web: hosts.workstation.url } } },
      { env: {}, home: '/home/tester' },
    );
    const cli = createFleetCli({ run: async () => ({ stdout: JSON.stringify(name === 'workstation' ? sessions : []), stderr: '' }) });
    const api = createApi({ config, fleet: createFleet({ cli, self: name, ttlMs: 0 }), backend: {}, transcripts: {}, spawner: {}, briefs: name === 'workstation' ? briefs : null, warmFleet: false });
    hosts[name].setHandle(api);
  }
  const base = `${hosts.laptop.url}/api/hosts/workstation/sessions/${SID.slice(0, 8)}/brief`;

  let res = await fetch(base);
  assert.equal(res.status, 200);
  let body = await res.json();
  assert.equal(body.host, 'workstation');
  assert.equal(body.id, SID, 'a prefix resolves to the session id');
  assert.ok(body.parsed.resources.length > 0);
  assert.equal(body.enabled, false);
  // The peer knows its absolute path; the laptop (which received the request) builds the link
  // with ITS ssh alias for the workstation. Asked directly, the workstation opens a local folder.
  assert.equal(body.absCwd, fx.cwd);
  assert.equal(body.gitRoot, null, 'not a git checkout');
  assert.equal(body.editor, 'vscode');
  assert.equal(body.editorUrl, `vscode://vscode-remote/ssh-remote+workstation${fx.cwd}`);
  const direct = await (await fetch(`${hosts.workstation.url}/api/hosts/workstation/sessions/${SID}/brief`)).json();
  assert.equal(direct.editorUrl, `vscode://file${fx.cwd}`);
  const fleetBody = await (await fetch(`${hosts.laptop.url}/api/fleet`)).json();
  const row = fleetBody.hosts.find((h) => h.name === 'workstation').sessions[0];
  assert.equal(row.editorUrl, `vscode://vscode-remote/ssh-remote+workstation${fx.cwd}`, 'session rows carry the link too');

  res = await fetch(base, { method: 'PUT', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ markdown: '## Summary\nhand written\n\n## Plan\n- [ ] one\n' }) });
  assert.equal(res.status, 200, 'PUT bodies are proxied too');
  body = await res.json();
  assert.equal(body.parsed.summary, 'hand written');
  assert.deepEqual(body.parsed.todos, [{ done: false, text: 'one' }], 'a legacy ## Plan edit lands in todos');
  assert.match(body.markdown, /\n## Todos\n- \[ \] one\n/);
  assert.ok(body.editorUrl, 'PUT answers carry the link too');
  assert.ok(body.editedAt);
  res = await fetch(base, { method: 'PUT', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ markdown: 42 }) });
  assert.equal(res.status, 400);

  res = await fetch(`${base}/regenerate`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: '{}' });
  assert.equal(res.status, 202);
  assert.deepEqual(await res.json(), { host: 'workstation', id: SID, started: true, queued: false, generating: true });
  for (let i = 0; i < 50 && !(await (await fetch(base)).json()).parsed.summary.includes('model'); i += 1) await new Promise((r) => setTimeout(r, 20));
  assert.equal((await (await fetch(base)).json()).parsed.summary, 'from the model');
  assert.equal(asks.length, 1, 'manual works with background generation off');

  assert.equal((await fetch(`${base}/regenerate`)).status, 405);
  assert.equal((await fetch(`${hosts.laptop.url}/api/hosts/laptop/sessions/${SID}/brief`)).status, 501, 'no briefs on the laptop server');

  // The session is gone: GET by full id still serves the file; regenerate is 404.
  sessions.length = 0;
  res = await fetch(`${hosts.workstation.url}/api/hosts/workstation/sessions/${SID}/brief`);
  assert.equal(res.status, 200);
  assert.equal((await res.json()).parsed.summary, 'from the model');
  assert.equal((await fetch(`${hosts.workstation.url}/api/hosts/workstation/sessions/${SID}/brief/regenerate`, { method: 'POST', body: '{}' })).status, 404);
  assert.equal((await fetch(`${hosts.workstation.url}/api/hosts/workstation/sessions/dddddddd-0000-0000-0000-000000000000/brief`)).status, 404);

  const health = await (await fetch(`${hosts.workstation.url}/api/health`)).json();
  assert.equal(health.briefs.callsLastHour, 1);
  assert.equal(health.briefs.lastRun.ok, true);
});

// ------------------------------------------------------------------ git line

test('format: the Git line — parse, format, one key', () => {
  const r = parseResourceLine('Git: `feat-x` · worktree `~/Code/p/.worktrees/x`');
  assert.deepEqual([r.kind, r.branch, r.path, r.linked, r.label, r.key], ['Git', 'feat-x', '~/Code/p/.worktrees/x', true, 'feat-x', 'git']);
  const d = parseResourceLine('Git: detached · repo `~/Code/p`');
  assert.deepEqual([d.branch, d.path, d.linked, d.label], [null, '~/Code/p', false, '~/Code/p']);
  assert.equal(formatResource(d), 'Git: detached · repo `~/Code/p`');
  assert.equal(formatResource(r), r.text);
  const hand = parseResourceLine('git: `main` — merged, safe to delete');
  assert.deepEqual([hand.kind, hand.branch, hand.path, hand.key], ['Git', 'main', null, 'git']);
  assert.notEqual(formatResource(hand), hand.text, 'not the canonical form: a human line');
});

test('format: mergeGit — insert, replace in place, migrate legacy lines, respect edits and dismissal', () => {
  const item = { kind: 'Git', branch: 'fix', path: '~/Code/p', linked: false };
  // A fresh section: at the top.
  assert.deepEqual(mergeGit('- PR: [o/r#1](https://github.com/o/r/pull/1)', item), { text: '- Git: `fix` · repo `~/Code/p`\n- PR: [o/r#1](https://github.com/o/r/pull/1)', changed: true });
  // Same again: nothing to do.
  const once = mergeGit('', item).text;
  assert.deepEqual(mergeGit(once, item), { text: once, changed: false });
  // Branch / checkout changed: the auto line is replaced where it is.
  const moved = mergeGit(`- File: \`a\`\n${once}\n- hand note`, { ...item, branch: 'next', path: '~/Code/p/.worktrees/n', linked: true });
  assert.equal(moved.text, '- File: `a`\n- Git: `next` · worktree `~/Code/p/.worktrees/n`\n- hand note');
  // A human edited the line: theirs stays, none is added.
  const edited = '- Git: `fix` · repo `~/Code/p` (merged)';
  assert.deepEqual(mergeGit(edited, { ...item, branch: 'other' }), { text: edited, changed: false });
  // A human deleted it (dismissed): not added back.
  assert.deepEqual(mergeGit('- File: `a`', item, { dismissed: ['git'] }), { text: '- File: `a`', changed: false });
  // Legacy: the server's old Branch / Worktree lines go (the Git line takes the first one's
  // place); hand-written ones — other bullets, extra text — stay. Only before the migration.
  const legacy = '- File: `a`\n- Branch: `old`\n- Worktree: `~/Code/p`\n* Branch: `mine`\n- Branch: `x` is the base\n- Branch: `newer`';
  assert.equal(mergeGit(legacy, item, { migrated: false }).text, '- File: `a`\n- Git: `fix` · repo `~/Code/p`\n* Branch: `mine`\n- Branch: `x` is the base');
  assert.match(mergeGit(legacy, item, { migrated: true }).text, /- Branch: `old`/, 'after the migration they are hand lines');
  // Dismissed and legacy: the old lines still go, nothing is added.
  assert.equal(mergeGit('- Branch: `old`\n- File: `a`', item, { migrated: false, dismissed: ['git'] }).text, '- File: `a`');
});

test('briefs: the Git line follows the branch and worktree in place; edits and deletes stick', async (t) => {
  const s = setup(t, { settings: { enabled: false } });
  const session = s.state.sessions[0];
  const gitLines = (b) => b.resources.filter((r) => r.kind === 'Git' || r.kind === 'Branch' || r.kind === 'Worktree').map((r) => r.text);
  const root = displayPath(s.cwd, null, s.home);
  await s.svc.get(SID, session);
  assert.deepEqual(gitLines(parseBrief(s.read())), [`Git: \`fix-login\` · repo \`${root}\``]);
  assert.equal(parseBrief(s.read()).meta.git, `Git: \`fix-login\` · repo \`${root}\``);

  s.state.git = () => ({ branch: 'next', toplevel: path.join(s.cwd, 'wt'), linked: true, worktree: path.join(s.cwd, 'wt') });
  s.advance(2 * 60 * 1000); // past the git cache
  const view = await s.svc.get(SID, session);
  assert.deepEqual(gitLines(parseBrief(s.read())), [`Git: \`next\` · worktree \`${root}/wt\``], 'replaced, not appended');
  assert.equal(view.gitRoot, path.join(s.cwd, 'wt'));

  // The user annotates it: from now on it is theirs.
  const b = parseBrief(s.read());
  await s.svc.put(SID, serializeBrief({ ...b, resourcesText: b.resourcesText.replace(/^- Git: .*$/m, (l) => `${l} — review branch`) }), session);
  s.state.git = () => ({ branch: 'third', toplevel: s.cwd, linked: false, worktree: null });
  s.advance(2 * 60 * 1000);
  await s.svc.get(SID, session);
  assert.deepEqual(gitLines(parseBrief(s.read())), [`Git: \`next\` · worktree \`${root}/wt\` — review branch`]);

  // Deleted: dismissed, never added back.
  const c = parseBrief(s.read());
  await s.svc.put(SID, serializeBrief({ ...c, resourcesText: c.resourcesText.replace(/^- Git: .*\n?/m, '') }), session);
  assert.ok(parseBrief(s.read()).meta.dismissed.includes('git'));
  s.advance(2 * 60 * 1000);
  await s.svc.get(SID, session);
  assert.deepEqual(gitLines(parseBrief(s.read())), []);
});

test('briefs: legacy Branch / Worktree auto lines migrate to one Git line on the next write', async (t) => {
  const s = setup(t, { settings: { enabled: false } });
  fs.mkdirSync(s.dir, { recursive: true });
  fs.writeFileSync(
    path.join(s.dir, `${SID}.md`),
    `---\nsession: ${SID}\n---\n## Summary\nx\n\n## Resources\n- Branch: \`main\`\n- Worktree: \`~/old/wt\`\n- a note: Branch: \`main\` was rebased\n- Branch: \`fix-login\`\n\n## Plan\n- [ ] a\n`,
  );
  await s.svc.get(SID, s.state.sessions[0]);
  const b = parseBrief(s.read());
  assert.equal(b.resourcesText.split('\n')[0], `- Git: \`fix-login\` · repo \`${displayPath(s.cwd, null, s.home)}\``);
  assert.ok(!/^- (Branch|Worktree):/m.test(b.resourcesText), b.resourcesText);
  assert.match(b.resourcesText, /- a note: Branch: `main` was rebased/, 'hand lines stay');
  assert.match(s.read(), /\n## Todos\n/, 'the legacy Plan heading is rewritten');
  assert.doesNotMatch(s.read(), /## Plan/);
});

// ------------------------------------------------------------------ editor links

test('editor: links are built for the machine the browser is on', () => {
  const cfg = { self: 'workstation', editor: 'vscode', sshHosts: { laptop: 'lt' }, hostAddrs: { laptop: '100.0.0.1', workstation: '100.0.0.2' }, editorSsh: null };
  const req = (addr, host = '100.0.0.2:7777') => ({ socket: { remoteAddress: addr }, headers: { host } });
  const url = (r, host, p = '/w/p') => editorUrl(editorViewer(cfg, r, { user: 'me' }), host, p);
  // On the server's own machine (loopback, or its own tailnet address): local folders.
  assert.equal(url(req('127.0.0.1'), 'workstation'), 'vscode://file/w/p');
  assert.equal(url(req('::ffff:100.0.0.2'), 'workstation'), 'vscode://file/w/p');
  assert.equal(url(req('::1'), 'laptop'), 'vscode://vscode-remote/ssh-remote+lt/w/p');
  // From the laptop: its own sessions are local, the server's go over ssh to the address it used.
  assert.equal(url(req('100.0.0.1'), 'laptop'), 'vscode://file/w/p');
  assert.equal(url(req('::ffff:100.0.0.1'), 'workstation'), 'vscode://vscode-remote/ssh-remote+me@100.0.0.2/w/p');
  // web.editorSsh wins over the guess; an unknown machine (a phone) gets ssh links for all.
  assert.equal(editorUrl(editorViewer({ ...cfg, editorSsh: 'ws' }, req('100.0.0.1')), 'workstation', '/p'), 'vscode://vscode-remote/ssh-remote+ws/p');
  assert.equal(url(req('100.0.0.9'), 'laptop'), 'vscode://vscode-remote/ssh-remote+lt/w/p');
  assert.equal(url(req('100.0.0.9', '[fd00::1]:7777'), 'workstation'), null, 'an IPv6 literal is not a usable destination');
  // Behind a local HTTPS proxy (`tailscale serve`): the forwarded address counts, the proxy's own does not.
  const proxied = (xff, host = 'ws.tailnet.ts.net:7443') => ({ socket: { remoteAddress: '127.0.0.1' }, headers: { host, 'x-forwarded-for': xff } });
  assert.equal(url(proxied('100.0.0.1'), 'laptop'), 'vscode://file/w/p');
  assert.equal(url(proxied('100.0.0.1'), 'workstation'), 'vscode://vscode-remote/ssh-remote+me@ws.tailnet.ts.net/w/p');
  assert.equal(url(proxied('100.0.0.9, 100.0.0.1'), 'laptop'), 'vscode://file/w/p', 'the last hop is the one the proxy added');
  assert.equal(url(proxied(''), 'workstation'), 'vscode://file/w/p', 'no forwarded address: loopback');
  const remoteSpoof = { socket: { remoteAddress: '100.0.0.9' }, headers: { host: '100.0.0.2:7777', 'x-forwarded-for': '100.0.0.1' } };
  assert.equal(url(remoteSpoof, 'laptop'), 'vscode://vscode-remote/ssh-remote+lt/w/p', 'X-Forwarded-For is trusted from loopback only');
  // No request: the config as is.
  assert.equal(editorViewer(cfg, null), cfg);
});

test('editor: local folder vs Remote-SSH, cursor, off, and unusable input', () => {
  const cfg = { self: 'laptop', editor: 'vscode', sshHosts: { workstation: 'workstation', odd: 'bad alias; rm' } };
  assert.equal(editorUrl(cfg, 'laptop', '/home/u/Code/my app'), 'vscode://file/home/u/Code/my%20app');
  assert.equal(editorUrl(cfg, 'workstation', '/home/u/Code/p'), 'vscode://vscode-remote/ssh-remote+workstation/home/u/Code/p');
  assert.equal(editorUrl({ ...cfg, editor: 'cursor' }, 'workstation', '/p'), 'cursor://vscode-remote/ssh-remote+workstation/p');
  assert.equal(editorUrl({ ...cfg, editor: 'cursor' }, 'laptop', '/p'), 'cursor://file/p');
  assert.equal(editorUrl(cfg, 'nas', '/p'), null, 'no ssh alias');
  assert.equal(editorUrl(cfg, 'odd', '/p'), null, 'an alias that is not URL-safe');
  assert.equal(editorUrl({ ...cfg, editor: null }, 'laptop', '/p'), null);
  assert.equal(editorUrl(cfg, 'laptop', '~/p'), null, 'absolute paths only');
  const body = withBriefEditor({ absCwd: '/w/p/src', gitRoot: '/w/p' }, 'laptop', cfg);
  assert.equal(body.editorUrl, 'vscode://file/w/p', 'the git root wins over the cwd');
  assert.equal(body.editor, 'vscode');
  assert.equal(withBriefEditor({ absCwd: null, gitRoot: null }, 'laptop', cfg).editorUrl, null);
  // An older peer (no absCwd): the frontmatter's absolute cwd.
  assert.equal(withBriefEditor({ markdown: '---\ncwd: /w/old\n---\n' }, 'workstation', cfg).editorUrl, 'vscode://vscode-remote/ssh-remote+workstation/w/old');
  assert.equal(withBriefEditor({ markdown: '---\ncwd: ~/rel\n---\n' }, 'workstation', cfg).editorUrl, null);
});

test('config: web.editor and the ssh aliases', () => {
  const home = '/home/tester';
  const d = normalizeConfig({}, { env: {}, home });
  assert.equal(d.editor, 'vscode');
  assert.deepEqual(d.sshHosts, {});
  const c = normalizeConfig({ self: 'laptop', hosts: { laptop: { ssh: null }, workstation: { ssh: ' workstation ', web: 'http://100.x.y.z:7777' } }, web: { editor: 'cursor' } }, { env: {}, home });
  assert.equal(c.editor, 'cursor');
  assert.deepEqual(c.sshHosts, { workstation: 'workstation' });
  // A web-only peer gets its editor alias from `editorSsh`, which also wins over `ssh`.
  const e = normalizeConfig({ self: 'laptop', hosts: { laptop: { ssh: null }, office: { ssh: null, web: 'http://100.x.y.z:7777', editorSsh: 'office' }, ws: { ssh: 'ws', editorSsh: 'ws-alt' } } }, { env: {}, home });
  assert.deepEqual(e.sshHosts, { office: 'office', ws: 'ws-alt' });
  assert.equal(normalizeConfig({ web: { editor: null } }, { env: {}, home }).editor, null);
  assert.throws(() => normalizeConfig({ web: { editor: 'vim' } }, { env: {}, home }), /web\.editor/);
});
