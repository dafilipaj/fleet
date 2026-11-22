// Host load (docs/architecture.md → Host load): this server samples its machine's memory pressure
// and CPU load, keeps a state with hysteresis (`ok` / `hot` / `critical`) and writes it to
// `<fleet state>/load.json`, where the CLI reads it. While the host is `hot` or `critical`, new
// sessions wait: automatic ones (bots, schedule, Slack / GitHub spawns) are held until it
// recovers, one you start asks first (409 `overloaded` → the UI confirms → `ignoreLoad: true`). Nothing else
// is gated: send, peek, kill, tmux keep working, because they are how you get a host back.
//
// Fail-open everywhere: a sample that cannot be read counts as `ok`, a state file older than
// STALE_MS is ignored, and the whole thing is off unless `resources.enabled` is set.
import { promises as fsp } from 'node:fs';
import os from 'node:os';
import path from 'node:path';

export const LEVELS = ['ok', 'hot', 'critical'];
export const SAMPLE_MS = 15 * 1000;
/** The CLI and the API ignore a state older than this (the server is down, the machine slept). */
export const STALE_MS = 90 * 1000;
export const DEFAULTS = {
  enabled: false,
  /** 1-minute load average per core. */
  load: { hot: 2, critical: 4 },
  /** Linux PSI, avg60 in %: `some` for hot, `full` for critical. */
  psi: { hot: 10, critical: 10 },
  /** How long a higher level must hold before the state goes up, and a lower one before it comes down. */
  enterSeconds: 60,
  exitSeconds: 300,
};

const rank = (l) => Math.max(0, LEVELS.indexOf(l));
const num = (v, d) => (typeof v === 'number' && Number.isFinite(v) && v > 0 ? v : d);

/** Config `resources` → settings with defaults; anything malformed falls back to the default. */
export function loadSettings(raw) {
  const r = raw && typeof raw === 'object' ? raw : {};
  return {
    enabled: r.enabled === true,
    load: { hot: num(r.load?.hot, DEFAULTS.load.hot), critical: num(r.load?.critical, DEFAULTS.load.critical) },
    psi: { hot: num(r.psi?.hot, DEFAULTS.psi.hot), critical: num(r.psi?.critical, DEFAULTS.psi.critical) },
    enterSeconds: num(r.enterSeconds, DEFAULTS.enterSeconds),
    exitSeconds: num(r.exitSeconds, DEFAULTS.exitSeconds),
  };
}

/** `sysctl -n kern.memorystatus_vm_pressure_level vm.swapusage` → { pressure, swapUsedMb }. */
export function parseDarwin(stdout) {
  const lines = String(stdout ?? '').trim().split('\n');
  const level = Number.parseInt(lines[0], 10);
  // 1 normal, 2 warn, 4 critical
  const pressure = level >= 4 ? 'critical' : level >= 2 ? 'warn' : level >= 1 ? 'normal' : null;
  const used = /used = ([\d.]+)M/.exec(lines[1] ?? '');
  return { pressure, swapUsedMb: used ? Math.round(Number(used[1])) : null };
}

/** `/proc/pressure/memory` → { some, full } avg60 in %. */
export function parsePsi(text) {
  const out = { some: null, full: null };
  for (const line of String(text ?? '').split('\n')) {
    const m = /^(some|full) .*avg60=([\d.]+)/.exec(line);
    if (m) out[m[1]] = Number(m[2]);
  }
  return out;
}

/** One sample of this machine. Throws when nothing can be read (the caller counts that as ok). */
export async function sampleHost({ platform = process.platform, run, readFile = (p) => fsp.readFile(p, 'utf8') } = {}) {
  const cores = typeof os.availableParallelism === 'function' ? os.availableParallelism() : os.cpus().length;
  const load1 = os.loadavg()[0];
  const sample = { cores, load1: Math.round(load1 * 100) / 100, perCore: Math.round((load1 / Math.max(1, cores)) * 100) / 100 };
  if (platform === 'darwin') {
    const { stdout } = await run('/usr/sbin/sysctl', ['-n', 'kern.memorystatus_vm_pressure_level', 'vm.swapusage'], { timeout: 5000 });
    Object.assign(sample, parseDarwin(stdout));
  } else if (platform === 'linux') {
    try {
      sample.psi = parsePsi(await readFile('/proc/pressure/memory'));
    } catch {
      sample.psi = null; // no PSI (old kernel, container): CPU load only
    }
  }
  return sample;
}

