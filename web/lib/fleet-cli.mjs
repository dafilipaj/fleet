// The one place the web server talks to the `fleet` CLI. Everything the server needs
// from the CLI goes through here, so an alternative UI/TUI server can reuse it and the
// contracts (`fleet list --json`, `fleet rename --json`, `fleet name --all --apply`, `fleet name <id> --apply`, `fleet group` (+ `--rename` / `--move`),
// `fleet usage --json`, `fleet --local prs --json`, `fleet --local pr checkout --json`, `fleet config set`, `fleet --local repos …`, `fleet --local jobs …`, `fleet --local stack … --json`, `fleet --local restore … --json`) are documented in one
// spot (see ARCHITECTURE.md).
//
// Everything that types into a session goes through the CLI too (`fleet send --json`, `fleet keys
// --json`, `fleet answer --question … --json`, `fleet rename --json`): each holds the session's
// typing lock (core::lock), the one every fleet command takes, so the web server's keys never
// interleave with an agent's `fleet answer`. Only reading the screen (peek) is done directly, by
// lib/backends.mjs: `fleet peek` truncates to terminal width and adds a header.

export class FleetCliError extends Error {
  constructor(message, { timedOut = false } = {}) {
    super(message);
    this.name = 'FleetCliError';
    this.timedOut = timedOut;
  }
}

/**
 * The exact CLI target of a `fleet list` row: `id:<session_id>`, or `pid:<n>` for a row with no
 * real session id. Never a fragment or a name — these commands type into a live agent.
 */
export function targetOf(session) {
  const id = session?.session_id;
  return id && !String(id).startsWith('pid-') ? `id:${id}` : `pid:${session?.pid}`;
}

/** clap's answer when the binary predates `fleet stack` (an older CLI on that host). */
const NO_SUBCOMMAND_RE = /unrecognized subcommand|unexpected argument 'stack'|invalid subcommand/i;

/**
 * `run(file, args, opts)` is injected (lib/run.mjs in production, a fake in tests).
 * `bin` is the resolved `fleet` binary (lib/config.mjs#resolveFleetBin).
 */
