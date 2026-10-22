#!/bin/sh
# Regenerates the screens in this folder. Each one is what `tmux capture-pane -p -e` prints for
# a pane that drew one of the layouts below: the input box at the bottom of a session's screen,
# or a screen without one. The wording is synthetic; the layouts and text attributes are the
# harnesses' own:
#
# - Claude Code (2.1) draws its input box between two rules from column 0, `❯ ` before the
#   text. Its placeholder (`Try "…"`, "Press up to edit queued messages") and its prompt
#   suggestions are faint (SGR 2); the cursor it draws itself is reverse video (SGR 7), on the
#   placeholder's first character — or there is none (the terminal's own cursor, or the
#   terminal not focused), and the placeholder is faint throughout. With an IDE connected, the
#   box's first line starts with a pill naming its selection (`[⧉ 12 lines selected] `), in
#   the theme's `ide` colour (24-bit), not faint. Its themes draw in 24-bit colour
#   (`38;2;r;g;b`), whose components are never attributes: the `-truecolor` screens use 2, 7,
#   22 and 27, which read as SGR codes would turn faint or reverse video on or off.
# - Codex (0.16x) draws its composer as `› ` from column 0 on a background of its own, its
#   placeholder ("Ask Codex to do anything") faint, and its footer under it ("? for shortcuts",
#   "… context left"; newer ones "<model> <effort> · <n>% used"), also faint. Its cursor is
#   the terminal's. The `codex-status` screens paint the composer in 24-bit colour.
# - pi draws its editor between two rules from column 0, its cursor a reverse-video cell.
#
# crates/fleet/src/core/input.rs (unit tests) and crates/fleet/tests/send.rs (fake agents
# drawing these very captures) read them. live/ holds captures of real sessions instead (see
# input.rs): `capture-pane -p -e` of a 110x34 pane on a private tmux 3.6 server, Claude Code
# 2.1.303 and Codex 0.163.0, scrubbed (the working directory, usage figures and a plugin name
# replaced by neutral text of the same width, escape sequences untouched). This script does not
# make those, and leaves out the cases they cover. Run from the repo root: sh testdata/screens/gen.sh
# — it needs tmux, and starts (and kills) a server of its own, never the default one.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
dir=$(mktemp -d /tmp/fleet-screens.XXXXXX)
sock="fleet-screens-$$"
t() { env -u TMUX -u TMUX_PANE TMUX_TMPDIR="$dir" tmux -L "$sock" -f /dev/null "$@"; }
trap 't kill-server >/dev/null 2>&1 || true; rm -rf "$dir"' EXIT

E='\033['
DIM="${E}2m" UNDIM="${E}22m" REV="${E}7m" UNREV="${E}27m" BOLD="${E}1m" UNBOLD="${E}22m"
GRAY="${E}38;5;244m" FG="${E}39m" BG="${E}48;5;236m" NOBG="${E}49m"
# An OSC 8 hyperlink: LINK, its URI, ST, the text; LINK ST ends it.
LINK='\033]8;;' ST='\033\\'
RULE="${GRAY}$(printf '─%.0s' $(seq 1 80))${FG}"
# 24-bit colours: Claude Code's `ide` colour and rules; text, a suggestion and a Codex composer
# in colours whose components would be attributes if read as SGR codes (2 faint, 7 reverse,
# 22 not faint, 27 not reverse).
IDE="${E}38;2;71;130;200m" TRULE="${E}38;2;136;136;136m$(printf '─%.0s' $(seq 1 80))${FG}"
TEXT="${E}38;2;2;7;2m" SUGGEST="${E}38;2;22;27;22m" TBG="${E}48;2;7;2;27m"
# A Codex composer row: its text, then its background to the right edge.
row() { printf '%b%b%*s%b\n' "$BG" "$1" "$2" '' "$NOBG"; }
trow() { printf '%b%b%*s%b\n' "$TBG" "$1" "$2" '' "$NOBG"; }

# draw NAME FUNCTION: what FUNCTION prints, drawn in an 80x14 pane and captured to NAME.txt.
draw() {
  "$2" > "$dir/$1.raw"
  t new-session -d -s "$1" -x 80 -y 14 "cat '$dir/$1.raw'; exec sleep 600"
  i=0
  until t capture-pane -p -t "$1" | grep -q .; do
    i=$((i + 1))
    [ "$i" -lt 50 ] || { echo "gen.sh: $1 never drew" >&2; exit 1; }
    sleep 0.1
  done
  sleep 0.3
  t capture-pane -p -e -t "$1" > "$here/$1.txt"
  t kill-session -t "$1"
}

claude_footer() { printf '%b\n' "$RULE"; printf '  %b\n' "${DIM}? for shortcuts${UNDIM}"; }

