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
  /** 5-minute load average per core (a build that takes every core for a few minutes stays below). */
  load: { hot: 3, critical: 5 },
  /** macOS: free memory in % (`kern.memorystatus_level`) below which it is hot / critical. Memory
   * pressure `warn` alone is the kernel compressing, not trouble; `critical` always counts. */
  memory: { hot: 10, critical: 5 },
  /** Linux PSI, avg60 in %: `some` for hot, `full` for critical. */
  psi: { hot: 10, critical: 10 },
  /** How long a higher level must hold before the state goes up (critical: `criticalEnterSeconds`),
   * and a lower one before it comes down. */
  enterSeconds: 300,
  criticalEnterSeconds: 60,
  exitSeconds: 300,
  /** DM the owner when this host goes critical and when it recovers (the Slack bridge's server
   * sends them). Off for a second fleet on the same machine, so one machine alerts once. */
  alerts: true,
};

const rank = (l) => Math.max(0, LEVELS.indexOf(l));
const num = (v, d) => (typeof v === 'number' && Number.isFinite(v) && v > 0 ? v : d);

/** Config `resources` → settings with defaults; anything malformed falls back to the default. */
export function loadSettings(raw) {
  const r = raw && typeof raw === 'object' ? raw : {};
  return {
    enabled: r.enabled === true,
    load: { hot: num(r.load?.hot, DEFAULTS.load.hot), critical: num(r.load?.critical, DEFAULTS.load.critical) },
    memory: { hot: num(r.memory?.hot, DEFAULTS.memory.hot), critical: num(r.memory?.critical, DEFAULTS.memory.critical) },
    psi: { hot: num(r.psi?.hot, DEFAULTS.psi.hot), critical: num(r.psi?.critical, DEFAULTS.psi.critical) },
    enterSeconds: num(r.enterSeconds, DEFAULTS.enterSeconds),
    criticalEnterSeconds: num(r.criticalEnterSeconds, DEFAULTS.criticalEnterSeconds),
    exitSeconds: num(r.exitSeconds, DEFAULTS.exitSeconds),
    alerts: r.alerts !== false,
  };
}

/**
 * `sysctl -n kern.memorystatus_vm_pressure_level kern.memorystatus_level vm.swapusage` →
 * { pressure, freePct, swapUsedMb }. (An older two-line answer, without the free %, still parses.)
 */
