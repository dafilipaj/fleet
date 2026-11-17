// The inbox's Slack source: a poller that reads Slack as the user (a user token, read-only) and
// decides what is answered without a model; a model only sorts the conversations that have new
// messages from other people, from a compact digest, with no tools.
//
// Every `slackMinutes`:
//   1. three searches since the last poll: `to:me` (DMs), `<@me>` (mentions), `from:me` (what the
//      user answered, and the threads they take part in)
//   2. every touched conversation, every open one and every thread the user posted in lately
//      (watched for TRACK_MS) is re-read: a DM's channel history, a channel message's thread
//   3. a conversation is closed when the newest message from a person is the user's, or the user
//      reacted to it — at once, no model
//   4. an open conversation whose newest message from someone else is new since its last verdict
//      goes to one model call (no tools) with its last messages → needsMe, title, ask, effort
//
// Items: the open conversations the model said need the user. Ids: `slack:<channel>:<root ts>` for
// a thread or a channel message, `slack:<channel>` for a DM's top level.

/** The Keychain item (or env var) with the user token (`xoxp-…`). */
export const SLACK_USER_TOKEN_NAME = 'FLEET_INBOX_SLACK_TOKEN';
/** Scopes the token needs (Settings → Inbox shows them when one is missing). */
export const SLACK_USER_SCOPES = ['search:read', 'channels:history', 'groups:history', 'im:history', 'mpim:history', 'im:read', 'mpim:read', 'users:read', 'reactions:read'];

const DAY_MS = 24 * 3600 * 1000;
/** The first poll (and one after a long gap) looks this far back. */
export const FIRST_LOOKBACK_MS = 7 * DAY_MS;
/** A poll starts this long before the previous one (search indexing lags a little). */
export const POLL_OVERLAP_MS = 10 * 60 * 1000;
/** A thread the user posted in is re-read this long after their last post there. */
export const TRACK_MS = 3 * DAY_MS;
/** At most this many watched threads (newest first), so a poll stays within Slack's rate limits. */
export const MAX_TRACKED = 40;
/** Messages per conversation in the model's digest, and characters per message. */
export const DIGEST_MESSAGES = 8;
export const DIGEST_CHARS = 500;
/** Conversations per model call. */
export const CLASSIFY_BATCH = 15;
/** A conversation nobody wrote in for this long drops off. */
export const STALE_MS = 14 * DAY_MS;

const tsMs = (ts) => Math.round(Number(ts) * 1000);

/** A message from a person (not a bot, a join, a topic change…). Pure. */
export function isHuman(m) {
  if (!m?.user || m.bot_id || m.bot_profile) return false;
  return !m.subtype || m.subtype === 'thread_broadcast' || m.subtype === 'file_share' || m.subtype === 'me_message';
}

/** `thread_ts` from a Slack permalink, or null. Pure. */
export function threadOfPermalink(permalink) {
  const m = /[?&]thread_ts=(\d+\.\d+)/.exec(String(permalink ?? ''));
  return m ? m[1] : null;
}

/** A search match → its conversation `{ key, channel, root, dm, label }`. Pure. */
export function conversationOf(match) {
  const ch = match?.channel ?? {};
  const dm = Boolean(ch.is_im || ch.is_mpim);
  const thread = threadOfPermalink(match.permalink) ?? (match.thread_ts && match.thread_ts !== match.ts ? match.thread_ts : null);
  const label = dm ? (ch.is_mpim ? 'group DM' : 'DM') : `#${ch.name ?? ch.id}`;
  if (thread) return { key: `slack:${ch.id}:${thread}`, channel: ch.id, root: thread, dm, label };
  if (dm) return { key: `slack:${ch.id}`, channel: ch.id, root: null, dm, label };
  return { key: `slack:${ch.id}:${match.ts}`, channel: ch.id, root: match.ts, dm, label };
}

/**
 * A conversation's messages (oldest first) → { open, lastOther, since } — open when the newest
 * message from a person is not the user's and the user has not reacted to it; `since` = the
 * messages after the user's last one. Pure.
 */