# A new session's placeholder, the drawn cursor on its first character.
claude_placeholder() {
  printf '✻ Welcome to Claude Code!\n\n%b\n' "$RULE"
  printf '❯ %b\n' "${REV}T${UNREV}${DIM}ry \"write a test for src/parser.rs\"${UNDIM}"
  claude_footer
}
# A pasted image alone in the box: its `[Image #1]` tag is typed, like a paste marker.
claude_image() {
  printf '⏺ Fixed the flaky login test.\n\n%b\n' "$RULE"
  printf '❯ [Image #1] %b\n' "${REV} ${UNREV}"
  claude_footer
}
# The same tag wrapped in an OSC 8 link to the image, as Claude Code draws it at times: typed.
claude_image_link() {
  printf '⏺ Fixed the flaky login test.\n\n%b\n' "$RULE"
  printf '❯ %b %b\n' "${LINK}file:///tmp/image-1.png${ST}[Image #1]${LINK}${ST}" "${REV} ${UNREV}"
  claude_footer
}
# A prompt suggestion after a turn, faint throughout (the terminal's own cursor).
claude_suggestion() {
  printf '⏺ Fixed the flaky login test: the retry waits for the session cookie.\n\n%b\n' "$RULE"
  printf '❯ %b\n' "${DIM}run the full suite again${UNDIM}"
  claude_footer
}
claude_multiline() {
  printf '⏺ Fixed the flaky login test.\n\n%b\n' "$RULE"
  printf '❯ fix the flaky login test, then:\n'
  printf '  1. run the full suite\n'
  printf '  2. open a PR%b\n' "${REV} ${UNREV}"
  claude_footer
}
# A permission prompt in place of the box.
claude_permission() {
  printf '⏺ Removing the build output\n\n%b\n' "$RULE"
  printf ' Bash command\n\n   rm -rf build\n   Remove the build output\n\n'
  printf ' Do you want to proceed?\n'
  printf ' %b\n' "${E}36m❯ 1. Yes${FG}"
  printf '   2. No\n\n'
  printf ' %b\n' "${DIM}Esc to cancel · Tab to amend${UNDIM}"
}
# The transcript view (ctrl+o): no box.
claude_transcript() {
  printf '⏺ Bash(cargo test)\n  ⎿  test result: ok. 42 passed; 0 failed\n\n'
  printf '⏺ The suite passes: 42 tests, 0 failures.\n\n'
  printf '  %b\n' "${DIM}Showing detailed transcript · ctrl+o to toggle${UNDIM}"
}

# An IDE connected: its selection's pill, then the drawn cursor in an empty box.
claude_ide_pill() {
  printf '⏺ Fixed the flaky login test.\n\n%b\n' "$RULE"
  printf '❯ %b %b\n' "${IDE}[⧉ 12 lines selected]${FG}" "${REV} ${UNREV}"
  claude_footer
}
# The pill, then a restored prompt.
claude_ide_pill_restored() {
  printf '⏺ Fixed the flaky login test.\n\n%b\n' "$RULE"
  printf '❯ %b now run the full suite and open a PR%b\n' "${IDE}[⧉ In src/parser.rs]${FG}" "${REV} ${UNREV}"
  claude_footer
}
# A restored prompt in 24-bit colour (2;7;2): text, never faint or reverse video.
claude_truecolor() {
  printf '⏺ Fixed the flaky login test.\n\n%b\n' "$TRULE"
  printf '❯ %b%b\n' "${TEXT}now run the full suite and open a PR${FG}" "${REV} ${UNREV}"
  printf '%b\n' "$TRULE"
  printf '  %b\n' "${DIM}? for shortcuts${UNDIM}"
}
# A faint suggestion in 24-bit colour (22;27;22): still faint.
claude_truecolor_suggestion() {
  printf '⏺ Fixed the flaky login test.\n\n%b\n' "$TRULE"
  printf '❯ %b\n' "${DIM}${SUGGEST}run the full suite again${FG}${UNDIM}"
  printf '%b\n' "$TRULE"
  printf '  %b\n' "${DIM}? for shortcuts${UNDIM}"
}

