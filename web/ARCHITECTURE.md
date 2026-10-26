# fleet-web — architecture

A zero-dependency Node server that exposes the fleet over a small JSON HTTP API, plus a
mobile-first PWA built on that API. Every host runs the same server; any one of
them shows the whole fleet by merging in its peers. No auth — meant for a private network
(e.g. a Tailscale tailnet). The server has no build step and no npm dependencies.

The API is the stable part. The UI is `ui/` (React + shadcn/ui, built to `ui/dist/` — see
[ui/README.md](./ui/README.md)). Another UI can be pointed at the same API (or served by it via
`web.ui`, see Config).

## Topology

```
phone ──http──▶ workstation:7777 (self=workstation) ──http──▶ laptop:7777 (?local=1)
           or ▶ laptop:7777      (self=laptop)      ──http──▶ workstation:7777 (?local=1)
```

- Each instance serves its **own** sessions under its `self` name and knows its **peers**
  (other hosts in the config that have a `web` URL).
- A request for host X is served locally when X == self, proxied when X is a peer, otherwise 404.
- Proxied requests carry `?local=1` and are never proxied again (no chains, no loops).

## Modules

```
server.mjs            wiring: config → deps → API → HTTP server → listen
lib/config.mjs        shared fleet config loader + binary resolution
lib/fleet-cli.mjs     the ONLY place that invokes the `fleet` CLI (`list --json`, `name --all --apply`, `usage --json`, `config set`, `--local stack … --json`, `--local restore … --json`, …)
lib/fleet.mjs         local discovery: cache (2s TTL), in-flight de-dup, never throws
lib/backends.mjs      peek/send/keys straight to tmux / iTerm2 (osascript)
lib/transcript.mjs    transcript JSONL → chat messages (Claude Code; Codex rollouts and pi sessions via the row's `transcript` path)
lib/spawn-dirs.mjs    Settings → Start directories: validate a `spawnDirs` list, write it via `fleet config set`, hot-reload
lib/repos.mjs         Settings → Git repos: `fleet repos` list/sync/timer, validate + write the `repos` config key
lib/jobs.mjs          Bots screen: `fleet jobs` (alias of `fleet bots`) list / show / run now / enable|disable, job-name validation
lib/spawn.mjs         new tmux session + `claude [--model <id>] [-n <name>] '<prompt>'`, auto-accept folder trust
lib/kill.mjs          close a session: SIGTERM/SIGKILL Claude, then its tmux session/window or iTerm tab
lib/autoname.mjs      periodic `fleet name --all --apply` + generic-tmux-name sync
lib/grouping.mjs      periodic `fleet group` over the merged fleet (the grouping host only)
lib/snapshot.mjs      warm stale-while-revalidate snapshot of the merged /api/fleet
lib/uploads.mjs       dropped/pasted files → <uploads dir>/YYYY-MM-DD/<rand>-<name> (streamed, size-capped, daily cleanup)
lib/files.mjs         files mentioned in chat: resolve against the session cwd (+ touched/roots fallback), $HOME/cwd sandbox, stat/kind, raw stream, open
lib/diff.mjs          the diff reviewer: a session's worktree (incl. untracked) as a git tree via a throwaway index, diffed against the merge-base with the default branch → files/hunks/lines
lib/reviews.mjs       the diff reviewer's comment threads: prompts with `[review c-…]` markers typed into the session (single, or drafts submitted as one review), answers read back from its transcript, one JSON file per session under <state>/fleet/reviews
lib/complete.mjs      composer autocomplete: skills/commands (built-in, user, project, enabled plugins), the cwd's .md/.plain files, brain notes (~/.brainrc)
lib/touched.mjs       absolute paths a session's tool calls touched, parsed incrementally from its transcript (for lib/files.mjs)
lib/brief-format.mjs  session brief file format: parse/serialise, resource + Git-line merge, model-output check, continue prompt (pure)
lib/brief-extract.mjs brief resources + todos from a transcript (incremental, no model); the conversation delta for the model
lib/briefs.mjs        brief store (atomic files), budgeted `claude -p` generation, background pass (see Session briefs)
lib/dormant.mjs       dormant + recently closed sessions (session recovery): `fleet --local restore [--closed] … --json` errors → HTTP, restore.onBoot
lib/stacks.mjs        session stacks: `fleet stack` errors → HTTP, sibling spawn dir rule, post-spawn `stack add`, background `stack sync`
lib/editor.mjs        "Open in editor" links (vscode:// / cursor://, local folder or Remote-SSH)
lib/notes.mjs         notes explorer: the `web.notes.root` sandbox, tree, frontmatter, built-in search / `searchCmd`
lib/peers.mjs         peer fetch + one-hop proxy (JSON bodies; uploads streamed up, files/raw streamed down, unbuffered)
lib/api.mjs           /api/* request handling (no UI knowledge)
lib/app.mjs           node:http server: /api/* → api, everything else → static UI dir
lib/http.mjs, util.mjs, run.mjs   helpers (body limit, static path safety, execFile wrapper)
ui/                   the React UI (Vite + TS + Tailwind v4 + shadcn/ui); only ui/dist is served/installed
```

`createApi(deps)` and `createHttpServer({ handleApi, uiDir })` take all I/O as injected
dependencies, so tests (and alternative servers) can mount the API with fakes.

## Config

The server reads the **shared fleet config** written by `fleet init`:
`$FLEET_CONFIG`, else `${XDG_CONFIG_HOME:-~/.config}/fleet/config.json`. See
[`config.example.json`](./config.example.json). Unknown keys are ignored.

