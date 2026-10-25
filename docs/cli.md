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
| `fleet list [--json] [-a, --all-hosts]` (alias `ls`) | live sessions: title, status, age, context usage (`ctx 62%`), terminal, cwd. `--json` is an array whose rows all carry `host`; `-a` queries every configured host in parallel. The text view ends with `N dormant (fleet restore)` when a reboot left [dormant sessions](#session-recovery). Every local `list` also records the [snapshot](#session-recovery) |
| `fleet peek <target> [--lines N]` | what the session's terminal shows now (default 40 lines) |
| `fleet send <target> <text>` | type text into the session and press Enter |
| `fleet rename <target> <title> [--no-tmux-sync] [--force] [--json]` | rename the session's one title: Claude's `/rename` (the source of truth), then its tmux session follows as a slug of it (`Fix Login` → `fix-login`, `-2` on a collision) when the tmux session is that Claude session's own (one window, one pane). A session **waiting on a prompt** is held (exit 1; `--json`: exit 3) — typed keys would be its answer; busy sessions are fine (Claude runs `/rename` mid-turn without disturbing the turn). Codex gets `/rename`, pi `/name`; a busy Codex / pi session is held too (`held: "busy"`) — they would queue the text as a message. `--force` overrides the hold. `--json` prints `{ ok, result: renamed\|sent\|held, session_id, pid, host, from, title, held, tmux: { renamed, from, to, note }, message }` |
| `fleet name [<target> \| --all [--settled]] [--apply] [--refresh] [--no-tmux-sync]` | suggest a name from what the session is doing; only `--apply` sends it (through the same path as `rename`, tmux included). `--all` = every session with a derived (cwd+hash) name. A derived-name session alone in a tmux session somebody named (`fleet new fix-login`) adopts that name (`(tmux)`, no model call) instead of getting a generated one. `--settled` (with `--all`) skips sessions with one prompt and no brief yet — the model reads the brief's Summary and the first three prompts, and a lone first prompt is often just a link. `--refresh` ignores the cache |
| `fleet group [-a, --all-hosts] [--json] [--apply] [--refresh] [--consolidate] [--input <file\|->] [--cached]` | sort sessions into work-stream groups (the web Board view) — see [Grouping](#grouping). Reads sessions and asks `claude -p`; never sends anything to a session |
| `fleet group --rename <group id> --label <l>` / `--move <host/id> (--to <group id> \| --label <l>)` / `--create --label <l>` / `--delete <group id>` | edit the stored groups by hand (the Board's rename / drag'n'drop) — see [Grouping](#grouping) |
| `fleet spawn [prompt] --dir <path> [--harness claude\|codex\|pi] [--name <n>] [--model <id>] [--backend iterm\|tmux] [--tmux-session <s>] [--window]` | start a new agent session in a new tab/pane. `--harness` picks the coding agent (default `claude`; see [Harnesses](#harnesses)). `--model <id>` runs `<harness> --model '<id>'` (letters, digits and `._[]-/:` only — pi's ids are `provider/model`; blank = the harness's default). `--name` is passed as `-n` to Claude and pi; Codex has no such flag (the tmux session carries the name). Default backend: tmux inside tmux, over ssh or off macOS; else iTerm. With tmux, each spawn gets its own tmux session (one session per job) named from `--name` or the dir's basename, sanitised like `fleet new`; a taken `--name` is an error, a taken basename is uniquified (`app-2`). `--tmux-session <s>` opens a window in `s` instead (created if missing) |
| `fleet harness list [--json] [--no-models]` | the [harnesses](#harnesses) on this host: id, label, launch command, `installed`, and the models each offers for a picker. `-H <host>` asks another machine |
| `fleet spawn --from <target> [prompt] [--name <n>] [--model <id>] [--backend …]` | continue a session's work in a new one: same host (with `-H`, the host the session is on) and its cwd (`--dir` overrides), first prompt = its [brief](#briefs)'s continue prompt, then a blank line and `prompt`. The prompt goes through a file under `~/.claude/fleet-handoffs/`, like a handoff. Errors when the session has no brief yet. `-n` prints the launch command and the prompt and writes nothing |
| `fleet handoff [brief] [--file <f\|->] --dir <path> [--harness <h>] [--name <n>] [--model <id>] [--tmux-session <s>] [--tab] [--no-wait]` | start a new session in another window seeded with a brief (saved under `~/.claude/fleet-handoffs/`); waits until it registers. Same tmux placement as `spawn` |
| `fleet brief <target> [--json \| --prompt]` | print the session's [brief](#briefs) — its body (Summary / Resources / Todos, no frontmatter); `--json` the parsed shape; `--prompt` only the continue prompt |
| `fleet brief <target> --edit` / `--set [--expect-updated <iso>]` / `--regenerate` / `--open` | edit it in `$VISUAL`/`$EDITOR`; save markdown from stdin; ask the web server to regenerate it; open the session's checkout in VS Code — see [Briefs](#briefs) |
| `fleet stack list\|show\|new\|ensure\|spawn\|add\|remove\|rename\|edit\|set\|sync\|rm …` | [session stacks](#stacks): sessions that share one context file (the StackBrief) |
| `fleet stack spawn <target> [prompt] [--harness <h>] [--name <n>] [--model <id>] [--dir <path>] [--on <host>] [--no-wait]` | spawn a **sibling** of `<target>`: a new session in its cwd (`--on`: on another host), in its stack (created around it first when it has none), told where the StackBrief is — see [Stacks](#stacks) |
| `fleet watch [--interval 5] [--stuck 300] [--quiet] [--rows 1\|2\|auto] [--no-mouse]` | live dashboard + notifications on finished/stuck sessions |
| `fleet usage [--json] [--refresh]` | the Claude subscription's usage limits on this host's login — current session (5h), weekly all models, weekly per model (e.g. Fable), extra usage — as bars with reset times; the numbers Claude Code's `/usage` shows. Cached 60s (`--refresh` skips it); a failed read serves the last one marked stale and backs off 2 minutes. `--json`: see [architecture](architecture.md#subscription-usage). `-H <host>` reads another machine's login |
| `fleet skill install [--force]\|show` | install / print the Claude Code skill (`~/.claude/skills/fleet/SKILL.md`; `--force` overwrites a different one) |

`send`, `rename --force`, `name --apply` and `spawn` change a live agent's state — scripts and agents
should confirm before running them.

### Harnesses

fleet drives three coding agents: **Claude Code** (`claude`, the default), **Codex** (`codex`) and
**pi** (`pi`). Every session row carries `harness`; `spawn`, `handoff` and `stack spawn` take
`--harness`. Codex and pi have no live registry like Claude's, so they are found with `ps`
(interactive runs with a terminal — `codex exec`, `pi -p` and the like are not sessions) and
matched to their transcript by cwd and start time (see
[architecture](architecture.md#other-harnesses)). Peek, send, keys, kill, rename (`/rename` in
Codex, `/name` in pi — held while the session is busy) and restore (`codex resume <id>`,
`pi --session <id>`) work the same; briefs, naming and the web's question answering are
Claude-only for now.

Launch commands: Claude's is `FLEET_CMD` / config `claude`; the others' are config
`harnesses.<id>.command` (default the bare name). `fleet harness list --json`:

```json
[{ "id": "codex", "label": "Codex", "command": "codex", "installed": true,
   "models": [{ "id": "", "label": "Default (gpt-6.1-sol)" }, { "id": "gpt-6.1-sol", "label": "GPT-6.1-Sol" }] }]
```

Models: Claude's built-in list; Codex's from its `models_cache.json` (listed ones, its order);
pi's from `pi --list-models` (ids `provider/model`). `id: ""` is the harness's own default.

### GitHub identities

A session can act on GitHub as a GitHub App (a bot) instead of as you — for comments, reviews
and approvals that come from automation. Which identity a session has is decided by whoever
spawns it (`--gh-identity <name>`, a bot's `ghIdentity`), never by the agent inside it.
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
| `fleet stack list [--json]` | the host's stacks (after a sync): label, id, live/closed counts, cwd, updated. `--json`: `{ host, stacks: [StackView…] }` |
| `fleet stack show <stack> [--json \| --path]` | the StackBrief body (no frontmatter); `--json` the StackView; `--path` only the file path |
| `fleet stack new --from <target> [--label <l>] [--no-llm] [--json]` | create a stack around a live session (error if it is in one already — says which). The model writes Summary / Resources and a label (unless `--label`); `--no-llm`, `FLEET_FIXTURE`, `stacks.enabled: false` or a failing call give the skeleton (Summary "Started from <title> in <cwd>.", Resources = the Git line + Folder) — a failed call never blocks the stack: `generated: false` and a `warning` |
| `fleet stack ensure <target> [--label] [--no-llm] [--json]` | the session's stack, created like `new` when it has none |
| `fleet stack spawn <target> [prompt] [--harness] [--name] [--model] [--dir] [--backend iterm\|tmux] [--tmux-session <s>] [--window] [--on <host>] [--no-wait] [--label] [--no-llm] [--json]` | a sibling: `ensure` on `<target>`, then a new session on this host in `<target>`'s cwd (`--dir` overrides) whose first prompt is the context line + `prompt`, always through a prompt file `~/.claude/fleet-handoffs/<stamp>-stack-<hex>.md`; then waits (≤ 75 s) for it to register and adds it. `--on <host>`: the session starts on that host instead — `fleet --local spawn` there over ssh, in `--dir` (a path there; default `<target>`'s cwd as `~/…`, the same place on that machine) with the remote context line + `prompt` as the argument; it then waits for it in that host's `fleet list` and adds it as a member of that host (`spawned.host`, `session.host` in `--json`). `--no-wait` skips the waiting and prints the `fleet stack add` to run later. `-n` / `FLEET_DRY_RUN` prints the launch line (the ssh line with `--on`) and the prompt and writes nothing (not even a new stack). Starts an agent — confirm like `spawn` |
| `fleet stack add <stack> <target> [--on <host>] [--input <src>] [--json]` | add a live session (refused when it is live in another stack of this host). `--on <host>`: a session running on that host — `<target>` is resolved among its rows, from `fleet list --json` over ssh, or from `--input` (`-` = stdin: a `list --json` array or an `/api/fleet` body carrying that host; what the web server passes, so it never needs ssh). Naming this machine is the plain form |
| `fleet stack remove <stack> <target\|session_id> [--json]` | drop a member (a gone one by its session id or a prefix of ≥ 4, or its name); removing the last leaves an empty stack (the file stays) |
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
and those sessions must survive that). The `watch` header shows `· N dormant` while there are any.

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
| `fleet restore --all [-n] [--json]` | bring every dormant session back (a failure is reported and the rest still go) |
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
  title, cwd }] }`. `target` is what `fleet restore <target>` takes (the tmux name, or the full
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
| `--web-port`, `--web-bind`, `--web-dir` | web server settings (`--web-dir` defaults to auto-detected) |
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

Keep the git checkouts under a few roots fresh, on every machine. Fetch, then fast-forward only:
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
and a cron schedule that spawn a session on the host whose config lists the bot. Everything that
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
| `prompt` / `promptFile` | the first prompt, inline or read from a file at run time (one of them, or neither); a file's YAML frontmatter (`---` … `---`) is left out, so a note can be the prompt |
| `command` | launcher for the bot's sessions **and everything they spawn** (it travels as `FLEET_CMD`); default config `claude` |
| `harness`, `model` | as for `fleet spawn` |
| `ghIdentity` | the [GitHub identity](#github-identities) the bot's sessions (and what they spawn) act as on GitHub; default none — yours |
| `enabled` | `false` pauses the bot (`fleet bots disable`). `fleet config set bots.<name> null` removes it |
| `keep` | default `24h`, or `"never"`: once a run is older than this, its sessions that are idle and quiet for an hour are closed (their tmux session is killed). A session waiting for input is never closed |

A bot never fires on the tick that first sees it (a bot added at 08:00 with a 07:00 schedule first
runs tomorrow). A slot more than 6 hours old when a tick sees it (the machine was off) is recorded
as `missed`; a slot that comes while a session of the bot is still working is `skipped`.

| command | does |
| --- | --- |
| `fleet bots [list] [--json]` | each bot: schedule, next run, last run, live sessions (working / needs you) |
| `fleet bots show <name> [--json]` | settings and the latest runs, each with its sessions as a tree (`└` = spawned by a bot session) and their state (`busy` / `idle` / `waiting` / `closed` / `gone`) |
| `fleet bots run <name> [--force] [--prompt <text>]` | start a run now (`trigger: manual`); refuses while a session of the bot is working unless `--force`. `--prompt` is added after the recipe's prompt — a task for this run, with the recipe's context; dir, launcher, model, `ghIdentity` and `keep` stay the recipe's: a bot's other task (acting on its own report, say) runs as the bot, with whatever GitHub identity this host gives it |
| `fleet bots tick [-q]` | fire due bots, record missed / skipped slots, close sessions past `keep` — what the service runs. Exits 1 when a bot failed to start |
| `fleet bots enable\|disable <name>` | set `<section>.<name>.enabled` (`bots`, or `jobs` for a bot still there) |
| `fleet bots install-service` | macOS: a launchd agent `fleet.jobs` that runs `fleet --local jobs tick -q` every minute and at login (`--uninstall`, `--no-load`, `--print`); logs to `~/Library/Logs/fleet.jobs.log` |

A run's first session is the tmux session `<bot>-<MMDD>-<HHMM>` (Claude `-n` the same). Every bot
session has `FLEET_JOB_RUN=<bot>/<run>` in its tmux environment and the pane option
`@fleet_job_run`; `fleet list` reports it as `origin: { job, run }`.

`--json` of `bots` / `bots show` is `{ name, schedule, scheduleText (the schedule in plain English), enabled, dir, harness, model, command, keep,
prompt: { inline } | { file } | null, nextRun, lastSlot, runs, live }` per bot (`runs`: the latest one
for `bots`, all kept for `show`, newest first: `{ id, trigger, startedAt, slot, outcome, reason,
promptFile, sessions: [{ tmuxSession, name, parent, at, closedAt }] }`; `live`: `{ run, sessionId,
tmuxSession, status, title }`; times in ms epoch, `keep` in seconds). State:
`${XDG_STATE_HOME:-~/.local/state}/fleet/jobs/<bot>.json` plus each run's prompt in `<bot>/<run>.md`
(`FLEET_JOBS_DIR`); the last 50 runs are kept.

In the web UI, the **Bots** screen (`#/bots`; `#/jobs` still opens it) lists every host's bots with
Run now and the enabled switch, and each run's sessions as a tree; bot sessions wear a bot chip and
stay out of the session list unless they need you — the **Bots** status filter (bot icon) lists only them ([Web API](architecture.md#web-api)).

## Web

| command | does |
| --- | --- |
| `fleet web serve [--port N] [--bind ADDR] [--dir PATH]` | run `node <web.dir>/server.mjs` with this machine's config (needs Node ≥ 22) |
| `fleet web build [--dir PATH] [--install]` | build the React UI: `npm --prefix <web.dir>/ui ci` (when `node_modules` is missing, or `--install`) then `npm … run build` → `ui/dist`, which the server then serves by default. Needs a checkout (an installed web dir has only `ui/dist`) |
| `fleet web install-service` | macOS: write and load a launchd agent that keeps `fleet web serve` running (`--uninstall`, `--no-load`, `--print`); logs to `~/Library/Logs/fleet.web.log`, runs as `ProcessType=Interactive` so macOS does not throttle it like a background job (that made `fleet list` take 10s+ under Low Power Mode); elsewhere: print a systemd user unit. The agent pins one node (config `web.node`, else `/opt/homebrew/bin/node` or `/usr/local/bin/node`, else `PATH`) and leaves version-manager dirs (nvm/volta/fnm) out of its `PATH`; if the only node is version-managed it is used with a warning |

## MCP server

`fleet mcp` serves the CLI as a [Model Context Protocol](https://modelcontextprotocol.io) server on
stdio (newline-delimited JSON-RPC). Register it once:

```sh
claude mcp add --scope user fleet -- fleet mcp          # this machine (tmux tools: defaultHost)
claude mcp add --scope user fleet-ws -- fleet -H workstation mcp   # every call defaults to that host
```

- **One tool per subcommand**, generated from the CLI's own argument definitions, so it never
  drifts: `fleet tmux list` is `tmux_list`, `fleet pr cleanup-worktrees` is
  `pr_cleanup_worktrees`. Flags and positionals become typed properties (booleans, integers, enums,
  string arrays); help texts become descriptions. `tools/list` is the authoritative catalog.
- **Every tool** also takes `host` (= `-H`, overrides the one given to `fleet mcp`) and `dry_run`
  (= `-n`). `brief`, `stack_set`, `handoff` and `group` take `stdin` (for `--set`, `--file -`,
  `--input -`). Read-only commands carry `readOnlyHint`.
- **A call runs `fleet <argv>`** (positionals after `--`) with no terminal, `NO_COLOR=1` and
  `FLEET_NO_TTY=1`, and returns stdout, then stderr under `[stderr]`. A non-zero exit is a tool
  error (`isError`) that names the command and the exit code. Calls run concurrently; each is
  killed after `FLEET_MCP_TIMEOUT` seconds (default 600).
- **Left out** — they need a terminal or never return: `watch`, `enter`, `last`, `new`, `ssh`,
  `tmux enter`, `tmux last`, `web serve`, `stack edit`, `config edit`, `brief --edit`,
  `exec --tty`. `tmux_new` always detaches. Confirmations (`tmux kill`, `stack rm`) can't be
  answered: pass `force`.
- **`--tools a,b,stack_*` / `--exclude …`** narrow the catalog (`*` suffix globs). Each call is
  logged to stderr: time, tool, ok/error, duration and the argv (cut at 200 characters).

### Over HTTP (`--http`)

For an agent runtime that speaks MCP over HTTP (e.g. a hosted agent's custom connectors):

```sh
umask 077; openssl rand -hex 32 > ~/.config/fleet/mcp-token
fleet mcp --http [--bind 127.0.0.1] [--port 7778] --token-file ~/.config/fleet/mcp-token
```

- Streamable HTTP, stateless: `POST /mcp` takes one JSON-RPC message (or a batch) and answers
  `application/json`; notifications get `202`; `GET` is `405` (no server-sent stream). One request
  per connection, bodies up to 4 MB, no chunked uploads.
- Every request needs `Authorization: Bearer <token>` (from `--token-file`, else
  `FLEET_MCP_TOKEN`; at least 16 characters), else `401`.
- Listens on `127.0.0.1` by default: put a TLS reverse proxy (Caddy, `tailscale serve`/`funnel`)
  in front for anything that is not on this machine.
- Without `--tools` it leaves out what changes the machine rather than its sessions: `exec`,
  `install`, `init`, `config_set`, `web_build`, `skill_install` and every `*_install_service`.
- Tools with `no_wait` (`handoff`, `stack_spawn`) default it to `true`: remote runtimes give a call
  ~30 s, and waiting for a new session to register can take longer.

## Environment

| variable | meaning |
| --- | --- |
| `FLEET_CONFIG` | config file path (default `${XDG_CONFIG_HOME:-~/.config}/fleet/config.json`) |
| `FLEET_HOST` | default target host (same as `-H`) |
| `FLEET_DRY_RUN=1` | print what would run, run nothing |
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
| `FLEET_JOBS_DIR` | [bot](#bots) state (default `${XDG_STATE_HOME:-~/.local/state}/fleet/jobs`) |
| `FLEET_STACKS_DIR` | where [stacks](#stacks) live (default `${XDG_STATE_HOME:-~/.local/state}/fleet/stacks`) |
| `VISUAL`, `EDITOR` | the editor for `fleet brief --edit`, `fleet stack edit` and `fleet config edit` (default `vi`) |
| `FLEET_SNAPSHOT` | the [session-recovery](#session-recovery) snapshot (default `${XDG_STATE_HOME:-~/.local/state}/fleet/snapshot.json`) |
| `FLEET_BOOT_ID` | override this boot's id (tests; a different value simulates a reboot) |
| `FLEET_GROUPS_STATE` | `fleet group` state file (default `${XDG_STATE_HOME:-~/.local/state}/fleet/groups.json`) |
| `FLEET_FIXTURE=<sessions.json>` | read a canned fleet from a file instead of the live registry (demo/tests; backends are inert) |
| `NO_COLOR=1` | no colors |
| `FLEET_NO_TTY=1` | never prompt on the terminal: confirmations fail as if there were none (set by `fleet mcp`) |
| `FLEET_MCP_TIMEOUT` | seconds one `fleet mcp` tool call may run (default 600) |
| `FLEET_MCP_TOKEN` | bearer token for `fleet mcp --http` (when no `--token-file`) |

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
