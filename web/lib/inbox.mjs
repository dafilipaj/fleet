// The inbox: one live to-do list the system keeps for you — never edited by hand. Everything on
// it links out to the thing that waits on you (a PR, a task, a session, a Slack thread, a mail),
// and it drops off by itself once its source says it is done.
//
// One host's server runs it (config `inbox.host`; peers proxy /api/inbox there). Sources:
//
// - github      `fleet --local prs --json` (gh, no model): review requests, unseen @mentions,
//               your PRs that are approved, have changes requested or a failing CI
// - productive  `fleet --local tasks --json` (REST, no model): due / overdue tasks, tasks in
//               progress you touched lately
// - sessions    the merged fleet (no model): sessions on any host waiting for your answer
// - slack       with a Slack user token (lib/inbox-slack.mjs): a poller every `slackMinutes`
//               that closes answered conversations itself and has a model (no tools) sort only
//               the ones with new messages
// - comms       Gmail (and Slack, without the token) through the claude.ai connectors — one
//               `claude -p` with read-only tools, every `scanMinutes` in work hours, fed the items
//               it found last time so ids stay stable and answered threads drop off
//
// Quick wins are mostly decided without a model (a small PR review, a waiting session, an
// approved PR to merge); the scan rates Slack and mail itself. The brief (a few bullets over the
// whole list) is one more model call, only when the list changed and at most every `briefMinutes`.
//
// State (`<fleet state>/inbox.json`): the comms items and when they were scanned, the brief, and
// when each item was first seen.

import crypto from 'node:crypto';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

import { claudeFailure, isLoginError, LOGIN_HINT } from './claude-login.mjs';
import { MODEL_ID_RE } from './config.mjs';
import { createSlackPoller, emptySlackState } from './inbox-slack.mjs';

export const SECTIONS = ['quick', 'waiting', 'mine'];
/** Tasks in progress count as "your work" when touched this recently. */
export const RECENT_TASK_MS = 3 * 24 * 3600 * 1000;
/** A PR review this small is a quick win. */
export const QUICK_PR_LINES = 150;
export const QUICK_PR_FILES = 6;
/** The first scan (and any after a long gap) looks this far back. */
export const SCAN_LOOKBACK_MS = 3 * 24 * 3600 * 1000;
/** A later scan starts this long before the previous one, so nothing falls between two. */
export const SCAN_OVERLAP_MS = 2 * 3600 * 1000;

/** What the scan may call: read-only Slack and Gmail. */
export const COMMS_TOOLS = [
  'mcp__claude_ai_Slack__slack_search_public_and_private',
  'mcp__claude_ai_Slack__slack_read_thread',
  'mcp__claude_ai_Slack__slack_read_channel',
  'mcp__claude_ai_Slack__slack_read_user_profile',
  'mcp__claude_ai_Gmail__search_threads',
  'mcp__claude_ai_Gmail__get_thread',
];
/** The scan's tools when the Slack poller runs: Gmail only. */
export const EMAIL_TOOLS = COMMS_TOOLS.filter((t) => t.includes('Gmail'));

/** Denied outright (a deny wins over any permission mode the user's settings pick). */
export const DENIED_TOOLS = [
  'Bash', 'Edit', 'Write', 'Read', 'WebFetch', 'WebSearch', 'Task', 'NotebookEdit',
  'mcp__claude_ai_Slack__slack_send_message', 'mcp__claude_ai_Slack__slack_send_message_draft',
  'mcp__claude_ai_Slack__slack_schedule_message', 'mcp__claude_ai_Slack__slack_add_reaction',
  'mcp__claude_ai_Slack__slack_create_canvas', 'mcp__claude_ai_Slack__slack_update_canvas',
  'mcp__claude_ai_Gmail__send_message', 'mcp__claude_ai_Gmail__reply', 'mcp__claude_ai_Gmail__forward',
  'mcp__claude_ai_Gmail__create_draft', 'mcp__claude_ai_Gmail__trash_thread', 'mcp__claude_ai_Gmail__trash_message',
  'mcp__claude_ai_Gmail__label_thread', 'mcp__claude_ai_Gmail__label_message',
  'mcp__claude_ai_Gmail__update_message_labels',
];