| config | server meaning |
| --- | --- |
| `self` | this host's name (default `local`) |
| `hosts.<name>.web` | peer base URL for every host ≠ self; hosts without `web` are not peers |
| `web.port` | listen port (default 7777) |
| `web.bind` | listen address (default `0.0.0.0` with a config, `127.0.0.1` without one) |
| `web.editor` | `"vscode"` (default) \| `"cursor"` \| `null`: scheme of `editorUrl` on briefs and session rows; `null` = none. Other values → config error |
| `hosts.<name>.ssh` | used here only for `editorUrl`: the Remote-SSH alias of that host (must be letters, digits, `._@-`) |
| `web.ui` | static UI: a directory path; unset/`null` (or the retired `"classic"`) → `web/ui/dist`, a "not built" placeholder until it has an `index.html` |
| `web.quickReplies` | composer chips: `["text", { "label", "text" }]` (default Continue/Yes/No/1/2); `{ label, kind: "text", value }` is accepted too, `kind: "key"` entries are skipped (the key chips are built in) |
| `web.models` | New session model picker: `[{ id, label }]` or bare ids; id `""` = no `--model` (Claude's default); ids are letters, digits and `._[]-`. Default: Default, Fable 5.1 `claude-fable-5-1`, Opus 5.5 `claude-opus-5-5`, Sonnet 5 `claude-sonnet-5`, Haiku 4.5 `claude-haiku-4-5-20251001` |
| `web.autoName` | `{ enabled, intervalMinutes }`, default `{ false, 5 }` (opt-in): the periodic naming pass and the targeted pass after a spawn (see Auto-naming); `false` also makes a nameless spawn pass `-n fw-hhmmss` |
| `web.grouping` | `{ enabled, intervalMinutes }`, default `{ false, 10 }` (opt-in): run the grouping pass here for the whole fleet (see Smart grouping) |
| `web.uploads` | `{ dir, maxMB, retentionDays }`, default `{ "~/.local/share/fleet/uploads", 100, 14 }`: where files attached in the UI are stored on this host, the per-file limit, and how many days a day dir is kept (`0` = forever; cleanup runs at start and daily) |
| `web.briefs` | `{ enabled, model, idleMs, minIntervalMs, maxDeltaChars, maxCallsPerHour, minNewTurns, minNewChars, maxBriefChars }`, default `{ false, "haiku", 60000, 900000, 12000, 12, 2, 2000, 3000 }` (opt-in): background brief generation on this host (see Session briefs); GET/PUT and a manual regenerate work when off |
| `web.stacks` | `{ syncMinutes }`, default `{ 2 }` (> 0): how often the background `fleet stack sync` runs — only while the last `fleet list` shows a session in a stack; first run 30 s after start (see Session stacks) |
| `stacks` | the CLI's (`{ enabled, model }`, default `{ true, "sonnet" }`: whether/which model writes a new StackBrief). The server only reports them in `/api/settings`; a bad value falls back to the default, never a startup error |
| `web.notes` | `{ root, name?, searchCmd?, exclude? }`, default none (off): the notes explorer over the markdown notes under `root` (`~` expanded, absolute). `name` defaults to the root's basename; `searchCmd` is an argv array (or a space-separated string) run with cwd = root — `{query}` is the query as one argument, `{args}` one argument per word, neither → the query is appended; `exclude` = extra names / root-relative paths to hide (see Notes) |
| `web.files.roots` | array of dirs (`~` expanded, default `[]`): extra places a relative path in chat may live, tried after the session's touched files (see files). They add candidates only; the sandbox stays `$HOME` + cwd |
| `grouping.host` | the host whose server runs grouping; set, it is the only one (a `web.grouping.enabled` elsewhere is ignored) and every other server proxies `/api/groups` to it |
| `restore.onBoot` | `true`: at start, `fleet --local restore --all` once when this host has dormant sessions — once per boot (marker `${XDG_STATE_HOME:-~/.local/state}/fleet/web-restored-boot`), logged. Default `false`; a non-boolean is a config error. Recently closed sessions are never included (`restore.keepClosedDays` is the CLI's) |
| `tmux` | tmux binary; `null` → PATH, `/opt/homebrew/bin`, `/usr/local/bin` |
| `fleetBin` | `fleet` binary; `null` → PATH, fallbacks, `~/.local/bin`, `~/.cargo/bin` |
| `claude` | launcher typed by spawn (default `claude`) |
| `spawnDirs[]` | `{ label, paths: { <host>: dir }, hidden? }` → this host offers `{ label, path: paths[self] }`; `{ label, path }` means the same dir on every host; `~` expanded; none offered → `[{ label: "Home", path: $HOME }]`. `hidden: true` → not offered but allowed (`config.hiddenSpawnDirs`; the allow-list is `spawnRoots(config)` = offered + hidden) |

Env overrides: `FLEET_CONFIG`, `FLEET_WEB_PORT` (or `PORT`), `FLEET_WEB_BIND`, `FLEET_WEB_UI`,
`FLEET_WEB_AUTONAME` / `FLEET_WEB_GROUPING` / `FLEET_WEB_BRIEFS` / `FLEET_WEB_STACKS` (`0`/`false`/`off` disables, anything else enables;
`FLEET_WEB_STACKS` only turns the background stack sync off — the routes keep working),
`FLEET_BRIEFS_DIR` (where brief files live), `FLEET_BIN`, `FLEET_TMUX`. The CLI reads `FLEET_STACKS_DIR` (where StackBriefs live).

Missing config → runs as a single host `local` on 127.0.0.1 and logs a hint to run
`fleet init`. A config that exists but is not valid JSON / has a bad shape → exits with code
2 and a message naming the file and the key.

The server prepends `/opt/homebrew/bin:~/.local/bin:/usr/local/bin` to `PATH` for children,
because service managers (launchd) do not provide the user's PATH.

## The CLI contract

Local discovery is `fleet list --json` (timeout 8s), an array of:

```
pid, session_id (uuid), name, cwd, status ("busy"|"idle"|"waiting"|"unknown"), updated_at (ms),
tty, backend ("iterm"|"tmux"|"unknown"), handle (iTerm session id | tmux pane id like "%87"),
tab, tmux_session (string|null), name_source, waiting_for (string|null),
title (first prompt, may be long), gen_title (string|null),
display_title (string — THE title to draw, see docs/architecture.md → Session titles),
context ({ used, window, pct, model } | null — context-window usage, see docs/architecture.md)
```

Empty output → no sessions. Non-JSON, non-array, a timeout or a missing binary → the host
entry becomes `{ ok: false, error }`; the server never crashes on it. `title` is trimmed to 300
chars (+ `…`) for transport: the first prompt can run to 10 KB and the list shows one line.
Every other field, `context` included, is passed through untouched (older CLIs omit `context`;
UIs treat a missing one as unknown).

The naming pass is `fleet name --all --apply` (timeout 5 min, `NO_COLOR=1`); its human output is
parsed line by line: `<from>  →  <to>` renamed, `⏸ …` held (waiting on a prompt), `✕ …` error,
`⧉ …` tmux note (the CLI renames the tmux session with the title).

A rename is `fleet rename <session_id> <title> --json` (timeout 20s): the report
`{ ok, result: renamed|sent|held, held, tmux: { renamed, from, to, note }, message, … }` on
stdout; a held session exits 3 with the report still on stdout (`lib/fleet-cli.mjs#rename`
resolves it), anything else non-zero is an error.

The grouping pass is `fleet group --input - --apply --json` (timeout 5 min, the merged
`/api/fleet` body on stdin; `lib/run.mjs` takes `opts.input`); at start the server reads the stored
groups with `fleet group --cached --json`. Both answer the JSON report in docs/cli.md → Grouping.

Session stacks are `fleet --local stack <sub> … --json` (`NO_COLOR=1`, timeout 20 s; `ensure`
150 s, it may call the model): `list`, `show <id>`, `set <id> [--expect-updated=<iso>]` (markdown
on stdin), `rm <id> -f`, `ensure <session_id> [--label=<l>]`, `add <stack_id> <session_id>`,
`sync`. Every answer is passed through as-is (StackView, docs/architecture.md → Session stacks);
the server never parses a StackBrief. Failures map to HTTP by exit code and stderr
(`lib/stacks.mjs#stackHttpError`): "no stack …" / "not found" → 404; `set` exit 3 → 409 with the
JSON report the CLI printed on stdout (`{ error, updated }`); other exit 2/3 → 409; exit 1 → 400;
a timeout → 504; a CLI without the `stack` subcommand (clap "unrecognized subcommand") → 501;
anything else → 502.

Peek/send/keys do **not** go through `fleet peek/send` (those truncate to terminal width and
add a header). `lib/backends.mjs` drives the backends directly, mirroring the CLI:

- **tmux** (handle = pane id): peek `capture-pane -p -J -t <h> -S -<n>`; send
  `send-keys -t <h> -l -- <text>` in pieces of at most 512 bytes (a longer burst reaches
  Claude as a paste, or loses its first ~1 KB), then `send-keys -t <h> Enter` (multi-line text:
  `load-buffer` + `paste-buffer -p`); keys `send-keys -t <h> <Key>`.
- **iterm** (handle = iTerm session id): one AppleScript written to a temp file, values passed
  as argv (never interpolated). Send = `write text … without newline`, 0.2s, `write text ""`
  (bracketed paste would swallow a trailing newline).
- **unknown** → 409.

## HTTP API

JSON everywhere, same origin, no auth. Errors are `{ "error": "message" }`.
`:host` is a host name (self or a peer). `:id` is a `session_id` or a unique prefix of ≥ 8 chars.

| method | path | request | response |
| --- | --- | --- | --- |
| GET | `/api/health` | | `{ name, version, apiVersion, self, uptime, now, autoName: { enabled, intervalMinutes, lastRun }, grouping, briefs: { enabled, model, callsLastHour, maxCallsPerHour, generating, lastRun }, stacks: { sync, syncMinutes, lastSync: { at, ms, reason, ok, changed: [ids], error? } \| null } }` |
| GET | `/api/settings` | | `{ apiVersion, self, hosts: [names], quickReplies: [{ label, text }], uploads: { maxMB }, models: [{ id, label }], notes: { enabled, name? }, stacks: { enabled, model, generate }, focus: { host } }` — `stacks.model` / `generate` = the config's `stacks.model` (default `"sonnet"`) / `stacks.enabled` |
| GET | `/api/fleet` | `?local=1` = this host only | `{ self, hosts: [Host], snapshotAt }` — self first, then peers (`snapshotAt` only on the merged view) |
| GET | `/api/hosts/:host/sessions/:id/peek` | `?lines=200` (10..2000) | `{ host, id, backend, lines, text, capturedAt }` |
| GET | `/api/hosts/:host/sessions/:id/messages` | `?limit=60` (1..500) | `{ host, id, status, backend, name, limit, messages: [Message], total, truncated, updatedAt, capturedAt }` |
| POST | `/api/hosts/:host/sessions/:id/send` | `{ text }` (1..8000 chars, not blank) | `{ ok: true }` |
| POST | `/api/hosts/:host/sessions/:id/keys` | `{ key }`, one of `Enter`, `Escape`, `Up`, `Down` | `{ ok: true }` |
| POST | `/api/hosts/:host/sessions/:id/rename` | `{ title }` (trimmed, 1..64 chars, one line) | `{ ok: true, host, id, result, title, from, tmux, message, … }` — the `fleet rename --json` report. **409** `{ error, result: "held", held: "waiting", … }` when the session is waiting on a prompt (nothing typed); 400 bad title; 404 unknown session; 502/504 CLI failure / timeout. Proxied once to a peer like the other session actions |
| POST | `/api/hosts/:host/spawn` | `{ name?, dir?, prompt?, model?, harness?, ghIdentity?, embed? }` | `{ ok, host, name, dir, tmuxSession, command, trusted, model, harness }` — `harness`: `claude` (default) \| `codex` \| `pi` |
| GET | `/api/hosts/:host/harnesses` | `?refresh=1` skips the 10 min cache | `{ host, harnesses: [{ id, label, command, installed, models: [{ id, label }] }] }` — `fleet --local harness list --json`; Claude's models are `web.models`; a CLI without `harness` → Claude alone. Proxied once to a peer |
| POST | `/api/hosts/:host/sessions/:id/kill` | `{}` | `{ ok: true, host, id, name, process, terminal }` |
| GET | `/api/hosts/:host/usage` | `?refresh=1` skips the CLI's 60s cache | `{ host, account, limits: [{ kind, group, label, model, percent, severity, resets_at, active }], extra_usage, fetched_at, stale, error }` — `fleet usage --json` (docs/architecture.md → Subscription usage); 502 with the CLI's reason, 504 on timeout. Proxied once to a peer |
| POST | `/api/hosts/:host/prs/checkout` | `{ pr: "owner/repo#123" }` | `{ host, worktree: { owner, repo, number, branch, path, repoPath, created, contextLine, editorUrl, … } }` — `fleet --local pr checkout --json` + the worktree's editor link; 400 on a bad ref, 502 with the CLI's reason (no clone, fetch failed). Proxied once to a peer (5 min) |
| POST | `/api/hosts/:host/prs/seen` | `{ pr: "owner/repo#123" }` | `{ host, pr, seenAt }` — `fleet --local pr seen --json`: out of the Unseen mentions column; 400 on a bad ref, 501 when the CLI has no `pr seen`. Proxied once to a peer |
| POST | `/api/hosts/:host/prs/spawn` | `{ pr, prompt?, name?, model?, harness? }` (validated like `spawn` before anything is fetched) | `{ host, worktree, spawn: SpawnResponse }` — the checkout, then the spawner in `worktree.path` with `contextLine` + `prompt`. Proxied once to a peer |
| GET | `/api/hosts/:host/prs` | `?refresh=1` fetches from GitHub whatever the cache's age | `{ host, user, scope, fetchedAt, stale, error, truncated, columns: [{ id, title, count }], prs: [Pr…] }` — `fleet --local prs --json` (docs/cli.md → Pull requests), each checked-out PR's `local.editorUrl` added by the server the browser asked (like a session's `editorUrl`); 502 with the CLI's reason, 504 on timeout, 501 when the CLI has no `prs`. Proxied once to a peer (130 s) |
| GET | `/api/hosts/:host/tasks` | `?refresh=1` fetches from Productive whatever the cache's age | `{ host, user, organization, fetchedAt, stale, error, truncated, columns: [{ id, title, count }], tasks: [Task…], statuses: { workflowId: [Status…] } }` — `fleet --local tasks --json` (docs/cli.md → Tasks); 502 with the CLI's reason (no token, Productive down with nothing cached), 504 on timeout, 501 when the CLI has no `tasks`. Proxied once to a peer (130 s) |
| POST | `/api/hosts/:host/tasks/status` | `{ task: "1001" \| task link, status: id or name }` | `{ host, task }` — `fleet --local task status --json`: the task in its new status; 400 on a bad id. Proxied once to a peer |
| POST | `/api/hosts/:host/tasks/spawn` | `{ task, dir, prompt?, name?, model?, harness? }` (validated like `spawn`; `dir` within the spawn dirs) | `{ host, task, spawn: SpawnResponse }` — `fleet --local task show --json` for the task's `contextLine`, then the spawner in `dir` with `contextLine` + `prompt`. Proxied once to a peer |
| GET/POST | `/api/hosts/:host/focus` | POST `{ on: boolean, minutes?: 1–600 }` | `{ host, active, remainingMs, endsAt, apps, stale }` — lib/focus.mjs parses `focus status`; POST runs `focus start [minutes]` / `focus stop` first. 400 on a bad body, 501 without `focus`, 502 with its message. Proxied once to a peer (45 s) |
| GET | `/api/hosts/:host/spawn-dirs` | | `{ host, hosts: [names], spawnDirs: [{ label, paths: { host: dir }, hidden? }], checks: [{ path, resolved, exists, isDir } \| null], offered: [{ label, path }], hiddenSpawnDirs: [{ label, path }], limits: { maxEntries, maxLabel, maxPath } }` — see **spawn-dirs** |
| PUT | `/api/hosts/:host/spawn-dirs` | `{ spawnDirs: [{ label, paths, hidden? }], dryRun? }` | the GET shape for the new list + `saved` (`false` on a dry run). 400 `{ error, errors: [{ index, field, host?, error }], checks }`; 502 when `fleet config set` failed |
| GET | `/api/hosts/:host/repos` | | `{ host, settings: { roots, every, overrides, exclude }, repos: [fleet repos --json --all rows], error, service: { installed, supported, tickMinutes }, limits }` — see **repos**; 501 when that host's CLI has no `repos` |
| PUT | `/api/hosts/:host/repos` | `{ roots, every, overrides, exclude }` | the GET shape + `saved`. 400 `{ error, errors: [{ field, error }] }`; 502 when `fleet config set` failed |
| POST | `/api/hosts/:host/repos/sync` | `{ names? }` | `{ host, results }` — `fleet --local repos sync --json [-- names]`; 409 while a sync runs there; peers get 16 min |
| GET | `/api/hosts/:host/jobs` | | `{ host, jobs: [fleet jobs --json rows] }` — see **jobs**; 501 when that host's CLI has no `jobs` |
| GET | `/api/hosts/:host/jobs/:name` | | `fleet jobs show <name> --json` + `host` (every kept run, newest first); 404 no such job, 400 a name outside `[A-Za-z0-9_-]{1,40}` |
| POST | `/api/hosts/:host/jobs/:name/run` | `{ force? }` | `{ host, name, output }` — `fleet --local jobs run [--force] <name>` (starts an agent); 409 `{ error, busy: true }` while a job session works; peers get 90 s |
| POST | `/api/hosts/:host/jobs/:name/enabled` | `{ enabled: boolean }` | `{ host, name, enabled, output, job }` — `fleet --local jobs enable\|disable <name>`, then `show` |
| POST | `/api/hosts/:host/repos/service` | `{ install: boolean }` | the GET shape after `fleet --local repos install-service [--uninstall]`; 501 off macOS |
| GET | `/api/groups` | | `{ enabled, host, intervalMinutes, running, updatedAt, lastRun: { at, ms, ok, reason, mode, modelCalls, classified, note?, error? } \| null, groups: [{ id, label, description, source, members: [{ host, id, dormant? }] }], error? }` (`dormant: true` only when that member is dormant on its host) — `enabled: false` (and `groups: []`) when no host runs grouping or the grouping host is unreachable |
| POST | `/api/groups/edit` | `{ op: "rename", id, label }` \| `{ op: "move", host, session, to }` \| `{ op: "move", host, session, label }` \| `{ op: "create", label }` \| `{ op: "delete", id }` | the same shape after `fleet group --rename/--move/--create/--delete` (400 bad body, 409 refused by the CLI, 501 when grouping is off). Waits for a running pass; edits run one at a time |
| POST | `/api/groups/run` | `{}` | the same shape after the run (502 when it failed, 501 when grouping is off) |
| POST | `/api/hosts/:host/uploads` | `?name=<file name>`, the raw file as the body (any `content-type`) | `{ host, path, name, size }` — `path` is absolute on `:host`. 413 over `web.uploads.maxMB` (no partial file is left) |
| POST | `/api/hosts/:host/autoname` | `{}` | `{ host, ok, at, ms, reason, dryRun, renamed: [{ from, to }], tmux: ["a → b"], held: [..], errors: [..], error? }` (502 when the pass failed) |
| POST | `/api/hosts/:host/sessions/:id/files/stat` | `{ paths: [string] }` (≤ 200) | `{ host, id, cwd, home, files: [FileStat] }` — 400 not an array / too many |
| GET | `/api/hosts/:host/sessions/:id/diff` | `?base=<ref>&since=<fingerprint>` | `{ host, id, root, branch, head, base: { ref, sha }, tree, fingerprint, unchanged, files: [{ path, oldPath, status, binary, additions, deletions, truncated, hunks: [{ header, oldStart, oldLines, newStart, newLines, section, lines: [{ t, o, n, text, noEol? }] }] }], stats, truncated }` — the session's worktree against the merge-base of HEAD with `base` (default: origin/HEAD, else main/master); `since` = the last `fingerprint` → `{ …, unchanged: true }` without `files` while nothing moved (lib/diff.mjs). 400 bad ref, 409 not a git repo / no cwd, 501 no differ |
| GET | `/api/hosts/:host/sessions/:id/diffs` | `?since=<fingerprint>` | every checkout the session works in (cwd's + those of the files in lib/touched.mjs `worked`), each diffed like `…/diff` → `{ host, id, cwd, fingerprint, unchanged, repos: [{ root, label, …diff } \| { root, label, error }] }` (docs/architecture.md → Web API) |
| GET · POST · PUT · DELETE | `/api/hosts/:host/sessions/:id/review[/threads[/:thread[/reply]] \| /submit]` | see docs/architecture.md → Web API | the comment threads (lib/reviews.mjs): GET lists them with replies merged from the transcript (≤ 500 messages back; stored replies stay); POST `threads` `{ path, side, start, end, code, body, draft }`, POST `…/reply` `{ body }`, PUT `{ body?, resolved? }`, DELETE, POST `submit` `{ body? }`. Bodies ≤ 8000 chars, a prompt ≤ 24000 (400), body limit 1 MB. 404 unknown thread, 409 a draft replied to / a sent comment edited / nothing to submit, 501 no reviews |
| GET | `/api/hosts/:host/sessions/:id/complete` | `?kind=skills\|files\|brain` | `{ host, id, kind, skills? \| files?, truncated? \| available?, notes? }` — what the composer autocompletes (lib/complete.mjs). 400 bad kind, 501 no completer |
| GET | `/api/hosts/:host/sessions/:id/files/raw` | `?path=<as in chat or absolute>`, `&download=1` | the file's bytes (streamed; see Files). 400 no path / a directory, 403 outside the sandbox, 404 missing, 413 text over 5 MB inline |
| POST | `/api/hosts/:host/sessions/:id/files/open` | `{ path }` | `{ ok, host, path, revealed, command }` — 403/404 as raw, 501 no `open`/`xdg-open` on the host, 502 the opener failed |
| GET | `/api/hosts/:host/sessions/:id/brief` | | `Brief` (below). No brief yet → the empty skeleton with `exists: false` (not an error). A session that is gone: served from its file by full id, else 404 |
| GET | `/api/hosts/:host/notes/tree` | | `{ host, name, root (~/…), rootAbs, searchEngine: builtin \| command, files: [NoteEntry], truncated, scannedAt, editorUrl }` — 501 when `web.notes.root` is unset on that host, 503 when the root is missing |
| GET | `/api/hosts/:host/notes/search` | `?q=<query>&limit=50` (1..200) | `{ host, q, engine, fallback?, results: [{ path, kind, title, mtime, score, matches: [{ line, text, ranges: [[start, end]] }], more }], total, ms }` |
| GET | `/api/hosts/:host/notes/file` | `?path=<root-relative>` | `{ host, path, abs, kind, size, mtime, title, encrypted, meta: [[key, value \| [values]]], body, bodyLine, text, editorUrl }` — 400 bad path / an image, 404 not listed, 413 over 2 MB |
| GET | `/api/hosts/:host/notes/raw` | `?path=<root-relative image>` | the image bytes (streamed, also through a peer); 400 for anything but an image |
| PUT | `/api/hosts/:host/sessions/:id/brief` | `{ markdown }` (≤ 60000 chars; frontmatter optional — the server keeps its own keys) | `Brief` after the edit (`editedAt` set; resource lines removed by the edit become `dismissed`). 400 not a string |
| POST | `/api/hosts/:host/sessions/:id/brief/regenerate` | `{}` | **202** `{ host, id, started, queued, generating: true }` — returns at once, poll GET until `generating` is false. `started: false, queued: false` = one for this session is already running; `queued: true` = waiting for another session's call. **429** `{ error, retryAfterMs }` at `maxCallsPerHour`; 404 no transcript / gone session |
| GET | `/api/hosts/:host/stacks` | | `{ host, stacks: [StackView] }` (`fleet stack list --json`, which syncs membership first) |
| POST | `/api/hosts/:host/stacks/sync` | `{}` | `{ host, changed: [ids], stacks: [StackView] }` (`fleet stack sync --input - --json` with the merged fleet on stdin, so members on other hosts are judged too; a plain `sync` when it cannot be built) |
| GET | `/api/hosts/:host/dormant` | | `{ host, bootId, dormant: [{ kind, target, name, since, windows, panes, sessions: [{ sessionId, name, title, cwd }] }], closed: [{ …same, closedAt, tmuxSession? }] }` — `fleet --local restore --json` (docs/architecture.md → Session recovery, Recently closed; `closed` only from CLIs that know it). 501 when the CLI predates `restore` |
| POST | `/api/hosts/:host/dormant/restore` | `{ target, dryRun?, closed? }` \| `{ all: true, dryRun? }` (`closed: true` → `--closed`, a recently closed session; never with `all` → 400) | `{ host, restored: [{ kind, from, session, renamed, windows, panes, launched, warnings, commands, dryRun }], failed: [{ target, error }] }`. Starts agents: **409** `{ error, candidates }` ambiguous target, 404 no match, 400 bad body, 502 when nothing was restored; a partial `all` is 200. Refreshes the fleet (now, ~3 s, ~10 s) and then runs a stack sync. Proxy timeout 6 min |
| POST | `/api/hosts/:host/dormant/forget` | `{ target, closed? }` \| `{ all: true, closed? }` | `{ host, forgotten: [names] }`; 409 / 404 as restore; `closed: true` → `--closed`, 501 when the CLI has no `--closed` |
| GET | `/api/hosts/:host/stacks/:id` | `:id` = `st-` + 8 hex | `StackView`. 400 bad id, 404 unknown stack |
| PUT | `/api/hosts/:host/stacks/:id` | `{ markdown, expectUpdated? }` — markdown a string ≤ 64 kB (UTF-8; the request body may be up to 256 kB), `expectUpdated` the `updated` you loaded | `StackView` after the human edit (`fleet stack set`). **409** `{ error, updated }` when the stored `updated` differs (nothing written — reload); 400 bad body / id; 404 unknown |
| POST | `/api/hosts/:host/stacks/:id/rename` | `{ label }` — one line, 1–80 characters | `StackView` with the new label (`fleet stack rename`; `editedAt` stamped). 400 bad label / id; 404 unknown |
| DELETE | `/api/hosts/:host/stacks/:id` | | `{ removed: id }` (`fleet stack rm <id> -f`). 404 unknown |
| POST | `/api/hosts/:host/sessions/:id/stack/spawn` | `{ prompt?, name?, model?, harness?, dir?, label?, host? }` — as spawn; `label` names a stack created here; `host` starts the sibling on that peer | **Sibling spawn**: `fleet stack ensure <session_id>` (creates the stack around the session when it has none: one model call, ≤ 150 s), then this server's spawner in the session's cwd (`dir` must be inside it or inside a spawn dir) with the prompt `contextLine + " " + prompt` (only the context line when empty); the new session is added to the stack in the background. → `{ host, stack: StackView, created, generated, spawn: { ok, host, name, dir, tmuxSession, command, trusted, model } }`. 400 bad body / dir (checked before `ensure`), 404 unknown session, 409 no session id / cwd yet. Proxy timeout 180 s. **On another host** (`host` = a peer with a web url, else 400; `dir` required, a path there, else 400): after `ensure`, `POST /api/hosts/<host>/spawn` on that peer with `dir`, the prompt `remoteContextLine + " " + prompt`, `name` / `model` when given and `harness` (the peer checks `dir` against its spawn dirs; its error comes back with its status, 5xx as 502); the join watches the peer's list and runs `stack add <stack> <session> --on <host> --input -` here; `spawn.host` is the peer |
| POST | `/api/hosts/:host/stacks/:id/spawn` | `{ prompt?, name?, model?, harness?, dir?, host? }` | the same without `ensure`: `dir` defaults to the stack's `absCwd` (400 when that is not a directory here); `created` / `generated` are `false` |

The stack routes need the `fleet stack` subcommand on that host (501 otherwise; absent routes on
an older server are 404). `editor` / `editorUrl` (for `absCwd`) are added to every StackView by the
server that received the request, like briefs. **StackView** = `{ host, id, label, path, cwd,
absCwd, created, updated, generatedAt, editedAt, contextLine, members: [{ session, host, name,
added, closed, live, status, briefPath, briefExists }], markdown, body, parsed: { summary,
resources: [Resource], notes } }` — the CLI's shape, passed through (docs/architecture.md →
Session stacks).

**Brief** = `{ host, id, exists, markdown, parsed: { summary, resources: [{ kind, label, url, path, text, branch, linked }], todos: [{ done, text }], plan }, updated, editedAt, generatedAt, generatedThrough, generating, enabled, continuePrompt, absCwd, gitRoot, editor, editorUrl }` —
`markdown` is the whole file (frontmatter included, canonical form); `kind` is `Origin` | `PR` | `Issue` |
`Artifact` | `Spec` | `File` | `Git` | `Link` | `null` (a hand-written line), or the legacy
`Branch` | `Worktree`; `url` or `path` is set, `text` is the bullet as written; for `Git`, `path`
is the checkout root, `branch` the branch (null = detached) and `linked` whether it is a linked
worktree (both null on other kinds); `parsed.plan` is a **deprecated** alias of `parsed.todos`
(same array, one release); `continuePrompt` is the first prompt for a new session that continues
this one (summary, origin, PRs, worktree, ending with a `---` line the task goes below); `enabled` = background generation is on for that host; `absCwd` / `gitRoot` are
absolute paths on the session's host (null when unknown / outside git); `editor` / `editorUrl`
are filled in by the server that received the request (lib/editor.mjs), also for a proxied
answer — see docs/architecture.md → Session briefs → Open in editor.
Proxied to a peer like the other session routes (PUT bodies included).

**Host** = `{ name, ok, error?, fetchedAt, spawnDirs?: [{ label, path }], hiddenSpawnDirs?: [{ label, path }], home?, notes?: { name }, stacks?: [{ id, label, host, members: [{ session, host, live }] }], sessions: [Session] }`.
`stacks` is that host's session stacks, compact (what its `fleet stack` calls last showed); the server building a merged view stamps `stack: { id, label, host }` on the rows of members that run on other hosts (`lib/stacks.mjs#stampForeign`), since only a stack's home host lists it.
Each host advertises its own `spawnDirs` (absolute paths on that host; the New session menu), its
`hiddenSpawnDirs` when it has any (allowed, not offered: the embed view accepts them), its `home` (to expand a
`~/…` path from a client, e.g. the embed view's `dir`; older servers omit it) and, when it has `web.notes.root`, `notes`.

**Session** = the `fleet list --json` object + `host` + `editorUrl` (for its cwd; set by the server that answered). Sorted `waiting` → `busy` → `idle` →
`unknown`, then `updated_at` descending.

**Message** = `{ role: "user"|"assistant"|"system", kind: "user"|"assistant"|"command"|"system", text, ts, final? }`.
Conversation only: tool calls, tool results, thinking, hooks and sidechains are dropped.
Assistant text that ends a turn is `final: true`; narration between tool calls is `final: false`.
Last `limit` messages, oldest first; `truncated` when older ones exist.

**spawn**: `name` is sanitized (lowercase, `[a-z0-9_-]`, ≤ 40, default `fw-hhmmss`); `dir`
defaults to the host's first offered `spawnDirs` entry and must be an existing absolute directory that,
after `realpath` (symlinks and `..` resolved), is one of this host's `spawnDirs` (hidden ones included) or beneath one
(`$HOME` when none are configured) — anything else is a 400. A taken name is a 409.
Runs `tmux new-session -d -s <name> -c <dir>`, types `claude -n '<name>' '<prompt>'`, and
answers a first-run "trust this folder" dialog with "Yes" (`trusted: true` when it did). With no
`name` (and auto-naming on) it types plain `claude '<prompt>'`: Claude derives `<cwd>-9d`, and the
next naming pass replaces it, and the CLI renames the `fw-hhmmss` tmux session to match — with a
first prompt, a targeted pass for just that session runs as soon as it has answered (see
Auto-naming). `model` (optional; `""` = Claude's default) must be 1–100 chars of letters, digits
and `._[]-` (else 400) and is typed as `--model '<id>'`; it is not checked against `web.models`,
so a client can pick from its own server's list for a peer. The UI sends no `name` any more;
the field stays for other clients. `embed` (optional, `[A-Za-z0-9_-]{1,40}`, else 400): the app
whose embedded Fleet panel spawns it (the [embed view](../docs/architecture.md#embed-view)'s
`app`) — set as the pane option `@fleet_embed`, which `fleet list --json` reports as `embed`;
the UI sets those sessions aside in its Apps filter.
The session shows up in `/api/fleet` once Claude registers it; clients poll for a session whose
`tmux_session` equals `tmuxSession`.

**repos** (Settings → Git repos) is a thin layer over `fleet repos` (docs/cli.md → Repos): GET
reads the config's `repos` (defaults filled in), lists every repo (`--all`: excluded ones flagged)
and whether `~/Library/LaunchAgents/fleet.repos.plist` exists. PUT validates (1–20 roots, absolute
or `~/`; intervals `30m`/`12h`/`7d` ≥ 1m; override/exclude keys a dir name or a `~/` path) and
writes the whole `repos` object with `fleet --local config set`, keeping keys it doesn't know. Each
host has its own settings; the UI's "Save to all hosts" PUTs the same object to each.

**jobs** (the Bots screen; bots were called jobs, and the API keeps that name) is a thin layer over `fleet jobs` (docs/cli.md → Bots): the list, one
job's runs, run now and pause / resume all shell out to `fleet --local jobs …`; the server keeps no
state. Each host lists its own jobs (its config's `jobs`), so the UI asks every host. A successful
run refreshes the merged fleet so the new session shows at once; its row carries `origin: { job,
run }` (from `fleet list --json`), which the UI uses to badge job sessions and hide them by default.

**spawn-dirs** edits this host's `spawnDirs` (Settings → Start directories). GET reads the config
file (a `{ label, path }` / bare-string entry is spelled out as a path for every configured host)
and stats each of this host's paths (`~` expanded). PUT validates: ≤ 30 entries; labels 1–40
chars, one line, unique ignoring case; paths absolute or `~/…`, ≤ 1024 chars, no control
characters, `""` = not offered on that host, at least one per entry; `hidden` a boolean (kept only
when true); other keys on an entry are carried through unchanged; **this host's** path must be
an existing directory — other hosts' paths are stored as given (they are checked on their own
host). A valid list is written with `fleet --local config set spawnDirs '<json>'` (the CLI owns the
file: atomic tmp + rename, every other key and their order kept; writes are serialised), then
`config.spawnDirs` / `config.hiddenSpawnDirs` are replaced in place, so `/api/fleet` and the spawn allow-list use it at once,
and the merged snapshot is rebuilt before the response (a proxied save rebuilds the proxying
server's too). `dryRun: true` validates and answers without writing. The UI writes one shared
list to every host: a dry run on all reachable hosts first, the writes only when all pass.
Editing the list is as powerful as spawning (it widens the allow-list) and has no auth either.

**kill** closes a session for good: SIGTERM to Claude's `pid` (SIGKILL after 4s), then the
terminal that hosted it. tmux: the whole tmux session when Claude's window was its only window
(`terminal: "tmux-session-killed"`), else just that window (`"tmux-window-killed"`). The pane's
session/window ids (`$3`, `@7`, never names) are resolved *before* the signal, because when Claude
is the pane's own command its pane, window or session vanish with it (a target that already
closed itself counts as done); a pane that was already gone is `"tmux-pane-gone"`. iTerm: the tab is closed best-effort (`"iterm-tab-closed"` |
`"iterm-tab-left"`); `unknown` backend: `"left"`. `process` is `terminated` | `killed` | `gone` |
`skipped`. It lives here (lib/kill.mjs, next to the backends) rather than in the CLI: everything
it needs is already in the `list --json` row. The UI exposes it in the ⋯ menu as a two-tap
"Close… → Confirm close" button that returns to the list.

**files** (chat file links → the UI's preview). A path is what the agent wrote: relative (to the
session's cwd), `~/…` or absolute, optionally with `:line`, `:line:col` or `#L12` (split off and
echoed as `line`/`col`). It must resolve — lexically and again after `realpath`, so no `..` or
symlink escapes — inside this host's `$HOME` or the session cwd; anything else is 403 (stat:
`exists: false, forbidden: true`, nothing else revealed). Never served even inside `$HOME`:
`.ssh`, `.gnupg`, `.aws`, `.kube`, `.docker`, `.password-store`, `.config/gh`, `Library/Keychains`,
and files named `.env`, `.env.*`, `.envrc`, `.netrc`, `.pgpass`. A symlinked dir pointing out of
`$HOME` (e.g. to another volume) is refused too.
A relative path missing under the cwd (a session in one repo that edited another and wrote
"Updated docs/x.md") falls back, in order: (1) an absolute path the session touched —
`file_path`/`path`/`notebook_path` of its tool calls and files its shell commands wrote
(`> f`, `tee f`, relative ones against a preceding `cd <dir>`), then (weaker) other absolute paths
in commands and tool results — that ends in `/<path>` at a segment boundary, most recently touched first;
(2) `<root>/<path>` for candidate roots: ancestors of touched files below `$HOME` (nearest and most
recent first, bounded), then `web.files.roots`. Paths containing `..` never fall back. Every
candidate passes the same sandbox + secrets check; the first that exists wins. lib/touched.mjs
reads the transcript incrementally (only the bytes it grew by; the first read scans at most the
last 16 MB; ≤ 2000 paths per session), so a stat costs one `fs.stat` of the transcript when
nothing changed. `raw`/`open` resolve through the same function, and stat returns the absolute
`path`, which the UI passes back.
**FileStat** = `{ input, path (absolute), rel (cwd-relative, else ~/…), line?, col?, exists, forbidden?, resolvedVia? (`cwd` | `touched` | `root`, found only), isFile?, isDir?, size?, mtime?, kind? }`;
`kind` by extension (markdown `.md`/`.markdown`/`.mdx`…, image png/jpg/gif/webp/avif/bmp/ico/svg,
pdf, known binaries → other), else a sniff of the first 4 KB (no NUL, valid UTF-8 → text). Stat
results are cached 5 s per (session, transcript offset, cwd, path). **raw** sets `content-type` by kind — every text kind,
markdown and HTML included, as `text/plain; charset=utf-8` — plus `content-disposition`
(inline / attachment with `download=1`), `x-content-type-options: nosniff`, `x-fleet-kind`,
`cache-control: no-store` and (except PDFs, which Chrome won't show under it)
`content-security-policy: sandbox`, so an SVG opened straight in a tab runs no script on this
origin. No Range support. For a peer the body is piped through (`lib/peers.mjs#streamFromPeer`;
the 20s timeout covers the response head only). **open** runs `open <path>` (macOS) or
`xdg-open <path>` via execFile (argv, never a shell); a file with an executable bit or a
runnable extension (`.app`, `.command`, `.sh`, `.py`, `.pkg`, `.webloc`, …) is revealed instead
(`open -R` / `xdg-open <dir>`, `revealed: true`).

**notes** (the notes explorer, `web.notes`). Everything is root-relative: a path must be plain
(no leading `/`, no `..`, no hidden segment, no NUL / backslash — else 400), resolve after
`realpath` inside the root's realpath (a symlink out is refused) and be a file the tree lists
(else 404). The tree lists `.md`/`.markdown`/`.mdx`…, common text files (`.txt`, `.json`, `.yaml`,
`.sh`, …) and images, at most 5000, 16 levels deep; skipped: every entry starting with `.`
(`.git`, `.obsidian`, `.env`…), `node_modules`, `__pycache__`, `exclude` entries, the simple
patterns of the root `.gitignore` (`name`, `dir/`, `/anchored`, `*.ext`, `**`; negations and
character classes ignored) and symlinked directories (no cycles or duplicate subtrees; symlinked
files inside the root are listed). The `$HOME` secrets deny list of files applies too. Blocks
armored as `-----BEGIN AGE ENCRYPTED FILE-----` are replaced by `[encrypted]` in every answer
and never searched (`encrypted: true`). `NoteEntry` = `{ path, kind: markdown | text | image,
size, mtime, title?, encrypted? }`; `title` = frontmatter `title`, else the first `# heading`,
else the file name. Frontmatter (`key: value`, `key: [a, b]`, `key:` + `- item` lines) comes back
split into `meta` and `body` (`bodyLine` = the body's first line). The tree is cached 3 s; note
texts are cached in memory by mtime + size (≤ 64 MB).
**Built-in search**: the query is split into words (`"quoted phrases"` kept, ≤ 8, ≤ 200 chars);
a note matches when every word is in its path, title, text or — for `#tag` — its frontmatter
`tags`; ranked by tag / title / file-name / path hits, body occurrences and matching headings,
then recency; up to 3 matching lines per note (≤ 180 chars around the first match) with
`ranges`. **`searchCmd`**: run with a 10 s timeout; stdout is read as `path:line:text` lines (grep
-n / rg) or a path line followed by indented match lines; paths (root-relative or absolute) that
are not listed files are dropped; exit 1 with no output = no matches; any other failure falls
back to the built-in search with `fallback` set. `editorUrl` is filled in by the server that
received the request, like brief links. Proxied to a peer like the other host routes (raw streamed).

**Statuses**: 400 bad input, 404 unknown host/session/route (also a `?local=1` request for a
non-self host), 405 wrong method, 409 existing tmux session or uncontrollable backend,
413 body over 64 KB (an upload: over `web.uploads.maxMB`), 502 peer unreachable / non-JSON, 503 local discovery failed, 504 peer timeout.

**Timeouts / caching**: local list cached 2s; peer `/api/fleet` fetch 6s; proxied uploads
15 min (also the server's whole-request budget); proxied session and spawn calls 20s (a proxied `autoname` can outlast that and then answers 504, while the pass still
finishes on the peer).

**uploads**: how the UI attaches files — browsers never expose a local path, so the file is
stored on the host the session runs on and its path is typed into the prompt. The name is
sanitized (last path segment, control chars dropped, anything but letters/digits/`._-` → `-`,
no leading dots, ≤ 80 chars with the extension kept, `file` when nothing is left) and stored as
`<web.uploads.dir>/YYYY-MM-DD/<6 hex>-<name>`, opened exclusively (`wx`) so concurrent uploads of
one name never clobber each other. The body is streamed to disk: a `content-length` over the limit
is refused before anything is written, a chunked body is cut off at the limit and its partial file
deleted. For a peer the body is piped through unbuffered (`content-type` / `content-length` pass
through) and the peer's answer — a 413 included — comes back as is.

**Warm snapshot**: the merged `/api/fleet` (local discovery + every peer) is kept warm
(lib/snapshot.mjs). While anyone asked in the last 90s the server rebuilds it every 3s in the
background and serves the last result immediately (stale-while-revalidate; `snapshotAt` says
when it was built). Only the first request ever waits for a build; after idling, the next one gets
the old snapshot at once and wakes the refresher. Spawn, kill and autoname rebuild it right away.
`?local=1` (what peers poll) bypasses it and uses the 2s local cache. Without this a page load
waited ~1–2s on `fleet list` (one `ps` per session plus an osascript for iTerm tab titles).

**Static**: every non-`/api/` GET/HEAD is served from the UI dir with a weak ETag
(`W/"<size>-<mtime>"`, hex) and `cache-control: no-cache`, so a reload revalidates and gets `304`
(path traversal rejected) — except content-hashed build assets (`/assets/…/<name>-<hash ≥ 8>.<ext>`,
what Vite emits), which get `public, max-age=31536000, immutable`. No SPA fallback: both UIs use
hash routing, so only `/` is ever loaded; a missing file is a 404. No UI dir / no `index.html` →
a placeholder page at `/`.

## Auto-naming

Sessions started without a name keep Claude's cwd+hash fallback (`project-9d`). `fleet name`
generates task-shaped names (`claude -p`, cached) and applies them with Claude's own `/rename`,
holding sessions waiting on a prompt, but only when someone runs it. The web server is the
scheduler: each host's server runs `fleet name --all --apply` every
`web.autoName.intervalMinutes` (default 5, first run 60s after start) and on
`POST /api/hosts/:host/autoname` (⋯ menu → Run now). It already runs on every host, inside tmux,
which on a headless machine is often the one place `claude -p` can reach a logged-in keychain (a
plain ssh shell may not).

There is one rename path: the CLI renames the Claude session (the title) and the tmux session
follows as a slug of it, only for a tmux session that is that one Claude session's own. A tmux
session somebody named by hand (`fleet new fix-login`) is not clobbered — the CLI adopts its name
as the title instead of generating one. The server touches no tmux names itself (docs/architecture.md
→ Session titles). Runs are de-duplicated; `/api/health` reports `autoName.lastRun`.

After an unnamed spawn with a first prompt (auto-naming on), `createSpawnNamer` names that one
session without waiting for the schedule: after 8, 10, 12, 15, 20, 25 and 30s (~2 min) it looks
the session up by its tmux name in a fresh `fleet list`, skips the try while it has not
registered or is still busy on its first turn, stops once it no longer carries a derived name,
and otherwise runs `fleet name <session_id> --apply` — done when renamed, retried when held
(waiting on a prompt) or failed. Out of tries, the periodic pass picks it up.

## Smart grouping

The Board view's groups come from `fleet group` (docs/architecture.md → Smart grouping). Exactly
one server runs it: `grouping.host`, else the one with `web.grouping.enabled`. `lib/grouping.mjs`
there reads the stored groups at start (`--cached`), runs the pass 20s later and then every
`web.grouping.intervalMinutes`, feeding it a freshly built merged fleet (`handleApi.buildFleet()`,
which doesn't warm the snapshot), and checks every 60s whether the *warm* snapshot (only there
while someone is watching; never triggers discovery) holds live sessions no group has — if so, and
the last run is ≥ 2 min old, it runs early. The CLI decides whether the model is called, so a
scheduled run over an unchanged fleet costs one `fleet group` process and no model call. Runs are
de-duplicated; `/api/health` reports `grouping: { enabled, host, lastRun }`.

Other servers answer `/api/groups` (and `/api/groups/edit`) by proxying to `grouping.host` (or the first peer whose
`/api/groups?local=1` says `enabled: true`, cached 5 min); `?local=1` is never forwarded again.

## Session briefs

Format, merge rules, generation and budget: docs/architecture.md → Session briefs. In the server:
`createBriefs` (lib/briefs.mjs) is always created; with `web.briefs.enabled` it also runs a
background pass every 30s over this host's live sessions (`fleet.localHost()`, so it rides the 2s
discovery cache). Per idle session whose transcript grew: model-free extraction
(lib/brief-extract.mjs, incremental, first read ≤ the last 16 MB) merged into the file, then — only
past every gate (idle ≥ `idleMs`, ≥ `minNewTurns` / `minNewChars` new, ≥ `minIntervalMs` since that
session's call, nothing else generating, < `maxCallsPerHour`) — one `claude -p` call. GET also runs
the model-free extraction (so a brief exists as soon as someone looks), never the model. The
`claude` binary is resolved from `PATH`, `~/.local/bin`, `~/.claude/local` and the Homebrew dirs.

## Session stacks

A stack is N sessions sharing one StackBrief file; the CLI owns it (docs/architecture.md →
Session stacks) and the server only shells out to `fleet --local stack … --json`
(lib/fleet-cli.mjs) through `createStacks` (lib/stacks.mjs), always created in server.mjs.

- **Sibling spawn** (`…/sessions/:id/stack/spawn`, `…/stacks/:id/spawn`): the body is validated
  and the directory resolved before anything else (no model call for a bad request), then
  `stack ensure` (session route only), then the normal spawner (lib/spawn.mjs: tmux, trust prompt,
  long prompts via a file) with the StackView's `contextLine` prepended. An unnamed spawn with a
  prompt still gets the targeted naming pass (`createSpawnNamer`), as with `/spawn`.
- **Joining**: `stacks.join(tmuxSession, stackId, host?)` polls this host's fresh `fleet list`
  — or, for a sibling started on a peer, that peer's `/api/fleet?local=1` (`attach({
  peerSessions })`, server.mjs) — after 3, 4, 5, 6, 7, 10, 10, 15 and 15 s (75 s in all) for the
  row with that `tmux_session` and a `session_id` (`findSpawned`, shared with the spawn namer),
  then runs `stack add <stack> <session>` (`--on <host> --input -` with the row for a peer's).
  A refusal (exit 1–3) stops it, other failures retry; each outcome is logged (`[stacks] …`).
  Out of tries, the log says which `fleet stack add` to run.
- **Sync**: `fleet stack sync` runs after every successful kill (best effort, never fails the
  kill), once 30 s after start, and every `web.stacks.syncMinutes` (default 2) while `fleet list`
  (the 2 s cache) shows any session with `stack != null` or this host knows a stack (members may
  run elsewhere). The merged `/api/fleet` body (`attach({ fleetBody })`: the warm snapshot, else
  one fresh build) goes in as `--input -`, so members on other hosts are judged against their
  host's list; a merge that fails means a plain sync. `FLEET_WEB_STACKS=0` turns the background
  pass off. Runs are de-duplicated; a call made during a run gets one trailing run.
  `/api/health` reports `stacks.lastSync` (`merged`: the input went in) and `stacks.known`.
- **Cross-host**: a stack lives on its home host; only that host lists it. Every StackView
  this server sees is kept compact (`compact()`: id, label, host, members' hosts and liveness)
  and advertised in its `/api/fleet?local=1` entry as `stacks`; a server building the merged
  view stamps `stack: { id, label, host }` on the rows of members on other hosts
  (`stampForeign`), so the UI shows one stack across hosts and talks to the home host for it.

## UI (`ui/`, React)

The list and session detail (Chat | Term, composer with quick replies, key chips and autocomplete — `/skill` at the start or mid-sentence in Claude sessions, `@file` for the cwd's `.md` / `.plain` files, `brain:note`; ↑↓, Tab/Enter, Esc), plus a host filter, the notes explorer (`#/notes`), a Settings screen (`#/settings`: text
size for the whole UI, theme (colour palette: Default / Earth / Dusk, `fleet.palette`), light/dark mode, terminal text,
progress notes, and the fleet's Start directories — `spawnDirs`, saved to every host) and a Details panel (the session brief,
then session details): same routes (hash routing `#/`, `#/s/<host>/<id>`), same polling (fleet 5s, messages 3s, peek 2s,
paused while hidden) and the same `fleet.*` localStorage keys (`fleet.snapshot`, `fleet.filter`,
`fleet.detailMode`, `fleet.termFont`, `fleet.termLines`, `fleet.chatFont`, `fleet.chatHideNotes`,
`fleet.spawnHost`, `fleet.spawnDirLabel.<host>`, `fleet.spawnModel`; the old vanilla UI's keys, so its preferences carried over). Built with
React 19 + Tailwind v4 + shadcn/ui; markdown is parsed to an AST and rendered as React elements
(no `innerHTML`, only `http(s)` links). A 404 whose error starts with `unknown session` means the
session is gone; any other 404 from `messages` means there is no transcript yet. Structure and conventions: [ui/README.md](./ui/README.md). Dev: `npm --prefix ui run dev`
proxies `/api` to a running server (`FLEET_WEB_URL`, default `http://127.0.0.1:7777`).

## Running as a service

Run the server from a tmux session or a user service, not a bare launchd agent: under launchd
every `ps` call can take ~0.3–0.5s and discovery runs one per session, which can exceed the 8s
list timeout; on macOS a tmux server started from iTerm2 also inherits the Automation
permission `osascript` needs for iTerm sessions. `fleet web serve` / `fleet web install-service`
in the CLI own this; `bin/dev.sh` runs it in the foreground for development.

## Non-goals (v1)

Auth, HTTPS, tmux sessions without Claude, ANSI colors, xterm.js, websockets, multi-user.