export function createFleetCli({ run, bin = 'fleet', timeoutMs = 8000 } = {}) {
  if (typeof run !== 'function') throw new TypeError('run must be a function');

  /** Local Claude sessions: the parsed `fleet list --json` array. Throws FleetCliError. */
  async function list() {
    let stdout;
    try {
      ({ stdout } = await run(bin, ['list', '--json'], { timeout: timeoutMs, maxBuffer: 16 * 1024 * 1024 }));
    } catch (err) {
      const killed = err?.killed || err?.signal === 'SIGTERM';
      if (killed) throw new FleetCliError(`fleet list timed out after ${Math.round(timeoutMs / 1000)}s`, { timedOut: true });
      if (err?.code === 'ENOENT') throw new FleetCliError(`fleet binary not found (${bin}) — install it or set "fleetBin" in the fleet config`);
      throw new FleetCliError(String(err?.message ?? err));
    }
    const text = (stdout ?? '').trim();
    if (!text) return [];
    let parsed;
    try {
      parsed = JSON.parse(text);
    } catch {
      throw new FleetCliError('fleet returned non-JSON output');
    }
    if (!Array.isArray(parsed)) throw new FleetCliError('fleet did not return an array');
    return parsed.filter((s) => s && typeof s === 'object');
  }

  /**
   * The naming pass: `fleet name --all --apply` (or `-n name --all` for a dry run).
   * Generates task-shaped names for every session still carrying Claude's cwd+hash name
   * and types `/rename` into them (the CLI holds ones waiting on a prompt). The CLI
   * brings each renamed session's tmux name along (a slug of the title) — one rename
   * path for everything, see docs/architecture.md → "Session titles".
   * Resolves { stdout, stderr } (human output, NO_COLOR); throws FleetCliError.
   */
  async function nameAll({ dryRun = false, timeoutMs: t = 300 * 1000 } = {}) {
    const args = dryRun ? ['-n', 'name', '--all'] : ['name', '--all', '--apply'];
    try {
      const { stdout, stderr } = await run(bin, args, { timeout: t, env: { ...process.env, NO_COLOR: '1' } });
      return { stdout: stdout ?? '', stderr: stderr ?? '' };
    } catch (err) {
      const killed = err?.killed || err?.signal === 'SIGTERM';
      if (killed) throw new FleetCliError(`fleet name timed out after ${Math.round(t / 1000)}s`, { timedOut: true });
      if (err?.code === 'ENOENT') throw new FleetCliError(`fleet binary not found (${bin})`);
      throw new FleetCliError(String(err?.message ?? err));
    }
  }

  /**
   * Name one session: `fleet name <target> --apply` — the same generator and rename path as
   * `nameAll`, for a session just spawned (lib/autoname.mjs#createSpawnNamer). Output as
   * `nameAll`. Throws FleetCliError.
   */
  async function nameOne({ target, timeoutMs: t = 120 * 1000 } = {}) {
    try {
      const { stdout, stderr } = await run(bin, ['name', String(target), '--apply'], { timeout: t, env: { ...process.env, NO_COLOR: '1' } });
      return { stdout: stdout ?? '', stderr: stderr ?? '' };
    } catch (err) {
      throw wrap(err, 'fleet name', t);
    }
  }

  /**
   * Rename one session: `fleet rename <target> <title> --json` — Claude's `/rename` (the
   * title, the source of truth) plus the derived tmux name. Resolves the report
   * ({ ok, result: renamed|sent|held, held, tmux, message, … }); a held session (waiting
   * on a prompt, exit 3) resolves too, with `ok: false`. Throws FleetCliError otherwise,
   * carrying `kind` and `report` as {@link typing} does (`busy`: another fleet command types
   * into the session).
   */
  async function rename({ target, title, timeoutMs: t = 20 * 1000 } = {}) {
    const args = ['rename', String(target), String(title), '--json'];
    let stdout;
    try {
      ({ stdout } = await run(bin, args, { timeout: t, env: { ...process.env, NO_COLOR: '1' } }));
    } catch (err) {
      // Held exits 3 with the report on stdout; anything else is a real failure.
      const report = tryObject(err?.stdout);
      if (report && report.result === 'held') return report;
      throw withKind(wrap(err, 'fleet rename', t), report);
    }
    return parseObject(stdout, 'fleet rename');
  }

  /**
   * A command that types into a session, with `--json`: resolves its report (`ok: true`);
   * throws FleetCliError carrying `kind` (the CLI's: `busy` while another fleet command types
   * into the session, …) and `report` (the failure object) when it fails.
   */
  async function typing(args, what, t) {
    let stdout;
    try {
      ({ stdout } = await run(bin, args, { timeout: t, env: { ...process.env, NO_COLOR: '1' } }));
    } catch (err) {
      throw withKind(wrap(err, what, t), tryObject(err?.stdout));
    }
    return parseObject(stdout, what);
  }

  /** `e` with the CLI's failure object (`{ ok: false, kind, error }`), when it printed one. */
  function withKind(e, report) {
    if (report && report.ok === false && typeof report.kind === 'string') {
      e.kind = report.kind;
      e.report = report;
      if (typeof report.error === 'string' && report.error) e.message = report.error;
    }
    return e;
  }

  /** Type `text` and submit it: `fleet send --json -- <target> <text>`. */
  async function send({ target, text, timeoutMs: t = 30 * 1000 } = {}) {
    return typing(['send', '--json', '--', String(target), String(text)], 'fleet send', t);
  }

  /** Press named keys: `fleet keys --json -- <target> <key>…`. */
  async function keys({ target, keys: names, timeoutMs: t = 20 * 1000 } = {}) {
    return typing(['keys', '--json', '--', String(target), ...names.map(String)], 'fleet keys', t);
  }

  /**
   * Answer a pending AskUserQuestion: `fleet answer <target> --question <id> --answers <json>
   * --json --allow-attached` — the CLI's checked path (the question pending and untouched on
   * screen, every key verified, the review checked). `--allow-attached`: the request is itself
   * a person's (or their operator's) answer, made from the app.
   */
  async function answer({ target, question, answers, timeoutMs: t = 60 * 1000 } = {}) {
    const args = ['answer', String(target), `--question=${question}`, `--answers=${JSON.stringify(answers)}`, '--json', '--allow-attached'];
    return typing(args, 'fleet answer', t);
  }

  function tryObject(text) {
    try {
      const v = JSON.parse(String(text ?? '').trim());
      return v && typeof v === 'object' && !Array.isArray(v) ? v : null;
    } catch {
      return null;
    }
  }

  function wrap(err, what, t) {
    const killed = err?.killed || err?.signal === 'SIGTERM';
    if (killed) return new FleetCliError(`${what} timed out after ${Math.round(t / 1000)}s`, { timedOut: true });
    if (err?.code === 'ENOENT') return new FleetCliError(`fleet binary not found (${bin})`);
    return new FleetCliError(String(err?.message ?? err));
  }

  function parseObject(stdout, what) {
    try {
      const v = JSON.parse(String(stdout ?? '').trim());
      if (v && typeof v === 'object' && !Array.isArray(v)) return v;
    } catch {
      /* below */
    }
    throw new FleetCliError(`${what} returned non-JSON output`);
  }

  /**
   * The grouping pass: `fleet group --input - --apply --json` with the merged fleet
   * (`/api/fleet` body) on stdin. Reads sessions and calls `claude -p`; never sends
   * anything to a session. Resolves the parsed report (docs/cli.md → `fleet group`).
   */
  async function groupRun({ fleet, refresh = false, consolidate = false, timeoutMs: t = 5 * 60 * 1000 } = {}) {
    const args = ['group', '--input', '-', '--apply', '--json'];
    if (refresh) args.push('--refresh');
    if (consolidate) args.push('--consolidate');
    let stdout;
    try {
      ({ stdout } = await run(bin, args, { timeout: t, input: JSON.stringify(fleet ?? { hosts: [] }), env: { ...process.env, NO_COLOR: '1' } }));
    } catch (err) {
      throw wrap(err, 'fleet group', t);
    }
    return parseObject(stdout, 'fleet group');
  }

  /** The stored groups (`fleet group --cached --json`): no discovery, no model call. */
  async function groupCached({ timeoutMs: t = 10 * 1000 } = {}) {
    let stdout;
    try {
      ({ stdout } = await run(bin, ['group', '--cached', '--json'], { timeout: t, env: { ...process.env, NO_COLOR: '1' } }));
    } catch (err) {
      throw wrap(err, 'fleet group --cached', t);
    }
    return parseObject(stdout, 'fleet group --cached');
  }

  /**
   * A change made on the board: `fleet group --rename <id> --label <l> --json`,
   * `fleet group --move <host/id> (--to <group id> | --label <new group>) --json`,
   * `fleet group --create --label <l> --json` or `fleet group --delete <id> --json`. Edits the
   * stored groups only (no discovery, no model call); resolves the report like `--cached`.
   */
  async function groupEdit({ op, id, label, host, session, to, timeoutMs: t = 10 * 1000 } = {}) {
    const args = ['group'];
    // `--flag=value`: a label starting with "-" must not read as a flag.
    if (op === 'rename') args.push(`--rename=${id}`, `--label=${label}`);
    else if (op === 'move') args.push(`--move=${host}/${session}`, to ? `--to=${to}` : `--label=${label}`);
    else if (op === 'create') args.push('--create', `--label=${label}`);
    else if (op === 'delete') args.push(`--delete=${id}`);
    else throw new FleetCliError(`unknown group edit "${op}"`);
    args.push('--json');
    let stdout;
    try {
      ({ stdout } = await run(bin, args, { timeout: t, env: { ...process.env, NO_COLOR: '1' } }));
    } catch (err) {
      const e = wrap(err, 'fleet group', t);
      const why = String(err?.stderr ?? '').replace(/^Error:\s*/, '').trim();
      if (why && !e.timedOut) e.message = why;
      e.refused = !e.timedOut && Boolean(why);
      throw e;
    }
    return parseObject(stdout, 'fleet group');
  }

  /**
   * This machine's Claude subscription limits: `fleet usage --json` (`--refresh` skips its
   * 60s cache). Resolves the CLI's object; throws FleetCliError (no login, endpoint down
   * with nothing cached) with the CLI's reason as the message.
   */
  async function usage({ refresh = false, timeoutMs: t = 20 * 1000 } = {}) {
    const args = ['--local', 'usage', '--json'];
    if (refresh) args.push('--refresh');
    let stdout;
    try {
      ({ stdout } = await run(bin, args, { timeout: t, env: { ...process.env, NO_COLOR: '1' } }));
    } catch (err) {
      const e = wrap(err, 'fleet usage', t);
      const why = String(err?.stderr ?? '').replace(/^(fleet:\s*)?(Error:\s*)?/i, '').trim();
      if (why && !e.timedOut) e.message = why;
      throw e;
    }
    return parseObject(stdout, 'fleet usage');
  }

  /**
   * The GitHub PRs that need you: `fleet --local prs --json [--refresh]` (docs/cli.md → Pull
   * requests). A fetch is four `gh` searches (several seconds each); a fresh cache is instant.
   */
  async function prs({ refresh = false, timeoutMs: t = 120 * 1000 } = {}) {
    const args = ['--local', 'prs', '--json'];
    if (refresh) args.push('--refresh');
    let stdout;
    try {
      ({ stdout } = await run(bin, args, { timeout: t, maxBuffer: 16 * 1024 * 1024, env: { ...process.env, NO_COLOR: '1' } }));
    } catch (err) {
      const e = wrap(err, 'fleet prs', t);
      const why = String(err?.stderr ?? '')
        .split('\n')
        .filter((l) => l.trim() && !l.startsWith('warning:'))
        .pop()
        ?.replace(/^(fleet:\s*)?(Error:\s*)?/i, '')
        .trim();
      if (why && !e.timedOut) e.message = why;
      e.missing = !e.timedOut && /unrecognized subcommand 'prs'/i.test(String(err?.stderr ?? ''));
      throw e;
    }
    return parseObject(stdout, 'fleet prs');
  }

  /**
   * Check a PR out in a worktree (or find the one it is in): `fleet --local pr checkout <ref>
   * --json` → `{ owner, repo, number, title, url, headBranch, baseBranch, crossRepository,
   * branch, path, repoPath, created, contextLine }`. Fetches the PR's head: can take a while.
   */
  async function prCheckout(ref, { timeoutMs: t = 4 * 60 * 1000 } = {}) {
    return prCommand('checkout', ref, t);
  }

  /** Mark a PR's @mentions of you seen: `fleet --local pr seen <ref> --json` → `{ pr, seenAt, applied }`. */
  async function prSeen(ref, { timeoutMs: t = 30 * 1000 } = {}) {
    return prCommand('seen', ref, t);
  }

  async function prCommand(sub, ref, t) {
    let stdout;
    try {
      ({ stdout } = await run(bin, ['--local', 'pr', sub, '--json', '--', String(ref)], { timeout: t, env: { ...process.env, NO_COLOR: '1' } }));
    } catch (err) {
      const e = wrap(err, `fleet pr ${sub}`, t);
      const why = String(err?.stderr ?? '')
        .split('\n')
        .filter((l) => l.trim() && !l.startsWith('warning:'))
        .pop()
        ?.replace(/^(fleet:\s*)?(Error:\s*)?/i, '')
        .trim();
      if (why && !e.timedOut) e.message = why;
      e.missing = !e.timedOut && new RegExp(`unrecognized subcommand '(pr|${sub})'`, 'i').test(String(err?.stderr ?? ''));
      throw e;
    }
    return parseObject(stdout, `fleet pr ${sub}`);
  }

  /**
   * The harnesses this machine can spawn (claude, codex, pi) and their model pickers:
   * `fleet --local harness list --json` → `[{ id, label, command, installed, path, version, models }]`.
   */
  async function harnesses({ timeoutMs: t = 30 * 1000 } = {}) {
    let stdout;
    try {
      ({ stdout } = await run(bin, ['--local', 'harness', 'list', '--json'], { timeout: t, env: { ...process.env, NO_COLOR: '1' } }));
    } catch (err) {
      throw wrap(err, 'fleet harness list', t);
    }
    const rows = JSON.parse(stdout);
    if (!Array.isArray(rows)) throw new Error('fleet harness list: expected an array');
    return rows;
  }

  /**
   * Write one config key: `fleet --local config set <key> <json>` against `configFile`
   * (FLEET_CONFIG). The CLI owns the config file: it rewrites the raw JSON atomically
   * (tmp + rename), keeps every other key and refuses a file that does not parse.
   * Resolves { stdout, stderr }; throws FleetCliError.
   */
  async function configSet({ key, value, configFile = null, timeoutMs: t = 10 * 1000 } = {}) {
    const env = { ...process.env, NO_COLOR: '1' };
    if (configFile) env.FLEET_CONFIG = configFile;
    try {
      const { stdout, stderr } = await run(bin, ['--local', 'config', 'set', String(key), JSON.stringify(value)], { timeout: t, env });
      return { stdout: stdout ?? '', stderr: stderr ?? '' };
    } catch (err) {
      throw wrap(err, 'fleet config set', t);
    }
  }

  /**
   * `fleet --local stack <args…> --json` (session stacks, docs/architecture.md → Session stacks).
   * Resolves the parsed JSON object. Throws FleetCliError carrying `exitCode`, `report` (a JSON
   * object the CLI printed on stdout before failing — `set` does on a conflict, exit 3),
   * `missing` (the binary has no `stack` command), `timedOut`; its message is the CLI's stderr.
   */
  async function stack(args, { input = null, timeoutMs: t = 20 * 1000 } = {}) {
    const argv = ['--local', 'stack', ...args.map(String), '--json'];
    let stdout;
    try {
      ({ stdout } = await run(bin, argv, { timeout: t, env: { ...process.env, NO_COLOR: '1' }, ...(input != null ? { input } : {}) }));
    } catch (err) {
      const e = wrap(err, `fleet stack ${args[0] ?? ''}`.trim(), t);
      const why = String(err?.stderr ?? '').replace(/^Error:\s*/, '').trim();
      if (why && !e.timedOut) e.message = why.split('\n')[0];
      e.exitCode = typeof err?.code === 'number' ? err.code : null;
      e.report = tryObject(err?.stdout);
      e.missing = !e.timedOut && NO_SUBCOMMAND_RE.test(why);
      throw e;
    }
    return parseObject(stdout, `fleet stack ${args[0] ?? ''}`.trim());
  }

  /**
   * `fleet --local restore <args…> --json` (session recovery, docs/cli.md → Session recovery):
   * the dormant list (no args), a restore (`--` target / `--all`, `--dry-run`) or a forget
   * (`--forget=<t>` / `--forget-all`), `--closed` for the recently closed list. Resolves the parsed object. Throws FleetCliError carrying
   * `exitCode` (2 ambiguous, 3 no match, 1 a restore with failures), `report` (the JSON the CLI
   * printed before exiting non-zero), `missing` (the binary predates `restore`), `timedOut`.
   */
  async function restore(args = [], { timeoutMs: t = 60 * 1000 } = {}) {
    const argv = ['--local', 'restore', '--json', ...args.map(String)];
    let stdout;
    try {
      ({ stdout } = await run(bin, argv, { timeout: t, env: { ...process.env, NO_COLOR: '1' } }));
    } catch (err) {
      const e = wrap(err, 'fleet restore', t);
      const why = String(err?.stderr ?? '').replace(/^Error:\s*/, '').trim();
      if (why && !e.timedOut) e.message = why.split('\n')[0];
      e.exitCode = typeof err?.code === 'number' ? err.code : null;
      e.report = tryObject(err?.stdout);
      // `--closed` is newer than `restore`: a CLI without it can't serve the closed list.
      e.missing = !e.timedOut && /unrecognized subcommand|unexpected argument '(?:restore|--closed)'|invalid subcommand/i.test(why);
      throw e;
    }
    return parseObject(stdout, 'fleet restore');
  }

  /** `fleet --local repos …`: errors carry the CLI's reason, `missing` (no `repos` command), `timedOut`. */
  async function reposRun(args, t) {
    let stdout;
    try {
      ({ stdout } = await run(bin, ['--local', 'repos', ...args], { timeout: t, maxBuffer: 16 * 1024 * 1024, env: { ...process.env, NO_COLOR: '1' } }));
    } catch (err) {
      const e = wrap(err, `fleet repos ${args[0] ?? ''}`.trim(), t);
      const why = String(err?.stderr ?? '').replace(/^Error:\s*/, '').trim();
      if (why && !e.timedOut) e.message = why.split('\n').filter((l) => !l.startsWith('warning:')).pop() || why;
      e.missing = !e.timedOut && /unrecognized subcommand 'repos'|unexpected argument '--all'/i.test(why);
      throw e;
    }
    return stdout ?? '';
  }

  /** Every repo under this host's roots: `fleet --local repos --json [--all]` (docs/cli.md → Repos). */
  async function repos({ all = false, timeoutMs: t = 30 * 1000 } = {}) {
    const out = await reposRun(['--json', ...(all ? ['--all'] : [])], t);
    try {
      const v = JSON.parse(out.trim() || '[]');
      if (Array.isArray(v)) return v;
    } catch {
      /* below */
    }
    throw new FleetCliError('fleet repos returned non-JSON output');
  }

  /** Fetch + fast-forward: `fleet --local repos sync --json [-- names…]` → the results array. */
  async function reposSync(names = [], { timeoutMs: t = 15 * 60 * 1000 } = {}) {
    const out = await reposRun(['sync', '--json', ...(names.length ? ['--', ...names.map(String)] : [])], t);
    try {
      const v = JSON.parse(out.trim() || '[]');
      if (Array.isArray(v)) return v;
    } catch {
      /* below */
    }
    throw new FleetCliError('fleet repos sync returned non-JSON output');
  }

  /** The sync timer: `fleet --local repos install-service [--uninstall]`. */
  async function reposService({ uninstall = false, timeoutMs: t = 20 * 1000 } = {}) {
    return reposRun(['install-service', ...(uninstall ? ['--uninstall'] : [])], t);
  }

  /**
   * `fleet --local jobs …` (docs/cli.md → Jobs). Errors carry the CLI's reason, `missing` (no
   * `jobs` command: an older CLI), `notFound` (no such job), `busy` (`run` refused: a session of
   * the job is still working), `timedOut`.
   */
  async function jobsRun(args, t) {
    let stdout;
    try {
      ({ stdout } = await run(bin, ['--local', 'jobs', ...args], { timeout: t, maxBuffer: 16 * 1024 * 1024, env: { ...process.env, NO_COLOR: '1' } }));
    } catch (err) {
      const e = wrap(err, `fleet jobs ${args[0] ?? ''}`.trim(), t);
      const why = String(err?.stderr ?? '').replace(/^Error:\s*/, '').trim();
      if (why && !e.timedOut) e.message = why.split('\n').filter((l) => !l.startsWith('warning:')).pop()?.replace(/^Error:\s*/, '') || why;
      e.missing = !e.timedOut && /unrecognized subcommand 'jobs'|invalid subcommand 'jobs'/i.test(why);
      e.notFound = !e.timedOut && /no job `/.test(why);
      e.busy = !e.timedOut && /pass --force/.test(why);
      throw e;
    }
    return stdout ?? '';
  }

  function parseJson(out, what, check) {
    try {
      const v = JSON.parse(String(out).trim());
      if (check(v)) return v;
    } catch {
      /* below */
    }
    throw new FleetCliError(`${what} returned non-JSON output`);
  }

  /** Every job with its latest run and live sessions: `fleet --local jobs --json`. */
  async function jobs({ timeoutMs: t = 30 * 1000 } = {}) {
    const out = await jobsRun(['--json'], t);
    return parseJson(out.trim() || '[]', 'fleet jobs', Array.isArray);
  }

  /** One job with all its kept runs (newest first): `fleet --local jobs show <name> --json`. */
  async function jobShow(name, { timeoutMs: t = 30 * 1000 } = {}) {
    const out = await jobsRun(['show', '--json', '--', String(name)], t);
    return parseJson(out, 'fleet jobs show', (v) => v && typeof v === 'object' && !Array.isArray(v));
  }

  /** Start a run now: `fleet --local jobs run [--force] <name>` → its one-line human report. */
  async function jobRunNow(name, { force = false, timeoutMs: t = 60 * 1000 } = {}) {
    const out = await jobsRun(['run', ...(force ? ['--force'] : []), '--', String(name)], t);
    return out.trim();
  }

  /** Pause / resume: `fleet --local jobs enable|disable <name>` → its one-line report. */
  async function jobSetEnabled(name, enabled, { timeoutMs: t = 15 * 1000 } = {}) {
    const out = await jobsRun([enabled ? 'enable' : 'disable', '--', String(name)], t);
    return out.trim();
  }

  // `--flag=value` everywhere: a value starting with "-" must not read as a flag.
  const stackList = () => stack(['list']);
  const stackShow = (id) => stack(['show', id]);
  /** Human edit: the markdown on stdin; exit 3 (conflict) when `expectUpdated` is stale. */
  const stackSet = (id, markdown, expectUpdated = null) =>
    stack(['set', id, ...(expectUpdated ? [`--expect-updated=${expectUpdated}`] : [])], { input: markdown });
  const stackRemove = (id) => stack(['rm', id, '-f']);
  /** Rename: the label is a positional, so `--` keeps one starting with "-" out of clap's flags. */
  const stackRename = (id, label) => stack(['rename', id, '--', label]);
  /** The session's stack, created around it when it has none (may call the model: 150 s). */
  const stackEnsure = (sessionId, { label = null } = {}) =>
    stack(['ensure', sessionId, ...(label ? [`--label=${label}`] : [])], { timeoutMs: 150 * 1000 });
  /**
   * Add a session: this host's, or — `host` — one running on another host, its row carried in
   * `input` (an /api/fleet-shaped body on stdin, so the CLI never needs ssh for it).
   */
  const stackAdd = (stackId, sessionId, { host = null, input = null } = {}) =>
    stack(['add', stackId, sessionId, ...(host ? ['--on', host] : []), ...(input ? ['--input', '-'] : [])], {
      ...(input ? { input: JSON.stringify(input) } : {}),
    });
  /** Membership sync; with `input` (the merged /api/fleet body) members on other hosts are judged too. */
  const stackSync = ({ input = null } = {}) =>
    stack(['sync', ...(input ? ['--input', '-'] : [])], { ...(input ? { input: JSON.stringify(input) } : {}) });

  return {
    bin,
    list,
    nameAll,
    nameOne,
    rename,
    send,
    keys,
    answer,
    groupRun,
    groupCached,
    groupEdit,
    usage,
    harnesses,
    prs,
    prCheckout,
    prSeen,
    configSet,
    repos,
    reposSync,
    reposService,
    jobs,
    jobShow,
    jobRunNow,
    jobSetEnabled,
    stackList,
    stackShow,
    stackSet,
    stackRemove,
    stackRename,
    stackEnsure,
    stackAdd,
    stackSync,
    restore,
  };
}
