// The inbox (lib/inbox.mjs): items from each source, the Slack + mail scan's answer, the
// brief's budget, the config keys, and /api/inbox served or proxied. No model, no network.
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

import { normalizeConfig } from '../lib/config.mjs';
import {
  checkInboxSettings,
  cleanBrief,
  isDismissed,
  linkBrief,
  commsItems,
  commsPrompt,
  createAgentAsk,
  createInbox,
  DEFAULT_SCAN_RULES,
  recordUsage,
  usageSummary,
  extractJsonArray,
  inWorkHours,
  prItems,
  sessionItems,
  sortItems,
  taskItems,
} from '../lib/inbox.mjs';

const HOME = '/home/tester';
const pr = (over) => ({
  column: 'waitingOnMe', owner: 'acme', repo: 'app', number: 1, title: 'Fix it', url: 'https://github.com/acme/app/pull/1',
  author: 'someone', draft: false, updatedAt: 1000, additions: 20, deletions: 5, changedFiles: 2, size: 'XS',
  checks: 'success', reviewDecision: 'reviewRequired', ...over,
});

test('prItems: small reviews and approved PRs are quick wins, big reviews wait, drafts and others are left out', () => {
  const items = prItems({
    prs: [
      pr({ number: 1, url: 'u1' }),
      pr({ number: 2, url: 'u2', additions: 400, changedFiles: 12, size: 'L' }),
      pr({ number: 3, url: 'u3', column: 'readyToMergeMine' }),
      pr({ number: 4, url: 'u4', column: 'reviewMine', reviewDecision: 'changesRequested' }),
      pr({ number: 5, url: 'u5', column: 'reviewMine' }),
      pr({ number: 6, url: 'u6', column: 'draftMine' }),
      pr({ number: 7, url: 'u7', column: 'unseenMentions', mention: { at: 5, author: 'ana', url: 'u7#c', excerpt: '@me?' } }),
    ],
  });
  const by = Object.fromEntries(items.map((i) => [i.meta.number, i]));
  assert.deepEqual(Object.keys(by).map(Number), [1, 2, 3, 4, 7]);
  assert.equal(by[1].section, 'quick');
  assert.equal(by[2].section, 'waiting');
  assert.equal(by[2].effort, 'deep');
  assert.equal(by[3].section, 'quick');
  assert.equal(by[4].ask, 'Changes requested');
  assert.equal(by[7].kind, 'mention');
  assert.equal(by[7].url, 'u7#c');
});

test('taskItems: due and overdue tasks, recent tasks in progress', () => {
  const now = new Date(2026, 9, 20, 10).getTime();
  const items = taskItems(
    {
      tasks: [
        { id: '1', title: 'A', url: 'x', column: 'due', dueDate: '2026-10-19', status: { name: 'Doing' }, project: { name: 'P' } },
        { id: '2', title: 'B', url: 'x', column: 'due', dueDate: '2026-10-20', status: { name: 'Doing' } },
        { id: '3', title: 'C', url: 'x', column: 'inProgress', lastActivityAt: now - 3600_000, status: { name: 'Doing' } },
        { id: '4', title: 'D', url: 'x', column: 'inProgress', lastActivityAt: now - 30 * 24 * 3600_000 },
        { id: '5', title: 'E', url: 'x', column: 'toDo' },
      ],
    },
    now,
  );
  assert.deepEqual(items.map((i) => [i.id, i.section]), [['task:1', 'waiting'], ['task:2', 'mine'], ['task:3', 'mine']]);
  assert.match(items[1].ask, /^Due today/);
});

test('sessionItems: waiting sessions on every reachable host, never the operator', () => {
  const items = sessionItems({
    hosts: [
      { name: 'laptop', ok: true, sessions: [{ session_id: 'a', status: 'waiting', display_title: 'fix-login' }, { session_id: 'b', status: 'busy' }, { session_id: 'c', status: 'waiting', operator: true }] },
      { name: 'workstation', ok: false, sessions: [{ session_id: 'd', status: 'waiting' }] },
    ],
  });
  assert.equal(items.length, 1);
  assert.equal(items[0].href, '#/s/laptop/a');
  assert.equal(items[0].section, 'quick');
});

test('sessionItems: a shared host is left out', () => {
  const items = sessionItems({
    hosts: [
      { name: 'laptop', ok: true, sessions: [{ session_id: 'a', status: 'waiting' }] },
      { name: 'shed', ok: true, shared: true, sessions: [{ session_id: 'b', status: 'waiting' }] },
    ],
  });
  assert.deepEqual(items.map((i) => i.meta.host), ['laptop']);
});

