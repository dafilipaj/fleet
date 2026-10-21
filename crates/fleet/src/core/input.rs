//! The input box at the bottom of a session's screen — Claude Code's prompt, Codex's composer,
//! pi's editor — read off a capture that keeps its text attributes (`tmux capture-pane -e`),
//! so that `fleet send` knows whether it types into an empty one.
//!
//! Why: Escape interrupting a turn before Claude Code has written anything puts the
//! interrupted prompt back into its input box, and text typed after it goes out with it as one
//! prompt — the very prompt the person stopped.
//!
//! **Typed is what is not faint.** The harnesses draw what they put in an empty box faint
//! (SGR 2): Claude Code its placeholder (`Try "…"`, "Press up to edit queued messages") and
//! its prompt suggestions, Codex its placeholder ("Ask Codex to do anything"). Typed text — a
//! restored prompt, a paste marker, an image tag — is never faint. Two exceptions, both
//! Claude Code's:
//!
//! - The cursor it draws itself is reverse video, and it sits on the placeholder's first
//!   character, which is then not faint; such a cell counts as the placeholder when it is the
//!   box's first character and faint text follows it.
//! - With an IDE connected, the box's first line starts with a pill naming the IDE's selection
//!   (`[⧉ 12 lines selected] `, `[⧉ In main.rs] `), in a colour of its own and not faint. A
//!   leading `[…]` drawn in one colour, not the terminal's default and not the colour of what
//!   follows it, is that pill and left out — unless it is a paste or image marker (`[Pasted
//!   text #1 …]`, `[Image #1]`, `[...Truncated text #1 …]`), which is typed.
//!
//! **Where the box is.** Claude Code: between the last two rules drawn from column 0, its first
//! line starting with `❯ ` (`!` in bash mode — text, since a send there would run as a shell
//! command; 2.1.30x puts a no-break space after the marker), every line under it indented. Codex: the last line starting with `› ` at column
//! 0, and the rows under it drawn on that line's background (the composer's own), every one
//! indented, the first row after them indented too (the footer, or a popup); on a screen
//! where that line has no background, down to its footer: the indented rows at the very
//! bottom, a blank row above them, one drawn faint, bold or in a colour, one saying what a
//! footer fleet knows says (`? for shortcuts`, `… context left`, `… % used`, …). Where that
//! footer cannot be told from the composer's own rows, the box is unreadable, never empty. The
//! image attachments Codex draws above the `›` line (`[Image #1]`, a blank row between) are in
//! the box: typed. pi: between the last two rules from column 0. A permission
//! prompt or question on screen is told apart first: the box is not there then. Anything else
//! is no box at all.

use crate::core::backend;
use crate::core::discovery::Session;
use crate::core::harness::Harness;
use crate::error::Result;
use unicode_width::UnicodeWidthChar;

/// What the screen says about the session's input box.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputRead {
    /// The box is on screen, and nothing is typed in it (a placeholder or suggestion aside).
    Empty,
    /// The box holds this text (its lines, faint text left out).
    Typed(String),
    /// The box shows this text, but the screen came without text attributes (iTerm), so a
    /// placeholder or suggestion cannot be told from typed text.
    Unclear(String),
    /// A permission prompt or a question waits for its answer where the box was.
    Prompt,
    /// No input box fleet knows is on screen: a menu, the transcript view, a layout it does
    /// not know.
    NoBox,
}

impl InputRead {
    /// The machine-readable reason a send is held, or typed with a warning; `None` when the
    /// box is empty.
    pub fn reason(&self) -> Option<&'static str> {
        match self {
            InputRead::Empty => None,
            InputRead::Typed(_) => Some("text"),
            InputRead::Unclear(_) => Some("unreadable"),
            InputRead::Prompt => Some("prompt"),
            InputRead::NoBox => Some("no_input_box"),
        }
    }

    /// Does this read hold what was about to be typed? Text in the box, or a prompt in its
    /// place, always does: typing would do harm there. No box found, or one that cannot be
    /// read, holds only when the caller asks to see an empty box (`require_empty`); otherwise
    /// the text is typed, as before fleet read the box, with a [`InputRead::warning`].
    pub fn holds(&self, require_empty: bool) -> bool {
        match self {
            InputRead::Empty => false,
            InputRead::Typed(_) | InputRead::Prompt => true,
            InputRead::Unclear(_) | InputRead::NoBox => require_empty,
        }
    }

    /// The text the box shows, when it shows any. It is whatever the person typed or pasted
    /// there, so no message carries it: only `send --json`'s `input` does.
    pub fn text(&self) -> Option<&str> {
        match self {
            InputRead::Typed(t) | InputRead::Unclear(t) => Some(t),
            _ => None,
        }
    }

    /// Why nothing was sent, for people — never with the box's text.
    pub fn message(&self) -> &'static str {
        match self {
            InputRead::Empty => "the input box is empty",
            InputRead::Typed(_) => {
                "the input box already holds text — the text would go out with it as one prompt; nothing sent (clear it first, or --force to send anyway)"
            }
            InputRead::Unclear(_) => {
                "the input box shows text, and this terminal gives no text attributes to tell a placeholder or suggestion from typed text; nothing sent (--force to send anyway)"
            }
            InputRead::Prompt => {
                "the terminal shows a prompt waiting for its answer, not the input box; nothing sent (answer it with `fleet answer`, or press Escape)"
            }
            InputRead::NoBox => {
                "fleet cannot find the input box on screen (a menu, the transcript view, or a layout it does not know), so it cannot tell it is empty; nothing sent (--force to send anyway)"
            }
        }
    }

    /// What a send typed anyway (the read did not [`hold`](InputRead::holds) it) could not
    /// know, for people: one line.
    pub fn warning(&self) -> Option<&'static str> {
        match self {
            InputRead::Unclear(_) => Some(
                "typed without knowing the input box was empty: it shows text, and this terminal gives no text attributes to tell a placeholder from typed text (--require-empty holds instead)",
            ),
            InputRead::NoBox => Some(
                "typed without knowing the input box was empty: fleet cannot find it on screen (a menu, the transcript view, or a layout it does not know) (--require-empty holds instead)",
            ),
            _ => None,
        }
    }
}

/// A colour a cell is drawn in: the terminal's own, one of its palette, or 24-bit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Colour {
    #[default]
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

/// The text attributes in force at a point of the capture.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Style {
    faint: bool,
    bold: bool,
    reverse: bool,
    fg: Colour,
    bg: Colour,
}

