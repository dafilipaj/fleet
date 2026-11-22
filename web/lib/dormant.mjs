// Dormant sessions, the web side (docs/architecture.md → Session recovery). After a reboot the
// CLI's snapshot holds what the old boot ran; `fleet --local restore … --json` lists, restores
// and forgets it (lib/fleet-cli.mjs#restore). The server never reads snapshot.json itself: it
// passes the CLI's JSON through and maps its exit codes to HTTP statuses. The same routes carry
// the recently closed sessions (ended within a boot): `closed` in the list, `closed: true` on a
// restore / forget (→ `--closed`). Those are never part of `all` nor of restore.onBoot.
import { promises as fs } from 'node:fs';
import path from 'node:path';

import { HttpError } from './http.mjs';
import { BUSY_MARK } from './load.mjs';

export const MAX_DORMANT_TARGET = 200;
/** One dormant tmux session or Claude session; `--all` restores every one of them. */
const RESTORE_ONE_MS = 60 * 1000;
const RESTORE_ALL_MS = 5 * 60 * 1000;

/** `"q" matches 2 dormant (or recently closed) sessions: a, b — be more specific` → ['a', 'b']. */
export function ambiguityCandidates(message) {
  const m = /matches \d+ (?:dormant|recently closed) sessions?: (.*?)(?: — be more specific)?$/.exec(String(message ?? ''));
  if (!m) return [];
  return m[1].split(', ').map((s) => s.trim()).filter(Boolean);
}

/** A FleetCliError from `fleet restore` → the HttpError the API answers with. */
export function dormantHttpError(err) {
  if (err instanceof HttpError) return err;
  const msg = String(err?.message ?? err);
  if (err?.timedOut) return new HttpError(`fleet restore timed out: ${msg}`, 504);
  if (err?.missing) return new HttpError("this host's fleet CLI has no `restore` command (or no `restore --closed`) — update fleet there", 501);
  if (err?.exitCode === 2) {
    // Ambiguous: a restore starts agents, so it never guesses — the candidates go back to the UI.
    const e = new HttpError(msg, 409);
    e.body = { error: msg, candidates: ambiguityCandidates(msg) };
    return e;
  }
  if (err?.exitCode === 3) return new HttpError(msg, 404);
  if (msg.includes(BUSY_MARK)) return Object.assign(new HttpError(msg, 409), { body: { error: msg, overloaded: true } });
  return new HttpError(`fleet restore failed: ${msg}`, 502);
}

/**
 * The body of POST …/dormant/restore and …/dormant/forget: `{ target }` or `{ all: true }`
 * (+ `dryRun` for a restore), + `closed: true` for the recently closed list (no `all` restore
 * there — only a forget of all). Throws HttpError(400).
 */
export function validateDormantRequest(body, { allowDryRun = false, allowAll = true } = {}) {
  const b = body && typeof body === 'object' && !Array.isArray(body) ? body : {};
  const dryRun = allowDryRun && b.dryRun === true;
  if (b.closed != null && typeof b.closed !== 'boolean') throw new HttpError('closed must be true or false', 400);
  const closed = b.closed === true;
  // A restore on a host under heavy load asks first (lib/load.mjs); the confirm sends this.
  const ignoreLoad = b.ignoreLoad === true ? { ignoreLoad: true } : {};
  if (b.all === true) {
    if (b.target != null) throw new HttpError('give either target or all, not both', 400);
    if (closed && !allowAll) throw new HttpError('recently closed sessions are resumed one at a time (no all)', 400);
    return { all: true, target: null, dryRun, closed, ...ignoreLoad };
  }
  if (typeof b.target !== 'string') throw new HttpError('target must be a string (or all: true)', 400);
  const target = b.target.trim();
  if (!target || target.length > MAX_DORMANT_TARGET || /[\0\r\n]/.test(target)) {
    throw new HttpError(`target must be one non-empty line of at most ${MAX_DORMANT_TARGET} characters`, 400);
  }
  return { all: false, target, dryRun, closed, ...ignoreLoad };
}