/** The knobs the UI may change (Settings → Inbox), checked. → { ok: {…} } or { error }. Pure. */
export function checkInboxSettings(body) {
  const out = {};
  if (body?.scanMinutes != null) {
    const n = Number(body.scanMinutes);
    if (!Number.isInteger(n) || n < 15 || n > 24 * 60) return { error: 'scanMinutes must be a whole number of minutes, 15–1440' };
    out.scanMinutes = n;
  }
  if (body?.workHours != null) {
    const h = body.workHours;
    if (!Array.isArray(h) || h.length !== 2 || !h.every((x) => Number.isInteger(x) && x >= 0 && x <= 24) || h[0] >= h[1]) {
      return { error: 'workHours must be [from, to] hours, 0–24, from < to' };
    }
    out.workHours = h;
  }
  if (body?.workDays != null) {
    const d = body.workDays;
    if (!Array.isArray(d) || !d.length || !d.every((x) => Number.isInteger(x) && x >= 0 && x <= 6)) return { error: 'workDays must be weekdays 0–6 (0 = Sunday), at least one' };
    out.workDays = [...new Set(d)].sort();
  }
  for (const key of ['model', 'briefModel']) {
    if (body?.[key] == null) continue;
    if (typeof body[key] !== 'string' || !MODEL_ID_RE.test(body[key])) return { error: `${key} must be a model id (e.g. haiku, sonnet, opus)` };
    out[key] = body[key];
  }
  if (body?.briefMinutes != null) {
    const n = Number(body.briefMinutes);
    if (!Number.isInteger(n) || n < 5 || n > 24 * 60) return { error: 'briefMinutes must be a whole number of minutes, 5–1440' };
    out.briefMinutes = n;
  }
  if (body?.slackMinutes != null) {
    const n = Number(body.slackMinutes);
    if (!Number.isInteger(n) || n < 1 || n > 120) return { error: 'slackMinutes must be a whole number of minutes, 1–120' };
    out.slackMinutes = n;
  }
  if (body?.slackModel != null) {
    if (typeof body.slackModel !== 'string' || !MODEL_ID_RE.test(body.slackModel)) return { error: 'slackModel must be a model id (e.g. haiku, sonnet, opus)' };
    out.slackModel = body.slackModel;
  }
  if (body && 'slackRules' in body) {
    const r = body.slackRules == null ? '' : body.slackRules;
    if (typeof r !== 'string' || r.length > MAX_SCAN_RULES) return { error: `slackRules must be text, at most ${MAX_SCAN_RULES} characters` };
    out.slackRules = r.trim() || null;
  }
  if (body?.comms != null) {
    if (typeof body.comms !== 'boolean') return { error: 'comms must be true or false' };
    out.comms = body.comms;
  }
  if (body?.language != null) {
    if (typeof body.language !== 'string' || !body.language.trim() || body.language.length > 40) return { error: 'language must be a short name, e.g. English' };
    out.language = body.language.trim();
  }
  if (body && 'scanRules' in body) {
    // '' or null → back to DEFAULT_SCAN_RULES.
    const r = body.scanRules == null ? '' : body.scanRules;
    if (typeof r !== 'string' || r.length > MAX_SCAN_RULES) return { error: `scanRules must be text, at most ${MAX_SCAN_RULES} characters` };
    out.scanRules = r.trim() && r.trim() !== DEFAULT_SCAN_RULES ? r.trim() : null;
  }
  if (!Object.keys(out).length) return { error: `nothing to change (${SETTING_KEYS.join(', ')})` };
  return { ok: out };
}

/** The knobs Settings → Inbox shows and may change. */
export const SETTING_KEYS = ['scanMinutes', 'workHours', 'workDays', 'model', 'briefModel', 'briefMinutes', 'comms', 'language', 'scanRules', 'slackMinutes', 'slackModel', 'slackRules'];
export const MAX_SCAN_RULES = 8000;

/** Done by hand comes back on newer activity for these; the others stay away until their source drops them. */
const REAPPEAR_SOURCES = new Set(['slack', 'email', 'session']);

/** Is the item hidden by a dismissal `{ at, itemAt }`? Pure. */
export function isDismissed(item, d) {
  if (!d) return false;
  if (REAPPEAR_SOURCES.has(item.source) || item.kind === 'mention') return (item.at ?? 0) <= (d.itemAt ?? 0);
  return true;
}

// ------------------------------------------------------------------ items from each source

const fileCount = (n) => `${n} file${n === 1 ? '' : 's'}`;

/** `fleet prs --json` → items. Pure. */
export function prItems(body) {
  const out = [];
  for (const p of body?.prs ?? []) {
    const meta = {
      repo: `${p.owner}/${p.repo}`,
      number: p.number,
      size: p.size,
      additions: p.additions,
      deletions: p.deletions,
      checks: p.checks ?? null,
      draft: Boolean(p.draft),
    };
    const base = { id: `pr:${p.url}`, source: 'github', title: p.title, url: p.url, href: null, at: p.updatedAt, meta };
    const failing = p.checks === 'failure' || p.checks === 'error';
    if (p.column === 'unseenMentions' && p.mention) {
      out.push({
        ...base,
        kind: 'mention',
        section: 'waiting',
        effort: null,
        from: p.mention.author,
        ask: `@mention: ${p.mention.excerpt}`,
        url: p.mention.url || p.url,
        at: p.mention.at,
      });
    } else if (p.column === 'waitingOnMe') {
      const lines = (p.additions ?? 0) + (p.deletions ?? 0);
      const quick = !p.draft && lines <= QUICK_PR_LINES && (p.changedFiles ?? 0) <= QUICK_PR_FILES;
      out.push({
        ...base,
        kind: 'review',
        section: quick ? 'quick' : 'waiting',
        effort: quick ? 'quick' : p.size === 'L' || p.size === 'XL' ? 'deep' : 'medium',
        from: p.author,
        ask: `Review · +${p.additions} −${p.deletions} in ${fileCount(p.changedFiles ?? 0)}`,
      });
    } else if (p.column === 'readyToMergeMine') {
      out.push({
        ...base,
        kind: 'myPr',
        section: failing ? 'waiting' : 'quick',
        effort: failing ? 'medium' : 'quick',
        from: null,
        ask: failing ? 'Approved, but CI is failing' : 'Approved — merge it',
      });
    } else if (p.column === 'reviewMine' && (p.reviewDecision === 'changesRequested' || failing)) {
      out.push({
        ...base,
        kind: 'myPr',
        section: 'waiting',
        effort: 'medium',
        from: null,
        ask: p.reviewDecision === 'changesRequested' ? 'Changes requested' : 'CI is failing',
      });
    }
  }
  return out;
}

