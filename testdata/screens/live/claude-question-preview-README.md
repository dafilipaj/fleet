# Question-with-previews captures

`claude-question-preview-*` are **real captures of Claude Code 2.1.312's `AskUserQuestion`
form**, recorded on 2026-10-28 in tmux panes on macOS (200x50 unless the name says 80x24), with
Claude Code asked to call the tool with an exact input.

- `*.ans`: `tmux capture-pane -p -e` of the visible screen (SGR included). `fleet answer`
  captures without `-e`; the tests strip the SGR.
- `*.cursor`: the terminal's cursor at the same moment, `cursor_x,cursor_y flag`. Fleet doesn't
  read it.
- `*-input.json`: the tool call's `input`, copied from the session's transcript.
- Nothing private: every line above a form's top rule (the rule over its tab bar) is blank,
  keeping the screen's row count; on an `after` screen (no form) every line holding a path, a
  user or host name, a hook message or a plugin name is blank. `*.cursor` files are as
  recorded.

| capture | screen |
|---|---|
| `A-initial` | one question, three options, all with previews: the preview layout, untouched |
| `A-down1` … `A-down4` | after 1 to 4 Down: options 2 and 3, then "Chat about this" (`❯` on it, and still on option 3) |
| `A-notes-open`, `A-notes-typed` | after `n` (the notes field open), after typing "my note here" |
| `A-notes-enter`, `A-after` | after Enter in the notes field (it submits the one-question form), after the answer |
| `B-initial-80x24`, `B-down1-80x24`, `B-after-esc` | A's input at 80x24: the box cut (`✂ … lines hidden`), option 3 under it; after Esc |
| `C-initial`, `C-q2`, `C-review`, `C-after` | two questions, the first with previews, the second plain; the review (no gutter) |
| `D-initial`, `D-down1` | only option 1 has a preview; on option 2 the box says "No preview available" |
| `E-initial`, `E-down1` | multi-select with previews: drawn plainly |
| `F-initial` | option 1's preview 45 lines: the box cut at 200x50 |
| `G-*` | A's input after Tab/Right, then Down to "Chat about this" |
| `N1-*` | four questions of the live form's shape (neutral text): question 1 with previews and a wrapped label, 2 to 4 plain; each question, the review (` │ ● …` items) and `N1-after`; `N1-transcript.jsonl` is the session's transcript (its working directory renamed) |
| `P1-q1`, `P1-q2`, `P1-q2-down1`, `P1-review` | two questions, 1 plain, 2 with previews: question 2 after an answered tab (`☒ Fruit  ☐ Layout`), the cursor on option 2, the review |
| `P2-q1`, `P2-q2`, `P2-review` | the same shape at 80x24: the several-question footer wraps (`… · Tab to switch questions · ` / `Esc to cancel`) |
| `P3-initial`, `P3-notes-open`, `P3-notes-typed`, `P3-after-esc` | one question at 80x24: the footer wraps; the notes field open (`… · ctrl+g to edit in Vim · Esc` / `to cancel`), a note typed, after the first Esc (closes only the field, the note `a note` stays) |
| `H2-*`, `H3-*`, `H4-*` | plain questions of 60 to 230 characters: the `│ ` gutter over 80 |

Inputs: `A`, `C`, `H2` to `H4`, `N1`, `P1` to `P3` from their recordings. B and G asked A's
input; D, E and F are derived from A's in the tests, as their prompts on screen show them. H2's recording asked
the first three questions of `H2-input.json` (its tab bar shows L60, L90, L120), not L190; the
tests drop the fourth. `H2-q4` is H2's review.

Edits: only the blanking above. The first live form (a work question) and its first copy, H1,
are not here: N1 replaces them.

What they show: the box's left edge at column 34 at both sizes; option labels wrapped at four
spaces; the notes row at the box's column; "Chat about this" unnumbered under a solid rule; the
footer "Enter to select · ↑/↓ to navigate · n to add notes · [Tab to switch questions · ]Esc to
cancel" (`ctrl+g to edit in Vim` added while the notes field is open). At 80 columns the longer footers wrap
(P2, P3). A question longer than 80
characters carries a `│ ` gutter on every line, on the form and on the review (` │ ● …`,
` │   …`), whatever its options.
