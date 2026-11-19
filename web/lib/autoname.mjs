// Periodic titling. Sessions started without a name carry Claude's cwd+hash fallback
// (`project-9d`); the `fleet` CLI generates a task-shaped title for them into its name cache
// (`~/.claude/fleet-names.json`), which every view draws (`display_title`). The web server runs
// it on a schedule: it already runs on every host, inside tmux, which is also where `claude -p`
// (the generator) has a logged-in keychain on a headless machine; a plain ssh shell often does not.
//
// Generated titles are display-only: nothing here sends `/rename`, so the Claude session name
// stays a human's choice and a title can keep following the work. Once a session has a brief,
// the brief call keeps its title current (lib/briefs.mjs, lib/titles.mjs) and the CLI leaves it
// be. The one rename left is `--adopt-tmux`: a session alone in a tmux session somebody named by
// hand takes that name (docs/architecture.md → "Session titles").

const ANSI_RE = /\x1b\[[0-9;]*m/g;

/** Turn `fleet name` human output into a summary. Lines look like:
 *  `project-1a  →  fix-login-flow`          (renamed; dry run appends `   (llm)`)
 *  `   ⧉ fix-login → fix-login-flow`        (tmux synced, indented note)
 *  `⏸ project-4f is mid-turn — nothing sent (--force overrides)` (held)
 *  `✕ project-01: ...` (error)
 *  `⚠ naming model unavailable: ...` (the model failed; the names are the branch/title guess)
 *  `no sessions still carry a Claude-derived name — nothing to rename`
 */
export function parseNameOutput(text) {
  const renamed = [];
  const tmux = [];
  const held = [];
  const errors = [];
  const warnings = [];
  for (const raw of String(text).replace(ANSI_RE, '').split('\n')) {
    const line = raw.trim();
    if (!line) continue;
    if (line.startsWith('⏸')) held.push(line.slice(1).trim());
    else if (line.startsWith('✕')) errors.push(line.slice(1).trim());
    else if (line.startsWith('⚠')) warnings.push(line.slice(1).trim());
    else if (line.startsWith('⧉')) tmux.push(line.slice(1).trim());
    else if (line.includes('  →  ')) {
      const [from, to] = line.split('  →  ');
      renamed.push({ from: from.trim(), to: to.trim().split(/\s+/)[0] });
    }
  }
  return { renamed, tmux, held, errors, warnings };
}

/**
 * @param deps
 *   cli           lib/fleet-cli.mjs instance (`nameAll`)
 *   intervalMs, initialDelayMs, log, dryRun
 */
export function createAutoNamer({ cli, intervalMs = 5 * 60 * 1000, initialDelayMs = 60 * 1000, log = () => {}, dryRun = false }) {
  if (!cli || typeof cli.nameAll !== 'function') throw new TypeError('cli.nameAll must be a function');
  let timer = null;
  let inFlight = null;
  let lastRun = null;
  let stopped = false;

  async function runOnce(reason = 'manual') {
    if (inFlight) return inFlight;
    inFlight = (async () => {
      const startedAt = Date.now();
      try {
        const { stdout, stderr } = await cli.nameAll({ dryRun });
        const summary = parseNameOutput(`${stdout}\n${stderr}`);
        lastRun = { at: startedAt, ms: Date.now() - startedAt, reason, ok: true, dryRun, ...summary };
        const brief = summary.renamed.map((r) => `${r.from}→${r.to}`).join(', ') || 'nothing to rename';
        const tmuxBrief = summary.tmux.length ? ` tmux: ${summary.tmux.join(', ')}` : '';
        const heldBrief = summary.held.length ? ` (${summary.held.length} held)` : '';
        const errBrief = summary.errors.length ? ` (${summary.errors.length} errors)` : '';
        log(`[autoname] ${reason}: ${brief}${tmuxBrief}${heldBrief}${errBrief}`);
        for (const w of summary.warnings) log(`[autoname] ${reason}: ${w}`);
      } catch (err) {
        lastRun = {
          at: startedAt,
          ms: Date.now() - startedAt,
          reason,
          ok: false,
          dryRun,
          error: err?.message ?? String(err),
          renamed: [],
          tmux: [],
          held: [],
          errors: [],
        };
        log(`[autoname] ${reason} failed: ${lastRun.error}`);
      } finally {
        inFlight = null;
      }
      return lastRun;
    })();
    return inFlight;
  }

  function schedule(delay) {
    if (stopped) return;
    clearTimeout(timer);
    timer = setTimeout(async () => {
      await runOnce('scheduled');
      schedule(intervalMs);
    }, delay);
    timer.unref?.();
  }

  return {
    start() {
      stopped = false;
      schedule(initialDelayMs);
    },
    stop() {
      stopped = true;
      clearTimeout(timer);
    },
    runOnce,
    get lastRun() {
      return lastRun;
    },
  };
}

/**
 * The row of a session the web just spawned into tmux session `tmuxSession`, once it has
 * registered (has a session id) — shared by the spawn namer and the stack joiner (lib/stacks.mjs).
 */
export function findSpawned(rows, tmuxSession) {
  const s = Array.isArray(rows) ? rows.find((r) => r?.tmux_session === tmuxSession) : null;
  return s?.session_id ? s : null;
}

/** Waits between tries of a targeted pass: ~2 min in all, tighter at first. */
export const SPAWN_NAME_DELAYS_MS = [8000, 10000, 12000, 15000, 20000, 25000, 30000];

/**
 * A targeted titling pass for a session the web just spawned with a first prompt, so it gets
 * a task-shaped (provisional, display-only) title within a minute or two instead of at the next
 * periodic run; its brief refines it later. For each
 * try (after `delaysMs[i]`): find the session by its tmux name in the local fleet; skip the
 * try while it has not registered or is still busy on its first turn; stop once it no longer
 * carries a Claude-derived name; else `fleet name <session_id>` (cache only) — done when renamed,
 * retried when held (waiting on a prompt) or failed.
 * @param deps
 *   cli          lib/fleet-cli.mjs instance (`nameOne`)
 *   listSessions () => Promise<session[]> — this host's `fleet list --json` rows (fresh)
 *   delaysMs, sleep, log
 */
export function createSpawnNamer({ cli, listSessions, delaysMs = SPAWN_NAME_DELAYS_MS, sleep, log = () => {} }) {
  if (!cli || typeof cli.nameOne !== 'function') throw new TypeError('cli.nameOne must be a function');
  if (typeof listSessions !== 'function') throw new TypeError('listSessions must be a function');
  const wait = sleep ?? ((ms) => new Promise((r) => setTimeout(r, ms).unref?.()));
  const pending = new Map(); // tmux session name -> promise
  let stopped = false;

  /** Resolves { ok, reason, tries, renamed? } once named or out of tries (never rejects). */
  async function run(tmuxSession) {
    let tries = 0;
    for (const delay of delaysMs) {
      await wait(delay);
      if (stopped) return { ok: false, reason: 'stopped', tries };
      tries += 1;
      let rows = [];
      try {
        rows = await listSessions();
      } catch {
        continue;
      }
      const s = findSpawned(rows, tmuxSession);
      if (!s) continue; // not registered yet
      if (s.name_source && s.name_source !== 'derived') return { ok: true, reason: 'already named', tries };
      if (s.status === 'busy') continue; // still on its first turn
      try {
        const { stdout, stderr } = await cli.nameOne({ target: s.session_id });
        const summary = parseNameOutput(`${stdout}\n${stderr}`);
        if (summary.renamed.length) {
          const to = summary.renamed[0].to;
          log(`[autoname] spawned ${tmuxSession}: → ${to}${summary.tmux.length ? ` tmux: ${summary.tmux.join(', ')}` : ''}`);
          return { ok: true, reason: 'renamed', tries, renamed: to };
        }
      } catch (err) {
        log(`[autoname] spawned ${tmuxSession}: try ${tries} failed: ${err?.message ?? err}`);
      }
    }
    log(`[autoname] spawned ${tmuxSession}: not named after ${tries} tries — the periodic pass will`);
    return { ok: false, reason: 'gave up', tries };
  }

  return {
    /** Start the pass for `tmuxSession` (once per name at a time); returns its promise. */
    schedule(tmuxSession) {
      if (pending.has(tmuxSession)) return pending.get(tmuxSession);
      stopped = false;
      const p = run(tmuxSession).finally(() => pending.delete(tmuxSession));
      pending.set(tmuxSession, p);
      return p;
    },
    stop() {
      stopped = true;
    },
    get pending() {
      return [...pending.keys()];
    },
  };
}