/// One character on screen and how it is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Cell {
    ch: char,
    faint: bool,
    bold: bool,
    reverse: bool,
    /// The text's colour.
    fg: Colour,
    /// The colour behind it.
    bg: Colour,
}

impl Cell {
    fn new(ch: char, st: Style) -> Self {
        Cell {
            ch,
            faint: st.faint,
            bold: st.bold,
            reverse: st.reverse,
            fg: st.fg,
            bg: st.bg,
        }
    }
}

/// The screen's lines as cells. Escape sequences are read for their text attributes when
/// `styled` (a tmux capture carries them from one line to the next), dropped otherwise.
fn rows(screen: &str, styled: bool) -> Vec<Vec<Cell>> {
    let mut rows = vec![Vec::new()];
    let mut st = Style::default();
    let mut it = screen.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\n' => rows.push(Vec::new()),
            '\x1b' => match it.next() {
                Some('[') => {
                    let mut params = String::new();
                    let mut last = None;
                    for c in it.by_ref() {
                        if ('\x40'..='\x7e').contains(&c) {
                            last = Some(c);
                            break;
                        }
                        params.push(c);
                    }
                    if styled && last == Some('m') {
                        sgr(&params, &mut st);
                    }
                }
                Some(']') => {
                    while let Some(c) = it.next() {
                        if c == '\x07' || (c == '\x1b' && it.next_if_eq(&'\\').is_some()) {
                            break;
                        }
                    }
                }
                Some(c) if ('\x20'..='\x2f').contains(&c) => {
                    for c in it.by_ref() {
                        if !('\x20'..='\x2f').contains(&c) {
                            break;
                        }
                    }
                }
                _ => {}
            },
            '\t' => rows.last_mut().unwrap().push(Cell::new(' ', st)),
            c if c.is_control() => {}
            ch => rows.last_mut().unwrap().push(Cell::new(ch, st)),
        }
    }
    rows
}

