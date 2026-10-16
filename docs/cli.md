# CLI reference

`fleet <command> [args]`. `fleet --help` and `fleet <command> --help` are authoritative; this page
explains the model and the semantics behind them.

## Global options

| option | meaning |
| --- | --- |
| `-H, --host <name>` | run against that host from the config (see [dispatch](#host-dispatch)) |
| `--local` | run on this machine, never over ssh |
| `-n, --dry-run` | print what would run, run nothing (also `FLEET_DRY_RUN=1`) |
| `--gh-identity <name>` | sessions this command spawns act on GitHub as that [GitHub identity](#github-identities), not as you (also `FLEET_GH_IDENTITY`) |
| `--ignore-load` | start the session even while the machine is under heavy load (also `FLEET_IGNORE_LOAD=1`). With `resources.enabled` in the config, the web server watches the machine's memory pressure and CPU load; while it is `hot` or `critical`, `spawn`, `handoff`, `pr` / `task` / `stack spawn`, `restore` and `bots run` refuse with "`<host> is under heavy load (…) — new sessions wait until it recovers; add --ignore-load to start it anyway`", and `schedule tick` / `bots tick` hold the round (due entries fire once it recovers). See [Host load](architecture.md#host-load) |
| `--no-fallback` | start on the asked-for harness even when its subscription is at its limit (also `FLEET_NO_FALLBACK=1`). With `usage.enabled` in the config, `spawn`, `handoff`, `pr` / `task` / `stack spawn` start on the fallback harness instead (a `usage: claude → codex: …` note on stderr); bots and scheduled sessions do the same, and wait when the fallback is at its limit too. See [Subscription fallback](architecture.md#subscription-fallback) |
| `--json` | machine-readable output, where the command supports it |
| `-h, --help`, `-V, --version` | |

Global options are accepted before or after the subcommand (`fleet -H workstation list` =
`fleet list -H workstation`).

### Host dispatch

The target host is `-H`, else `FLEET_HOST`, else a default: `defaultHost` from the config for
tmux-session commands (`tmux …`, `enter`, `last`, `new`, `exec`, `ssh`), this machine for everything
else. `self`, `local` and `localhost` always mean this machine. If the target is another machine,
`fleet` runs `fleet --local <same args>` there over `ssh <hosts.<name>.ssh>` (with a tty when both
stdin and stdout are terminals). A name that is not in the config is used as an ssh destination
as-is, so `fleet -H some-ssh-alias tmux list` works before `fleet init`.

On the remote, `fleet` is looked up as `hosts.<name>.fleetBin`, then `~/.local/bin/fleet` (where
`fleet install` puts it), then `PATH`. `--dir` / `-C` values under your `$HOME` are rewritten to
`~/…` so they mean the same place on the other machine; relative paths are refused.
`FLEET_CMD`, `FLEET_JOB_RUN`, `FLEET_GH_IDENTITY`, `FLEET_DEBUG` and `FLEET_DRY_RUN` are forwarded (exported, single-quoted, in the
remote `sh -c`); `FLEET_CONFIG` is not — the remote uses its own config.

## Claude sessions

`<target>` is a session's generated title, its Claude session name, a session-id prefix, or a pid.
Any unambiguous fragment works: matching tries exact → prefix → substring → characters-in-order and
the first rung with hits wins. Two hits on the same rung is an error that lists the candidates.

| command | does |
| --- | --- |
| `fleet` | `watch` on a terminal; a one-shot `list` when stdout is piped |
| `fleet list [--json] [-a, --all-hosts] [--mine \| --shared]` (alias `ls`) | live sessions: title, status, age, context usage (`ctx 62%`), terminal, cwd. `--json` is an array whose rows all carry `host`; `-a` queries every configured host in parallel (its JSON rows also carry `shared`: their host's `hosts.<name>.shared`); `--mine` / `--shared` do the same over only the own hosts / only the shared sheds. The text view ends with `N dormant (fleet restore)` when a reboot left [dormant sessions](#session-recovery). Every local `list` also records the [snapshot](#session-recovery) |
| `fleet peek <target> [--lines N]` | what the session's terminal shows now (default 40 lines) |
| `fleet send <target> <text>` | type text into the session and press Enter |
| `fleet done [--note <text>] [--cancel]` | run by an agent **inside its own session** (by `$TMUX_PANE`; a Codex session by `$CODEX_THREAD_ID`, since Codex runs commands through a shared daemon whose `$TMUX_PANE` is another pane's; never sent over ssh): its work is finished and nothing waits on the user. Fleet shows the session as done (nothing closes it) — see [Ask done](#ask-done). `--cancel` takes the mark back |
| `fleet snooze <target> (--for <30m\|4h\|1d> \| --until <time> \| --clear) [--json]` | set a session aside until a time — see [Snooze](#snooze). It keeps running; the views move it to **Snoozed** and bring it back, marked, once the time has passed. `--until`: `"2026-11-13 09:00"` (local time), an RFC 3339 time, or epoch ms. `--clear` ends the snooze (or drops the back mark). `--json`: `{ host, sessionId, title, cleared, snooze: { at, until } \| null }` |
| `fleet sleep <target> [--json]` (alias `deactivate`) | close a session but keep it [dormant](#session-recovery) (Deactivate in the app, which lists dormant sessions as **Inactive**): SIGTERM (then SIGKILL) its agent, kill its tmux window (or its tmux session when that was the only window; an iTerm tab is left — the app closes it), and put it in the dormant list, marked `slept`, instead of recently closed. `fleet restore <target>` (or `enter`, or Resume in the app) brings it back with `--resume`; `--all` and `restore.onBoot` leave it be. `--json`: `{ host, sessionId, title, process, terminal, dormant: DormantView, dryRun }` |
| `fleet ask-done <target>` | ask a tmux session to report itself done once finished: types the [instruction](#ask-done) into it. A session waiting on a prompt is refused |
| `fleet rename <target> <title> [--no-tmux-sync] [--force] [--json]` | rename the session's one title: Claude's `/rename` (the source of truth), then its tmux session follows as a slug of it (`Fix Login` → `fix-login`, `-2` on a collision) when the tmux session is that Claude session's own (one window, one pane). A session **waiting on a prompt** is held (exit 1; `--json`: exit 3) — typed keys would be its answer; busy sessions are fine (Claude runs `/rename` mid-turn without disturbing the turn). Codex gets `/rename`, pi `/name`; a busy Codex / pi session is held too (`held: "busy"`) — they would queue the text as a message. `--force` overrides the hold. `--json` prints `{ ok, result: renamed\|sent\|held, session_id, pid, host, from, title, held, tmux: { renamed, from, to, note }, message }` |
| `fleet name [<target> \| --all [--settled]] [--apply \| --adopt-tmux] [--refresh] [--no-tmux-sync]` | suggest a name from what the session is doing and store it as the session's display title (the name cache); only `--apply` sends it as `/rename` (through the same path as `rename`, tmux included). `--adopt-tmux` sends only the adopted tmux names below, never a generated one (the web's periodic pass). A title the brief generator wrote (source `brief`) is served as is, no model call, until `--refresh`. `--all` = every session with a derived (cwd+hash) name. A derived-name session alone in a tmux session somebody named (`fleet new fix-login`) adopts that name (`(tmux)`, no model call) instead of getting a generated one. `--settled` (with `--all`) skips sessions with one prompt and no brief yet — the model reads the brief's Summary and the first three prompts, and a lone first prompt is often just a link. `--refresh` ignores the cache |
| `fleet group [-a, --all-hosts] [--json] [--apply] [--refresh] [--consolidate] [--input <file\|->] [--cached]` | sort sessions into work-stream groups (the web Board view) — see [Grouping](#grouping). Reads sessions and asks `claude -p`; never sends anything to a session |
| `fleet group --rename <group id> --label <l>` / `--move <host/id> (--to <group id> \| --label <l>)` / `--create --label <l>` / `--delete <group id>` | edit the stored groups by hand (the Board's rename / drag'n'drop) — see [Grouping](#grouping) |
| `fleet spawn [prompt] --dir <path> [--harness claude\|codex\|pi] [--name <n>] [--model <id>] [--backend iterm\|tmux] [--tmux-session <s>] [--window] [--json]` | start a new agent session in a new tab/pane. `--json`: one object saying where it landed, or why it did not — see [Spawning](#spawning) (not with `--slack`). `--harness` picks the coding agent (default `claude`; see [Harnesses](#harnesses)). A Codex spawn in tmux answers "Trust this folder?" with "Trust and continue" (the directory was picked explicitly), as the web spawner does. `--model <id>` runs `<harness> --model '<id>'` (letters, digits and `._[]-/:` only — pi's ids are `provider/model`; blank = the harness's default). `--name` is passed as `-n` to Claude and pi; Codex has no such flag (the tmux session carries the name). Default backend: tmux inside tmux, over ssh or off macOS; else iTerm. With tmux, each spawn gets its own tmux session (one session per job) named from `--name` or the dir's basename, sanitised like `fleet new`; a taken `--name` is an error, a taken basename is uniquified (`app-2`). `--tmux-session <s>` opens a window in `s` instead (created if missing). `--ask-done`: the [instruction](#ask-done) goes below the prompt and the session runs in tmux. `--slack <slack-url \| @person> [--slack-for 2h] [--slack-task <instructions>] [--slack-proactive]`: start [linked to Slack](#slack-links) (tmux, any host whose web server reaches `slack.host`; checked before anything opens). The session starts with no prompt and is linked once it has registered (≤ ~45 s); the prompt goes in last in the link's intro, as the owner's request to start on — so it never works on it, or reaches Slack another way, before it knows it is linked. Max 4000 chars. If the link fails, the session stays, idle, and the prompt was not sent |
| `fleet harness list [--json] [--no-models]` | the [harnesses](#harnesses) on this host: id, label, launch command, `installed`, and the models each offers for a picker. `-H <host>` asks another machine |
| `fleet spawn --from <target> [prompt] [--name <n>] [--model <id>] [--backend …] [--json]` | continue a session's work in a new one: same host (with `-H`, the host the session is on) and its cwd (`--dir` overrides), first prompt = its [brief](#briefs)'s continue prompt (summary, origin, PRs, worktree, then a `---` line), then a blank line and `prompt` — the task (default: pick up where it left off). The prompt goes through a file under `~/.claude/fleet-handoffs/`, like a handoff. Errors when the session has no brief yet. `-n` prints the launch command and the prompt and writes nothing |
| `fleet handoff [brief] [--file <f\|->] --dir <path> [--harness <h>] [--name <n>] [--model <id>] [--tmux-session <s>] [--tab] [--no-wait]` | start a new session in another window seeded with a brief (saved under `~/.claude/fleet-handoffs/`); waits until it registers. Same tmux placement as `spawn` |
| `fleet brief <target> [--json \| --prompt]` | print the session's [brief](#briefs) — its body (Summary / Resources / Todos, no frontmatter); `--json` the parsed shape; `--prompt` only the continue prompt |
| `fleet brief <target> --edit` / `--set [--expect-updated <iso>]` / `--regenerate` / `--open` | edit it in `$VISUAL`/`$EDITOR`; save markdown from stdin; ask the web server to regenerate it; open the session's checkout in VS Code — see [Briefs](#briefs) |
| `fleet stack list\|show\|new\|ensure\|spawn\|add\|remove\|rename\|edit\|set\|sync\|rm …` | [session stacks](#stacks): sessions that share one context file (the StackBrief) |
| `fleet stack spawn <target> [prompt] [--harness <h>] [--name <n>] [--model <id>] [--dir <path>] [--on <host>] [--no-wait]` | spawn a **sibling** of `<target>`: a new session in its cwd (`--on`: on another host), in its stack (created around it first when it has none), told where the StackBrief is — see [Stacks](#stacks) |
| `fleet watch [--interval 5] [--stuck 300] [--quiet] [--rows 1\|2\|auto] [--no-mouse]` | live dashboard + notifications on finished/stuck sessions |
| `fleet usage [--json] [--refresh]` | the Claude subscription's usage limits on this host's login — current session (5h), weekly all models, weekly per model (e.g. Fable), extra usage — as bars with reset times; the numbers Claude Code's `/usage` shows. Then the ChatGPT plan Codex (and pi) bill against — current session, weekly, credits — when Codex is logged in with ChatGPT, as Codex's `/status` shows it. Cached 60s (`--refresh` skips it); a failed read serves the last one marked stale and backs off 2 minutes. `--json`: see [architecture](architecture.md#subscription-usage). `-H <host>` reads another machine's login |
| `fleet usage check [--harness claude\|codex\|pi] [--model <id>] [--ctx manual\|auto\|bot] [--json]` | where a new session would start now: each subscription's state, the fallback, and the harness it starts on or why it would wait ([Subscription fallback](architecture.md#subscription-fallback)). `--ctx bot` adds the weekly pace check |
| `fleet usage override <minutes>` | turn the subscription fallback check off on this machine for up to 1440 minutes (0 ends it) |
| `fleet skill install [--force]\|show` | install / print the Claude Code skill (`~/.claude/skills/fleet/SKILL.md`; `--force` overwrites a different one) |

`send`, `rename --force`, `name --apply` and `spawn` change a live agent's state — scripts and agents
should confirm before running them.

A machine whose config has `"spawn": false` (an always-on server that runs no agent sessions; see
[architecture](architecture.md#config)) refuses every command that starts an agent there — `spawn`,
`spawn --from`, `handoff`, `stack spawn` (unless `--on` names another host), `pr spawn`, `task spawn`,
`schedule add`, `bots run` and `restore <target> | --all` — with `<host> does not run agent sessions
(config spawn: false)`, also when reached with `-H <host>` (the remote CLI reads its own config).
tmux-only commands (`new`, `tmux …`) still work.

### Spawning

Before anything opens, `spawn` checks that the directory exists, the name and model are valid,
the tmux session name is free, and the launcher can start: its first word must be an executable
path, or a command found on `PATH`, in the usual install dirs or — failing those — by
`command -v` in a login shell of `$SHELL` (a launcher it cannot judge, such as `VAR=1 claude`,
goes ahead as before). It is judged from `--dir`, where the shell will run it: `./agent` is the
`agent` in that directory, and the login shell starts there.

`spawn --json` (also with `--from`) prints one object on stdout. On success (exit 0):

```json
{ "ok": true, "dry_run": false, "host": "laptop", "backend": "tmux",
  "tmux_session": "auth-fix", "tmux_window": "auth-fix:0", "handle": "%12",
  "created_session": true, "dir": "/work/project", "harness": "codex",
  "model": "gpt-6", "name": "auth-fix", "command": "cd '/work/project' && codex --model 'gpt-6' 'fix the login'",
  "prompt_file": null, "from": null }
```

| field | notes |
| --- | --- |
| `host` | this machine's name, as in `list --json` |
| `backend` | `tmux` \| `iterm` — the one used (inside a [job](#jobs) run an iTerm spawn lands in tmux; a dry run reports the same) |
| `tmux_session`, `tmux_window` | where it runs (`session:index`); `null` for iTerm |
| `handle` | the pane id (`%12`) or iTerm session id — the `handle` `list --json` will report for the session once it registers; `null` when iTerm gave none |
| `created_session` | a new tmux session was made for it (`false`: a window in an existing one, `--tmux-session`) |
| `dir`, `harness`, `model`, `name` | as resolved (`model` / `name` `null` when not given) |
| `command` | the shell line typed into the new terminal |
| `prompt_file` | the file the first prompt was read from (a long prompt, `--from`), else `null` |
| `from` | `--from`: `{ session_id, label }` of the session it continues, else `null` |
| `dry_run` | `-n`: nothing ran; `command` is what would be typed and the terminal fields are `null` |

On failure the exit code is 1 (unless noted below), stderr carries the message as usual, and
stdout carries `{ "ok": false, "host", "kind", "error", "partial" }`. `partial` is `null` unless
the terminal was created before the spawn failed (typing the launch line into it failed): then it
says where that terminal is — `{ backend, tmux_session, tmux_window, handle, created_session,
command, prompt_file }` as above (iTerm: only `backend` and `handle`) — so the caller can find it
and close it. `kind` is one of:

| kind | when |
| --- | --- |
| `dir_missing` | `--dir` does not exist (with `--from`: no working directory known) |
| `name_invalid` | `--name` is empty, multi-line, or empty after sanitising for tmux |
| `name_taken` | a tmux session already has that `--name` |
| `model_invalid` | `--model` has characters outside the allowed set |
| `harness_missing` | the launcher cannot start on this host |
| `no_brief` | `--from`: the session has no brief to continue from |
| `backend_failed` | tmux or iTerm refused (the message carries what it said); see `partial` |
| `host_unknown` | `-H`: the host cannot be resolved |
| `dir_invalid` | `-H`: `--dir` is not absolute or `~/…`, so it cannot name a path there |
| `host_unreachable` | `-H`: ssh could not connect (exit 4) |
| `fleet_missing` | `-H`: fleet is not installed there (exit 127) |
| `remote_timeout` | `-H`: no answer within 90 s |
| `remote_failed` | `-H`: the host answered with something that is not a JSON object (an older fleet) |
| `ssh_failed` | `-H`: `ssh` could not be started at all (not installed, not on `PATH`) |
| `io`, `json`, `other` | anything else |

With `-H <host>` the object comes from that host's own `spawn --json` (its exit code too), with
`host` the name this machine knows it by; only a failure to get that answer is reported here.
`-n -H <host>` contacts nothing and prints `{ "ok": true, "dry_run": true, "host", "remote": true,
"command" }`, `command` being the ssh line that would run.

New kinds may be added; treat an unknown one like `other`. The session itself starts after
`spawn` returns: it shows up in `list --json` once its agent has started (Codex and pi: once the
process is up; Claude: once it has registered).

### Harnesses

fleet drives three coding agents: **Claude Code** (`claude`, the default), **Codex** (`codex`) and
**pi** (`pi`). Every session row carries `harness`; `spawn`, `handoff` and `stack spawn` take
`--harness`. Codex and pi have no live registry like Claude's, so they are found with `ps`
(interactive runs with a terminal — `codex exec`, `pi -p` and the like are not sessions) and
matched to their transcript by cwd and start time (see
[architecture](architecture.md#other-harnesses)). A Codex session sitting on an approval prompt is
`waiting` (`waiting_for`: `exec approval`, `patch approval`, …): Codex does not record the
prompt, so fleet reads it off the tmux pane. Peek, send, keys, kill, rename (`/rename` in
Codex, `/name` in pi — held while the session is busy) and restore (`codex resume <id>`,
`pi --session <id>`) work the same; briefs, naming and the web's question answering are
Claude-only for now.

Launch commands: Claude's is `FLEET_CMD` / config `claude`; the others' are config
`harnesses.<id>.command` (default the bare name). `fleet harness list --json`:

```json
[{ "id": "codex", "label": "Codex", "command": "codex", "installed": true,
   "models": [{ "id": "", "label": "Default (gpt-6.1-sol)" }, { "id": "gpt-6.1-sol", "label": "GPT-6.1-Sol" }],
   "efforts": ["low", "medium", "high", "xhigh", "max"], "defaultModel": null, "defaultEffort": "high" }]
```

Models: Claude's built-in list; Codex's from its `models_cache.json` (listed ones, its order);
pi's from `pi --list-models` (ids `provider/model`), narrowed to the `enabledModels` patterns of
pi's `settings.json` when set. `id: ""` is the harness's own default.
Efforts: Claude `low…max`; Codex the levels its model cache lists; pi `off…max`.

**Default model and effort.** Config `harnesses.<id>.model` / `harnesses.<id>.effort` (Settings →
Models in the web writes them to every host) are what a spawn starts with when it picks no model:
`spawn`, the web, bots, scheduled sessions and stack spawns alike. A picked model wins over the
default one; the effort goes in as Claude `--effort <l>`, Codex `-c model_reasoning_effort=<l>`, pi
`--thinking <l>`. A launcher that already sets one (a bot's own `command`, a wrapper with flags)
keeps its own. Unset, the harness's own config decides. A resumed session keeps its own.

### GitHub identities

A session can act on GitHub as a GitHub App (a bot) instead of as you — for comments, reviews
and approvals that come from automation. Two ways:

- **On request, any session**: when you ask a session to act as one ("comment as the bot"), it
  runs that identity's `command` (`fleet gh-identity list`) for those actions and plain `gh`,
  as you, otherwise. It can switch at any point of the session; nothing is enforced.
- **Enforced, from the spawn** (`--gh-identity <name>`, a bot's `ghIdentity`): the whole
  session acts as the App and cannot act as you — decided by whoever spawns it, never by the
  agent inside it. Used by bots; the web New session form no longer offers it.

Identities are per machine, in the config:

```json
"ghIdentities": {
  "bot": { "command": "gh-bot", "login": "my-app[bot]", "prompt": "(optional) replaces the default note" }
}
```

`command` is a `gh` wrapper that authenticates as the App (mints an installation token and runs
`gh` with it as `GH_TOKEN`); `login` is the App's login (default the name). A session spawned
with the identity runs as

```sh
export FLEET_GH_IDENTITY=bot GH_CONFIG_DIR=<empty dir> PATH=<shim dir>:"$PATH" &&
  claude --append-system-prompt "$(cat <note>)" …
```

- the note (system prompt) tells the agent it acts as `login` and to use `command`;
- `gh` in that shell is a shim for `command`, so scripts and skills that call `gh` act as the App
  too; the empty `GH_CONFIG_DIR` leaves a `gh` reached by absolute path without your credentials;
- what the session spawns inherits the identity (`FLEET_GH_IDENTITY`, forwarded to `-H` hosts);
  asking for another one is an error. A [restore](#session-recovery) resumes it under the same
  identity, and refuses when the identity is no longer configured.

`fleet gh-identity list [--json]` shows the configured ones (`[{ name, login, command }]`);
`fleet gh-identity launcher <name> [--launcher <cmd>]` prints the launch command a session with
that identity starts with (writing its shim) — the web server's spawn uses it. The pane carries the option `@fleet_gh_identity`; `fleet list --json` reports it as
`gh_identity` (`null` = yours). Claude sessions only, for now. (Likewise `@fleet_embed` → `embed`:
the app whose embedded Fleet panel started the session — see [Embed view](architecture.md#embed-view).) `git push` is untouched — it uses
the machine's own credentials. Shim, empty config dir and note live in
`${XDG_STATE_HOME:-~/.local/state}/fleet/gh-identities/<name>/`.

### Slack links

For a while, a session talks with the people in one Slack thread (or a whole channel or DM): what
they write there is typed into it, and it answers there. You decide which session, which thread,
for how long and what it may help with — the web UI's **Add link** (session Details → Links), or:

| Command | What |
|---|---|
| `fleet slack [list] [--json]` | the bridge (connected?) and the active links |
| `fleet slack link <slack-url \| @person> --for <30m\|2h\|1w\|90\|forever> [--session <target>] [--task <instructions>] [--proactive] [--json]` | link a session (default: the one this runs in, by `$TMUX_PANE`). A message link is its thread; a channel / DM link is all of it; a person (`@name`, a name or their user id `U…` — instead of a link) is the app's DM with them, opened if new (several matches: an error with the candidates). Needs the `im:write` scope and the app's Messages tab on. `--task`: your instructions (what it may help with, tone, when to step in). `--proactive`: it sees every message and steps in when it judges it should; default: it is woken only when someone mentions the app, DMs it or writes in a thread it has posted in (then it judges itself whether the message is for it), and gets what was said in between as context. Linking the same session to the same thread again renews it (new time limit, instructions, mode). Up to a week, or `forever`: no end, until unlinked. An ended link can be relinked in one click from the web UI (Details → Links → **Link again**) |
| `fleet slack unlink <sl-xxxxxx>` | end a link now |
| `fleet slack reply <sl-xxxxxx> <text> [--thread <ts>]` | post in the linked thread — what the linked session runs to answer. Prints the new message's `ts` and permalink (`--json`: `{ link, channel, ts, threadTs, permalink }`), e.g. to reply in a thread it just started. Refused (exit 3) once the link has ended, and for any session but the linked one. Threads first: in a channel or group DM link, a post outside a thread must be a one-line headline (≤ 300 chars) whose details go in its thread — anything longer is refused (400); a one-on-one DM is exempt |
| `fleet slack react <sl-xxxxxx> <ts> <emoji> [--remove] [--json]` | an emoji reaction (a Slack name like `white_check_mark`, colons optional) on a message of the linked conversation — what the linked session runs when a reaction says enough (`--json`: `{ link, channel, ts, emoji, removed }`). Each message typed in carries its `ts`. Already there / already gone is not an error. Same rules as `reply`: the linked session only, while linked (exit 3 after). Needs `reactions:write` |
| `fleet slack history <sl-xxxxxx> [--limit 50] [--cursor <c>] [--oldest <ts>] [--latest <ts>] [--json]` | read the linked conversation, a page at a time (up to 200): a channel / DM from the newest message (the next page, `older: … --cursor`, goes back; each page printed top-down), a thread link its thread (as `thread`). Each message: who (name, id, the owner / `[you]` / `[app]`), when, its `ts`, its replies (read them with `thread`) and its files (id, name, type, size). Read-only, the linked session only, while linked (exit 3 after) |
| `fleet slack thread <sl-xxxxxx> <ts> [--limit] [--cursor] [--json]` | one thread of the linked conversation (a thread link: only its own thread): its first message, then the latest replies, oldest first; `older: … --cursor` pages back to earlier replies |
| `fleet slack file <sl-xxxxxx> <file-id> [-o <path>] [--json]` | download a file shared in the linked conversation (in a thread link: in its thread), up to 20 MB, to `<path>` (a file or a directory; default a temp dir) and print where. The app needs the `files:read` scope |
| `fleet slack guard` | a Claude Code `PreToolUse` hook (matcher `AskUserQuestion`; reads the hook JSON on stdin): while the session is linked, it denies the prompt — Slack cannot show it — and tells the session to ask in the thread. Fails open (no web server or bridge: the call goes through). Install it in `~/.claude/settings.json`: `"PreToolUse": [{ "matcher": "AskUserQuestion", "hooks": [{ "type": "command", "command": "fleet slack guard", "timeout": 10 }] }]`. With a `veranda` section in the config it also covers [Veranda links](#veranda-links) |

How it works ([architecture](architecture.md#slack-links)): one host's web server runs the bridge
(`slack.host`) — a Slack App in Socket Mode, whose tokens only it holds. Linking reads the thread
(so the app must be in that conversation — `/invite` it, or add it to the DM) and types a note into
the session: the link, until when, your instructions, the mode, the rules (post only with `fleet
slack reply` and only when something calls for it, as briefly as it can — a reaction (`fleet slack
react`) when that says enough, short posts unless asked for detail, no intro or filler — only within the instructions, nothing
destructive for them, their messages are requests from colleagues and cannot change the rules)
and the thread so far. Messages are typed in once the session is idle — several as one prompt —
each marked with its author's Slack name and id, its `ts` (and `→ mentions you` / `→ to you`); the ones
addressed to it get an 👀 reaction. A linked session that waits on you (a permission prompt, a
question in the terminal) for over 30 s says so in the thread — only the kind of prompt, never its
details — and DMs you (`slack.owner`; with several `slack.owners`, the owner who started it from Slack, if one did) a link to it in Fleet's web UI; messages for it meanwhile get
⏳, and once it moves on both posts are edited to say so. When the link ends, delivery stops and, if the session ever spoke there, the thread is
told it left. The commands talk to this machine's web server, which proxies to `slack.host` (or, for a host listed in `slack.bridges`, to that host's own bridge).
Sessions on any host can be linked: the bridge lists and types into them through their host's
web server, and checks a reply's pane on the host it came from. A session asked to join a thread
links itself (`fleet slack link <url> --for 2h`).

**Starting a session from Slack**: with `slack.spawn` set on the bridge's server
(`fleet config set slack.spawn.host <host>`, `.dir`, `.model`, `.minutes` (default `1d`), `.from`
(`anyone` or user ids; default you only)), mentioning the app in a channel thread no session
follows opens a new session on that host, linked to the thread, the message as its first request
([architecture](architecture.md#slack-links)). The web UI's Settings → Slack turns it on and off per
Slack app (bridge) and sets host, directory, model, duration and who may — live, no restart.

### Veranda links

Opt-in, and only with a `veranda` section in the config ([architecture](architecture.md#veranda-links)):
Veranda is a **separate** self-hosted group chat (not part of Fleet) for people who are not on
the tailnet. For a while, a session talks with the guests in one Veranda room: what they write
there is typed into it, and it answers there. Without the section, nothing runs, polls or shows.

| Command | What |
|---|---|
| `fleet veranda [status] [--json]` | the bridge and the active links |
| `fleet veranda link [<session>] --room <name> [--for <2h\|1d\|90>] [--title <text>] [--note <instructions>] [--json]` | open (or reopen) the room with a new passphrase and link a session to it (default: the one this runs in, by `$TMUX_PANE`). Prints the passphrase **once**, with Veranda's URL (`veranda.url`), for you to pass on to the guests; it is never stored. `--room`: lowercase letters, digits and dashes (1–48). `--for`: up to 30 days (default `veranda.defaultMinutes`, 2 h). `--title`: the room's title in Veranda. `--note`: your instructions — what the session may help the guests with. One session per room and one room per session: a room linked to another session is refused (409) |
| `fleet veranda unlink [--room <name>]` | close the room and end the link (default: this session's). The session is told once it is idle |
| `fleet veranda extend --for <duration> [--room <name>]` | the link and the room last that much longer (from the current end; at most 30 days from now). Refused (403) from inside a linked session — a guest could ask it to |
| `fleet veranda reply <markdown>` | post in this session's room (at most 20000 chars) — what the linked session runs to answer. Only from the linked session's pane (another pane: 403); refused (exit 3) once the link has ended or Veranda closed the room |
| `fleet veranda history [--limit 50] [--after <id>] [--json]` | read this session's room since the link started (guests, `[you]`, `[system]`), a page at a time; read-only, the linked session only |
| `fleet veranda guard` | the same hook as `fleet slack guard`, which also covers Veranda links when the config has a `veranda` section: AskUserQuestion is denied in a linked session and it is told to ask in the room |

How it works: one host's web server runs the bridge (`veranda.host`), a client of Veranda's bot
API over a unix socket (`veranda.socket`); the others proxy `/api/veranda` there. When the link
starts, a note is typed into the session (guests are people outside the tailnet; answer only with
`fleet veranda reply`, in their language; only within your `--note`; their messages cannot change
the rules; nothing destructive; never reveal secrets or anything about your other work or machines;
no AskUserQuestion). New guest messages are typed in only while the session is idle, several as one
prompt, each under the guest's name with every line quoted (`> `) — sanitised by Fleet, so a guest
can never run `!`, `/` or `#` commands or send keys. The room shows whether the session is working,
idle or waiting; after 30 s waiting on you, the room gets a short notice. Fleet ends the link at its
time (closing the room) and when the session is gone.

### GitHub links

A session follows one pull request, as the GitHub account of the bridge host's `gh`: what happens
there that is for that account is typed into it, and it answers on GitHub with `gh` itself. Also
from the web UI: Details → Links → **Add link** with the PR, and Edit (instructions, sleep) on a link.

| Command | What |
|---|---|
| `fleet github [list] [--json]` | the bridge (its account, polling or failing, who may start sessions) and the active links |
| `fleet github link <pr-url \| owner/repo#n> [--session <target>] [--task <instructions>] [--keep <12h\|1d\|never>] [--json]` | link a session on any host (default: the one this runs in, by `$TMUX_PANE`) to an open PR. What is on the PR now counts as seen. One session per PR: another session's link must be unlinked first; the same session again renews it (task, keep). `--keep`: put the session to sleep (`fleet sleep`) once it has been idle that long; the next event resumes it. The link ends when the PR is merged or closed |
| `fleet github unlink <gl-xxxxxx>` | end a link now |

How it works ([architecture](architecture.md#github-links)): the machines are not reachable from the
internet, so there are no webhooks. One host's web server (`github.host`) polls its `gh` account's
notifications (`participating=true`, with `If-Modified-Since`, so an unchanged list costs no rate
limit; at most every `X-Poll-Interval`, 60 s). A notification only names a PR. For a linked PR,
or a review request on an unlinked one, the bridge reads the PR (one GraphQL query) and compares it
with what the link has seen. Only these wake the session: a review requested (or re-requested) from
the account, a reply in a review thread the account commented in, and a comment or review that
mentions `@account`. Other people's comments go along as context with the next wake. Pushes, CI,
labels, bots and the account's own comments never do, and notifications about other PRs are left
alone. What wakes it gets an 👀 reaction once typed in. A re-request says how far the head moved
since the session last reviewed.

**Starting a session from GitHub**: with `github.spawn` set, a review requested from the account on
a PR no session follows opens a new session on `github.host`, linked to the PR, the request as its
first prompt — only when the person who requested it is in `github.spawn.from` (GitHub logins, or
`"anyone"`; none = nobody), and only for requests made after the bridge first ran.
`fleet config set github '{"host":"<host>","spawn":{"dir":"~/work","model":"opus","keep":"1d","from":["<login>"]}}'`
(`keep`, default `1d`: such sessions sleep after that long idle; the next event resumes them, and if
that fails a new session takes the link over and reads the PR's history first). Once its PR is
merged or closed, such a session is put to sleep as soon as it is idle (after a 10-minute grace);
an ended link shows `retired: true` then. Sessions linked by hand are left alone. `github.spawn`
applies within a few seconds; `github.host` needs a restart of the web server.

### Productive links

A session follows one Productive task, as the Productive account (a person in the organization)
whose API token the bridge host holds: comments and changes on the task are typed into it, and it
answers with `fleet productive reply`, which posts a comment as that account. Also from the web
UI: Details → Links → **Add link** with the task URL, and Edit (instructions, sleep) on a link.

| Command | What |
|---|---|
| `fleet productive [list] [--json]` | the bridge (its account, polling or failing, who may start sessions) and the active links |
| `fleet productive link <task-url \| id> [--session <target>] [--task <instructions>] [--keep <12h\|1d\|never>] [--json]` | link a session on any host (default: the one this runs in, by `$TMUX_PANE`) to an open task. What is on the task now counts as seen. One session per task; the same session again renews it (task, keep). `--keep`: sleep after that long idle; the next event resumes it. The link ends when the task is closed |
| `fleet productive reply <text \| -> [--file <path>] [--link <pl-xxxxxx>] [--session <target>] [--json]` | post a comment on the linked task (default: this session's link) as the account. Text with a little Markdown: paragraphs, `- ` lists, `**bold**`, `` `code` ``, links; `@[Person:<id>:<Name>]` mentions someone. `-` or no text: read stdin |
| `fleet productive unlink <pl-xxxxxx>` | end a link now |

How it works ([architecture](architecture.md#productive-links)): one host's web server
(`productive.host`) polls its account's notifications every minute. A notification only names a
task; the bridge then reads the task's activity feed (comments and changes) and compares it with
the newest activity the link has seen. Linked tasks are also re-read every 3 minutes. What wakes
the session: a comment that mentions the account, any comment by someone in `productive.spawn.from`,
and any change on the task (status, assignee, todos…). Comments by others go along as context; the
account's own activity never comes back. **Throttled**: a burst goes in as one prompt once the task
has been quiet for a while (20 s after a mention, 45 s otherwise), at least 30 s (a mention), 2 min
(comments) or 10 min (changes only) after the last delivery, and only into an idle session.

**Starting a session from Productive**: with `productive.spawn` set, a mention of the account on a
task no session follows opens a new session on `productive.host` (named `task-<number>`), linked to
the task, with the task, its description, its recent comments and the mention as its first prompt —
only when the comment's author is in `productive.spawn.from` (Productive person ids or emails, or
`"anyone"`; none = nobody), and only for mentions made after the bridge first ran.
`fleet config set productive '{"host":"<host>","org":"<org id>","spawn":{"dir":"~/work","model":"opus","keep":"1d","from":["<email>"]}}'`.
The token: a Keychain item (`productive.keychain`, its service name, default
`PRODUCTIVE_AUTH_TOKEN`) or a file (`productive.tokenFile`, mode 0600). `productive.spawn` applies
within a few seconds; the rest needs a restart of the web server.

### Briefs

A brief is a small markdown file per session — what it is doing, the resources it produced (PRs,
files, its git branch and checkout, artifacts, links) and its todos — kept by the web server on the host the session
lives on (`$FLEET_BRIEFS_DIR`, else `${XDG_STATE_HOME:-~/.local/state}/fleet/briefs/<session_id>.md`;
format and merge rules in [architecture.md](architecture.md#session-briefs)). The CLI reads and
edits the same files; generating one (a `claude -p` call) is the web server's job.

`<target>` resolves like any other session target; a session that is gone still has its brief,
reachable by its **full** session id. With `-H <host>` every form runs on that host, where the file is.

| form | does |
| --- | --- |
| `fleet brief <target>` | the body as markdown (a skeleton, and a note on stderr, when there is none yet) |
| `--json` | `{ host, id, exists, markdown, body, parsed: { summary, resources: [{ kind, label, url, path, text, branch, linked }], todos: [{ done, text }], plan }, updated, editedAt, generatedAt, generatedThrough, continuePrompt, absCwd, gitRoot, path }` — the web API's GET shape (minus the server-only `generating` / `enabled` / `editor` / `editorUrl`), plus `body` (markdown without frontmatter) and `path`. `branch` / `linked` are set on the `Git` resource only; `absCwd` is the session's directory (absolute), `gitRoot` its checkout root (null outside git). `parsed.plan` is a **deprecated** alias of `parsed.todos` (same array; the section used to be `## Plan`), kept for one release |
| `--prompt` | only the continue prompt — the first prompt for a new session that picks the work up (`fleet spawn --from` uses it) |
| `--edit` | the body in `$VISUAL` / `$EDITOR` (default `vi`), saved as a human edit when changed. For a session on another host the brief is fetched over ssh (`fleet brief --json` there), edited here, and written back with `--set --expect-updated` |
| `--set` | the markdown on stdin, saved as a human edit — the same rules as the web UI's save (`PUT`): it is authoritative for the body, the stored frontmatter is kept (the machine keys stay the server's), resource lines it removes are added to `dismissed` so they are never re-added, `editedAt` is stamped. `--expect-updated <iso>` refuses (exit 3, nothing written) unless the stored `updated` still is that (`""` = no brief yet) — what `--edit` uses so a brief regenerated while you were editing isn't overwritten. `-n` prints the result instead of writing it |
| `--open` | open the session's git root (else its cwd) in VS Code: `code <path>`, or for a session on another host (`-H`) `code --remote ssh-remote+<its ssh dest> <path>` with the path from `fleet brief --json` there. `cursor` instead of `code` with `web.editor: "cursor"`. Needs the editor's command on `PATH`; `-n` prints the command |
| `--regenerate` | POST `/api/hosts/<self>/sessions/<id>/brief/regenerate` to this host's web server (`hosts.<self>.web`, else `http://127.0.0.1:<web.port>`) via `curl`; prints whether it started or was queued. The model call runs in the background — read the result with `fleet brief` a little later. At the server's hourly cap: exit 3 with the wait. Needs a live session and a running web server (`fleet web serve`) |

### Stacks

A **stack** is a set of sessions — on one host or across hosts — that share one context layer:
a markdown file, the **StackBrief** — `$FLEET_STACKS_DIR`, else
`${XDG_STATE_HOME:-~/.local/state}/fleet/stacks/<stack_id>.md` on the host where the stack was
created, its **home** (dir `0700`, files `0600`, atomic writes; format in
[architecture.md](architecture.md#session-stacks)).
The file is the state: id (`st-` + 8 hex, never changes), label, cwd, the members (each with the
host it runs on), and the sections Summary / Resources / Sessions / Notes. It is written with one
`claude -p --model <stacks.model>` call (default `sonnet`) when the stack is created; after that
scripts keep `## Sessions` (membership) and people — or the sessions themselves — edit the rest
by hand.

Every session spawned into a stack on its home host gets this first prompt: `You're running in
the session stack with shared context: <abs path to the file>. <prompt>`. One spawned on another
host (`--on`) cannot read that path and gets: `You're running in the session stack with shared
context: stack <id> on host <home>. Read it with \`fleet -H <home> stack show <id>\` and update it
with \`fleet -H <home> stack set <id>\` (the whole markdown on stdin; …). <prompt>`.

**The lead.** A stack may have one lead (orchestrator) session (`fleet stack lead`). With
`stacks.siblingReports: true` (default off) a sibling spawned into a stack that has a lead gets
one more sentence after its context line (`stacks.reportPrompt`, default
`core::stack::DEFAULT_REPORT_PROMPT`): report to the lead with `fleet -H <its host> send <its id>`
when a task is done, blocked or needs a decision. Both prompts take the placeholders `{label}`,
`{id}`, `{home}`, `{context}` (the file's path, or how to reach it from a lead on another host),
`{fleet}` (`fleet`, or `fleet -H <home>` for such a lead), `{lead}` (its short session id),
`{leadName}` and `{leadHost}`; Settings → Session stacks edits them on every host.

`<stack>` resolves: exact id (`st-1a2b3c4d` or `1a2b3c4d`) → label (exact, then case-insensitive
prefix, then substring) → a member's session id (whole, or a prefix of ≥ 8) → a session target
that is a member. Two hits on the same rung is an error listing the candidates (exit 2); no hit is
exit 3. `<target>` is a live session target as everywhere else (an ambiguous one is exit 2).
With `-H <host>` every form runs on that host, where the files are — a stack is always addressed
on its home, whichever hosts its members run on. A host this machine has no ssh destination for
but a web url (`hosts.<host>.web` — how the always-on machines know each other) is reached
through its web server instead (`curl` to its stack routes): `list`, `show`, `set`, `rename` and
`rm`, with the stack given by id (`st-…` or the 8 hex). That is what a sibling started on
another host needs for its context line. The other forms need ssh. `--on <host>` (`add`,
`spawn`) is different: the stack stays here, the session is (started) there.

| form | does |
| --- | --- |
| `fleet stack list [--json]` | the host's stacks (after a sync): label, id, live/closed counts, cwd, updated. `--json`: `{ host, stacks: [StackView…], settings: { leadPrompt, leadPromptDefault, siblingReports, reportPrompt, reportPromptDefault } }` (a prompt `null` = its default) |
| `fleet stack show <stack> [--json \| --path]` | the StackBrief body (no frontmatter); `--json` the StackView; `--path` only the file path |
| `fleet stack new --from <target> [--label <l>] [--no-llm] [--json]` | create a stack around a live session (error if it is in one already — says which). The model writes Summary / Resources and a label (unless `--label`); `--no-llm`, `FLEET_FIXTURE`, `stacks.enabled: false` or a failing call give the skeleton (Summary "Started from <title> in <cwd>.", Resources = the Git line + Folder) — a failed call never blocks the stack: `generated: false` and a `warning` |
| `fleet stack ensure <target> [--label] [--no-llm] [--json]` | the session's stack, created like `new` when it has none |
| `fleet stack spawn <target> [prompt] [--harness] [--name] [--model] [--dir] [--backend iterm\|tmux] [--tmux-session <s>] [--window] [--on <host>] [--no-wait] [--label] [--no-llm] [--json]` | a sibling: `ensure` on `<target>`, then a new session on this host in `<target>`'s cwd (`--dir` overrides) whose first prompt is the context line + `prompt`, always through a prompt file `~/.claude/fleet-handoffs/<stamp>-stack-<hex>.md`; then waits (≤ 75 s) for it to register and adds it. `--on <host>`: the session starts on that host instead — `fleet --local spawn` there over ssh, in `--dir` (a path there; default `<target>`'s cwd as `~/…`, the same place on that machine) with the remote context line + `prompt` as the argument; it then waits for it in that host's `fleet list` and adds it as a member of that host (`spawned.host`, `session.host` in `--json`). `--no-wait` skips the waiting and prints the `fleet stack add` to run later. `-n` / `FLEET_DRY_RUN` prints the launch line (the ssh line with `--on`) and the prompt and writes nothing (not even a new stack). Starts an agent — confirm like `spawn` |
| `fleet stack add <stack> <target> [--on <host>] [--input <src>] [--json]` | add a live session (refused when it is live in another stack of this host). `--on <host>`: a session running on that host — `<target>` is resolved among its rows, from `fleet list --json` over ssh, or from `--input` (`-` = stdin: a `list --json` array or an `/api/fleet` body carrying that host; what the web server passes, so it never needs ssh). Naming this machine is the plain form |
| `fleet stack remove <stack> <target\|session_id> [--json]` | drop a member (a gone one by its session id or a prefix of ≥ 4, or its name); removing the last leaves an empty stack (the file stays) |
| `fleet stack lead <stack> [<target\|session_id>] [--clear] [--quiet] [--json]` | the stack's **lead** (orchestrator): a live member becomes it (frontmatter `lead`; one per stack) and is sent the lead prompt (`stacks.leadPrompt`, default `core::stack::DEFAULT_LEAD_PROMPT`: follow the other sessions, delegate to them, keep the stack's plan and progress in the StackBrief) — typed into it like `send`, over ssh for a member on another host; `--quiet` sends nothing. A failed send still sets the lead (`sent: false` and a `warning` in `--json`). `--clear`: no lead. No session: print the lead. Removing the lead member clears it. The lead is listed first in its stack's group in the web UI and marked `, lead` in `## Sessions`. `--json`: the StackView + `sent`, `warning` |
| `fleet stack rename <stack> <label> [--json]` | rename the stack: the label (one line, ≤ 80 chars, cleaned like `--label`) in the frontmatter, the header line follows; `editedAt` stamped. `--json`: the StackView |
| `fleet stack edit <stack>` | the body in `$VISUAL` / `$EDITOR`, saved as a human edit (with `--expect-updated` semantics; on another host fetched with `stack show --json` and written back with `stack set`) |
| `fleet stack set <stack> [--expect-updated <iso>] [--json]` | the markdown on stdin, saved as a human edit: authoritative for everything but the header and `## Sessions` (regenerated); the stored machine keys stay; a `label:` in its frontmatter renames the stack; `editedAt` stamped. A stale `--expect-updated` is exit 3, nothing written (with `--json` stdout still carries `{ error, id, updated }`, the stored timestamp) |
| `fleet stack sync [--input <src>] [--json]` | reconcile members with the live sessions: gone → `closed` (once; a session dormant after a reboot is left open), live → name refreshed; only changed files are rewritten. A member is judged only against its own host's sessions: without `--input` that is this machine (members on other hosts are left as they were); `--input <src>` (`-` = stdin: a `list --json` array or an `/api/fleet` body) brings the other hosts' live sessions, a host with `ok: false` in it is still not judged. `--json`: `{ host, changed: [ids], stacks: [StackView…] }`. Also runs implicitly (without input) in `list`, `show` and after `spawn` / `add` / `remove` |
| `fleet stack rm <stack> [-f] [--json]` | delete the stack's file (asks unless `-f`; `--json` needs `-f`) → `{ removed: id }` |

**StackView** (`--json`; the web API serves it as-is):

```json
{ "host": "laptop", "id": "st-1a2b3c4d", "label": "Login redirect fix", "path": "/abs/…/stacks/st-1a2b3c4d.md",
  "cwd": "~/Code/project", "absCwd": "/abs/…/Code/project",
  "created": "…", "updated": "…", "generatedAt": "…", "editedAt": null,
  "contextLine": "You're running in the session stack with shared context: /abs/…/stacks/st-1a2b3c4d.md.",
  "remoteContextLine": "You're running in the session stack with shared context: stack st-1a2b3c4d on host laptop. Read it with `fleet -H laptop stack show st-1a2b3c4d` and update it with `fleet -H laptop stack set st-1a2b3c4d` (the whole markdown on stdin; `fleet -H laptop stack show st-1a2b3c4d --json` has it under `markdown`).",
  "members": [ { "session": "<uuid>", "host": "laptop", "name": "login-redirect", "added": "…", "closed": null,
                 "cwd": "~/Code/project", "firstPrompt": "…",
                 "live": true, "dormant": false, "status": "idle", "briefPath": "/abs/…/briefs/<uuid>.md", "briefExists": true },
               { "session": "<uuid>", "host": "workstation", "name": "login-e2e", "added": "…", "closed": null,
                 "cwd": "/home/x/Code/project", "firstPrompt": "…",
                 "live": true, "dormant": false, "status": null, "briefPath": null, "briefExists": false } ],
  "markdown": "<whole file>", "body": "<without frontmatter>",
  "parsed": { "summary": "…", "resources": [ { "kind": "PR", "label": "…", "url": "…", "path": null, "text": "…", "branch": null, "linked": null } ],
              "notes": "…" } }
```

`status` is `null` for a member that is not live, and for one on a host not observed by this call
(then `live` is `closed == null`; `briefPath` is `null` — the brief is on that host). `new` /
`ensure --json` print the StackView plus
`created` (**bool**: made by this call — it shadows the view's timestamp, which is repeated as
`createdAt`), `generated`, `warning` (string \| null) and `stack` (the untouched StackView).
`spawn --json`: `{ stack: StackView, created, generated, warning, spawned: { desc, dir,
promptFile, tmuxSession }, session: { session_id, pid, display_title } | null, addError,
addCommand, contextLine }` (`addError` when the new session didn't register in time or could not
be added; `addCommand` with `--no-wait`); under `-n`: `dryRun: true`, `prompt`, `session: null`.

### Grouping

`fleet group` sorts the live sessions into a handful of work-stream groups (labels of 2–4 words,
an optional one-line description) with `claude -p --model <grouping.model>` (default `haiku`),
and keeps them stable across runs. State: `$FLEET_GROUPS_STATE`, else
`${XDG_STATE_HOME:-~/.local/state}/fleet/groups.json` on the machine that runs it.

- Sessions: this machine's by default; `-a` every configured host (over ssh); `--input <file|->`
  a `list --json` array or a `/api/fleet` body (what the web server pipes in — hosts with
  `ok: false` keep their assignments).
- Without `--apply` the result is printed and nothing is saved; `--apply` writes the state.
  `-n` builds the prompts and reports how many model calls a run would make, calling none.
- A run only calls the model for sessions it hasn't placed yet (new, renamed/retitled/moved, or
  parked in a fallback group while the model was down) — an unchanged fleet costs nothing. At
  most once an hour (`grouping.consolidateMinutes`), after changes, one consolidation call may
  merge ≤ 2 groups and rename ≤ 2. `--consolidate` forces it; `--refresh` starts from scratch.
- Model off (`grouping.enabled: false`), unavailable or unusable → sessions go to a group per
  repository (the cwd, worktree-aware), `source: "fallback"`.
- `--cached` prints the stored groups: no discovery, no model call.
- Hand edits (what the Board's rename and drag'n'drop run; they edit the state file, no discovery,
  no model call, then print like `--cached`; `-n` saves nothing):
  - `--rename <group id> --label <l>` — the label (one line, ≤ 48 characters, not another
    group's) is the user's from then on: consolidation never renames that group nor merges it
    away. A renamed repository group becomes `source: "manual"` and keeps its sessions when the
    model is back.
  - `--move <host/id> --to <group id>` — the session stays in that group for good (`manual`
    assignment: never reclassified, whatever changes about the session; its group is never
    merged away). `--label <l>` instead of `--to` moves it to a new group of that name
    (`source: "manual"`), or to the existing group that has it. A group left empty goes —
    unless it is `manual`.
  - `--create --label <l>` — a new, empty group (`source: "manual"`, the label not another
    group's). A `manual` group stays, empty or not, until `--delete <group id>`, which refuses
    a group that still has sessions.

`--json` (stable contract):

```json
{ "version": 1, "applied": true, "updatedAt": 1790000000000,
  "lastRun": { "at": 0, "ms": 0, "mode": "noop|incremental|full|consolidate|fallback|dry-run", "ok": true,
               "modelCalls": 0, "classified": 0, "kept": 30, "pruned": 0, "created": 0, "merged": 0,
               "renamed": 0, "note": "…", "error": "…" },
  "groups": [ { "id": "g-1a2b3c4d", "label": "Fleet Board", "description": "…", "source": "llm",
                "members": [ { "host": "laptop", "id": "<session id>", "name": "board-view" } ] } ],
  "ungrouped": [ { "host": "laptop", "id": "…", "name": "…" } ],
  "hosts": { "laptop": "ok", "workstation": "unreachable: …" } }
```

Group ids never change once created (a merge keeps the target's id); empty groups are dropped.
`source` is `llm`, `fallback` or `manual` (made on the board); a `manual` group may have no members.
A member that is dormant on its host after a reboot ([Session recovery](#session-recovery)) keeps
its group and carries `"dormant": true` (the field is absent otherwise, and in `--cached`); a host
whose dormant list can't be fetched has nothing pruned that run.

### Dashboard keys (`watch`)

| key | does |
| --- | --- |
| `1`–`9`, `0` | jump to that row and focus it |
| Enter, space, `l`, `o` (and Ctrl-J) | focus the selected session's pane/tab |
| ↑/↓, `k`/`j`, scroll | move the selection |
| click / tap | select + focus (`--no-mouse` to disable) |
| `n` | rename the selected session |
| `N` | prefill the rename with the row's generated title |
| Ctrl-N | suggest names for every unnamed session |
| `r` | refresh now |
| `z` | compact one-line rows ⇄ full rows |
| `e` | toggle the events pane |
| `?` | help overlay |
| `q`, Esc, Ctrl-C | quit |

Run it in a tmux session named `fleet` and bind `prefix f` to `switch-client -t fleet` to jump back
after focusing a session (see [setup.md](setup.md#4-tmux)).

## tmux sessions

Manage tmux *sessions* (one per job) on the target host — `defaultHost` by default. `fleet tmux`
alone lists. Alias: `fleet t …`; `enter` (alias `e`), `last` and `new` also exist at the top level.

| command | does |
| --- | --- |
| `fleet tmux list [-q] [--json]` (alias `ls`) | sessions, most recently attached first (`-q`: names only). The text view adds [dormant](#session-recovery) sessions, dimmed and marked `◌ … dormant`; `-q` and `--json` list live sessions only |
| `fleet tmux enter <query>` (alias `e`) | attach by exact > prefix > substring match (case-insensitive); a [dormant](#session-recovery) session is restored first when no live one matches |
| `fleet tmux last` | attach to the session you were in before the current one |
| `fleet tmux new [name] [-d] [-C <dir>] [-- cmd]` | attach to, or create, a session (default `main`). `-d` creates without attaching |
| `fleet tmux kill <query> [-f]` | kill a session (asks first; `-f` skips) |
| `fleet tmux rename <query> <name>` | rename a tmux session — when it hosts exactly one Claude session (one window, one pane), the rename goes through that session's title instead (`fleet rename`), so the two stay in sync |
| `fleet tmux stale [--kill] [--older-than 24h] [--no-fleet-check] [-f] [-q] [--json]` | idle shells nothing uses; lists only unless `--kill` |

Details:

- **`enter` looks on every host** when no host is named (no `-H`, no `FLEET_HOST`, no `--local`):
  the default host first; if nothing matches there (or it can't be reached), every other host with
  an `ssh` destination plus this machine, in parallel (web-only peers are skipped). Hits are ranked
  by the same tiers across hosts. One match → `→ laptop: <name>` on stderr, then it attaches there
  (`switch-client` inside tmux when it's this machine). Several → listed with their host, exit 2
  (pick one with `-H`). None → the same search over each answering host's *dormant* sessions
  (`fleet restore --json` there); one hit is restored on its host and attached (`→ laptop: <name>
  (dormant)`), several are exit 2. Still none → exit 3 naming the hosts searched. An unreachable
  host is a warning, not a failure, unless no host answered (exit 4). `kill` and `rename` don't
  fall back — they act on the target host only.
- **`enter` restores dormant sessions**: when no live session matches but exactly one
  [dormant](#session-recovery) tmux session does (same tiers), it prints `restoring dormant session
  <name>…`, runs `fleet restore <name>` and attaches to the result. A live match always wins over a
  dormant one; two dormant matches are exit 2 with the candidates.
- **Names** are sanitized the same way by `new` and `rename`: anything outside `A-Za-z0-9_-`
  becomes `-`, runs collapse, leading/trailing `-` are dropped.
- One title per session (see [architecture → Session titles](architecture.md#session-titles)):
  `fleet rename` sets the Claude name and the tmux name follows. `tmux rename` of a single-Claude
  tmux session does the same (renames Claude, if it is waiting on a prompt only tmux is renamed,
  with a note); of any other tmux session it renames just the tmux session. A raw
  `tmux rename-session` is not watched — it sticks until the title next changes.
- **stale** = no Claude session in it, an idle shell with no background or suspended job, idle
  longer than `--older-than` (`30m`, `12h`, `7d`, or a number of hours; max `3650d`). Candidates
  are cross-checked against live Claude sessions (the same discovery as `fleet list`); if that
  check cannot be trusted (discovery fails, or a live session's handle matches no tmux pane),
  `--kill` refuses and `-q` prints nothing and exits non-zero.
  `--no-fleet-check` skips the cross-check but then confirms every kill individually (it cannot be
  combined with `-f`).

JSON shapes:

- `tmux list --json` — array of `{ name, attached, windows, last_attached, activity, created,
  current, host }` (times are Unix seconds; `current` = the session you are in).
- `tmux stale --json` — `{ candidates: [{ name, idle_secs, windows, command }], kept: [{ name,
  reason, class }], claude_checked, claude_problem }`; `class` is one of `here`, `attached`,
  `infra`, `claude`, `unknown`, `running`, `recent`.

## Session recovery

A reboot kills the tmux server and every Claude process. Fleet keeps a per-machine
[snapshot](architecture.md#session-recovery) of what was running — tmux sessions with their
windows, panes, cwds and layouts, which pane runs which Claude session and with which flags, and
Claude sessions outside tmux — recorded by every local `fleet list` (the web server polls it, so
it is always fresh). After a reboot (a new boot id), whatever the old boot ran is **dormant** until
you bring it back or forget it. Within one boot, a session you close is closed — never dormant
(except in the last 5 minutes before a reboot: a restart quits the terminals before the daemons,
and those sessions must survive that) — unless you put it to sleep: `fleet sleep <target>`
(Deactivate in the app — the Close button offers it) ends it and keeps it dormant, marked `slept`, until you restore or forget it. A slept
session is restored only by name (never by `--all` / `restore.onBoot`) and keeps its group and stack
membership meanwhile. The `watch` header shows `· N dormant` while there are any.

Sessions closed within a boot are kept separately as **recently closed** (`--closed`) for config
`restore.keepClosedDays` (default 7, `0` = off; newest 50): a closed tmux session (one that had a
Claude pane), a Claude session closed outside tmux, or a Claude session that exited while its
tmux pane lived on (`/exit`). They show up at once, are never dormant — never part of `--all`,
`restore.onBoot`, `fleet enter`, groups or stacks — and a reboot within 5 minutes of closing
makes them dormant instead.

| command | does |
| --- | --- |
| `fleet restore [--list] [--json]` | the dormant sessions on this host: tmux name (or the Claude title), windows/panes, Claude sessions, how long ago |
| `fleet restore <target> [-n] [--json]` | bring one back. `<target>`: a dormant tmux name (exact > prefix > substring, like `enter`), a session id (whole, or a prefix of 4+ characters — a session inside a dormant tmux session restores that tmux session) or a Claude title (exact > prefix > substring) |
| `fleet restore --all [-n] [--json]` | bring every dormant session a reboot left back (a failure is reported and the rest still go); slept ones stay |
| `fleet restore --forget <target> \| --forget-all [--json]` | drop dormant entries without restoring them |
| `fleet restore --closed [--json]` | the recently closed sessions, newest first: title, cwd, `closed 2h ago` (text `fleet restore` ends with `N recently closed (fleet restore --closed)` when there are any) |
| `fleet restore --closed <target> [-n] [--json]` | bring one closed session back (same targets and rules as above). A Claude session whose tmux session still exists comes back in a new window of it; otherwise as below. No `--closed --all` |
| `fleet restore --closed --forget <target> \| --forget-all [--json]` | drop recently closed entries |

What a restore does:

- **A tmux session** comes back under its own name — or `<name>-restored` (`-restored-2`, …)
  when a live session already has that name. Windows are recreated in order (with their names,
  unless tmux named them itself), panes are split in each pane's recorded cwd and the recorded
  layout is reapplied (`select-layout`); the active window and pane are selected again. A cwd that
  no longer exists becomes `$HOME`, with a warning.
- **Each Claude pane** gets `cd <cwd> && <launcher> <flags> --resume <sessionId>` typed into its
  shell (the shell loads your profile, like `spawn`). `<launcher>` is what `spawn` uses (`FLEET_CMD`
  → config `claude` → `claude`); the flags are the replay-safe ones the session was started with —
  `--dangerously-skip-permissions`, `--chrome`, `--model`, `--permission-mode`, `--add-dir`,
  `--agent`, `--fallback-model` — minus any the launcher already passes. Prompts, `-n`,
  `--resume`/`--continue`, `-p`, `--session-id` are never replayed. `--resume` keeps the session
  id (only `--fork-session` would mint a new one), so groups, stacks and briefs carry over. A
  session that is already running again (resumed by hand) is not started twice.
- **Other panes** get a plain shell in their cwd: old commands are never re-run.
- **A Claude session that was not in tmux** (iTerm, unknown) comes back in a *new tmux session*
  named after its title (sanitised, made unique) — tmux is the backend a restore can drive
  reliably, and `fleet enter` / the web UI attach to it from anywhere.
- On success the entry stops being dormant. `-n` prints every tmux command and launch line and
  changes nothing. Ambiguous targets are exit 2 with the candidates, unknown ones exit 3 — a
  restore starts agents, so it never guesses.

`-H <host>` restores on that host (fleet runs there); the default is this machine.

JSON shapes:

- `restore [--list] [--closed] --json` — `{ host, bootId, dormant: [DormantView…], closed:
  [ClosedView…] }` (the same with or without `--closed`), where ClosedView = DormantView +
  `closedAt` (ISO; `since` is the same) + `tmuxSession?` (a Claude session's tmux session that
  outlived it), newest first, and DormantView =
  `{ kind: "tmux"|"claude", target, name, since, windows, panes, sessions: [{ sessionId, name,
  title, cwd }], slept }` (`slept`: put to sleep with `fleet sleep`, not left by a reboot). `target` is what `fleet restore <target>` takes (the tmux name, or the full
  session id); `name` is the tmux name or the Claude title; `since` is when it went down (ISO).
- `restore <target>|--all --json` — `{ host, restored: [{ kind, from, session, renamed, windows,
  panes, launched: [{ sessionId, title, line }], warnings, commands, dryRun }], failed: [{ target,
  error }] }`; exit 1 when anything failed. `session` is the tmux session it now lives in;
  `commands` every tmux command and launch line, in order.
- `restore [--closed] --forget …|--forget-all --json` — `{ host, forgotten: [name…] }`.

## Hosts

| command | does |
| --- | --- |
| `fleet exec [-C <dir>] [-t] -- <cmd…>` | run a command on the host, outside tmux (cwd defaults to `$HOME`; `-t` allocates a tty) |
| `fleet ssh [cmd…]` | plain shell on the host, no tmux |
| `fleet doctor` | reachability, dispatch mode and why, tools and versions on both ends, config sanity |

## Setup

| command | does |
| --- | --- |
| `fleet init` | wizard: this machine's name, hosts (name, ssh target, web URL — can suggest from `tailscale status --json`), spawn dirs, web port. Never overwrites an existing config without confirmation (`--force`) |
| `fleet config path\|show\|get\|set\|edit` | where the config lives / print it (`--resolved` fills in defaults; its `spawnDirs` are this host's, a hidden one marked `"hidden": true`) / read or write one dotted key (`hosts.workstation.ssh`; values parse as JSON when they can) / open in `$VISUAL`/`$EDITOR` |
| `fleet install --host <name>` | copy this binary to `~/.local/bin/fleet` and the web app to `~/.local/share/fleet/web` on the host (rsync, or tar over ssh) — the host needs no Rust toolchain. Refuses when `uname -sm` differs between the machines unless `--force`; `--no-web` skips the web app. Of `web/ui` only the built `ui/dist` is copied (sources and `node_modules` never); when it is not built you get a warning (build with `fleet web build`) and the host serves only the API and a "not built" page |

Non-interactive `init` (scripts, tests):

```sh
fleet init --yes --self laptop \
  --add-host laptop,web=http://100.x.y.z:7777 \
  --add-host workstation,ssh=workstation,web=http://100.x.y.z:7777 \
  --default-host workstation \
  --spawn-dir Project=laptop:~/Code/project,workstation:~/Code/project \
  --web-port 7777 --web-bind 0.0.0.0
```

| flag | meaning |
| --- | --- |
| `-y, --yes` | take the flags (and defaults) as the answers, ask nothing |
| `--self <name>` | this machine's host name (default: the short hostname) |
| `--add-host NAME[,ssh=DEST][,web=URL]` | a host (repeatable); this machine needs no `ssh` |
| `--default-host <name>` | target for tmux-session commands |
| `--spawn-dir LABEL=HOST:PATH[,HOST:PATH…]` or `LABEL=PATH` | a spawn directory, per host or the same everywhere (repeatable) |
| `--web-port`, `--web-bind`, `--web-dir` | web server settings (`--web-dir` defaults to auto-detected). `--web-bind <tailnet IP>` keeps the server off the LAN (it also listens on loopback); `0.0.0.0` is every interface ([architecture](architecture.md#web-server-web)) |
| `--force` | overwrite an existing config without asking |
| `--print` | print the config instead of writing it |

## Pull requests

The open GitHub pull requests that need you, sorted into columns, cached, and joined with your
local checkouts. GitHub is reached only through the [`gh`](https://cli.github.com) CLI (`gh api
graphql`, its auth — run `gh auth login` once). Config: `prs` in [the config](architecture.md#config).

| command | does |
| --- | --- |
| `fleet prs [--json] [--column C] [--refresh \| --cached]` | four searches (`is:pr is:open archived:false` plus `prs.orgs` / `prs.repos`), in parallel, 50 per page up to 4 pages each: `author:@me` → **draftMine** (draft) · **readyToMergeMine** (review decision approved) · **reviewMine**; `user-review-requested:@me` (asked of you by name — team requests are left out) → **waitingOnMe**; `reviewed-by:@me -author:@me` whose latest review from you is not an approval → **waitingOnAuthor**. A PR is listed once, in the first of those that claims it. The fourth, `mentions:@me` (25 per page, with each PR's last 30 comments and last 20 reviews), finds the latest `@you` by someone else in the description, a comment, a review or a review comment (`mention`); until you comment or review on that PR after it, or mark it seen (`fleet pr seen`), the PR is in **unseenMentions** instead of its own column, and a PR there only for a mention leaves the list once it is seen. Fetches when the cache is older than `prs.refresh` (default `5m`); `--refresh` always fetches, `--cached` never does. A failed fetch serves the cache with a warning (`stale`, `error`). Each PR is matched to a repo under the [`repos`](#repos) roots by directory name (by `origin` when two share it), and to the worktree that has its head branch checked out. `--column` takes `waiting-on-me`, `waitingOnMe`, … Cache: `${XDG_CACHE_HOME:-~/.cache}/fleet/prs.json` (`FLEET_PRS_CACHE`) |

| `fleet pr checkout <pr> [--json]` | check one PR out in a worktree. `<pr>` is `owner/repo#123`, `repo#123` (its owner from the cache; ambiguous → error with the candidates) or the PR's URL. Asks GitHub for its head (`gh api repos/O/R/pulls/N`), finds the repo under the [`repos`](#repos) roots, and when no worktree has the branch yet: `git fetch origin <branch>`, then `git worktree add` at `<prs.worktrees>/<repo>-<branch slug>` (`-pr<N>` appended when taken) — on the local branch if there is one, else a new one tracking `origin/<branch>`. A fork's PR gets `pr-<N>` from `pull/<N>/head`. Existing branches are never reset or moved. `-n` changes nothing. `--json`: `{ owner, repo, number, title, url, headBranch, baseBranch, crossRepository, branch, path, repoPath, created, contextLine }` (`created: false` — it was already checked out) |
| `fleet pr seen <pr> [--json]` | mark the PR's @mentions of you so far seen: out of **unseenMentions** (a later mention brings it back). `<pr>` as for `pr checkout`. Kept per machine in `${XDG_STATE_HOME:-~/.local/state}/fleet/prs-seen.json` (`FLEET_PRS_SEEN`; marks older than 180 days are dropped). `-n` changes nothing. `--json`: `{ pr: "owner/repo#123", seenAt, applied }` |
| `fleet pr cleanup-worktrees [--apply] [--json]` | the worktrees under `prs.worktrees` (of the repos under the [`repos`](#repos) roots — nothing else is ever looked at), each with a verdict. **Removed** when its branch's PR (`gh pr list --head <branch> --state all`, newest; `pr-<N>`: PR N) is merged or closed, it has no uncommitted or untracked files, nothing on it is unpublished (`HEAD` is the PR's head or behind it, or every commit is on a remote branch), and no live session's cwd is in it. Kept otherwise, with the reason (PR still open, no PR for the branch, detached HEAD, local changes, unpushed commits, a session running in it, GitHub unreachable). Without `--apply` it only lists; `--apply` runs `git worktree remove` (never `--force`) and, for a merged PR, `git branch -D` (a squash merge leaves the branch unmerged as far as git knows); a closed PR's branch stays. `-n` never removes. `--json`: `{ applied, worktrees: [{ path, repoPath, branch, pr: { number, state, url, headOid } \| null, action: "remove" \| "keep", reason, deleteBranch, error? }] }` |
| `fleet pr spawn <pr> [prompt] [--name] [--model] [--harness] [--backend] [--tmux-session] [--window]` | `pr checkout`, then `spawn` in the worktree with the first prompt `contextLine` + `prompt`: which PR, its branch and base, the worktree path, and that `gh pr view` / `gh pr diff` show the rest. Starts an agent — confirm like `spawn` |

`--json`: `{ user, scope, fetchedAt, stale, error, truncated, columns: [{ id, title, count }], prs: [Pr…] }`
(times in ms epoch; `columns` in display order; `truncated`: a search had more than 200 hits). A `Pr` is
`{ column, owner, repo, number, title, url, author, draft, createdAt, updatedAt, additions, deletions,
changedFiles, size, baseBranch, headBranch, crossRepository, headOid, reviewDecision, checks, comments,
requestedReviewers, myReview, mention?, tasks?, rotting, local? }`: `size` is `XS`…`XL` by lines changed (≤10, ≤100, ≤500, ≤1000);
`reviewDecision` `approved` · `changesRequested` · `reviewRequired` · `null`; `checks` (the head
commit's combined CI) `success` · `failure` · `error` · `pending` · `expected` · `null`;
`requestedReviewers` logins and `@team` slugs; `myReview` `{ state, at, newCommits }` (your latest
review, `newCommits`: the head moved since) or `null`; `mention` `{ at, author, url, excerpt, answered, seen }`
(the latest @mention of you by someone else: where, the line it is on, whether you commented or
reviewed after it, whether that or `fleet pr seen` makes it seen) when there is one; `tasks` the
[Productive task](#productive-tasks) ids its title or description links to
(`app.productive.io/<org>/tasks/<id>`), left out when none; `rotting`
`fresh` · `warming` · `stale` · `rotting` · `critical` — how long it has waited (since it was
opened; in **unseenMentions** since the mention), by its column: **waitingOnMe** / **unseenMentions**
4h · 1d · 2d · 4d, **reviewMine** / **readyToMergeMine** 1d · 3d · 7d · 14d, **waitingOnAuthor**
2d · 5d · 10d · 14d, **draftMine** 3d · 7d · 14d · 30d (the age cell's colour in the text output); `local` `{ repoPath, worktree }` when the repo
is checked out here (`worktree`: where the head branch is checked out, or `null`).

## Productive tasks

Your open [Productive](https://productive.io) tasks — assigned to you, in projects that are not
archived — sorted into columns, cached, with each workflow's statuses so a task can be moved
along. Productive is reached over its REST API with `curl`; the token goes to `curl` on stdin,
never on its command line. Token: `$FLEET_PRODUCTIVE_TOKEN`, else `tasks.token` in
[the config](architecture.md#config), else the macOS keychain item whose service is `tasks.keychain`,
else `$PRODUCTIVE_API_TOKEN` (Productive → Settings → API
integrations). Config: `tasks`.

| command | does |
| --- | --- |
| `fleet tasks [--json] [--column C] [--refresh \| --cached]` | looks you up (`organization_memberships`: your person in `tasks.organization`, or in the only organization you belong to — several and none configured → error listing them), then your open tasks (`filter[assignee_id]`, `filter[status]=1`, `tasks.projects` when set; 200 per page up to 5 pages) with their project, workflow status and task list, and every workflow's statuses. Tasks in archived projects are left out. Columns, set at read time: **due** (due within 3 days, or overdue), else by the status's category: **inProgress** (started) · **toDo** (not started). Due by date, the rest by latest activity first. Fetches when the cache is older than `tasks.refresh` (default `5m`); `--refresh` always fetches, `--cached` never does. A failed fetch serves the cache with a warning (`stale`, `error`). `--column` takes `in-progress`, `inProgress`, … Cache: `${XDG_CACHE_HOME:-~/.cache}/fleet/tasks.json` (`FLEET_TASKS_CACHE`) |
| `fleet task show <task> [--json]` | one task: project, task list, status, due date, todos, description. `<task>` is its id (`1234`, `#1234`) or its Productive URL. From the cache, else fetched (any task, open or not). `--json`: the `Task` plus `contextLine` (the first prompt `task spawn` gives) and `statuses` (its workflow's) |
| `fleet task status <task> [status] [--json]` | without `status`, its workflow's statuses (`●` the current one). With one — an id, a name (any case) or a unique prefix of one; ambiguous or unknown → exit 2 with the candidates — moves the task there and updates the cache (a closed task leaves it). `-n` changes nothing. `--json`: `{ task, applied }` (the task as it is now) |
| `fleet task spawn <task> [prompt] [-C DIR] [--name] [--model] [--harness] [--backend] [--tmux-session] [--window]` | `spawn` in `DIR` (default: here) with the first prompt `contextLine` + `prompt`: `Productive task #<number> <url> — “<title>” (<project>, <status>).` (the URL early: the web UI links a session to a task by it, and sees only the first ~300 characters of a first prompt) Starts an agent — confirm like `spawn` |
| `fleet task schedule <task> [prompt] [--time HH:MM] [-C DIR] [--name] [--model] [--harness] [--json]` | = `fleet schedule add --task <task>`: a session on the task's start date at `--time` (default `08:00`) that follows the date if it moves — see [Scheduled sessions](#scheduled-sessions) |

`--json`: `{ user: { id, name }, organization: { id, name, slug }, scope, fetchedAt, stale, error,
truncated, columns: [{ id, title, count }], tasks: [Task…], statuses: { <workflowId>: [Status…] } }`
(times in ms epoch; `columns` in display order; `truncated`: more than 1000 open tasks). A `Task` is
`{ column, id, number, title, url, project: { id, name }, taskList, status, workflowId, dueDate,
startDate, createdAt, updatedAt, lastActivityAt, todos, openTodos, tags, description }`: `number`
is the project's task number; `url` `https://app.productive.io/<organization.slug>/tasks/<id>`;
`dueDate` / `startDate` `YYYY-MM-DD` or `null`; `description` plain text, at most 600 characters.
A `Status` is `{ id, name, category: "notStarted" | "started" | "closed", position }`; a
workflow's statuses are in position order, closed ones last. `column` is `closed` only on a task
`fleet task status` just closed.

## Repos

Keep the git checkouts under a few roots fresh, on every machine. A root that is a checkout itself (e.g. `~/work`) is that one repo; the repos inside it need their own root (`~/work/repos`). Fetch, then fast-forward only:
nothing is ever stashed, reset, rebased or merged — a repo that can't fast-forward is reported and
left alone. Config: `repos` in [the config](architecture.md#config), or the web UI's Settings → Git repos.

| command | does |
| --- | --- |
| `fleet repos [--json] [--all]` | each repo (`--all`: excluded ones too, `excluded: true`): branch, position (`↓behind ↑ahead ✎dirty`, as of its last fetch — no network), when it was last fetched, its interval and the last sync's outcome |
| `fleet repos sync [NAME…] [--due] [--notify] [-q] [--json]` | `git fetch --all --prune` (8 in parallel, 10 min timeout, ssh in batch mode), then: `merge --ff-only @{u}` on the checked-out branch when it is only behind; and the remote's default branch (`origin/HEAD`, e.g. `develop`) is fast-forwarded with `update-ref` when it is checked out nowhere. Outcomes: `updated` · `current` · `ahead` · `diverged` (both moved) · `blocked` (local changes in the way) · `busy` (rebase/merge/… in progress — fetched only) · `noUpstream` · `noRemote` · `fetchFailed`. `NAME` is a directory name or a `~/…` path. `--due`: only repos whose interval is up (a daily one is also due on a new local day; a failing fetch backs off 1, 2, 4… ticks). `--notify`: macOS notification when a repo newly diverges or is blocked, or fails to fetch 3 times running. `-q`: print only updates and problems, timestamped. One run at a time (a lock next to the state file; `-q` exits 0 quietly when locked). State: `${XDG_STATE_HOME:-~/.local/state}/fleet/repos.json` (`FLEET_REPOS_STATE`) |
| `fleet repos install-service` | macOS: a launchd agent `fleet.repos` that runs `fleet --local repos sync --due --notify -q` every 10 minutes and at login (`--uninstall`, `--no-load`, `--print`); logs to `~/Library/Logs/fleet.repos.log`. On another host: `fleet -H <host> repos install-service` |

`--json` of `sync` is the list of results `{ name, path, branch, outcome, pulled, defaultBranch?: { branch, pulled }, detail? }`;
of `repos` the rows `{ name, path, every, excluded, due, branch, upstream, ahead, behind, dirty, busy, lastAttempt, lastFetch, outcome, detail }` (times in ms epoch, `every` in seconds).

## Bots

Agent sessions started from a recipe, like Claude Code routines but on your own machines: a prompt
and a cron schedule — or a [GitHub trigger](#github-triggers): a new branch matching a pattern —
that spawn a session on the host whose config lists the bot. Everything that
session spawns in turn (`fleet spawn`, `fleet stack spawn`, a handoff — on this host or, through
`-H`, another) belongs to the same **run**. Config: `bots` in [the config](architecture.md#config).

Bots used to be called **jobs**: `fleet jobs` is an alias of `fleet bots`, a `jobs` config
section is still read (a name in both: the `bots` one wins), and the names below that carry `job`
— the web API (`/api/hosts/:host/jobs`), `--json` fields, `origin.job`, `FLEET_JOB_RUN`,
`FLEET_JOBS_DIR`, the state dir and the launchd label `fleet.jobs` — keep it.

```json
"bots": {
  "triage": {
    "schedule": "0 7 * * 1-5",
    "dir": "~/Code/project",
    "promptFile": "~/notes/triage.md",
    "command": "claude --dangerously-skip-permissions",
    "model": "opus",
    "keep": "24h"
  }
}
```

| key | meaning |
| --- | --- |
| `schedule` | 5-field cron in local time: `*`, lists, ranges, steps (`*/15`, `5/20`), names (`mon-fri`, `jan`), `7` = Sunday, `@hourly` / `@daily` / `@weekly` / `@monthly`. Both day fields set → either matches (classic cron) |
| `dir` | where the session starts (must exist) |
| `pull` | `true`: bring the git checkout `dir` is in up to date before each run, as [`fleet repos sync`](#repos) does for one repo (fetch, then fast-forward only; never stash, reset or merge). When it can't (local changes, diverged, offline, not a checkout) the run starts anyway: the run records it (`pull: { outcome, pulled, warning }`, shown by `fleet bots show`) and the prompt starts with a line saying files there may be out of date. Default `false` |
| `prompt` / `promptFile` | the first prompt, inline or read from a file at run time (one of them, or neither); a file's YAML frontmatter (`---` … `---`) is left out, so a note can be the prompt |
| `command` | launcher for the bot's sessions **and everything they spawn** (it travels as `FLEET_CMD`); default config `claude` |
| `harness`, `model` | as for `fleet spawn` |
| `ghIdentity` | the [GitHub identity](#github-identities) the bot's sessions (and what they spawn) act as on GitHub; default none — yours |
| `enabled` | `false` pauses the bot (`fleet bots disable`). `fleet config set bots.<name> null` removes it |
| `keep` | default `72h`, or `"never"`: once a run is older than this, its sessions that are idle and quiet for an hour are closed (their tmux session is killed). A session waiting for input is never closed. A finished bot session shows as done before that: its prompt carries the [ask-done](#ask-done) instruction |
| `closeDone` | default `24h`, or `"never"`: a session that reported itself done (`fleet done`) is closed this long after the mark, while it is idle and nobody prompted it since (a prompt after `done` drops the mark). Whichever of `keep` and `closeDone` comes first closes it |

A bot never fires on the tick that first sees it (a bot added at 08:00 with a 07:00 schedule first
runs tomorrow). A slot more than 6 hours old when a tick sees it (the machine was off) is recorded
as `missed`; a slot that comes while a session of the bot is still working is `skipped`.

| command | does |
| --- | --- |
| `fleet bots [list] [--json]` | each bot: schedule, next run, last run, live sessions (working / needs you) |
| `fleet bots show <name> [--json]` | settings and the latest runs, each with its sessions as a tree (`└` = spawned by a bot session) and their state (`busy` / `idle` / `waiting` / `closed` / `gone`) |
| `fleet bots run <name> [--force] [--prompt <text>]` | start a run now (`trigger: manual`); refuses while a session of the bot is working unless `--force`. `--prompt` is added after the recipe's prompt — a task for this run, with the recipe's context; dir, launcher, model, `ghIdentity` and `keep` stay the recipe's: a bot's other task (acting on its own report, say) runs as the bot, with whatever GitHub identity this host gives it |
| `fleet bots tick [-q]` | fire due bots, record missed / skipped slots, close sessions past `keep` or done for `closeDone` — what the service runs. Exits 1 when a bot failed to start |
| `fleet bots set <name> [--keep <span>] [--close-done <span>]` | change `keep` / `closeDone` (`6h`, `2d`, `30m`, or `never`); the Bots screen edits them in place |
| `fleet bots enable\|disable <name>` | set `<section>.<name>.enabled` (`bots`, or `jobs` for a bot still there) |
| `fleet bots install-service` | macOS: a launchd agent `fleet.jobs` that runs `fleet --local jobs tick -q` every minute and at login (`--uninstall`, `--no-load`, `--print`); logs to `~/Library/Logs/fleet.jobs.log` |

A run's first session is the tmux session `<bot>-<MMDD>-<HHMM>` (Claude `-n` the same). Every bot
session has `FLEET_JOB_RUN=<bot>/<run>` in its tmux environment and the pane option
`@fleet_job_run`; `fleet list` reports it as `origin: { job, run }`. A closed bot session that
`fleet restore` brings back keeps both and its bot's launcher, and is open in its run's record again.

`--json` of `bots` / `bots show` is `{ name, schedule, scheduleText (the schedule in plain English), enabled, dir, harness, model, command, keep, closeDone,
prompt: { inline } | { file } | null, nextRun, lastSlot, runs, live }` per bot (`runs`: the latest one
for `bots`, all kept for `show`, newest first: `{ id, trigger, startedAt, slot, outcome, reason,
promptFile, sessions: [{ tmuxSession, name, parent, at, closedAt }] }`; `live`: `{ run, sessionId,
tmuxSession, status, title }`; times in ms epoch, `keep` and `closeDone` in seconds, `null` = never). State:
`${XDG_STATE_HOME:-~/.local/state}/fleet/jobs/<bot>.json` plus each run's prompt in `<bot>/<run>.md`
(`FLEET_JOBS_DIR`); the last 50 runs are kept.

In the web UI, the **Bots** screen (`#/bots`; `#/jobs` still opens it) lists every host's bots with
Run now and the enabled switch, `keep` / `closeDone` editable in place, and each run's sessions as a
tree. With a shared shed configured (`hosts.<name>.shared`) it has two tabs, **My bots** and
**Shared** (one group per shed); bot sessions wear a bot chip and
stay out of the session list unless they need you — the **Bots** status filter (bot icon) lists only them ([Web API](architecture.md#web-api)).

## GitHub triggers

A bot can start on GitHub instead of on a clock: with `on.github` in place of `schedule`, it
starts one run for every new branch whose name matches a pattern — a release pipeline that
creates `release/<n>` gets a session per release. The machines are not reachable from GitHub, so
there is no webhook: the tick polls.

```json
"bots": {
  "release-bot": {
    "on": { "github": { "repo": "owner/repo", "branch": "release/*" } },
    "sessionName": "release-bot {branch}",
    "dir": "~/Code/project",
    "promptFile": "~/notes/release-bot.md",
    "ghIdentity": "bot",
    "keep": "never"
  }
}
```

| key | meaning |
| --- | --- |
| `on.github.repo` | `owner/repo` |
| `on.github.branch` | branch pattern: `*` matches within one `/` segment, `**` across them, `?` one character (`release/*`, `hotfix/**`) |
| `on.github.every` | seconds between polls (`90`, `"90s"`, `"5m"`); default and minimum 60 |
| `on.github.backfill` | `true`: the first poll starts runs for the branches already there too (default: it only remembers them) |
| `sessionName` | the run's session name, a template; default `{bot} {branch}` (tmux: `release-bot-release-42`) |
| `prompt` / `promptFile` | as for a scheduled bot, plus the variables `{branch}`, `{sha}` (its head when found), `{repo}`, `{url}` (the branch on github.com), `{bot}`; none set = `New branch {branch} in {repo} (head {sha}): {url}` |

Everything else (`dir`, `command`, `harness`, `model`, `ghIdentity`, `enabled`, `keep`) is as
for any bot. A bot has `schedule` or `on`, not both.

**How it polls.** `fleet bots tick` (the `fleet.jobs` launchd agent, every minute — install it
with `fleet bots install-service` on the host that lists the bot) polls each GitHub bot every
`every` with one `gh api -i repos/<repo>/git/matching-refs/heads/<prefix>` (the pattern's literal
leading directories, e.g. `release/`), matching the pattern on what comes back. It sends the
previous answer's ETag (`If-None-Match`), so an unchanged answer is a `304` that GitHub does not
count against the rate limit. It runs as the bot's `ghIdentity` (that identity's `command`, e.g. a
GitHub App's `gh` wrapper) when it has one, else as the machine's `gh` (`$FLEET_GH`, config `gh`,
PATH). A failed poll — auth (401/403), not found (404), rate limited (403/429 with
`x-ratelimit-remaining: 0` or `retry-after`), gh missing — is logged and recorded, and the next one
waits 5 minutes (a rate limit: until `retry-after` / `x-ratelimit-reset`); when fewer than 50
requests remain, polling waits for the reset.

**Exactly one run per branch.** Polling compares states, not events: each poll is the set of
matching branches now, compared with the set the bot remembers (its state file, below).

- A branch it does not know starts one run (`trigger: github`). The branch is written to the state
  *before* the session is spawned, so a tick that dies halfway does not start it twice.
- A branch it knows never starts another — through restarts of fleet, reboots, missed polls, a
  new head (`sha` is updated, nothing runs).
- When a session with the run's name is already open (state lost, or `forget` while it runs), no
  second one starts: the branch is recorded as `adopted`.
- The first poll (and the first after `enabled` goes back to `true`, or after `repo` / `branch`
  change) only remembers what is there — like a scheduled bot, which never fires on the tick that
  first sees it — unless `backfill`.
- A branch that disappears is dropped from the state; its run gets `subject.goneAt`, its session is
  **not** closed (only `keep` and `closeDone` close bot sessions; set both to `"never"` for sessions
  that must stay until you close them). `fleet list` shows `[branch gone: release/42]` before its title,
  `fleet list --json` has it as `origin.subject.goneAt`.
- A run that fails to start (a missing `dir`, an unknown `ghIdentity`) is `failed` and is not
  retried by itself: fix the cause, then `fleet bots forget <bot> <branch>`.

| command | does |
| --- | --- |
| `fleet bots check <name> [--json]` | poll now, whatever `every` says, and act on it like a tick; then the branches it remembers. Exits 1 when the poll or a start failed |
| `fleet bots check <name> --dry-run [--json]` | poll and show what a poll would do per matching branch — `start`, `adopt`, `remember` (first poll), `known` — and which remembered ones are gone; starts nothing, changes nothing |
| `fleet bots forget <name> <branch>` | drop the branch from what the bot remembers: if it still exists, the next poll starts a run for it again (not while its session is open) |
| `fleet bots run <name>` | refused for a GitHub bot — it runs per branch |

`fleet bots` shows a GitHub bot's trigger, its last poll (or the error) and the branches it started
runs for; `fleet bots show` all remembered branches with their sessions' state and the runs whose
branch is gone. `fleet doctor` reports each enabled GitHub bot's last poll — failing on a poll
error or on three missed polls in a row (the tick is not running) — and, on macOS, a missing tick
service when any bot is enabled. The tick's log: `~/Library/Logs/fleet.jobs.log`.

`--json` of `bots` / `bots show` adds `on` (`{ github: { repo, branch, every, backfill } }` or
null), `sessionName` and `github` (the poll state: `{ watch, armed, lastPoll, lastOk, nextPoll,
etag, error: { kind, message, at } | null, rateRemaining, branches: { "<branch>": { sha, firstSeen,
outcome: spawned | existing | adopted | failed | starting, run, tmuxSession, reason } } }`); a GitHub
bot's `schedule` is `""`, `scheduleText` the trigger in words, `nextRun` null. A triggered run has
`subject: { kind: "branch", repo, branch, sha, url, goneAt }` and an id `YYYYMMDD-HHMMSS-<branch>`.
The state is `github` in the bot's state file (`<bot>.json`, below). Tests poll a fixture instead
of GitHub: `FLEET_GH_WATCH_FIXTURE` = a JSON file `{ "<owner/repo>": [{ "name", "sha" }] }`.

## Resident bots (Slack)

A bot can live in a Slack channel instead of running now and then: with `on.slack` in place of
`schedule`, Fleet keeps **exactly one** session of it live, linked to that channel for good
(`fleet slack link --for forever`). A channel that people talk to (a CI channel, a review
channel) gets one standing session that follows it, while the work itself runs in other bots.

```json
"bots": {
  "ci-channel": {
    "on": { "slack": {
      "channel": "https://<workspace>.slack.com/archives/C0123ABCDEF",
      "mode": "proactive",
      "task": "CI support for this channel: …"
    } },
    "dir": "~/Code/project",
    "pull": true,
    "prompt": "Run bots/ci-channel/channel.md. Settings: ~/.config/fleet/bots/ci-channel/settings.json"
  }
}
```

| key | meaning |
| --- | --- |
| `on.slack.channel` | the channel's link (`…/archives/C…`); a whole channel, not a thread |
| `on.slack.mode` | `mention` (default: woken when mentioned, or in a thread it posted in) or `proactive` (every message) |
| `on.slack.task` | the link's instructions (`--task`, at most 1000 characters); default: follow the prompt |
| `sessionName` | the session's name; default the bot's name |
| `prompt` / `promptFile` | goes in with the link's intro (as `fleet spawn --slack`), so the session never starts on it before it knows it is in Slack |

Everything else (`dir`, `pull`, `command`, `harness`, `model`, `ghIdentity`, `enabled`) is as for
any bot. `keep` and `closeDone` are refused: nothing closes a resident's session. The host must
run the Slack bridge (`slack.host`).

**One session, kept.** Each tick (`fleet bots tick`, every minute) looks at the bot's live
sessions and the bridge's links:

- none live → it starts one and links it (`trigger: resident`);
- one live, linked to the channel → nothing;
- one live that lost its link (unlinked, the link failed) → it links that session again, the prompt
  with the intro (`trigger: relink`);
- more than one live → nothing; `fleet hosts` reports it, and a person closes the extra.

After a start or a relink the next one waits 5 minutes (the session may still be registering; a
failing bridge is not asked every minute). No bridge to ask → nothing is decided. `fleet bots run`
starts it now when none is live, and refuses when one is. To stop it, pause the bot
(`fleet bots disable`) and close its session; unlinking alone gets it linked again.

## Snooze

Set a session aside until a time, like snoozing a mail: `fleet snooze <target> --for 4h` (or
`--until "2026-11-13 09:00"`; Snooze in the app's session details offers 1 hour, 4 hours, this
evening, tomorrow morning, 1 day, next week and a custom date and time). Nothing stops: the
session keeps running. `fleet list` stamps `snooze: { at, until }` on the row; while `until` is
ahead the views leave it out of the session list, the board and the header counters and show it
under **Snoozed** (beside Inactive and Closed) with the time it comes back. Once `until` has
passed it is back in the list, on top, with a **Back** chip, until the user opens it (the app
then clears it) or prompts it. A prompt also ends a snooze early, and `--clear` ends it by hand.
`fleet bots tick` drops the snoozes of gone or re-prompted sessions.

Snoozes: `${XDG_STATE_HOME:-~/.local/state}/fleet/snooze.json` (`FLEET_SNOOZE_STATE`),
`{ "snoozes": [{ sessionId, tmuxSession, at, until }] }`, on the session's host.

## Ask done

A session reports that its work is finished: the agent runs `fleet done --note "…"`, then ends with a
short final message (what it did, what is worth checking). Nothing is closed: `fleet list` stamps
`done: { at, note }` on the row and the views show it — a Done chip and the note in place of the
first prompt in the session list and on the board, `done` in `fleet list`'s status column, and a
Done card at the end of the conversation with **Close** (two taps). The user reads it and closes it.
The mark counts while the session sits idle on it; a prompt from the user after it ends it (the
conversation went on; the agent runs `fleet done` again when it is finished again), and `fleet bots
tick` drops the marks of gone or re-prompted sessions.

The agent is told so by one instruction (run `fleet done` when finished and nothing waits on the
user, then the final message; otherwise say what it needs and stop). It goes below every
[bot](#bots) prompt and the prompt of everything a bot session spawns, below `fleet spawn
--ask-done`'s prompt, and into a running session with `fleet ask-done <target>`. A user-level
`CLAUDE.md` / `AGENTS.md` can carry the same rule, so every agent reports itself done.

Marks: `${XDG_STATE_HOME:-~/.local/state}/fleet/done.json` (`FLEET_DONE_STATE`),
`{ marks: [{ sessionId, pane, tmuxSession, at, note }] }`.

## Scheduled sessions

One-off sessions started later on this host — at a set time, or on a Productive task's start date.
No config: entries are state, added and cancelled with the commands below (or the web UI). The
`fleet.jobs` launchd agent ([`bots install-service`](#bots)) fires them: every `bots tick` ends
with the scheduled sessions, so a host needs that service for them to run.

| command | does |
| --- | --- |
| `fleet schedule add [prompt] --at <when> [-C DIR] [--name] [--model] [--harness] [--json]` | a session in `DIR` (default: here; must exist) at `<when>`: `YYYY-MM-DD HH:MM` (or with a `T`), `YYYY-MM-DD` (08:00), `HH:MM` (the next one: today, else tomorrow), `today\|tomorrow\|<weekday> HH:MM`, `in 90m` / `in 2h` / `in 3d` (or `+2h`), local time. `--gh-identity` (global) is kept for the session. Starts an agent later — confirm like `spawn` |
| `fleet schedule add [prompt] --task <task> [--time HH:MM] …` | task-linked: at `--time` (default `08:00`) on the task's start date. The tick re-reads the task every hour and right before firing: a moved start date moves the session, a closed task skips it (`skipped`), a task with no start date waits for one. The first prompt is `contextLine` + `prompt`, as with `task spawn`; the task's status is left alone. Session name default `task-<number>`. A closed task is refused; a start date already past fires on the next tick |
| `fleet schedule [list] [--json] [--all]` | pending entries by time, then the latest 5 finished (`--all`: all kept) |
| `fleet schedule cancel <id>` (`rm`) | cancel a pending entry (its id, or a unique prefix of a pending one) |

An entry runs in tmux, like a bot. A timed entry more than 6 hours late when a tick sees it (the
machine was off) is recorded as `missed`; a task-linked one fires late instead.

`--json` of `schedule` is `{ host, entries: [Entry…] }`, of `schedule add` the new `Entry`:
`{ id, at, time?, task?: { id, number, title, url, startDate, checkedAt }, dir, prompt, name,
model, harness, ghIdentity, createdAt, state: "pending" | "started" | "skipped" | "missed" |
"failed" | "cancelled", reason, firedAt, tmuxSession }` (times in ms epoch; `at` `null` while a
task has no start date). State: `${XDG_STATE_HOME:-~/.local/state}/fleet/schedule.json`
(`FLEET_SCHEDULE_FILE`): every pending entry and the latest 50 finished ones.

In the web UI: **New session → When: Later** schedules a session on the chosen host
(`datetime-local`, that host's clock); a task's dialog has **Now / On start date** (+ the time)
and lists the task's pending entries; the **Bots** screen lists every host's pending entries with
Cancel. Web API: `GET /api/hosts/:host/schedule`, `POST /api/hosts/:host/schedule` `{ at | task
(+ time), dir, prompt?, name?, model?, harness?, ghIdentity? }` → `{ host, entry }`,
`POST /api/hosts/:host/schedule/:id/cancel`.

## Web

| command | does |
| --- | --- |
| `fleet web serve [--port N] [--bind ADDR] [--dir PATH]` | run `node <web.dir>/server.mjs` with this machine's config (needs Node ≥ 22) |
| `fleet web build [--dir PATH] [--install]` | build the React UI: `npm --prefix <web.dir>/ui ci` (when `node_modules` is missing, or `--install`) then `npm … run build` → `ui/dist`, which the server then serves by default. Needs a checkout (an installed web dir has only `ui/dist`) |
| `fleet web install-service` | macOS: write and load a launchd agent that keeps `fleet web serve` running (`--uninstall`, `--no-load`, `--print`); logs to `~/Library/Logs/fleet.web.log`, runs as `ProcessType=Interactive` so macOS does not throttle it like a background job (that made `fleet list` take 10s+ under Low Power Mode); elsewhere: print a systemd user unit (`fleet-web.service`; `scripts/deploy.sh` restarts it with `systemctl --user restart fleet-web` when it is enabled). The agent pins one node (config `web.node`, else `/opt/homebrew/bin/node` or `/usr/local/bin/node`, else `PATH`) and leaves version-manager dirs (nvm/volta/fnm) out of its `PATH`; if the only node is version-managed it is used with a warning |

## Environment

| variable | meaning |
| --- | --- |
| `FLEET_CONFIG` | config file path (default `${XDG_CONFIG_HOME:-~/.config}/fleet/config.json`) |
| `FLEET_HOST` | default target host (same as `-H`) |
| `FLEET_DRY_RUN=1` | print what would run, run nothing |
| `FLEET_IGNORE_LOAD=1` | start sessions on a machine under heavy load (same as `--ignore-load`) |
| `FLEET_NO_FALLBACK=1` | keep the asked-for harness when its subscription is at its limit (same as `--no-fallback`) |
| `FLEET_LOAD_FILE` | where the CLI reads the load verdict (default `${XDG_STATE_HOME:-~/.local/state}/fleet/load.json`, written by the web server) |
| `FLEET_DEBUG=1` | trace what runs, on stderr |
| `FLEET_CONNECT_TIMEOUT` | ssh connect timeout in seconds (default 5) |
| `FLEET_REMOTE_TIMEOUT` | limit for one captured remote call, e.g. a host in `list --all-hosts` (default 15 s) |
| `FLEET_MUX=0` | disable ssh connection multiplexing (control socket `~/.ssh/fleet-mux-%C`) |
| `FLEET_TMUX` | tmux binary (over config `tmux`) |
| `FLEET_CMD` | command that launches Claude in spawned sessions (over config `claude`; default `claude`). Codex / pi: config `harnesses.<id>.command` |
| `CODEX_HOME` / `PI_CODING_AGENT_DIR` | where Codex / pi keep their state (default `~/.codex`, `~/.pi/agent`) — discovery reads transcripts from there |
| `FLEET_BIN` | `fleet` binary the web server calls |
| `FLEET_NODE` | `node` binary for `fleet web serve` (over config `web.node`; set by the launchd agent) |
| `FLEET_WEB_PORT`, `FLEET_WEB_BIND`, `FLEET_WEB_UI` | web server listen address / UI directory (over config `web.*`) |
| `FLEET_BRIEFS_DIR` | where [briefs](#briefs) live (default `${XDG_STATE_HOME:-~/.local/state}/fleet/briefs`); shared with the web server |
| `FLEET_GH` | `gh` binary for [`fleet prs`](#pull-requests) (over config `gh`) |
| `FLEET_PRS_CACHE` | the [`fleet prs`](#pull-requests) cache file |
| `FLEET_PRS_SEEN` | where [`fleet pr seen`](#pull-requests) keeps its marks |
| `FLEET_PRODUCTIVE_TOKEN` / `PRODUCTIVE_API_TOKEN` | Productive API token for [`fleet tasks`](#productive-tasks) (the first over config `tasks.token`, the second under it) |
| `FLEET_TASKS_CACHE` | the [`fleet tasks`](#productive-tasks) cache file |
| `FLEET_CURL`, `FLEET_PRODUCTIVE_API` | `curl` binary and API base URL for [`fleet tasks`](#productive-tasks) (tests) |
| `FLEET_GH_IDENTITY` | the [GitHub identity](#github-identities) a session acts as — set in its shell; spawns inherit it (and it is forwarded to `-H` hosts). Same as `--gh-identity` |
| `FLEET_JOB_RUN` | `<bot>/<run>`: the [bot](#bots) run a session belongs to — set in bot sessions; a spawn with it set joins that run (and is forwarded to `-H` hosts) |
| `FLEET_SCHEDULE_FILE` | [scheduled sessions](#scheduled-sessions) state (default `${XDG_STATE_HOME:-~/.local/state}/fleet/schedule.json`) |
| `FLEET_JOBS_DIR` | [bot](#bots) state (default `${XDG_STATE_HOME:-~/.local/state}/fleet/jobs`) |
| `FLEET_STACKS_DIR` | where [stacks](#stacks) live (default `${XDG_STATE_HOME:-~/.local/state}/fleet/stacks`) |
| `VISUAL`, `EDITOR` | the editor for `fleet brief --edit`, `fleet stack edit` and `fleet config edit` (default `vi`) |
| `FLEET_SNAPSHOT` | the [session-recovery](#session-recovery) snapshot (default `${XDG_STATE_HOME:-~/.local/state}/fleet/snapshot.json`) |
| `FLEET_BOOT_ID` | override this boot's id (tests; a different value simulates a reboot) |
| `FLEET_GROUPS_STATE` | `fleet group` state file (default `${XDG_STATE_HOME:-~/.local/state}/fleet/groups.json`) |
| `FLEET_FIXTURE=<sessions.json>` | read a canned fleet from a file instead of the live registry (demo/tests; backends are inert) |
| `NO_COLOR=1` | no colors |

## Exit codes

| code | meaning |
| --- | --- |
| 0 | success |
| 1 | error, bad usage |
| 2 | tmux, `stack` and `restore` commands: ambiguous match (candidates are printed) |
| 3 | `rename --json`: held; `brief --set`/`--edit`: the brief changed since it was opened (nothing saved); `brief --regenerate`: hourly cap reached; `stack set`/`edit`: the StackBrief changed since it was opened; `stack`: no stack matches; `restore`: no dormant (with `--closed`: recently closed) session matches; tmux commands: nothing to act on — no sessions, no match, or a confirmation was needed but there is no terminal (use `-f`) |
| 4 | host unreachable |
| 127 | a required tool (tmux, node) was not found |

`exec` and `ssh` return the remote command's own exit code.
