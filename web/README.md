# fleet-web

HTTP API + mobile-first PWA for supervising and steering Claude Code sessions across your
hosts. The server is Node ≥ 22 with zero npm dependencies and no build step; no auth (keep it on
a private network such as a Tailscale tailnet). The UI is `ui/`, a React + shadcn/ui app
(**built**: `npm --prefix ui ci && npm --prefix ui run build`, or `fleet web build`; output
`ui/dist/`, gitignored) — see [ui/README.md](./ui/README.md). Design, config mapping and the full
API: [ARCHITECTURE.md](./ARCHITECTURE.md).

Each host runs the same `server.mjs`. It lists its own sessions with `fleet list --json` and,
when asked for the whole fleet, merges in the other hosts' servers over HTTP — any host's URL
shows everything.

## Run

```sh
fleet web serve            # via the CLI (uses your fleet config)
bin/dev.sh                 # foreground, 127.0.0.1:7799
bin/dev.sh 7800            # another port
node server.mjs            # plain: config port/bind (default 7777)
```

Open `http://<host>:<port>/` on your phone and "Add to Home Screen". Health: `/api/health`.

## Config

Reads the shared fleet config (`$FLEET_CONFIG` or `~/.config/fleet/config.json`, written by
`fleet init`). Relevant keys: `self`, `hosts.<name>.web` (peers), `web.port`, `web.bind`,
`web.ui` (a path), `web.editor`, `web.quickReplies`, `web.models`, `web.autoName`, `web.grouping`, `web.briefs`, `web.stacks`, `restore.onBoot`, `grouping.host`, `stacks.model` (reported only), `tmux`, `fleetBin`, `claude` / `harnesses.<codex|pi>.command` (how each agent launches in a spawn), `harnesses.<claude|codex|pi>.model` / `.effort` (the default a spawn starts with; Settings → Models), `spawnDirs` (spawn only accepts dirs inside these; editable in Settings → Start directories, which writes it through `fleet config set`), `repos` (Settings → Git repos: `fleet repos` roots, intervals and the sync timer, per host). Example:
[`config.example.json`](./config.example.json). Without a config it runs as a single local host
on 127.0.0.1.

Env overrides: `FLEET_CONFIG`, `FLEET_WEB_PORT` / `PORT`, `FLEET_WEB_BIND`, `FLEET_WEB_UI`,
`FLEET_WEB_AUTONAME` / `FLEET_WEB_GROUPING` / `FLEET_WEB_BRIEFS` / `FLEET_WEB_STACKS` (`0` / `1`), `FLEET_BRIEFS_DIR`, `FLEET_BIN`, `FLEET_TMUX`.

`web.autoName` (default off; opt in with `{ "enabled": true, "intervalMinutes": 5 }`) makes each server run
`fleet name --all --apply` on its own host every few minutes, so sessions started without a
name get a task-shaped one (tmux session included, when its name was generic). Set
`"enabled": false` to keep names manual.

`web.grouping` (default off; opt in on ONE host with `{ "enabled": true, "intervalMinutes": 10 }`,
and point the others at it with top-level `"grouping": { "host": "<that host>" }`) makes that
server run `fleet group` over the whole fleet and serve `GET /api/groups` for the Board view; the
others proxy to it. It only reads sessions and calls `claude -p`; nothing is sent to a session.

`web.briefs` (default off; opt in with `fleet config set web.briefs.enabled true`) makes the server keep a **brief** per
idle session on its host — summary, resources it produced (incl. one `Git:` line: branch +
checkout), todos — in
`~/.local/state/fleet/briefs/<session_id>.md`, for starting a new session that continues the work.
Resources and todos come from the transcript for free; the summary costs one
`claude -p --model haiku` call, gated hard (idle ≥ 60s, ≥ 2 new prompts or 2000 new characters,
≥ 15 min per session, one at a time, ≤ 12/hour — all tunable: `model`, `idleMs`, `minIntervalMs`,
`maxDeltaChars`, `maxCallsPerHour`, `minNewTurns`, `minNewChars`, `maxBriefChars`). Reading and
editing briefs (`GET`/`PUT …/brief`) and a manual regenerate work with it off.

**Dormant sessions** (session recovery after a reboot, owned by `fleet restore`): the server
serves `GET /api/hosts/:host/dormant` and `POST …/dormant/restore` / `…/dormant/forget`
(`{ target }` or `{ all: true }`); the UI lists them per host under the session list (Resume,
Forget, Resume all) and dims dormant group members on the Board. Config `restore.onBoot: true`
resumes everything once per boot when the server starts. Sessions closed within a boot come in
the same list as `closed` (body `closed: true` on restore / forget) and show in a collapsed
"Recently closed" section (Resume, Forget — no Resume all, not on the Board). Details:
ARCHITECTURE.md → HTTP API.

**Session stacks** (sessions sharing one StackBrief file, owned by `fleet stack`): the server
serves `/api/hosts/:host/stacks…` (list, show, edit with conflict check, rename, delete, sync) and spawns
siblings (`POST …/sessions/:id/stack/spawn` creates the stack around a session first — one
Sonnet call — and `POST …/stacks/:id/spawn`), here or on another host (`host` + `dir` in the
body: the peer starts it, this host adds it), adding the new session to the stack once it
registers. It runs `fleet stack sync` (with the merged fleet, so members on other hosts count)
after a kill and every `web.stacks.syncMinutes` (default 2) while a session is in a stack;
`FLEET_WEB_STACKS=0` turns that background sync off. Each host advertises its stacks in
`/api/fleet` so members on other hosts show up in them. Routes, statuses and limits:
ARCHITECTURE.md → HTTP API / Session stacks.

`web.editor` (`"vscode"` default, `"cursor"`, or `null` to hide it) sets the "Open in editor"
link on briefs, stacks and session rows (`editorUrl`): a local folder for sessions on this server's host,
Remote-SSH through `hosts.<host>.ssh` for the others (no ssh alias → no link).

## Another UI

The API is independent of both bundled UIs. Point `web.ui` (or `FLEET_WEB_UI`) at a directory
with your own `index.html` and it is served instead, same origin as `/api/*`. Files under
`/assets/` with a content hash in the name (`index-R-dVrV7d.js`) are sent `immutable`; everything
else is `no-cache` + ETag.

## Service / deploy

`fleet install --host <name>` copies the web app to a remote host (the server and
only the *built* `ui/dist` — never `ui/` sources or `node_modules`; build first) and
`fleet web install-service` sets it up as a service (launchd, logs to
`~/Library/Logs/fleet.web.log`, node pinned via `web.node` or a stable Homebrew install); see the CLI docs. Prefer running it from
tmux or a user service with a normal PATH (see ARCHITECTURE.md → "Running as a service").

## Tests

```sh
node --test tests/         # fast, no network beyond 127.0.0.1, child processes faked
npm --prefix ui test       # UI helper tests (vitest); also: run lint, run typecheck, run build
```
