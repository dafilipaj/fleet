# Setup guide

How to build the whole thing: an always-on Mac that runs Claude Code in tmux, reachable from a
laptop and a phone over Tailscale, supervised with `fleet`.

```
laptop / phone ── ssh + http over Tailscale ──▶ workstation
                                                  └── tmux server (lives 24/7)
                                                        └── one tmux session per job, each running claude
```

The tmux server lives on the workstation independently of any connection, so a laptop going to
sleep or a phone losing signal never interrupts an agent.

Names used below: `laptop` (your client machine), `workstation` (the always-on Mac). Replace
`100.x.y.z` with the workstation's Tailscale IP and `me` with its macOS user.

## 1. Workstation machine prep

Any Apple Silicon Mac works (a Mac mini is ideal; a laptop with the lid closed on power also works).

1. **User account.** A local user with a strong password. An Apple ID is optional — leaving it
   off keeps the machine unattended and free of iCloud prompts; install everything via Homebrew or
   direct downloads instead of the App Store.
2. **Remote Login.** System Settings → General → Sharing → Remote Login ON; make sure your user
   is in "Allow access for".
3. **Never sleep.**
   ```sh
   sudo pmset -a sleep 0 disablesleep 1
   pmset -g            # verify
   ```
   Apple Silicon Macs power back on by themselves when power returns.
4. **FileVault — pick your trade-off.**
   - **ON** (recommended): the disk is encrypted at rest. After an *unplanned* reboot (power cut,
     panic) the machine waits at the FileVault unlock screen until someone unlocks it. For
     *planned* reboots use an authorized restart, which comes back up unlocked:
     ```sh
     ssh -t workstation 'sudo fdesetup authrestart'
     ```
     For macOS updates: `softwareupdate -ia`, then `sudo fdesetup authrestart` — never a plain
     `reboot`. Auto-login is not possible with FileVault on (and not needed).
   - **OFF**: survives unplanned reboots unattended (enable auto-login), at the cost of an
     unencrypted disk.
