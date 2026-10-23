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
remote `sh -c`); `FLEET_CONFIG` is not — the remote uses its own config. Under
[`FLEET_PATH_ONLY`](#path-only-mode) another host is refused instead.

## Claude sessions

`<target>` is a session's generated title, its Claude session name, a session-id prefix, or a pid.
Any unambiguous fragment works: matching tries exact → prefix → substring → characters-in-order and
the first rung with hits wins. Two hits on the same rung is an error that lists the candidates.

Two **exact forms** never fall through to a looser rung: `pid:<n>` is the session with that pid,
`id:<session_id>` the session with that full session id (case-insensitive; a prefix is not
enough). Nothing matching exactly is an error (`kind: "target_not_found"`), even when the value
would match a title as a fragment — a bare pid that has just exited can match another session's
title by its digits, and `send` would type into that one. Bare targets keep the ladder above.
Any target that starts with `pid:` or `id:` (any case, spaces allowed) is an exact form — so a
session titled `ID: refactor auth` can no longer be named by that title; use a fragment of it,
or its id.

- **Prefer `id:`** for a row a UI or script keeps around (from `list --json`): pids get reused, a
  session id does not. `pid:` is for a session you just looked up.
- **Identity:** an exact form checks that the process is still the one Claude registered — when
  the registry entry records `procStart` (Claude writes it as `ps -o lstart` in UTC), the live
  pid's start time must be that instant; a value that is not a `ps` time must match as text, so
  an entry that cannot be checked fails closed. A pid a newer process reused is "not found",
  not that process.
- **Limit:** an entry without `procStart` (older Claude versions) is taken on liveness alone, and
  bare targets and `list` never check start times — a stale entry whose pid was reused by another
  process on a terminal can still show up there until Claude cleans it.
- **Cheap:** an exact form looks up only that session. A Claude one: its registry entry
  (`<pid>.json` for `pid:`; for `id:` every entry is parsed to find the ones naming it — files
  only), one `ps` over the pids found, the tmux pane list when it runs in tmux, and the first
  lines of its own transcript for its title — no other session's process or transcript. A Codex / pi one (no registry entry):
  the harness processes are listed (one `ps`, one `lsof`) and paired with their transcripts, then
  only the named one is inspected and read. Either way no context usage is read.

Every verb that takes a session `<target>` accepts them (`peek`, `send`, `keys`, `rename`, `name`,
`brief`, `spawn --from`, `stack …`; `close` and `model` take nothing else). A **gone** session's brief is found by `id:<id>` too, but only
when no live session matches — an ambiguous or malformed target stays an error.

With `--json`, a target that does not resolve prints `{ ok: false, host, kind, error }` on stdout:
`rename`, `keys`, `brief` (every mode) and every `stack` verb that takes a `<stack>` or `<target>`. `kind`
is `target_not_found`, `target_ambiguous`, `target_invalid` (`pid:abc`, `id:`), or for a
`<stack>`, `stack_not_found` / `stack_ambiguous`. The exit code is the command's own: 1 for
`rename`, `keys` and `brief`; `stack` keeps its codes — 2 for anything ambiguous, 3 for a `<stack>` that
matches nothing (a `pid:` / `id:` given as `<stack>` included), 1 for a session `<target>` that
matches nothing, and 3 (`target_not_found`) for `stack remove` given a live session that is not a
member (see [Stacks](#stacks)).

| command | does |
| --- | --- |
| `fleet` | `watch` on a terminal; a one-shot `list` when stdout is piped |
| `fleet list [--json] [-a, --all-hosts]` (alias `ls`) | live sessions: title, status (`working`, `idle`, `needs you`; Claude Code 2.1.308+'s `shell`, idle with a background shell running, is `idle`), age, context usage (`ctx 62%`), terminal, cwd. `--json` is an array whose rows all carry `host`; `-a` queries every configured host in parallel. The text view ends with `N dormant (fleet restore)` when a reboot left [dormant sessions](#session-recovery). Every local `list` also records the [snapshot](#session-recovery) |
| `fleet peek <target> [--lines N]` | what the session's terminal shows now (default 40 lines): a `▼ <title> (<backend>, <status>) — <dir>` header, then each line cut to this terminal's width. A pane tmux cannot capture (just closed, or its server gone) is an error (exit 1, tmux's message), never an empty box |
| `fleet peek <target> --json [--lines N] [--ansi]` | the screen whole, for programs: `{ ok: true, host, target: { session_id, pid }, backend, status, cols, rows, lines: [...], truncated }` — no header, no clipping, the last N lines that have content (trailing blank lines and trailing spaces dropped). tmux: lines it wrapped are joined (`capture-pane -J`), and N counts those logical lines — the capture reaches back into the scrollback until each of the N is whole (up to what tmux still holds), but never more than 2,000 rows: a line longer than that comes back with its start cut and `truncated: true` (otherwise `false`; always `false` for iTerm); `cols`/`rows` are the pane's. iTerm: its visible screen as shown, `cols`/`rows` the session's. Escape sequences are stripped unless `--ansi` (tmux `-e`; iTerm gives none). On failure `{ ok: false, host, kind, error }`, exit 1 — kind `target_not_found`, `target_ambiguous`, `target_invalid`, `backend_unsupported` or `backend_failed`; a command line clap rejects (an unknown flag, wherever `--json` stands) is the same object with kind `usage`, exit 2 |
| `fleet messages <target> [--json] [--after <cursor> \| --before <cursor>] [--limit N]` | the session's conversation from its transcript: prompts, replies, tool calls (never their output) and questions, oldest first — see [Messages](#messages). Without a cursor the newest `--limit` (default 100, max 500); `--after` everything newer (cheap enough to poll every second); `--before` the `--limit` entries before ("show earlier") |
| `fleet send <target> <text> [--json] [--force] [--require-empty]` | type text into the session and press Enter. Under the session's typing lock, the input box is read off the screen first: text in it (Claude Code puts a prompt stopped with Escape back there) or a prompt waiting for its answer **holds** the send — nothing typed, exit 3, kind `send_held` with a `reason`; no box fleet can find or read types as before, with a warning (`--require-empty` holds then too) — see [Sending](#sending). `--force` types without reading the box. After `keys Escape`, wait about a second before sending. While another fleet command types into the session, it fails at once and sends nothing (kind `busy`). `--json`: `{ ok: true, host, target: { session_id, pid }, warning? }`, or `{ ok: false, host, kind, error }` (exit 1; held: exit 3, plus `reason` and `input` — the box's text, sensitive) — kind `send_held`, `busy`, `target_not_found`, `target_ambiguous`, `target_invalid`, `target_changed` (the session's id changed between its lookup and its lock, as `/clear` changes it; nothing sent), `backend_unsupported` (no tmux pane or iTerm session to drive), `backend_failed` (tmux or iTerm refused: the screen could not be read, nothing sent; or typing failed, and part of the text may have gone out) or `io` (the lock could not be taken; with `typed`, `false`: nothing reached the pane, see [Sending](#sending)); a command line clap rejects (an unknown flag, a missing text, wherever `--json` stands) is the same object with kind `usage`, exit 2. Text starting with `-` goes after `--` |
| `fleet done [--note <text>] [--cancel]` | run by an agent **inside its own session** (by `$TMUX_PANE`; never sent over ssh): its work is finished and nothing waits on the user. Fleet shows the session as done (nothing closes it) — see [Ask done](#ask-done). `--cancel` takes the mark back |
| `fleet sleep <target> [--json]` (alias `deactivate`) | close a session but keep it [dormant](#session-recovery) (Deactivate in the app, which lists dormant sessions as **Inactive**): SIGTERM (then SIGKILL) its agent, kill its tmux window (or its tmux session when that was the only window; an iTerm tab is left — the app closes it), and put it in the dormant list, marked `slept`, instead of recently closed. `fleet restore <target>` (or `enter`, or Resume in the app) brings it back with `--resume`; `--all` and `restore.onBoot` leave it be. `--json`: `{ host, sessionId, title, process, terminal, dormant: DormantView, dryRun }` |
| `fleet ask-done <target>` | ask a tmux session to report itself done once finished: types the [instruction](#ask-done) into it. A session waiting on a prompt is refused |
| `fleet keys <target> <key>... [--json]` | press named keys in the session's terminal, in order (80 ms apart), holding the session's lock as `send` and `answer` do: `Escape` (interrupt a turn), `Enter`, `Up`, `Down`, `Left`, `Right`, `Tab`, `BTab` (Shift-Tab), `Space`, `C-c`. Names are tmux's, case-sensitive; any other name is refused (exit 1, kind `invalid_key`) and nothing is pressed. tmux sessions get one `send-keys` per key, iTerm sessions the bytes a terminal sends for the key. **Not atomic:** the first key that fails stops the call — the keys before it stay pressed (nothing can take them back), the rest are not sent, and the error says how many got through (`… — 1 of 3 keys pressed before it`). After `Escape`, wait about a second before a `send`: Claude Code puts the stopped prompt back in its input box only once the turn has stopped, and a `send` that reads the box before then types after it (see [Sending](#sending)). `--json`: `{ ok: true, host, target: { session_id, pid }, keys: [...] }`, or `{ ok: false, host, kind, error }` (exit 1) — kind `invalid_key`, `target_not_found`, `target_ambiguous`, `target_invalid`, `target_changed` (the session's id changed between its lookup and its lock; nothing is pressed), `busy` (another fleet command is typing into the session; nothing is pressed), `backend_unsupported` (no tmux pane or iTerm session to drive), `backend_failed` or `io` (with `typed`, see [Sending](#sending)); a command line clap rejects (an unknown flag, wherever `--json` stands) is the same object with kind `usage`, exit 2 |
| `fleet close <id:… \| pid:…> [--json] [--timeout <s>] [--expect-pid <n> --expect-start <t>]` | end a session that is alone in its own tmux session (the layout `spawn` creates): its exit command (not when its input box holds text or a prompt), then that tmux session is killed; exact targets only, `-n` touches nothing — see [Closing sessions](#closing-sessions) |
| `fleet model <id:… \| pid:…> <model-id> [--json] [--expect-pid <n> --expect-start <t>] [--require-empty]` | switch a Claude Code session's model as `/model <id>` in its terminal does (the pick also becomes Claude Code's default for new sessions), answering only Claude Code's confirmation of that switch; typed only into an idle session's empty input box, under its typing lock; done once its transcript says `Set model to …`; exact targets only, `-n` types nothing — see [Switching the model](#switching-the-model) |
| `fleet effort <id:… \| pid:…> <level> [--json] [--expect-pid <n> --expect-start <t>] [--require-empty]` | switch a Claude Code session's effort level as `/effort <level>` in its terminal does (`low` to `xhigh` also become Claude Code's default for new sessions on its model; `max` is for the session only), answering only Claude Code's confirmation of that switch; `model`'s checks, lock and wait; done once its transcript says `Set effort level to <level>` — see [Switching the effort](#switching-the-effort) |
| `fleet answer <target> [--json] [--allow-attached] (--list \| --choice <n> [--prompt <id>] \| --question <tool_use_id> --answers <json>)` | answer the prompt the session waits on by typing what a human would — only while the terminal shows that very prompt, untouched; see [Answering prompts](#answering-prompts) |
| `fleet rename <target> <title> [--no-tmux-sync] [--force] [--require-empty] [--json]` | rename the session's one title: Claude's `/rename` (the source of truth), then its tmux session follows as a slug of it (`Fix Login` → `fix-login`, `-2` on a collision) when the tmux session is that Claude session's own (one window, one pane). A session **waiting on a prompt** is held (exit 1; `--json`: exit 3) — typed keys would be its answer; busy sessions are fine (Claude runs `/rename` mid-turn without disturbing the turn). Codex gets `/rename`, pi `/name`; a busy Codex / pi session is held too (`held: "busy"`) — they would queue the text as a message. `--force` overrides the hold. It types under the session's typing lock (see [Answering prompts](#answering-prompts)): another fleet command typing into the session makes it fail at once (kind `busy`, nothing sent); and, unless forced, the input box is read under the lock, as `send` reads it ([Sending](#sending)) — a prompt that came up since the lookup holds it as waiting, and text in the box (a prompt Escape put back, a draft: `/rename` would go out with it as one prompt) holds it as `held: "input"`; a box fleet cannot find or read (a panel such as `/usage`, a dialog, the transcript view) holds nothing — unless `--require-empty`, which holds it as `send --require-empty` does, with `send`'s reasons: `held: "no_input_box"` or `"unreadable"` (see [Sending](#sending); `--force` skips the read, and so this). `--json` prints `{ ok, result: renamed\|sent\|held, session_id, pid, host, from, title, held, tmux: { renamed, from, to, note }, message }`; a failure before that report (`busy`, a target error, `target_changed` when the session's id changed between its lookup and its lock, `io` with `typed: false`) is `{ ok: false, host, kind, error }` (exit 1) |
| `fleet name [<target> \| --all [--settled]] [--apply] [--refresh] [--no-tmux-sync]` | suggest a name from what the session is doing; only `--apply` sends it (through the same path as `rename`, tmux included). `--all` = every session with a derived (cwd+hash) name. A derived-name session alone in a tmux session somebody named (`fleet new fix-login`) adopts that name (`(tmux)`, no model call) instead of getting a generated one. `--settled` (with `--all`) skips sessions with one prompt and no brief yet — the model reads the brief's Summary and the first three prompts, and a lone first prompt is often just a link. `--refresh` ignores the cache |
| `fleet group [-a, --all-hosts] [--json] [--apply] [--refresh] [--consolidate] [--input <file\|->] [--cached]` | sort sessions into work-stream groups (the web Board view) — see [Grouping](#grouping). Reads sessions and asks `claude -p`; never sends anything to a session |
| `fleet group --rename <group id> --label <l>` / `--move <host/id> (--to <group id> \| --label <l>)` / `--create --label <l>` / `--delete <group id>` | edit the stored groups by hand (the Board's rename / drag'n'drop) — see [Grouping](#grouping) |
| `fleet spawn [prompt] --dir <path> [--harness claude\|codex\|pi] [--name <n>] [--model <id>] [--effort <level>] [--skip-permissions] [--backend iterm\|tmux] [--tmux-session <s>] [--window] [--json]` | start a new agent session in a new tab/pane. `--json`: one object saying where it landed, or why it did not — see [Spawning](#spawning) (not with `--slack`). `--harness` picks the coding agent (default `claude`; see [Harnesses](#harnesses)). `--model <id>` runs `<harness> --model '<id>'` (letters, digits and `._[]-/:` only — pi's ids are `provider/model`; blank = the harness's default). `--name` is passed as `-n` to Claude and pi; Codex has no such flag (the tmux session carries the name). The prompt goes after `--` for Claude and Codex, so one that starts with `-` (or reads like a subcommand, `resume`) is still the prompt; pi has no `--`, so a prompt it would take as a flag, a file (`@…`) or one of its commands (`update`) gets a leading space. Pass `--` to `fleet` too for such a prompt: `fleet spawn --dir . -- '--help me'`. A prompt whose launch line would pass 900 bytes is written to `~/.claude/fleet-prompts/` and read back at launch with its trailing newlines intact (`fleet_prompt="$(cat <file>; printf .)" && <launcher> -- "${fleet_prompt%.}"`). Default backend: tmux inside tmux, over ssh or off macOS; else iTerm. With tmux, each spawn gets its own tmux session (one session per job) named from `--name` or the dir's basename, sanitised like `fleet new`; a taken `--name` is an error, a taken basename is uniquified (`app-2`). `--tmux-session <s>` opens a window in `s` instead (created if missing). `--ask-done`: the [instruction](#ask-done) goes below the prompt and the session runs in tmux. `--slack <slack-url \| @person> [--slack-for 2h] [--slack-task <instructions>] [--slack-proactive]`: start [linked to Slack](#slack-links) (tmux, only on `slack.host`; checked before anything opens). The session starts with no prompt and is linked once it has registered (≤ ~45 s); the prompt goes in last in the link's intro, as the owner's request to start on — so it never works on it, or reaches Slack another way, before it knows it is linked. Max 4000 chars. If the link fails, the session stays, idle, and the prompt was not sent |
| `fleet harness list [--json] [--no-models]` | the [harnesses](#harnesses) on this host: id, label, launch command, `installed`, and the models each offers for a picker, each with its effort levels (`efforts`, `default_effort`). `-H <host>` asks another machine |
| `fleet spawn --from <target> [prompt] [--name <n>] [--model <id>] [--backend …] [--json]` | continue a session's work in a new one: same host (with `-H`, the host the session is on) and its cwd (`--dir` overrides), first prompt = its [brief](#briefs)'s continue prompt (summary, origin, PRs, worktree, then a `---` line), then a blank line and `prompt` — the task (default: pick up where it left off). The prompt goes through a file under `~/.claude/fleet-handoffs/`, like a handoff. Errors when the session has no brief yet. `-n` prints the launch command and the prompt and writes nothing |
| `fleet spawn --resume <session_id> --dir <path> [--harness claude\|codex] [--name <n>] [--model <id>] [--backend …] [--json]` | resume a past session with the harness's own resume, in a new terminal in `--dir` (required: the directory it ran in): `<claude launcher> --resume=<id>` (the launcher as `spawn` builds it, `-n` for `--name`), `codex resume -- <id>` — the id bound so the harness never reads it as a flag. Same path as a plain `spawn` — checks, login shell, env scrub, naming, `--json` object (plus `resumed`). No prompt, and no `--ask-done` or `--slack` (`usage`). The id is ASCII letters, digits and `-`, 1–128 of them, not starting with `-`. One session never gets a second process: refused while it is live in `fleet list` (same harness, `session_live`), and while another resume of it is starting it (`session_starting`): each resume claims `resume-<harness>-<lowercased id>.lock` under `$FLEET_LOCK_DIR` (an `flock` held through the liveness check and the start) and, before typing anything, writes `resume-<harness>-<lowercased id>.stamp` beside it: the start time, then the tmux pane it types the resume into (and that pane's shell pid). Until the session registers, the stamp holds it for 30 s, and past that for as long as the pane exists and its shell still runs what was typed (Claude waiting at its trust prompt has not registered). Once the pane is gone or its shell is back at the prompt, the stamp is stale; an iTerm resume has no pane to check, so only the 30 s hold. A start that never got a terminal removes the stamp; a stamp that cannot be written stops the resume before anything starts. `-n` claims and writes nothing. See [Spawning](#spawning) |
| `fleet handoff [brief] [--file <f\|->] --dir <path> [--harness <h>] [--name <n>] [--model <id>] [--tmux-session <s>] [--tab] [--no-wait]` | start a new session in another window seeded with a brief (saved under `~/.claude/fleet-handoffs/`); waits until it registers. Same tmux placement as `spawn` |
| `fleet brief <target> [--json \| --prompt]` | print the session's [brief](#briefs) — its body (Summary / Resources / Todos, no frontmatter); `--json` the parsed shape; `--prompt` only the continue prompt |
| `fleet brief <target> --edit` / `--set [--expect-updated <iso>]` / `--regenerate` / `--open` | edit it in `$VISUAL`/`$EDITOR`; save markdown from stdin; ask the web server to regenerate it; open the session's checkout in VS Code — see [Briefs](#briefs) |
| `fleet stack list\|show\|new\|ensure\|spawn\|add\|remove\|rename\|edit\|set\|sync\|rm …` | [session stacks](#stacks): sessions that share one context file (the StackBrief) |
| `fleet stack spawn <target> [prompt] [--harness <h>] [--name <n>] [--model <id>] [--dir <path>] [--on <host>] [--no-wait]` | spawn a **sibling** of `<target>`: a new session in its cwd (`--on`: on another host), in its stack (created around it first when it has none), told where the StackBrief is — see [Stacks](#stacks) |
| `fleet watch [--interval 5] [--stuck 300] [--quiet] [--rows 1\|2\|auto] [--no-mouse]` | live dashboard + notifications on finished/stuck sessions |
| `fleet usage [--json] [--refresh]` | usage limits on this host, as bars with reset times. **Claude**: the subscription of this host's login — current session (5h), weekly all models, weekly per model (e.g. Fable), extra usage; the numbers Claude Code's `/usage` shows. Cached 60s (`--refresh` skips it); a failed read serves the last one marked stale and backs off 2 minutes. **Codex**: the 5h and weekly windows (plus any scoped ones), plan and credits from the newest rate-limit snapshots in Codex's transcripts (no network call, no cache), as of the last Codex turn on this host; skipped when Codex isn't installed. `--json`: Claude's fields at the top level as before, plus `providers[]` — see [Usage JSON](#usage-json). `-H <host>` reads another machine's |
| `fleet skill install [--force]\|show` | install / print the Claude Code skill (`~/.claude/skills/fleet/SKILL.md`; `--force` overwrites a different one) |

`send`, `model`, `rename --force`, `name --apply` and `spawn` change a live agent's state — scripts and agents
should confirm before running them.

### List JSON: subagents

Every `fleet list --json` row carries `subagents`: `{ "running": n, "finished": m }`, or
`null` when unknown. Both counts are nonnegative whole numbers; `finished` alone may be
`null` when the Turn start is unknown: no prompt in the last 8 MiB of the Transcript, or
its latest prompt has no usable timestamp.
`running` includes tasks started in earlier Turns; `finished` includes only tasks that
started and ended since the last main-thread prompt. Notifications and local commands such
as `/effort` do not start a Turn. These counts do not change the Session's Status.

Supported for Claude Code 2.1.301+ with a registry start time and a located Transcript,
from its Subagent sidecars. No sidecar directory means zero; malformed sidecars are skipped,
and sidecars older than the Session's start are ignored. A sidecar marked finished still
counts as running while its latest task notification says the task waits on background work
of its own or resumes on new input, until a later notification or a successful `TaskStop`
settles it. A sidecar not marked finished counts as finished once its latest notification
ends the task (`killed`, `stopped`, `failed`, or `completed` without such a note) or a
successful `TaskStop` stops it: Claude Code does not mark a stopped task finished. Evidence
older than the sidecar's last write is ignored, so a revived task counts as running. Nested,
stopped and forked tasks
count; background shell commands do not. Codex and pi always return `null` for now:
Codex counting waits on a known pairing bug: Fleet's Codex pairing can pick a child agent's
rollout, rather than the parent's, as an interactive Session's Transcript.
See [architecture](architecture.md#subagent-counts) for the read strategy and known limits.

### Spawning

Before anything opens, `spawn` checks that the directory exists, the name, model and effort are valid,
the tmux session name is free, and the launcher can start: its first word must be an executable
path, or a command found on `PATH`, in the usual install dirs or — failing those — by
`command -v` in a login shell of `$SHELL` (a launcher it cannot judge, such as `VAR=1 claude`,
goes ahead as before). It is judged from `--dir`, where the shell will run it: `./agent` is the
`agent` in that directory, and the login shell starts there. Under
[`FLEET_PATH_ONLY`](#path-only-mode) `PATH` alone decides: no install dirs, no login shell.

Fresh Claude Code and Codex sessions accept `--effort <level>` and `--skip-permissions`.
These affect this run only; the harness's saved defaults stay unchanged. After the existing
`--model` and `-n` flags, before `--` and the prompt, Fleet appends:

| Harness | `--effort <level>` | `--skip-permissions` |
| --- | --- | --- |
| Claude Code | `--effort <level>` | `--dangerously-skip-permissions` |
| Codex | `-c model_reasoning_effort=<level>` | `--dangerously-bypass-approvals-and-sandbox` (also disables its sandbox) |

The effort follows `fleet effort`'s validator: 1–16 lowercase ASCII letters, passed as given,
without checking which levels the model supports (`fleet harness list --json` lists those).
An invalid level, or either flag with pi, `--resume` or `--from`, fails before launch or
remote dispatch, including dry runs, with kind `usage` and exit 1 (also in `--json`).
These flags belong only to `spawn`; `handoff`,
`stack spawn`, `pr spawn` and jobs do not accept them. Remote spawns (`-H`) forward both flags.

**Size.** Every tmux session fleet creates — a tmux spawn (and `handoff`, `stack spawn`, `pr
spawn`, a job run), `fleet tmux new`, a [restore](#session-recovery) — starts at 200 columns by 50 rows (`new-session -x 200 -y 50`), not tmux's 80x24 for a detached
session: Claude Code's fullscreen TUI keeps no scrollback, so a prompt taller than the pane is cut
off where [`fleet answer`](#answering-prompts) cannot read it (`clipped`). tmux keeps the size as
the session's `default-size`, so its later windows start at it too; a client attaching resizes it
to its terminal as usual. A window opened in an existing session (`--tmux-session`, a closed
session's window) gets that session's size, and sessions that already exist are never resized.

A tmux spawn (and `handoff`, `stack spawn`, `pr spawn`, a job run) starts its pane without the
variables an agent sets to describe itself to its tools — the agent that ran `fleet`, whose
markers would make the new session act as part of it (a nested `claude` takes its session id,
messaging socket and effort; `codex` thinks it runs sandboxed in another one's thread;
`TRACEPARENT` joins its trace):

- Claude Code: `CLAUDECODE`, `CLAUDE_CODE_ENTRYPOINT`, `CLAUDE_CODE_EXECPATH`,
  `CLAUDE_CODE_SESSION_ID`, `CLAUDE_CODE_CHILD_SESSION`, `CLAUDE_CODE_SESSION_ATTENDED`,
  `CLAUDE_CODE_MESSAGING_SOCKET`, `CLAUDE_CODE_MESSAGING_TOKEN`, `CLAUDE_CODE_SSE_PORT`,
  `CLAUDE_PID`, `CLAUDE_EFFORT`, `AI_AGENT`
- trace context: `TRACEPARENT`, `TRACESTATE`
- Codex: `CODEX_THREAD_ID`, `CODEX_CI`, `CODEX_SANDBOX`, `CODEX_SANDBOX_NETWORK_DISABLED`,
  `CODEX_MANAGED_BY_NPM` / `_BUN` / `_PNPM` / `_VITE_PLUS`, `CODEX_MANAGED_PACKAGE_ROOT`

Only those names: configuration stays, wherever it is set — `CLAUDE_CODE_OAUTH_TOKEN`,
`CLAUDE_CODE_USE_BEDROCK` / `_VERTEX`, `OTEL_*` exporter endpoints, headers and resource
attributes, … (Claude Code does not hand its own `OTEL_*` to tools at all). The markers are dropped
wherever they come from: fleet's own environment, or the global environment of a tmux server
started from inside an agent: the pane's first process, `sh`, unsets them itself (no `env`).
The pane then becomes what tmux would have started — the session's
effective `default-command` when one is set, else `$SHELL -l` (tmux sets `SHELL` in every pane
to the server's `default-shell`) — so the profile runs and may set
any of them on purpose. An iTerm tab gets iTerm's environment, not fleet's.

`spawn --json` (also with `--from` or `--resume`) prints one object on stdout. On success (exit 0):

```json
{ "ok": true, "dry_run": false, "host": "laptop", "backend": "tmux",
  "tmux_session": "auth-fix", "tmux_window": "auth-fix:0", "handle": "%12",
  "created_session": true, "dir": "/work/project", "harness": "codex",
  "model": "gpt-6", "name": "auth-fix", "command": "cd '/work/project' && codex --model 'gpt-6' -- 'fix the login'",
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
| `resumed` | `--resume` only: the session id it resumes (absent otherwise) |
| `dry_run` | `-n`: nothing ran; `command` is what would be typed and the terminal fields are `null` |

On failure the exit code is 1 (unless noted below), stderr carries the message as usual, and
stdout carries `{ "ok": false, "host", "kind", "error", "partial" }`. `partial` is `null` unless
the terminal was created before the spawn failed (typing the launch line into it failed): then it
says where that terminal is — `{ backend, tmux_session, tmux_window, handle, created_session,
command, prompt_file }` as above (iTerm: only `backend` and `handle`) — so the caller can find it
and close it. `kind` is one of:

| kind | when |
| --- | --- |
| `dir_missing` | `--dir` does not exist (with `--from`: no working directory known; with `--resume`: no `--dir` given) |
| `usage` | an invalid `--effort` level, or `--effort` / `--skip-permissions` with pi, `--resume` or `--from`; `--resume` with a prompt or `--from`, or a session id outside `[A-Za-z0-9-]{1,128}` or starting with `-`; also a command line clap cannot parse (`--resume` without a value, an unknown flag); and, under [`FLEET_PATH_ONLY`](#path-only-mode), `-H` another host, the iTerm backend, no `sh` on `PATH` for a tmux pane, or `PATH` unset or empty |
| `harness_unsupported` | `--resume` with a harness that has no resume here (pi) |
| `session_live` | `--resume`: a live session in `fleet list` already has that id (same harness). The object also carries `"pid"` of the live one — select it instead |
| `session_starting` | `--resume`: another `spawn --resume` of that id (same harness, any case) is starting it right now, or started it and it has not registered yet: under 30 s ago, or its tmux pane still runs it (however long ago). Nothing was started; select it once it appears, do not retry the resume |
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

### Messages

`fleet messages <target>` reads the session's transcript — Claude Code's
`~/.claude/projects/<cwd>/<id>.jsonl` (found in any project dir once the session moved to another
directory or worktree), Codex's rollout, pi's session file (found by session id when discovery could
not pair the process with one) — and prints one line per entry. A session that has not written a
transcript yet (no first prompt) is an empty conversation, not an error.

Shown: user prompts, slash commands and `!` commands (`command`), task notifications (`system`),
assistant text, tool calls (one short argument: a path, a command's first line, a pattern) and
Claude Code `AskUserQuestion` prompts with their outcome. Never shown: thinking, tool results/output,
hook output, bookkeeping entries, subagent (sidechain) messages.

`--json` prints exactly one object (one line):

```json
{
  "ok": true, "session_id": "…", "harness": "claude", "transcript": "/abs/path.jsonl",
  "entries": [ { "cursor": "m2.…", "role": "assistant", "kind": "assistant", "ts": "2026-10-16T09:00:00.000Z", "text": "Done.", "final": true } ],
  "updated": [],
  "has_more_before": true, "has_more_after": false,
  "cursor": "m2.…"
}
```

- Entry fields: `cursor`, `role` (`user` | `assistant`), `kind` (`user` | `assistant` | `command` |
  `system` | `question` | `tool_call`), `ts` (RFC 3339 or `null`), `text` (Markdown for `assistant`).
  `assistant`: `final` (the turn ended on it). `user`: `queued: true` while still waiting in Claude's
  queue. `command`: `name`, `args`. `tool_call`: `id`, `tool`, `summary` (≤ 120 chars). `question`:
  `id`, `questions` (`[{ question, header, multiSelect, options: [{ label, description }] }]`),
  `status` (`pending` | `answered` | `declined` | `abandoned` — a new prompt came before any
  result: the session was interrupted or restarted on it), `answer` — `null` while pending or
  abandoned,
  `{ "declined": true }`, or `{ "answers": [string|null…], "picked": [[option index…]…] }` (one
  element per question; `picked` matches the recorded answer against the labels, a typed answer
  picks nothing).
- `cursor` (top level): poll with it. It is the newest entry's position plus a read hint, so an
  `--after` call reads only what was appended since. It re-reads from an earlier line only while
  something is still open there — an unanswered question, a reply that may still grow — and never
  more than 2 MiB of what it already delivered: a cursor that would need more is `cursor_invalid`
  (reload without a cursor) rather than resumed with that state lost. Paging on through what a
  `--limit`ed page has not delivered yet is not bounded (it is progress). Claude's prompt queue travels in the hint. With nothing
  newer it is the `--after` cursor again. `null` only when there are no entries and no `--after`
  was given. Cursors are printable ASCII, at most 512 characters, without a leading `-`. An entry's
  own `cursor` works with `--after` too, but restarts at its line without the queue state before
  it — poll with the page cursor.
- Every cursor names its file (device and inode) and its anchor line's fingerprint: a transcript
  replaced or edited under it is `cursor_invalid`.
- `updated` (`--after`): older entries that changed since the last read — a question answered,
  streamed text appended, Codex narration turned non-final by a later tool call — each reported
  once. Replace them by `cursor`.
- Queued prompts (`queued: true`) come last, as the full current queue, in every no-cursor and
  `--after` response (a `--limit`ed one too); drop the ones shown before. Their cursors (`q2.…`)
  are not positions.
- Only complete lines are read: a half-written last line shows up in the next call.
- Failures (exit 1): one object `{ "ok": false, "kind", "error" }` on stdout, the message on stderr.
  Kinds: `target_not_found`, `target_ambiguous`, `target_invalid` (`pid:abc`, `id:`),
  `transcript_not_found` (a Claude session without a session id), `harness_unsupported` (reserved), `cursor_invalid` (garbage, or the transcript was
  rewritten or replaced, or too far back to resume — reload without a cursor), `usage` (`--after`
  with `--before`, a bad `--limit`), `io`. A usage error clap catches (a missing target, an unknown
  flag, also behind grouped global flags like `-nH host`) is the same object with
  `kind: "usage"` and `host`, exit 2 — clap's code; the usage errors fleet finds itself exit 1.
- `--before` settles what is open where its page ends: a reply's later blocks (after a tool
  call) are glued in, a question's outcome (or abandonment) is filled in, from the lines after.

### Sending

`fleet send <target> <text> [--force] [--require-empty]` types into the session's input box and
presses Enter, so it first reads that box. Escape stopping a turn before Claude Code has written
anything puts the stopped prompt back in its input box; text typed after it goes out with it as
one prompt — the very prompt the person stopped.

**One read, one lock.** `send` takes the session's typing lock (the one `answer`, `keys`, `close`
and every rename take), reads the visible screen once — tmux: `capture-pane -p -e`, the text's
attributes kept — and holds the lock until after its Enter. No fleet command can type between the
read and the text; see the limits below for what can. The lock is named after the session's id,
and `/clear` gives the same process a new one: once `send` holds the lock it looks the process up
again, and a new id there (another fleet command would now take that id's lock) is
`target_changed`, nothing sent. `keys`, `answer`, `close` and the renames check the same.

**What counts as empty.** Whatever the harness draws faint (SGR 2) in the box is not typed:
Claude Code's placeholder (`Try "…"`, "Press up to edit queued messages") and its prompt
suggestions, Codex's placeholder ("Ask Codex to do anything"). Claude Code draws its own cursor in
reverse video on the placeholder's first character; that cell counts as the placeholder when
faint text follows it. With an IDE connected, Claude Code starts the box's first line with a pill
naming the IDE's selection (`[⧉ 12 lines selected] `, `[⧉ In main.rs] `) in a colour of its own:
a leading `[…]` all in one colour — not the terminal's default, not faint, not the colour of the
text after it — is that pill and is not typed, unless it is a paste or image marker. Anything else
in the box is text: a restored prompt, typed text, a `[Pasted text …]` or `[...Truncated text …]`
marker, an `[Image #…]` tag. Colours are read as colours, never as attributes: the numbers of a
24-bit `38;2;r;g;b` (or 256-colour `38;5;n`) never turn faint or reverse video on. The box is:

- Claude Code: between the last two rules drawn from column 0, its first line `❯ …`, every line
  under it indented. Bash mode (`! …`) is never empty: text typed there would run as a shell
  command. Claude Code draws nothing for blanks typed into the box, so a box holding only
  spaces or line breaks looks empty, and what `send` typed would go out behind them: ` /model
  opus` is a prompt, not the command. On tmux the caret tells: in an empty box it sits where
  the text starts (after the marker, and the IDE pill); in the box past that, the blanks before
  it are `text`; outside the box it says nothing. The caret is the terminal's cursor while it
  shows; while it is hidden (`#{cursor_flag}` 0) — Claude Code hides it unless its native
  cursor is on, and paints its own caret instead — it is the one reverse-video (SGR 7) cell in
  the box: on the placeholder's first character, or a blank where the next character goes.
  Without either — the cursor hidden with no such cell in the box (or more than one), the
  query failing, or iTerm, which gives neither — a box that shows nothing cannot be told from
  one holding blanks: `unreadable`, never empty. A known gap: blanks typed and the caret then
  moved back before them (Home) look the same as an empty box, so such a box reads empty, the
  cursor shown or hidden; at worst what `send` types goes out behind them, plain text with
  leading spaces.
- Codex: the last `› …` line at column 0, and the rows under it drawn on that line's background
  (the composer's own, painted across its width), every one indented, the first row past them
  that shows anything indented too (the footer, or a popup) or none. Where that line has no
  background (or iTerm gives no attributes), down to its footer instead: the rows that show
  anything at the very bottom of the screen, indented, one of them saying what a footer fleet
  knows says (`? for shortcuts`, `… context left`, `… % used`, `⏎ send`, `… to edit previous
  message`, `… again to quit`, `… to queue message`), every line above them indented. Those words
  count only there, since they can be typed. One row of them drawn as typed text never is
  (faint, bold or in a colour) is enough: Codex's key hints are bold, while its status row can be
  plain (no colours) and a flash ("Copied selection") is. When those rows are not sure to be the
  footer — no blank row above them, none of them drawn so, or no attributes at all — the box is
  never read as empty: `unreadable`. The image attachments
  Codex draws above that line (`[Image #1]` rows, a blank row between, as when a prompt with an
  image is recalled) are in the box, and are text: they go out with the next prompt.
- pi: between the last two rules drawn from column 0.

A permission prompt, a question form or its review screen on screen (as `answer --list` reads
them) comes first: the box is not there then.

**What the read decides.** Each case has a `reason`:

| `reason` | the screen shows | by default | `--require-empty` | `input` |
| --- | --- | --- | --- | --- |
| — | an empty box | typed | typed | — |
| `text` | text in the box | **held** | **held** | that text, faint text and the IDE pill left out, its lines joined with `\n` |
| `prompt` | a permission prompt or question waiting for its answer, in place of the box | **held** | **held** | — |
| `no_input_box` | no box fleet can find: a menu or picker, the transcript view, a dialog, a layout it does not know | typed, with a warning | **held** | — |
| `unreadable` | the box shows text, but the screen came without text attributes (iTerm: AppleScript gives text only), so a placeholder or suggestion cannot be told from typed text; or Claude Code's box shows nothing and its caret, which alone tells blanks typed there, cannot be found: the terminal's cursor not given, or hidden with no caret painted in the box | typed, with a warning | **held** | the text as shown; none when it shows nothing |

**Held** only on evidence by default: text in the box or a prompt in its place is something
typing would harm — the stopped prompt sent again glued to the new one, a permission prompt's
Enter taken as its answer. A box fleet cannot find or read is not evidence of anything, and
holding there would hold every send whenever a harness changes its layout, every iTerm session
showing a suggestion, and every caller that cannot pass `--force`. So it types as `send` did before reading the box, and says so.
A caller that would rather hold than type blind — a UI that shows the person why and lets them
choose — passes `--require-empty`, which holds on all four.

- **Held:** exit 3, nothing typed, the message on stderr; with `--json`, `{ ok: false, host,
  kind: "send_held", reason, error, input }` on stdout — `input` only when the box shows text.
- **Typed with a warning:** exit 0, one line on stderr (`warning: typed without knowing the
  input box was empty: …`); with `--json`, `{ ok: true, host, target: { session_id, pid },
  warning }`, `warning` the reason (`no_input_box` or `unreadable`). An empty box has no
  `warning`.
- **`--force`** skips the read and types after whatever the box holds, still under the lock —
  `--require-empty` or not. `fleet peek` shows what the screen holds.

**`io`: whether anything was typed.** A failure of kind `io` from `send`, `keys`, `rename`,
`answer`, `model` or `close` with `--json` carries `typed`: `true` when any key reached the
pane before it, or may have (one that failed going out counts), `false` when nothing did — the
typing lock could not be taken, the screen or a process could not be read before typing. Only
`typed: false` is safe to retry as is; a caller goes by it, never by the `error` text. Other
kinds say it their own way (`send_held`, `busy`, `target_changed`: nothing typed;
`outcome_uncertain`: typed).

**`input` is sensitive.** It is whatever the person typed or pasted into the box — a draft, a
pasted secret, `! export TOKEN=…` in bash mode. It is only ever in `--json`'s `input` field:
never in `error`, never on stderr, so it stays out of agent transcripts and logs that keep a
command's messages. A caller that shows or logs `input` decides who may see it.

**Where it applies.** Every `fleet send` into a tmux pane or an iTerm session (the web server
types into its sessions directly, without it), and `-H <host>` runs `fleet --local send` on that host, so the check is that
host's fleet's. An older fleet there types without it, and rejects `--force` and
`--require-empty` as unknown flags: exit 2, kind `usage` (with `--json`), nothing typed. `send`
has no other way into a session — no Codex app-server or SDK channel: it always types keys. A
session with no terminal fleet can drive (`backend_unsupported`), or a `FLEET_FIXTURE` one, is
refused before any read, as before. `rename` and `name --apply` read the box the same way, under
the same lock, and hold on text (`held: "input"`) or a prompt (`held: "waiting"`); with `rename --require-empty` also on a box not found (`held: "no_input_box"`) or not readable (`held: "unreadable"`), as `send --require-empty` does; `close` types
its exit command only when the box holds neither, and kills the session otherwise.

**Limits.**

- Faint is the one signal: a harness that drew typed text faint, or a placeholder not faint,
  would be misread. Synthetic screens in each harness's layout back it (`testdata/screens`), and
  live captures of Claude Code 2.1.303 and Codex 0.163.0 (`testdata/screens/live`): empty,
  typed, multi-line, a prompt Escape put back (Claude Code; Codex does not put one back), a
  draft during a turn and after Escape stopped it. pi's layout is synthetic only.
- **Known gap: Codex's usage-limit menu.** "Approaching rate limits — Switch to … for lower
  credit usage?" replaces the composer with a menu that is not read as a prompt, so it is no
  input box (`no_input_box`). A plain `send` types into it with a warning, and its Enter picks
  the menu's first choice (switch model); `send --require-empty` holds there. Left as is.
- **Known gap: Codex in a narrow pane.** Codex cuts its footer short instead of wrapping it, and
  drops hints: below about 36 columns, "Context N% used" is cut off and "? for shortcuts"
  dropped, so no footer text fleet knows is left. Where its composer has no background (as in
  the live captures), there is then no input box (`no_input_box`): a plain `send` types with a
  warning, `send --require-empty` holds. Keep Codex panes at least 36 columns wide.
- A `[…]` typed alone in the box, in one colour that is not the terminal's default, reads as the
  IDE pill: an empty box.
- An image placeholder drawn faint is not detected: Claude Code 2.1.303 draws its `[Image #N]`
  as typed text (plain, in a colour, or in an OSC 8 link), but one it drew faint would read as
  an empty box, and the image would go out with the next send, `--require-empty` or not.
- **Escape, then wait about a second before `send`.** The read is one moment. Fleet commands wait
  for the lock, but the agent itself can redraw its box after the read: Claude Code puts the
  stopped prompt back only once the turn has stopped, so `keys Escape` followed at once by
  `send` can read the box still empty and glue the text to the prompt that appears right after.
  A person typing into an attached terminal is not held either.

### Answering prompts

`fleet answer <target>` answers the prompt a session is waiting on, from outside its terminal:

- `--list`: what the screen shows. `prompt` is `null`, a **permission** prompt — Claude Code's
  "Do you want to proceed?" (Bash, edits, …), its confirmation of a slash command ("Change effort
  level?" after `/effort`, "Switch model?" after `/model`, both while the prompt cache is warm: a
  dialog topped by a `▔` rule — which Claude Code 2.1.308 may end with the hint it draws above
  the input box, `▔▔▔ ◐ medium · /effort ▔` — its question the line under it), or a Codex approval ("Would you like to run the
  following command?") — `{ kind: "permission", id, title, detail: [lines], choices: [{ n, label }],
  selected, untouched }` (`{ kind: "permission", incomplete: true, reason }` when it cannot be told
  whole: its top out of reach, its frame ambiguous, a Codex approval without exactly one title — on
  a Codex or pi pane, a numbered list with no title counts as one only with the approval's answer
  shape, a shortcut in an answer (`(y)`, `(esc)`) or its footer "Press enter to confirm or esc to
  cancel"; Codex's popups (/model, /approvals, /review), its transcript pager and a numbered draft
  in an input box are `null`), or a Claude Code **question** form
  `{ kind: "question", id: <tool_use id> | null, questions | null, active, untouched }` (the id comes
  from the transcript: the pending `AskUserQuestion` the form shows, checked whole — tabs, the active
  question's full text, its options and descriptions; never a question whose options have
  previews, see `layout_unsupported` below). A question form not all of which is on screen
  (`detail: "clipped"`, whichever check finds it) is
  `{ kind: "question", incomplete: true, id: null, questions: null, active: null, untouched: false }`
  — no id even when what is visible reads as a pending question.
  Beside `prompt`: `answerable` — whether `--choice` / `--question` would pass the screen checks
  now: the prompt shown whole (for a form: its rows up to row 1 and its head up to its top rule
  or tab bar, so none of it is off screen), untouched, its first question active; not the
  attached-client, lock or transcript checks (`question_not_active`, `not_waiting`) — so a
  caller offers an answer only for `prompt.id` equal to its question and `answerable: true`;
  `detail` — why not, the code a refusal would carry (below; `null` when answerable, `no_form`
  when `prompt` is `null`); and `pane` — `{ cols, rows }`, the pane's size (`null` outside tmux).
  The screen is read once per `--list`; the size is one more tmux query. Types nothing.
- `--choice <n> [--prompt <id>]`: pick answer `n` of the permission prompt on screen. With
  `--prompt`, only if the screen still shows the prompt `--list` returned (its `id` fingerprints the
  title, the detail and the answers, so another command's prompt has another — the detail as
  drawn, indentation and blank lines included: two commands that differ only in whitespace are two
  prompts. Left out: a side frame (a box character at the left edge and one at the end of a line;
  an indented line keeps its box characters), trailing blanks, and Claude Code's frame rules,
  which are as wide as the pane — so the frame's width is not part of the id; a line the harness
  wraps differently after a resize still changes it, and the answer is refused).
- `--question <tool_use_id> --answers '<json>'`: answer a pending `AskUserQuestion` (Claude Code;
  the id is the `question` entry's `id` in [`fleet messages`](#messages)). It must be the one call
  waiting: an abandoned question, or another asked since (even in the same words), is
  `question_not_active`. `answers` has one element per question, in order:
  `{ "options": [index…], "text": "…" }` — 0-based option indexes; single-select takes exactly one
  option or a text, multi-select any options and/or a text, at least one; text is one line,
  ≤ 2000 characters, with no control characters or escape sequences. The session must be `waiting`.
  With two or more questions the answer is checked on the review screen, which joins picks and
  text with `", "`; an answer it could not tell from another — an option labelled `A, B` against
  the options `A` and `B`, or text that reads as an option — is refused (`answer_invalid`, nothing
  typed).

**Exclusive input.** A terminal binds no answer to a prompt id, so the sources of a race are closed
instead. With a terminal client attached to the session's tmux session — a human who could be typing
— nothing is typed (`human_attached`) unless `--allow-attached` is given; an iTerm session always
counts as attached. And a per-session lock (an `flock` under `$FLEET_LOCK_DIR`, else
`${XDG_RUNTIME_DIR:-cache dir}/fleet/locks`) is held for the whole read → type → verify sequence.
Every fleet command that types into a running session takes the same lock — `send`, `keys`,
`close` and every rename (`rename`, `name --apply`, `tmux rename`, the dashboard) — so a second one
fails at once (`busy`) instead of interleaving its keys. The web server drives its sessions'
terminals directly and does not take it. Typed without it, into a terminal just opened, where no
registered session runs yet: `spawn` and `restore` (the launch line into a new pane's shell).

Nothing is typed unless the terminal shows that very prompt **untouched** at the bottom of the
visible screen — at most a short footer under its answers and no input box; for a form also every
tab unanswered, no box ticked, nothing typed, the cursor on the first answer. The same words quoted
in the conversation, or left in the scrollback, never count. 1000 lines of scrollback are read too,
only to reach the top of a Claude Code prompt whose command is taller than the window; when even
that does not reach it — or the rules at its left edge are not exactly its frame's (a lone or
extra dashed rule, a dashed pair with no header above it), so the frame cannot be told from a
command's own lines — it is `prompt_incomplete` (answer it in the terminal). A Codex approval has no
frame: it is everything between the last conversation item (the line at the left edge above it)
and its answers, and exactly one line of it may read as its title ("Would you like to …", "Allow
…"); a command line that reads like one too, no line that does (a line merely ending in `?`
does not), or a top out of sight (over 60 lines up, or no line at the left edge captured at all:
a fresh or cleared pane) is `prompt_incomplete`. That can refuse a harmless prompt: a command or
reason that wraps so a line starts with `Allow `, `Do you want to` or `Proceed?`, or that says
"needs your approval" anywhere, is `prompt_incomplete` too (answer it in the terminal). A list
with no title in sight is taken for an approval only when it has the approval's answer shape — a
shortcut in an answer (`(y)`, `(p)`, `(esc)`) or the footer "Press enter to confirm or esc to
cancel" — and is then `prompt_incomplete`; without it (a popup, the transcript pager) it is no
prompt, its top in sight or not. With that shape, answers that cannot be read whole (the list
running off the captured text, a wrapped line that reads as a rule or as another answer) are
`prompt_incomplete` too, never "no prompt". A numbered list in an input box is a draft, never a
prompt: Claude Code's or pi's (a plain rule from the left edge under it), Codex's composer (its
footer: "? for shortcuts", "… context left", "⏎ send"). Codex's `request_user_input` questions are
not approvals: answer them in the terminal. `--choice` refuses them (`prompt_not_on_screen`, or
`prompt_incomplete` when their answers carry the approval's shape) unless the question reads as an
approval's title ("Would you like to …"), and then takes it for one. (The column-0 rules are an assumption about how the harnesses draw. Claude Code: the
frame from column 0, everything inside it padded; only a synthetic Bash layout backs it so far.
Codex: the conversation items from column 0, everything in the approval — title, command, reason —
padded, so no line of a command can pass for the item above it; only recorded approvals of a
one-line command (at 40, 60 and full width) back it so far, no patch approval, multi-line command
or `request_user_input` layout. A line the terminal wraps to column 0 itself — a width it measured
wrong — breaks either.)

No key is typed blind:

- A permission prompt is answered with **one key** where it takes one — Claude Code: the answer's
  number; Codex: the shortcut in its label (`(y)`, `(p)`, `(esc)`) when no other answer shares it —
  right after the screen is read (`method: "key"`).
- Otherwise (`method: "cursor"`, and every question form) each key is checked for: before the next
  one the screen must show exactly what the keys so far lead to — the cursor on the expected answer,
  the expected boxes ticked, the expected text typed, the expected tabs answered (☒); before the
  final Enter, the review screen listing every question with exactly the answer planned (whitespace
  included) and "Submit answers" focused. Anything else stops it: `prompt_changed` (it says how
  many keys went out).
- After the last key the screen is read again: the prompt must be gone, and for 300 ms no prompt
  with the same answers may stand in its place (a queued prompt the key may have answered instead),
  nor a permission prompt that cannot be read whole (one `--list` reports as `incomplete`: it may
  be the prompt just answered or a queued one). Only then is it `answered: true`; otherwise
  `outcome_uncertain`, with what is on screen now. A
  terminal that can no longer be read (its pane or tmux gone) proves nothing: `outcome_uncertain`
  after the last key, `io` (saying how many keys went out) before it.

**What remains: the approval race.** A terminal has no "answer prompt X" operation — a key goes to
whatever is on screen when it arrives — so nothing can make the key conditional on the prompt
read a moment earlier. The lock keeps every other fleet command out (not the web server, which types directly), and
attached clients are checked once, before the first key. Not covered:

- a client that **attaches after** that check, answers the prompt (A) and leaves the next one (B)
  — with the same answers — on screen in the moments before fleet's key: the key then answers B
  (a permission prompt with a one-key answer has the whole read → key window; a cursor answer is
  checked again before each key, so only the last gap);
- anything else writing to the pane (`tmux send-keys` from another tool, an iTerm script) in that
  same window.

**What the post-key check detects, exactly:** a prompt with the same answers, or a permission
prompt that cannot be read whole, still on screen within 300 ms of the key (or the read prompt not
gone within 1.5 s) is `outcome_uncertain`.
That is the case where *another* prompt follows the one the key answered — usually fleet's key
landed on A and B came up behind it, so the report may be a false alarm. **The plain two-prompt
case above is not detected:** the key answers B, B leaves the screen, nothing follows, and the
result is `answered: true` — for the wrong prompt. With identical commands the two prompts even
have the same `id`, so `--prompt` cannot tell them apart either. fleet has no cheap, sound way to
detect it: the screen keeps no record of which prompt a key answered, Claude Code's transcript does
not tie a prompt on screen to its tool call, and Codex records the call that asked (an escalated
command, with its justification) but not the decision. Nothing
can undo it either. Making it exclusive would mean taking the terminal away from its user
(read-only or locked clients, a moved pane) for every answer, which fleet does not do. Answer only
sessions nobody is about to drive by hand, and treat `outcome_uncertain` as "look at it now".

`-n` / `--dry-run` (and MCP `dry_run: true`) does every check and types nothing:
`{ ok: true, answered: false, dry_run: true, prompt_kind, …, steps }`.

**A form taller than the pane.** Claude Code's fullscreen TUI keeps no scrollback: a question
form taller than the pane loses its top (tabs, question, first rows) where no capture reaches, and
Claude Code may draw a long option list a window at a time. Such a form is `prompt_incomplete` with
`detail: "clipped"` — its rows not running up to row 1, rows left out, or its head (the question
above row 1) not reaching the form's top rule or tab bar — never matched and never typed into.
Sessions fleet creates start at 200x50 for this ([Spawning](#spawning)); a smaller one (an older
session, a client that attached at a small size) can still clip a tall form: answer it there, or
make the pane bigger.

**Why a refusal: `detail`.** Every failure object `answer --json` prints carries `detail` and
`pane` beside `kind` and `error` — also a usage error clap catches and a
[`FLEET_PATH_ONLY`](#path-only-mode) refusal, reported before `answer` runs (both `null` there). `error` is free text and may quote the screen (a label, a title); `detail` never does —
it is one code from a fixed set, so a caller can log it — or `null` for a failure that is not a
screen check (`busy`, `human_attached`, `choice_invalid`, `answer_invalid`, `question_*`,
`not_waiting`, `io`, `send_failed`, `outcome_uncertain`, …). `pane` is `{ cols, rows }`, the
session's pane size when the failure was reported — `null` before the target is known (`usage`,
`target_*`) or outside tmux. Neither carries screen or transcript text. The codes:

| `detail` | kind | when |
| --- | --- | --- |
| `no_form` | `prompt_not_on_screen` | the screen does not end in a prompt of the kind asked about: no question form for `--question`, no permission prompt for `--choice` (an input box, a list quoted in the conversation, nothing) |
| `clipped` | `prompt_incomplete` | it is there, but not all of it is on the captured screen: a form's top or first rows off it, or only some of its rows shown (above); a Claude Code permission prompt whose frame is out of reach, a Codex approval whose top is, answers that do not run up to answer 1 |
| `layout_unsupported` | `prompt_incomplete` | it is there in a layout fleet does not read: a question whose options have previews (drawn beside them; fleet never reads or answers that layout), a permission frame that cannot be told from its command, a Codex approval with no title or two, answers it cannot walk (a blank or rule-like line among them) |
| `tabs` | `prompt_not_on_screen` | the form's tabs are not the question's (another number, other headers), or no tabs for a question in several parts |
| `question` | `prompt_not_on_screen` | the form asks another question |
| `options` | `prompt_not_on_screen` | the form's answers are not the question's: a label or description, single vs. multi-select, the rows after the options |
| `touched` | `prompt_not_on_screen` | the form has a tab answered (☒), a box ticked or text typed |
| `cursor_moved` | `prompt_not_on_screen` | the cursor is off the first answer, or the form shows another of the call's questions |
| `different_prompt` | `prompt_not_on_screen` | another prompt: not the `--prompt` id, a question form for `--choice`, a permission prompt for `--question`; in `--list`, a form no pending question of the transcript accounts for |

`prompt_changed` (keys went out, then the screen was not what they lead to) carries the code for
what the screen showed instead, the last time it was read: `cursor_moved` (the cursor elsewhere),
`touched` (other boxes, text, answered tabs, or other answers on the review screen), or one of
the above. A refusal for a clipped form, in full:

```json
{ "ok": false, "kind": "prompt_incomplete", "error": "the question form's answers run off the captured screen (taller than the terminal) — answer it there; nothing typed",
  "detail": "clipped", "pane": { "cols": 80, "rows": 24 } }
```

and `--list` for a form fleet can answer now, then for one it cannot:

```json
{ "ok": true, "session_id": "…", "harness": "claude",
  "prompt": { "kind": "question", "id": "toolu_…", "questions": […], "active": 0, "untouched": true },
  "answerable": true, "detail": null, "pane": { "cols": 200, "rows": 50 } }
{ "ok": true, "session_id": "…", "harness": "claude",
  "prompt": { "kind": "question", "incomplete": true, "id": null, "questions": null, "active": null, "untouched": false },
  "answerable": false, "detail": "clipped", "pane": { "cols": 80, "rows": 24 } }
```

`--json` success: `{ ok: true, session_id, harness, prompt, answerable, detail, pane }` for `--list`;
`{ ok: true, session_id, harness, answered: true, prompt_kind: "permission", prompt, choice: { n, label }, method, steps }`
or `{ ok: true, session_id, harness, answered: true, prompt_kind: "question", id, steps }` after typing
(`steps`: `[{ key } | { text }]`, what was typed). Failure (exit 1): `{ ok: false, kind, error, detail, pane }`;
`outcome_uncertain` adds `screen` (the `--list` prompt now, or `null`), `prompt_kind` and `steps`.
Kinds: `usage` (exit 2 when clap catches it, then with `host`), `target_not_found`, `target_ambiguous`,
`target_changed` (the session's id changed between its lookup and its lock; nothing typed),
`human_attached`, `busy`, `prompt_not_on_screen` (not there, a different prompt, or touched —
nothing typed), `prompt_incomplete` (there, but not read whole — `clipped` or
`layout_unsupported`; nothing typed), `prompt_changed`, `outcome_uncertain`, `choice_invalid`,
`question_not_found`, `question_not_active`, `question_answered` (already answered or declined),
`not_waiting`, `answer_invalid`, `harness_unsupported` (`--question` on Codex / pi),
`transcript_not_found`, `backend_unsupported` (no terminal fleet can drive, or `FLEET_FIXTURE`),
`send_failed` (says how many keys went out), `io` (with `typed`: whether a key went out before
it, see [Sending](#sending)).

### Closing sessions

`fleet [-n] close <target> [--json] [--timeout <seconds>] [--expect-pid <n> --expect-start <t>]`
ends a session and removes the tmux session it ran in:

- **Exact targets only:** `id:<session_id>` or `pid:<n>`. A bare pid, a fragment or a title is
  `usage` — closing never matches loosely.
- **That process only.** The agent is its pid plus its start time (`proc_start` in `list
  --json`, the identity check behind exact targets). `--expect-pid` and `--expect-start` (both
  or neither, else `usage`) name the process the caller saw: when the session is now run by
  another one (resumed under a new process, a reused pid) it is `target_changed` and nothing is
  touched. The same check runs again right before the kill; an agent that has exited by then
  passes it (its pid may be another process's) unless the target now names another process.
- **Only a session alone in its own tmux session**: one window, one pane, and that pane is the
  session's — the "one tmux session per job" layout `fleet spawn` creates. Anything else is
  refused and nothing is touched: `host_shared` (a window of a shared session like `work`, a
  pane beside others), `backend_unsupported` (an iTerm tab).
- **Graceful first**, under the per-session lock `send`, `keys` and `answer` take (`busy` when
  another fleet command holds it): a `busy` or `waiting` session gets `Escape` and up to 3 s to
  settle; then, only if that same process still runs in the foreground of the session's pane,
  the harness's exit command is typed — `/exit` for Claude Code, `/quit` for Codex (pi: none,
  it goes straight to the kill) — and the agent gets `--timeout` seconds (default 5, at most
  600) to exit. Before that command, its input box is read as `send` reads it
  ([Sending](#sending)): when it holds text — the prompt the Escape put back, a draft — or a
  prompt waits in its place, or the pane cannot be captured, nothing more is typed and the
  session goes straight to the kill (`how: "killed"`): the command would go out with that text
  as one prompt, and that prompt would run, or it would answer that prompt. No box fleet can
  find (`no_input_box`) does not stop it, as it does not stop `send`. The process's pid, start
  time, terminal and foreground state are read in one `ps`, so they always describe one
  process. Nothing is typed, `Escape` included, to an agent that is not in the foreground of
  its pane's terminal or has exited (on Escape too):
  - suspended (Ctrl-Z) or in the background with nothing in front of it but its own stopped
    process group or the shell that started it, at its prompt, it goes straight to the kill;
  - with a process of its own in front of it — reading its terminal in its process group, or in
    a group of its own — such as Claude Code's Ctrl-G external editor, it is `busy`;
  - suspended behind another job (one the user ran after Ctrl-Z), it is `host_shared`, since
    the kill would end that job too.

  Whether it is in front or not, another job **stopped** on the pane's terminal — outside the
  agent's process group and not started by it (`vim`, Ctrl-Z, then `fg` back to the agent) —
  is `host_shared` too: the kill would end it as well. A job running in the background
  (`cmd &`) is not looked for (a prompt's own helpers, like `gitstatusd`, run that way): it
  gets the hangup with the pane.

  In these refusals nothing is typed or killed. Whether a child of the agent reads the
  terminal is asked of `/proc` on Linux and of `lsof` elsewhere; when that cannot be told —
  `lsof` missing, killed, exiting non-zero while a pid it said nothing of still runs, or not
  done within 3 s (it is killed then) — close fails closed: `io`, nothing typed or killed.
  The same look is taken again right before the kill: an editor opened by then is `busy`, a
  pane now on another terminal `target_changed`, and the message says what was typed.
- **Then the tmux session is killed** by its id (`$3`, never reused, unlike a name), whether the
  agent exited (its shell would stay) or not — in one tmux command with the check that the pane
  is still the one pane of its one window (`if-shell -F` on `session_id`, `session_windows`,
  `window_panes`, `window_linked` and `pane_id`, then `kill-session`), so a session renamed or
  replaced, or a pane split or a window added at that moment, is never hit: that is
  `host_shared`, with nothing killed. The kill is confirmed against the session list; a tmux
  query that fails (anything but "no server") is `io`, never success.
- `-n` (or `FLEET_DRY_RUN=1`) runs every check (the lock included) and reports the plan; it
  types and kills nothing, and prints nothing but the result.

`--json` prints one object: `{ ok: true, host, session_id, pid, harness, how, tmux_session,
dry_run }`. `how` is `"exit"` whenever the agent was not alive at the end: gone before the kill
(on its exit command, on Escape, or by itself, after the wait gave up on it too) or with its
tmux session already gone at the kill. It is `"killed"` when it was still running when its tmux session was killed
(under `-n`: `"exit"` when the agent is in front of its pane and the harness has an exit
command); `tmux_session` is the name it had at the
kill. Failures are `{ ok: false, host, kind, error }`, exit 1 — kinds `usage` (clap's too,
wherever `--json` stands), `target_not_found`, `target_ambiguous`, `target_invalid`,
`target_changed` (also for a session whose id changed under the same process — a `/clear` —
between the lookup and the lock: nothing typed or killed), `host_shared`,
`backend_unsupported`, `busy`, `io` (with `typed`: `true` once Escape or the exit command went
out, see [Sending](#sending)).

`busy` and `host_shared` failures add `reason`, which says which situation it is (any other
kind has none; a caller that does not know a reason goes by `kind` alone):

| kind | `reason` | when |
|---|---|---|
| `busy` | `locked` | another fleet command (`send`, `keys`, `answer`, another `close`) holds the session's lock — retry |
| `busy` | `editor_open` | a process of the agent's own is in front of it: one in its process group reading its terminal, or one in a group of its own (Claude Code's Ctrl-G editor) — close it first |
| `host_shared` | `shared_host` | its tmux session holds more than its one pane (another window or pane, a linked window), or no longer holds that pane, at the start or at the kill |
| `host_shared` | `job_in_front` | the agent is suspended or in the background, and another job of the pane's shell is in front of it |
| `host_shared` | `other_jobs` | another job is stopped on the pane's terminal (in front of the agent or behind it) |

### Switching the model

`fleet [-n] model <target> <model-id> [--json] [--expect-pid <n> --expect-start <t>]
[--require-empty]` switches a Claude Code session to another model, as `/model <model-id>` typed
in its terminal does: from its next reply on, and as Claude Code's default for new sessions too
(`/model` saves it there). A plain `send "/model …"` is not enough: while the prompt cache is warm
(on a subscription, for an hour after the last reply) Claude Code asks to confirm the switch, and
`send` returns before Claude Code acts, so the caller would need a second command to confirm it
(`answer`, which reads that dialog as a permission prompt) and still could not tell whether the
switch happened. `model` confirms only that very switch, under the one lock it typed under, and
reads the outcome off the transcript.

- **Exact targets only:** `id:<session_id>` or `pid:<n>`; anything else is `usage`.
  `--expect-pid` and `--expect-start` (both or neither) name the process the caller saw, a
  `list --json` row's `pid` and `proc_start`, as for `close`: another process now is
  `target_changed`.
- **The model id** is checked as `spawn --model`'s (letters, digits and `._[]-/:`, at most 100;
  blank is `usage`) and typed as given. Claude Code says whether it knows it.
- **Claude Code only.** Codex (0.163 opens a picker on `/model`) and pi are
  `harness_unsupported`; an iTerm tab is `backend_unsupported` (the box is read with its text
  attributes, which only tmux gives).
- **Checks, under the session's typing lock**, right before typing, in this order — nothing is
  typed unless all four hold:
  1. the agent owns its pane, by `close`'s foreground check: that very process, in the
     foreground of its pane's terminal, nothing of its own reading the terminal in front of it.
     Its editor open is `busy` (`editor_open`); suspended behind another job `host_shared`
     (`job_in_front`); suspended (Ctrl-Z) or in the background with only its shell in front,
     `host_shared` with the new reason `suspended` — the shell's prompt would get the text
     (`close` kills such a session instead); exited, `target_not_found`. A job stopped
     elsewhere on the terminal does not matter here (nothing is killed): `close`'s
     `other_jobs` does not apply;
  2. its Status, read afresh as `list` reads it (Claude Code's session registry, not the
     screen), is `idle` — or `shell`, Claude Code's idle with a background shell running —
     else `not_idle`, its `status` given. That read, by the agent's pid
     whatever the target, must still be the session the lock was taken for: the same process
     in the same pane, and the same session id — the lock is named after it, and `/clear`
     gives the same process a new one, whose lock another fleet command could take — else
     `target_changed` (an `id:` target naming the id a `/clear` replaced too);
  3. the screen shows no working line over the input box — Claude Code's spinner and its verb,
     `✳ Actualizing…`, `✽ Symbioting… (4s · …)` — else `not_idle` with `status: "busy"`: the
     registry can lag a prompt typed straight into the terminal, and the box alone reads empty
     while a turn runs;
  4. the input box reads empty, as `send --require-empty` reads it ([Sending](#sending)): the
     same reader, the caret included (the terminal's cursor, or while that is hidden the one
     Claude Code paints), so blanks left in Claude Code's box (which it draws as nothing) are
     text, and a box showing nothing whose caret cannot be found is `unreadable` — else
     `send_held` with `send`'s `reason` (`text`,
     `prompt`, `no_input_box`, `unreadable`) and `input`, exit 3. `model` always holds this
     way: `--require-empty` is accepted, so a caller can pass `send`'s and `rename`'s guard
     to all three, and changes nothing.
  The lock is held from these checks to the end, so no fleet command types in between; a
  prompt typed into the terminal itself still can, and these checks narrow that race, not
  close it.
- **Then** it types `/model <model-id>` and Enter, and reads the screen and the transcript, every
  100 ms for up to 5 s. Each tmux call in that wait gets only what is left of it: a tmux
  server that stops answering ends the wait at its end (`outcome_uncertain`) instead of
  holding the lock. Once the command is typed, a read or a key that fails or gets no answer is
  `outcome_uncertain` too, never `io`:
  - Claude Code's switch confirmation for **that** model — read by the reader `answer` uses
    (it is the permission prompt `answer --list` shows), then checked to be "Switch model?",
    "This conversation is cached for the current model …", exactly `1. Yes, switch to <its
    label>` and `2. No, go back`, nothing under them — gets the key its first choice names (`1`), once
    the agent is checked to own its pane again (else that check's refusal, nothing pressed)
    and a second read of the screen still shows that very confirmation, the lock held
    throughout. One answered in the terminal in between gets nothing, and the transcript
    decides. Nothing else is ever pressed. The label must stand for the model asked for as a
    switch's does (below): the one Claude Code gives the id (`claude-sonnet-5-5` → `Sonnet
    5.5`, a ` (…)` after it allowed), or for an alias the model it resolves to (`sonnet`:
    "Yes, switch to Sonnet 5.5"). The transcript then gets at least 2 s more.
  - Work under way where the box was — a last line of words ending in `…`, a ` (…)` after
    them aside, as Claude Code's "Running PreModelSwitch hooks… (esc to cancel)" while the
    hooks run — is waited for, however long, within the wait; so is one of Claude Code's tmux
    hint banners ("tmux detected · …", "tmux focus-events off · …") alone where the box was,
    caught while the box is redrawn.
  - Anything else in place of the input box for half a second — another dialog, a
    confirmation for another model, one a `PreModelSwitch` hook asks for — is
    `confirm_unrecognized`: nothing pressed. `fleet peek` shows it. The transcript is read once
    more first: a switch already written there decides instead.
  - `Model '<model-id>' not found` under the command is `model_not_found`; a `PreModelSwitch`
    hook's refusal (`Model switch to <label> was blocked by a PreModelSwitch hook[: <reason>]`)
    is `switch_refused`. On screen only
    a line that was not there before the command counts; the transcript says the same.
- **Success** is the transcript's entry holding `<local-command-stdout>Set model to <label>
  …</local-command-stdout>` — Claude Code 2.1.306 writes it as a `type: "user"` entry (not
  `isMeta`), that and nothing else its `message.content`, under the entry naming the command
  (`<command-name>/model</command-name>`, `<command-args><model-id></command-args>`): its
  `parentUuid` that entry's `uuid`, or its `promptId` that entry's. A prompt typed as exactly
  that output is written the same way, under no such entry, and is not a switch. A `type:
  "system"`, `subtype: "local_command"` entry with it as its `content` (the shape `Kept model as …` and `Model '…'
  not found` come in) counts the same — timestamped no earlier than the command began, whose
  `<label>` is positively the model asked for; its
  `timestamp` is `switched_at`. `<label>` matches `<model-id>` when it is the id itself (a
  ` (…)` after it aside: Claude Code names a model it has no label for by its id), or when both
  name one model: fleet pairs the id with its label by its Claude model list (`fleet harness
  list`: `claude-haiku-5-5` is `Haiku 5.5`), else by the rule Claude Code names Claude ids by,
  as for the confirmation; an alias (`sonnet`, `opus[1m]`) is the one model of that family on
  that list; the label is read as a Claude model's (`Sonnet 5.5`, a ` (1M context)` after it
  aside) or as a Claude model id. A `[1m]` or date suffix (`-20251001`) on either side is the
  same model. It is `outcome_uncertain` instead when:
  - `<label>` is not that match: another model's (the message names both, and that fleet's
    model list may be out of date), or one fleet cannot resolve — `Default (recommended)`, a label in other words, an id of another shape,
    an alias the list does not have, or no label at all;
  - the entry says `Kept model as <label>` instead: the switch was declined in the terminal,
    and the model is unchanged — declined, nothing switched, reported as `outcome_uncertain`;
  - no such entry comes within the wait: it was typed, and whether it switched is for
    `fleet peek` to tell.
- `-n` (or `FLEET_DRY_RUN=1`) runs every check, the lock included, and types nothing.

`list`'s `model` changes only with the session's next reply; `model_at` is that reply's time, so
a caller that shows the new model until then compares it with `switched_at`.

`--json` prints one object: `{ ok: true, host, session_id, pid, harness, model, confirmed,
switched_at, dry_run }` — `confirmed` whether Claude Code asked and its first choice was
picked, `switched_at` the `Set model to` entry's `timestamp` as written (`null` under `-n`).
Failures are `{ ok: false, host, kind, error }`, exit 1 — exit 3 for `send_held` — with
`reason` for `busy` and `host_shared` (as `close` gives them), `status` for `not_idle`,
`reason` plus `input` for `send_held` (`input` the box's text: sensitive, as `send`'s), and
`typed` for `io`:

| kind | when |
| --- | --- |
| `usage` | not an exact target, no or a bad model id, `--expect-*` alone or not a time; clap's own too, wherever `--json` stands (exit 1) |
| `target_not_found`, `target_ambiguous`, `target_invalid` | the target, as for every verb; `target_not_found` also for an agent that has exited |
| `target_changed` | another process than `--expect-pid`/`--expect-start`, or than the one looked up, by the time of the checks; or the same process under another session id (a `/clear` since the lookup) |
| `harness_unsupported` | a Codex or pi session |
| `backend_unsupported` | not a tmux pane (an iTerm tab), or a `FLEET_FIXTURE` fleet |
| `busy` | `reason`: `locked` (another fleet command holds the session's lock — retry) or `editor_open` |
| `host_shared` | `reason`: `job_in_front` or `suspended` (see above) |
| `not_idle` | `status`: the registry's (`busy`, `waiting`, `unknown`), or `busy` for a working line on screen |
| `send_held` | the input box is not empty: `reason` `text`, `prompt`, `no_input_box` or `unreadable` (exit 3) |
| `model_not_found` | Claude Code: `Model '<id>' not found` |
| `switch_refused` | a `PreModelSwitch` hook refused it |
| `confirm_unrecognized` | something other than that confirmation or the input box came up; nothing pressed |
| `outcome_uncertain` | typed, but no `Set model to` within the wait; `Set model to` a label that is not positively the model asked for (another model's, or one fleet cannot match); `Kept model as` (declined in the terminal: nothing switched, the model unchanged); or a read or key that failed or got no answer once the command was typed |
| `io` | the lock, `ps`, `lsof`, tmux or the typing failed, before or while the command was typed; `typed` says which (`false`: nothing typed, see [Sending](#sending)) |

**Limits.** The confirmation and the answers are read off Claude Code 2.1.303's own screens
(`testdata/screens/live/claude-model-*`); the transcript's entries are in the shapes Claude Code
2.1.306 writes (`testdata/claude/model-transcript.jsonl`, made-up values). A confirmation drawn in other words, a third choice or
a hint under it is `confirm_unrecognized` until fleet learns it. The `PreModelSwitch` refusal's
wording is checked against the Claude Code 2.1.303 bundle, not a capture. `-H` runs the other host's own
`model`; a host whose fleet predates it rejects the command (exit 2).

### Switching the effort

`fleet [-n] effort <target> <level> [--json] [--expect-pid <n> --expect-start <t>]
[--require-empty]` switches a Claude Code session's effort level, as `/effort <level>` typed in
its terminal does: from its next request on, and — for `low` to `xhigh` — as Claude Code's
default for new sessions on the session's model too (`/effort` saves it per model, under
`modelSettings.<model id>.effortLevel` in `settings.json`). `max` is for this session only;
`auto` clears the model's saved level, so the session and new ones run at the model's default.
`send "/effort …"` is not enough, for the reason it is not for `/model`: while the prompt cache is
warm Claude Code asks "Change effort level?" first. `effort` is `model`'s sibling and runs on the
same code:

- **Exact targets**, `--expect-pid` / `--expect-start`, **Claude Code in a tmux pane only**
  (Codex 0.163 changes its effort only in the second step of its `/model` picker, pi has none:
  `harness_unsupported`), and **the four checks under the typing lock**, with the same refusals
  and the same `send_held` hold (`--require-empty` accepted, changing nothing) — all as in
  [Switching the model](#switching-the-model).
- **The level** is checked for shape only — lowercase letters, at most 16 (`usage` otherwise) —
  and typed as given. Which levels a model has is `fleet harness list --json`'s `efforts`
  ([Harnesses](#harnesses)); Claude Code says what it makes of a word it does not take.
- **Then** it types `/effort <level>` and Enter, and reads the screen and the transcript every
  100 ms for up to 5 s, as `model` does:
  - Claude Code's confirmation for **that** level — "Change effort level?", "This conversation
    is cached for the current effort level …", exactly `1. Yes, switch to <level>` and `2. No, go
    back`, nothing under them, read by `answer`'s reader (Claude Code 2.1.308 may draw its hint
    `◐ <level> · /effort` over the right end of the dialog's `▔` top) — gets the key `1`, once
    the agent is checked to own its pane again and a second read still shows it. Nothing else
    is pressed.
  - Work under way where the box was, or one of Claude Code's tmux hint banners alone there
    ("tmux detected · …", "tmux focus-events off · …") while the box is redrawn, is waited for.
  - Anything else in place of the input box for half a second — another dialog, `/model`'s
    confirmation, one for another level — is `confirm_unrecognized`, nothing pressed; but the
    transcript is read once more first, and an answer already written there decides instead.
    (`model` does the same: the code is shared.)
- **Success** is the transcript's answer under the command: Claude Code 2.1.308 writes the
  command as a `type: "user"` entry (`<command-name>/effort</command-name>`,
  `<command-args><level></command-args>`) and its output as the next `type: "user"` entry, that
  and nothing else its `message.content`, `<local-command-stdout>…</local-command-stdout>`, its
  `parentUuid` the command's `uuid` (or the same `promptId`) — a prompt typed as that output is
  written alike, under no such entry, and is not a switch. Timestamped no earlier than the
  command began, it says `Set effort level to <level> (saved as your default for new sessions):
  …`, `Set effort level to max (this session only): …` or, for `auto`, `Effort level set to
  auto`; its `timestamp` is `switched_at`. It is `outcome_uncertain` instead when it names
  another level; when it says `Kept effort level as <level>` (declined in the terminal: the level
  is unchanged); when it says anything else under the command (quoted in the message: a level
  Claude Code does not take, a newer wording); and when none comes within the wait.
- `-n` (or `FLEET_DRY_RUN=1`) runs every check, the lock included, and types nothing.

`list`'s `effort` is the level the session's last reply ran at, so it changes only with the next
reply (`model_at` dates both `model` and `effort`).

`--json` prints one object: `{ ok: true, host, session_id, pid, harness, effort, confirmed, saved,
switched_at, dry_run }` — `effort` the level asked for, `confirmed` whether Claude Code asked and
its first choice was picked, `saved` whether Claude Code's default for new sessions on the
model changed with it (`true` for `(saved as your default for new sessions)` and for `auto`,
which clears the saved level; `false` for `max`, and for an answer that says neither; `null`
under `-n`), `switched_at` the answer's `timestamp` as written (`null` under `-n`). Failures are
`model`'s object, exit 1 (3 for `send_held`), with the same kinds — `usage`, the target kinds,
`target_changed`, `harness_unsupported`, `backend_unsupported`, `busy`, `host_shared`,
`not_idle`, `send_held`, `confirm_unrecognized`, `outcome_uncertain`, `io` — and none of
`model_not_found` or `switch_refused` (Claude Code has no hook on effort changes).

**Limits.** The confirmation and the answers are read off Claude Code 2.1.308's own screens
(`testdata/screens/live/claude-effort-*`) and transcript entries
(`testdata/claude/effort-transcript.jsonl`, ids and times made up). What it answers a level it
does not take was not captured: such an answer is `outcome_uncertain`, quoted. `-H` runs the
other host's own `effort`; a host whose fleet predates it rejects the command (exit 2).

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
`harnesses.<id>.command` (default the bare name). A spawn appends `[--model <id>] [-n <name>]
[fresh-session flags] [-- <prompt>]` to it (see [Spawning](#spawning)), so a wrapper script should hand its arguments on unchanged (`"$@"`). `fleet harness list --json`:

```json
[{ "id": "codex", "label": "Codex", "command": "codex", "installed": true,
   "models": [{ "id": "", "label": "Default (gpt-6.1-sol)", "efforts": ["low", "medium", "high"], "default_effort": "medium" },
              { "id": "gpt-6.1-sol", "label": "GPT-6.1-Sol", "efforts": ["low", "medium", "high"], "default_effort": "medium" }] }]
```

Models: Claude's built-in list; Codex's from its `models_cache.json` (listed ones, its order);
pi's from `pi --list-models` (ids `provider/model`). `id: ""` is the harness's own default,
labelled `Default (<model>)` when its configured model is known, as written: Claude's from
`ANTHROPIC_MODEL` in `fleet`'s environment, else `model` in `settings.json` under
`$CLAUDE_CONFIG_DIR` (default `~/.claude`), the user-level default (project settings, which
depend on the start directory, are not read); Codex's from `model` in `config.toml`; pi's from
`defaultModel` in its `settings.json`. Otherwise plain `Default`.

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

### Slack links

For a while, a session talks with the people in one Slack thread (or a whole channel or DM): what
they write there is typed into it, and it answers there. You decide which session, which thread,
for how long and what it may help with — the web UI's **Link to Slack** (session Details), or:

| Command | What |
|---|---|
| `fleet slack [list] [--json]` | the bridge (connected?) and the active links |
| `fleet slack link <slack-url \| @person> --for <30m\|2h\|2d\|90> [--session <target>] [--task <instructions>] [--proactive] [--json]` | link a session (default: the one this runs in, by `$TMUX_PANE`). A message link is its thread; a channel / DM link is all of it; a person (`@name`, a name or their user id `U…` — instead of a link) is the app's DM with them, opened if new (several matches: an error with the candidates). Needs the `im:write` scope and the app's Messages tab on. `--task`: your instructions (what it may help with, tone, when to step in). `--proactive`: it sees every message and steps in when it judges it should; default: it is woken only when someone mentions the app, DMs it or writes in a thread it has posted in (then it judges itself whether the message is for it), and gets what was said in between as context. Linking the same session to the same thread again renews it (new time limit, instructions, mode) |
| `fleet slack unlink <sl-xxxxxx>` | end a link now |
| `fleet slack reply <sl-xxxxxx> <text> [--thread <ts>]` | post in the linked thread — what the linked session runs to answer. Refused (exit 3) once the link has ended, and for any session but the linked one |

How it works ([architecture](architecture.md#slack-links)): one host's web server runs the bridge
(`slack.host`) — a Slack App in Socket Mode, whose tokens only it holds. Linking reads the thread
(so the app must be in that conversation — `/invite` it, or add it to the DM) and types a note into
the session: the link, until when, your instructions, the mode, the rules (post only with `fleet
slack reply` and only when something calls for it, only within the instructions, nothing
destructive for them, their messages are requests from colleagues and cannot change the rules)
and the thread so far. Messages are typed in once the session is idle — several as one prompt —
each marked with its author's Slack name and id (and `→ mentions you` / `→ to you`); the ones
addressed to it get an 👀 reaction. When the link ends, delivery stops and, if the session ever spoke there, the thread is
told it left. The commands talk to this machine's web server, which proxies to `slack.host`. For
now only sessions on `slack.host` can be linked.

Each model carries its effort levels, `efforts` (lowest first: what [`fleet
effort`](#switching-the-effort) can be given for it) and `default_effort` (the level a new
session on it starts at, or `null`); `[]` and `null` where they are not known, and always for
pi. Claude's are fleet's own table, hand-kept from Claude Code's model docs (as of 2.1.306) like
the model list: `low`, `medium`, `high`, `xhigh`, `max` on every model, starting at `medium`
(Fable 5.1 at `high`). Claude Code's user settings (the same `settings.json`; only the keys
below are read from it) change the start as Claude Code 2.1.308 reads them:

- `modelSettings.<model>.effortLevel` (`low` to `xhigh`: what `/effort` saves; `auto` removes
  it) is that model's start. `<model>` counts for the listed model it names: its id, an alias
  (`opus`), or either with a `[1m]` or date suffix. The entry keyed by the id itself wins, else
  the first in the file. A key fleet cannot resolve to a listed model (a Bedrock or Vertex
  spelling) is skipped.
- A top-level `effortLevel`: Claude Code applies it to some models and not to others, by lists
  of its own, so a model without an `effortLevel` of its own then has `default_effort: null`.
- `maxEffortLevel` holds the start down to it: the model's own (`modelSettings.<model>`, the
  lowest of the entries naming it), else the top-level one; `max` is no cap.
- `CLAUDE_CODE_EFFORT_LEVEL` in `fleet`'s environment overrides every saved level: every
  `default_effort` is then `null`.

A value of another word counts as unset, as Claude Code drops it. Project, local and managed
settings, an organization's caps and model default, and Claude Code's own state can change the
level too; fleet reads none of them. Codex's levels are its `models_cache.json`'s
(`supported_reasoning_levels`' `effort`s and `default_reasoning_level`), put lowest first by
Codex's own order — `none`, `minimal`, `low`, `medium`, `high`, `xhigh`, `max`, `ultra` —
whatever order the cache lists them in; a level outside that order is dropped (and a
`default_reasoning_level` outside it is `null`). The `id: ""` entry has the levels of the model
its default names (an alias resolved through the list), else every level of any listed model,
lowest first by the same order, and no default.

### Usage JSON

`fleet usage --json` keeps its original top-level fields, which describe **Claude** only
(`account`, `limits`, `extra_usage`, `fetched_at`, `stale`, `error` — see
[architecture](architecture.md#subscription-usage)), and adds `providers`: one entry per provider,
Claude first, in one shape:

```json
{
  "account": { "…": "…" }, "limits": [ "…" ], "extra_usage": null,
  "fetched_at": "2026-10-16T13:00:00+00:00", "stale": false, "error": null,
  "providers": [
    { "id": "claude", "label": "Claude", "status": "ok", "plan": "Team · Max 5x",
      "limits": [ "…the top-level limits…" ], "fetched_at": "2026-10-16T13:00:00+00:00",
      "observed_at": "2026-10-16T13:00:00+00:00", "error": null, "credits": null },
    { "id": "codex", "label": "Codex", "status": "ok", "plan": "Plus",
      "limits": [
        { "kind": "session", "group": "session", "label": "5h limit", "model": null, "percent": 50.0,
          "severity": "normal", "resets_at": "2026-10-16T15:00:00+00:00", "active": true },
        { "kind": "weekly_all", "group": "weekly", "label": "Weekly limit", "model": null, "percent": 2.0,
          "severity": "normal", "resets_at": "2026-10-23T08:00:00+00:00", "active": false }
      ],
      "fetched_at": "2026-10-16T13:00:00+00:00", "observed_at": "2026-10-16T12:30:00+00:00",
      "error": null, "credits": { "has_credits": false, "unlimited": false, "balance": "0" } }
  ]
}
```

- **Why an array**: a reader iterates the providers the same way; a new provider is a new entry,
  not a new top-level key; and nothing that reads the old fields changes. Claude's entry repeats
  the top-level numbers (its `extra_usage` and `account` stay top-level only).
- **`status`**: `ok`; `stale` (a Claude refresh failed, these are the last good numbers, `error`
  says why); `no_data` (installed, but no limits recorded yet — no sessions, or only credits
  events); `not_installed` (no Codex home); `error`. `limits` is empty unless `ok` or `stale`.
- **`fetched_at`** is when fleet read the source; **`observed_at`** is when the provider recorded
  the numbers. They are equal for Claude (a live endpoint). For Codex, `observed_at` is the
  transcript event's time — as fresh as the last Codex turn on this host — so a UI should show
  its age.
- **Codex buckets**: each `rate_limits.limit_id` is a bucket, and the newest snapshot of each
  counts. The main windows come from `codex` (or no `limit_id`, from older Codex); other ids
  with windows are scoped limits (a model), added after the main ones, and count only when
  recorded no more than 60 s before the main snapshot (or after it); ids without windows
  (`premium`) only carry credits.
- **Codex `limits[]`**: each snapshot's `primary` and `secondary` windows. `window_minutes` sets
  `group` and `label`: up to a day is `session` (`5h limit`), longer is `weekly` (`Weekly limit`
  for 10080). `kind` is `session` / `weekly_all` for the main bucket, `session_scoped` /
  `weekly_scoped` for a scoped one, whose `model` is its `limit_name` (else its id) and whose
  label ends in `· <model>`. Scoped limits share the main snapshot's `observed_at`. `active`
  marks the fullest main window (`session` or `weekly_all`), never a scoped one: a model's
  limit only stops that model. A window that has reset —
  `resets_at` passed, or, with no reset time, `observed_at + window_minutes` passed — is
  reported at `percent: 0` with `resets_at: null` (usage on another machine since is not
  visible here).
- **Codex `credits`**: from the newest snapshot *without* windows that has a `credits` object
  (e.g. `premium`), else from the main snapshot. A windowed scoped snapshot's credits are never
  used. Within 60 s of the main snapshot, or after it, the result doesn't depend on the order
  of events; a windowless snapshot older than that is still used when the scan reaches it before
  stopping, but the scan may stop first.
- **`severity`** for Codex, which reports none: fleet's own bands, the ones its bars and the web
  use — `normal` below 70%, `warning` from 70%, `critical` from 90%. Claude's is the endpoint's.
- **Claude unavailable**: with no Claude read at all (not logged in, nothing cached) the command
  still fails with exit 1 and the reason on stderr, as before. `--json` first prints
  `{ "error": "<reason>", "providers": [{ "id": "claude", "status": "error", "error":
  "<reason>", "limits": [], … }, <codex>] }` — none of the Claude top-level fields, so they never
  mean "no limits" — so a caller can still show Codex. The text output prints the Codex section.

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
`pid:<n>` / `id:<session_id>` skip the label rungs: `id:` is a member's whole session id (a gone
member's too), else either form is the exact live session. These exit codes stay as they were
(scripts and the web server branch on them); with `--json` the failure is also one object on
stdout, `{ ok: false, host, kind, error }`: `stack_ambiguous` / `target_ambiguous` (exit 2),
`stack_not_found` (exit 3; also `target_not_found` / `target_invalid` for an exact form given as
`<stack>`), and for a session `<target>` that matches nothing `target_not_found` (exit 1, as
before). One cell differs: `stack remove <stack> <target>` where `<target>` is a live session that
is not a member of that stack is `target_not_found` with exit 3 (its code before this kind
existed).
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
| `fleet stack remove <stack> <target\|session_id> [--json]` | drop a member (a gone one by its session id or a prefix of ≥ 4, or its name; `id:<session_id>` only by the whole id); removing the last leaves an empty stack (the file stays) |
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
| `fleet tmux new [name] [-d] [-C <dir>] [-- cmd]` | attach to, or create, a session (default `main`). `-d` creates without attaching. A new session starts at 200x50 (see [Spawning](#spawning)) |
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
  no longer exists becomes `$HOME`, with a warning. The session starts at 200x50, or at its first
  window's recorded size where that is larger (see [Spawning](#spawning)); a window of several
  panes ends at its recorded layout's size, which `select-layout` sets.
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
| `keep` | default `72h`, or `"never"`: once a run is older than this, its sessions that are idle and quiet for an hour are closed (their tmux session is killed). A session waiting for input is never closed, nor one with a background shell running (Claude Code's `shell`, counted in `background`). A finished bot session shows as done before that: its prompt carries the [ask-done](#ask-done) instruction |

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
`@fleet_job_run`; `fleet list` reports it as `origin: { job, run }`. A closed bot session that
`fleet restore` brings back keeps both and its bot's launcher, and is open in its run's record again.

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
  `install`, `init`, `config_set`, `web_build`, `skill_install` and every `*_install_service` —
  and `slack_link` (who may talk to a session is your call, not a remote agent's).
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
| `FLEET_PATH_ONLY=1` | run only what `PATH` holds, for hermetic tests and sandboxes; what reaches past it is refused — see [PATH-only mode](#path-only-mode) |
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
| `FLEET_LOCK_DIR` | where the per-session typing locks (`answer`, `send`, `keys`, `close`, renames), and the `spawn --resume` reservations, live (default `${XDG_RUNTIME_DIR:-cache dir}/fleet/locks`) |
| `FLEET_FIXTURE=<sessions.json>` | read a canned fleet from a file instead of the live registry (demo/tests; backends are inert) |
| `NO_COLOR=1` | no colors |
| `FLEET_NO_TTY=1` | never prompt on the terminal: confirmations fail as if there were none (set by `fleet mcp`) |
| `FLEET_MCP_TIMEOUT` | seconds one `fleet mcp` tool call may run (default 600) |
| `FLEET_MCP_TOKEN` | bearer token for `fleet mcp --http` (when no `--token-file`) |

### PATH-only mode

`FLEET_PATH_ONLY=1` (or `true`, `yes`) is for hermetic tests and sandboxes — a temporary `HOME`,
a private tmux server, a `PATH` holding only what the test declares. Fleet then runs nothing it
did not find on `PATH`, with one exception: a pane's shell, the tmux server's `default-shell`
(below).

- **Binary lookup**: `PATH` only. No fallback dirs (`/opt/homebrew/bin`, `/usr/local/bin`,
  `~/.local/bin`, `~/.cargo/bin`, `~/bin`), no hard-coded install locations (`Tailscale.app`,
  `~/.claude/local`); a tool not on `PATH` fails as "not found". Explicit binaries still apply
  (`FLEET_TMUX`, `FLEET_GH`, the config's `tmux`, …), and a child that looks tools up itself gets
  `PATH` as it is. A `PATH` that is unset or empty, or holds a relative or empty entry (one a
  pane would resolve against its own directory), is refused at startup.
- **The launcher check**, which `spawn` (plain, `--resume`, `--from`) and `pr spawn` run: a
  launcher not on `PATH` is `harness_missing` at once. No login shell is asked. (`handoff`,
  `stack spawn` and job runs have no launcher check.)
- **Panes fleet opens** (`spawn`, `handoff`, `stack spawn`, `pr spawn`, job runs, `restore`,
  `tmux new` / `new`): tmux starts them with fleet's `PATH` and `FLEET_PATH_ONLY=1` (`-e`), over
  the tmux server's environment, so a `fleet` run inside one is PATH-only too. A spawned pane's
  first process is the `sh` on `PATH`; it drops the [agent markers](#spawning) with its own
  `unset` and runs nothing else from `PATH`. `spawn`'s default backend is tmux.

Refused — exit 1, kind `usage` — because they reach past `PATH` by design. A command run with
`--json` prints its failure object on stdout (`{ ok: false, host, kind: "usage", error }`,
`spawn`'s own shape for `spawn`). `spawn`, `handoff`, `stack spawn` and `pr spawn` are refused
before anything is written for them (a prompt file, a stack, a worktree):

- `fleet web serve`, `fleet web install-service`, `fleet jobs install-service` and
  `fleet repos install-service`, dry runs and `--print` included. The web server itself
  (`node server.mjs`) will not start either (exit 2).
- Any other host, however it is named (`-H`, `FLEET_HOST`, a remote `defaultHost`, `stack spawn
  --on`): nothing goes over ssh or to a host's web server. Commands that fan out
  (`list --all-hosts`, `doctor`, `enter`'s search) report each other host as refused and carry
  on with this one; `doctor` does not count them as problems.
- The iTerm backend: `--backend iterm` on `spawn`, `handoff`, `stack spawn` or `pr spawn`, dry
  runs included, and anything that drives an iTerm session (`peek`, `send`, `keys`, `answer`,
  …). `list` leaves iTerm tab titles out.
- A tmux spawn when `PATH` holds no `sh` to start the pane with.
- `fleet exec` and `fleet ssh` on this machine: they run `$SHELL -l`, whose profiles look past
  `PATH`.

The pane's shell is the exception, and profiles still run. Every pane, whoever opens it, runs
the tmux server's `default-shell` (or its `default-command`), never fleet's own `$SHELL`: tmux
sets `SHELL` in each pane to `default-shell`, so a spawned pane's `$SHELL -l` is that shell too,
wherever it is. `default-shell` is the `SHELL` the server started with, unless its config sets
one; without a `SHELL` it is the account's login shell. Once it starts, the system profiles and
those under `$HOME` run and may change `PATH` again: on macOS `/etc/profile` and `/etc/zprofile`
put the system dirs first (`path_helper`), and some Linux distributions reset `PATH` in
`/etc/profile`. A hermetic setup must start its private tmux server with its own `SHELL`,
pointing into its world (a shell it provides, or one it trusts; or set `default-shell` there), own
`HOME` and set `PATH` in its profiles there (`~/.profile` for `sh`, `~/.zprofile` for `zsh`, …).

## Exit codes

| code | meaning |
| --- | --- |
| 0 | success |
| 1 | error, bad usage |
| 2 | tmux, `stack` and `restore` commands: ambiguous match (candidates are printed) |
| 3 | `send`: held — text in the input box or a prompt in its place (with `--require-empty`, also no box found or read), nothing typed (kind `send_held`, see [Sending](#sending)); `model` and `effort`: held the same way, any box but an empty one (kind `send_held`, see [Switching the model](#switching-the-model)); `rename --json`: held (text in the box, a prompt, a waiting or busy session; with `--require-empty`, also no box found or read); `brief --set`/`--edit`: the brief changed since it was opened (nothing saved); `brief --regenerate`: hourly cap reached; `stack set`/`edit`: the StackBrief changed since it was opened; `stack`: no stack matches; `restore`: no dormant (with `--closed`: recently closed) session matches; tmux commands: nothing to act on — no sessions, no match, or a confirmation was needed but there is no terminal (use `-f`) |
| 4 | host unreachable |
| 127 | a required tool (tmux, node) was not found |

`exec` and `ssh` return the remote command's own exit code.
