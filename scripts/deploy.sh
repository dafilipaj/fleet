#!/bin/sh
# Deploy `main` on this machine from its repo checkout: pull, install the CLI, build the web UI,
# restart the web server, wait until it answers.
#
#   scripts/deploy.sh                        # on the machine itself
#   ssh <host> '~/Code/project/scripts/deploy.sh'   # another host, from its own checkout
#
# Refuses a checkout with uncommitted changes to tracked files (that is someone's work in
# progress — deploy what is pushed, not what is lying around). The binary goes to
# ~/.local/bin/fleet (where `fleet install` and the launchd agent expect it), replaced
# atomically. Restart: the `fleet.web` launchd agent (`fleet web install-service`), else the
# `fleet-web` systemd user unit (what `fleet web install-service` prints on Linux), else a tmux
# session `fleet-web`, else it only tells you to restart the server yourself. A Claude Code skill
# installed earlier (`fleet skill install`, ~/.claude/skills/fleet/SKILL.md) is refreshed to the new
# binary's; a machine without one is left without one.
set -eu

export PATH="$HOME/.local/bin:$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"
cd "$(dirname "$0")/.."
repo=$(pwd)
say() { printf '\033[1mdeploy:\033[0m %s\n' "$*"; }

if [ -n "$(git status --porcelain --untracked-files=no)" ]; then
  echo "deploy: uncommitted changes in $repo — refusing (commit, stash or deploy from a clean checkout)" >&2
  exit 1
fi
branch=$(git rev-parse --abbrev-ref HEAD)
[ "$branch" = main ] || { echo "deploy: $repo is on '$branch', not main" >&2; exit 1; }

say "pull"
git pull --ff-only --quiet
say "at $(git log --oneline -1)"

say "build the CLI"
cargo build --release --locked -q -p fleet
mkdir -p "$HOME/.local/bin"
# Copy then rename: overwriting a running binary in place can get it killed on macOS.
cp target/release/fleet "$HOME/.local/bin/.fleet.new"
mv -f "$HOME/.local/bin/.fleet.new" "$HOME/.local/bin/fleet"
say "installed $("$HOME/.local/bin/fleet" --version)"

skill="$HOME/.claude/skills/fleet/SKILL.md"
if [ -f "$skill" ] && ! "$HOME/.local/bin/fleet" skill show | cmp -s - "$skill"; then
  "$HOME/.local/bin/fleet" --local skill install --force >/dev/null
  say "refreshed the fleet skill"
fi

say "build the web UI"
"$HOME/.local/bin/fleet" web build --dir "$repo/web" --install >/dev/null

port=$("$HOME/.local/bin/fleet" --local config get web.port 2>/dev/null || true)
case "$port" in '' | *[!0-9]*) port=7777 ;; esac

uid=$(id -u)
if launchctl print "gui/$uid/fleet.web" >/dev/null 2>&1; then
  say "restart (launchd fleet.web)"
  launchctl kickstart -k "gui/$uid/fleet.web"
elif command -v systemctl >/dev/null 2>&1 && systemctl --user is-enabled fleet-web >/dev/null 2>&1; then
  say "restart (systemd --user fleet-web)"
  systemctl --user restart fleet-web
elif tmux has-session -t fleet-web 2>/dev/null; then
  say "restart (tmux fleet-web)"
  tmux respawn-pane -k -t fleet-web "while true; do '$HOME/.local/bin/fleet' web serve --dir '$repo/web'; sleep 2; done"
else
  say "no fleet.web launchd agent, fleet-web systemd user unit or fleet-web tmux session — restart the web server yourself"
  exit 0
fi

i=0
until curl -fsS "http://127.0.0.1:$port/api/health" >/dev/null 2>&1; do
  i=$((i + 1))
  if [ "$i" -ge 30 ]; then
    echo "deploy: the web server did not answer on :$port within 30s" >&2
    exit 1
  fi
  sleep 1
done
say "web server up on :$port"
