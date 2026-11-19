// `claude -p` failing because its login is gone ("this login reached its time limit…", "Not
// logged in"). Background jobs (briefs, inbox, naming) run under the web server's LaunchAgent,
// which reads the login Keychain; an ssh shell may read a different credential store, so a
// `claude` that works over ssh can still be logged out here. The message says where to fix it.
// Mirrored in crates/fleet/src/core/naming.rs (`login_hint`).

const LOGIN_RE = /failed to authenticate|not logged in|auth login|\/login\b|invalid api key|oauth token (?:has )?expired|login (?:has )?expired|reached its time limit/i;

export const LOGIN_HINT =
  'run `claude auth login` on this host in its GUI session (a Fleet tmux session or Screen Sharing, not ssh): background jobs use the login Keychain';

/** Does this `claude` output say it is logged out? */
export function isLoginError(text) {
  return LOGIN_RE.test(String(text ?? ''));
}

/**
 * The reason a failed `claude -p` run (an execFile error) gives: the last line of stderr, else
 * of stdout (`claude` prints auth failures there), with the login hint when it is logged out.
 */
export function claudeFailure(err) {
  const pick = (s) => String(s ?? '').trim().split('\n').pop().trim();
  const why = pick(err?.stderr) || pick(err?.stdout) || pick(err?.message ?? err);
  const text = `${err?.stderr ?? ''}\n${err?.stdout ?? ''}\n${err?.message ?? ''}`;
  return `\`claude -p\` failed: ${why}${isLoginError(text) ? ` — ${LOGIN_HINT}` : ''}`;
}