test('the scan answer: JSON array found in chatter, bad entries dropped, ids namespaced, mail links filled', () => {
  const raw = extractJsonArray('Here you go:\n[{"source":"slack","id":"C1:1.2","url":"https://x.slack.com/archives/C1/p12","title":"Q","effort":"quick","at":"2026-10-20T10:00:00Z"},{"source":"email","id":"abc","title":"Sign this","effort":"weird"},{"source":"fax","id":"z","title":"?"},{"source":"slack","id":"slack:C1:1.2","title":"dup"}]');
  const items = commsItems(raw, 7);
  assert.deepEqual(items.map((i) => i.id), ['slack:C1:1.2', 'email:abc']);
  assert.equal(items[0].section, 'quick');
  assert.equal(items[1].effort, 'medium');
  assert.equal(items[1].section, 'waiting');
  assert.match(items[1].url, /mail\.google\.com.*abc$/);
  assert.equal(items[1].at, 7);
  assert.equal(extractJsonArray('no array'), null);
});

test('cleanBrief keeps only bullets', () => {
  assert.equal(cleanBrief('Sure!\n```markdown\n- one\n- two\n```\nBye'), '- one\n- two');
  assert.equal(cleanBrief('nothing'), null);
});

test('sortItems: section order, newest first', () => {
  const s = sortItems([{ id: 'a', section: 'mine', at: 9 }, { id: 'b', section: 'quick', at: 1 }, { id: 'c', section: 'quick', at: 5 }]);
  assert.deepEqual(s.map((i) => i.id), ['c', 'b', 'a']);
});

test('inWorkHours', () => {
  const s = { workHours: [8, 19], workDays: [1, 2, 3, 4, 5] };
  assert.equal(inWorkHours(new Date(2026, 9, 20, 9).getTime(), s), true); // Tuesday
  assert.equal(inWorkHours(new Date(2026, 9, 20, 19).getTime(), s), false);
  assert.equal(inWorkHours(new Date(2026, 9, 18, 10).getTime(), s), false); // Sunday
});

test('config: inbox runs only on inbox.host, with its knobs checked', () => {
  const base = { version: 1, self: 'workstation', hosts: { workstation: {}, laptop: { web: 'http://100.x.y.z:7777' } } };
  const on = normalizeConfig({ ...base, inbox: { host: 'workstation', scanMinutes: 20, language: 'Croatian' } }, { env: {}, home: HOME });
  assert.equal(on.inbox.enabled, true);
  assert.equal(on.inbox.scanMinutes, 20);
  assert.equal(on.inbox.language, 'Croatian');
  assert.equal(normalizeConfig({ ...base, inbox: { host: 'laptop' } }, { env: {}, home: HOME }).inbox.enabled, false);
  assert.equal(normalizeConfig(base, { env: {}, home: HOME }).inbox.enabled, false);
  assert.equal(normalizeConfig({ ...base, inbox: { host: 'workstation' } }, { env: { FLEET_WEB_INBOX: '0' }, home: HOME }).inbox.enabled, false);
  assert.throws(() => normalizeConfig({ ...base, inbox: { workHours: [19, 8] } }, { env: {}, home: HOME }), /workHours/);
});

const flush = () => new Promise((r) => setImmediate(r));

function setup({ now, agentText = '[]' } = {}) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'fleet-inbox-'));
  const calls = { agent: 0, ask: 0, prompts: [] };
  const clock = { t: now };
  const inbox = createInbox({
    settings: { host: 'workstation', scanMinutes: 30, workHours: [8, 19], workDays: [1, 2, 3, 4, 5], model: 'haiku', briefModel: 'haiku', briefMinutes: 45, language: 'English', comms: true },
    cli: { prs: async () => ({ prs: [pr({})] }), tasks: async () => { throw new Error('no token'); } },
    getFleet: async () => ({ hosts: [] }),
    ask: async ({ prompt }) => { calls.ask++; calls.prompts.push(prompt); return '- do the PR'; },
    agent: async ({ prompt }) => { calls.agent++; calls.prompts.push(prompt); return { text: agentText, costUsd: 0.01 }; },
    file: path.join(dir, 'inbox.json'),
    now: () => clock.t,
    setTimer: () => null,
  });
  return { inbox, calls, clock, dir };
}

