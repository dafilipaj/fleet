# Architecture

`fleet` has one core and several front-ends. The core knows how to find Claude Code sessions (and
Codex and pi ones — [Other harnesses](#other-harnesses)) on a
machine, drive them, manage tmux sessions and reach other machines. The CLI, the TUI dashboard and
the web app are views on top of it.

```
                ┌────────────────────────── crates/fleet ──────────────────────────┐
                │  core (library)                                                  │
                │   discovery ── Claude registry + ps + tmux/iTerm → Session[]     │
                │   backend   ── peek / send / keys / focus / spawn (tmux, iTerm)  │
                │   naming    ── generated titles + cache                          │
                │   tmux      ── tmux-session management (list/new/kill/stale…)    │
                │   config    ── the shared config file                            │
                │   hosts     ── host resolution, ssh dispatch                     │
                ├──────────────────────────────────────────────────────────────────┤
                │  cli (clap + text/JSON rendering)      tui (ratatui dashboard)   │
                └──────────────────────────────────────────────────────────────────┘
                              ▲ `fleet list --json` etc.
                ┌─────────────┴──────────── web/ ──────────────────────────────────┐
                │  server.mjs (Node ≥ 22, zero deps) — HTTP API + static PWA       │
                │  peers with the same server on other hosts                       │
                └──────────────────────────────────────────────────────────────────┘
```

## Components

### Core library (`crates/fleet/src/core`)

Pure logic plus thin process wrappers; no printing. Everything a future front-end needs is here:

- **discovery** — builds the list of live sessions (see [below](#session-discovery)).
- **backend** — per-backend control: read the screen, type text, press keys, focus, spawn.
  - tmux: `capture-pane -p -J`, `send-keys -l -- <text>` in pieces of at most 512 bytes + `Enter` (Claude Code takes a bigger burst as a paste and drops the first ~1 KB of a write longer than that; multi-line text: `load-buffer` + `paste-buffer -p`, since `send-keys -l` drops newlines), `new-session` / `new-window`. A spawned pane starts as `/bin/sh -c <script>`, which drops the agent markers in `backend::AGENT_ENV` (`CLAUDECODE`, `CLAUDE_CODE_SESSION_ID`, `CODEX_THREAD_ID`, `TRACEPARENT`, … — exact names, never configuration such as `CLAUDE_CODE_OAUTH_TOKEN` or `OTEL_*`), wherever they came from (fleet, the tmux server's global environment or the session's), and then execs the session's effective `default-command` (asked of tmux from inside the pane, once the server has its config) or `$SHELL -l` — so the agent starts from a login shell that does not think it is part of the agent that spawned it; fleet also keeps the markers out of a tmux server it starts.
  - iTerm: AppleScript via `osascript`; values are passed as argv, never interpolated into the
    script. Text is written without a newline, then an empty write submits it (a trailing newline
    would be swallowed by bracketed paste).
- **naming** — generates short kebab-case titles by asking `claude -p --model haiku` what a
  session is working on (falls back to git branch / first prompt); cached across runs.
- **tmux** — tmux-*session* management inherited from `ws`: matching, sanitizing, stale detection.
- **config** — reads and patches the shared config file.
- **hosts** — resolves the target host and dispatches over ssh.
- **tools** — binary lookup (`PATH` plus Homebrew fallbacks), `~` expansion.
- **brief** / **stack** — the [session brief](#session-briefs) and [session stack](#session-stacks)
  files: format, merge rules, storage.
- **repos** / **prs** — [`fleet repos`](cli.md#repos) (keep checkouts fresh) and
  [`fleet prs`](cli.md#pull-requests) (the GitHub PRs that need you, through `gh`, joined with
  those checkouts and their worktrees).
- **snapshot** — [session recovery](#session-recovery): records what runs, marks it dormant after
  a reboot, restores it.

### CLI and TUI

`crates/fleet/src/cli` parses arguments and renders text or JSON; `crates/fleet/src/tui` is the
`watch` dashboard (ratatui). Neither contains discovery or backend logic. `cli/mcp.rs` (`fleet
mcp`) is another view: an MCP server whose tools are generated from the clap command tree, each call
re-running the binary with the matching argv — so the CLI stays the one contract.

### Host dispatch

Every command has a target host: `-H/--host <name>`, else `FLEET_HOST`, else a default
(`defaultHost` for tmux-session commands, this machine for everything else). `--local` forces the
local machine.

- target == `self` → run locally;
- otherwise → `ssh <hosts.<name>.ssh> fleet <same args>`.

The remote runs `fleet --local <args>` (so it never hops again), found as `hosts.<name>.fleetBin`,
then `~/.local/bin/fleet`, then `PATH` (with `FLEET_CMD`/`FLEET_DEBUG`/`FLEET_DRY_RUN` exported, never
`FLEET_CONFIG`) — install it with `fleet install --host <name>`. ssh is
invoked with a connect timeout (`FLEET_CONNECT_TIMEOUT`, default 5 s), keepalives and ControlMaster
multiplexing (off with `FLEET_MUX=0`); a tty (`-t`) is requested when both stdin and stdout are
terminals, otherwise `-T` with `BatchMode`. Directory arguments under `$HOME` are rewritten to `~/…`
so they resolve on the remote. An ssh failure (exit 255) becomes a friendly message and exit code 4.
`FLEET_DRY_RUN=1` prints what would run.

There is no chained hopping: a host dispatches only to hosts in its own config.

### Web server (`web/`)

Every machine runs the same `server.mjs`. It:

- serves the static UI, installable as a PWA: the React app built to `web/ui/dist/`
  (`fleet web build`; until then `/` is a "not built" placeholder);
- exposes **its own** sessions, discovered with `fleet list --json` (cached ~2 s);
- drives its own backends directly for peek/send/keys (it needs untruncated, header-free output);
- reads chat transcripts from `~/.claude/projects/<encoded-cwd>/<session-id>.jsonl`;
- for the whole-fleet view, fetches each peer's local list over HTTP and merges.

Requests for another host are proxied once to that peer with `?local=1`, and requests carrying
`?local=1` are never proxied again, so there are no loops. Either machine's URL shows everything.

The server has no auth; it relies on the network (tailnet) for access control. The files
endpoints (chat file links → preview) therefore only serve paths that, after `realpath`, are
inside that host's `$HOME` or the session's cwd, and never secret stores like `~/.ssh` or `.env`.

## Config

One JSON file per machine, owned by the CLI (`fleet init`, `fleet config set`) and read by the web
server. Path: `$FLEET_CONFIG`, else `${XDG_CONFIG_HOME:-~/.config}/fleet/config.json`.

```json
{
  "version": 1,
  "self": "laptop",
  "defaultHost": "workstation",
  "hosts": {
    "laptop":      { "ssh": null,          "web": "http://100.x.y.z:7777" },
    "workstation": { "ssh": "workstation", "web": "http://100.x.y.z:7777" }
  },
  "web":  { "port": 7777, "bind": "0.0.0.0", "dir": null },
  "tmux": null,
  "spawnDirs": [
    { "label": "Project", "paths": { "laptop": "~/Code/project", "workstation": "~/Code/project" } }
  ]
}
```

| key | meaning |
| --- | --- |
| `self` | which `hosts` entry this machine is |
| `defaultHost` | target for tmux-session commands when no `-H` is given |
| `hosts.<name>.ssh` | ssh destination (an `~/.ssh/config` alias or `user@host`); `null` for `self` |
| `hosts.<name>.web` | that host's web server URL; hosts without one are not web peers |
| `web.port` / `web.bind` | web server listen address; with no config at all it binds `127.0.0.1` only |
| `web.dir` | where the web app lives (repo checkout or install dir), detected by `init` |
| `web.node` | `node` binary that runs the web app; `null` → `/opt/homebrew/bin/node`, `/usr/local/bin/node`, then `PATH` |
| `web.editor` | `"vscode"` (default) \| `"cursor"` \| `null`: the scheme of the "Open in editor" link the API puts on briefs and session rows (`editorUrl`, see [Session briefs](#session-briefs)); `null` = no link (the UI hides the button) |
| `web.editorSsh` | ssh destination other machines use for **this** host in those links (e.g. the alias in the laptop's `~/.ssh/config`); default `<user>@<the hostname the browser used>` |
| `web.ui` | static UI directory to serve instead of the bundled one; unset → `web/ui/dist`. The old `"classic"` value (the removed vanilla UI) is ignored |
| `web.quickReplies` | composer chips: strings or `{ label, text }` |
| `web.models` | New session model picker: `[{ id, label }]` or bare ids (`""` = Claude's default, no `--model`); default Default, Fable 5.1, Opus 5.5, Sonnet 5, Haiku 4.5 |
| `web.autoName` | `{ enabled, intervalMinutes }` (default off, 5 — opt in with `enabled: true`): the web server runs `fleet name --all --apply` on its host on that schedule (tmux names follow the titles, see [Session titles](#session-titles)); a web spawn with a first prompt also gets a targeted `fleet name <id> --apply` once it has replied |
| `web.grouping` | `{ enabled, intervalMinutes }` (default off, 10): this host's web server runs `fleet group` over the whole fleet on that schedule and serves `/api/groups` (see [Smart grouping](#smart-grouping)) |
| `web.uploads` | `{ dir, maxMB, retentionDays }` (default `~/.local/share/fleet/uploads`, 100, 14): files dropped / pasted / picked in the web UI are stored there on the session's host as `YYYY-MM-DD/<rand>-<name>`, and their absolute path goes into the prompt; day dirs older than `retentionDays` are removed (`0` keeps them) |
| `web.briefs` | `{ enabled, model, idleMs, minIntervalMs, maxDeltaChars, maxCallsPerHour, minNewTurns, minNewChars, maxBriefChars }` (default off, `haiku`, 60 s, 15 min, 12000, 12, 2, 2000, 3000): background generation of [session briefs](#session-briefs) on this host; reading and editing briefs (and a manual regenerate) work either way |
| `web.stacks` | `{ syncMinutes }` (default 2): how often this host's web server runs `fleet stack sync` while any of its sessions is in a [stack](#session-stacks) |
| `web.memos` | `{ dir, audioDir, model, vocabulary }` (default `~/.local/share/fleet/memos`, `~/.local/share/fleet/memo-audio`, `haiku`, `[]`): voice memos (`#/memos`). Each memo is `dir/<date>-<slug>.md` (frontmatter `title`, `created`, `duration`, `recording`, `status`; the transcript as the body), so `dir` can be a folder in a notes repo; segment audio stays in `audioDir`. `model` titles them via `claude -p`; `vocabulary` (names, projects, tools — up to ~1500 chars) steers the spelling of every segment's transcription. Needs the transcription key |
| `web.notes` | `{ root, name?, searchCmd?, exclude? }` (default off): the web UI's notes explorer (`#/notes`) over the markdown notes under `root` on this host — browse, full-text search (built in, or an external `searchCmd` such as `rg -n -i -F {query}`), preview with `[[wiki]]` / relative links between notes; hidden entries, `node_modules`, `.gitignore`d paths and age-encrypted blocks are never served (web/ARCHITECTURE.md → notes). Peers browse each other's notes through the usual proxy |
| `web.operator` | `{ hosts?, model?, voice?, clearIdleMinutes? }` (default `[self]`, Claude's default model, `alloy`, 60): the [voice operator](#voice-operator) — `hosts`: where it runs, the first that answers (set the same list on every machine so all talk to one operator); the model its Claude session runs; the OpenAI voice that reads its replies; `/clear` after that many idle minutes (0 = never) |
| `web.files.roots` | array of dirs (`~` expanded, default `[]`): extra roots a relative file path in chat may be under. A relative path missing under the session cwd first matches files the session touched (from its transcript), then ancestors of those, then these roots; the sandbox stays `$HOME` + cwd |
| `desktop.url` | the server [Fleet.app](desktop.md) opens instead of `hosts.<self>.web`; unset → this machine's server |
| `tmux` | tmux binary; `null` → `PATH`, then `/opt/homebrew/bin`, `/usr/local/bin` |
| `hosts.<name>.fleetBin` | path to `fleet` on that host; `null` → `~/.local/bin/fleet`, then `PATH` |
| `fleetBin` | this machine's `fleet` binary (used by the web server) |
| `claude` | command that launches Claude Code in spawned sessions (default `claude`) |
| `harnesses` | `{ codex: { command }, pi: { command } }`: how the [other harnesses](#other-harnesses) launch (default the bare name); read leniently |
| `spawnDirs` | directories offered for new sessions, per host (`paths.<host>`); also edited from the web UI (Settings → Start directories, which writes it through `fleet config set`) |
| `tui` | dashboard preferences: `rows` (`"1"`, `"2"`, `"auto"`), `mouse` |
| `naming` | generated names: `enabled`, `model` (default `haiku`), `syncTmux` (tmux name follows the title, default on), `autoTitle` |
| `stacks` | [session stacks](#session-stacks): `enabled` (default `true` — `false` = never call the model; a new stack gets the skeleton StackBrief), `model` (default `sonnet`) |
| `restore` | [session recovery](#session-recovery): `onBoot` (default `false`) — when `true`, this host's web server runs `fleet --local restore --all` once at start if there are dormant sessions, at most once per boot (it remembers the boot id in `${XDG_STATE_HOME:-~/.local/state}/fleet/web-restored-boot`); the result is logged. For an always-on machine. Recently closed sessions are never part of it. `keepClosedDays` (default `7`, a number ≥ 0; `0` turns the [recently closed](#recently-closed) list off) — how long a session closed within a boot stays restorable; at most the newest 50 are kept |
| `jobs` | [`fleet jobs`](cli.md#jobs), per machine: `{ "<name>": { schedule, dir, prompt \| promptFile, command, harness, model, enabled, keep } }` — scheduled sessions on this host. Read leniently: a bad job is a warning and is skipped |
| `repos` | [`fleet repos`](cli.md#repos), per machine: `roots` (default `["~/Code"]`; every directory directly under a root with a `.git` is a repo), `every` (default `"24h"`; `30m` / `12h` / `7d`), `overrides` (`{ "<dir name or ~/path>": "30m" }`), `exclude` (dir names or `~/` paths). Read leniently: a bad value is a warning and falls back to its default |
| `prs` | [`fleet prs`](cli.md#pull-requests), per machine: `orgs` (search only these orgs), `repos` (`owner/name`s; with `orgs`, any of them), `refresh` (default `"5m"`, at least `1m`: how old the cache may get), `worktrees` (default `"~/Code/worktrees"`: where `fleet pr checkout` puts new worktrees). None of `orgs`/`repos` = all of GitHub. Read leniently |
| `gh` | `gh` binary for `fleet prs`; `null` → `PATH`, then `/opt/homebrew/bin`, `/usr/local/bin` |
| `grouping` | smart grouping: `enabled` (default `true` — `false` = repository fallback only), `model` (default `haiku`), `host` (the one host whose web server runs it; peers proxy `/api/groups` there), `consolidateMinutes` (default 60) |

Rules: `~` is expanded at use time; unknown keys are preserved when the CLI rewrites the file
(writes go through the raw JSON, never the typed view); a missing file is fine — this machine is
the single host `local`, and remote features say "run `fleet init`"; a present but invalid file is
an error.

Env overrides for the web server: `FLEET_WEB_PORT` (or `PORT`), `FLEET_WEB_BIND`, `FLEET_WEB_UI`,
`FLEET_WEB_AUTONAME`, `FLEET_WEB_GROUPING`, `FLEET_WEB_BRIEFS`, `FLEET_BRIEFS_DIR`, `FLEET_STACKS_DIR`, `FLEET_BIN`, `FLEET_TMUX` — see [web/README.md](../web/README.md).

## Session discovery

Claude Code keeps a live registry: one `~/.claude/sessions/<pid>.json` per running interactive
session with its session id, cwd, name, status (`busy` / `idle` / `waiting`) and what it waits for.
That is the source of truth; `fleet` enriches each entry:

1. **pid → tty** via `ps`.
2. **tty → terminal**: match against `tmux list-panes -a` (pane id, window name, tmux session) and,
   on macOS, iTerm's sessions (session id, tab title). The match decides the **backend**
   (`tmux` / `iterm` / `unknown`) and the **handle** used to control it (tmux pane id like `%87`,
   or the iTerm session id).
3. **titles** from the naming cache (never generated by `list` itself).

Headless runs (`claude -p`, SDK and CI entrypoints) register too and are filtered out.

### Other harnesses

Codex and pi sessions (`core::harness`) have no registry. One `ps -axww -o pid,etime,tty,args`
finds their interactive processes with a terminal (`codex`, `codex resume|fork`, `pi` — not
`codex exec|app-server|…`, `pi -p|--mode json|install|…`); one `lsof -d cwd` gives their cwds. A
harness whose state dir (`$CODEX_HOME` or `~/.codex`, `$PI_CODING_AGENT_DIR` or `~/.pi/agent`) does
not exist is skipped. Each process is matched to a transcript:

- **Codex**: `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`, first line `session_meta` (`id`,
  `cwd`, `timestamp`); only files written to since the oldest process started are read. Names come
  from `~/.codex/session_index.jsonl` (`/rename`, or the title Codex generates) with
  `name_source: "codex"`.
- **pi**: `~/.pi/agent/sessions/--<cwd, / as ->--/*.jsonl`, first line `{ type: "session", id,
  cwd, timestamp }`; `/name` is a `session_info` entry (`name_source: "user"`).

Matching: an id on the command line (`codex resume <id>`, `pi --session <id>`) wins; otherwise,
later-started processes first, the newest transcript in the process's cwd created after it
started (3 s slack), then the newest written to since it started. A Codex session before its first
prompt has no transcript yet: `session_id` is `null` and its title `<cwd>-codex`.

Status from the transcript tail (512 KiB): Codex is `busy` from `task_started` until
`task_complete` / `turn_aborted`, `waiting` on an `*_approval_request` (until anything else
happens). Codex 0.162 records no approval event, so a `busy` Codex session in a tmux pane also
has its screen read (`capture-pane -p`, at most 500 ms, so a hung tmux cannot stall a poll). An
approval prompt is the bottom-most block of numbered `Yes, …` / `No, …` answers with nothing but
its own wrapped lines, its footer (`Press enter to confirm or esc to cancel`) and blank lines below
it — the composer, a working indicator or any later conversation item below means the dialog was
quoted or already answered — whose head (the lines between the last conversation item and the
answers, joined, so a title wrapped in a narrow pane still counts) holds a title: `Would you like
to run the following command?`, `… make the following edits?`, `… grant these permissions?`, `Do
you want to approve network access to …`, `… needs your approval.`. It makes the session
`waiting`, `waiting_for` `exec approval` / `patch approval` / `permissions approval` / `network
approval` / `terminal input approval` / `mcp approval`; a screen without one leaves it `busy`.
Without a readable screen (iTerm, a pane that does not answer in time), a tool call whose
`sandbox_permissions` argument is `"require_escalated"` and has no output yet counts as `exec
approval` — the field itself, read from the call's JSON arguments or, for the code-mode `exec`
tool, from its JavaScript with string literals treated as data, so a command that only mentions
the words is no request (it is briefly wrong for a command approved earlier for the session,
while it runs); pi is `busy` while the last message is the user's, a tool result, or an assistant turn
that stopped for a tool call. `context` comes from Codex's last `token_count`
(`last_token_usage.input_tokens` of `model_context_window`); pi records no window, so it is `null`.

Targets (`peek <target>`, `send <target>` …) resolve against title, name, session-id prefix or pid,
walking exact → prefix → substring → characters-in-order; two matches on the same rung is an error
listing the candidates, so a loose fragment can never type into the wrong session.
`pid:<n>` and `id:<session_id>` (`discovery::exact_target`) skip the ladder: exactly that pid or
that full session id, or `TargetError::NotFound` — a pid that has just exited never falls through
to a title with the same digits. They also skip the fleet: `discovery::find_exact` reads
`<pid>.json` for `pid:`, or parses every registry entry to find those naming the `id:` (files
only, no process); inspects the pids found with one `ps` (tty, environment, start time — an entry's
`procStart` must be the same UTC instant, or the same text when it is not a time, so a reused
pid is not the session); lists tmux panes only for a tmux session; and reads only the chosen
session's transcript head (≤ 400 lines, ≤ 1 MiB) for its title. With no registry entry naming
it, the target can only be a Codex / pi session: the harness processes are listed and paired
with their transcripts as in the full discovery, and only the named one becomes a row. A `--json` verb reports a target failure as
`{ ok: false, host, kind, error }` — `kind` `target_not_found | target_ambiguous | target_invalid`,
or `stack_not_found | stack_ambiguous` for a `<stack>` (stack keeps exit 2 / 3).

## Session titles

One title per session, one source of truth (`core::title`):

- **The Claude session name is the truth.** `/rename` sets it and Claude persists it in its
  session registry (`name`, `name_source: "user"`).
- **`display_title`** is derived once, in core, and shipped in `list --json`: the Claude name when
  someone chose it (`name_source` ≠ `derived`), else the generated title (`gen_title`), else a
  slug of the first prompt, else Claude's derived `<cwd>-9d` name, else the short session id / pid.
  The CLI, TUI and web UIs draw only this; the tmux name is at most metadata (details panel, the
  TUI's terminal column).
- **The tmux session name is derived**: a slug of the title (lowercase kebab, common Latin
  accents folded, ≤ 48 chars), `-2`, `-3`… on a collision; a name already derived from the title
  (with or without its suffix) is left as is. It is synced on **every** rename — `fleet rename`,
  `fleet name --apply` (auto-naming), the TUI rename buffer, the web UI — but only when the tmux
  session is that Claude session's own: one window, one pane. Shared sessions, `fleet` and the
  dashboard's own session are left alone, with a note saying so. `naming.syncTmux = false` or
  `--no-tmux-sync` opts out.
- **Reverse edge**: `fleet tmux rename` of a single-Claude tmux session renames the Claude session
  (and so the title); raw `tmux rename-session` is not watched and is overwritten the next time the
  title changes. Auto-naming never clobbers a hand-picked tmux name: a derived-name session alone in
  a non-generic tmux session (not `fw-hhmmss`, digits or `<cwd>-xx`) adopts that name as its title.
- **iTerm** tab titles are not touched: Claude already puts its session name in the terminal title,
  and a tab title set by hand is the user's.
- **When `/rename` may be typed**: `/rename <title>` + Enter goes into the live Claude TUI. A
  session **waiting** on a permission prompt or question is held (the keys would answer it; `--force`
  overrides). A **busy** session is renamed: Claude Code runs `/rename` as a local command the moment
  it is submitted, mid-turn, and the turn carries on untouched (verified live; an older Claude that
  queues input would run it after the turn). Codex and pi get their own command (`/rename`,
  `/name`), and a **busy** one is held (`held: "busy"`): they would queue the text as a message.
  Nothing is queued by fleet — a held rename is refused
  with a message and retried by the caller. Known gap: text half-typed into the session's prompt
  box would be prefixed to `/rename`.

## JSON contracts

These are stable APIs — the web app, scripts and future UIs depend on them. Add fields freely;
never rename, remove or change the type of an existing one without a version bump.

### `fleet list --json`

An array of sessions; every row carries `host` (the name the *caller's* config knows that machine
by — with no config, `local`):

| field | type | notes |
| --- | --- | --- |
| `pid` | number | agent process id |
| `session_id` | string \| null | Claude session UUID |
| `name` | string \| null | Claude session name |
| `name_source` | string \| null | `"derived"` = Claude's cwd+hash fallback |
| `cwd` | string \| null | working directory |
| `status` | string | `busy` \| `idle` \| `waiting` \| `unknown` |
| `waiting_for` | string \| null | what a `waiting` session is blocked on |
| `updated_at` | number \| null | last status change, ms since epoch |
| `tty` | string \| null | |
| `backend` | string | `tmux` \| `iterm` \| `unknown` |
| `handle` | string \| null | tmux pane id or iTerm session id |
| `tab` | string \| null | tmux window name / iTerm tab title |
| `tmux_session` | string \| null | tmux session the pane lives in |
| `title` | string \| null | first prompt (can be long) |
| `gen_title` | string \| null | generated title, if cached |
| `display_title` | string | **the** title every view draws — see [Session titles](#session-titles) |
| `context` | object \| null | context-window usage (below); `null` when no transcript usage is found |
| `stack` | object \| null | `{ id, label }` of the [session stack](#session-stacks) the session is a member of; `null` when none (read-only stamp: `list` never syncs or writes stack files) |
| `harness` | string | `claude` \| `codex` \| `pi` — see [Other harnesses](#other-harnesses) |
| `model` | string \| null | the model it runs, when known (Claude: the context entry's) |
| `transcript` | string \| null | absolute transcript path (Codex / pi only) |
| `origin` | object \| null | `{ job, run }` of the [job](cli.md#jobs) run that started the session (its tmux pane option `@fleet_job_run`); `null` when none |
| `host` | string | which machine the row came from |

`list --all-hosts --json` is the same array across every configured host.

#### Context usage

`context` is `{ used, window, pct, model }`, computed in `core::context` from the tail of the
session's transcript (`~/.claude/projects/<cwd with / and . as ->/<session_id>.jsonl`, falling
back to a scan of the project dirs when the session's cwd changed):

- **`used`** (number): prompt tokens of the **last main-thread assistant entry** with real usage —
  `input_tokens + cache_read_input_tokens + cache_creation_input_tokens`. Sidechain (subagent)
  entries, API-error entries, `<synthetic>` and all-zero usage are skipped. Output tokens are not
  counted (the same sum Claude Code's statusline context percentage uses); the reply is counted as
  input on the next turn.
- **`window`** (number): `200000` or `1000000`, inferred — transcripts don't record it. 1M when the
  model id ends in `[1m]`, when the model is natively 1M in Claude Code without a suffix (every
  Fable and Mythos, Opus / Sonnet 5.5 and later), when `used` already exceeds 200k, when `settings.json` pins a `[1m]`
  model of that family, when Claude Code's `~/.claude.json` has recorded that exact model id as
  `<id>[1m]`, or when another live session on the same model id is past 200k without a suffix
  (the model is natively 1M). Otherwise 200k — so a 1M session under 200k with none of these
  signals over-reports its percentage.
- **`pct`** (number): `round(used / window * 100)`; can exceed 100.
- **`model`** (string \| null): `message.model` of that entry.

### Subscription usage

`fleet usage --json` (and the web's `GET /api/hosts/:host/usage`, which adds `host`) reports the
Claude subscription limits of the account logged in to Claude Code on that machine, from
`core::usage`:

```json
{
  "account": { "uuid": "…", "email": "…", "organization": "…", "plan": "max", "tier": "default_claude_max_5x", "plan_label": "Max 5x" },
  "limits": [
    { "kind": "session", "group": "session", "label": "Current session", "model": null, "percent": 18.0, "severity": "normal", "resets_at": "2026-09-29T22:20:00+00:00", "active": false },
    { "kind": "weekly_all", "group": "weekly", "label": "Weekly · all models", "model": null, "percent": 38.0, "severity": "normal", "resets_at": "…", "active": true },
    { "kind": "weekly_scoped", "group": "weekly", "label": "Weekly · Fable", "model": "Fable", "percent": 37.0, "severity": "normal", "resets_at": "…", "active": false }
  ],
  "extra_usage": { "enabled": false, "used": 0.0, "limit": null, "currency": "USD", "percent": 0.0 },
  "fetched_at": "2026-09-29T20:00:00+00:00",
  "stale": false,
  "error": null
}
```

- **Source**: the plan-usage endpoint Claude Code's `/usage` calls (`GET
  https://api.anthropic.com/api/oauth/usage`), authenticated with Claude Code's OAuth access
  token — the macOS keychain item `Claude Code-credentials`, else `~/.claude/.credentials.json`.
  The endpoint is undocumented: its generic `limits` list is used when present, the named windows
  (`five_hour`, `seven_day`, `seven_day_opus`, `seven_day_sonnet`) otherwise. `curl` makes the
  request, with the token on stdin (never in argv). The token is never refreshed (that would
  rewrite Claude Code's credentials): an expired one is an error until Claude Code runs again.
- **`account`**: from `~/.claude.json` (`oauthAccount`) and the credentials' `subscriptionType` /
  `rateLimitTier`; `null` when unknown. The web groups hosts by `account.uuid`, so machines on one
  subscription share one card.
- **`limits[].active`**: the limit currently binding. `severity` is the endpoint's (open set:
  `normal`, `warning`, …); `percent` may exceed 100.
- **Caching**: `~/.claude/fleet-usage.json` holds the last good read and the last failure. A read
  younger than 60s is served from it; after a failure the last good read comes back with
  `stale: true` and `error`, and the endpoint isn't asked again for 2 minutes. With no good read
  at all, the command fails (exit 1) with the reason.

Only the tail is read (256 KiB, widened to 2 MiB then 8 MiB if no usable entry is in it), and the
result is cached in-process per `(path, size, mtime)`, so the `watch` dashboard re-reads a
transcript only after it has grown. Views colour `pct` in the same bands: under 60 dim, 60–85
amber, over 85 red.

### `fleet spawn --json`

One object: where the new session landed (`host`, `backend`, `tmux_session`, `tmux_window`,
`handle`, `created_session`, `dir`, `harness`, `model`, `name`, `command`, `prompt_file`, `from`,
`dry_run`, `ok: true`), or why it failed (`ok: false`, `kind`, `error`, `host`, `partial`; exit 1,
4 for an unreachable `-H` host). `partial` (`Error::Partial`) says where the terminal is when it was
created before the failure. With `-H`, the dispatch in `main` captures the host's own object and
passes it through, or prints the failure to reach it, so the output is one object either way.
`kind` is a stable `snake_case` code (`dir_missing`, `name_taken`, `harness_missing`, …) carried by
`Error::Tagged` from where the failure happens (`core::backend`, `SpawnOpts::resolve`); untagged
errors report `io` / `json` / `other`. See [cli.md](cli.md#spawning) for every field and kind.

### `fleet group --json`

See [cli.md](cli.md#grouping) for the shape. State file: `groups.json` (below).

### `fleet tmux list --json` / `fleet tmux stale --json`

See [cli.md](cli.md#tmux-sessions) for the shapes. `tmux list --json` lists live sessions only;
dormant ones are in `fleet restore --json`.

### `fleet prs --json`

See [cli.md](cli.md#pull-requests) for the shape. Column ids and `Pr` fields follow the add-only
rule; `local` and `rotting` are worked out on every call and never cached, and so is the **unseenMentions** column:
the cache keeps each PR's own column and its `mention`, and every read moves the PRs whose mention
is neither answered nor marked seen (`prs-seen.json`) there.

### `fleet restore --json`

`{ host, bootId, dormant: [DormantView…] }`, and the restore / forget results — see
[cli.md](cli.md#session-recovery) for the shapes and [Session recovery](#session-recovery) for the
state file behind them.

### Web API

JSON over HTTP, errors as `{ "error": "..." }`. At a high level:

| method | path | purpose |
| --- | --- | --- |
| GET | `/api/health` | name, version, apiVersion, self, uptime, autoName (`lastRun`) |
| GET | `/api/settings` | apiVersion, self, host names, quick replies, `transcribe: { enabled }` (this server has a transcription key), `speak: { enabled, voice }` (it can read replies aloud), `operator: { enabled, hosts }` (the [voice operator](#voice-operator) and where it runs) |
| GET | `/api/fleet[?local=1]` | `{ self, hosts: [{ name, ok, error?, sessions, spawnDirs, notes?, stacks? }], snapshotAt }` — `stacks`: that host's [session stacks](#session-stacks), compact (`{ id, label, host, members: [{ session, host, live }] }`), from which the merged view stamps `stack: { id, label, host }` on the rows of members running on *other* hosts; sessions are `list --json` objects + `host` (a Codex session with no transcript yet gets `session_id: "pid-<pid>"` until it has one) (`title` capped at 300 chars) + `editorUrl` (the "Open in editor" link for its cwd, built by the server that answered — see [Session briefs](#session-briefs)); the merged view is a warm background-refreshed snapshot |
| GET | `/api/hosts/:host/sessions/:id/peek?lines=N` | plain-text screen |
| GET | `/api/hosts/:host/sessions/:id/messages?limit=N` | conversation (no tool calls) from the transcript; an AskUserQuestion call is a message of `kind: "question"` with `id` (the tool use id), `questions` (`[{ question, header, multiSelect, options: [{ label, description }] }]`), `text` (a markdown rendering) and, once it has a result, `answers` (per question, the answer string) or `declined: true`; a prompt typed while Claude works and still in its queue is a `kind: "user"` message with `queued: true`, always last (prompts it took mid-turn show where it took them) |
| POST | `/api/hosts/:host/sessions/:id/send` | `{ text }` → typed + Enter |
| POST | `/api/hosts/:host/sessions/:id/keys` | `{ key }` (Enter, Escape, …) |
| POST | `/api/hosts/:host/sessions/:id/answer` | `{ toolUseId, answers: [{ options: [index…], text? }] }` (one per question) → types the arrows/Enter/text a human would into the pending AskUserQuestion prompt. 400 bad answer (single-select: one option or text; multi-select: at least one), 404 unknown question, **409** (nothing typed) when it is already answered, the session isn't waiting, or the terminal isn't showing that prompt untouched (first question, cursor on option 1) |
| POST | `/api/hosts/:host/sessions/:id/rename` | `{ title }` (1–64 chars, one line) → `fleet rename <session_id> <title> --json`; 200 with the report, **409** when held (waiting on a prompt, or a busy Codex / pi session; nothing typed), 400 bad title, 502 CLI failure |
| POST | `/api/hosts/:host/spawn` | `{ name?, dir?, prompt?, model?, harness? }` → new tmux session running that [harness](#other-harnesses) (`claude` default, `codex`, `pi`; its launcher is config `claude` / `harnesses.<id>.command`; `-n <name>` for Claude and pi only); `dir` must resolve inside one of the host's `spawnDirs` (else 400); a prompt too long to type (the launch line over ~900 bytes) is written to `~/.claude/fleet-prompts/` and passed as `"$(cat <file>)"`; a first-run trust prompt is accepted (Claude: Down + Enter; Codex: Enter on "Trust and continue"); when the agent never starts (the pane is still just a shell after ~10s) the tmux session is killed and the answer is 502 with the pane's last lines |
| GET | `/api/hosts/:host/harnesses[?refresh=1]` | `{ host, harnesses: [{ id, label, command, installed, models }] }` — `fleet --local harness list --json`, cached 10 min; Claude's `models` are `web.models`. An older CLI answers Claude alone |
| GET | `/api/hosts/:host/usage[?refresh=1]` | that host's Claude subscription limits: `fleet usage --json` + `host` (see [Subscription usage](#subscription-usage)); 502 with the CLI's reason (no login, endpoint down with nothing cached) |
| POST | `/api/hosts/:host/prs/checkout` | `{ pr: "owner/repo#123" }` → `{ host, worktree }`: `fleet pr checkout --json` on that host |
| POST | `/api/hosts/:host/prs/seen` | `{ pr: "owner/repo#123" }` → `{ host, pr, seenAt }`: `fleet pr seen --json` on that host — its @mentions of you so far are seen |
| POST | `/api/hosts/:host/prs/spawn` | `{ pr, prompt?, name?, model?, harness? }` → `{ host, worktree, spawn }`: the checkout, then a new session in the worktree whose first prompt is `worktree.contextLine` + `prompt` (like a stack sibling: the web spawner, not `fleet pr spawn`). Starts an agent |
| GET | `/api/hosts/:host/prs[?refresh=1]` | the GitHub PRs that need you, as `gh` on that host sees them: `fleet prs --json` + `host` (see [cli.md](cli.md#pull-requests)), and `local.editorUrl` ("Open in VS Code / Cursor" for the worktree, like a session's `editorUrl`); 502 with the CLI's reason (no `gh`, not logged in, GitHub down with nothing cached), 501 when that host's CLI predates `fleet prs` |
| GET | `/api/hosts/:host/spawn-dirs` | that host's stored `spawnDirs` (canonical `{ label, paths }`), whether each of its own paths is a directory there, and what it offers now |
| PUT | `/api/hosts/:host/spawn-dirs` | `{ spawnDirs, dryRun? }` → validated on that host (its own paths must exist), written with `fleet --local config set spawnDirs`, used at once (no restart); 400 with per-entry `errors`. As powerful as spawn (it widens the spawn allow-list); no auth, like spawn |
| GET/PUT | `/api/hosts/:host/repos` | Settings → Git repos: that host's `repos` settings, every repo (`fleet --local repos --json --all`) and its sync timer; PUT writes `repos` with `fleet --local config set` |
| GET | `/api/hosts/:host/jobs` | `{ host, jobs }`: that host's scheduled jobs, `fleet --local jobs --json` (each with its latest run and live sessions, see [cli.md](cli.md#jobs)); a prompt file also gets `prompt.abs` — `~` expanded on that host — `prompt.root` — its git checkout, else its directory — and editor links built like the notes': `prompt.editorUrl` opens the file (at line 1, so a remote path opens as a file), `prompt.rootEditorUrl` its root; null without an editor or ssh alias); 501 when that host's CLI predates `fleet jobs` |
| GET | `/api/hosts/:host/jobs/:name` | one job with every kept run, newest first, each run's sessions with `parent`: `fleet --local jobs show <name> --json` + `host`; 404 no such job. Names: `[A-Za-z0-9_-]{1,40}` (else 400) |
| POST | `/api/hosts/:host/jobs/:name/run` | `{ force? }` → `{ host, name, output }`: `fleet --local jobs run [--force] <name>`. Starts an agent (spawn's trust level); 409 `{ error, busy: true }` while a session of the job is working, unless `force` |
| POST | `/api/hosts/:host/jobs/:name/enabled` | `{ enabled: boolean }` → `{ host, name, enabled, output, job }`: `fleet --local jobs enable\|disable <name>`, then the job as `show` reports it |
| POST | `/api/hosts/:host/repos/sync` · `/repos/service` | `{ names? }` → `fleet --local repos sync --json` (409 while one runs); `{ install }` → `fleet --local repos install-service [--uninstall]` |
| POST | `/api/hosts/:host/sessions/:id/kill` | `{}` → SIGTERM (then SIGKILL) Claude, then kill its tmux session (or just its window when the session has others) / close its iTerm tab |
| GET | `/api/groups` | the Board view's groups: `{ enabled, host, intervalMinutes, running, updatedAt, lastRun, groups: [{ id, label, description, source, members: [{ host, id, dormant? }] }] }` (`dormant: true` only on a member that is dormant on its host; never set from stored groups, so the UI also cross-references `GET …/dormant`); served by the grouping host, proxied by every other server (`enabled: false` when nobody runs it) |
| POST | `/api/groups/run` | `{}` → run the grouping pass now (on the grouping host) → the same shape; 501 when grouping is off |
| POST | `/api/groups/edit` | `{ op: "rename", id, label }` or `{ op: "move", host, session, to }` / `{ op: "move", host, session, label }` (a new group) / `{ op: "create", label }` / `{ op: "delete", id }` (empty groups) → `fleet group --rename/--move/--create/--delete` on the grouping host (proxied there) → the same shape; 400 bad body, 409 refused (unknown id, taken label), 501 when grouping is off |
| POST | `/api/hosts/:host/uploads?name=<file>` | raw file body → stored under that host's `web.uploads.dir` → `{ host, path, name, size }` (absolute `path`; 413 over `web.uploads.maxMB`); streamed through to a peer |
| POST | `/api/transcribe?hint=<words>` | raw audio body (webm / ogg / mp4 / mp3 / wav, ≤ 25 MB) → `{ text, model }` via OpenAI `gpt-4o-transcribe`; always this server, never proxied. `hint` (≤ 400 chars: session name, repo) steers spelling. Key: `$OPENAI_SPEECH_TO_TEXT_API_KEY`, else the macOS Keychain item of that name (never the config file). 501 without a key, 415 non-audio, 413 too large, 502 upstream errors (including a bad key), 429 rate limit |
| PUT | `/api/memos/recordings/:rec/segments/:n` | voice memos, always this server: one segment's raw audio (≤ 25 MB) is stored under `web.memos.audioDir`, then transcribed → `{ stored: true, transcribed, error? }` (a failed transcription keeps the audio; `finish` retries it). `:rec` is the client's recording id (`[a-z0-9-]`, 8–64), `:n` 0-based. 501 without a transcription key |
| POST | `/api/memos/recordings/:rec/finish` | `{ segments, startedAt, durationMs }` → transcribes what is missing, titles it (`claude -p`, else `Memo <date> <time>`), writes `web.memos.dir/<date>-<slug>.md` → the memo (below). Idempotent: again updates the same file. 409 a segment was never uploaded |
| GET | `/api/memos` | `{ dir, memos: [{ id, title, created, durationSec, status: done\|partial, words, preview }] }`, newest first (only files with a `recording` frontmatter key) |
| GET / PUT / DELETE | `/api/memos/:id` | one memo (`+ transcript, path`); PUT `{ title }` renames (the body is kept); DELETE removes the file and its audio |
| POST | `/api/memos/:id/retry` | transcribe the segments that failed and rewrite the memo |
| POST | `/api/speak` | `{ text }` (markdown is fine: code, tables, URLs and ids are dropped, at most ~4000 chars) → `audio/mpeg` via OpenAI `gpt-4o-mini-tts`; always this server. Key: `$OPENAI_TEXT_TO_SPEECH_API_KEY` (env, else Keychain), else the transcription key (which needs the audio scope). 400 nothing to say, 501 without a key, 502 upstream errors, 429 rate limit |
| GET/POST | `/api/operator` | the [voice operator](#voice-operator): `{ host, dir, session }` (`session`: its `fleet list` row, null when not running); POST finds it or starts it → `+ { started, tmuxSession }` (409 when a tmux session `operator` exists without it). Served by the first host of `web.operator.hosts` that answers (proxied with `?local=1`; an unreachable peer, or one without an operator, passes to the next); 503 when none does |
| POST | `/api/hosts/:host/autoname` | `{}` → run the naming pass on that host now → `{ renamed: [{ from, to }], tmux, held, errors }` |
| POST | `/api/hosts/:host/sessions/:id/files/stat` | `{ paths }` (≤ 200, as written in chat, `:line` allowed) → per path: resolved absolute path, `exists`, `isFile`, size, mtime, `kind` (markdown/text/image/pdf/other); the UI links only existing files |
| GET | `/api/hosts/:host/sessions/:id/files/raw?path=…[&download=1]` | the file itself, streamed (also through a peer); text/markdown over 5 MB only as a download |
| GET | `/api/hosts/:host/sessions/:id/complete?kind=skills\|files\|brain` | composer autocomplete, listed on the session's host (cached ~10 s): `skills` → `{ skills: [{ name, description, source: builtin\|user\|project\|plugin, argumentHint? }] }` (Claude Code's built-in commands, `~/.claude/skills` + `commands`, the cwd's and its ancestors' `.claude/skills` + `commands`, enabled plugins as `plugin:name`); `files` → `{ files, truncated }` (the `.md` / `.plain` files under the cwd: `git ls-files`, else a walk); `brain` → `{ available, notes: [{ ref, path, title? }] }` (the markdown under `brain_root:` in `~/.brainrc`; `ref` = file name, or path when two share it). 400 bad kind |
| POST | `/api/hosts/:host/sessions/:id/files/open` | `{ path }` → opens it with its default app **on that host** (`open` / `xdg-open`; runnable files are revealed in their folder instead) |
| GET | `/api/hosts/:host/sessions/:id/brief` | the session's [brief](#session-briefs): `{ host, id, exists, markdown, parsed: { summary, resources, todos, plan }, updated, editedAt, generatedAt, generatedThrough, generating, enabled, continuePrompt, absCwd, gitRoot, worktrees, editor, editorUrl }` — an empty skeleton (`exists: false`) before there is one; a gone session's brief is still served by its full id. `parsed.plan` is a **deprecated** alias of `parsed.todos` (the section was called Plan), kept for one release |
| GET | `/api/hosts/:host/notes/{tree,search,file,raw}` | the notes explorer (`web.notes`): the file list, `?q=` search with snippets + match ranges, `?path=` one note (frontmatter split, encrypted blocks withheld), `?path=` an image; 501 when the host has no `web.notes.root` |
| PUT | `/api/hosts/:host/sessions/:id/brief` | `{ markdown }` → a human edit (sets `editedAt`) → the same shape |
| GET | `/api/hosts/:host/stacks` | `fleet --local stack list --json` → `{ host, stacks: [StackView + editorUrl] }` ([session stacks](#session-stacks); `editorUrl` for `absCwd`, built like briefs' via `lib/editor.mjs`) |
| GET | `/api/hosts/:host/stacks/:id` | `fleet --local stack show <id> --json` → StackView (+ `editorUrl`); 404 when unknown |
| PUT | `/api/hosts/:host/stacks/:id` | `{ markdown, expectUpdated? }` → `fleet --local stack set <id> [--expect-updated] --json` (markdown on stdin) → StackView; **409** `{ error, updated }` on conflict; 400 bad body (markdown a string ≤ 64 kB) |
| POST | `/api/hosts/:host/stacks/:id/rename` | `{ label }` (one line, 1–80 chars) → `fleet --local stack rename <id> <label> --json` → StackView; 400 bad label; 404 unknown |
| DELETE | `/api/hosts/:host/stacks/:id` | `fleet --local stack rm <id> -f --json` → `{ removed }` |
| POST | `/api/hosts/:host/stacks/:id/spawn` | `{ prompt?, name?, model?, harness?, dir?, host? }` → a sibling in that stack: `dir` defaults to the stack's `absCwd`; otherwise as the session route below, without `ensure` |
| POST | `/api/hosts/:host/sessions/:id/stack/spawn` | `{ prompt?, name?, model?, harness?, dir?, host? }` → (1) `fleet --local stack ensure <session_id> --json` (timeout 150 s: it may call the model); (2) the server's own spawner (tmux, trust prompt handled) in the session's cwd (or `dir`, inside the session's cwd or a spawn dir) with `prompt` = `contextLine + ' ' + prompt`; (3) in the background, find the new session (by tmux session name, ≤ ~75 s) and `fleet --local stack add <stack id> <session_id> --json` → 200 `{ host, stack: StackView, created, generated, spawn: { name, dir, tmuxSession, command, trusted, model } }`. With `host` (a peer of `:host`, the stack's home): `dir` is required (a path there), the peer's `/spawn` starts it with `remoteContextLine + ' ' + prompt`, the join watches the peer's list and runs `stack add … --on <host> --input -` here; `spawn.host` names the peer |
| POST | `/api/hosts/:host/stacks/sync` | `fleet --local stack sync --input - --json` with the merged `/api/fleet` body on stdin (plain `sync` when it cannot be built) → `{ host, changed, stacks }` |
| GET | `/api/hosts/:host/dormant` | [session recovery](#session-recovery): `fleet --local restore --json` → `{ host, bootId, dormant: [DormantView], closed: [ClosedView] }` (`closed`: [recently closed](#recently-closed), from CLIs that know it); 501 when this host's CLI has no `restore` |
| POST | `/api/hosts/:host/dormant/restore` | `{ target, dryRun? }` (a dormant tmux name or session id, one line ≤ 200 chars) or `{ all: true, dryRun? }` → `fleet --local restore [--dry-run] (-- <target> \| --all) --json` → `{ host, restored, failed }`. Starts agents, like spawn: **409** `{ error, candidates }` when the target is ambiguous (exit 2), 404 no match (exit 3), 502 `{ error, restored: [], failed }` when nothing came back; a partial `--all` is 200 with `failed` filled. The fleet list (and a stack sync) refresh as the sessions register. Proxy timeout 6 min |
| POST | `/api/hosts/:host/dormant/forget` | `{ target }` or `{ all: true }` → `fleet --local restore (--forget=<target> \| --forget-all) --json` → `{ host, forgotten }`; 409 / 404 as restore |
| | | Both POSTs take `closed: true` for a [recently closed](#recently-closed) entry (→ `--closed`); `{ all: true, closed: true }` is a 400 on restore (closed ones are resumed one at a time), allowed on forget. A CLI without `--closed` → 501 |
| POST | `/api/hosts/:host/sessions/:id/brief/regenerate` | `{}` → **202** `{ host, id, started, queued, generating: true }`, the model call runs in the background (poll GET); **429** `{ error, retryAfterMs }` at the hourly cap |

`:host` is `self` or a configured peer; `:id` is a session id or a unique prefix (≥ 8 chars) — on
the `/stacks/:id` routes a stack id (or anything `fleet stack` resolves). The
full contract (status codes, limits, timeouts) lives with the server: [web/README.md](../web/README.md),
[web/ARCHITECTURE.md](../web/ARCHITECTURE.md).

## Voice operator

`#/operator` (web UI): talk to the fleet. The operator is a Claude session the web server starts
on request (POST `/api/operator`) in tmux session `operator`, in its own workspace
`${XDG_STATE_HOME:-~/.local/state}/fleet/operator/` — regenerated at every server start:

- `CLAUDE.md` — its brief: a dispatcher that answers by voice (short spoken sentences, the
  user's language), always looks before it answers, and acts only on an unambiguous target;
  closing or interrupting needs an explicit yes. Your own additions go in `CLAUDE.local.md`.
- `.claude/settings.json` — allows `fleet`, `./api`, `jq` and `date` without permission prompts
  (a prompt would stall a voice conversation); extend it in `.claude/settings.local.json`.
- `api` — `./api METHOD /path [JSON]`: this server's HTTP API (which reaches every host).

The screen records with `POST /api/transcribe`, sends the text with the ordinary `send` route,
polls `messages`, and reads each new final reply aloud with `POST /api/speak` — or, without a
speech key (or after it fails), the device's own voice (Web Speech API). Sessions that start
waiting on you are announced while the screen is open (from the fleet poll, no model call).
The operator is found by its cwd. Its row in `/api/fleet` carries `operator: true` and the UI
keeps it out of session lists (the CLI still shows it). `web.operator.hosts` picks one operator
for every device: each server sends `/api/operator` to the first host of the list that answers.

**Auto-clear**: every 5 minutes the server checks its operator; idle for `clearIdleMinutes`, it
gets `/clear`. Claude Code then starts a new session (a new session id, an empty transcript), so
the next conversation starts fresh and the app shows no history. A cleared session has no context
until it is used, which keeps it from being cleared again.

## Voice memos

`#/memos` (and the red record button on the phone's session list) records a conversation and
saves it as a titled transcript (`web.memos`). One recorder serves the whole app, so recording
continues while you look at other screens. The mic is cut into 5-minute segments — a fresh
`MediaRecorder` on the same stream, so each is a file the transcriber takes on its own — and every
finished segment is uploaded and transcribed while recording goes on; stopping only waits for the
last one, then `finish` titles and writes the memo. There is no length limit.

Nothing is lost on the way: audio chunks (5 s) go to IndexedDB as they are recorded and are
dropped only once the server has stored the segment. A failed upload is retried with backoff,
when the network comes back and when the app is shown again; a recording whose tab died (no
heartbeat for 30 s) is stopped where it ended and saved by the next app start. The screen is
kept awake while recording (Wake Lock) — a phone that locks or backgrounds the app may end the
mic, and what was recorded is saved.

## Smart grouping

The web Board view shows sessions as columns of work streams, and nobody maintains those groups:
`core::grouping` (the `fleet group` verb) does, with one source of truth for the whole fleet.

- **Where it runs.** One host: `grouping.host`, or — without it — the host whose server has
  `web.grouping.enabled`. That server runs `fleet group --input - --apply --json` every
  `web.grouping.intervalMinutes` (default 10, first run 20s after start) with its merged
  `/api/fleet` body on stdin — so it needs no ssh to its peers, only their web servers — and early
  (≥ 2 min after the last run) when the warm snapshot shows live sessions no group knows. Every
  other server proxies `/api/groups` to it (and finds it by asking peers when `grouping.host` is
  unset). The state lives on that host (`~/.local/state/fleet/groups.json`).
- **Input per session**: display name (generated title, else name), name, the last two cwd
  segments (worktree paths carry the branch-ish name; the git branch itself is not read — a
  remote session's branch isn't cheaply known), host, the first prompt's first 160 characters.
  Prompts are capped at 12k characters per call; overflow goes to another call, at most 4 per run.
- **Stability.** Assignments persist per `host/sessionId` with a fingerprint of those inputs; an
  unchanged session is never re-sent. New or changed sessions are classified into the existing
  groups (the model sees their labels and a few member names and is told never to rename them) or
  a new group; a new label equal to an existing one reuses that group. Consolidation — merge
  clearly identical streams, fix a clearly wrong label — runs at most hourly, only after changes,
  and is capped at 2 merges + 2 renames per pass. Group ids never change; empty groups vanish;
  sessions of a host that didn't answer keep their group.
- **Dormant sessions** (left by a reboot, see [Session recovery](#session-recovery)) count as
  present: they keep their assignment (and so their group), are never sent to the model, and come
  back with `"dormant": true` on their member object in a pass's report (`fleet group --json`, and
  so `/api/groups`; absent otherwise, never set by `--cached`). The pass reads this machine's
  snapshot and, for each other answering host, `fleet -H <host> restore --json` — in parallel with
  the session fetch. A host whose dormant list can't be read is treated like a host that didn't
  answer: none of its assignments are pruned that run (a fleet too old to have `restore` has none).
- **Hand edits win.** Renaming a group on the Board locks its label (never renamed, never merged
  away); a session dragged to another group is a `manual` assignment the passes never touch.
  A group made on the Board (`manual`) stays even when empty, until the user deletes it.
  Column order is the UI's (`fleet.boardOrder`, per browser), not part of the state.
- **Fallback.** Model disabled, missing, logged out, or answering garbage (one retry on an
  unparseable answer, none on a failed call): group by repository, marked `fallback`, reclassified
  once the model is back. The UI does the same grouping client-side when no server runs grouping.
- **Cost.** A run with nothing new makes no model call; a typical run after one new session makes
  one (haiku, ~2–8k prompt characters, 10–45s wall clock with `claude -p` start-up).

## Session briefs

A brief is a small markdown doc per session: 1–2 sentences on exactly what the session is doing and
where (repo/cwd, branch, host), every resource it produced, and its todos with progress — enough
to start a **new** session from it and just continue (`continuePrompt` in the API is that first
prompt). The user can edit it; their edits are authoritative.

**Storage.** One file per session on the host the session lives on:
`$FLEET_BRIEFS_DIR/<session_id>.md`, else `${XDG_STATE_HOME:-~/.local/state}/fleet/briefs/<session_id>.md`
(dir `0700`, files `0600`), always written atomically (temp file + rename). The web server owns
generation (`web/lib/briefs.mjs`); any other reader/writer — `fleet brief` (`core::brief`, a port
of `web/lib/brief-format.mjs` and the PUT rules) — uses the same files and must follow the same
format and merge rules. Both implementations pin the shared fixtures in `testdata/briefs/`
(regenerate the expected outputs from the server code with `node testdata/briefs/gen.mjs`).

**Format** (a contract, like the JSON outputs — add keys freely, never rename or retype one):

```markdown
---
session: <session_id>
host: laptop
cwd: ~/Code/project
updated: 2026-01-02T03:04:05.000Z
generatedThrough: 48213
generatedAt: 2026-01-02T03:04:05.000Z
editedAt: 2026-01-02T03:10:00.000Z
todos: 3f2a9c01b7de
dismissed: ["https://github.com/owner/repo/pull/9"]
git: Git: `fix-login` · worktree `~/Code/project-wt`
---
## Summary
Fixing the login redirect loop in ~/Code/project on branch fix-login; the fix is in review.

## Resources
- Git: `fix-login` · worktree `~/Code/project-wt`
- PR: [owner/repo#12](https://github.com/owner/repo/pull/12)
- File: `src/login.ts`
- Spec: `specs/login/SPEC.md`
- Artifact: [Login report](https://claude.ai/code/artifact/…)
- Link: [docs.example.dev/auth](https://docs.example.dev/auth)

## Todos
- [x] reproduce the loop
- [ ] fix the redirect (in progress)
```

- Frontmatter: `key: value` lines between `---` fences. Numbers are bare, arrays/objects are
  one-line JSON, a string that would read back as another type is a JSON string. Keys:
  `session`, `host`, `cwd` (informational, written by the server); `updated` (last write of any
  kind, ISO 8601); **`generatedThrough`** — the **byte offset** into the session's transcript JSONL
  (`~/.claude/projects/<encoded cwd>/<session_id>.jsonl`) up to which the conversation has been
  summarised, always at a line boundary (a transcript smaller than it was replaced: start over);
  `generatedAt` (last model generation); `editedAt` (last human edit, absent until one);
  `todos` (hash of the todo list last copied into Todos); `dismissed` (resource keys a human
  deleted — never added back; at most 200); `git` (the auto `Git:` line last written; its
  presence also means the legacy `Branch:` / `Worktree:` lines were migrated). Unknown keys are
  preserved.
- Body: exactly three `## ` sections, written in this order — `Summary`, `Resources`, `Todos`.
  Parsers are tolerant: headings case-insensitive, sections in any order or missing, text before
  the first heading and other `## ` sections are kept (written after Todos), `*`/`+` bullets,
  `[X]`. A `## Plan` section (the name before Todos) is read as `## Todos` and written back as
  `## Todos` on the next write.
- Resources: one bullet per item, `- <Kind>: <value>`, value a markdown link `[label](url)` or a
  code span `` `path` ``. Kinds: `PR`, `Issue`, `Artifact`, `Spec`, `File`, `Git`, `Link` (and the
  legacy `Branch`, `Worktree`). Any other bullet (no kind, free text) is a hand-written line and
  kept as is. An item's **key** is its URL (fragment and trailing `/` dropped), else its path
  (`branch:<name>` / `worktree:<path>` for the legacy kinds); every `Git` line has the key `git`.
  Paths are relative to the session's cwd when inside it, else `~/…`, else absolute.
- The **Git line** — one per brief, where the session's checkout is: ``- Git: `<branch>` · worktree
  `<root>` `` for a linked worktree, `` · repo `<root>` `` for the main checkout (`<root>` = the
  checkout root, `~/…`; `detached` in place of the branch). A Git line in exactly this form is the
  auto one and is **replaced in place** when the branch or root changes; with no Git line at all
  one is inserted at the top of Resources unless `git` is in `dismissed` (a human deleted it). A
  Git line in any other form (e.g. with a note appended) is a human's and left alone. Migration:
  before the `git` frontmatter key exists, the server's old auto lines — exactly
  `` - Branch: `<name>` `` / `` - Worktree: `<path>` `` — are dropped on the next write and the Git
  line takes the first one's place; hand-written variants (other bullets, extra text) stay.
- Todos: `- [ ] todo` / `- [x] todo` lines; other lines in the section are kept.

**Merge rules** (every writer): never drop a line a human wrote; new auto items are appended
unless their key is already present or in `dismissed` (the Git line: replaced in place, above); a
human edit (PUT) that removes a resource line adds its key to `dismissed`; the machine keys
(`generatedThrough`, `generatedAt`, `todos`, `dismissed`, `git`) are the writer's, not taken from
an edited body.

**Generation** (hybrid, `web/lib/brief-extract.mjs` + `web/lib/briefs.mjs`):

1. *Resources without a model*, read incrementally from the transcript: files the session wrote
   (Edit / Write / MultiEdit / NotebookEdit targets, shell redirects and `tee`; temp dirs skipped;
   `SPEC.md`, `FINAL.md` and files under `specs/` are `Spec`), PR/issue URLs printed by
   `gh pr|issue create`, artifact URLs returned by an Artifact publish, PR/issue/artifact/other
   links in the assistant's text, PR/issue/artifact links in the user's prompts; the Git line
   (branch, checkout root, linked worktree or not) from one `git rev-parse` in the cwd. Links to this machine or the private network (IP
   literals, dotless or `.local`/`.ts.net` hosts), schema hosts, templated or `…`-truncated URLs are
   dropped. Read-only tool output (a file that lists PRs) never counts.
2. *Todos without a model* when the session keeps todos: the latest `TodoWrite` list, or the task
   list from `TaskCreate`/`TaskUpdate`, is the Todos section (rewritten only when the list
   changes, so a hand edit stands until the next todo change).
3. *Summary* (and the Todos when the session keeps none) from `claude -p --model <model>` — flags as in
   `core::naming` (prompt on stdin, `--strict-mcp-config`, tools disallowed, a neutral cwd) plus
   `--no-session-persistence`, 2 min timeout. Input: the current Summary + Todos (≤ `maxBriefChars`)
   and ONLY the conversation since `generatedThrough` — user prompts and turn-ending assistant
   text, each clipped, newest kept, ≤ `maxDeltaChars` — told that hand-edited content is
   authoritative and to update, not rewrite. The answer must be a `## Summary` (≤ 1200 chars) and
   optionally a `## Todos` of checkboxes (`## Plan` is accepted too); anything else keeps the old brief. An edit that lands
   while the model runs wins (the answer is discarded).

**Budget** — model calls are what costs, so every automatic one has to pass all of:

- the session is `idle` or `waiting`, and has been for `idleMs` (registry `updated_at`);
- its transcript grew past `generatedThrough` by ≥ `minNewTurns` user prompts or ≥
  `minNewChars` characters of conversation;
- ≥ `minIntervalMs` since that session's last call (a gated update stays pending and runs when the
  gate opens, without needing more growth);
- no other brief call is running on this host (one at a time), and fewer than
  `maxCallsPerHour` calls in the last hour.

The background pass checks this host's live sessions every 30 s (the 2 s discovery cache — no
extra `fleet list` while a UI is polling) and forgets sessions that are gone. Resource and todo
extraction (no model) runs whenever the transcript grew — in the background pass and on GET. A
manual regenerate skips the idle, interval and new-content gates (with nothing new it re-reads the
recent conversation) but waits for the one-at-a-time slot and counts against the hourly cap. Each
call is logged with its input size (`[briefs] idle 1a2b3c4d: claude -p --model haiku, 12 msg(s) /
3 user turn(s), 8123 chars in (4/12 this hour)`); `/api/health` reports `briefs: { enabled, model,
callsLastHour, maxCallsPerHour, generating, lastRun }`.

**Open in editor.** GET/PUT brief bodies carry `absCwd` (the session's directory on its host,
absolute, no `~`), `gitRoot` (the checkout root — repo or linked worktree — containing it, absolute;
null outside git), `editor` (`web.editor` of the answering server) and `editorUrl`; session rows in
`/api/fleet` carry `editorUrl` for their cwd. `worktrees` lists the git checkouts the session works
in — `[{ path, display, branch, linked, editorUrl }]`, the cwd's first, then the checkouts of the
directories the transcript records the session in (its entries' `cwd`, kept in the brief's `dirs`
frontmatter key) and of the File / Spec resources; a directory's checkout is the nearest ancestor
with a `.git`, deduped by root — each with its own `editorUrl`. The link targets `gitRoot`, else `absCwd`:
`vscode://file/<path>` when the session is on the machine the browser runs on, else
`vscode://vscode-remote/ssh-remote+<hosts.<host>.ssh><path>` (Remote-SSH with **that** server's ssh
alias for the host). The browser's machine is read from the request: a loopback address is the
answering server's own host, an address equal to a host's `web` url hostname is that host, anything
else (a phone) is none of them. For a session on the answering server's own host seen from another
machine the ssh destination is `web.editorSsh`, else `<server user>@<the hostname the browser
used>`. `cursor://…` alike for `web.editor: "cursor"`; `null` when `web.editor` is null,
there is no absolute path, or the host has no usable `ssh` alias. A proxied request is answered by
the peer (which knows only its own absolute paths) and the server that received it fills in
`editor` / `editorUrl` from its own config and the request's address.
`fleet brief <target> --open` does the same from the CLI: `code <path>` here, `code --remote
ssh-remote+<ssh dest> <path>` for a session on another host (`cursor` with `web.editor: "cursor"`).

## Session stacks

A **stack** is N Claude Code sessions — on one host or across hosts — sharing one context
layer: a markdown file, the **StackBrief**. Any session can spawn a *sibling* (`fleet stack
spawn`, the web UI's "Spawn sibling"), on its own host or on another one (`--on <host>`, the
form's Host choice); the sibling joins the source's stack, which is created around the source
first when it has none. Every session spawned into a stack starts with the **context line**:

```
You're running in the session stack with shared context: <abs path to the stack file>. <prompt>
```

(nothing after the period when the prompt is empty; `core::stack::context_line` /
`stack_prompt`, and `contextLine` in every `--json` output so the web server prepends the same
text). A sibling on **another host** than the stack's home cannot read that path, so it gets the
**remote context line** instead (`core::stack::remote_context_line`, `remoteContextLine` in the
StackView):

```
You're running in the session stack with shared context: stack <id> on host <home>. Read it with `fleet -H <home> stack show <id>` and update it with `fleet -H <home> stack set <id>` (the whole markdown on stdin; `fleet -H <home> stack show <id> --json` has it under `markdown`). <prompt>
```

`fleet -H <home> stack show` / `set` reach the home over ssh when there is a destination for it,
else over its web server (`hosts.<home>.web`, the stack routes below — the sheds have web urls
for each other and no ssh). The StackBrief is generated once, at creation; after that it is
maintained by scripts (membership) and by hand. Logic: `core::stack`; CLI: `fleet stack`
([cli.md](cli.md#stacks)); the web server only shells out to `fleet --local stack … --json` and
never parses the markdown.

**Storage.** `$FLEET_STACKS_DIR`, else `${XDG_STATE_HOME:-~/.local/state}/fleet/stacks/`, one
`<stack_id>.md` per stack (dir `0700`, files `0600`, atomic temp + rename writes). The file is
the state — no index; "which stack is session X in" scans the dir. Stack id: `st-` + 8 random
lowercase hex, never changes. A stack lives on the host where its file is — its **home**
(frontmatter `host`) — and every `fleet stack` command about it runs there (`-H <home>` from
elsewhere). Its members may run on any host: `members[].host` (a member without one is on the
home). Only the home host knows the stack: `fleet list` on another host does not stamp its
members' rows (the web server does, from the home's `/api/fleet` entry — see below), and
`fleet stack list` there does not show it.

**Format** (a contract: add keys freely, never rename or retype one):

```markdown
---
stack: st-1a2b3c4d
label: Login redirect fix
host: laptop
cwd: ~/Code/project
created: 2026-09-29T10:00:00.000Z
updated: 2026-09-29T12:00:00.000Z
generatedAt: 2026-09-29T10:00:05.000Z
editedAt: 2026-09-29T11:00:00.000Z
members: [{"session":"<uuid>","host":"laptop","name":"login-redirect","added":"2026-09-29T10:00:00.000Z","closed":null,"cwd":"~/Code/project","firstPrompt":"Fix the login redirect loop"}]
---
> Shared context for the session stack **Login redirect fix** (`st-1a2b3c4d`). Every session in
> this stack reads this file when it starts. Keep **Summary** and **Resources** current for your
> siblings (PRs, worktrees, folders, decisions). **Sessions** is maintained by `fleet stack` — do
> not edit it; read a sibling's brief or transcript from there when you need to know what it did.

## Summary
2–4 sentences on the stack as a whole: what, and where (repo, branch/worktree, host).

## Resources
- Git: `fix-login` · worktree `~/Code/project-wt`
- PR: [owner/repo#12](https://github.com/owner/repo/pull/12)
- Folder: `~/Code/project`

## Sessions
- **login-redirect** (`1a2b3c4d`, live) — added 2026-09-29 10:00 · brief `~/.local/state/fleet/briefs/<uuid>.md` · transcript `~/.claude/projects/<encoded cwd>/<uuid>.jsonl`
  first prompt: "Fix the login redirect loop"

## Notes
free text; any other `## ` section is kept as is
```

- Frontmatter: the briefs' encoding (`key: value`, arrays/objects one-line JSON). Machine keys —
  `stack`, `host`, `cwd`, `created`, `updated` (last write of any kind), `generatedAt` (the model
  wrote Summary/Resources), `editedAt` (last human edit), `members` — are never taken from an
  edited body; `label` is (from the submitted frontmatter). Unknown keys are preserved.
- `members[]`: `{ session, host, name, added, closed, cwd?, firstPrompt? }` — `host` is where
  the session runs (the stack's home when absent), `name` is the
  display title at the last sync, `closed` `null` while live and an ISO time once the session was
  seen gone (members are never removed automatically; `fleet stack remove` does), `cwd` (`~/…`,
  or absolute on another host) and `firstPrompt` (≤ 160 chars) are kept so the Sessions line
  survives the session. Unknown member keys are preserved.
- Body: the header blockquote and `## Sessions` are **regenerated** on every machine write
  (Sessions from `members`: `live` / `closed <date>`, `, on <host>` for a member on another host
  than the home (its paths are paths there), the brief path whether or not it exists, the
  transcript path for the member's cwd, `first prompt:` when known; dates UTC). `Summary`,
  `Resources`, `Notes` and any other section, and text above the first heading, are a human's
  and kept verbatim. Parsing is tolerant like briefs (case-insensitive headings, any order,
  missing sections); Resources bullets parse like a brief's (`parse_resource_line`; `Folder:`
  lines get the kind `Folder`).
- Human edit (`fleet stack set` / `edit`, web PUT): the submitted body is authoritative except
  for the header and Sessions; machine keys are the stored file's; `editedAt` + `updated`
  stamped; `--expect-updated` / `expectUpdated` refuses a stale edit (CLI exit 3, HTTP 409). At
  most 64 kB.

**Membership sync** (`core::stack::sync`, no model): for every stack file on this host, a member
whose session id is not among the live sessions *of its host* gets `closed = now` (once); a live
one gets its `name` refreshed from `display_title` (and `closed` cleared — a resumed session).
A member is judged only against the rows of its own host (`core::stack::Observed`): plain
`fleet stack sync` knows this machine's sessions, so members on other hosts are left as they
were; `--input <src>` (`-` = stdin; a `list --json` array or a `/api/fleet` body) adds the other
hosts' live rows, and a host with `ok: false` there is still not judged. The web server passes
its merged fleet that way, so a sibling that ended on another host is closed within a sync
interval (and re-opened if it comes back). Only files whose members changed are rewritten. It
runs in `fleet stack list` / `show` / `sync` and after `spawn` / `add` / `remove`; the web server
runs `stack sync` after a kill, once 30 s after start, and every `web.stacks.syncMinutes` while
any session is in a stack or it knows a stack. Nothing is marked closed when the session
registry can't be read, and a member dormant on this machine (left by a reboot,
`snapshot::dormant_session_ids`) is not marked closed either — it is left as it was. StackView
members carry `host`, `dormant: true|false` next to `live` (dormant = not live and in this
machine's dormant set); for a member on a host not observed, `live` is `closed == null`,
`status` is `null` and `briefPath` is `null` (the brief is on that host). `fleet list` only
stamps `stack: { id, label, host } | null` on each row (read-only; `host` = the stack's home).

**Generation** (creation only; `stacks.model`, default `sonnet`): one `core::naming::ask_claude`
call (`-p --model <m> --strict-mcp-config`, tools disallowed, prompt on stdin, neutral cwd, 120 s)
with the source session's display title, cwd, host, its Git line (`git rev-parse` in the cwd),
first prompt, its session brief's body when there is one, and the last ~6000 characters of its
conversation (user prompts and turn-ending assistant text, main thread, tool blocks skipped).
The answer must be `Label: <2–5 words>`, `## Summary` (≤ 1000 chars, generalised to the stack)
and `## Resources` (`- Kind: value` bullets of things really in the input); the Git line and a
Folder line are added when missing. Anything unparseable, a failed call, `--no-llm`,
`FLEET_FIXTURE` or `stacks.enabled: false` → the skeleton (Summary "Started from <title> in
<cwd>.", Resources = Git line + Folder): a model failure never blocks creating the stack
(`generated: false` and a `warning`). There is no regeneration after creation (v1).

## Session recovery

After a reboot fleet shows what was running as **dormant** and brings it back on demand: the same
tmux session name, windows, panes and cwds, every Claude pane re-launched with
`<launcher> <flags> --resume <sessionId>`. The session id is unchanged by `--resume` (only
`--fork-session` mints a new one), so groups, stacks and briefs — all keyed by session id — carry
over. Code: `core::snapshot` (no printing); CLI: `fleet restore` and `fleet enter`
([cli.md](cli.md#session-recovery)).

### `snapshot.json`

One per machine: `$FLEET_SNAPSHOT`, else `${XDG_STATE_HOME:-~/.local/state}/fleet/snapshot.json`
(dir `0700`, file `0600`, written atomically under a `snapshot.json.lock` file lock). Unknown keys
are preserved at every level on rewrite — it is a contract like `groups.json`.

```json
{
  "version": 1, "host": "laptop", "bootId": "1727000000", "updatedAt": "2026-10-04T12:00:00Z",
  "tmux":  [ TmuxSnap… ],
  "iterm": [ ClaudeSnap… ],
  "dormant": { "tmux": [ TmuxSnap… ], "iterm": [ ClaudeSnap… ] },
  "closed":  { "tmux": [ TmuxSnap… ], "claude": [ ClaudeSnap… ] }
}
TmuxSnap   = { name, windows: [ { index, name, layout, active, autoName,
                                  panes: [ { index, cwd, active, claude: ClaudeSnap|null } ] } ],
               since?, goneAt?, closedAt? }
ClaudeSnap = { sessionId, name, cwd, title, flags: [argv…], since?, goneAt?, closedAt?, tmuxSession? }
```

- `tmux` — the live tmux sessions of the current boot; `iterm` — live Claude sessions that are
  not in a tmux pane fleet can see (iTerm, unknown). `dormant` — what earlier boots ran.
  `closed` — what ended within a boot ([Recently closed](#recently-closed)); added later, so a
  file without it is an empty list. A session id is never both dormant and closed.
- `layout` is `#{window_layout}`; `autoName` = tmux named the window itself (a restore then
  leaves the name to tmux). `title` is the display title when recorded. `flags` is the
  replay-safe part of the Claude command line (`--dangerously-skip-permissions`, `--chrome`,
  `--model`, `--permission-mode`, `--add-dir`, `--agent`, `--fallback-model`; `--k=v` is
  normalised to `--k v`).
- `since` (dormant entries) — when it went down: the old boot's last `updatedAt`. An unchanged
  snapshot is still rewritten every 10 minutes so this stays accurate.
- `goneAt` (live entries) — it vanished this long ago; after 5 minutes it moves into `closed`. A
  restart quits the terminal apps (and the Claude sessions in them) before it kills the daemons,
  and a poll in between must not erase them; a Claude session gone from a pane that is still there
  lingers in that pane, one whose pane is gone lingers in `iterm` (with `tmuxSession`).
- `closedAt` (closed entries) — when it ended (its `goneAt`). `tmuxSession` — the tmux session a
  Claude session ran in, kept when it left its pane (`/exit`, the shell stayed).
- `bootId` — `$FLEET_BOOT_ID` (tests), else Linux `/proc/sys/kernel/random/boot_id`, else macOS
  `kern.boottime` seconds. Unknown → nothing is ever marked dormant.

**Recording** happens on every local `fleet list` with a successful discovery (one `tmux
list-panes -a`, one `ps` for the Claude pids), never in fixture mode, never when tmux fails for a
reason other than "no server", and only writes when something changed. Rules: a stored boot id
different from the current one moves the stored `tmux`/`iterm` into `dormant` (merged by tmux name
/ session id); then `tmux`/`iterm` become the live state; a dormant Claude session that is live
again (resumed by hand) leaves `dormant` (inside a dormant tmux session its pane becomes a plain
shell), and a dormant tmux session left with no dormant Claude pane whose name is live again is
dropped. Readers that do not record (the `watch` header, `fleet tmux list`, the `list` footer)
apply a pending reboot in memory; `fleet restore` and the dormant lookup of `fleet enter` record
first, so the first command after a reboot already sees the old boot as dormant.

**For other views** (groups, stacks, the web Board): `core::snapshot::dormant_session_ids()` is the
set of this machine's dormant session ids; for another host, `fleet -H <host> restore --json` and
the `sessionId`s under `dormant[].sessions`. A dormant session counts as *present*, not gone:
grouping keeps its assignment and flags the member `dormant: true` ([Smart
grouping](#smart-grouping)); stack sync doesn't close it and StackView members carry `dormant`.

### Recently closed

A session that ends *within* a boot — Close in the app, `fleet kill`, `/exit`, a killed tmux
session — is **closed**, deliberately separate from dormant: it is never part of `restore --all`,
`restore.onBoot`, `fleet enter`'s auto-restore, `dormant_session_ids()` or the groups'/stacks'
dormant sets, so a closed session's group and stack membership lapses as before. It can still be
brought back by hand for `restore.keepClosedDays` (default 7; `0` = off), the newest 50 at most.

- It shows as closed at once: `core::snapshot::closed_entries` is the stored `closed` lists plus
  whatever still lingers with `goneAt`. When the grace period ends it moves into `closed`
  (`closedAt` = its `goneAt`); a reboot within the grace period makes it dormant instead (and it
  leaves the closed list). Older closed entries stay closed across a reboot (expiry still applies).
- Whole tmux sessions are kept only when they had a Claude pane; plain shell sessions are not. A
  Claude session that exits while its tmux pane lives on is a lone closed entry with
  `tmuxSession`.
- A closed (or dormant) Claude session whose id is live again leaves the list; a closed tmux
  session left with no Claude pane goes. A newer closed tmux session of the same name replaces
  the older one, whose Claude sessions stay as lone entries.
- Restore (`fleet restore --closed <target>`) is the dormant restore: a tmux session with its
  layout (non-Claude panes a plain shell), a lone Claude session in a new tmux session named
  after its title — except one with a `tmuxSession` that still exists, which gets a new window
  (`new-window -d`) there running the resume line. Same launcher, flags, name-clash handling,
  dry run, file lock and exit codes (2 ambiguous, 3 no match).

## Extension points

- **Another web UI.** The HTTP API is the contract; point `web.ui` (or `FLEET_WEB_UI`) at a
  directory with its own `index.html` and it is served same-origin with `/api/*`, or build a
  separate app against the same API.
- **A different TUI or native app.** Link the core library (`fleet::core`) directly, or shell out
  to `fleet list --json` and the other commands.
- **Integrations** (notifications, chat bots, status bars): poll `fleet list --all-hosts --json`
  or `GET /api/fleet`; act through `fleet send` / `POST …/send`. Treat anything that can send as
  having the agent's full permissions.
- **New backends** (another terminal emulator): implement discovery matching (tty → handle) and
  the control operations in `core::backend`; everything above picks it up.
- **The Claude Code skill** (`fleet skill install`) teaches an agent to use `fleet` itself —
  e.g. to hand work off to another session.