/// Apply one SGR sequence's parameters to the attributes read so far.
fn sgr(params: &str, st: &mut Style) {
    let mut ps = params.split(';');
    while let Some(p) = ps.next() {
        // A parameter with sub-parameters (`4:3`, `38:2::r:g:b`) is one parameter.
        let mut sub = p.split(':');
        let code: u32 = sub.next().unwrap_or("").parse().unwrap_or(0);
        match code {
            0 => *st = Style::default(),
            1 => st.bold = true,
            2 => st.faint = true,
            22 => {
                st.faint = false;
                st.bold = false;
            }
            7 => st.reverse = true,
            27 => st.reverse = false,
            30..=37 => st.fg = Colour::Indexed((code - 30) as u8),
            90..=97 => st.fg = Colour::Indexed((code - 90 + 8) as u8),
            39 => st.fg = Colour::Default,
            40..=47 => st.bg = Colour::Indexed((code - 40) as u8),
            100..=107 => st.bg = Colour::Indexed((code - 100 + 8) as u8),
            49 => st.bg = Colour::Default,
            // An extended colour, whose own parameters are numbers, never attributes: `38;5;n`,
            // `38;2;r;g;b`, or as sub-parameters `38:5:n`, `38:2::r:g:b`, `38:2:r:g:b`.
            38 | 48 | 58 => {
                let colour = if p.contains(':') {
                    extended(&sub.collect::<Vec<_>>())
                } else {
                    match ps.next() {
                        Some("5") => ps.next().and_then(byte).map(Colour::Indexed),
                        Some("2") => {
                            let (r, g, b) = (ps.next(), ps.next(), ps.next());
                            rgb(r, g, b)
                        }
                        _ => None,
                    }
                };
                match code {
                    38 => st.fg = colour.unwrap_or_default(),
                    48 => st.bg = colour.unwrap_or_default(),
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

/// An extended colour's sub-parameters, after its `38`: `5:n`, `2::r:g:b` (a colour space id,
/// usually empty, before the components) or `2:r:g:b`.
fn extended(sub: &[&str]) -> Option<Colour> {
    match sub {
        ["5", n, ..] => byte(n).map(Colour::Indexed),
        ["2", _, r, g, b, ..] => rgb(Some(r), Some(g), Some(b)),
        ["2", r, g, b] => rgb(Some(r), Some(g), Some(b)),
        _ => None,
    }
}

fn byte(s: &str) -> Option<u8> {
    s.parse().ok()
}

fn rgb(r: Option<&str>, g: Option<&str>, b: Option<&str>) -> Option<Colour> {
    Some(Colour::Rgb(byte(r?)?, byte(g?)?, byte(b?)?))
}

fn text_of(row: &[Cell]) -> String {
    row.iter()
        .map(|c| c.ch)
        .collect::<String>()
        .trim_end()
        .to_string()
}

/// A rule the box is drawn with: from column 0, at least 8 of `─`.
fn is_box_rule(line: &str) -> bool {
    line.chars().take_while(|c| matches!(c, '─' | '━')).count() >= 8
}

/// `line` starts with `marker` at column 0, then a space (Claude Code 2.1.30x draws a no-break
/// space) or nothing.
fn starts_with_marker(line: &str, marker: char) -> bool {
    let mut cs = line.chars();
    cs.next() == Some(marker) && matches!(cs.next(), None | Some(' ' | '\u{a0}'))
}

/// Indented, or blank: a line inside a box, never one of its own at column 0.
fn inside(line: &str) -> bool {
    line.is_empty() || line.starts_with(char::is_whitespace)
}

/// Text in the footer under Codex's composer: what tells, on a screen that gives no background
/// to find the composer by, that the rows at the bottom are that footer. Looked for there alone
/// ([`codex_footer`]), never in the rows of the composer, where it may be typed.
const CODEX_FOOTER: [&str; 7] = [
    "? for shortcuts",
    "context left",
    "% used",
    "⏎ send",
    "to edit previous message",
    "again to quit",
    "to queue message",
];

/// Where the box is on screen.
struct Found {
    /// Each of its lines: the row, and the column its text starts at (after the marker).
    lines: Vec<(usize, usize)>,
    /// Claude Code's bash mode (`!`): what is typed runs as a shell command.
    bash: bool,
    /// Rows under the box that may be part of it or not (Codex's footer, when its boundary
    /// cannot be told): the box is never read as empty while there are any.
    doubt: Vec<usize>,
}

/// The box of `harness`'s layout on screen, or `None` when the screen shows none.
fn locate(rows: &[Vec<Cell>], texts: &[String], harness: Harness) -> Option<Found> {
    // The marker's line, from column 2; the lines under it whole.
    let found = |first: usize, end: usize, bash: bool| Found {
        lines: std::iter::once((first, 2))
            .chain((first + 1..end).map(|i| (i, 0)))
            .collect(),
        bash,
        doubt: Vec::new(),
    };
    match harness {
        Harness::Claude | Harness::Pi => {
            let rules: Vec<usize> = (0..texts.len())
                .filter(|&i| is_box_rule(&texts[i]))
                .collect();
            let [.., top, bottom] = rules[..] else {
                return None;
            };
            if bottom <= top + 1 {
                return None;
            }
            if harness == Harness::Pi {
                return Some(Found {
                    lines: (top + 1..bottom).map(|i| (i, 0)).collect(),
                    bash: false,
                    doubt: Vec::new(),
                });
            }
            let marker = ['❯', '>', '!']
                .into_iter()
                .find(|m| starts_with_marker(&texts[top + 1], *m))?;
            texts[top + 2..bottom]
                .iter()
                .all(|l| inside(l))
                .then(|| found(top + 1, bottom, marker == '!'))
        }
        Harness::Codex => {
            let at = texts.iter().rposition(|l| starts_with_marker(l, '›'))?;
            let (end, doubt) = match codex_composer_end(rows, texts, at) {
                Some(end) => (end, Vec::new()),
                None => codex_footer(rows, texts, at)?,
            };
            if !texts[at + 1..end].iter().all(|l| inside(l)) {
                return None;
            }
            let mut box_ = found(at, end, false);
            // Image attachments above the textarea are in the box: they go out with the text.
            box_.lines
                .splice(0..0, codex_images(texts, at).map(|i| (i, 0)));
            box_.doubt = doubt;
            Some(box_)
        }
    }
}

/// Where Codex's composer, its first line (`› …`) row `at`, ends: after the last row under it
/// drawn on that line's background — the composer's own, painted across its width — as long as
/// the first row past them that shows anything is indented (its footer, or a popup) or there is
/// none. `None` when that line has no background of its own (or the screen came without
/// attributes), or something at column 0 follows: no composer, then (the transcript pager
/// shows past prompts that way).
///
/// A row that shows nothing tells no background: tmux leaves out a line's trailing blanks,
/// colour and all, so the composer's padding rows and the blank rows past it read the same.
/// They are passed over, and the composer ends at its last row that shows anything.
fn codex_composer_end(rows: &[Vec<Cell>], texts: &[String], at: usize) -> Option<usize> {
    let bg = rows[at].first()?.bg;
    if bg == Colour::Default {
        return None;
    }
    let mut end = at + 1;
    for (i, row) in rows.iter().enumerate().skip(at + 1) {
        match row.first() {
            None => {}
            Some(c) if c.bg == bg => end = i + 1,
            Some(_) => return inside(&texts[i]).then_some(end),
        }
    }
    Some(end)
}

/// Where Codex's composer, its first line (`› …`) row `at`, ends on a screen that gives it no
/// background (the live captures have none): above its footer, the last rows on screen that
/// show anything. The footer is told by where it is and how it is drawn, never by its words
/// alone, which can be typed: the run of rows that show something at the very bottom, indented,
/// one of them saying what a footer fleet knows says ([`CODEX_FOOTER`]) — else `None`, no
/// composer — with a blank row above it (Codex leaves one between the composer and its footer)
/// and a row of it drawn as typed text never is: faint, bold or in a colour. One is enough:
/// Codex draws its status row plain without colours (`NO_COLOR`, or its status line's colours
/// off on a terminal that gives no default colours), and a flash ("Copied selection") plain in
/// its place, its key hints (`?`, `esc`) bold all the same. A draft's rows are not in that run
/// under a footer (the blank row parts them), and a draft alone at the bottom has no such row.
///
/// The composer ends above that run. When the run is not sure to be the footer (no blank row
/// above it, no row of it drawn as one, or a screen without attributes) its rows are returned
/// too, as rows that may be the composer's: the box is then never read as empty.
fn codex_footer(rows: &[Vec<Cell>], texts: &[String], at: usize) -> Option<(usize, Vec<usize>)> {
    let blank = |i: usize| texts[i].trim().is_empty();
    let last = (at + 1..texts.len()).rev().find(|&i| !blank(i))?;
    let mut start = last;
    while start > at + 1 && !blank(start - 1) {
        start -= 1;
    }
    let run = start..=last;
    if !run.clone().all(|i| inside(&texts[i]))
        || !run
            .clone()
            .any(|i| CODEX_FOOTER.iter().any(|m| texts[i].contains(m)))
    {
        return None;
    }
    let drawn_as_footer = |row: &[Cell]| {
        row.iter()
            .any(|c| !c.ch.is_whitespace() && (c.faint || c.bold || c.fg != Colour::Default))
    };
    let sure = start > at + 1 && run.clone().any(|i| drawn_as_footer(&rows[i]));
    Some((start, if sure { Vec::new() } else { run.collect() }))
}

/// The rows of Codex's image attachments above its textarea, its first line row `at`: each
/// `[Image #N]`, indented, the last one a blank row (or none) above that line. Codex 0.163.0
/// draws an image recalled from history so, with the text cleared or not; it goes out with
/// the next prompt.
fn codex_images(texts: &[String], at: usize) -> impl Iterator<Item = usize> {
    let image = |l: &str| {
        l.starts_with(char::is_whitespace)
            && l.trim()
                .strip_prefix("[Image #")
                .and_then(|r| r.strip_suffix(']'))
                .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
    };
    let mut top = at;
    if top > 0 && texts[top - 1].trim().is_empty() {
        top -= 1;
    }
    let mut first = top;
    while first > 0 && image(&texts[first - 1]) {
        first -= 1;
    }
    first..top
}

/// How many cells at the start of `line` — the first line of Claude Code's box, from after its
/// marker — its IDE pill takes, with the space after it; 0 when it shows none. The pill
/// (`[⧉ 12 lines selected] `) is a leading `[…]` all in one colour, not the default, not faint
/// and not the colour of the text after it; a paste or image marker is not one.
fn ide_pill(line: &[Cell]) -> usize {
    let Some(open) = line.iter().position(|c| !c.ch.is_whitespace()) else {
        return 0;
    };
    let first = line[open];
    if first.ch != '[' || first.faint || first.fg == Colour::Default {
        return 0;
    }
    let Some(close) = line[open..]
        .iter()
        .position(|c| c.ch == ']')
        .map(|i| open + i)
    else {
        return 0;
    };
    if !line[open..=close]
        .iter()
        .all(|c| c.fg == first.fg && !c.faint)
    {
        return 0;
    }
    let inner: String = line[open + 1..close].iter().map(|c| c.ch).collect();
    if ["Pasted ", "Image #", "...Truncated "]
        .iter()
        .any(|m| inner.starts_with(m))
    {
        return 0;
    }
    let end = match line.get(close + 1) {
        None => close + 1,
        Some(c) if c.ch == ' ' => close + 2,
        Some(_) => return 0,
    };
    // Text after it in its very colour: a `[…]` typed in that colour, not the pill.
    match line[end..].iter().find(|c| !c.ch.is_whitespace()) {
        Some(c) if !c.faint && c.fg == first.fg => 0,
        _ => end,
    }
}

/// What `s`'s input box shows now, read off its visible screen with the text's attributes
/// ([`backend::peek_styled`]). A screen that cannot be read is an error, never an empty box.
pub fn of(s: &Session) -> Result<InputRead> {
    let (screen, styled) = backend::peek_styled(s)?;
    Ok(read_at(&screen, s.harness, styled, backend::cursor(s)))
}

/// What the input box of a `harness` session shows on `screen`: a capture of its visible
/// screen, with its escape sequences when `styled` (tmux), as plain text otherwise (iTerm).
pub fn read(screen: &str, harness: Harness, styled: bool) -> InputRead {
    read_at(screen, harness, styled, None)
}

/// [`read`], with where the terminal's cursor is on that screen (`(column, row)`), when it
/// shows. Claude Code draws nothing for blanks typed into its box: a box holding only spaces
/// or line breaks looks empty, and text typed after them would go out behind them (` /model
/// opus`: a prompt, not the command). The cursor tells: in an empty box it sits where the
/// text starts; past that, blanks are typed.
pub fn read_at(
    screen: &str,
    harness: Harness,
    styled: bool,
    cursor: Option<(usize, usize)>,
) -> InputRead {
    let rows = rows(screen, styled);
    let texts: Vec<String> = rows.iter().map(|r| text_of(r)).collect();
    let plain = texts.join("\n");
    if crate::core::title::shows_prompt(&plain, harness)
        || crate::core::prompt::parse_review(&plain).is_some()
    {
        return InputRead::Prompt;
    }
    let Some(found) = locate(&rows, &texts, harness) else {
        return InputRead::NoBox;
    };
    let mut cells: Vec<&[Cell]> = found
        .lines
        .iter()
        .map(|&(i, from)| rows[i].get(from..).unwrap_or_default())
        .collect();
    let mut pill = 0;
    if harness == Harness::Claude
        && !found.bash
        && let Some(first) = cells.first_mut()
    {
        let line: &[Cell] = first;
        let chars = ide_pill(line);
        // The cursor counts terminal cells, a wide character two: measure the pill so.
        pill = line[..chars]
            .iter()
            .map(|c| c.ch.width().unwrap_or(0))
            .sum();
        *first = &line[chars..];
    }
    let verdict = judge(&cells, found.bash, styled);
    if verdict == InputRead::Empty
        && harness == Harness::Claude
        && !found.bash
        && let Some(blanks) = blanks_typed(&found, pill, cursor)
    {
        return InputRead::Typed(blanks);
    }
    if verdict == InputRead::Empty && !found.doubt.is_empty() {
        // Where the box ends cannot be told, and the rows that may be in it show text:
        // unreadable, never empty.
        cells.extend(found.doubt.iter().map(|&i| &rows[i][..]));
        return InputRead::Unclear(text(&cells, |_| true));
    }
    verdict
}

/// The blanks typed into Claude Code's box, `found`, when it shows nothing else: those before
/// the terminal's `cursor`, when that sits in the box past where its text starts (after the
/// marker, and an IDE pill `pill` terminal cells wide: its display width, not its characters). `None` when it sits there, outside the box, or
/// is unknown.
fn blanks_typed(found: &Found, pill: usize, cursor: Option<(usize, usize)>) -> Option<String> {
    let (x, y) = cursor?;
    let &(first, from) = found.lines.first()?;
    let start = from + pill;
    if y == first {
        return (x > start).then(|| " ".repeat(x - start));
    }
    let line = found.lines.iter().position(|&(row, _)| row == y)?;
    Some(format!("{}{}", "\n".repeat(line), " ".repeat(x)))
}

/// The text on the box's lines `cells` whose cells `keep` keeps, its blank lines at either end
/// left out.
fn text(cells: &[&[Cell]], keep: impl Fn(&Cell) -> bool) -> String {
    let mut out: Vec<String> = cells
        .iter()
        .map(|r| {
            let t: String = r.iter().filter(|c| keep(c)).map(|c| c.ch).collect();
            t.trim().to_string()
        })
        .collect();
    while out.last().is_some_and(String::is_empty) {
        out.pop();
    }
    let start = out.iter().position(|l| !l.is_empty()).unwrap_or(out.len());
    out.split_off(start).join("\n")
}

/// What the box's lines `cells` hold.
fn judge(cells: &[&[Cell]], bash: bool, styled: bool) -> InputRead {
    // What the box holds, faint text left out.
    let typed = || text(cells, |c| !c.faint);
    if bash {
        let rest = typed();
        return InputRead::Typed(if rest.is_empty() {
            "!".into()
        } else {
            format!("! {rest}")
        });
    }
    let ink: Vec<Cell> = cells
        .iter()
        .flat_map(|r| r.iter())
        .filter(|c| !c.ch.is_whitespace())
        .copied()
        .collect();
    if ink.is_empty() {
        return InputRead::Empty;
    }
    if !styled {
        return InputRead::Unclear(typed());
    }
    let unfaint: Vec<usize> = (0..ink.len()).filter(|&i| !ink[i].faint).collect();
    match unfaint[..] {
        [] => InputRead::Empty,
        // Claude Code's own cursor on the placeholder's first character, the rest faint.
        [0] if ink[0].reverse && ink.len() > 1 => InputRead::Empty,
        _ => InputRead::Typed(typed()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A capture from testdata/screens (see gen.sh there): what `tmux capture-pane -p -e` (3.6)
    /// printed for a pane drawing that layout — synthetic wording, the harness's attributes.
    /// `live/…` names one of the live captures (see [`live`]).
    fn screen(name: &str) -> String {
        let path = format!(
            "{}/../../testdata/screens/{name}.txt",
            env!("CARGO_MANIFEST_DIR")
        );
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
    }

    /// The same screen as iTerm gives it: the text alone.
    fn unstyled(screen: &str) -> String {
        screen
            .lines()
            .map(crate::core::backend::strip_ansi)
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn typed(t: &str) -> InputRead {
        InputRead::Typed(t.into())
    }

    /// A live capture from testdata/screens/live: `tmux capture-pane -p -e` (tmux 3.6) of a
    /// real session in a 110x34 pane — Claude Code v2.1.303, Codex v0.163.0 — showing what the
    /// file is named for, then scrubbed (the working directory, usage figures and a plugin name
    /// replaced by neutral text of the same width; escape sequences untouched). gen.sh does not
    /// make these.
    fn live(name: &str) -> String {
        screen(&format!("live/{name}"))
    }

    #[test]
    fn live_captures_read_as_what_their_box_holds() {
        use Harness::{Claude, Codex};
        let cases = [
            // The placeholder (`Try "…"`), faint throughout: the terminal's cursor, not one
            // Claude Code draws.
            ("claude-empty", Claude, InputRead::Empty),
            // `❯` alone: after a turn, and while one runs.
            ("claude-after-turn", Claude, InputRead::Empty),
            ("claude-busy", Claude, InputRead::Empty),
            (
                "claude-typed",
                Claude,
                typed("draft text typed but not sent"),
            ),
            // Three lines pasted, not sent.
            (
                "claude-multiline",
                Claude,
                typed("first line of a draft\nsecond line\nthird line"),
            ),
            // A draft typed while a turn runs (its spinner over the box), and the same draft
            // after Escape stopped that turn: kept, the stopped prompt not put back.
            (
                "claude-busy-with-draft",
                Claude,
                typed("a draft typed while busy"),
            ),
            (
                "claude-interrupted-with-draft",
                Claude,
                typed("a draft typed while busy"),
            ),
            // The prompt Escape put back.
            (
                "claude-restored",
                Claude,
                typed("Write a 300-word essay about tmux."),
            ),
            // Bash mode is text, empty or not: a send there would run as a shell command.
            ("claude-bash-mode-empty", Claude, typed("!")),
            ("claude-bash-mode-text", Claude, typed("! ls")),
            // No background behind the composer here: it is bounded by its footer.
            ("codex-empty", Codex, InputRead::Empty),
            ("codex-typed", Codex, typed("draft text typed but not sent")),
            // The usage-limit error above, the placeholder back in the composer.
            ("codex-after-limit", Codex, InputRead::Empty),
            // A finished turn, the placeholder back in the composer.
            ("codex-after-turn", Codex, InputRead::Empty),
            // A draft typed while a turn runs ("tab to queue message" under it), and the
            // same draft after Escape stopped that turn.
            (
                "codex-busy-with-draft",
                Codex,
                typed("a draft typed while busy"),
            ),
            (
                "codex-interrupted-with-draft",
                Codex,
                typed("a draft typed while busy"),
            ),
            // Escape before any output: Codex does not put the prompt back, the placeholder
            // is there.
            ("codex-interrupted-empty", Codex, InputRead::Empty),
            // The rate-limit popup ("Switch to … for lower credit usage?", `› 1.` …, "enter
            // select · esc back") in place of the composer. A menu, not an approval `answer`
            // reads: no box. A known gap (cli.md, Sending): a plain send types there, and its
            // Enter picks the first choice; tests/send.rs pins the --require-empty hold.
            ("codex-menu-ratelimit", Codex, InputRead::NoBox),
        ];
        for (name, harness, want) in cases {
            assert_eq!(read(&live(name), harness, true), want, "{name}");
        }
    }

    #[test]
    fn claude_code_an_empty_box_placeholder_and_suggestion_included_is_empty() {
        for name in [
            "live/claude-after-turn",
            "claude-placeholder",
            "claude-suggestion",
            "claude-truecolor-suggestion",
        ] {
            assert_eq!(
                read(&screen(name), Harness::Claude, true),
                InputRead::Empty,
                "{name}"
            );
        }
    }

    #[test]
    fn claude_code_blanks_typed_before_the_cursor_are_text() {
        use Harness::{Claude, Codex};
        // The box's first line (`❯`) is the last line that starts with it.
        let box_row = |s: &str| {
            rows(s, true)
                .iter()
                .rposition(|r| text_of(r).starts_with('❯'))
                .expect("a box")
        };
        for name in ["claude-after-turn", "claude-empty"] {
            let s = live(name);
            let row = box_row(&s);
            let at = |x: usize, y: usize| read_at(&s, Claude, true, Some((x, y)));
            // Where the text starts: empty.
            assert_eq!(at(2, row), InputRead::Empty, "{name}");
            // Past it, on the box's line: as many blanks typed (Claude Code draws none).
            assert_eq!(at(3, row), typed(" "), "{name}");
            assert_eq!(at(5, row), typed("   "), "{name}");
            assert!(at(3, row).holds(false), "{name}");
            // Outside the box, or unknown: what the screen shows, as before.
            assert_eq!(at(0, 0), InputRead::Empty, "{name}");
            assert_eq!(at(0, row + 1), InputRead::Empty, "{name}");
            assert_eq!(read_at(&s, Claude, true, None), InputRead::Empty, "{name}");
        }
        // Text in the box is read as it shows, wherever the cursor is.
        let s = live("claude-typed");
        assert_eq!(
            read_at(&s, Claude, true, Some((40, box_row(&s)))),
            typed("draft text typed but not sent")
        );
        let s = live("claude-multiline");
        let row = box_row(&s);
        assert_eq!(
            read_at(&s, Claude, true, Some((3, row))),
            typed("first line of a draft\nsecond line\nthird line")
        );
        // Codex draws its own: its composer is read by what shows.
        assert_eq!(
            read_at(&live("codex-empty"), Codex, true, Some((30, 0))),
            InputRead::Empty
        );
    }

    #[test]
    fn claude_code_a_restored_prompt_is_text() {
        let s = live("claude-restored");
        let r = read(&s, Harness::Claude, true);
        assert_eq!(r, typed("Write a 300-word essay about tmux."));
        assert_eq!(r.reason(), Some("text"));
        assert!(r.holds(false));
        // The text is the person's: it stays out of the message.
        assert!(!r.message().contains("essay"), "{r:?}");
        // In 24-bit colour whose components are 2 and 7 (`38;2;2;7;2`): neither faint nor
        // reverse video, so still text.
        assert_eq!(
            read(&screen("claude-truecolor"), Harness::Claude, true),
            typed("now run the full suite and open a PR")
        );
    }

    #[test]
    fn claude_code_multi_line_text_is_read_whole() {
        // A numbered list in the box is a draft, not a prompt.
        assert_eq!(
            read(&screen("claude-multiline"), Harness::Claude, true),
            typed("fix the flaky login test, then:\n1. run the full suite\n2. open a PR")
        );
    }

    #[test]
    fn claude_code_blanks_after_an_ide_pill_are_counted_in_cells() {
        // The cursor counts terminal cells: a CJK character in the pill takes two.
        let ascii = screen("claude-ide-pill");
        let wide = ascii.replace("[⧉ 12 lines selected]", "[⧉ In 中文.ts]");
        assert_ne!(wide, ascii);
        let row = |s: &str| {
            rows(s, true)
                .iter()
                .rposition(|r| text_of(r).starts_with('❯'))
                .expect("a box")
        };
        // (screen, column where the text starts: the marker, the pill and the space after it)
        for (s, start) in [(&ascii, 24), (&wide, 17)] {
            let at = |x: usize| read_at(s, Harness::Claude, true, Some((x, row(s))));
            assert_eq!(at(start), InputRead::Empty, "start {start}");
            assert_eq!(at(start + 1), typed(" "), "start {start}");
            assert_eq!(at(start + 3), typed("   "), "start {start}");
            assert_eq!(read_at(s, Harness::Claude, true, None), InputRead::Empty);
        }
    }

    #[test]
    fn claude_code_an_ide_selection_pill_is_not_text() {
        // An IDE connected: `[⧉ 12 lines selected] ` leads the empty box, in its own colour.
        assert_eq!(
            read(&screen("claude-ide-pill"), Harness::Claude, true),
            InputRead::Empty
        );
        // The pill, then a restored prompt: the prompt is the text.
        assert_eq!(
            read(&screen("claude-ide-pill-restored"), Harness::Claude, true),
            typed("now run the full suite and open a PR")
        );
        // The pill's colour, but a paste or image marker: typed.
        let s = screen("claude-ide-pill");
        for marker in [
            "[Pasted text #1 +12 lines]",
            "[Image #1]",
            "[...Truncated text #1 +40 lines...]",
        ] {
            let pasted = s.replace("[⧉ 12 lines selected]", marker);
            assert_ne!(pasted, s);
            assert_eq!(
                read(&pasted, Harness::Claude, true),
                typed(marker),
                "{marker}"
            );
        }
        // A `[…]` in the colour of the text after it was typed that way, pill-like or not.
        let same = s.replace("\x1b[39m \x1b[7m", " and more\x1b[39m \x1b[7m");
        assert_ne!(same, s);
        assert_eq!(
            read(&same, Harness::Claude, true),
            typed("[⧉ 12 lines selected] and more")
        );
        // In the terminal's default colour: typed.
        let plain = s.replace("\x1b[38;2;71;130;200m", "");
        assert_eq!(
            read(&plain, Harness::Claude, true),
            typed("[⧉ 12 lines selected]")
        );
        // Without attributes the pill cannot be told apart: the box is unreadable, not empty.
        assert_eq!(
            read(&unstyled(&s), Harness::Claude, false),
            InputRead::Unclear("[⧉ 12 lines selected]".into())
        );
    }

    #[test]
    fn claude_code_without_its_box_cannot_be_told_empty() {
        assert_eq!(
            read(&screen("claude-permission"), Harness::Claude, true),
            InputRead::Prompt
        );
        assert_eq!(
            read(&screen("claude-transcript"), Harness::Claude, true),
            InputRead::NoBox
        );
        assert_eq!(read("", Harness::Claude, true), InputRead::NoBox);
        // The rules alone, or a line at column 0 under the marker: not the box.
        let s = live("claude-restored");
        let broken = s.replace("❯\u{a0}Write", "Write");
        assert_ne!(broken, s);
        assert_eq!(read(&broken, Harness::Claude, true), InputRead::NoBox);
        let broken = s.replace("about tmux.", "about tmux.\nmerged");
        assert_ne!(broken, s);
        assert_eq!(read(&broken, Harness::Claude, true), InputRead::NoBox);
    }

    #[test]
    fn claude_code_typed_text_is_never_taken_for_a_placeholder() {
        let rule = "─".repeat(40);
        let boxed = |line: &str| format!("{rule}\n{line}\n{rule}\n  ? for shortcuts\n");
        let cases = [
            // The cursor on the first of two typed characters.
            ("❯ \x1b[7ma\x1b[0mb", "ab"),
            // One typed character under the cursor, nothing after it.
            ("❯ \x1b[7ma\x1b[0m", "a"),
            // Typed, then a faint completion after it.
            ("❯ /rev\x1b[2miew\x1b[0m", "/rev"),
            // A paste marker.
            ("❯ [Pasted text #1 +12 lines]", "[Pasted text #1 +12 lines]"),
        ];
        for (line, want) in cases {
            assert_eq!(
                read(&boxed(line), Harness::Claude, true),
                typed(want),
                "{line:?}"
            );
        }
        // Bash mode: a send would run as a shell command.
        assert_eq!(read(&boxed("!"), Harness::Claude, true), typed("!"));
        assert_eq!(read(&boxed("! ls"), Harness::Claude, true), typed("! ls"));
        // The placeholder under the cursor, faint after it: empty.
        assert_eq!(
            read(
                &boxed("❯ \x1b[7mT\x1b[0;2mry it\x1b[0m"),
                Harness::Claude,
                true
            ),
            InputRead::Empty
        );
    }

    #[test]
    fn without_text_attributes_only_a_blank_box_is_known_empty() {
        let r = read(
            &unstyled(&screen("claude-placeholder")),
            Harness::Claude,
            false,
        );
        assert_eq!(
            r,
            InputRead::Unclear("Try \"write a test for src/parser.rs\"".into())
        );
        assert_eq!(r.reason(), Some("unreadable"));
        assert_eq!(
            read(
                &unstyled(&live("claude-after-turn")),
                Harness::Claude,
                false
            ),
            InputRead::Empty
        );
        // Escape sequences in a screen not read as styled are ignored, not trusted.
        assert!(matches!(
            read(&screen("claude-suggestion"), Harness::Claude, false),
            InputRead::Unclear(_)
        ));
    }

    #[test]
    fn codex_an_empty_composer_with_its_placeholder_is_empty() {
        assert_eq!(
            read(&screen("codex-empty"), Harness::Codex, true),
            InputRead::Empty
        );
        // A newer footer (`gpt-6 medium · 0% used`), the composer in 24-bit colour.
        assert_eq!(
            read(&screen("codex-status"), Harness::Codex, true),
            InputRead::Empty
        );
    }

    #[test]
    fn codex_the_composer_is_found_by_its_background_whatever_its_footer_says() {
        // A footer fleet has never seen: the composer's background still bounds it.
        for name in ["codex-empty", "codex-status"] {
            let s = screen(name);
            let new = s
                .replace("? for shortcuts", "a footer from a later Codex")
                .replace("gpt-6 medium · 0% used", "a footer from a later Codex")
                .replace("100% context left", "");
            assert!(new.contains("a footer from a later Codex"), "{name}");
            assert!(!CODEX_FOOTER.iter().any(|m| new.contains(m)), "{name}");
            assert_eq!(read(&new, Harness::Codex, true), InputRead::Empty, "{name}");
        }
        let s = screen("codex-status-restored").replace("gpt-6 medium · 2% used", "later");
        assert_eq!(
            read(&s, Harness::Codex, true),
            typed("fix the flaky login test")
        );
        // No background (and no footer fleet knows): no composer it can bound.
        let flat = screen("codex-empty")
            .replace("\x1b[48;5;236m", "")
            .replace("? for shortcuts", "a footer from a later Codex")
            .replace("100% context left", "");
        assert_eq!(read(&flat, Harness::Codex, true), InputRead::NoBox);
    }

    #[test]
    fn codex_the_screen_answer_reads_has_its_composer_found() {
        // prompt.rs's Codex screen (a quoted approval above the composer, the footer
        // `gpt-6 medium · 0% used`): the composer is found — by that footer, as the text
        // comes without attributes here.
        let quoted = "• Codex asks \"Would you like to run the following command?\" first.\n\
                      › 1. Yes, proceed (y)\n  2. No (esc)\n\n› Ask Codex to do anything\n\n  gpt-6 medium · 0% used\n";
        assert_eq!(
            read(quoted, Harness::Codex, false),
            InputRead::Unclear("Ask Codex to do anything".into())
        );
    }

    #[test]
    fn codex_a_restored_prompt_is_text() {
        // Synthetic: Codex 0.163.0 does not put a stopped prompt back (live
        // codex-interrupted-empty), but text in its composer is text. The same words in the
        // conversation above (a `›` line at column 0) are not the box.
        assert_eq!(
            read(&screen("codex-restored"), Harness::Codex, true),
            typed("fix the flaky login test")
        );
        // On a 24-bit background whose components are 7, 2 and 27: not faint, so text.
        assert_eq!(
            read(&screen("codex-status-restored"), Harness::Codex, true),
            typed("fix the flaky login test")
        );
    }

    #[test]
    fn codex_multi_line_text_is_read_whole() {
        assert_eq!(
            read(&screen("codex-multiline"), Harness::Codex, true),
            typed("fix the flaky login test\n\nthen run the full suite")
        );
    }

    #[test]
    fn codex_without_its_composer_cannot_be_told_empty() {
        // The transcript pager: a `›` line, but no composer background or footer under it.
        assert_eq!(
            read(&screen("codex-pager"), Harness::Codex, true),
            InputRead::NoBox
        );
        // An approval prompt in place of the composer (recorded).
        let approval = include_str!("../../../../testdata/codex/exec-approval-pane.txt");
        assert_eq!(read(approval, Harness::Codex, true), InputRead::Prompt);
        // A conversation item at column 0 between the `›` line and the footer.
        let s = screen("codex-empty").replace(
            "\n\n\n\x1b[49m  \x1b[2m?",
            "\n• Ran the tests\n\n\x1b[49m  \x1b[2m?",
        );
        assert_ne!(s, screen("codex-empty"));
        assert_eq!(read(&s, Harness::Codex, true), InputRead::NoBox);
        // A past prompt on its background, then something at column 0 (the pager's way).
        let s = screen("codex-status").replace(
            "\x1b[49m  \x1b[2mgpt-6",
            "\x1b[49m• Ran the tests\n  \x1b[2mgpt-6",
        );
        assert_ne!(s, screen("codex-status"));
        assert_eq!(read(&s, Harness::Codex, true), InputRead::NoBox);
        assert_eq!(
            read(&unstyled(&screen("codex-empty")), Harness::Codex, false),
            InputRead::Unclear("Ask Codex to do anything".into())
        );
    }

    #[test]
    fn codex_footer_words_typed_in_the_composer_are_text_not_its_footer() {
        // Live: a draft that starts with a newline, then words Codex's footer says. The
        // footer is the last rows on screen, under a blank row and drawn in colour or bold;
        // the plain row above them is the draft's.
        let s = live("codex-typed");
        let line = "\n\x1b[1m›\x1b[0m draft text typed but not sent\n";
        assert!(s.contains(line));
        for words in ["context left", "? for shortcuts", "gpt-6 medium · 0% used"] {
            let draft = s.replace(line, &format!("\x1b[1m›\x1b[0m\n  {words}\n"));
            assert_eq!(read(&draft, Harness::Codex, true), typed(words), "{words}");
            // Without attributes the footer cannot be told by how it is drawn: unreadable.
            assert_eq!(
                read(&unstyled(&draft), Harness::Codex, false),
                InputRead::Unclear(words.into()),
                "{words}"
            );
        }
    }

    #[test]
    fn codex_a_footer_it_cannot_tell_from_the_composer_is_never_empty() {
        let s = live("codex-empty");
        let placeholder = "\x1b[1m›\x1b[0m \x1b[2mAsk Codex to do anything\x1b[0m\n";
        assert!(s.contains(placeholder));
        let footer = s.find("  \x1b[38;5;223mGPT-6").unwrap();
        // The draft (`\ncontext left`) alone at the bottom, no footer under it: the only row
        // that could be the footer is plain and has no blank row above it.
        let draft = s[..footer]
            .trim_end()
            .replace(placeholder.trim_end(), "\x1b[1m›\x1b[0m\n  context left");
        assert_eq!(
            read(&draft, Harness::Codex, true),
            InputRead::Unclear("context left".into())
        );
        // The footer straight under the composer, no blank row between: unreadable.
        let tight = s.replace(&format!("{placeholder}\n"), placeholder);
        assert_ne!(tight, s);
        let r = read(&tight, Harness::Codex, true);
        assert!(
            matches!(&r, InputRead::Unclear(t) if t.contains("Context 0% used")),
            "{r:?}"
        );
        // Every row of the footer drawn plain, as typed text is: unreadable.
        let plain = format!(
            "{}{}",
            &s[..footer],
            crate::core::backend::strip_ansi(&s[footer..])
        );
        let r = read(&plain, Harness::Codex, true);
        assert!(matches!(r, InputRead::Unclear(_)), "{r:?}");
    }

    #[test]
    fn codex_a_footer_with_its_status_row_drawn_plain_is_still_its_footer() {
        // Codex draws its status row plain without colours (`NO_COLOR`, or its status line's
        // colours off on a terminal that gives no default colours), and a flash plain in its
        // place; the key hints under it stay bold, so the box is still empty.
        let s = live("codex-empty");
        let status = s.lines().find(|l| l.contains("GPT-6-Astra")).unwrap();
        let plain = s.replace(status, &crate::core::backend::strip_ansi(status));
        assert_ne!(plain, s);
        assert_eq!(read(&plain, Harness::Codex, true), InputRead::Empty);
        let flash = s.replace(status, "  Copied selection");
        assert_eq!(read(&flash, Harness::Codex, true), InputRead::Empty);
    }

    #[test]
    fn codex_an_image_attachment_above_the_composer_is_text() {
        // A recalled image prompt, its text cleared: `[Image #1]` above the placeholder.
        let r = read(&screen("codex-image"), Harness::Codex, true);
        assert_eq!(r, typed("[Image #1]"));
        assert!(r.holds(false));
        assert_eq!(
            read(&unstyled(&screen("codex-image")), Harness::Codex, false),
            InputRead::Unclear("[Image #1]\nAsk Codex to do anything".into())
        );
        // On a live screen, no background behind the composer.
        let image = |n: u8| format!("  \x1b[36m[Image #{n}]\x1b[39m\n");
        let line = "\n\n\n\x1b[1m›";
        let s = live("codex-empty");
        assert!(s.contains(line));
        let one = s.replace(line, &format!("\n{}\n\x1b[1m›", image(1)));
        assert_eq!(read(&one, Harness::Codex, true), typed("[Image #1]"));
        let s = live("codex-typed");
        let two = s.replace(line, &format!("{}{}\n\x1b[1m›", image(1), image(2)));
        assert_ne!(two, s);
        assert_eq!(
            read(&two, Harness::Codex, true),
            typed("[Image #1]\n[Image #2]\ndraft text typed but not sent")
        );
        // An image further up, more than one blank row above the `›` line: the conversation's.
        let far = live("codex-empty").replace(line, &format!("{}\n\n\x1b[1m›", image(1)));
        assert_eq!(read(&far, Harness::Codex, true), InputRead::Empty);
    }

    #[test]
    fn claude_code_an_image_alone_in_the_box_is_text() {
        // Claude Code puts a pasted image in its box as `[Image #1]`: typed.
        assert_eq!(
            read(&screen("claude-image"), Harness::Claude, true),
            typed("[Image #1]")
        );
        // Wrapped in an OSC 8 link to the image, as tmux gives it back (ST), or ended by BEL:
        // the same.
        let link = screen("claude-image-link");
        assert!(link.contains("\x1b]8;;file:///tmp/image-1.png\x1b\\[Image #1]\x1b]8;;\x1b\\"));
        let bel = link.replace("\x1b\\", "\x07");
        assert_ne!(bel, link);
        for s in [link, bel] {
            assert_eq!(read(&s, Harness::Claude, true), typed("[Image #1]"));
        }
    }

    #[test]
    fn pi_an_empty_editor_is_empty() {
        assert_eq!(
            read(&screen("pi-empty"), Harness::Pi, true),
            InputRead::Empty
        );
        assert_eq!(
            read(&unstyled(&screen("pi-empty")), Harness::Pi, false),
            InputRead::Empty
        );
    }

    #[test]
    fn pi_text_in_the_editor_is_text() {
        assert_eq!(
            read(&screen("pi-text"), Harness::Pi, true),
            typed("now run the full suite and open a PR")
        );
        assert_eq!(
            read(&screen("pi-multiline"), Harness::Pi, true),
            typed("fix the flaky login test, then:\n- run the full suite\n- open a PR")
        );
    }

    #[test]
    fn pi_without_its_editor_cannot_be_told_empty() {
        assert_eq!(
            read(&screen("pi-selector"), Harness::Pi, true),
            InputRead::NoBox
        );
    }

    #[test]
    fn only_text_or_a_prompt_holds_unless_an_empty_box_is_required() {
        for (r, default, required) in [
            (InputRead::Empty, false, false),
            (typed("x"), true, true),
            (InputRead::Prompt, true, true),
            (InputRead::NoBox, false, true),
            (InputRead::Unclear("x".into()), false, true),
        ] {
            assert_eq!(
                (r.holds(false), r.holds(true)),
                (default, required),
                "{r:?}"
            );
            // What is typed anyway says what fleet could not know.
            assert_eq!(r.warning().is_some(), !default && r != InputRead::Empty);
        }
    }

    #[test]
    fn attributes_carry_from_line_to_line_and_reset() {
        let r = rows(
            "\x1b[2ma\nb\x1b[22mc\x1b[7md\x1b[mE\x1b[38;5;2;7mf\x1b[38:2::1:2:3;2mg",
            true,
        );
        let flags: Vec<(char, bool, bool)> = r
            .iter()
            .flatten()
            .map(|c| (c.ch, c.faint, c.reverse))
            .collect();
        assert_eq!(
            flags,
            [
                ('a', true, false),
                ('b', true, false),
                ('c', false, false),
                ('d', false, true),
                ('E', false, false),
                ('f', false, true),
                ('g', true, true),
            ]
        );
    }

    #[test]
    fn colours_are_read_never_as_attributes() {
        // Each 24-bit or palette colour's own numbers (2 faint, 7 reverse, 22, 27) are not
        // attributes, in either notation.
        let r = rows(
            "\x1b[38;2;2;7;2ma\x1b[48;2;7;2;27mb\x1b[2;38;2;22;27;22mc\x1b[0;38:2:7:2:7md\x1b[38:5:2;48;5;7me\x1b[31;102mf\x1b[39;49mg",
            true,
        );
        let cells: Vec<(char, bool, bool, Colour, Colour)> = r
            .iter()
            .flatten()
            .map(|c| (c.ch, c.faint, c.reverse, c.fg, c.bg))
            .collect();
        use Colour::{Default as D, Indexed, Rgb};
        assert_eq!(
            cells,
            [
                ('a', false, false, Rgb(2, 7, 2), D),
                ('b', false, false, Rgb(2, 7, 2), Rgb(7, 2, 27)),
                ('c', true, false, Rgb(22, 27, 22), Rgb(7, 2, 27)),
                ('d', false, false, Rgb(7, 2, 7), D),
                ('e', false, false, Indexed(2), Indexed(7)),
                ('f', false, false, Indexed(1), Indexed(10)),
                ('g', false, false, D, D),
            ]
        );
    }
}
