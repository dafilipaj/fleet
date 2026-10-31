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
- **input** — reads a session's input box (Claude Code's prompt, Codex's composer, pi's editor)
  off a capture that keeps the text's attributes (tmux `capture-pane -e`): `send`, `rename` and
  `close` never type after text already there or into a prompt, a faint placeholder or
  suggestion aside ([cli.md](cli.md#sending)). It also reads Claude Code's working line over
  its box (`✳ Actualizing…`), which `model` and `effort` take as Busy.
- **slash** — what `fleet model` and `fleet effort` share of Claude Code's switching slash
  commands: the warm-cache confirmation read by `answer`'s reader and checked to be that
  command's for that value, work under way or a passing tmux hint banner where the box was
  (waited for, not taken for another screen), the command's output read off the transcript's
  new lines (`<local-command-stdout>` in a `user` entry only under the entry naming that very
  command, chained by `parentUuid` / `promptId`, or in a `system` / `local_command` entry). The
  CLI side (`cli::switch`) holds the checks before typing, the lock and the wait both commands
  run, and reads the transcript once more before calling a screen `confirm_unrecognized`.
- **effort** — Claude Code's `/effort <level>` as `fleet effort` reads it: "Change effort
  level?" for that level, and the answers 2.1.308 writes as `user` entries (`Set effort level
  to <level> (saved as your default for new sessions)` / `(this session only)`, `Effort level
  set to auto`, `Kept effort level as <level>`) ([cli.md](cli.md#switching-the-effort)).
- **model** — Claude Code's `/model <id>` as `fleet model` reads it: its switch confirmation
  (the warm-cache one, for that model, told from any other screen), `Model '…' not found` and a
  `PreModelSwitch` hook's refusal on screen, work still under way where the box was ("Running
  PreModelSwitch hooks…"), the command's output it writes into the transcript (`Set model to
  <label> …` as a `user` entry in 2.1.306, taken only under the entry naming that `/model
  <id>` since a prompt typed as that output is written alike; `Kept model as <label>` and the
  rest as `system` / `local_command` entries; either shape read), and whether a label stands for the
  id asked for, or for another model (by `harness::claude_models`, aliases resolved through
  it, else the rule Claude Code names ids by) — for a switch's label and the confirmation's
  alike ([cli.md](cli.md#switching-the-model)).
- **naming** — generates short kebab-case titles by asking `claude -p --model haiku` what a
  session is working on (falls back to git branch / first prompt); cached across runs.
- **tmux** — tmux-*session* management inherited from `ws`: matching, sanitizing, stale detection.
- **trust_prompt** / **trust** — `fleet trust`, the folder-trust prompt of a tmux Session host
  before its harness registers. `trust_prompt` reads the visible screen only, with the web
  spawner's signals (`❯ Yes, I trust`, `› 1. Trust and continue`), `answer`'s live-prompt
  rule (nothing else on screen) and what Claude Code 2.1.311 was recorded doing (only blank rows
  under its footer, the terminal's cursor on the selected `❯`; a dialog left above the shell
  after Esc is no prompt): the dialog's lines, answers, cursor and FNV-1a id. `trust` binds the exact tmux session and pane, holds
  `trust-<name>.lock`, re-reads before every key and verifies the outcome; `--decline --close`
  kills the session with `tmux::kill_if_alone` once the harness has exited. No Session
  registry, transcript or harness trust record is consulted. `spawn --resume`'s
  `session_starting` names the pending start's Session host (`lock::resume_host`)
  ([cli.md](cli.md#folder-trust-before-a-session-registers)).
- **config** — reads and patches the shared config file.
- **hosts** — resolves the target host and dispatches over ssh.
- **tools** — binary lookup (`PATH` plus Homebrew fallbacks), `~` expansion.
- **brief** / **stack** — the [session brief](#session-briefs) and [session stack](#session-stacks)
  files: format, merge rules, storage.
- **repos** / **prs** — [`fleet repos`](cli.md#repos) (keep checkouts fresh) and
  [`fleet prs`](cli.md#pull-requests) (the GitHub PRs that need you, through `gh`, joined with
  those checkouts and their worktrees).
- **tasks** — [`fleet tasks`](cli.md#productive-tasks): your open Productive tasks over its REST
  API (`curl`), by column, cached, with each workflow's statuses; moving a task between statuses.
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
then `~/.local/bin/fleet`, then `PATH` (with `FLEET_CMD`/`FLEET_JOB_RUN`/`FLEET_GH_IDENTITY`/`FLEET_DEBUG`/`FLEET_DRY_RUN` exported, never
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
- drives its own backends directly for peek/send/keys (it needs untruncated, header-free output —
  which the CLI now also gives, as `fleet peek --json` and `fleet keys`);
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
| `hosts.<name>.editorSsh` | ssh destination the browser's machine uses for that host in "Open in editor" links, when it differs from `ssh` or `ssh` is unset (a web-only peer); default `ssh` |
| `web.port` / `web.bind` | web server listen address; with no config at all it binds `127.0.0.1` only |
| `web.dir` | where the web app lives (repo checkout or install dir), detected by `init` |
| `web.node` | `node` binary that runs the web app; `null` → `/opt/homebrew/bin/node`, `/usr/local/bin/node`, then `PATH` |
| `web.editor` | `"vscode"` (default) \| `"cursor"` \| `null`: the scheme of the "Open in editor" link the API puts on briefs and session rows (`editorUrl`, see [Session briefs](#session-briefs)); `null` = no link (the UI hides the button) |
| `web.editorSsh` | ssh destination other machines use for **this** host in those links (e.g. the alias in the laptop's `~/.ssh/config`); default `<user>@<the hostname the browser used>` |
| `web.ui` | static UI directory to serve instead of the bundled one; unset → `web/ui/dist`. The old `"classic"` value (the removed vanilla UI) is ignored |
| `web.quickReplies` | composer chips: strings or `{ label, text }` |
| `web.models` | New session model picker: `[{ id, label }]` or bare ids (`""` = Claude's default, no `--model`); default Default, Fable 5.1, Opus 5.5, Sonnet 5, Haiku 4.5 |
| `web.autoName` | `{ enabled, intervalMinutes }` (default off, 5 — opt in with `enabled: true`): the web server runs `fleet name --all --settled --apply` on its host on that schedule (a session is named once it has a second prompt or a brief) (tmux names follow the titles, see [Session titles](#session-titles)); a web spawn with a first prompt also gets a targeted `fleet name <id> --apply` once it has replied |
| `web.grouping` | `{ enabled, intervalMinutes }` (default off, 10): this host's web server runs `fleet group` over the whole fleet on that schedule and serves `/api/groups` (see [Smart grouping](#smart-grouping)) |
| `web.uploads` | `{ dir, maxMB, retentionDays }` (default `~/.local/share/fleet/uploads`, 100, 14): files dropped / pasted / picked in the web UI are stored there on the session's host as `YYYY-MM-DD/<rand>-<name>`, and their absolute path goes into the prompt; day dirs older than `retentionDays` are removed (`0` keeps them) |
| `web.briefs` | `{ enabled, model, idleMs, minIntervalMs, maxDeltaChars, maxCallsPerHour, minNewTurns, minNewChars, maxBriefChars }` (default off, `haiku`, 60 s, 15 min, 12000, 12, 2, 2000, 3000): background generation of [session briefs](#session-briefs) on this host; reading and editing briefs (and a manual regenerate) work either way |
| `web.stacks` | `{ syncMinutes }` (default 2): how often this host's web server runs `fleet stack sync` while any of its sessions is in a [stack](#session-stacks) |
| `web.memos` | `{ dir, audioDir, model, vocabulary }` (default `~/.local/share/fleet/memos`, `~/.local/share/fleet/memo-audio`, `haiku`, `[]`): voice memos (`#/memos`). Each memo is `dir/<date>-<slug>.md` (frontmatter `title`, `created`, `duration`, `recording`, `status`; the transcript as the body), so `dir` can be a folder in a notes repo; segment audio stays in `audioDir`. `model` titles them via `claude -p`; `vocabulary` (names, projects, tools — up to ~1500 chars) steers the spelling of every segment's transcription. Needs the transcription key |
| `web.notes` | `{ root, name?, searchCmd?, exclude? }` (default off): the web UI's notes explorer (`#/notes`) over the markdown notes under `root` on this host — browse, full-text search (built in, or an external `searchCmd` such as `rg -n -i -F {query}`), preview with `[[wiki]]` / relative links between notes; hidden entries, `node_modules`, `.gitignore`d paths and age-encrypted blocks are never served (web/ARCHITECTURE.md → notes). Peers browse each other's notes through the usual proxy |
| `web.operator` | `{ hosts?, model?, voice?, clearIdleMinutes? }` (default `[self]`, Claude's default model, `alloy`, 60): the [voice operator](#voice-operator) — `hosts`: where it runs, the first that answers (set the same list on every machine so all talk to one operator); the model its Claude session runs; the OpenAI voice that reads its replies; `/clear` after that many idle minutes (0 = never) |
| `web.focus` | `{ host? }` (default: none): Focus mode — the sidebar toggle runs the `focus` CLI (Do Not Disturb + quitting distracting apps) on `host`, the machine you sit at; set the same on every machine. Unset, or a host without `focus` (501): no toggle |
| `web.files.roots` | array of dirs (`~` expanded, default `[]`): extra roots a relative file path in chat may be under. A relative path missing under the session cwd first matches files the session touched (from its transcript), then ancestors of those, then these roots; the sandbox stays `$HOME` + cwd |
| `desktop.url` | the server [Fleet.app](desktop.md) opens instead of `hosts.<self>.web`; unset → this machine's server |
| `tmux` | tmux binary; `null` → `PATH`, then `/opt/homebrew/bin`, `/usr/local/bin` |
| `hosts.<name>.fleetBin` | path to `fleet` on that host; `null` → `~/.local/bin/fleet`, then `PATH` |
| `fleetBin` | this machine's `fleet` binary (used by the web server) |
| `claude` | command that launches Claude Code in spawned sessions (default `claude`) |
| `harnesses` | `{ codex: { command }, pi: { command } }`: how the [other harnesses](#other-harnesses) launch (default the bare name); read leniently |
| `spawnDirs` | directories offered for new sessions, per host (`paths.<host>`); also edited from the web UI (Settings → Start directories, which writes it through `fleet config set`). They are also the spawn allow-list. An entry with `"hidden": true` stays on the allow-list (e.g. a dir only an app's [embedded panel](#embed-view) spawns in) but is not offered in the New session menu; when a host has only hidden entries it offers Home, and the hidden dirs stay allowed. `fleet config show --resolved` lists them with `"hidden": true` |
| `tui` | dashboard preferences: `rows` (`"1"`, `"2"`, `"auto"`), `mouse` |
| `naming` | generated names: `enabled`, `model` (default `haiku`), `syncTmux` (tmux name follows the title, default on), `autoTitle` |
| `stacks` | [session stacks](#session-stacks): `enabled` (default `true` — `false` = never call the model; a new stack gets the skeleton StackBrief), `model` (default `sonnet`) |
| `restore` | [session recovery](#session-recovery): `onBoot` (default `false`) — when `true`, this host's web server runs `fleet --local restore --all` once at start if there are dormant sessions, at most once per boot (it remembers the boot id in `${XDG_STATE_HOME:-~/.local/state}/fleet/web-restored-boot`); the result is logged. For an always-on machine. Recently closed sessions are never part of it. `keepClosedDays` (default `7`, a number ≥ 0; `0` turns the [recently closed](#recently-closed) list off) — how long a session closed within a boot stays restorable; at most the newest 50 are kept |
| `bots` | [`fleet bots`](cli.md#bots), per machine: `{ "<name>": { schedule, dir, prompt \| promptFile, command, harness, model, ghIdentity, enabled, keep } }` — agent sessions this host starts from a recipe. Read leniently: a bad bot is a warning and is skipped |
| `ghIdentities` | [GitHub identities](cli.md#github-identities), per machine: `{ "<name>": { command, login, prompt } }` — GitHub Apps a spawned session can act as. Read leniently: an entry without `command` (or with a bad name) is skipped |
| `jobs` | the old name of `bots`, still read; a name in both is the `bots` one |
| `repos` | [`fleet repos`](cli.md#repos), per machine: `roots` (default `["~/Code"]`; every directory directly under a root with a `.git` is a repo), `every` (default `"24h"`; `30m` / `12h` / `7d`), `overrides` (`{ "<dir name or ~/path>": "30m" }`), `exclude` (dir names or `~/` paths). Read leniently: a bad value is a warning and falls back to its default |
| `prs` | [`fleet prs`](cli.md#pull-requests), per machine: `orgs` (search only these orgs), `repos` (`owner/name`s; with `orgs`, any of them), `refresh` (default `"5m"`, at least `1m`: how old the cache may get), `worktrees` (default `"~/Code/worktrees"`: where `fleet pr checkout` puts new worktrees). None of `orgs`/`repos` = all of GitHub. Read leniently |
| `tasks` | [`fleet tasks`](cli.md#productive-tasks), per machine: `organization` (an id; may be left out when the token belongs to one organization), `projects` (ids: only tasks in these), `refresh` (default `"5m"`, at least `1m`), `token` (the Productive API token; `$FLEET_PRODUCTIVE_TOKEN` wins over it), `keychain` (else: the service name of a macOS keychain item holding it, read with `security find-generic-password -s <service> -w` — keeps the token out of the file; `$PRODUCTIVE_API_TOKEN` is the last fallback). Read leniently |
| `gh` | `gh` binary for `fleet prs`; `null` → `PATH`, then `/opt/homebrew/bin`, `/usr/local/bin` |
| `grouping` | smart grouping: `enabled` (default `true` — `false` = repository fallback only), `model` (default `haiku`), `host` (the one host whose web server runs it; peers proxy `/api/groups` there), `consolidateMinutes` (default 60) |
| `inbox` | the [inbox](#inbox): `host` (the one host whose web server runs it; peers proxy `/api/inbox` there — unset = no inbox), `scanMinutes` (60), `workHours` (`[8, 18]`), `workDays` (`[0,1,2,3,4,5,6]`, 0 = Sunday; the three are also set from Settings → Inbox), `model` (`haiku`, the Slack + mail scan), `briefModel` (`haiku`), `briefMinutes` (45), `language` (`"English"`, the brief's), `comms` (`true`; `false` = no Slack + mail scan). `FLEET_WEB_INBOX=0` turns it off on a server |
| `slack` | [Slack links](#slack-links): `host` (the one host whose web server runs the bridge; peers proxy `/api/slack` there — unset = off), `owner` (your Slack user id `U…`: your messages in a linked thread are marked as the owner's). `FLEET_WEB_SLACK=0` turns it off on a server |

Rules: `~` is expanded at use time; unknown keys are preserved when the CLI rewrites the file
(writes go through the raw JSON, never the typed view); a missing file is fine — this machine is
the single host `local`, and remote features say "run `fleet init`"; a present but invalid file is
an error.

Env overrides for the web server: `FLEET_WEB_PORT` (or `PORT`), `FLEET_WEB_BIND`, `FLEET_WEB_UI`,
`FLEET_WEB_AUTONAME`, `FLEET_WEB_GROUPING`, `FLEET_WEB_BRIEFS`, `FLEET_BRIEFS_DIR`, `FLEET_STACKS_DIR`, `FLEET_BIN`, `FLEET_TMUX` — see [web/README.md](../web/README.md).

## Session discovery

Claude Code keeps a live registry: one `~/.claude/sessions/<pid>.json` per running interactive
session with its session id, cwd, name, status (`busy` / `idle` / `waiting`, and from Claude Code
2.1.308 `shell`) and what it waits for. That is the source of truth; `fleet` enriches each entry:

1. **pid → tty** via `ps`.
2. **tty → terminal**: match against `tmux list-panes -a` (pane id, window name, tmux session) and,
   on macOS, iTerm's sessions (session id, tab title). The match decides the **backend**
   (`tmux` / `iterm` / `unknown`) and the **handle** used to control it (tmux pane id like `%87`,
   or the iTerm session id).
3. **titles** from the naming cache (never generated by `list` itself).
4. **subagents and background shells** (`core::background`). The registry says `busy` while *any*
   agent of the session generates, background subagents included, and `shell` while only
   background shells run — though the main prompt is free and takes input at once. So for a
   `busy` / `shell` entry fleet reads the main transcript: a turn ends with `system` /
   `turn_duration`; an `Agent` result with `toolUseResult.status: "async_launched"` starts a
   background agent, a Bash result with `backgroundTaskId` a background shell, and a
   `<task-notification>` (in a user entry or a queue operation) ends either (`SendMessage` starts a
   finished agent again; `TaskStop` / `KillShell` stop one; what started before the process did is
   not running). When the main turn has ended and something runs beside it (or the registry says
   `shell`), the session is reported `idle` — `updated_at` then is when the turn ended — with
   `background`. An `Agent` call not answered yet in a turn in progress is a foreground agent
   (`background: false`). None of this is a stable Claude Code API: what is not recognised leaves
   the registry status as it is.

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
Codex's `effort` (and `model`) is the last `turn_context`'s (`null` effort there, the model's default, is
`null`). Only the transcript's last 512 KiB is read; when a turn has written more than that since
its `turn_context`, the last one is searched for backwards from there, up to 8 MiB from the end (beyond
that, `null`). pi's effort is not read.

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
- **Under the typing lock**: every rename types through `title::apply_rename`, which takes the
  session's typing lock (`core::lock`) first: another fleet command typing there (an answer's keys)
  makes it fail at once, nothing sent (kind `busy`; `fleet rename --json` prints it as one object).
  Holding the lock, and unless forced, it reads the terminal once more: a permission prompt or
  question form that came up since the session was looked up holds the rename as `waiting`.

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
| `status` | string | `busy` \| `idle` \| `waiting` \| `unknown` — `idle` = the main prompt is free, subagents may still run (`background`). Claude Code 2.1.308+ writes `shell` for a session idle at its prompt with a background shell (a `run_in_background` Bash) running: it comes out as `idle`, with `background.shells` at least 1 even when the transcript shows no shell |
| `waiting_for` | string \| null | what a `waiting` session is blocked on |
| `updated_at` | number \| null | last status change, ms since epoch |
| `background` | object \| null | what runs beside the main thread: `{ agents: [{ id, description, type, model, background, started_at, last_activity_at, activity }], shells }` (`background: false` = a foreground agent; `activity` = its last tool call, like `Bash: run the tests`); `null` = none or unknown |
| `prompted_at` | number \| null | last human prompt (transcript `origin.kind: "human"`), ms since epoch; `null` when unknown (none yet, Codex / pi). The web list sorts by it |
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
| `effort` | string \| null | the reasoning effort it runs at, as its transcript last recorded it (`low`, `medium`, `high`, `xhigh`, `max`, or whatever the harness writes). Claude: the context entry's top-level `effort`; Codex: `effort` of the last `turn_context`. A mid-session change (`/effort`, Codex's `/model` picker) shows from the next reply on. `null` when unknown: no reply yet, a harness version that records none, and always for pi |
| `subagents` | object \| null | `{ running, finished }`: tasks running now, across all Turns, and tasks that started and ended in the current Turn. `finished` alone is nullable. Claude Code 2.1.301+, from its sidecars, and Codex, from its child rollouts (`null` before the Session's first prompt); always `null` for pi. See below |
| `activity` | string \| null | what a Busy Claude Code Session is doing now: `"{tool}: {summary}"` of the latest main-thread `tool_use` with no `tool_result` for its id (the chat's own summary, `core::transcript::tool_summary`), or `Thinking` when every call is answered and the turn has not ended; `null` for any other Status (a pending `AskUserQuestion` is not Busy), after `end_turn` or an interrupt marker, and always for Codex and pi. See below |
| `model_at` | string \| null | the `timestamp` of the transcript entry `model` was read from, as written there (RFC 3339): Claude Code's last reply (the context entry), Codex's last `turn_context`, pi's last reply or model change; `null` when there is none (no transcript, or `model` from the command line). It moves with every reply, whatever its model — so a caller holding a `fleet model` switch's `switched_at` knows once a reply came after it |
| `transcript` | string \| null | absolute transcript path (Codex / pi only) |
| `origin` | object \| null | `{ job, run }` of the [job](cli.md#bots) run that started the session (its tmux pane option `@fleet_job_run`); `null` when none |
| `embed` | string \| null | the app whose [embedded panel](#embed-view) started the session (its tmux pane option `@fleet_embed`); `null` when none |
| `git` | object \| null | the git checkout its cwd is in, read from the `.git` files: `{ branch, root, linked, repo }` — `branch` (`null` = detached), `root` (absolute), `linked` (`true` = a linked worktree, `false` = the main checkout), `repo` (GitHub `owner/name` of `origin`, `null` when none). `null` outside a checkout. The web UI links a session to a pull request by `repo` + `branch` (a linked worktree only), on any host |
| `proc_start` | string \| null | the agent process's start time in RFC 3339 UTC (`2026-10-17T13:19:29Z`), read as the identity check reads it (`ps -o lstart`, what Claude records as `procStart`); with `pid`, what tells this process from a later one (`close --expect-pid/--expect-start`). `null` when it cannot be read |
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

That entry's top-level `effort` is the row's `effort` (not part of `context`). Its `perTurnEffort`
is not read: SDK sessions write it as `null` while `effort` is set. A `/effort` run in the
terminal is recorded at once only as a local command's output text, so `effort` follows it from
the next reply, as `model` follows `/model`.

#### Activity

`activity` comes from the same tail buffer and reverse pass as `context` (`core::context`), which
therefore does not stop at the first usage entry: it walks the main-thread `assistant` and `user`
entries back (`attachment`, `system` and bookkeeping lines are skipped) until the answer is
decided. A `tool_result` met on the way marks its id answered (parallel calls interleave their
results); the latest `tool_use` not so marked gives `"{tool}: {summary}"`, skipping
`AskUserQuestion`; control characters (carriage return, escape, tab) and the Unicode line and
paragraph separators become spaces, so it stays one line. Otherwise `Thinking` while the last entry
is a result, a thinking or text block without `end_turn`, or a prompt, including a prompt with no
reply yet (`activity` does not depend on `context`). The walk ends at a prompt, an earlier
`end_turn`, or, once every result met has its call, at an entry of an earlier API message (another
`message.id`; parallel sibling calls are separate entries sharing one id): the model writes a
message only after every call of the one before has its result, so only the latest can hold a
pending call. A long Turn's earlier calls, cut from their results by the buffer's edge, are so never
reached, and a poll parses a few entries. `null` after `end_turn` or a `[Request interrupted`
marker, and when a result's call is not in the buffer before the walk ends (a sibling may still be
pending there; the read is not widened for this). `discovery` blanks it unless the registry Status
is `busy`, so a stale call never shows on another row.

#### Subagent counts

`core::subagents` reads `<sessionId>/subagents/agent-*.meta.json` beside the Claude Code
Transcript. A sidecar without `finished: true` counts as running unless its task's lifecycle
evidence says it ended (below); a finished or failed task with it counts as finished only
when its first transcript line's `timestamp` is at or after the current Turn's start, unless
its task notifications say it still runs (below). A task stopped with `TaskStop` never gets
`finished: true` (seen on 2.1.309): only the evidence ends it. Nested tasks share this flat directory and each counts once;
foreground, background and forked tasks count alike. Background shell commands do not count.

The registry's `version` must be at least 2.1.301, and sidecars older than its `startedAt`
(epoch milliseconds) are ignored, including abandoned tasks left by a crashed process before
a resume. Missing or invalid version/start time, a missing Transcript, or an unreadable
sidecar directory gives `null`. A missing sidecar directory gives `{ running: 0, finished: 0 }`.
Every poll is a fresh `fleet list` process: there is no subagent cache, persistent or
in-process. Each call reads the directory and stats each matching meta before opening it;
nonregular, stale or oversized metas are skipped without being opened. Eligible regular
metas are read and parsed once each, capped at 1 MiB (including a growth check during the
read); malformed metas are skipped without affecting other candidates.
Both meta and log opens use `O_NONBLOCK`, then check the opened descriptor's metadata
(`fstat`) for a regular file before reading. Replacing a pathname with a FIFO after its
initial stat therefore cannot block the poll. FIFO regressions run in child processes
with a timeout and forced termination, without writers or thread joins.

A running task's transcript is never opened. For finished tasks, only sidecars with mtime
at or after the Turn start are candidates for a log read. Each candidate log is statted and
must be regular before opening; only its first line is read, bounded to 1 MiB. A missing or
malformed first timestamp skips that candidate until a later poll can read it. The 210-meta
I/O regression asserts the exact meta/log opens on each call; the ignored
`tests/subagents.rs` benchmark measures fresh CLI processes, comparing an absent sidecar
directory with 210 synthetic metas using alternating trials and a release build.

A Codex Session's subagents are the child rollouts of its descendant threads: every rollout
the day-folder scan already reads whose `session_meta.source.subagent.thread_spawn.parent_thread_id`
is the Session's thread id or a descendant's, at any depth, each once (`/review`'s child names
no parent and doesn't count). Only rollouts written at or after the Session's process start
count, so a child an earlier process left mid-turn never runs forever. Each child's last turn
event in its last 256 KiB decides: `task_started` runs, `task_complete` or `turn_aborted` ended
it, and a cut tail with none is a turn that has written more than that since it started, so it
runs (a whole rollout with none hasn't started one and runs nothing). A child opens with a zero-length copy of its parent's turn (`task_started`, then
`turn_aborted`, in one millisecond) before its own, which the last event passes over. A child
open between turns runs nothing, as the Session's own Idle says. `finished` counts the ended
children whose last turn started at or after the Turn's start (its `task_started`, or, once that
has left the tail, the `started_at` its end carries, in whole seconds): the parent's last
`task_started` in its state tail whose `turn_attribution.turn_trigger` is `user` or `exec`, or
that records none (a turn something else started keeps your Turn). No Turn start in that tail
gives `finished: null` once a child has ended; with none ended it is 0. Seen on Codex 0.166.0:
your prompt's turn has `turn_trigger: "user"`, a child's own turn does too, with
`parent_turn_id` and `initiating_agent_path`, and a child's report starts no turn in the parent.
Like the Claude Code count, nothing is cached across polls.

`core::context` finds the Turn start in the same reverse pass and buffer as context, model and
effort: the last main-thread human `user` prompt or human `queued_command` attachment with
`commandMode: "prompt"`; an SDK `user` prompt without origin counts too. Notifications, tool
results, meta/peer messages, compaction summaries, interrupt markers, local commands and
`last-prompt` bookkeeping do not reset it. The shared read widens from 256 KiB through 2 MiB
to 8 MiB until both reply and prompt are found, and further while a finished task's
notification may still lie beyond the buffer (below). Finding a prompt ends the prompt search
even if its timestamp is missing, not a string or invalid: `finished` is then `null`, and
widening for a reply cannot reuse an older Turn. No prompt within that cap also gives
`finished: null`; with no eligible finished sidecars, the prompt search is skipped and
`finished` is 0. Running counts never depend on finding a prompt. Sidecars without
`finished: true` never widen the read for the prompt, though the pass still looks for it in
the buffer it reads: one they settle counts in `finished` only if that buffer holds the Turn's
prompt, and `finished` is `null` otherwise. They widen it for their own evidence only while
quiet (below).

`finished: true` marks the end of a run, not of the task: one waiting on background work of
its own gets it, sends an interim task notification, and runs again under the same id. So a
current sidecar with `finished: true` still counts as running when the latest task
notification naming its id (the `<task-notification>` element's `<task-id>`) is `completed`
with a `<note>` containing `background work of its own still running`, or has a `<note>`
containing `arrived after this run` (new input resumes it at once). Notifications are `user`
entries with `origin.kind: "task-notification"` and `queued_command` attachments with
`commandMode: "task-notification"`, their content a string or text blocks. One string or
block can hold several elements: a parked owner that resumes gets all its queued notices in
one prompt, joined by a blank line, so after the element that opens the text, each
`</task-notification>`, blank line, `<task-notification>` starts another. Every element is
read, and within one entry a later element for an id beats an earlier one. Only each
element's direct children `<task-id>`, `<status>` and `<note>` are read: `<summary>` is
skipped over and the walk stops at `<result>`, since both can quote anything; the search for
the next element passes over the `<result>`, so one that quotes that exact joiner would be
read as a further element. A notification without a `<status>` (routing) is skipped. A later successful `TaskStop` result for the id
(a `tool_result` that isn't an error, whose `toolUseResult` has `task_type: "local_agent"` and
that `task_id`; without a `toolUseResult`, the same fields in the `tool_result` content, a JSON
string or text blocks holding one) settles it; so does any other latest notification with a
`killed`, `stopped`, `failed` or `completed` status. No notification for the id in the tail
counts it finished, as before.

The same evidence ends a current sidecar without `finished: true`: when its latest is a
successful `TaskStop` or a notification with one of those terminal statuses (`completed`
with the settled note or one Fleet doesn't recognise), it counts as finished, not running; an
interim or pending-input note keeps it running, and a later one reopens a stopped task. A
status Fleet doesn't know decides nothing either way: the sidecar's own `finished` stands. A
revive (`SendMessage` to a task that ended) clears `finished` and records `revivedAt` (epoch
ms), so once a meta has `revivedAt` only evidence whose `timestamp` is at or after it counts:
an older terminal entry, or one without a `timestamp`, belongs to an earlier run and the
revived task runs. Claude Code 2.1.316 writes no `revivedAt`: the revive only drops
`finished`, so the earlier run's settled notice would still be the latest evidence. A
successful `SendMessage` result naming the task (`toolUseResult.revivedAgentId` or
`resumedAgentId`; without a `toolUseResult`, the same fields in the `tool_result` content's
JSON, and never one with `success: false`) is therefore evidence that it runs, at its
`timestamp`; the revived run's own notice, written after it, settles it again. A message to a
task still running is only queued and names none. The meta's mtime decides nothing here, since worktree cleanup
(`worktreeCleanlyRemoved`) and a user stop rewrite the meta after the stop. A meta with
`stoppedByUser: true` has ended with or without evidence: Claude Code won't revive it. A
sidecar settled this way ended at the `timestamp` of the entry that
settled it, and counts in `finished` when that and its first transcript line are at or after
the Turn's start. The evidence comes from the same reverse pass and
buffer as the Turn start, which then runs to the buffer's start. While a finished depth-1
sidecar has no evidence in that buffer and its mtime (the run's end) is older than the
buffer's first `timestamp` (or the buffer has none), the read keeps widening along the same
256 KiB → 2 MiB → 8 MiB steps: the notification is written at or after the end, so it may lie
further back, as an interim note does when the parent has written more than the tail since a
task parked on background work of its own. Beyond 8 MiB, no notification still means
finished. The read widens the same way while an unfinished depth-1 sidecar has no evidence in
the buffer and its own `agent-<id>.jsonl` (statted, never opened) was last written before the
buffer's first `timestamp`: a task quiet since before the buffer starts may have been stopped
further back, as a stop is once about 256 KiB of new transcript follows it. One without a log
file, or whose log was written since, never widens it; beyond 8 MiB it counts as running. When
every such sidecar has evidence, or ended or last wrote inside the buffer, nothing more is
read. A nested task's (`parentAgentId`) notifications go to its owner, so the last 256 KiB of the
owner's `agent-<parentAgentId>.jsonl` is read too, once per owner per call, under the same
regular-file open rules; the later of the two files' latest entries (by `timestamp`) wins.
An owner's tail is not widened, and a nested sidecar never widens the parent's read. Nested
sidecars without `finished: true` read their owner's tail too. With no current sidecar, no
notification is parsed and no owner transcript is read.

Per-poll cost of the lifecycle evidence: nothing survives between polls, since each
`fleet list` is a fresh process (the in-process tail cache helps only a long-running caller
such as `watch`). With any current sidecar, finished or not, every poll:

- stats the log of each unfinished depth-1 sidecar;
- reads the parent's tail as far as it widens, and parses in one reverse pass that runs to
  the buffer's start instead of stopping at the reply: every lifecycle candidate line (one
  containing `task-notification` or `local_agent`), and every `user` or `attachment` line
  until the Turn's prompt is found. A Session whose current sidecars are all unfinished pays
  this too: they don't widen the read for the prompt, but the pass still looks for it;
- reads the last 256 KiB of the owner transcript of every nested sidecar, finished or
  running, once per owner, for as long as that process runs;
- reads the first line of each log that ended in this Turn.

The parent read stays at 256 KiB only while the prompt, every finished depth-1 sidecar's
latest notification and every quiet unfinished one's stop lie within it. In a long Session
that ran a background Subagent the steady state is a 2 MiB or 8 MiB read on every poll, since
an old Subagent's notification soon lies beyond the first step, and a running Subagent that
has written nothing for longer than the first tail spans (one waiting on a long tool, say)
holds the read at 2 MiB or 8 MiB until it writes again. Measured with a release build, six
alternating fresh `fleet --local list --json` processes per build against a synthetic home
(one registry entry, a 7.95 MiB Transcript read whole, 200 metas: 120 finished before the
Turn, 40 in it, 30 running, 10 stopped with `TaskStop`): 30.1–33.2 ms (median 32.2 ms),
against 30.5–33.4 ms (median 32.9 ms) for the build before unfinished sidecars were settled
(warm cache).

Known limits: a task that dies without its sidecar being finalized and without a terminal
notification or `TaskStop` in the tail counts until the Session ends. Notifications are written when Claude Code delivers them, which can lag a run's end:
a task whose interim notification isn't written yet, or that new input resumed before the
notification saying so, reads as finished until it is written (live, for 1–6 s). A revived
task's first timestamp still dates the original run. A delayed first entry can place a task
launched just before a prompt in the next Turn. A Codex descendant is reached only through
threads the day-folder scan reads: a new grandchild under a child last written before the
oldest Codex process started (an idle child of a resumed Session, say) is missed.

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
- **`providers`** (added; the fields above are unchanged and stay Claude's): Claude and Codex in
  one shape — `{ id, label, status, plan, limits[], fetched_at, observed_at, error, credits }`,
  `limits[]` items as above. Shape, statuses and severities: [cli.md](cli.md#usage-json).

**Codex limits** (`core::usage_codex`) come from Codex's own transcripts — no network call. Each
turn appends a `token_count` event to `$CODEX_HOME/sessions/YYYY/MM/DD/rollout-*.jsonl` (default
`~/.codex`) with `rate_limits: { limit_id, primary, secondary, credits, plan_type, … }`; a window is
`{ used_percent, window_minutes, resets_at }` (epoch seconds; older Codex wrote
`resets_in_seconds`). `rate_limits` is `null` for logins without subscription limits (an API
key). Each `limit_id` is a bucket and its newest snapshot counts: `codex` (or none, from older
Codex) holds the main windows, other ids with windows are scoped limits, and ids without windows
(`premium`) only carry credits — see [cli.md](cli.md#usage-json).

- **Finding the newest snapshots**: every regular `rollout-*.jsonl` file is `stat`ed (no reads),
  then read newest mtime first, backwards in 256 KiB chunks, newest line first; torn, malformed
  and non-UTF-8 lines are skipped, and so are lines longer than about 1.25 MiB (a line is dropped
  once more than 1 MiB of it is held, so up to 1 MiB + one chunk can come back). Files are ordered by mtime, not by day directory,
  because a resumed or long session keeps writing to the day it started in.
- **Early stop**, on an *assumption*: a file's mtime is at least the time of its last event
  (Codex only appends). So once a file's mtime is more than 60 s older than the main snapshot
  found, no later file can hold a newer one, and the scan stops. Within a file, reading stops at
  the first event more than 60 s older than the main snapshot once windowless credits are known.
  Scoped snapshots that old don't count either, so scoped limits and credits don't depend on the
  order of events within that window.
- **Bounds per call**: 16 MiB read in total, 64 files, and 8 files whose `token_count` events all
  have `rate_limits: null` while nothing else was found. So a poll during a busy Codex session,
  or on an API-key login, never reads more than 16 MiB. A window more than 16 MiB from the end of
  every file is missed (`no_data`).
- **No cache**: a scan takes a few ms (about 4–7 ms over ~860 rollouts), so every call reads
  afresh and nothing is written. The reset rule runs on every call.
- **Errors**: a missing `sessions` directory is `no_data`; any other error reading it is
  `status: "error"` with the reason. An unreadable year, month or day directory or rollout is
  skipped.

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

Fresh `spawn` sessions accept per-run effort and permission overrides for Claude Code and
Codex. Core backend launch options append the harness's flags after model/name and before the
prompt; the CLI validates effort with `core::effort::valid_level` and rejects these overrides
for pi, `--resume` and `--from`. Other spawn commands leave the options at their defaults.

### `fleet diff --json`

See [cli.md](cli.md#changes). `core::diff` ports the snapshot of the web's `web/lib/diff.mjs`
(temp index, `add -A`, `write-tree`, merge-base) with `--numstat`/`--raw` in place of hunks, and
narrows `web/lib/touched.mjs` to edits: the web's touched index also counts paths a tool read, which
would make every repo a session looked at a candidate. The edits index is incremental per session
(state file + `flock`), since the CLI is a fresh process per call and a long transcript runs to
100 MB. Every git call is fenced (`GIT_*` cleared, no hooks, no fsmonitor, no lazy fetch, no
optional locks) because a poller runs it against repos an agent is working in. Codex sessions get
their cwd only: 0.16x rollouts carry no `patch_apply_end`, and `apply_patch` runs inside `exec`.

### `fleet messages --json`

See [cli.md](cli.md#messages) for the shape. The parser (`core::transcript`) is a port of the web's
`web/lib/transcript.mjs` plus tool calls, question outcomes and cursors:

- An entry's **position** is the byte offset of the transcript line that started it plus its index
  among that line's entries — stable while the transcript is only appended to. An entry cursor is
  `m2.<offset>.<index>.<line fingerprint>.<file id>` (file id: device and inode, hashed); the
  top-level cursor appends a read hint
  `.<end>.<rescan>.<fingerprint of the bytes before end>.<queue>.<a|p>`,
  `queue` being Claude's prompt queue at `rescan` (`<enqueue line>:<content hash>,…`, or `!` when
  it would not fit the 512-character cap — such a cursor cannot resume); `p` marks a cursor minted
  by a `--limit`ed page (entries after its position not delivered yet), `a` one that delivered
  everything it read.
- `--after` checks the file id, the anchor line's fingerprint and the bytes before `end` (a
  replaced, edited or shrunk transcript is `cursor_invalid`), re-reads the queue's enqueue lines,
  and parses from `rescan`: the latest line at which nothing was open — no question waiting for its
  result, no reply a later line could still extend. That is `end` itself most of the time, so a
  poll reads only the appended bytes. Re-reading more than 2 MiB of what was delivered — from
  `rescan` to `end`, or for a `p` cursor to its position — is `cursor_invalid`: the client
  reloads rather than resuming with that state lost.
- A question still waiting when a new prompt arrives is `abandoned` (its result never comes).
- `--before` parses its window, then reads on past it until nothing opened in the window is
  still open (a reply's later blocks, a question's outcome, Codex's final flag).
- A Claude message's text blocks are glued into one entry, across the tool-call entries between
  them, like the web parser. The message is taken as complete at its first tool result or at the
  next message's first line (results and later messages always come after a message's lines), so
  gluing never keeps a restart point open past it.
- Lines no parser reads (API request logs, hook output, tool output events, reasoning) are skimmed
  by a typed prefilter without building a JSON value; on real transcripts the initial tail (2 MiB,
  doubled up to 64 MiB until it holds `--limit` entries) parses in ~10 ms on average.

### `fleet answer --json`

See [cli.md](cli.md#answering-prompts) for the shapes and kinds. `core::prompt` reads prompts off
the screen; `core::questions` ports the web's `validateAnswer` / `answerSteps` as a plan whose every
step carries the state the form must be in before it (`At`: cursor row, ticked rows, typed text,
answered tabs; for the final Enter the whole review). `core::lock` is the per-session typing lock.

- A capture is the scrollback, a mark, then the visible screen. A prompt is live only when its
  numbered answers are on the visible screen and end it (≤ 4 footer lines after them, no input
  box); the scrollback only extends a prompt upward. A pane with no scrollback (a fullscreen
  TUI's alternate screen) has nothing above the mark: tmux would return the screen's first line
  for `capture-pane -S -N -E -1`, so `history_size` is asked first.
- Claude Code's permission prompt is framed: a solid rule above it, then a header, then
  optionally one dashed pair setting off the command; everything in the frame is its detail. The
  frame's rules are drawn from column 0 while everything inside it is padded, so a rule-like line
  of the command (indented) is content, never the frame. No frame in the capture, or column-0
  rules between the top (the nearest solid one) and the title that are not exactly that shape (a
  lone or third dashed rule, a pair with no header above it): incomplete. **Assumption:** Claude
  Code pads every line inside the frame. Only a synthetic Bash fixture backs it (no captured Edit,
  Write, MCP or plan-approval layout), and a terminal that autowraps a line Ink measured narrower
  than it draws (emoji, ZWJ, ambiguous-width characters) puts the overflow at column 0, where a
  crafted command could forge a frame. Binding the identity to the pending `tool_use` in the
  transcript, with the screen only confirming that the prompt is up, would remove it; not done.
- Codex's approval has no frame, and its title, command and reason share one indent. The prompt
  is everything between the last line drawn from column 0 above the answers (the last conversation
  item, `•` / `›`) and the answers; exactly one line of it may read as a title, else (a command line
  like `Allow …`) incomplete, as is none (a line merely ending in `?` is not one) and a top out
  of sight (over 60 lines up, or no column-0 line captured at all). Its detail is that whole span
  but the title. With no title in sight, a list is an approval only with the approval's answer
  shape (a `(y)` / `(esc)` shortcut in an answer, or the footer `… esc to cancel`): Codex's popups
  (`… esc to go back`) and transcript pager are no prompt. With that shape, answers the walk
  cannot read are incomplete, never no prompt; Codex's answers wrap onto any number of lines
  (Claude Code's: at most 3). **Assumption:** Codex pads everything it draws in the approval (title,
  command, reason), so no line of a command starts at column 0 and passes for the conversation item
  above it; a terminal that wraps an overlong line to column 0 itself breaks it, as for Claude Code.
  Only recorded approvals of a one-line command back it (no patch, multi-line or
  `request_user_input` layout).
- A list in an input box is a draft, never a prompt: a plain rule from column 0 under it (Claude
  Code's input box, pi's editor; not a boxed prompt's `╰───╯`), or Codex's composer footer (`? for
  shortcuts`, `… context left`, `⏎ send`).
- An incomplete prompt is never read as no prompt: `answer` refuses it (`prompt_incomplete`), the
  post-answer check counts it as a prompt that may be the one answered or a queued lookalike
  (`outcome_uncertain`), and a rename holds for it.
- The `id` is `p2.<FNV-1a of the title, every line of the detail as drawn (indentation, blank and
  rule-like lines kept; a side frame — a box character at the left edge and one at the end; an
  indented line keeps its box characters — trailing blanks and Claude Code's frame rules, as wide
  as the pane, dropped), and the answers>` — whitespace is part of a command, the frame's width is
  not (a line wrapped differently after a resize still changes the id). (`p1.` ids hashed trimmed lines; they no longer match.)
- A question form is parsed whole (tabs with ☐/☒, the active question, rows with checkboxes and
  descriptions, "Type something", Submit/Next, "Chat about this") and matched against the
  transcript's one active question; the review screen (`● question` / `→ answer`, multi-select picks
  and typed text joined with `", "`, wrapped lines rejoined at a space) is compared exactly. Since
  that join is ambiguous (an option labelled `A, B` vs the options `A` and `B`), an answer to a
  multi-question form whose review text another choice would also produce is refused up front
  (`answer_invalid`).
- A form is answerable only read whole: its numbered rows run without a gap up to row 1 on the
  capture, and its head (tabs, question) is read up to the form's top rule or tab bar. Otherwise
  it is clipped (`prompt_incomplete`) — the form taller than the pane (Claude Code's fullscreen
  TUI keeps no scrollback, so its top is nowhere), or a long list drawn a window at a time. A
  single-select question with a preview on an option (`Question::preview_layout`; the option's
  `preview` string is passed on in `messages` / `answer --list` JSON) is drawn in Claude Code's
  preview layout, read by `core::prompt::read_preview_form` into a `Form` with
  `Layout::Preview` (options left of the box's column, the box never read, the notes row,
  "Chat about this" unnumbered) and matched on tabs, text and labels only — each question of a
  form in its own layout; a footer over rows it cannot walk is `layout_unsupported`. Backed by
  real Claude Code 2.1.312 captures (`testdata/screens/live/claude-question-preview-*`), as is
  the `│ ` gutter before a question over 80 characters (dropped on the form and the review).
  **Assumption:** only synthetic layouts back the clipping rules;
  how Claude Code 2.1.303 windows a long list (and whether a scroll marker sits in it) is not
  captured yet.
- Every refusal by a screen check says which one as a `core::prompt::Detail` (`detail` in the
  JSON: `no_form`, `clipped`, `layout_unsupported`, `tabs`, `question`, `options`, `touched`,
  `cursor_moved`, `different_prompt`), with the pane's size — codes only, never screen or
  transcript text, so a caller can log them; `error` stays free text. `PromptRead::Incomplete`
  carries its code beside its reason, `Form::check` / `Form::ready` theirs beside their words.
- A capture tmux reports as failed (pane or server gone) is an error, never an empty screen.

### `fleet model --json`

See [cli.md](cli.md#switching-the-model). `{ ok: true, host, session_id, pid, harness, model,
confirmed, switched_at, dry_run }`, or `{ ok: false, host, kind, error }`: exit 1, `send_held`
exit 3 (`reason`, `input?`, as `send`'s), `busy` / `host_shared` with `close`'s `reason` (plus
`host_shared`'s new `suspended`), `not_idle` with `status`. Under one typing lock: `close`'s
foreground check (`cli::close::standing`, without `other_jobs`: nothing is killed), the
registry's Status (read again under the lock, by the agent's pid; its session id must still
be the locked one, else `target_changed`), one styled capture for the working line
(`core::input::claude_working`) and the box (`core::input::of_screen`: `send`'s reader, the
caret included, holding as `--require-empty` always), then `/model <id>` + Enter, then the screen (`core::model::shown`,
which reads the dialog with `core::prompt::permission_prompt`, `answer`'s reader) and the transcript's new lines (`core::model::Tail`, entries no earlier
than the command's start) every 100 ms for 5 s — at least 2 s past the confirmation's key, the
foreground check run again and the screen read again before it (no key unless that very
confirmation is still there), each tmux call in that wait bounded by what is left of it
(`backend::peek_styled_within`, `press_within`, `tmux::pane_tty_within`). `switched_at` is the
transcript's own `timestamp` string, on the same clock as `list`'s `model_at`; success needs a
`Set model to` label that is positively the model asked for (`core::model::same_model`: the id
itself, or one model's label, aliases through fleet's Claude model list, `[1m]` and date
suffixes the same model). Any other label, `Kept model as`, silence, and a read or key failing
after the command are `outcome_uncertain`. An `io` failure carries `typed` (as `send`'s).

### `fleet send --json`

See [cli.md](cli.md#sending). Under the session's typing lock, `core::input` reads the visible
screen once with its text attributes (`backend::peek_styled`: tmux `capture-pane -p -e`; iTerm's
text has none) and, for Claude Code, its caret (`backend::cursor`: the terminal's cursor, its
position while it shows, `Hidden` while it does not — then the one reverse-video cell Claude Code
paints in its box; blanks typed in the box show only by the caret, and without one a box
showing nothing is `unreadable`), and the text is typed under that same lock. Holding the
lock, the process is looked up again by its pid (`lock::still_named`): the lock is named after
the session id, which `/clear` changes, so a new id there is `target_changed`, nothing typed
(`keys`, `answer`, `close` and the renames check the same). An `io` failure of `send`, `keys`,
`rename`, `answer`, `model` or `close` carries `typed` (`commands::typed_field`): whether a key
reached the pane before it — an addition a caller retries by instead of parsing `error`.
Text in the box (`reason: "text"`) or
a prompt in its place (`"prompt"`) holds the send; no box found (`"no_input_box"`) or none
readable (`"unreadable"`) types, adding `warning: <reason>` to `{ ok: true, host, target }` and
one line on stderr — unless `--require-empty`, which holds on those too. Held is `{ ok: false,
host, kind: "send_held", reason, error, input? }`, exit 3, `input` the box's text when it shows
any: sensitive (whatever was typed or pasted there), so it is in that field alone, never in
`error` or on stderr. `warning`, `reason` and `input` are additions: every other result keeps its
shape, and every other failure exit 1. A host running an older fleet rejects `--force` and
`--require-empty` sent through `-H` (exit 2, `usage`). `rename` (`held: "input"` / `"waiting"`, and with `--require-empty` `"no_input_box"` / `"unreadable"`, in its own `result: "held"` report)
and `close` (no exit command, straight to the kill) read the box the same way.

- The capture is parsed into cells (character, faint, bold, reverse video, foreground and background
  colour — palette or 24-bit, its numbers never read as attributes); tmux carries attributes
  from one line to the next, so the parse does too. Plain text decides where Claude Code's and
  pi's boxes are (the prompt check `answer --list` uses runs first); Codex's composer is bounded
  by its background, or else by its footer (the last rows on screen, under a blank row, one of
  them drawn faint, bold or in a colour, one with a footer's known text; `unreadable`, never
  empty, when those rows cannot be told from the composer's), its `[Image #N]` attachment rows
  above it included; the attributes decide what in the box is typed, and Claude Code's IDE pill (a leading `[…]` in a colour of its own) is left
  out.
- **Assumption:** the harnesses draw what an empty box shows faint, and typed text never.
  Claude Code 2.1's rendering was read from its code (placeholder `chalk.dim`, its drawn cursor
  `\e[7m` on the placeholder's first character — the terminal's cursor hidden then, unless its
  native cursor is on, when it draws none —, the IDE pill
  `[${…}] ` in the theme's `ide` colour); the fixtures in `testdata/screens` are tmux captures of
  synthetic screens in each layout (`gen.sh` there makes them), not of live sessions.

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

### `fleet tasks --json`

See [cli.md](cli.md#productive-tasks) for the shape. Column ids, `Task` and `Status` fields follow
the add-only rule; `column` is worked out on every call (the due-soon window moves with the date),
never cached. Each PR's `tasks` (in `fleet prs --json`) holds the task ids its title or description
links to — the join between the two lists.

### `fleet restore --json`

`{ host, bootId, dormant: [DormantView…] }`, and the restore / forget results — see
[cli.md](cli.md#session-recovery) for the shapes and [Session recovery](#session-recovery) for the
state file behind them.

### Web API

JSON over HTTP, errors as `{ "error": "..." }`. At a high level:

| method | path | purpose |
| --- | --- | --- |
| GET | `/api/health` | name, version, apiVersion, self, uptime, autoName (`lastRun`) |
| GET | `/api/settings` | apiVersion, self, host names, quick replies, `transcribe: { enabled }` (this server has a transcription key), `speak: { enabled, voice }` (it can read replies aloud), `operator: { enabled, hosts }` (the [voice operator](#voice-operator) and where it runs), `focus: { host }` (`web.focus.host`, null when unset) |
| GET | `/api/fleet[?local=1]` | `{ self, hosts: [{ name, ok, error?, sessions, spawnDirs, hiddenSpawnDirs?, home?, notes?, stacks? }], snapshotAt }` — `spawnDirs` (`[{ label, path }]`): what that host offers in New session, without its hidden entries (Home when it offers none); `hiddenSpawnDirs` (same shape, present only when it has any; older servers omit it): its `hidden: true` entries, allowed for spawns (the embed view checks `dir` against both) but not offered — `home`: that host's home directory (for `~/…` paths, e.g. the [embed view](#embed-view)'s `dir`); older servers omit it — `stacks`: that host's [session stacks](#session-stacks), compact (`{ id, label, host, members: [{ session, host, live }] }`), from which the merged view stamps `stack: { id, label, host }` on the rows of members running on *other* hosts; sessions are `list --json` objects + `host` (a Codex session with no transcript yet gets `session_id: "pid-<pid>"` until it has one) (`title` capped at 300 chars) + `editorUrl` (the "Open in editor" link for its cwd, built by the server that answered — see [Session briefs](#session-briefs)); the merged view is a warm background-refreshed snapshot |
| GET | `/api/hosts/:host/sessions/:id/peek?lines=N` | plain-text screen |
| GET | `/api/hosts/:host/sessions/:id/messages?limit=N` | conversation (no tool calls) from the transcript; an AskUserQuestion call is a message of `kind: "question"` with `id` (the tool use id), `questions` (`[{ question, header, multiSelect, options: [{ label, description }] }]`), `text` (a markdown rendering) and, once it has a result, `answers` (per question, the answer string) or `declined: true`. Claude Code (2.1.310+) writes the call to the transcript only once it is answered, so while the session waits on one the terminal is read instead: the prompt's current tab (or its "Review your answers" screen, a question with `noOther: true`) is appended as a question message with `id: "screen:<hash>"`, one question per tab; a prompt typed while Claude works and still in its queue is a `kind: "user"` message with `queued: true`, always last (prompts it took mid-turn show where it took them) |
| POST | `/api/hosts/:host/sessions/:id/send` | `{ text }` → typed + Enter |
| POST | `/api/hosts/:host/sessions/:id/keys` | `{ key }` (Enter, Escape, …) |
| POST | `/api/hosts/:host/sessions/:id/answer` | `{ toolUseId, answers: [{ options: [index…], text? }] }` (one per question) → types the arrows/Enter/text a human would into the pending AskUserQuestion prompt. 400 bad answer (single-select: one option or text; multi-select: at least one), 404 unknown question, **409** (nothing typed) when it is already answered, the session isn't waiting, or the terminal isn't showing that prompt untouched (first question, cursor on option 1). A `screen:` id answers just that tab, and only while the terminal still shows it |
| POST | `/api/hosts/:host/sessions/:id/rename` | `{ title }` (1–64 chars, one line) → `fleet rename <session_id> <title> --json`; 200 with the report, **409** when held (waiting on a prompt, or a busy Codex / pi session; nothing typed), 400 bad title, 502 CLI failure |
| POST | `/api/hosts/:host/sessions/:id/autoname` | regenerate one session's name → `fleet name <session_id> --apply --refresh`; 200 `{ ok, host, id, name }` (`name` null: unchanged), **409** when held (waiting on a prompt; nothing typed), 502 CLI failure |
| POST | `/api/hosts/:host/spawn` | `{ name?, dir?, prompt?, model?, harness?, ghIdentity?, embed?, slack? }` → new tmux session running that [harness](#other-harnesses) (`claude` default, `codex`, `pi`; its launcher is config `claude` / `harnesses.<id>.command`; `-n <name>` for Claude and pi only); `dir` must resolve inside one of the host's `spawnDirs`, hidden ones included (else 400; no `dir` = its first offered one); a prompt too long to type (the launch line over ~900 bytes) is written to `~/.claude/fleet-prompts/` and passed as `"$(cat <file>)"`; a first-run trust prompt is accepted (Claude: Down + Enter; Codex: Enter on "Trust and continue"); when the agent never starts (the pane is still just a shell after ~10s) the tmux session is killed and the answer is 502 with the pane's last lines. `ghIdentity` (Claude only; one of the host's `ghIdentities`, else 400): the session acts on GitHub as that [GitHub identity](cli.md#github-identities) — its launcher comes from `fleet --local gh-identity launcher`, and the pane gets `@fleet_gh_identity`. `prs/spawn`, `tasks/spawn` and the stack spawns take it too. `embed` (`[A-Za-z0-9_-]{1,40}`): the app whose [embedded panel](#embed-view) starts it — the pane gets `@fleet_embed`, `list` reports it as `embed`. `slack` (`{ url, minutes, mode?, task? }`, as `/api/slack/link`; only on `slack.host`, else 400 before anything opens): the session starts with no prompt, is linked as soon as it registers (≤ ~45 s), and `prompt` (max 4000 chars) ends the link's intro as the owner's first request; the answer carries the Link as `slack`. A link that fails after the spawn: 502 (400 for a link the bridge refuses), the session stays idle, its prompt unsent |
| GET | `/api/hosts/:host/harnesses[?refresh=1]` | `{ host, harnesses: [{ id, label, command, installed, path, version, models }], ghIdentities: [{ name, login }] }` — `fleet --local harness list --json`, cached 10 min; Claude's `models` are `web.models`. An older CLI answers Claude alone |
| GET | `/api/hosts/:host/usage[?refresh=1]` | that host's usage limits (Claude's subscription, plus Codex in `providers`): `fleet usage --json` + `host` (see [Subscription usage](#subscription-usage)); 502 with the CLI's reason (no login, endpoint down with nothing cached) |
| POST | `/api/hosts/:host/prs/checkout` | `{ pr: "owner/repo#123" }` → `{ host, worktree }`: `fleet pr checkout --json` on that host |
| POST | `/api/hosts/:host/prs/seen` | `{ pr: "owner/repo#123" }` → `{ host, pr, seenAt }`: `fleet pr seen --json` on that host — its @mentions of you so far are seen |
| POST | `/api/hosts/:host/prs/spawn` | `{ pr, prompt?, name?, model?, harness? }` → `{ host, worktree, spawn }`: the checkout, then a new session in the worktree whose first prompt is `worktree.contextLine` + `prompt` (like a stack sibling: the web spawner, not `fleet pr spawn`). Starts an agent |
| GET | `/api/hosts/:host/prs[?refresh=1]` | the GitHub PRs that need you, as `gh` on that host sees them: `fleet prs --json` + `host` (see [cli.md](cli.md#pull-requests)), and `local.editorUrl` ("Open in VS Code / Cursor" for the worktree, like a session's `editorUrl`); 502 with the CLI's reason (no `gh`, not logged in, GitHub down with nothing cached), 501 when that host's CLI predates `fleet prs` |
| GET | `/api/hosts/:host/tasks[?refresh=1]` | your open Productive tasks, as that host's token sees them: `fleet tasks --json` + `host` (see [cli.md](cli.md#productive-tasks)); 502 with the CLI's reason (no token, several organizations and none configured, Productive down with nothing cached), 501 when that host's CLI predates `fleet tasks` |
| POST | `/api/hosts/:host/tasks/status` | `{ task, status }` → `{ host, task }`: `fleet task status --json` on that host — moves the task (for everyone) |
| POST | `/api/hosts/:host/tasks/spawn` | `{ task, dir, prompt?, name?, model?, harness? }` → `{ host, task, spawn }`: a new session in `dir` (validated like `/spawn`) whose first prompt is the task's `contextLine` (`fleet task show --json`) + `prompt`. Starts an agent |
| GET/POST | `/api/hosts/:host/focus` | that host's `focus status`: `{ host, active, remainingMs, endsAt, apps, stale }`; POST `{ on, minutes? }` runs `focus start [minutes]` / `focus stop` (a no-op when already so) and answers the new status. 501 when that host has no `focus` CLI, 502 with its message when it fails |
| GET | `/api/hosts/:host/spawn-dirs` | that host's stored `spawnDirs` (canonical `{ label, paths, hidden? }`, `hidden` only when true, keys it does not know kept), whether each of its own paths is a directory there, what it offers now (`offered`) and its hidden dirs now (`hiddenSpawnDirs: [{ label, path }]`) |
| PUT | `/api/hosts/:host/spawn-dirs` | `{ spawnDirs, dryRun? }` → validated on that host (its own paths must exist; `hidden` must be a boolean, stored only when true; other keys on an entry are kept), written with `fleet --local config set spawnDirs`, used at once (no restart); 400 with per-entry `errors`. As powerful as spawn (it widens the spawn allow-list); no auth, like spawn |
| GET/PUT | `/api/hosts/:host/repos` | Settings → Git repos: that host's `repos` settings, every repo (`fleet --local repos --json --all`) and its sync timer; PUT writes `repos` with `fleet --local config set` |
| GET | `/api/hosts/:host/jobs` | `{ host, jobs }`: that host's [bots](cli.md#bots) (the API keeps their old name, jobs), `fleet --local jobs --json` (each with its latest run and live sessions, see [cli.md](cli.md#bots)); a prompt file also gets `prompt.abs` — `~` expanded on that host — `prompt.root` — its git checkout, else its directory — and editor links built like the notes': `prompt.editorUrl` opens the file (at line 1, so a remote path opens as a file), `prompt.rootEditorUrl` its root; null without an editor or ssh alias); 501 when that host's CLI predates `fleet jobs` |
| GET | `/api/hosts/:host/jobs/:name` | one job with every kept run, newest first, each run's sessions with `parent`: `fleet --local jobs show <name> --json` + `host`; 404 no such job. Names: `[A-Za-z0-9_-]{1,40}` (else 400) |
| POST | `/api/hosts/:host/jobs/:name/run` | `{ force? }` → `{ host, name, output }`: `fleet --local jobs run [--force] <name>`. Starts an agent (spawn's trust level); 409 `{ error, busy: true }` while a session of the job is working, unless `force` |
| POST | `/api/hosts/:host/jobs/:name/enabled` | `{ enabled: boolean }` → `{ host, name, enabled, output, job }`: `fleet --local jobs enable\|disable <name>`, then the job as `show` reports it |
| GET | `/api/hosts/:host/schedule` | `{ host, entries }`: `fleet --local schedule --json` — pending and recent scheduled sessions ([CLI](cli.md#scheduled-sessions)) |
| POST | `/api/hosts/:host/schedule` | `{ at \| task (+ time), dir, prompt?, name?, model?, harness?, ghIdentity? }` → `{ host, entry }`: `fleet --local schedule add`. `dir` must be in the host's spawn dirs, like a spawn; starts an agent later |
| POST | `/api/hosts/:host/schedule/:id/cancel` | → `{ host, id, output }`: `fleet --local schedule cancel <id>` |
| POST | `/api/hosts/:host/repos/sync` · `/repos/service` | `{ names? }` → `fleet --local repos sync --json` (409 while one runs); `{ install }` → `fleet --local repos install-service [--uninstall]` |
| POST | `/api/hosts/:host/sessions/:id/sleep` | `{}` → `fleet --local sleep <id> --json` (ends it, keeps it dormant), then closes its iTerm tab if it had one |
| POST | `/api/hosts/:host/sessions/:id/kill` | `{}` → SIGTERM (then SIGKILL) Claude, then kill its tmux session (or just its window when the session has others) / close its iTerm tab |
| GET | `/api/groups` | the Board view's groups: `{ enabled, host, intervalMinutes, running, updatedAt, lastRun, groups: [{ id, label, description, source, members: [{ host, id, dormant? }] }] }` (`dormant: true` only on a member that is dormant on its host; never set from stored groups, so the UI also cross-references `GET …/dormant`); served by the grouping host, proxied by every other server (`enabled: false` when nobody runs it) |
| GET | `/api/inbox` | the [inbox](#inbox): `{ enabled, host, error?, updatedAt, brief: { text, at, model } \| null, sources: { github?, productive?, sessions?, comms? : { at, ok, error, count, nextAt?, running? } }, items: [{ id, source, kind, section, title, ask, url, href, from, at, firstSeen, effort, meta }] }` — served by `inbox.host`, proxied by every other server (`enabled: false` when none is set) |
| GET | `/api/slack` | [Slack links](#slack-links): `{ enabled, host, error?, socket: { connected, error }, bot: { userId, team, url }, owner, links: [Link], ended: [Link] }` — Link = `{ id, host, session, sessionName, channel, threadTs \| null, permalink, task, mode, createdAt, until, pending, delivered, replies, endedAt, endReason }`; served by `slack.host`, proxied by the others (`enabled: false` when none is set) |
| POST | `/api/slack/link` | `{ host, session \| pane \| tmuxSession, url \| user, minutes (1–2880, or like `2h`, `2d`), task?, mode?: mention \| proactive, first? }` (`user`, or a `url` that is not `http…`: a person — `@name` / name / `U…` — and the link is the app's DM with them; the Link then has `dm: { user, name }`) → Link (`renewed: true` when that session already followed that thread). 400: a bad link or duration, a session on another host, a conversation the app cannot see; 404: no such session |
| POST | `/api/slack/links/:id/unlink` | end the link now → Link |
| POST | `/api/slack/reply` | `{ link, text, thread?, pane? \| session? }` → `{ ok, link, channel, ts, threadTs }`: post in the linked thread. 403: `pane`/`session` is not the linked session; 410: the link has ended |
| POST | `/api/inbox/refresh` | Scan now: start a Slack + mail scan (also outside work hours; its brief skips the `briefMinutes` wait) and re-read every other source; answers the GET shape |
| POST | `/api/inbox/dismiss` | `{ id }` → the item is done: off the list until its source drops it — a Slack / mail item, a session or a PR mention comes back on newer activity; answers the GET shape (404: no such item) |
| PUT | `/api/inbox/settings` | `{ scanMinutes?, workHours?, workDays? }` (15–1440 min; `[from, to)` hours; weekdays 0–6) → `fleet config set inbox.<key>` on the inbox host, live at once; answers the GET shape, whose `settings` carries the three |
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
| GET | `/api/hosts/:host/sessions/:id/diff[?base=<ref>&since=<fingerprint>]` | the diff reviewer: the session's worktree (tracked edits + untracked files) against the merge-base of HEAD with `base` (default branch by default) → `{ host, id, root, branch, base: { ref, sha }, tree, fingerprint, unchanged, files: [{ path, oldPath, status, additions, deletions, hunks: [{ …, lines: [{ t, o, n, text }] }] }], stats, truncated }`; with `since` equal to the current `fingerprint` → `unchanged: true` and no `files` (cheap polling). 409 outside git |
| GET | `/api/hosts/:host/sessions/:id/diffs[?since=<fingerprint>]` | the diff reviewer over **every checkout the session works in**: its cwd's, then the checkouts holding the files it worked on (tool inputs from its transcript, most recent first, at most 12, never `$HOME`) — a session in a meta repo editing several worktrees sees them all → `{ host, id, cwd, fingerprint, unchanged, repos: [{ root, label (relative to the cwd when inside it, else `~/…`), branch, head, base, tree, fingerprint, files, stats, truncated } \| { root, label, error }] }`; `since` = the combined fingerprint → `unchanged: true` without `repos` |
| GET | `/api/hosts/:host/sessions/:id/review` | the diff reviewer's comment threads → `{ host, id, threads: [{ id: "c-…", path, side: new\|old, start, end, code, status: draft\|sent, resolved, reviewId, entries: [{ body, sentAt, state: draft\|sent\|queued\|working\|done, replies: [{ text, ts, final }] }] }], reviews, drafts }`; every comment / reply was typed into the session with its marker `[review c-…]`, and the replies are read back from the transcript (the assistant messages after the marked prompt, up to the next prompt) and kept in `<state>/fleet/reviews/<session_id>.json` on the session's host |
| POST | `/api/hosts/:host/sessions/:id/review/threads` | `{ path, side, start, end, code: [lines], body, draft }` (+ `root`: the checkout, absolute — the prompt names the file relative to the session cwd, `display`) → a thread; `draft: false` types the comment (the lines quoted, fenced) into the session now, `draft: true` keeps it for submit → `{ …, thread }`. Types into the agent like `send` |
| POST | `/api/hosts/:host/sessions/:id/review/threads/:thread/reply` | `{ body }` → a follow-up typed into the session with the thread's marker; 409 on a draft |
| PUT / DELETE | `/api/hosts/:host/sessions/:id/review/threads/:thread` | `{ body?, resolved? }` (body: drafts only, else 409) / remove the thread from the review (what was sent stays in the conversation) |
| POST | `/api/hosts/:host/sessions/:id/review/submit` | `{ body? }` → every draft in one prompt (the overall note, then each comment with its marker and lines); 409 without drafts |
| GET / POST | `/api/hosts/:host/sessions/:id/queue` | the prompt queue → `{ host, id, items: [{ id: "q-…", text, createdAt }], paused, held }` / `{ text }` → adds one (`{ …, item }`; 409 past 20). Queued prompts go in one per run, only once the session is `idle` and its `updated_at` has moved past the last delivery (never while `waiting`); kept in `<state>/fleet/prompt-queue.json` on the session's host. The `messages` response carries the same `queue` (null when empty) |
| POST | `/api/hosts/:host/sessions/:id/queue/pause` | `{ paused }`: a pause holds the queue for the run under way; when that run ends it clears itself and the queue is `held` until the next run ends. `paused: false` resumes at once (the next idle sends) |
| PUT / DELETE | `/api/hosts/:host/sessions/:id/queue/:item` | `{ text }` edit / remove one |
| POST | `/api/hosts/:host/sessions/:id/queue/:item/send` · `…/:item/move` | type it in now, whatever the session is doing / `{ to }` new index |
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
| POST | `/api/hosts/:host/stacks/:id/add` | `{ session, host? }` (a full session id; `host`: the peer it runs on, default `:host`) → put an existing live session in that stack after the fact: its row from that host's list, then `fleet --local stack add <id> <session> [--on <host> --input -] --json` → `{ host, stack: StackView, created: false, generated: false }`. Already a member → the stack unchanged; 404 not live there; **409** already in another stack; 400 bad body or a host that is not a peer with a web url |
| POST | `/api/hosts/:host/sessions/:id/stack/link` | `{ session, host?, label? }` → link two existing sessions: `fleet --local stack ensure <session_id>` (the session's stack — created when it has none, timeout 150 s: it may call the model), then adds `session` as `…/stacks/:id/add` does → `{ host, stack, created, generated }`; 400 linking a session to itself |
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

## Embed view

`#/embed?host=<host>&dir=<dir>[&theme=dark|light][&app=<name>]` is Fleet's session chat for one directory,
made for another app to frame (an `<iframe>` panel: Fin's assistant, later Brain) instead of
each app writing its own chat UI. It is a separate tree in the web UI (`screens/EmbedScreen.tsx`,
`lib/embed.ts`): no sidebar, no Fleet navigation, no scratchpad, stacks or diff reviewer.

- **List**: the live sessions on `host` whose cwd is `dir` or beneath it (worktrees inside the
  repo), latest first, and **New session**: the New session form with host and directory fixed
  (agent, model, first prompt, attachments, voice as usual) → `POST /api/hosts/:host/spawn`.
- **Session**: the mobile session screen (`#/s/:host/:id`, chat, composer, AskUserQuestion
  answers, attachments, voice, the details drawer with Close) with Back to the list. Sessions on
  other hosts are not opened.
- **`dir`**: absolute or `~/…` (expanded with the host's `home`, now part of each `/api/fleet`
  host; an older server matches a spawn dir with the same tail). It must be one of that host's
  `spawnDirs` (offered or `hiddenSpawnDirs`) or beneath one, else the view shows an error and offers no spawn — the same rule
  the server applies to `/spawn`.
- **`app`**: the framing app's label (`[A-Za-z0-9_-]{1,40}`, default the dir's last folder).
  Sessions spawned from the view carry it (`embed` on their `list` row): Fleet's own list sets
  them aside like bot sessions — out of every status filter but **Apps** (and their app chip on
  rows / cards), except while one waits on you. A session restored after a reboot loses it.
- **`theme`**: the framing app's mode; without it the OS preference. It never changes the
  viewer's Fleet theme setting.
- **URL form**: the hash router moves a route's query into the page's query string, so the
  canonical URL is `https://<fleet>/?host=…&dir=…[&theme=…]#/embed` (`#/embed?…` works and is
  rewritten to it). The query stays while the frame moves to `#/s/…`, which is how a reload
  inside the frame stays in the embed view. Every move inside the view replaces the history
  entry, so the framing page's Back button stays its own.
- **Framing**: the server sends no `X-Frame-Options` / `frame-ancestors`, so any page that can
  reach it can frame it — like the rest of the API it relies on a private network (no auth).
  The frame talks to its own origin only (same-origin API calls; no cookies), so nothing
  cross-origin is needed. Voice needs the frame to allow it: `allow="microphone"`. There is no
  `postMessage` contract.
- **Desktop shell**: a profile app frames it only with the Fleet origin in its
  `frameOrigins` ([desktop.md](desktop.md#other-apps-profiles)).

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
speech key (or after it fails), the device's own voice (Web Speech API). A reply is read as soon
as its final message is in the transcript (Claude writes a message only once it is complete), not
when the status turns idle, and it is voiced in pieces (`speechChunks`): a short first piece of one
or two sentences, then pieces of up to ~500 characters, all requested at once and played in
order — the voice starts in about a second instead of after the whole reply is synthesized
(roughly 1 s per 90 characters). The brief points the operator at `./api GET /fleet` (the server's
snapshot of every host, instant; an unreachable host is `ok: false`) rather than
`fleet list --all-hosts` (ssh to each host, which waits out the connect timeout on a machine that
is asleep, and sees only hosts with an ssh destination), gives one-command recipes for the common
questions, and keeps replies under ~400 characters. Measurements, and what running it on Codex or pi would take:
[operator.md](operator.md). Sessions that start
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

## Inbox

One live to-do list for the whole day, kept by the system: every item links to the thing that
waits on you, and drops off by itself once its source says it is done — or at once when you mark
it Done (dismissals are kept in the state file; the scan is told which messages are done)
(`web/lib/inbox.mjs`, the `#/inbox` screen). One host's server runs it (`inbox.host`, best the
always-on one); the others proxy `/api/inbox` there.

| Source | How | Model |
|---|---|---|
| `github` | `fleet --local prs --json` every 2 min: review requests, unseen @mentions, your PRs approved / with changes requested / with failing CI | none |
| `productive` | `fleet --local tasks --json`: overdue and due-soon tasks, tasks in progress touched in the last 3 days | none |
| `sessions` | the merged fleet: sessions on any host with status `waiting` (link `#/s/<host>/<id>`) | none |
| `comms` | Slack + Gmail through the claude.ai connectors: one `claude -p --output-format json` with only read-only connector tools allowed (writes and built-ins denied), every `scanMinutes` within work hours. It looks back to the previous scan minus 2 h (3 days the first time) and is handed the items it returned last time, so their ids stay and answered threads drop off | `model` |

Sections: `quick` (quick wins — a review of ≤ 150 changed lines in ≤ 6 files, an approved PR to
merge, a waiting session, a message the scan rated `quick`), `waiting` (on you: other reviews,
mentions, changes requested, failing CI, overdue tasks, other messages), `mine` (your work: due
tasks, recent tasks in progress). Items are sorted by section, then newest first.

The **brief** (3–8 bullets in `language`) is one `claude -p` over the numbered item list; it links
what it names as `[name](#<n>)`, which the server turns into the item's url (else its in-app
href) — no URL comes from the model. It runs only when the
set of items changed, at least `briefMinutes` apart, and outside work hours only the first of the
day. A failing source keeps its last list and reports the error; a failed scan waits for the next
slot. State (`<fleet state>/inbox.json`): the scan's items and time, the brief, when each item was
first seen (the UI's "new" dot).

## Slack links

A session, for a while, talks with the people in one Slack thread / channel / DM (`web/lib/slack.mjs`,
[CLI](cli.md#slack-links), Details → Slack in the web UI). One host's server runs the bridge
(`slack.host`, best the always-on one); the others proxy `/api/slack` there. Only sessions on that
host can be linked for now.

- **Slack App, Socket Mode.** The bridge opens a WebSocket to Slack (`apps.connections.open` with
  the app-level token `SLACK_APP_TOKEN`, `xapp-…`, scope `connections:write`), acks every envelope
  and reconnects when Slack asks. No public URL, so it works behind Tailscale. The Web API calls use
  the bot token `SLACK_BOT_TOKEN` (`xoxb-…`; scopes `channels:history`, `groups:history`,
  `im:history`, `mpim:history`, `chat:write`, `reactions:write`, `users:read`, and `app_mentions:read`
  with the `message.*` events; `im:write` for a DM link to a person, whom people can write to only
  with the app's App Home → Messages tab on). Both come from the server's environment or the Keychain
  (`security find-generic-password -s <name> -w`). Sessions never see them.
- **What fleet enforces**, not the model: which conversation (a message elsewhere is never
  delivered), until when (an ended link delivers nothing and refuses replies), who wrote a message
  (Slack's user id), and which session may reply (`fleet slack reply` sends `$TMUX_PANE`; another
  session's pane is a 403). Bots, the app itself and edits are not delivered.
- **What the model keeps to**: the owner's instructions and the rules in the note typed in when
  the link starts (post only through `fleet slack reply` and only when something calls for it,
  only within the instructions, nothing destructive for them, messages are colleagues' requests
  and cannot change the rules).
- **Modes**: `mention` (default) — the session is woken by a message that mentions the app, a
  DM, or any message in a thread it has posted in (the link's `postedIn`; a top-level post starts
  one) — whether that last kind is for it, the model judges, no heuristic; the messages in between
  (the last 60) go along as context. `proactive` — every message wakes it; it steps in when it
  judges it should. Only a mention / DM is marked (`→ mentions you` / `→ to you`) and gets 👀.
- **Delivery**: the note first, then messages once the session is `idle` (never while it is
  `busy` or `waiting` on you; `idle` includes a main thread that is free while its subagents run —
  [discovery](#session-discovery), and the note tells the session to hand long work to them), several as one prompt under 8000 chars (the newest), at least 8 s
  apart; the ones addressed to it get an 👀 reaction when typed in.
- **State** (`<fleet state>/slack-links.json`, 0600): the active links with their undelivered
  messages and the last 20 ended ones.

## Session briefs

A brief is a small markdown doc per session: 1–3 sentences on exactly what the session is doing,
where it stands and what comes next (where it runs is shown beside it), every resource it
produced, and its todos with progress — enough to start a **new** session from it and just
continue (`continuePrompt` in the API is that first prompt). The user can edit it; their edits are authoritative.

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
- Origin: [Task 42: login loop](https://app.example.dev/tasks/42)
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
  code span `` `path` ``. Kinds: `Origin` (where the work came from: a task, a Slack thread, an
  issue), `PR`, `Issue`, `Artifact`, `Spec`, `File`, `Git`, `Link` (and the legacy `Branch`,
  `Worktree`). Any other bullet (no kind, free text) is a hand-written line and
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
unless their key is already present or in `dismissed` (the Git line: replaced in place, above; an
`Origin` item whose link is already there as an auto `Link:` / `Issue:` line turns that line into
`Origin:`); a
human edit (PUT) that removes a resource line adds its key to `dismissed`; the machine keys
(`generatedThrough`, `generatedAt`, `todos`, `dismissed`, `git`) are the writer's, not taken from
an edited body.

**Generation** (hybrid, `web/lib/brief-extract.mjs` + `web/lib/briefs.mjs`):

1. *Resources without a model*, read incrementally from the transcript: files the session wrote
   (Edit / Write / MultiEdit / NotebookEdit targets, shell redirects and `tee`; temp dirs skipped;
   `SPEC.md`, `FINAL.md` and files under `specs/` are `Spec`), PR/issue URLs printed by
   `gh pr|issue create`, artifact URLs returned by an Artifact publish, PR/issue/artifact/other
   links in the assistant's text, PR/issue/artifact links in the user's prompts; `Origin` = the
   other links (issue, task, Slack thread, …) of the session's **first** prompt, plus the `Origin:`
   list of a brief that prompt hands over (so the origin carries down a chain of continued
   sessions — as do the handed brief's PR links); the Git line
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

**Continue prompt** (`continuePrompt` in `web/lib/brief-format.mjs`, `continue_prompt` in
`core::brief`; byte-identical, pinned by `testdata/briefs/sample.continue.txt`) — the first prompt
of a new session that picks the work up. It carries only what the next session needs to find the
work, not the session's detail:

```text
Continue the work of session <id> on <host> in <cwd>. Its brief:

<Summary>

Origin:
- [Task 42: login loop](https://app.example.dev/tasks/42)

PRs:
- [owner/repo#12](https://github.com/owner/repo/pull/12)

Worktree: `~/Code/project-wt` (branch `fix-login`)

---
```

`Origin:` / `PRs:` / `Worktree:` (a linked worktree only) are left out when empty; files,
artifacts, links and todos are never included. The **task** for the new session goes below the
`---` line (the UI puts the caret there; `fleet spawn --from <s> "<task>"` appends it); sent with
nothing below it, the task is `Pick up where it left off. Check the current state (git status, the
PRs above) before changing anything, and tell me briefly what you found first.` Extraction treats
the text below the separator as the user's own prompt and the part above it as the handed brief.

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

### Slept sessions

`fleet sleep` (Deactivate in the app, whose dormant list is called Inactive; `core::sleep`) is a Close that keeps the session dormant. It
records the snapshot, ends the agent and its tmux window/session, records again (the session now
lingers as just ended) and `snapshot::put_to_sleep` moves that entry — the whole tmux session when
it went with it, else the lone Claude session — into `dormant`, `since` = now, `slept: true`. From
there it is like any dormant entry (restore, `enter`, forget, groups, stacks), except that
`restore --all` (and so `restore.onBoot`) skips it.

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