/** A sample → { level, reasons }: the worse of memory and CPU. */
export function classify(sample, s = loadSettings({ enabled: true })) {
  let level = 'ok';
  const reasons = [];
  const raise = (l, why) => {
    if (rank(l) > rank(level)) level = l;
    reasons.push(why);
  };
  if (sample.pressure === 'critical') raise('critical', 'memory pressure critical');
  else if (sample.pressure === 'warn') raise('hot', 'memory pressure warn');
  if (sample.psi) {
    if (sample.psi.full != null && sample.psi.full >= s.psi.critical) raise('critical', `memory stalls ${sample.psi.full}% (full)`);
    else if (sample.psi.some != null && sample.psi.some >= s.psi.hot) raise('hot', `memory stalls ${sample.psi.some}% (some)`);
  }
  if (sample.perCore >= s.load.critical) raise('critical', `load ${sample.perCore}× cores`);
  else if (sample.perCore >= s.load.hot) raise('hot', `load ${sample.perCore}× cores`);
  return { level, reasons };
}

/**
 * The state machine: `state` follows the sampled level, but only once a higher level has held for
 * `enterSeconds` (or a lower one for `exitSeconds`). Pure: (prev, level, now) → next.
 */
export function step(prev, { level, reasons }, now, s) {
  const p = prev ?? { state: 'ok', since: now, upSince: null, downSince: null, reasons: [] };
  const next = { ...p, level, latest: reasons };
  if (rank(level) > rank(p.state)) {
    next.downSince = null;
    next.upSince = p.upSince ?? now;
    if (now - next.upSince >= s.enterSeconds * 1000) Object.assign(next, { state: level, since: now, upSince: null, reasons });
  } else if (rank(level) < rank(p.state)) {
    next.upSince = null;
    next.downSince = p.downSince ?? now;
    if (now - next.downSince >= s.exitSeconds * 1000) Object.assign(next, { state: level, since: now, downSince: null, reasons });
  } else {
    Object.assign(next, { upSince: null, downSince: null });
    if (rank(level) > 0) next.reasons = reasons;
  }
  return next;
}

/** "<host> is under heavy load (…) — …": the one wording the web, the CLI and the UI match on. */
export function busyMessage(host, st) {
  const why = st?.reasons?.length ? ` (${st.reasons.join(', ')})` : '';
  return `${host} is under heavy load${why} — new sessions wait until it recovers`;
}

export const BUSY_MARK = 'is under heavy load';

/** The sampler. `current()` → the public state, or null while off. */
export function createLoadMonitor({ settings, self, file, run, sample = (o) => sampleHost(o), now = Date.now, log = () => {} }) {
  let st = null;
  let lastSample = null;
  let error = null;
  let timer = null;

  async function write(body) {
    if (!file) return;
    const tmp = `${file}.${process.pid}.tmp`;
    await fsp.mkdir(path.dirname(file), { recursive: true });
    await fsp.writeFile(tmp, JSON.stringify(body), { mode: 0o600 });
    await fsp.rename(tmp, file);
  }

  async function tick() {
    const t = now();
    let cls;
    try {
      lastSample = await sample({ run });
      error = null;
      cls = classify(lastSample, settings);
    } catch (err) {
      // Fail-open: an unreadable machine is not a busy one.
      error = String(err?.message ?? err);
      cls = { level: 'ok', reasons: [] };
    }
    const prevState = st?.state ?? 'ok';
    st = step(st, cls, t, settings);
    if (st.state !== prevState) log(`[load] ${self}: ${prevState} → ${st.state}${st.reasons.length ? ` (${st.reasons.join(', ')})` : ''}`);
    try {
      await write(current());
    } catch (err) {
      log(`[load] could not write ${file}: ${err.message}`);
    }
    return current();
  }

  function current() {
    if (!st) return null;
    return {
      state: st.state,
      since: st.since,
      reasons: st.state === 'ok' ? [] : st.reasons,
      level: st.level,
      sample: lastSample,
      ...(error ? { error } : {}),
      updatedAt: now(),
    };
  }

  /** The 409 a spawn answers on a busy host, or null. */
  function busy() {
    const c = current();
    if (!c || c.state === 'ok') return null;
    return busyMessage(self, c);
  }

  return {
    tick,
    current,
    busy,
    start() {
      tick().catch(() => {});
      timer = setInterval(() => tick().catch(() => {}), SAMPLE_MS);
      timer.unref?.();
    },
    stop() {
      if (timer) clearInterval(timer);
      timer = null;
    },
  };
}