5. **Homebrew** — install from [brew.sh](https://brew.sh), then:
   ```sh
   brew install tmux node      # node ≥ 22, for the web app
   brew install mosh           # optional, nicer on flaky mobile links
   ```
6. **PATH for non-login shells.** `ssh workstation 'tmux ...'` runs a non-interactive,
   non-login shell that does not read `~/.zprofile`, so Homebrew tools are "not found". Put the
   PATH in `~/.zshenv`, which every zsh reads:
   ```sh
   # ~/.zshenv
   eval "$(/opt/homebrew/bin/brew shellenv)"
   export PATH="$HOME/.local/bin:$PATH"
   ```
7. **Optional GUI fallback.** Enable Screen Sharing (works over Tailscale, after FileVault unlock)
   for the rare browser-only task.

## 2. Tailscale

Install Tailscale on every device (on macOS use the standalone download from tailscale.com, not the
App Store build, so it runs without an Apple ID) and log in to the same tailnet.

```sh
tailscale status          # lists peers and their 100.x.y.z addresses
tailscale ip -4           # this machine's address
```

MagicDNS gives each machine a name (`workstation.<your-tailnet>.ts.net`); IPs work too. `fleet
init` can read `tailscale status --json` to suggest hosts.

## 3. ssh

On the **laptop**:

```sh
ssh-keygen -t ed25519                 # if you don't have a key yet
ssh-copy-id me@100.x.y.z              # install it on the workstation
```

If `ssh-copy-id` fails with "Too many authentication failures" (the agent offers too many keys),
force password auth for that one call:

```sh
ssh-copy-id -o PubkeyAuthentication=no -o PreferredAuthentications=password \
  -i ~/.ssh/id_ed25519.pub me@100.x.y.z
```

Add a host alias in `~/.ssh/config` — `fleet` uses it as the host's `ssh` target:

```
Host workstation
  HostName 100.x.y.z            # or the MagicDNS name
  User me
  IdentityFile ~/.ssh/id_ed25519
  ServerAliveInterval 15
  ServerAliveCountMax 3
```

**Connection multiplexing.** `fleet` already multiplexes its own ssh calls (ControlMaster with a
short ControlPersist, disable with `FLEET_MUX=0`), so repeated commands skip the handshake. You do
not need to add ControlMaster to your ssh config for `fleet`; add it only if you want the same for
plain `ssh` too:

```
Host workstation
  ControlMaster auto
  ControlPath ~/.ssh/cm-%C
  ControlPersist 10m
```

Check: `ssh workstation 'tmux -V; claude --version'` — if either is "not found", revisit step 1.6.

## 4. tmux

Minimal `~/.tmux.conf` on the workstation:

```tmux
set -g mouse on                      # scroll + click panes, also on the phone
bind f switch-client -t fleet        # prefix f: jump back to the fleet dashboard
bind S switch-client -l              # prefix S: previous session
```

Conventions that make the fleet readable:

- **One tmux session = one job** (a task, a worktree), named after the job. `fleet new <name>`
  creates or attaches; `fleet rename` on a Claude session renames its tmux session to match.
- Keep a tmux session called `fleet` running `fleet watch` — the dashboard. Select a row and press
  Enter to jump into that session; `prefix f` brings you back.
- **The prefix is a sequence, not a chord**: press `Ctrl-b`, release, then the key. Holding Ctrl
  while pressing the letter sends a different key.
- Detach with `prefix d`, or just close the window — the session keeps running.
- Switch sessions: `prefix s` (tree), `prefix S` (previous), or `fleet last` / `fleet enter <query>`.

For native iTerm tabs/scrollback on the laptop, use tmux control mode directly:
`ssh workstation -t 'tmux -CC new -A -s <name>'`.

**Stuck mouse reporting.** With `mouse on`, an abruptly dropped connection (sleep, network change)
can leave mouse reporting enabled in your *local* terminal, which then types sequences like
`35;10;2M` into your prompt. Reset it with `printf '\e[?1000l\e[?1002l\e[?1003l\e[?1006l\e[?1004l'`
(or add that to a `precmd` hook in your local shell).

## 5. Claude Code

On the workstation:

```sh
curl -fsSL https://claude.ai/install.sh | bash     # native installer → ~/.local/bin/claude
```

Log in without a GUI: start `claude` inside tmux, run `/login`, open the printed OAuth URL in the
browser on your laptop, and paste the code back. (If the URL wraps across lines in tmux, join the
lines before opening it.)

Notes:

- The macOS login keychain is not available to ssh sessions ("User interaction is not allowed"),
  so tools that store secrets in the keychain need a file-based fallback on the workstation.
- After a reboot the tmux sessions and Claude processes are gone, but fleet remembers them as
  *dormant*: `fleet restore` lists them, `fleet restore <name>` (or `--all`) recreates each tmux
  session — windows, panes, cwds, layout — and resumes every Claude session in it with
  `claude --resume <id>` and its original flags; `fleet enter <name>` does it on the fly. The web
  UI lists them under the sessions (Resume / Forget / Resume all), and `"restore": { "onBoot": true }`
  in the workstation's config makes its web server resume them all once after each boot. Sessions
  you closed yourself are not dormant: `fleet restore --closed` (and the web UI's collapsed
  "Recently closed") brings one back for a week (`restore.keepClosedDays`). See
  [cli.md → Session recovery](cli.md#session-recovery).

## 6. fleet

On the laptop (from a checkout of this repo):

```sh
cargo install --path crates/fleet  # see README "Build prerequisites" if linking fails
fleet init                         # name this machine "laptop", add host "workstation" (ssh: workstation)
fleet install --host workstation   # copies the binary + web app; the workstation needs no Rust
fleet -H workstation init          # the same wizard on the workstation (self = "workstation")
                                   # or non-interactively: fleet init --yes --self … --add-host … (docs/cli.md)
fleet doctor
```

Both machines get their own `~/.config/fleet/config.json`; each names itself in `self` and lists the
others under `hosts`. `fleet install` checks that the architectures match.

The directories a new session can start in (`spawnDirs`: a label with a path per host) are edited
in the web app under **Settings → Start directories**: one row per label, a path field per host
(empty = not offered there). Save checks every host's own paths, then writes the same list to
each reachable host's config (through `fleet config set spawnDirs …`); the running servers use it
at once, no restart. `fleet config set spawnDirs '<json>'` on each host does the same by hand.
The eye button on a row hides it (`"hidden": true`): the directory stays allowed for spawns — e.g.
an app whose [embedded panel](architecture.md#embed-view) starts sessions in its own folder — but
is no longer offered in New session.
Editing this list is as powerful as spawning (it widens what spawn accepts), and like spawn it is
unauthenticated — keep the web servers on a private network.

Start the web app on each machine:

```sh
fleet web serve                    # foreground
fleet web install-service          # macOS: launchd agent; elsewhere: prints instructions
```

The launchd agent logs to `~/Library/Logs/fleet.web.log` and pins a stable node
(`/opt/homebrew/bin/node` or `/usr/local/bin/node`). If your only node comes from nvm/volta/fnm it
is used with a warning, since the path breaks on the next version switch — install a stable node
or pin one with `fleet config set web.node <path>`.

A detached tmux session is the most robust option on macOS: under a bare launchd agent every `ps`
call can be much slower (discovery runs one per session and may time out), and a tmux server
started from your terminal inherits its permissions (e.g. iTerm Automation, needed to drive iTerm
sessions):

```sh
tmux new -d -s fleet-web 'while true; do fleet web serve; sleep 2; done'
```

Neither launchd nor tmux survives a FileVault reboot without the unlock — restart the service
after one.

**Bots** (optional, on an always-on machine): add bots under `bots` in its config
([cli.md → Bots](cli.md#bots)), then install the ticker that starts them:

```sh
fleet bots install-service         # macOS: launchd agent `fleet.jobs`, every minute; logs to ~/Library/Logs/fleet.jobs.log
fleet bots                         # each bot, its schedule in plain English, next and last run
```

Bot sessions run unattended, so a bot usually sets `command` to a launcher that does not stop for
permission prompts, and its working directory must already be trusted by Claude Code (a first-run
"trust this folder" prompt would stall the session).

**Updating** a machine that runs from a repo checkout: `scripts/deploy.sh` there (over ssh:
`ssh workstation '~/Code/project/scripts/deploy.sh'`) — pulls `main` (refuses uncommitted
changes), builds the CLI into `~/.local/bin/fleet`, runs `fleet web build`, restarts the
`fleet.web` launchd agent (else the `fleet-web` tmux session) and waits for `/api/health`.

## 7. Phone

1. Install the **Tailscale** app, log in to the same tailnet, turn the VPN on.
2. Open `http://100.x.y.z:7777` (the workstation — it is always on) in Safari, then Share → **Add
   to Home Screen**. It runs full-screen as a PWA: session list, chat, a
   composer with quick replies, and "new session".
3. For a real terminal, use an ssh client such as **Termius**: generate a key in the app, append
   its public key to `~/.ssh/authorized_keys` on the workstation, host = `100.x.y.z`, enable Mosh
   if installed. Then `fleet last` / `fleet new main` for work and `fleet watch` for the dashboard.
   - Use the Ctrl key on the extra keyboard row for the tmux prefix.
   - Some mobile clients send Ctrl-J for Return; the dashboard accepts both, and digits `1`–`9`
     jump straight to a row.
   - Mosh needs UDP (ports 60000–61000) to reach the workstation; allow it in the macOS firewall
     or keep the firewall off and rely on Tailscale.

## Security

- **The web app has no authentication.** Anyone who reaches the port can read your sessions and
  type into them. Keep it on the tailnet: set `web.bind` to the machine's Tailscale IP (or keep
  `0.0.0.0` behind a firewall that only admits the tailnet), and never port-forward it or publish
  it with Tailscale Funnel.
- Use Tailscale ACLs if your tailnet has other users or devices you don't fully trust.
- ssh with keys only; consider `PasswordAuthentication no` on the workstation once keys work.
- The workstation holds your Claude credentials and whatever your agents can reach — treat it
  like your laptop: FileVault on, strong password, automatic security updates.
- `fleet send` / the web composer type into a live agent: an attacker with access is an attacker
  with your agent's permissions.

## Troubleshooting

| symptom | fix |
| --- | --- |
| `tmux: command not found` over ssh | PATH in `~/.zshenv`, not `~/.zprofile` (step 1.6) |
| `fleet: command not found` on the remote | `fleet install --host <name>`; ensure `~/.local/bin` is on the remote PATH |
| remote commands hang ~5 s then fail | host asleep/offline or Tailscale down; `fleet doctor`, `tailscale status` |
| web app shows a host as unreachable | its `fleet web serve` isn't running, or `hosts.<name>.web` is wrong |
| sessions missing in the web app under launchd | discovery is much slower under launchd on macOS; run the server in tmux instead |
| garbage like `35;10;2M` in the local prompt | stuck mouse reporting (section 4) |
