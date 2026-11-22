// Subscription fallback, the web side (docs/architecture.md → Subscription fallback). The CLI
// decides (`fleet usage check`, crates/fleet/src/core/fallback.rs); this module holds what the
// server needs around it: the settings (config `usage`, opt-in), their validation for Settings →
// Usage, the hold mark, and the owner's Slack DMs when a subscription reaches its limit or
// recovers.

import fsp from 'node:fs/promises';
import path from 'node:path';

/** Must match `USAGE_MARK` in core/fallback.rs: a spawn held for usage says it. */
export const USAGE_MARK = 'is at its usage limit';
export const MAX_OVERRIDE_MINUTES = 1440;
export const HARNESSES = ['claude', 'codex', 'pi'];
export const DEFAULTS = Object.freeze({
  enabled: false,
  fallback: Object.freeze({ claude: 'codex', codex: 'claude', pi: 'claude' }),
  limits: Object.freeze({ session: 90, weekly: 95, paceMargin: 15 }),
});

/** Config `usage` → full settings (lenient, like the CLI: a bad value keeps its default). */
export function usageSettings(raw) {
  const r = raw && typeof raw === 'object' ? raw : {};
  const fallback = { ...DEFAULTS.fallback };
  if (r.fallback && typeof r.fallback === 'object') {
    for (const [from, to] of Object.entries(r.fallback)) {
      if (!HARNESSES.includes(from)) continue;
      if (to === null || to === '' || to === 'none') fallback[from] = null;
      else if (HARNESSES.includes(to) && to !== from) fallback[from] = to;
    }
  }
  const pct = (v, def, min = 1) => (typeof v === 'number' && Number.isFinite(v) && v >= min && v <= 100 ? v : def);
  const l = r.limits && typeof r.limits === 'object' ? r.limits : {};
  return {
    enabled: r.enabled === true,
    fallback,
    limits: {
      session: pct(l.session, DEFAULTS.limits.session),
      weekly: pct(l.weekly, DEFAULTS.limits.weekly),
      paceMargin: pct(l.paceMargin, DEFAULTS.limits.paceMargin, 0),
    },
  };
}

/** A Settings → Usage PUT → the whole object to store; throws `{ status: 400 }` on junk. */
export function usagePatch(body, cur) {
  const bad = (msg) => Object.assign(new Error(msg), { status: 400 });
  if (!body || typeof body !== 'object' || Array.isArray(body)) throw bad('body must be an object');
  const next = { enabled: cur.enabled, fallback: { ...cur.fallback }, limits: { ...cur.limits } };
  if ('enabled' in body) {
    if (typeof body.enabled !== 'boolean') throw bad('enabled must be true or false');
    next.enabled = body.enabled;
  }
  if ('fallback' in body) {
    const f = body.fallback;
    if (!f || typeof f !== 'object' || Array.isArray(f)) throw bad('fallback must be an object');
    for (const [from, to] of Object.entries(f)) {
      if (!HARNESSES.includes(from)) throw bad(`fallback: unknown harness ${from}`);
      if (to === null) next.fallback[from] = null;
      else if (HARNESSES.includes(to) && to !== from) next.fallback[from] = to;
      else throw bad(`fallback.${from} must be another harness or null`);
    }
  }
  if ('limits' in body) {
    const l = body.limits;
    if (!l || typeof l !== 'object') throw bad('limits must be an object');
    for (const k of ['session', 'weekly', 'paceMargin']) {
      if (!(k in l)) continue;
      const min = k === 'paceMargin' ? 0 : 1;
      if (typeof l[k] !== 'number' || !Number.isFinite(l[k]) || l[k] < min || l[k] > 100) throw bad(`limits.${k} must be a number from ${min} to 100`);
      next.limits[k] = l[k];
    }
  }
  return next;
}

/** A spawn's 409 body when it was held for usage (Slack and GitHub retry on `overloaded`). */
export function isUsageHold(err) {
  return String(err?.message ?? '').includes(USAGE_MARK);
}

/**
 * On the Slack bridge's server: ask every host where a claude and a codex session would start
 * (`check(host, harness)` → `fleet usage check` there, `null` when it does not answer or the
 * check is off) and DM the owner when a subscription reaches its limit or recovers. What was
 * last told is kept in `file`, so a restart does not repeat it.
 */
export function createUsageAlerts({ hosts, check, notify, file, now = Date.now, log = () => {} }) {
  let told = null;

  async function load() {
    if (told) return;
    try {
      told = JSON.parse(await fsp.readFile(file, 'utf8')) ?? {};
    } catch {
      told = {};
    }
  }
  async function save() {
    try {
      await fsp.mkdir(path.dirname(file), { recursive: true });
      await fsp.writeFile(file, JSON.stringify(told), { mode: 0o600 });
    } catch (err) {
      log(`[usage] could not write ${file}: ${err.message}`);
    }
  }

  const LABEL = { claude: 'Claude', openai: 'ChatGPT' };

  function text(host, provider, st, prev, fallback) {
    const who = `*${host}*'s ${LABEL[provider] ?? provider} subscription`;
    if (!st.exhausted) {
      const mins = prev?.since ? Math.max(1, Math.round((now() - prev.since) / 60000)) : null;
      return `:white_check_mark: ${who} is below its limit again${mins ? ` (after ${mins} min)` : ''}. Sessions start on it again.`;
    }
    const then = fallback ? `New sessions on it start on ${fallback} instead` : 'There is no usable fallback: automatic sessions wait, one you start runs on it anyway';
    return `:hourglass_flowing_sand: ${who} is at its limit (${st.reasons.join(', ')}). ${then}.`;
  }

  async function tick() {
    await load();
    let changed = false;
    for (const host of hosts()) {
      // One check per harness family: what a claude session and a codex session would get.
      for (const harness of ['claude', 'codex']) {
        let d;
        try {
          d = await check(host, harness);
        } catch {
          d = null;
        }
        if (!d?.enabled || !d.primaryState) continue;
        const st = d.primaryState;
        const key = `${host}/${st.provider}`;
        const prev = told[key] ?? { exhausted: false };
        if (st.exhausted === prev.exhausted) continue;
        const fallback = d.fallback?.usable && !d.fallback.state?.exhausted ? d.fallback.harness : null;
        try {
          await notify(text(host, st.provider, st, prev, fallback));
          log(`[usage] told the owner: ${key} ${st.exhausted ? 'at its limit' : 'recovered'}`);
          told[key] = { exhausted: st.exhausted, since: st.exhausted ? now() : null };
          changed = true;
        } catch (err) {
          log(`[usage] could not DM the owner about ${key}: ${err.message}`);
        }
      }
    }
    if (changed) await save();
  }

  return { tick };
}