/** A state from a peer or the file: null when absent, malformed or stale. */
export function freshState(x, now = Date.now()) {
  if (!x || typeof x !== 'object' || !LEVELS.includes(x.state)) return null;
  if (typeof x.updatedAt !== 'number' || now - x.updatedAt > STALE_MS) return null;
  return x;
}

/**
 * Run `fn` (an automatic spawn); while the host answers busy, wait and try again — `onHold` once
 * with the message — for up to `maxMs`, then give up with the last error.
 */
export async function retryWhileBusy(fn, { onHold = () => {}, everyMs = 30 * 1000, maxMs = 60 * 60 * 1000, sleep = (ms) => new Promise((r) => setTimeout(r, ms)), now = Date.now } = {}) {
  const start = now();
  let held = false;
  for (;;) {
    try {
      return await fn();
    } catch (err) {
      if (!err?.overloaded || now() - start >= maxMs) throw err;
      if (!held) {
        held = true;
        try {
          await onHold(err.message);
        } catch {
          /* a note that does not post holds nothing up */
        }
      }
    }
    await sleep(everyMs);
  }
}

/**
 * On the Slack bridge's server: watch every host's load (this one's monitor, the peers'
 * `/api/load`) and DM the owner when one goes up, gets worse or recovers. A peer that does not
 * answer is skipped (a sleeping laptop is not an alert). What was last told is kept in `file`, so
 * a restart does not repeat it.
 */
export function createLoadAlerts({ hosts, fetchLoad, notify, file, now = Date.now, log = () => {} }) {
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
      log(`[load] could not write ${file}: ${err.message}`);
    }
  }

  function text(host, st, prev) {
    if (st.state === 'ok') {
      const mins = prev?.since ? Math.max(1, Math.round((now() - prev.since) / 60000)) : null;
      return `:white_check_mark: *${host}* has recovered${mins ? ` (under load for ${mins} min)` : ''}. New sessions start again; held ones go in now.`;
    }
    const icon = st.state === 'critical' ? ':rotating_light:' : ':warning:';
    const why = st.reasons?.length ? `: ${st.reasons.join(', ')}` : '';
    return `${icon} *${host}* is under heavy load (${st.state}${why}). New automatic sessions wait until it recovers; one you start asks first.`;
  }

  async function tick() {
    await load();
    let changed = false;
    for (const host of hosts()) {
      let st;
      try {
        st = freshState(await fetchLoad(host), now());
      } catch {
        st = null;
      }
      if (!st) continue;
      const prev = told[host] ?? { state: 'ok' };
      if (st.state === prev.state) continue;
      // Only up, worse, or back to ok: critical → hot is still under load, no new message.
      if (st.state === 'hot' && prev.state === 'critical') {
        told[host] = { ...prev, state: 'hot' };
        changed = true;
        continue;
      }
      try {
        await notify(text(host, st, prev));
        log(`[load] told the owner: ${host} ${prev.state} → ${st.state}`);
        told[host] = { state: st.state, since: st.state === 'ok' ? null : prev.state === 'ok' ? st.since ?? now() : prev.since };
        changed = true;
      } catch (err) {
        log(`[load] could not DM the owner about ${host}: ${err.message}`);
      }
    }
    if (changed) await save();
  }

  return { tick };
}