codex_footer() { printf '\n  %b\n' "${DIM}$1${UNDIM}"; }
# A newer footer (model, effort, context used), the composer in 24-bit colour (7;2;27).
codex_status() {
  printf '• The suite passes: 42 tests, 0 failures.\n\n'
  trow '' 80
  trow "${BOLD}›${UNBOLD} ${DIM}Ask Codex to do anything${UNDIM}" 54
  trow '' 80
  codex_footer 'gpt-6 medium · 0% used'
}
codex_status_restored() {
  printf '› fix the flaky login test\n\n'
  printf '■ Conversation interrupted - tell the model what to do differently.\n\n'
  trow '' 80
  trow "${BOLD}›${UNBOLD} fix the flaky login test" 54
  trow '' 80
  codex_footer 'gpt-6 medium · 2% used'
}
codex_empty() {
  printf '• The suite passes: 42 tests, 0 failures.\n\n'
  row '' 80
  row "${BOLD}›${UNBOLD} ${DIM}Ask Codex to do anything${UNDIM}" 54
  row '' 80
  codex_footer '? for shortcuts                                         100% context left'
}
# A composer on no background of its own (as in the live captures), its footer drawn plain, as
# typed text is: fleet cannot tell the footer from words typed in the composer, so the box
# reads as unreadable, never empty.
codex_footer_plain() {
  printf '• The suite passes: 42 tests, 0 failures.\n\n'
  printf '%b\n' "${BOLD}›${UNBOLD} ${DIM}Ask Codex to do anything${UNDIM}"
  printf '\n  %s\n' '? for shortcuts                                         100% context left'
}
# The stopped prompt's words in the composer after an interrupted turn: text. (Codex 0.163.0
# does not put a stopped prompt back: live/codex-interrupted-empty.)
codex_restored() {
  printf '› fix the flaky login test\n\n'
  printf '■ Conversation interrupted - tell the model what to do differently.\n\n'
  row '' 80
  row "${BOLD}›${UNBOLD} fix the flaky login test" 54
  row '' 80
  codex_footer '98% context left'
}
codex_multiline() {
  printf '• Fixed the flaky login test.\n\n'
  row '' 80
  row "${BOLD}›${UNBOLD} fix the flaky login test" 54
  row '' 80
  row '  then run the full suite' 55
  row '' 80
  codex_footer '98% context left'
}
# A recalled image prompt, its text cleared: Codex (0.163.0, chat_composer.rs) draws the image
# as a non-editable cyan `[Image #1]` row above the textarea, one blank row between, inside the
# composer; the textarea under it shows the placeholder. The image goes out with the next prompt.
codex_image() {
  printf '• Described the screenshot.\n\n'
  row '' 80
  row "  ${E}36m[Image #1]${FG}" 68
  row '' 80
  row "${BOLD}›${UNBOLD} ${DIM}Ask Codex to do anything${UNDIM}" 54
  row '' 80
  codex_footer '? for shortcuts                                         100% context left'
}
# The transcript pager (ctrl+t): no composer.
codex_pager() {
  printf '/ T R A N S C R I P T / / / / / / / / / / / / / / / / / / / / / / / / / / / /\n'
  printf '› fix the flaky login test\n\n'
  printf '• Fixed the flaky login test.\n\n'
  printf '%b\n' "${GRAY}──────────────────────────────────────────────────────────────── 100% ─${FG}"
  printf '  %b\n' "${DIM}↑/↓ to scroll   pgup/pgdn to page   home/end to jump${UNDIM}"
  printf '  %b\n' "${DIM}q to quit   esc to edit prev${UNDIM}"
}

pi_status() {
  printf '%b\n' "$RULE"
  printf '%b\n' "${DIM}~/Code/project (main)${UNDIM}"
  printf '%b\n' "${DIM}↑1.2k ↓300 R12k \$0.012 3.1%/200k${UNDIM}"
}
pi_empty() {
  printf ' Fixed the flaky login test.\n\n%b\n' "$RULE"
  printf '%b\n' "${REV} ${UNREV}"
  pi_status
}
pi_text() {
  printf ' Fixed the flaky login test.\n\n%b\n' "$RULE"
  printf 'now run the full suite and open a PR%b\n' "${REV} ${UNREV}"
  pi_status
}
pi_multiline() {
  printf ' Fixed the flaky login test.\n\n%b\n' "$RULE"
  printf 'fix the flaky login test, then:\n- run the full suite\n- open a PR%b\n' "${REV} ${UNREV}"
  pi_status
}
# A selector in place of the editor.
pi_selector() {
  printf ' Fixed the flaky login test.\n\n'
  printf ' Select a model\n'
  printf ' %b\n' "${E}36m→ model-a${FG}"
  printf '   model-b\n'
  printf ' %b\n' "${DIM}↑↓ navigate · enter select · esc cancel${UNDIM}"
}

draw claude-placeholder claude_placeholder
draw claude-suggestion claude_suggestion
draw claude-multiline claude_multiline
draw claude-image claude_image
draw claude-image-link claude_image_link
draw claude-permission claude_permission
draw claude-transcript claude_transcript
draw claude-ide-pill claude_ide_pill
draw claude-ide-pill-restored claude_ide_pill_restored
draw claude-truecolor claude_truecolor
draw claude-truecolor-suggestion claude_truecolor_suggestion
draw codex-empty codex_empty
draw codex-footer-plain codex_footer_plain
draw codex-status codex_status
draw codex-status-restored codex_status_restored
draw codex-restored codex_restored
draw codex-multiline codex_multiline
draw codex-image codex_image
draw codex-pager codex_pager
draw pi-empty pi_empty
draw pi-text pi_text
draw pi-multiline pi_multiline
draw pi-selector pi_selector