const DAY_MS = 24 * 3600 * 1000;
/** `YYYY-MM-DD` in local time. */
export function localDate(ms) {
  const d = new Date(ms);
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`;
}

/** `fleet tasks --json` → items. Pure. */
export function taskItems(body, now = Date.now()) {
  const out = [];
  const today = localDate(now);
  for (const t of body?.tasks ?? []) {
    const due = t.dueDate ?? null;
    const overdue = Boolean(due && due < today);
    const touched = t.lastActivityAt ?? t.updatedAt ?? 0;
    const base = {
      id: `task:${t.id}`,
      source: 'productive',
      kind: 'task',
      title: t.title,
      url: t.url,
      href: null,
      from: null,
      effort: null,
      at: touched,
      meta: { project: t.project?.name, due, overdue, status: t.status?.name },
    };
    if (t.column === 'due') {
      out.push({ ...base, section: overdue ? 'waiting' : 'mine', ask: `${overdue ? 'Overdue' : 'Due'} ${due === today ? 'today' : due} · ${t.status?.name ?? ''}`.trim() });
    } else if (t.column === 'inProgress' && now - touched <= RECENT_TASK_MS) {
      out.push({ ...base, section: 'mine', ask: t.status?.name ?? 'In progress' });
    }
  }
  return out;
}

/** The merged `/api/fleet` body → sessions waiting for an answer, any host but a shared one. Pure. */
export function sessionItems(fleetBody) {
  const out = [];
  for (const h of fleetBody?.hosts ?? []) {
    if (h?.shared === true) continue; // a shared shed's sessions are not the user's to answer
    if (h?.ok === false) continue;
    for (const s of h?.sessions ?? []) {
      if (s?.status !== 'waiting' || s.operator || !s.session_id) continue;
      const host = s.host || h.name;
      out.push({
        id: `session:${host}/${s.session_id}`,
        source: 'session',
        kind: 'session',
        section: 'quick',
        effort: 'quick',
        title: s.display_title || s.name || s.title || s.session_id.slice(0, 8),
        ask: s.waiting_for ? `Waiting: ${s.waiting_for}` : 'Waiting for your answer',
        url: null,
        href: `#/s/${encodeURIComponent(host)}/${encodeURIComponent(s.session_id)}`,
        from: null,
        at: s.updated_at ?? 0,
        meta: { host, sessionId: s.session_id },
      });
    }
  }
  return out;
}

// ------------------------------------------------------------------ the Slack + mail scan

/**
 * What the scan looks for and how it decides an item is still open — the part of the prompt the
 * user may rewrite (Settings → Inbox, `inbox.scanRules`). The frame around it (read only, the
 * time window, the previous items, the answer format) is fixed.
 */
export const DEFAULT_SCAN_RULES = [
  'Slack — what waits on the user to act or answer:',
  '- direct and group messages to the user whose latest message is not from the user',
  '- @mentions of the user (or their user groups) in channels and threads the user has not answered since',
  '- threads the user started or took part in where someone asked the user something new',
  'Find candidates with slack_search_public_and_private (e.g. `to:me`, mentions of the user), and find what the user already answered with `from:me` over the same window.',
  '',
  'Deciding whether a conversation still waits on the user — check the latest messages, never the snippet alone:',
  '- a direct or group DM (channel id starting with D, or a group DM): people answer there in the channel, not in a thread. Read it with slack_read_channel from the item\'s ts (oldest=<ts>); a thread with no replies says nothing.',
  '- a channel thread: read it with slack_read_thread, every page (follow the cursor) — the newest replies come last.',
  '- it is done when the newest message from a person is the user\'s, when the user reacted to the ask (✅ 👍 👀 or alike), or when someone else clearly took it over. Bots and automated posts do not count as an answer to the user, nor as a new ask.',
  '- a message older than 7 days the user never answered stays only if it is clearly still wanted.',
  '',
  'Gmail — search_threads for `in:inbox newer_than:3d -category:promotions -category:social -category:updates -category:forums`; keep only mail from a person that asks the user for something (no newsletters, notifications, calendar invites, automated mail). A thread whose latest message is from the user is done.',
].join('\n');