export function parseDarwin(stdout) {
  const lines = String(stdout ?? '').trim().split('\n');
  const level = Number.parseInt(lines[0], 10);
  // 1 normal, 2 warn, 4 critical
  const pressure = level >= 4 ? 'critical' : level >= 2 ? 'warn' : level >= 1 ? 'normal' : null;
  const swapLine = lines.find((l) => /used = /.test(l)) ?? '';
  const used = /used = ([\d.]+)M/.exec(swapLine);
  const free = lines.length >= 3 && /^\s*\d+\s*$/.test(lines[1]) ? Number.parseInt(lines[1], 10) : null;
  return { pressure, freePct: free != null && free >= 0 && free <= 100 ? free : null, swapUsedMb: used ? Math.round(Number(used[1])) : null };
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
  const [load1, load5] = os.loadavg();
  const r2 = (x) => Math.round(x * 100) / 100;
  // perCore is the 5-minute average: what decides. load1 is shown beside it.
  const sample = { cores, load1: r2(load1), load5: r2(load5), perCore: r2(load5 / Math.max(1, cores)) };
  if (platform === 'darwin') {
    const { stdout } = await run('/usr/sbin/sysctl', ['-n', 'kern.memorystatus_vm_pressure_level', 'kern.memorystatus_level', 'vm.swapusage'], { timeout: 5000 });
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
  const mem = s.memory ?? DEFAULTS.memory;
  if (sample.pressure === 'critical') raise('critical', 'memory pressure critical');
  if (sample.freePct != null) {
    if (sample.freePct < mem.critical) raise('critical', `${sample.freePct}% memory free`);
    else if (sample.freePct < mem.hot) raise('hot', `${sample.freePct}% memory free`);
  }
  if (sample.psi) {
    if (sample.psi.full != null && sample.psi.full >= s.psi.critical) raise('critical', `memory stalls ${sample.psi.full}% (full)`);
    else if (sample.psi.some != null && sample.psi.some >= s.psi.hot) raise('hot', `memory stalls ${sample.psi.some}% (some)`);
  }
  if (sample.perCore >= s.load.critical) raise('critical', `5-min load ${sample.perCore}× cores`);
  else if (sample.perCore >= s.load.hot) raise('hot', `5-min load ${sample.perCore}× cores`);
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
    const wait = level === 'critical' ? (s.criticalEnterSeconds ?? s.enterSeconds) : s.enterSeconds;
    if (now - next.upSince >= wait * 1000) Object.assign(next, { state: level, since: now, upSince: null, reasons });
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

/** The longest a manual override ("allow new sessions") may run. */
export const MAX_OVERRIDE_MINUTES = 24 * 60;

/**
 * The sampler. It always samples (Settings → Host load shows the numbers even while off), but it
 * holds spawns and writes the state file only while `settings.enabled`. `configure()` applies
 * new settings live; `override(minutes)` lets new sessions start for a while whatever the load.
 */
export function createLoadMonitor({ settings: initial, self, file, run, sample = (o) => sampleHost(o), now = Date.now, log = () => {} }) {
  let settings = initial;
  let st = null;
  let lastSample = null;
  let error = null;
  let timer = null;
  let overrideUntil = 0;

  async function write(body) {
    if (!file) return;
    if (!body) {
      await fsp.rm(file, { force: true });
      return;
    }
    const tmp = `${file}.${process.pid}.tmp`;
    await fsp.mkdir(path.dirname(file), { recursive: true });
    await fsp.writeFile(tmp, JSON.stringify(body), { mode: 0o600 });
    await fsp.rename(tmp, file);
  }

  const overridden = () => overrideUntil > now();

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
    if (st.state !== prevState && settings.enabled) log(`[load] ${self}: ${prevState} → ${st.state}${st.reasons.length ? ` (${st.reasons.join(', ')})` : ''}`);
    if (overrideUntil && !overridden()) {
      overrideUntil = 0;
      log(`[load] ${self}: override ended`);
    }
    try {
      await write(settings.enabled ? current() : null);
    } catch (err) {
      log(`[load] could not write ${file}: ${err.message}`);
    }
    return current();
  }

  /** The verdict: what holds spawns (`state`), what the sample says now (`level`), the numbers. */
  function current() {
    if (!st) return null;
    // An override reads as ok to everyone that gates on it (the CLI, the spawner, the alerts).
    const state = overridden() ? 'ok' : st.state;
    return {
      state,
      since: st.since,
      reasons: state === 'ok' ? [] : st.reasons,
      level: st.level,
      latest: st.latest ?? [],
      sample: lastSample,
      ...(overridden() ? { override: { until: overrideUntil, held: st.state } } : {}),
      ...(error ? { error } : {}),
      updatedAt: now(),
    };
  }

  /** The 409 a spawn answers on a busy host, or null. */
  function busy() {
    if (!settings.enabled) return null;
    const c = current();
    if (!c || c.state === 'ok') return null;
    return busyMessage(self, c);
  }

  return {
    tick,
    current,
    busy,
    enabled: () => settings.enabled,
    settings: () => settings,
    /** New settings, live (Settings → Host load); the state machine keeps its history. */
    configure(next) {
      const was = settings.enabled;
      settings = next;
      if (was !== next.enabled) log(`[load] ${self}: monitor ${next.enabled ? 'on' : 'off'}`);
      return tick();
    },
    /** Let new sessions start for `minutes` whatever the load (0: end it now). */
    override(minutes) {
      overrideUntil = minutes > 0 ? now() + Math.min(minutes, MAX_OVERRIDE_MINUTES) * 60 * 1000 : 0;
      log(`[load] ${self}: ${minutes > 0 ? `new sessions allowed for ${minutes} min whatever the load` : 'override cleared'}`);
      return tick();
    },
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

/** `ps -axo pid=,ppid=,rss=,pcpu=,comm=` → [{ pid, ppid, rssKb, cpu, comm }]. */
export function parsePs(stdout) {
  const out = [];
  for (const line of String(stdout ?? '').split('\n')) {
    const m = /^\s*(\d+)\s+(\d+)\s+(\d+)\s+([\d.]+)\s+(.+?)\s*$/.exec(line);
    if (m) out.push({ pid: Number(m[1]), ppid: Number(m[2]), rssKb: Number(m[3]), cpu: Number(m[4]), comm: m[5] });
  }
  return out;
}

/**
 * What uses this machine: each session's process tree summed (its agent and everything it runs),
 * heaviest first, and the heaviest other processes. `sessions`: this host's rows (`pid`).
 */
export function processUse(procs, sessions = [], { others = 8 } = {}) {
  const kids = new Map();
  for (const p of procs) {
    if (!kids.has(p.ppid)) kids.set(p.ppid, []);
    kids.get(p.ppid).push(p);
  }
  const byPid = new Map(procs.map((p) => [p.pid, p]));
  const claimed = new Set();
  const tree = (pid) => {
    const all = [];
    const stack = [pid];
    while (stack.length) {
      const x = stack.pop();
      const p = byPid.get(x);
      if (!p || claimed.has(x)) continue;
      claimed.add(x);
      all.push(p);
      for (const k of kids.get(x) ?? []) stack.push(k.pid);
    }
    return all;
  };
  const mb = (kb) => Math.round(kb / 1024);
  const rows = [];
  for (const s of sessions) {
    if (!Number.isInteger(s?.pid) || !byPid.has(s.pid)) continue;
    const t = tree(s.pid);
    rows.push({
      id: s.session_id ?? null,
      name: s.name || s.tmux_session || s.title || `pid ${s.pid}`,
      status: s.status ?? null,
      pid: s.pid,
      processes: t.length,
      rssMb: mb(t.reduce((a, p) => a + p.rssKb, 0)),
      cpu: Math.round(t.reduce((a, p) => a + p.cpu, 0) * 10) / 10,
    });
  }
  rows.sort((a, b) => b.rssMb - a.rssMb);
  const rest = procs
    .filter((p) => !claimed.has(p.pid))
    .sort((a, b) => b.rssKb - a.rssKb)
    .slice(0, others)
    .map((p) => ({ pid: p.pid, name: path.basename(p.comm), rssMb: mb(p.rssKb), cpu: p.cpu }));
  const total = procs.reduce((a, p) => a + p.rssKb, 0);
  return { sessions: rows, others: rest, totalMb: mb(total), sessionsMb: rows.reduce((a, r) => a + r.rssMb, 0) };
}

/** Run `ps` here and break it down by session. */
export async function readProcessUse({ run, sessions }) {
  const { stdout } = await run('/bin/ps', ['-axo', 'pid=,ppid=,rss=,pcpu=,comm='], { timeout: 10000, maxBuffer: 16 * 1024 * 1024 });
  return processUse(parsePs(stdout), sessions);
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

  // Only `critical` is worth a DM: `hot` holds automatic sessions quietly (the gauge in the UI
  // shows it) and passes on its own. The recovery DM follows only a critical one.
  function text(host, st, prev) {
    if (st.state !== 'critical') {
      const mins = prev?.since ? Math.max(1, Math.round((now() - prev.since) / 60000)) : null;
      const still = st.state === 'hot' ? ' It is still busy, so automatic sessions keep waiting a little.' : ' New sessions start again; held ones go in now.';
      return `:white_check_mark: *${host}* is no longer critical${mins ? ` (after ${mins} min)` : ''}.${still}`;
    }
    const why = st.reasons?.length ? `: ${st.reasons.join(', ')}` : '';
    return `:rotating_light: *${host}* is critically loaded${why}. New automatic sessions wait until it recovers; one you start asks first.`;
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
      // An override (Settings → Host load) is someone acting on it, not a recovery: no message.
      if (!st || st.override) continue;
      const prev = told[host] ?? {};
      const wasCritical = prev.critical ?? prev.state === 'critical'; // older files kept `state`
      const critical = st.state === 'critical';
      if (critical === wasCritical) continue;
      try {
        await notify(text(host, st, { since: prev.since }));
        log(`[load] told the owner: ${host} ${critical ? 'critical' : `no longer critical (${st.state})`}`);
        told[host] = { critical, since: critical ? st.since ?? now() : null };
        changed = true;
      } catch (err) {
        log(`[load] could not DM the owner about ${host}: ${err.message}`);
      }
    }
    if (changed) await save();
  }

  return { tick };
}