export function conversationState(messages, me) {
  const human = (messages ?? []).filter(isHuman);
  const last = human.at(-1);
  if (!last) return { open: false, reason: 'empty' };
  if (last.user === me) return { open: false, reason: 'answered' };
  if ((last.reactions ?? []).some((r) => (r.users ?? []).includes(me))) return { open: false, reason: 'reacted' };
  let mine = -1;
  for (let i = human.length - 1; i >= 0; i--) if (human[i].user === me) { mine = i; break; }
  return { open: true, lastOther: last, since: human.slice(mine + 1), answeredBefore: mine >= 0 };
}

/** Message text for the model: user ids → names, the user → @me, links shortened, clipped. Pure. */
export function plainText(text, names, me, max = DIGEST_CHARS) {
  const t = String(text ?? '')
    .replace(/<@([UW][A-Z0-9]+)(?:\|[^>]*)?>/g, (_, id) => (id === me ? '@me' : `@${names[id] ?? id}`))
    .replace(/<!subteam\^[A-Z0-9]+(?:\|([^>]*))?>/g, (_, n) => `@${n ?? 'group'}`)
    .replace(/<(https?:[^|>]+)\|([^>]+)>/g, '$2 ($1)')
    .replace(/<(https?:[^>]+)>/g, '$1')
    .replace(/\s+/g, ' ')
    .trim();
  return t.length > max ? `${t.slice(0, max - 1)}…` : t;
}

/** A message permalink built without an API call. Pure. */
export function permalink(base, channel, ts, root = null) {
  if (!base) return null;
  const p = `${base.replace(/\/$/, '')}/archives/${channel}/p${String(ts).replace('.', '')}`;
  return root && root !== ts ? `${p}?thread_ts=${root}&cid=${channel}` : p;
}

/** The model's prompt over the conversations that need a verdict. Pure. */
export function classifyPrompt({ convs, meName, rules = null, now = Date.now() }) {
  const blocks = convs.map((c) => [
    `=== ${c.key} · ${c.label}${c.root && !c.dm ? ' (thread)' : ''}${c.answeredBefore ? ' · the user already wrote here before' : ''}`,
    ...c.digest.map((m) => `[${new Date(m.at).toISOString().slice(0, 16).replace('T', ' ')}] ${m.who}: ${m.text}`),
  ].join('\n'));
  return [
    `You sort Slack conversations for the to-do inbox of ${meName} (written "me" / "@me" below). Now: ${new Date(now).toISOString()}.`,
    'In each conversation below the newest messages are from other people and the user has not answered them yet. Decide for each whether it waits on the user to act or answer.',
    'needsMe = true when someone asks the user something, asks them for a review, decision, approval or action, or clearly expects the user\'s reply.',
    'needsMe = false for FYI and announcements, talk between other people that does not need the user, closers ("thanks", "ok", "np", 👍), things someone else already took over or answered, and automated posts.',
    ...(String(rules ?? '').trim() ? ['', 'The user\'s own rules (they win):', String(rules).trim()] : []),
    '',
    'Answer with ONLY a JSON array (no prose, no code fence), one object per conversation, in any order:',
    '{"key":"<the conversation key after ===>","needsMe":true|false,"title":"<what they want, ≤ 80 chars, in the conversation\'s language>","ask":"<one sentence: what exactly the user should do>","effort":"quick"|"medium"|"deep"}',
    'quick = under ~5 minutes (a yes/no, a short answer, a link, a quick look). title and ask may be empty when needsMe is false.',
    '',
    ...blocks,
  ].join('\n');
}

const EFFORTS = new Set(['quick', 'medium', 'deep']);
const clip = (s, n) => {
  const t = String(s ?? '').replace(/\s+/g, ' ').trim();
  return t.length > n ? `${t.slice(0, n - 1)}…` : t;
};

/** The model's array → verdicts by key (only for the keys asked). Pure. */
export function readVerdicts(raw, keys) {
  const want = new Set(keys);
  const out = {};
  for (const r of raw ?? []) {
    if (!r || typeof r !== 'object' || !want.has(r.key)) continue;
    out[r.key] = {
      needsMe: r.needsMe === true,
      title: clip(r.title, 120),
      ask: r.ask ? clip(r.ask, 240) : null,
      effort: EFFORTS.has(r.effort) ? r.effort : 'medium',
    };
  }
  return out;
}