/** The scan's prompt. Pure. */
export function commsPrompt({ since, previous = [], done = [], now = Date.now(), rules = null, emailOnly = false }) {
  const sinceDate = localDate(since);
  const handled = done.length
    ? ['', 'The user marked these done — leave them out unless a newer message came after the time given:', ...done.map((d) => `- ${d.id} (done at ${new Date(d.itemAt).toISOString()})`)]
    : [];
  const prev = previous.length
    ? [
        '',
        'Open items from the previous scan — check EACH again by reading its conversation as above (they may be older than the window): keep it (same id, latest `at`) only while it still waits on the user; drop it once the user answered or it no longer needs them:',
        ...previous.map((i) => `- ${JSON.stringify({ id: i.id, url: i.url, from: i.from, title: i.title, at: new Date(i.at).toISOString() })}`),
      ]
    : [];
  return [
    'You keep the user\'s live to-do inbox from Slack and Gmail. Read only: never send, react, draft, label or change anything.',
    `Now: ${new Date(now).toISOString()}. Look at activity since ${new Date(since).toISOString()} (search with after:/newer_than: from ${sinceDate}).`,
    '',
    String(rules ?? '').trim() || DEFAULT_SCAN_RULES,
    ...(emailOnly ? ['', 'Slack is handled elsewhere: skip it entirely, look at Gmail only (source "email").'] : []),
    ...prev,
    ...handled,
    '',
    'Answer with ONLY a JSON array (no prose, no code fence), one object per thing that still waits on the user:',
    '{"source":"slack"|"email","id":"slack:<channel id>:<thread root ts, or the message ts in a DM>" or "email:<gmail thread id>","url":"<the message permalink exactly as a tool returned it; for mail leave empty>","from":"<person>","title":"<what they want, ≤ 80 chars, in the message\'s language>","ask":"<one sentence: what exactly the user should do>","effort":"quick"|"medium"|"deep","at":"<ISO time of the latest message>"}',
    'quick = done in under ~5 minutes (a yes/no, a short answer, a link, a quick look). Leave out everything that does not need the user. An empty array is fine.',
  ].join('\n');
}

/** The first JSON array in a model answer, or null. */
export function extractJsonArray(text) {
  const s = String(text ?? '');
  const start = s.indexOf('[');
  const end = s.lastIndexOf(']');
  if (start < 0 || end <= start) return null;
  try {
    const v = JSON.parse(s.slice(start, end + 1));
    return Array.isArray(v) ? v : null;
  } catch {
    return null;
  }
}

const EFFORTS = new Set(['quick', 'medium', 'deep']);
const clip = (s, n) => {
  const t = String(s ?? '').replace(/\s+/g, ' ').trim();
  return t.length > n ? `${t.slice(0, n - 1)}…` : t;
};

/** The scan's array → items (bad entries dropped, ids namespaced, mail links filled). Pure. */
export function commsItems(raw, now = Date.now()) {
  const out = [];
  const seen = new Set();
  for (const r of raw ?? []) {
    if (!r || typeof r !== 'object') continue;
    const source = r.source === 'email' ? 'email' : r.source === 'slack' ? 'slack' : null;
    if (!source || !r.title) continue;
    let id = String(r.id ?? '').trim();
    if (!id) continue;
    if (!id.startsWith(`${source}:`)) id = `${source}:${id}`;
    if (seen.has(id)) continue;
    seen.add(id);
    let url = typeof r.url === 'string' && /^https:\/\//.test(r.url) ? r.url : null;
    if (!url && source === 'email') url = `https://mail.google.com/mail/u/0/#all/${encodeURIComponent(id.slice('email:'.length))}`;
    const effort = EFFORTS.has(r.effort) ? r.effort : 'medium';
    const at = Date.parse(r.at);
    out.push({
      id,
      source,
      kind: 'message',
      section: effort === 'quick' ? 'quick' : 'waiting',
      effort,
      title: clip(r.title, 120),
      ask: r.ask ? clip(r.ask, 240) : null,
      url,
      href: null,
      from: r.from ? clip(r.from, 60) : null,
      at: Number.isFinite(at) ? at : now,
      meta: {},
    });
  }
  return out;
}

// ------------------------------------------------------------------ the brief

/** The brief's prompt. Pure. */
export function briefPrompt({ items, language = 'English', now = Date.now() }) {
  const label = { quick: 'Quick wins', waiting: 'Waiting on you', mine: 'Your work' };
  const lines = [];
  for (const sec of SECTIONS) {
    const its = items.map((i, n) => [i, n + 1]).filter(([i]) => i.section === sec);
    if (!its.length) continue;
    lines.push(`${label[sec]}:`);
    for (const [i, ref] of its) {
      lines.push(`- #${ref} [${i.source}] ${i.title}${i.ask ? ` — ${i.ask}` : ''}${i.from ? ` (from ${i.from})` : ''}`);
    }
  }
  return [
    `It is ${new Date(now).toString().slice(0, 21)}. Below is everything that waits on the user right now, from GitHub, Productive, Slack, mail and their Claude Code sessions.`,
    `Write their brief for the rest of the day in ${language}: 3–8 short markdown bullets, most urgent first.`,
    'Start with what to knock out first (quick wins, grouped when alike, e.g. "3 small PR reviews"), then what blocks other people, then deadlines. Link every thing you name to its item by number: [short name](#12) — the link text is always a few words naming it ("artifacts gating PR", "Luka’s Langfuse question"), never the number itself. No intro, no outro, no headings — bullets only.',
    '',
    ...lines,
  ].join('\n');
}