test('a pass: sources merged, a failing source reported, scan + brief within budget, state kept', async () => {
  const t0 = new Date(2026, 9, 20, 9).getTime();
  const { inbox, calls, clock, dir } = setup({ now: t0, agentText: '[{"source":"slack","id":"C1:1","url":"https://x.slack.com/archives/C1/p1","title":"Quick q","effort":"quick","at":"2026-10-20T07:00:00Z"}]' });
  await inbox.pass(); // starts the scan and the brief
  await flush();
  const r = inbox.response();
  assert.equal(r.sources.productive.ok, false);
  assert.match(r.sources.productive.error, /no token/);
  assert.deepEqual(r.items.map((i) => i.id).sort(), ['pr:https://github.com/acme/app/pull/1', 'slack:C1:1']);
  assert.equal(calls.agent, 1);
  assert.equal(calls.ask, 1);
  assert.equal(r.brief.text, '- do the PR');

  // Ten minutes later, nothing new: no scan, no brief.
  clock.t += 10 * 60 * 1000;
  await inbox.pass();
  await flush();
  assert.equal(calls.agent, 1);
  assert.equal(calls.ask, 1);

  // The next scan is fed the open items so their ids stay.
  clock.t += 30 * 60 * 1000;
  await inbox.pass();
  await flush();
  assert.equal(calls.agent, 2);
  assert.match(calls.prompts.at(-1), /"id":"slack:C1:1"/);

  // State survives a restart.
  const saved = JSON.parse(fs.readFileSync(path.join(dir, 'inbox.json'), 'utf8'));
  assert.equal(saved.comms.items[0].id, 'slack:C1:1');
  assert.ok(saved.firstSeen['slack:C1:1']);
});

test('no scan outside work hours', async () => {
  const { inbox, calls } = setup({ now: new Date(2026, 9, 18, 10).getTime() }); // Sunday
  await inbox.pass();
  assert.equal(calls.agent, 0);
  assert.ok(inbox.response().sources.comms.nextAt > new Date(2026, 9, 19, 7, 59).getTime());
});

test('linkBrief: item refs become their links, unknown refs plain text', () => {
  const items = [{ url: 'https://github.com/a/b/pull/1' }, { url: null, href: '#/s/laptop/x' }];
  assert.equal(linkBrief('- merge [the PR](#1), answer [fix-login](#2), [ghost](#9)', items), '- merge [the PR](https://github.com/a/b/pull/1), answer [fix-login](#/s/laptop/x), ghost');
});

test('checkInboxSettings', () => {
  assert.deepEqual(checkInboxSettings({ scanMinutes: 60, workHours: [8, 18], workDays: [3, 1, 1] }).ok, { scanMinutes: 60, workHours: [8, 18], workDays: [1, 3] });
  assert.match(checkInboxSettings({ scanMinutes: 5 }).error, /scanMinutes/);
  assert.match(checkInboxSettings({ workHours: [18, 8] }).error, /workHours/);
  assert.match(checkInboxSettings({ workDays: [] }).error, /workDays/);
  assert.match(checkInboxSettings({}).error, /nothing/);
});

test('isDismissed: messages and sessions come back on newer activity, PRs and tasks do not', () => {
  const d = { at: 10, itemAt: 5 };
  assert.equal(isDismissed({ source: 'slack', at: 5 }, d), true);
  assert.equal(isDismissed({ source: 'slack', at: 6 }, d), false);
  assert.equal(isDismissed({ source: 'github', kind: 'review', at: 99 }, d), true);
  assert.equal(isDismissed({ source: 'github', kind: 'mention', at: 99 }, d), false);
});