/** An empty poller state (kept in the inbox's state file under `slack`). */
export const emptySlackState = () => ({ at: null, me: null, meName: null, base: null, names: {}, convs: {}, tracked: {}, verdicts: {} });

/**
 * deps:
 *   api        lib/slack.mjs createSlackApi with the user token → (method, params) => body
 *   classify   ({ prompt }) => Promise<{ text, … }> (a model call, no tools; counted by the caller)
 *   state      the persisted poller state (emptySlackState()), changed in place
 *   rules      () => the user's extra rules for the model, or null
 * → { poll(): Promise<{ items, classified, fetched }> , items() }
 */
export function createSlackPoller({ api, classify, state, rules = () => null, now = Date.now, log = () => {}, concurrency = 4 }) {
  async function whoAmI() {
    if (state.me && state.base) return;
    const a = await api('auth.test');
    state.me = a.user_id;
    state.base = String(a.url ?? '').replace(/\/$/, '') || null;
    state.meName = await nameOf(a.user_id);
  }

  async function nameOf(id) {
    if (!id) return '?';
    if (state.names[id]) return state.names[id];
    try {
      const u = (await api('users.info', { user: id })).user ?? {};
      state.names[id] = u.real_name || u.profile?.real_name || u.profile?.display_name || u.name || id;
    } catch {
      state.names[id] = id;
    }
    return state.names[id];
  }

  async function search(query) {
    const out = [];
    for (let page = 1; page <= 3; page++) {
      const r = await api('search.messages', { query, sort: 'timestamp', sort_dir: 'desc', count: 100, page });
      const m = r.messages ?? {};
      out.push(...(m.matches ?? []));
      if (page >= (m.paging?.pages ?? 1)) break;
    }
    return out;
  }

  /** A conversation's messages, oldest first. */
  async function read(c) {
    if (c.root) {
      const all = [];
      let cursor;
      for (let i = 0; i < 5; i++) {
        const r = await api('conversations.replies', { channel: c.channel, ts: c.root, limit: 200, ...(cursor ? { cursor } : {}) });
        all.push(...(r.messages ?? []));
        cursor = r.response_metadata?.next_cursor;
        if (!r.has_more || !cursor) break;
      }
      return all;
    }
    const r = await api('conversations.history', { channel: c.channel, limit: 30 });
    return [...(r.messages ?? [])].reverse();
  }

  async function pool(list, fn) {
    const queue = [...list];
    const errors = [];
    await Promise.all(
      Array.from({ length: Math.min(concurrency, queue.length) }, async () => {
        while (queue.length) {
          const x = queue.shift();
          try {
            await fn(x);
          } catch (err) {
            errors.push(err);
          }
        }
      }),
    );
    return errors;
  }

  async function poll() {
    const started = now();
    await whoAmI();
    const me = state.me;
    const since = state.at && started - state.at < FIRST_LOOKBACK_MS ? state.at - POLL_OVERLAP_MS : started - FIRST_LOOKBACK_MS;
    // `after:` is a date, exclusive: from the day before, then filtered by time.
    const after = new Date(since - DAY_MS).toISOString().slice(0, 10);
    const [toMe, mentions, fromMe] = await Promise.all([search(`to:me after:${after}`), search(`<@${me}> after:${after}`), search(`from:me after:${after}`)]);
    const fresh = (m) => tsMs(m.ts) >= since;

    const touched = new Map();
    const touch = (c) => touched.set(c.key, { ...c, label: state.convs[c.key]?.label ?? c.label });
    for (const m of [...toMe, ...mentions].filter(fresh)) {
      if (m.user === me) continue;
      touch(conversationOf(m));
    }
    for (const m of fromMe.filter(fresh)) {
      const c = conversationOf(m);
      touch(c);
      const until = tsMs(m.ts) + TRACK_MS;
      if (!state.tracked[c.key] || state.tracked[c.key].until < until) state.tracked[c.key] = { ...c, until };
    }
    // Watched threads (newest first, capped) and every open conversation are read again.
    const tracked = Object.values(state.tracked).filter((t) => t.until > started).sort((a, b) => b.until - a.until).slice(0, MAX_TRACKED);
    state.tracked = Object.fromEntries(tracked.map((t) => [t.key, t]));
    for (const t of tracked) if (!touched.has(t.key)) touched.set(t.key, t);
    for (const c of Object.values(state.convs)) if (c.open && !touched.has(c.key)) touched.set(c.key, c);

    let fetched = 0;
    const errors = await pool([...touched.values()], async (c) => {
      const messages = await read(c);
      fetched++;
      const st = conversationState(messages, me);
      if (!st.open) {
        delete state.convs[c.key];
        return;
      }
      const ids = new Set(st.since.map((m) => m.user));
      if (c.root) ids.add(messages.find((m) => m.ts === c.root)?.user);
      for (const id of ids) await nameOf(id);
      if (c.dm && c.label === 'DM') c.label = `DM with ${state.names[st.lastOther.user] ?? st.lastOther.user}`;
      const rootMsg = c.root ? messages.find((m) => m.ts === c.root) : null;
      const tail = st.since.slice(-DIGEST_MESSAGES);
      const digest = [...(rootMsg && !tail.includes(rootMsg) ? [rootMsg] : []), ...tail].map((m) => ({
        at: tsMs(m.ts),
        who: m.user === me ? 'me' : (state.names[m.user] ?? m.user),
        text: plainText(m.text, state.names, me),
      }));
      state.convs[c.key] = {
        key: c.key,
        channel: c.channel,
        root: c.root,
        dm: c.dm,
        label: c.label,
        open: true,
        lastTs: st.lastOther.ts,
        from: state.names[st.lastOther.user] ?? st.lastOther.user,
        url: permalink(state.base, c.channel, st.lastOther.ts, c.root),
        answeredBefore: st.answeredBefore,
        digest,
      };
    });
    if (errors.length) log(`[inbox] slack: ${errors.length} conversation(s) not read: ${errors[0]?.message ?? errors[0]}`);

    // Stale conversations leave (closed ones already did); their verdicts too.
    for (const [key, c] of Object.entries(state.convs)) if (started - tsMs(c.lastTs) > STALE_MS) delete state.convs[key];
    for (const key of Object.keys(state.verdicts)) if (!state.convs[key]) delete state.verdicts[key];

    // A verdict only for what has a new message from someone else since the last one.
    const pending = Object.values(state.convs).filter((c) => state.verdicts[c.key]?.lastTs !== c.lastTs);
    let classified = 0;
    for (let i = 0; i < pending.length; i += CLASSIFY_BATCH) {
      const batch = pending.slice(i, i + CLASSIFY_BATCH);
      const res = await classify({ prompt: classifyPrompt({ convs: batch, meName: state.meName ?? 'the user', rules: rules(), now: started }) });
      const raw = extractArray(res?.text);
      if (!raw) throw new Error(`the Slack sort answered no JSON array: ${String(res?.text ?? '').slice(0, 160)}`);
      const v = readVerdicts(raw, batch.map((c) => c.key));
      for (const c of batch) if (v[c.key]) state.verdicts[c.key] = { ...v[c.key], lastTs: c.lastTs };
      classified += batch.length;
    }
    state.at = started;
    return { items: items(), classified, fetched, errors: errors.length };
  }

  /** The open conversations that need the user → inbox items. */
  function items() {
    const out = [];
    for (const c of Object.values(state.convs)) {
      const v = state.verdicts[c.key];
      // A newer message not sorted yet (a failed call) keeps the last verdict.
      if (!c.open || !v?.needsMe) continue;
      out.push({
        id: c.key,
        source: 'slack',
        kind: 'message',
        section: v.effort === 'quick' ? 'quick' : 'waiting',
        effort: v.effort,
        title: v.title || `${c.from} in ${c.label}`,
        ask: v.ask,
        url: c.url,
        href: null,
        from: c.from,
        at: tsMs(c.lastTs),
        meta: { channel: c.label },
      });
    }
    return out;
  }

  return { poll, items };
}

/** The first JSON array in a model answer, or null. */
function extractArray(text) {
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