/** `](#12)` → the 12th item's link (its url, else its in-app href); unknown refs lose the link. Pure. */
export function linkBrief(text, items) {
  return String(text ?? '').replace(/\[([^\]]+)\]\(#(\d+)\)/g, (all, label, n) => {
    const it = items[Number(n) - 1];
    const href = it?.url || it?.href;
    return href ? `[${label}](${href})` : label;
  });
}

/** A model answer → markdown bullets (code fences and chatter around them dropped), or null. */
export function cleanBrief(text) {
  const lines = String(text ?? '').replace(/```[a-z]*\n?/g, '').split('\n');
  const bullets = lines.filter((l) => /^\s*([-*]|\d+\.)\s+/.test(l));
  return bullets.length ? bullets.join('\n').trim() : null;
}

// ------------------------------------------------------------------ helpers

/** Section order, then newest first. Pure. */
export function sortItems(items) {
  return [...items].sort((a, b) => SECTIONS.indexOf(a.section) - SECTIONS.indexOf(b.section) || (b.at ?? 0) - (a.at ?? 0));
}

/** Within `workHours` on a `workDays` day, local time. */
export function inWorkHours(ms, settings) {
  const d = new Date(ms);
  const [from, to] = settings.workHours;
  return settings.workDays.includes(d.getDay()) && d.getHours() >= from && d.getHours() < to;
}

/** What the brief depends on: which items, in which section. */
export function fingerprint(items) {
  const key = items.map((i) => `${i.section}:${i.id}`).sort().join('\n');
  return crypto.createHash('sha1').update(key).digest('hex').slice(0, 16);
}

/** `claude -p --output-format json` usage → { input, output, cacheRead, cacheWrite } (token counts). Pure. */
export function readUsage(out) {
  const u = out?.usage ?? {};
  const n = (v) => (Number.isFinite(v) ? v : 0);
  return {
    input: n(u.input_tokens),
    output: n(u.output_tokens),
    cacheRead: n(u.cache_read_input_tokens),
    cacheWrite: n(u.cache_creation_input_tokens),
  };
}

/**
 * A `claude -p` that may use the claude.ai connectors, limited to `tools` (`mcp: false` → no
 * connectors and no tools, `--strict-mcp-config`: a plain answer, e.g. the brief). → { text,
 * costUsd, usage, turns, durationMs }.
 */
export function createAgentAsk({ bin = 'claude', run, timeoutMs = 6 * 60 * 1000, cwd = os.tmpdir(), env = process.env }) {
  if (typeof run !== 'function') throw new TypeError('run must be a function');
  return async function agent({ prompt, model = 'haiku', tools = [], mcp = true }) {
    const args = [
      '-p', '--model', model, '--no-session-persistence', '--output-format', 'json',
      '--permission-mode', 'default',
      ...(mcp && tools.length ? ['--allowed-tools', tools.join(',')] : []),
      ...(mcp ? [] : ['--strict-mcp-config']),
      '--disallowed-tools', DENIED_TOOLS.join(','),
    ];
    let stdout;
    try {
      ({ stdout } = await run(bin, args, { input: prompt, timeout: timeoutMs, cwd, env, maxBuffer: 8 * 1024 * 1024 }));
    } catch (err) {
      const why = err?.killed ? `\`claude\` did not finish within ${Math.round(timeoutMs / 1000)}s` : claudeFailure(err);
      throw new Error(why);
    }
    let out;
    try {
      out = JSON.parse(stdout);
    } catch {
      return { text: stdout, costUsd: null, usage: null, turns: null, durationMs: null };
    }
    if (out?.is_error) {
      const why = String(out.result ?? out.subtype ?? 'error').slice(0, 200);
      throw new Error(`\`claude -p\` failed: ${why}${isLoginError(why) ? ` — ${LOGIN_HINT}` : ''}`);
    }
    return {
      text: String(out?.result ?? ''),
      costUsd: typeof out?.total_cost_usd === 'number' ? out.total_cost_usd : null,
      usage: readUsage(out),
      turns: Number.isFinite(out?.num_turns) ? out.num_turns : null,
      durationMs: Number.isFinite(out?.duration_ms) ? out.duration_ms : null,
    };
  };
}

// ------------------------------------------------------------------ usage

/** Days of usage kept in the state file, and the last runs listed. */
export const USAGE_DAYS = 31;
export const USAGE_RUNS = 30;

/** What the inbox calls a model for: the connector scan, the Slack sort, the brief. */
export const USAGE_KINDS = ['scan', 'slack', 'brief'];
const zeroUsage = () => ({ runs: 0, input: 0, output: 0, cacheRead: 0, cacheWrite: 0, costUsd: 0 });
const addUsage = (into, r) => {
  into.runs += r.runs ?? 1;
  for (const k of ['input', 'output', 'cacheRead', 'cacheWrite', 'costUsd']) into[k] += r[k] ?? r.usage?.[k] ?? 0;
  return into;
};