/**
 * @param deps
 *   cli   lib/fleet-cli.mjs instance (`restore`)
 *   log   (line) => void
 */
export function createDormant({ cli, log = () => {} }) {
  if (typeof cli?.restore !== 'function') throw new TypeError('cli.restore must be a function');

  async function call(args, opts) {
    try {
      return await cli.restore(args, opts);
    } catch (err) {
      // A restore with failures (exit 1) still printed its report: pass it on.
      if (err?.exitCode === 1 && err.report && Array.isArray(err.report.failed)) return err.report;
      throw dormantHttpError(err);
    }
  }

  /** `{ host, bootId, dormant: [DormantView], closed: [ClosedView] }` (`closed` from newer CLIs). */
  const list = () => call([]);

  /**
   * `{ host, restored: [Restored], failed: [{ target, error }] }`. `closed`: a recently closed
   * session (`--closed`; never with `all`).
   */
  async function restore({ target = null, all = false, dryRun = false, closed = false, ignoreLoad = false } = {}) {
    if (closed && all) throw new HttpError('recently closed sessions are resumed one at a time (no all)', 400);
    const args = [...(ignoreLoad ? ['--ignore-load'] : []), ...(dryRun ? ['--dry-run'] : []), ...(closed ? ['--closed'] : []), ...(all ? ['--all'] : ['--', target])];
    const report = await call(args, { timeoutMs: all ? RESTORE_ALL_MS : RESTORE_ONE_MS });
    if (!dryRun) {
      const ok = (report.restored ?? []).map((r) => r?.session ?? r?.from).filter(Boolean);
      const failed = (report.failed ?? []).map((f) => `${f?.target}: ${f?.error}`);
      log(`[dormant] restore ${closed ? '--closed ' : ''}${all ? '--all' : target}: ${ok.length ? `restored ${ok.join(', ')}` : 'nothing restored'}${failed.length ? `; failed ${failed.join('; ')}` : ''}`);
    }
    return report;
  }

  /** `{ host, forgotten: [name] }`. `closed`: from the recently closed list (`--closed`). */
  async function forget({ target = null, all = false, closed = false } = {}) {
    const report = await call([...(closed ? ['--closed'] : []), ...(all ? ['--forget-all'] : [`--forget=${target}`])]);
    log(`[dormant] forgot${closed ? ' (closed)' : ''} ${(report.forgotten ?? []).join(', ') || 'nothing'}`);
    return report;
  }

  /**
   * Config `restore.onBoot`: at server start, bring back whatever the last boot left dormant
   * (`fleet --local restore --all`) — once per boot: `markerFile` remembers the boot id it ran
   * for, so a server restart within the same boot leaves what you chose not to resume alone.
   * Never throws; resolves the report or null.
   */
  async function restoreOnBoot({ markerFile = null } = {}) {
    try {
      const view = await list();
      const bootId = typeof view?.bootId === 'string' ? view.bootId : null;
      if (!bootId) {
        log('[dormant] restore.onBoot: boot id unknown — skipped');
        return null;
      }
      if (markerFile) {
        const seen = await fs.readFile(markerFile, 'utf8').then((t) => t.trim(), () => null);
        if (seen === bootId) {
          log('[dormant] restore.onBoot: already ran for this boot');
          return null;
        }
        await fs.mkdir(path.dirname(markerFile), { recursive: true, mode: 0o700 });
        await fs.writeFile(markerFile, `${bootId}\n`, { mode: 0o600 });
      }
      const n = Array.isArray(view?.dormant) ? view.dormant.length : 0;
      if (!n) {
        log('[dormant] restore.onBoot: nothing dormant');
        return null;
      }
      log(`[dormant] restore.onBoot: restoring ${n} dormant session${n === 1 ? '' : 's'}`);
      return await restore({ all: true });
    } catch (err) {
      log(`[dormant] restore.onBoot failed: ${err?.message ?? err}`);
      return null;
    }
  }

  return { list, restore, forget, restoreOnBoot };
}
