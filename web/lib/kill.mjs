// Close a session for good: terminate the Claude process, then tear down the terminal
// that hosted it (the tmux window, and the tmux session when that was its only window,
// or the iTerm tab).
//
// This lives in the web backend layer (next to lib/backends.mjs), not in the `fleet` CLI:
// it needs nothing discovery doesn't already hand us (pid, backend, handle, tmux_session),
// and the tmux half is the same few tmux calls the backends make directly.

function isAlive(pid, killProcess) {
  try {
    killProcess(pid, 0);
    return true;
  } catch (err) {
    return err?.code === 'EPERM';
  }
}

/**
 * @param deps
 *   run          execFile wrapper (lib/run.mjs)
 *   tmux         tmux binary
 *   killProcess  process.kill (injected for tests)
 *   sleep        (ms) => Promise
 *   closeIterm   (session) => Promise — backends.closeIterm
 *   graceMs      SIGTERM → SIGKILL grace period
 */
export function createKiller({ run, tmux = 'tmux', killProcess = process.kill.bind(process), sleep, closeIterm, graceMs = 4000 }) {
  if (typeof run !== 'function') throw new TypeError('run must be a function');
  const wait = sleep ?? ((ms) => new Promise((r) => setTimeout(r, ms)));

  /** SIGTERM, wait up to graceMs, then SIGKILL. Returns how it died ('gone' if it never was alive). */
  async function terminate(pid) {
    if (!Number.isInteger(pid) || pid <= 1) return 'skipped';
    if (!isAlive(pid, killProcess)) return 'gone';
    try {
      killProcess(pid, 'SIGTERM');
    } catch (err) {
      if (err?.code === 'ESRCH') return 'gone';
      throw err;
    }
    const deadline = Date.now() + graceMs;
    while (Date.now() < deadline) {
      await wait(200);
      if (!isAlive(pid, killProcess)) return 'terminated';
    }
    if (!isAlive(pid, killProcess)) return 'terminated';
    try {
      killProcess(pid, 'SIGKILL');
    } catch (err) {
      if (err?.code === 'ESRCH') return 'terminated';
      throw err;
    }
    await wait(200);
    return 'killed';
  }

  /**
   * Where the pane lives, resolved BEFORE the process dies: when Claude is the pane's own
   * command (not typed into a shell) its pane, window and maybe session vanish with it.
   * Stable ids (`$3`, `@7`) are used as targets afterwards, never names: `-t a:b` on a
   * session called `a:b` resolves as session `a`, window `b`, and a bare name prefix-matches.
   * Returns null when the pane is already gone.
   */
  async function tmuxShape(pane) {
    try {
      const { stdout } = await run(tmux, ['display-message', '-p', '-t', pane, '#{session_id} #{window_id} #{session_windows}'], {
        timeout: 5000,
      });
      const m = /^(\$\d+) (@\d+) (\d+)$/.exec(stdout.trim());
      return m ? { sessionId: m[1], windowId: m[2], windows: Number(m[3]) } : null;
    } catch {
      return null;
    }
  }

  const isGone = (err) => /can't find|no such|not found|no server running|no current/i.test(String(err?.message ?? err));

  async function tmuxKill(args) {
    try {
      await run(tmux, args, { timeout: 5000 });
    } catch (err) {
      if (!isGone(err)) throw err; // it closed itself when Claude exited: that is the goal
    }
  }

  /**
   * @param session a `fleet list --json` row { pid, backend, handle, tmux_session }
   * @returns {{ process, terminal }} what happened to each half
   *   process:  'terminated' | 'killed' | 'gone' | 'skipped'
   *   terminal: 'tmux-session-killed' | 'tmux-window-killed' | 'tmux-pane-gone'
   *           | 'iterm-tab-closed' | 'iterm-tab-left' | 'left'
   */
  async function kill(session) {
    const backend = String(session.backend || 'unknown');
    const shape = backend === 'tmux' && session.handle ? await tmuxShape(session.handle) : null;
    const processResult = await terminate(Number(session.pid));
    let terminal = 'left';

    if (backend === 'tmux' && session.handle) {
      if (!shape) {
        terminal = 'tmux-pane-gone';
      } else if (shape.windows <= 1) {
        await tmuxKill(['kill-session', '-t', shape.sessionId]);
        terminal = 'tmux-session-killed';
      } else {
        // Other windows live in this tmux session: only take down the one that hosted Claude.
        await tmuxKill(['kill-window', '-t', shape.windowId]);
        terminal = 'tmux-window-killed';
      }
    } else if (backend === 'iterm' && session.handle && closeIterm) {
      try {
        await closeIterm(session);
        terminal = 'iterm-tab-closed';
      } catch {
        terminal = 'iterm-tab-left'; // iTerm may refuse (prefs); the process is dead anyway
      }
    }
    return { process: processResult, terminal };
  }

  /** Close an iTerm tab whose process already ended (a sleep): what `kill` reports for it. */
  async function closeTab(session) {
    if (String(session.backend) !== 'iterm' || !session.handle || !closeIterm) return 'left';
    try {
      await closeIterm(session);
      return 'iterm-tab-closed';
    } catch {
      return 'iterm-tab-left';
    }
  }

  return { kill, terminate, closeTab };
}