/** One model call into the usage log `{ days: { date: { scan, brief } }, runs: [...] }` (old days dropped). Pure (returns a new log). */
export function recordUsage(log, { kind, at, model, ok = true, costUsd = null, usage = null, turns = null, durationMs = null }) {
  const days = { ...(log?.days ?? {}) };
  const date = localDate(at);
  const day = { scan: zeroUsage(), slack: zeroUsage(), brief: zeroUsage(), ...(days[date] ?? {}) };
  day[kind] = addUsage({ ...zeroUsage(), ...day[kind] }, { ...(usage ?? {}), costUsd: costUsd ?? 0 });
  days[date] = day;
  // Keep the last USAGE_DAYS days before the newest run (a late, old entry never wipes the rest).
  const newest = Math.max(at, ...(log?.runs ?? []).map((r) => r.at ?? 0));
  const oldest = localDate(newest - (USAGE_DAYS - 1) * DAY_MS);
  for (const d of Object.keys(days)) if (d < oldest) delete days[d];
  const run = { kind, at, model, ok, costUsd, turns, durationMs, ...(usage ?? {}) };
  return { days, runs: [run, ...(log?.runs ?? [])].slice(0, USAGE_RUNS) };
}

/** The usage log → { today, week, month: { scan, slack, brief, total } each, runs }. Pure. */
export function usageSummary(log, now = Date.now()) {
  const span = (n) => {
    const out = { scan: zeroUsage(), slack: zeroUsage(), brief: zeroUsage(), total: zeroUsage() };
    for (let i = 0; i < n; i++) {
      const d = log?.days?.[localDate(now - i * DAY_MS)];
      if (!d) continue;
      for (const k of USAGE_KINDS) if (d[k]) {
        addUsage(out[k], d[k]);
        addUsage(out.total, d[k]);
      }
    }
    return out;
  };
  return { today: span(1), week: span(7), month: span(30), runs: log?.runs ?? [] };
}

// ------------------------------------------------------------------ the store

export function loadState(file) {
  try {
    const s = JSON.parse(fs.readFileSync(file, 'utf8'));
    return {
      firstSeen: s.firstSeen && typeof s.firstSeen === 'object' ? s.firstSeen : {},
      comms: s.comms && Array.isArray(s.comms.items) ? s.comms : { at: null, items: [], error: null },
      brief: s.brief ?? null,
      dismissed: s.dismissed && typeof s.dismissed === 'object' ? s.dismissed : {},
      usage: s.usage && typeof s.usage === 'object' ? s.usage : { days: {}, runs: [] },
      slack: s.slack && typeof s.slack === 'object' ? { ...emptySlackState(), ...s.slack } : emptySlackState(),
    };
  } catch {
    return { firstSeen: {}, comms: { at: null, items: [], error: null }, brief: null, dismissed: {}, usage: { days: {}, runs: [] }, slack: emptySlackState() };
  }
}

function saveState(file, state) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  const tmp = `${file}.${process.pid}.tmp`;
  fs.writeFileSync(tmp, `${JSON.stringify(state, null, 2)}\n`);
  fs.renameSync(tmp, file);
}

// ------------------------------------------------------------------ the pass

/**
 * deps:
 *   settings   config.inbox
 *   cli        lib/fleet-cli.mjs — prs(), tasks()
 *   getFleet   () => Promise<the merged /api/fleet body>
 *   ask        ({ prompt, model }) => Promise<string | { text, costUsd, usage }>   (the brief; an
 *              object answer is counted in the usage log)
 *   agent      ({ prompt, model, tools }) => Promise<{ text, costUsd, usage }>   (createAgentAsk)
 *   slackApi   (method, params) => body with the user's Slack token (lib/slack.mjs createSlackApi), or
 *              null: no Slack poller, the connector scan reads Slack too
 *   file       the state file
 *   passMs     how often the no-model sources are read
 */