test('dismiss: off the list at once, kept across passes, the scan told it is done', async () => {
  const { inbox, calls, clock } = setup({ now: new Date(2026, 9, 20, 9).getTime(), agentText: '[{"source":"slack","id":"C1:1","title":"Q","effort":"quick","at":"2026-10-20T06:00:00Z"}]' });
  await inbox.pass();
  await flush();
  assert.equal(inbox.dismiss('pr:https://github.com/acme/app/pull/1').items.some((i) => i.source === 'github'), false);
  assert.equal(inbox.dismiss('slack:C1:1').items.length, 0);
  assert.equal(inbox.dismiss('nope'), null);
  clock.t += 2 * 3600 * 1000;
  await inbox.refresh(); // Scan now
  await flush();
  assert.equal(inbox.response().items.length, 0);
  assert.match(calls.prompts.at(-1).includes('marked these done') ? calls.prompts.at(-1) : calls.prompts.at(-2), /slack:C1:1 \(done at/);
});

test('commsPrompt: DMs are read as a channel, previous items carry their time, custom rules replace the default', () => {
  const t = new Date(2026, 9, 20, 9).getTime();
  const p = commsPrompt({ since: t - 3600e3, now: t, previous: [{ id: 'slack:D1:1.2', url: null, from: 'Ana', title: 'Q', at: t - 7200e3 }] });
  assert.match(p, /slack_read_channel/);
  assert.match(p, /from:me/);
  assert.match(p, /"at":"2026-/);
  assert.ok(p.includes(DEFAULT_SCAN_RULES));
  const own = commsPrompt({ since: t, now: t, rules: 'Only DMs from my manager.' });
  assert.ok(own.includes('Only DMs from my manager.'));
  assert.ok(!own.includes(DEFAULT_SCAN_RULES));
  assert.match(own, /Answer with ONLY a JSON array/); // the frame stays
});

test('checkInboxSettings: models, brief interval, comms, language, scan rules (default → null)', () => {
  assert.deepEqual(checkInboxSettings({ model: 'sonnet', briefModel: 'haiku', briefMinutes: 30, comms: false, language: 'Croatian' }).ok, { model: 'sonnet', briefModel: 'haiku', briefMinutes: 30, comms: false, language: 'Croatian' });
  assert.match(checkInboxSettings({ model: 'rm -rf' }).error, /model/);
  assert.match(checkInboxSettings({ briefMinutes: 1 }).error, /briefMinutes/);
  assert.equal(checkInboxSettings({ scanRules: 'Only DMs.' }).ok.scanRules, 'Only DMs.');
  assert.equal(checkInboxSettings({ scanRules: '' }).ok.scanRules, null);
  assert.equal(checkInboxSettings({ scanRules: DEFAULT_SCAN_RULES }).ok.scanRules, null);
  assert.match(checkInboxSettings({ scanRules: 'x'.repeat(9000) }).error, /scanRules/);
});

test('usage: each call is logged per day and kind; summaries over today, the week and the month', () => {
  const t = new Date(2026, 9, 20, 9).getTime();
  let log = recordUsage(null, { kind: 'scan', at: t, model: 'haiku', costUsd: 0.02, usage: { input: 10, output: 5, cacheRead: 100, cacheWrite: 20 } });
  log = recordUsage(log, { kind: 'brief', at: t, model: 'haiku', costUsd: 0.001, usage: { input: 3, output: 2, cacheRead: 0, cacheWrite: 0 } });
  log = recordUsage(log, { kind: 'scan', at: t - 3 * 86400e3, model: 'sonnet', costUsd: 0.1, usage: { input: 1, output: 1, cacheRead: 1, cacheWrite: 1 } });
  log = recordUsage(log, { kind: 'scan', at: t - 40 * 86400e3, model: 'haiku', costUsd: 9 }); // older than the log keeps
  const s = usageSummary(log, t);
  assert.equal(s.today.scan.runs, 1);
  assert.equal(s.today.brief.output, 2);
  assert.equal(s.today.total.cacheRead, 100);
  assert.equal(s.week.scan.runs, 2);
  assert.ok(Math.abs(s.week.total.costUsd - 0.121) < 1e-9);
  assert.equal(s.month.scan.runs, 2); // the 40-day-old day was dropped… from the days, not the run list
  assert.equal(s.runs[0].kind, 'scan');
  assert.equal(s.runs[1].model, 'sonnet');
});

test('a scan and a brief with usage are counted in the response', async () => {
  const t0 = new Date(2026, 9, 20, 9).getTime();
  const { inbox } = setup({ now: t0 });
  inbox.updateSettings({}); // no-op
  await inbox.pass();
  await flush();
  const r = inbox.response();
  assert.equal(r.usage.today.scan.runs, 1);
  assert.ok(Math.abs(r.usage.today.scan.costUsd - 0.01) < 1e-9);
  assert.equal(r.settings.model, 'haiku');
  assert.equal(typeof r.settings.defaultScanRules, 'string');
});

test('updateSettings: comms off drops the source, on brings it back', () => {
  const { inbox } = setup({ now: new Date(2026, 9, 20, 9).getTime() });
  assert.equal(inbox.updateSettings({ comms: false }).sources.comms, undefined);
  assert.ok(inbox.updateSettings({ comms: true }).sources.comms);
});

test('createAgentAsk: connectors limited to the tools, or none (mcp: false); usage read from the JSON', async () => {
  const seen = [];
  const run = async (bin, args) => {
    seen.push(args);
    return { stdout: JSON.stringify({ result: '[]', total_cost_usd: 0.02, num_turns: 4, duration_ms: 900, usage: { input_tokens: 7, output_tokens: 3, cache_read_input_tokens: 50, cache_creation_input_tokens: 9 } }) };
  };
  const agent = createAgentAsk({ run });
  const r = await agent({ prompt: 'x', tools: ['mcp__a'] });
  assert.deepEqual(r.usage, { input: 7, output: 3, cacheRead: 50, cacheWrite: 9 });
  assert.equal(r.turns, 4);
  assert.ok(seen[0].includes('--allowed-tools'));
  assert.ok(!seen[0].includes('--strict-mcp-config'));
  await agent({ prompt: 'x', mcp: false });
  assert.ok(seen[1].includes('--strict-mcp-config'));
  assert.ok(!seen[1].includes('--allowed-tools'));
});
