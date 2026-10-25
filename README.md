# fleet

Supervise a fleet of Claude Code sessions across your machines — from a terminal or from your phone.

`fleet` is one Rust CLI plus a small web app (a zero-dependency Node server and a React PWA). It finds every live Claude Code
session on a machine (in tmux panes or iTerm tabs), shows what each one is doing, and lets you
peek at, steer, rename, spawn and hand work off to them. [Codex](https://github.com/openai/codex)
and [pi](https://pi.dev) sessions are found and driven too — pick the agent and its model when you
spawn (`fleet spawn --harness codex`, or the web New session form); see
[docs/cli.md → Harnesses](docs/cli.md#harnesses). It also manages the tmux sessions on an
always-on **workstation** over ssh, so the same command works from your laptop and on the
workstation itself.

The setup it is built for:

- an always-on Mac (the *workstation*) runs Claude Code inside tmux, 24/7;
- your laptop and phone reach it over [Tailscale](https://tailscale.com);
- closing the laptop lid never interrupts an agent — the tmux server lives on the workstation.

## Architecture

```
  laptop                         phone                           workstation (always on)
 ┌──────────────────────┐      ┌──────────────────┐            ┌──────────────────────────────┐
 │ fleet <cmd>          │      │ browser / PWA    │            │ tmux server (24/7)           │
 │  ├─ local sessions   │      │  (fleet web UI)  │            │  ├─ job-a: claude            │
 │  └─ -H workstation ──┼─ssh──┼──────────────────┼──────────▶ │  ├─ job-b: claude            │
 │                      │      │                  │   http     │  └─ fleet: fleet watch (TUI) │
 │ fleet web serve :7777│◀─────┼── http ──────────┼──────────▶ │ fleet web serve :7777        │
 └──────────────────────┘ peer │                  │  (tailnet) └──────────────────────────────┘
            ▲                  └──────────────────┘                        ▲
            └─────────────── Tailscale (WireGuard mesh, private IPs) ──────┘
```

- **CLI** — `fleet` discovers sessions from Claude Code's own registry (`~/.claude/sessions/`),
  drives them through tmux or iTerm, and dispatches to other hosts by re-running itself over ssh
  (`ssh <host> fleet <same args>`).
- **Web** — every machine runs the same server. It serves its own sessions and merges in its
  peers', so any one URL shows the whole fleet. Built for a phone: list, chat,
  send a message, spawn a session.
- **Config** — one small JSON file per machine (`~/.config/fleet/config.json`), written by
  `fleet init`, read by both the CLI and the web server.

Details: [docs/architecture.md](docs/architecture.md).

## Quickstart

Requirements: macOS (the only tested platform; iTerm support is macOS-only), tmux, Claude Code, Rust (to build),
Node ≥ 22 (for the web app), Tailscale + ssh between machines for multi-host use.

Build prerequisites: a Rust toolchain ([rustup](https://rustup.rs); the repo pins it in
`rust-toolchain.toml`) and, on macOS, the Xcode Command Line Tools (`xcode-select --install`) for
the linker. If a full Xcode is installed but its license was never accepted, linking fails with
"You have not agreed to the Xcode license agreements" — run `sudo xcodebuild -license accept`, or
build against the Command Line Tools instead:
`DEVELOPER_DIR=/Library/Developer/CommandLineTools cargo install --path crates/fleet`.

```sh
git clone https://github.com/ilucin/fleet && cd fleet
cargo install --path crates/fleet     # installs `fleet` into ~/.cargo/bin
fleet web build                       # build the web UI (npm ci + vite build → web/ui/dist)

fleet init                            # interactive: this machine's name, hosts, spawn dirs, web port
fleet install --host workstation      # copy the binary + web app (incl. built UI) to the workstation
fleet doctor                          # reachability, versions on both ends, config sanity

fleet list                            # Claude sessions on this machine
fleet -H workstation list             # ...on the workstation
fleet web serve                       # web UI on :7777 (run it on each machine)
```

Then open `http://<workstation-tailscale-ip>:7777` on your phone and add it to the home screen. On a
Mac, `crates/fleet-desktop/bundle.sh --install` builds **Fleet.app**, the same UI as a desktop app
([docs/desktop.md](docs/desktop.md)). Other apps can frame one directory's sessions as a chat
panel: the embed view, `https://<fleet>/?host=…&dir=…#/embed`
([docs/architecture.md → Embed view](docs/architecture.md#embed-view)).

Building the whole setup from scratch (always-on Mac, Tailscale, ssh, tmux, phone):
[docs/setup.md](docs/setup.md).

## Commands

| area | commands |
| --- | --- |
| Claude sessions | `list` · `peek` · `send` · `rename` · `name` · `spawn` · `handoff` · `watch` (default) · `skill` |
| tmux sessions | `tmux list\|enter\|last\|new\|kill\|rename\|stale` (alias `t`), shortcuts `enter` · `last` · `new` |
| after a reboot | `restore` — dormant sessions: list, bring back (same tmux layout, `claude --resume`), forget |
| hosts | `exec` · `ssh` · `doctor` |
| setup | `init` · `config path\|show\|edit\|set` · `install --host <name>` |
| pull requests | `prs` — the GitHub PRs that need you (unseen @mentions, yours, waiting on your review, waiting on their author), through `gh`, with their local worktrees |
| Productive tasks | `tasks` · `task show\|status\|spawn` — your open Productive tasks (due soon, in progress, to do), moved between statuses, a session started on one |
| git repos | `repos` · `repos sync` · `repos install-service` — fetch + fast-forward every repo under `~/Code`, on a timer |
| bots | `bots` · `bots show\|run\|enable\|disable` · `bots install-service` — agent sessions a host starts from a recipe (prompt + schedule), like Claude Code routines; everything a bot session spawns joins its run. `jobs` is the old name, still an alias |
| web | `web serve` · `web build` · `web install-service` |
| agents | `mcp` — every command above as an MCP tool, for Claude Code and other agents |

Global flags: `-H, --host <name>`, `--local`, `--json` (where it applies). Full reference:
[docs/cli.md](docs/cli.md).

A few examples:

```sh
fleet                                   # live dashboard (TUI); piped, it prints `list`
fleet peek docs-refresh                 # what that session shows right now
fleet send docs-refresh "run the tests" # type into it and press Enter
fleet list -a --json                    # every host, one array, rows tagged with host
fleet new billing-fix -C ~/Code/project # attach to (or create) a tmux session on the workstation
fleet last                              # back to the previous tmux session
fleet tmux stale                        # idle shells nothing is using
```

## Docs

- [docs/setup.md](docs/setup.md) — end-to-end guide: workstation, Tailscale, ssh, tmux, Claude Code, phone
- [docs/architecture.md](docs/architecture.md) — components, config, discovery, JSON contracts, extension points
- [docs/cli.md](docs/cli.md) — command reference
- [docs/desktop.md](docs/desktop.md) — Fleet.app, the web UI as a macOS app (Tauri)
- [docs/migration.md](docs/migration.md) — coming from `tb-fleet`, `ws` or `fleet-web`
- [web/](web/) — the web server and its HTTP API

## Security

There is **no authentication** in the web app. It is meant to be reachable only over your
tailnet: bind it to the Tailscale interface (or firewall it) and never expose it publicly. Anyone
who can reach the port can type into your Claude sessions (and read the notes under `web.notes.root`, when set). See
[docs/setup.md#security](docs/setup.md#security).

## License

MIT — see [LICENSE](LICENSE). Portions derived from
[productiveio/cli-toolbox](https://github.com/productiveio/cli-toolbox).