export function createInbox({
  settings,
  cli,
  getFleet,
  ask,
  agent,
  slackApi = null,
  file,
  passMs = 2 * 60 * 1000,
  now = Date.now,
  setTimer = setTimeout,
  clearTimer = clearTimeout,
  log = () => {},
}) {
  const state = loadState(file);
  const sources = {
    github: { at: null, ok: true, error: null, count: 0 },
    productive: { at: null, ok: true, error: null, count: 0 },
    sessions: { at: null, ok: true, error: null, count: 0 },
    ...(settings.comms ? { comms: { at: state.comms.at, ok: !state.comms.error, error: state.comms.error ?? null, count: state.comms.items.length, nextAt: null, running: false } } : {}),
  };
  const lists = { github: [], productive: [], sessions: [], slack: [] };
  const slack = slackApi
    ? createSlackPoller({
        api: slackApi,
        state: state.slack,
        rules: () => settings.slackRules,
        classify: async ({ prompt }) => {
          const t = now();
          const model = settings.slackModel ?? 'haiku';
          try {
            const r = await agent({ prompt, model, mcp: false });
            note('slack', t, model, r);
            return r;
          } catch (err) {
            note('slack', t, model, { ok: false });
            throw err;
          }
        },
        now,
        log,
      })
    : null;
  if (slack) {
    sources.slack = { at: state.slack.at, ok: true, error: null, count: 0 };
    lists.slack = slack.items();
  }
  let slackPolling = null;
  let items = [];
  let updatedAt = null;
  let timer = null;
  let passing = null;
  let scanning = null;
  let briefing = null;

  async function readSource(name, fn) {
    const src = sources[name];
    try {
      lists[name] = await fn();
      Object.assign(src, { at: now(), ok: true, error: null, count: lists[name].length });
    } catch (err) {
      // Keep the last good list: an outage should not empty the inbox.
      Object.assign(src, { ok: false, error: String(err?.message ?? err).slice(0, 300) });
    }
  }

  function merge() {
    const t = now();
    const comms = settings.comms ? state.comms.items.filter((i) => !slack || i.source !== 'slack') : [];
    const all = [...lists.sessions, ...lists.github, ...lists.productive, ...lists.slack, ...comms];
    const live = new Set();
    for (const i of all) {
      live.add(i.id);
      if (!state.firstSeen[i.id]) state.firstSeen[i.id] = t;
      i.firstSeen = state.firstSeen[i.id];
    }
    for (const id of Object.keys(state.firstSeen)) if (!live.has(id)) delete state.firstSeen[id];
    // A dismissal lasts while its source still reports the item (and, for messages and
    // sessions, until newer activity on it).
    for (const id of Object.keys(state.dismissed)) if (!live.has(id)) delete state.dismissed[id];
    for (const i of all) if (state.dismissed[i.id] && !isDismissed(i, state.dismissed[i.id])) delete state.dismissed[i.id];
    items = sortItems(all.filter((i) => !state.dismissed[i.id]));
    updatedAt = t;
    persist();
  }

  /** One model call into the usage log (saved with the next persist). */
  function note(kind, at, model, r = {}) {
    state.usage = recordUsage(state.usage, { kind, at, model, ok: r.ok ?? true, costUsd: r.costUsd ?? null, usage: r.usage ?? null, turns: r.turns ?? null, durationMs: r.durationMs ?? (now() - at) });
  }

  function persist() {
    try {
      saveState(file, state);
    } catch (err) {
      log(`[inbox] cannot save ${file}: ${err.message}`);
    }
  }

  function scanDue(t) {
    if (!settings.comms || scanning) return false;
    if (!inWorkHours(t, settings)) return false;
    return !state.comms.at || t - state.comms.at >= settings.scanMinutes * 60 * 1000;
  }

  function nextScanAt(t) {
    if (!settings.comms) return null;
    const step = 15 * 60 * 1000;
    let at = Math.max(t, (state.comms.at ?? 0) + settings.scanMinutes * 60 * 1000);
    for (let i = 0; i < 4 * 24 * 8 && !inWorkHours(at, settings); i++) at = Math.ceil((at + 1) / step) * step;
    return at;
  }

  function scan({ manual = false } = {}) {
    if (scanning) return scanning;
    const started = now();
    const since = state.comms.at && started - state.comms.at < SCAN_LOOKBACK_MS ? state.comms.at - SCAN_OVERLAP_MS : started - SCAN_LOOKBACK_MS;
    sources.comms.running = true;
    scanning = (async () => {
      try {
        const own = state.comms.items.filter((i) => !slack || i.source !== 'slack');
        const previous = own.filter((i) => !state.dismissed[i.id]);
        const done = own.filter((i) => state.dismissed[i.id]).map((i) => ({ id: i.id, itemAt: state.dismissed[i.id].itemAt }));
        const model = settings.model;
        let res;
        try {
          res = await agent({ prompt: commsPrompt({ since, previous, done, now: started, rules: settings.scanRules, emailOnly: Boolean(slack) }), model, tools: slack ? EMAIL_TOOLS : COMMS_TOOLS });
        } catch (err) {
          note('scan', started, model, { ok: false });
          throw err;
        }
        const { text, costUsd } = res;
        const raw = extractJsonArray(text);
        note('scan', started, model, { ...res, ok: Boolean(raw) });
        if (!raw) throw new Error(`the scan answered no JSON array: ${String(text).slice(0, 160)}`);
        state.comms = { at: started, items: commsItems(raw, started).filter((i) => !slack || i.source !== 'slack'), error: null };
        Object.assign(sources.comms, { at: started, ok: true, error: null, count: state.comms.items.length });
        log(`[inbox] scan: ${state.comms.items.length} item(s) from ${slack ? 'mail' : 'Slack + mail'} in ${Math.round((now() - started) / 1000)}s${costUsd != null ? `, $${costUsd.toFixed(3)}` : ''}`);
      } catch (err) {
        // A failed scan keeps the last items and waits for the next slot (no retry storm).
        state.comms = { ...state.comms, at: started, error: String(err?.message ?? err).slice(0, 300) };
        Object.assign(sources.comms, { ok: false, error: state.comms.error });
        log(`[inbox] scan failed: ${state.comms.error}`);
      } finally {
        sources.comms.running = false;
        scanning = null;
        merge();
        maybeBrief({ skipInterval: manual });
      }
    })();
    return scanning;
  }

  function maybeBrief({ force = false, skipInterval = false } = {}) {
    if (briefing || !items.length) return briefing;
    const t = now();
    const fp = fingerprint(items);
    const b = state.brief;
    if (!force) {
      if (b?.fingerprint === fp) return null;
      const sameDay = b && localDate(b.at) === localDate(t);
      if (!skipInterval) {
        if (sameDay && t - b.at < settings.briefMinutes * 60 * 1000) return null;
        // Outside work hours the brief only catches up once (the first of the day).
        if (sameDay && !inWorkHours(t, settings)) return null;
      }
    }
    briefing = (async () => {
      try {
        const listed = items;
        const answer = await ask({ prompt: briefPrompt({ items: listed, language: settings.language, now: t }), model: settings.briefModel });
        if (answer && typeof answer === 'object') note('brief', t, settings.briefModel, answer);
        const text = linkBrief(cleanBrief(typeof answer === 'string' ? answer : answer?.text), listed);
        if (!text.trim()) throw new Error('the model answered no bullets');
        state.brief = { text, at: t, model: settings.briefModel, fingerprint: fp };
        persist();
        log(`[inbox] brief over ${items.length} item(s) (${settings.briefModel})`);
      } catch (err) {
        log(`[inbox] brief failed: ${err?.message ?? err}`);
      } finally {
        briefing = null;
      }
    })();
    return briefing;
  }

  async function pass({ refresh = false } = {}) {
    if (passing) return passing;
    passing = (async () => {
      await Promise.all([
        readSource('github', async () => prItems(await cli.prs({ refresh }))),
        readSource('productive', async () => taskItems(await cli.tasks({ refresh }), now())),
        readSource('sessions', async () => sessionItems(await getFleet())),
      ]);
      merge();
      if (slackDue(now()) || (refresh && slack)) pollSlack();
      if (scanDue(now())) scan();
      maybeBrief();
    })().finally(() => {
      passing = null;
    });
    return passing;
  }

  function slackDue(t) {
    if (!slack || slackPolling) return false;
    const every = (settings.slackMinutes ?? 2) * 60 * 1000;
    // A pass comes every passMs: poll on the pass closest to the interval, not the one after it.
    return !state.slack.at || t - state.slack.at >= every - passMs / 2;
  }

  /** The Slack poller in the background: answered conversations leave, new ones are sorted. */
  function pollSlack() {
    if (slackPolling) return slackPolling;
    const started = now();
    sources.slack.running = true;
    slackPolling = (async () => {
      try {
        const r = await slack.poll();
        lists.slack = r.items;
        Object.assign(sources.slack, { at: started, ok: true, error: r.errors ? `${r.errors} conversation(s) could not be read` : null, count: r.items.length });
        if (r.classified) log(`[inbox] slack: ${r.items.length} open, ${r.classified} sorted, ${r.fetched} read in ${Math.round((now() - started) / 1000)}s`);
      } catch (err) {
        // Keep the last list; the next pass tries again.
        Object.assign(sources.slack, { ok: false, error: String(err?.message ?? err).slice(0, 300) });
        log(`[inbox] slack poll failed: ${sources.slack.error}`);
      } finally {
        sources.slack.running = false;
        slackPolling = null;
        merge();
        maybeBrief();
      }
    })();
    return slackPolling;
  }

  function schedule() {
    timer = setTimer(async () => {
      try {
        await pass();
      } catch (err) {
        log(`[inbox] pass failed: ${err?.message ?? err}`);
      }
      if (timer) schedule();
    }, passMs);
    timer?.unref?.();
  }

  function response() {
    const t = now();
    if (sources.comms) sources.comms.nextAt = sources.comms.running ? null : nextScanAt(t);
    return {
      enabled: true,
      host: settings.host,
      updatedAt,
      brief: state.brief ? { text: state.brief.text, at: state.brief.at, model: state.brief.model } : null,
      settings: {
        ...Object.fromEntries(SETTING_KEYS.map((k) => [k, settings[k] ?? null])),
        defaultScanRules: DEFAULT_SCAN_RULES,
      },
      usage: usageSummary(state.usage, t),
      sources,
      items,
    };
  }

  return {
    start() {
      if (timer) return;
      timer = true;
      pass().catch((err) => log(`[inbox] pass failed: ${err?.message ?? err}`));
      schedule();
    },
    stop() {
      if (timer && timer !== true) clearTimer(timer);
      timer = null;
    },
    response,
    /** Scan now: start a Slack + mail scan (also outside work hours), re-read every other source. */
    async refresh() {
      if (settings.comms && !scanning) scan({ manual: true });
      await pass({ refresh: true });
      return response();
    },
    /** Done by hand: off the list until its source drops it (messages, sessions: or it has newer activity). */
    dismiss(id) {
      const item = items.find((i) => i.id === id);
      if (!item) return null;
      state.dismissed[id] = { at: now(), itemAt: item.at ?? 0 };
      merge();
      return response();
    },
    /** New knobs (checked by checkInboxSettings), in effect at once. */
    updateSettings(patch) {
      Object.assign(settings, patch);
      if (settings.comms && !sources.comms) sources.comms = { at: state.comms.at, ok: !state.comms.error, error: state.comms.error ?? null, count: state.comms.items.length, nextAt: null, running: false };
      if (!settings.comms && sources.comms && !scanning) delete sources.comms;
      if ('comms' in patch) merge();
      return response();
    },
    status() {
      return {
        enabled: true,
        items: items.length,
        scanning: Boolean(scanning),
        lastScan: state.comms.at,
        brief: state.brief?.at ?? null,
      };
    },
    // Tests.
    pass,
    scan,
    pollSlack,
    maybeBrief,
  };
}
