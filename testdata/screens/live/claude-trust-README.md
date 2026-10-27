# Folder-trust captures

`claude-trust-*` are **real captures of Claude Code 2.1.311's folder-trust dialog**, recorded on
2026-10-27 in a tmux pane on macOS. Claude Code ran with its own `CLAUDE_CONFIG_DIR`
(`command claude --model haiku`, typed by hand at a zsh prompt, not by `spawn`), because `/` is
trusted in the recording machine's own config, which would have skipped the dialog.

- `*.ans`: `tmux capture-pane -p -e` of the visible screen (SGR included; "Security guide" is an
  OSC 8 link). Taken **without `-J`**, which `fleet trust` passes: in `plain-80x24*` the shell
  wrapped the launch line over rows 0 and 1, and the tests join those two rows as `-J` would.
- `*.cursor`: the terminal's cursor at the same moment, `cursor_x,cursor_y flag` (0-based); the
  flag is 0 while the dialog is up and 1 once the shell has the cursor. Fleet doesn't read it.

| capture | screen |
|---|---|
| `plain-80x24`, `plain-200x50` | a new, empty folder: the dialog untouched, "No, exit" preselected |
| `permissions-80x24`, `permissions-200x50` | a folder whose `.claude/settings.json` pre-approves 12 permissions: the warning lists 8 and "and 4 more", wrapped (mid-entry at 80 columns) |
| `*-down`, `*-up` | after one Down (the cursor on "Yes, I trust this folder"), then one Up (back) |
| `*-after-esc` | after Esc: Claude Code exited and left the dialog's text on screen (no alternate screen), the shell's prompt and the terminal's cursor under its footer |
| `permissions-200x50-resume`, `-resume-after-esc` | the same with `--resume <id>`: the dialog comes first |

What they show: a full-width `─` rule (38;5;220) on top, then "Accessing workspace:" (bold,
38;5;220) alone, the folder on the next line; the answers unnumbered, `❯` and the selected
label in 38;5;153, the other indented three spaces; the footer "Enter to confirm · Esc to
cancel" (38;5;246); only blank rows under it while the dialog is up, and the terminal's cursor,
hidden, on the selected answer's `❯` (column 1). The recogniser reads the text, not the colours
(`read()` strips SGR): the rule, the heading, the two answer labels with the `❯` on one, the
footer, the blank rows under it, and the cursor's cell on that `❯`. In `permissions-80x24*` the
dialog filled the screen and the launch line scrolled away.

`permissions-200x50-after-accept-{0.5,2,5}s` (a separate config dir, so only that copy trusted
the folder): after Down and Enter, 0.5, 2 and 5 s later the dialog's text is gone and Claude
Code's own screen is up (logo, "Auto mode" notice, composer, footer); the process stays
foreground, the cursor is shown. A fleet that sees this has no prompt: `no_prompt`, not stale.
Long tokens (a folder path longer than the pane) are cut by Ink mid-token, so such a dialog's
`id` can still differ between widths.

The paths are the recording's (`~/.trustcap-scratch/{plain,perms}`), the permission rules those
of a real project's settings. The unit tests derive other screens from these (reworded,
reversed or numbered answers, framed, clipped, a conversation, a composer, Codex).
