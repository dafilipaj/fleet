//! Turning one [`Session`] into one [`ListItem`], at whatever width we were given.
//!
//! On a desktop the item is two lines — identity, then context:
//!
//! ```text
//! ▸1 ● cache-warmup-scheduler              ⧉ api-server-log    working    6m
//!      make the cache warmup idempotent across retries          ~/Code/notes
//! ```
//!
//! On a phone it is three, because a forty-column pane cannot fit the terminal
//! label and the prompt on one line without starving the prompt:
//!
//! ```text
//! ▸1 ○ app-03                idle   88m
//!      ▣ ✳ Nightly backup rotation (web…
//!      Set up nightly backups for me. Read…
//! ```
//!
//! Line 1 is identity — the caret, the jump digit, the state dot and then the
//! **headline**, bold and bright, with everything else pushed into a dim column on
//! the right. Then context: the prompt, given a whole line's width, and the
//! terminal and directory, which only say what the block title and each other
//! don't.
//!
//! The headline is [`Session::headline`] — the LLM-generated title, which is what
//! the work *is*. The Claude session name and the terminal tab name never take
//! that slot; both answer "which pane is this", and a row already carries the
//! terminal label for that. The session name only shows through while a title
//! hasn't been generated yet.
//!
//! Rows are assembled **to fit**: every line ends up exactly `plan.width` columns
//! wide, padding included. That's not cosmetic — the selected row is highlighted
//! with a background sweep across its spans, and relying on the terminal to clip
//! an over-long `{:<8}`-padded span leaves the highlight ragged.

use std::collections::HashMap;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::ListItem;

use crate::cli::render::{
    ago, clean_text, column, fit, home_rel, status_words, terminal_label, width_of,
};
use crate::core::discovery::Session;
use crate::tui::dash::layout::{Density, Plan};

/// Background of the selected row. Dark enough to read through in both themes.
const SELECTED_BG: Color = Color::Indexed(236);

/// Width of the status-word cell — `needs you` is the longest thing in it.
const WORD_CELL: usize = 9;

/// Columns line 1 keeps for the name before it starts dropping right-hand cells.
/// A desktop pane never gets near it; a phone-sized one trades the status word for
/// name room, because the coloured dot says the same thing in one column.
fn name_floor(p: &Plan) -> usize {
    if p.density == Density::Wide { 8 } else { 14 }
}

/// Where a continuation line starts: under the name, past the caret, the jump
/// digit and the state dot.
fn indent(p: &Plan) -> usize {
    if p.gutter { 5 } else { 4 }
}

/// Width of the context cell — `ctx 100%`.
const CTX_CELL: usize = 8;

/// `ctx 62%`, coloured by how full the window is; blank when unknown so the
/// column still lines up.
fn ctx_text(s: &Session) -> (String, Style) {
    match &s.context {
        Some(c) => {
            let style = match crate::core::context::level(c.pct) {
                crate::core::context::Level::Low => dim_style(),
                crate::core::context::Level::Warn => Style::default().fg(Color::Indexed(214)),
                crate::core::context::Level::Hot => Style::default().fg(Color::Red),
            };
            (format!("ctx {}%", c.pct.min(999)), style)
        }
        None => (String::new(), dim_style()),
    }
}

/// Width of the age cell — `120m` is the longest thing `ago` produces.
const AGE_CELL: usize = 4;

/// Widest the headline cell grows to when sizing line 1's content block. A
/// fleet's longest headline sets the block's width; past this it stops dragging
/// every other row's status column right with it, and pushes only its own.
const NAME_CELL_MAX: usize = 34;

/// The headline: the brightest thing in the item, because it's what the user
/// navigates by.
fn name_style() -> Style {
    Style::default()
        .fg(Color::White)
        .add_modifier(Modifier::BOLD)
}

/// Anything subordinate to the name: ages, paths, the jump digit.
fn dim_style() -> Style {
    Style::default().fg(Color::DarkGray)
}

fn term_style(label: &str) -> Style {
    if label == "-" {
        dim_style()
    } else {
        Style::default().fg(Color::Cyan)
    }
}

fn status_style(status: &str) -> Style {
    match status {
        "busy" => Style::default().fg(Color::Green),
        s if crate::core::discovery::is_idle(s) => Style::default().fg(Color::DarkGray),
        "waiting" => Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
        _ => Style::default().fg(Color::Gray),
    }
}

/// The `1`-`9`/`0` digit a row can be jumped to with, or a blank past the tenth.
fn gutter_char(idx: usize) -> char {
    match idx {
        0..=8 => (b'1' + idx as u8) as char,
        9 => '0',
        _ => ' ',
    }
}

/// Accumulates spans while tracking how many display columns are left.
struct RowBuf {
    spans: Vec<Span<'static>>,
    used: usize,
    width: usize,
}

impl RowBuf {
    fn new(width: usize) -> Self {
        Self {
            spans: Vec::new(),
            used: 0,
            width,
        }
    }

    fn room(&self) -> usize {
        self.width.saturating_sub(self.used)
    }

    /// Append, clipped to whatever room is left. A zero-width push is dropped.
    fn push(&mut self, text: impl AsRef<str>, style: Style) {
        let room = self.room();
        if room == 0 {
            return;
        }
        let text = fit(text.as_ref(), room);
        let w = width_of(&text);
        if w == 0 {
            return;
        }
        self.used += w;
        self.spans.push(Span::styled(text, style));
    }

    /// Pad with spaces up to `target` columns (no-op if already past it).
    fn fill_to(&mut self, target: usize) {
        let want = target.saturating_sub(self.used).min(self.room());
        if want > 0 {
            self.used += want;
            self.spans.push(Span::raw(" ".repeat(want)));
        }
    }

    /// Fill the rest of the line with spaces so the highlight covers it.
    fn fill(&mut self) {
        let room = self.room();
        if room > 0 {
            self.used += room;
            self.spans.push(Span::raw(" ".repeat(room)));
        }
    }

    fn finish(mut self, selected: bool) -> Line<'static> {
        self.fill();
        if selected {
            for s in &mut self.spans {
                s.style = s.style.bg(SELECTED_BG);
            }
        }
        Line::from(self.spans)
    }
}

/// Facts about the whole fleet that a single row can't work out on its own.
///
/// Pure, and recomputed per frame: both fields exist to make rows line up with
/// *each other* — a terminal cell as wide as the widest label there actually is,
/// and the directory every session shares, which is drawn once in the block title
/// instead of once per row.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FleetCtx {
    /// Home-relative directory most of the fleet sits in or under — `None` when
    /// there is no such directory, or when this pane can't name it (see
    /// [`FleetCtx::of`]). Rows only draw relative paths while it is `Some`.
    pub base: Option<String>,
    /// Width of the `⧉ <terminal>` cell.
    pub term_cell: usize,
    /// Width the name is laid out in when placing line 1's right-hand column.
    pub name_cell: usize,
    /// Whether rows carry a `ctx NN%` cell: only when some session has a known
    /// context usage, so a fleet without transcripts keeps its old layout.
    pub ctx_cell: bool,
}

impl FleetCtx {
    pub fn of(rows: &[Session], p: &Plan) -> Self {
        let widest =
            |f: fn(&Session) -> String| rows.iter().map(|s| width_of(&f(s))).max().unwrap_or(0);
        Self {
            // A base the block title can't print is a base the rows must not
            // elide: a relative path with nothing to relate it to is a lie about
            // where the session is. One decision, made here, so the title and the
            // rows can never disagree.
            base: base_dir(rows).filter(|b| {
                p.show_base && base_title(b).is_some_and(|t| width_of(&t) + 2 <= p.width)
            }),
            term_cell: widest(terminal_label).clamp(3, p.term_cell.max(3)),
            name_cell: widest(|s| s.headline()).clamp(12, p.name_width.clamp(12, NAME_CELL_MAX)),
            ctx_cell: rows.iter().any(|s| s.context.is_some()),
        }
    }
}

/// The sessions block's title when the fleet has a directory in common. `None`
/// keeps the plain ` Sessions `.
pub fn base_title(base: &str) -> Option<String> {
    Some(format!(" Sessions · {base} "))
}

/// Where line 1's right-hand column ends.
///
/// A 170-column terminal does not need ninety columns of nothing between the name
/// and the status, so line 1 lays itself out inside a **bounded content block** —
/// as wide as the fleet's longest name plus the cells behind it — and leaves the
/// rest of the pane alone. Line 2's prompt still gets the whole width, which is
/// the line that has something to do with it.
fn head_width(p: &Plan, ctx: &FleetCtx) -> usize {
    // A phone-sized pane has no columns to give away: there the block *is* the
    // pane, and the state column sits hard against the right edge.
    if p.density != Density::Wide {
        return p.width;
    }
    let term = if p.term_on_head { ctx.term_cell + 2 } else { 0 };
    let word = if p.show_status_word { WORD_CELL + 1 } else { 0 };
    let ctx_w = if ctx.ctx_cell { CTX_CELL + 2 } else { 0 };
    let left = if p.gutter { 5 } else { 4 };
    let cells = term + ctx_w + word + AGE_CELL;
    p.width.min(left + ctx.name_cell + 2 + cells)
}

/// Every directory a path sits in or under, shallowest first, itself included.
fn ancestors(path: &str) -> Vec<&str> {
    let mut out: Vec<&str> = path
        .char_indices()
        .filter(|&(i, c)| c == '/' && i > 0)
        .map(|(i, _)| &path[..i])
        .collect();
    if !path.is_empty() {
        out.push(path);
    }
    out
}

/// The directory the fleet has in common: the **deepest** home-relative path at
/// least half the sessions (and at least two of them) sit in or under.
///
/// This is what makes `~/Code/app` stop repeating on every row — the dashboard
/// names it once, in the sessions block's title, and rows draw what's left.
/// Requiring a quorum is what keeps it honest: three sessions in three unrelated
/// repos have no common base worth claiming, so they each show their own path.
pub fn base_dir(rows: &[Session]) -> Option<String> {
    let paths: Vec<String> = rows
        .iter()
        .filter_map(|s| s.cwd.as_deref())
        .map(home_rel)
        .collect();
    if paths.len() < 2 {
        return None;
    }
    let quorum = paths.len().div_ceil(2).max(2);
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for p in &paths {
        for a in ancestors(p) {
            *counts.entry(a).or_default() += 1;
        }
    }
    counts
        .into_iter()
        .filter(|&(_, n)| n >= quorum)
        // Deepest wins; the path itself is in the key so ties don't depend on the
        // hash map's iteration order.
        .max_by_key(|&(p, n)| (p.matches('/').count(), p.len(), n, p))
        .map(|(p, _)| p.to_string())
}

/// The terminal name as it is actually *drawn*, glyph stripped — or `""` when
/// the row shows no terminal at all.
fn drawn_terminal(label: &str) -> &str {
    label
        .strip_prefix("⧉ ")
        .or_else(|| label.strip_prefix("▣ "))
        .unwrap_or("")
        .trim()
}

/// Lowercase, minus a trailing `-<digits>` disambiguator: `app-3` and `Work`
/// are the same name as far as a reader of the row is concerned.
fn normalize(name: &str) -> String {
    let name = name.trim().to_lowercase();
    match name.rsplit_once('-') {
        Some((head, tail))
            if !head.is_empty() && !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit()) =>
        {
            head.to_string()
        }
        _ => name,
    }
}

/// Would printing `base` next to `label` just say the same thing twice?
///
/// Compared against the label the row *draws* — `terminal_label` shows the tmux
/// session **or** the tab, tmux winning — because comparing against both fields
/// suppressed cwds that weren't duplicated at all. Near-misses count too: a
/// `app-3` pane in `~/Code/app` drew `⧉ app-3  app`, which is exactly the
/// redundancy this exists to remove.
fn echoes_terminal(base: &str, label: &str) -> bool {
    let term = normalize(drawn_terminal(label));
    let base = normalize(base);
    if term.is_empty() || base.is_empty() {
        return false;
    }
    term.starts_with(&base) || base.starts_with(&term)
}

/// The directory a row draws, or `None` when it would add nothing.
///
/// The rule is: **draw the part of the path that neither the block title nor the
/// row's own terminal name already tells you.** In practice that's
/// 1. at the fleet's common base — nothing, because the title says it
///    (`~/Code/app` on all twelve rows is the noise this exists to remove);
/// 2. under the base — the remainder, minus a last component that just repeats
///    the terminal label: `⧉ api-server-log` plus `branches/` *is* the whole path,
///    and `branches/` vs `clones/` is what tells two same-named sessions apart;
/// 3. outside the base — the whole path, terminal name or not, because "this one
///    is somewhere else entirely" is worth every column it costs.
fn location(s: &Session, p: &Plan, ctx: &FleetCtx) -> Option<String> {
    if !p.show_cwd {
        return None;
    }
    let cwd = home_rel(s.cwd.as_deref()?);
    let label = terminal_label(s);
    let Some(root) = ctx.base.as_deref() else {
        return Some(cwd);
    };
    if cwd == root {
        return None;
    }
    match cwd.strip_prefix(root).and_then(|r| r.strip_prefix('/')) {
        Some(rest) => {
            // The terminal name is already on the row, so a remainder ending in
            // it draws only the directory that *contains* it: `⧉ api-server-log`
            // plus `branches/` is the whole path, said once. Nothing left to
            // say means nothing drawn.
            let (head, tail) = rest.rsplit_once('/').unwrap_or(("", rest));
            if echoes_terminal(tail, &label) {
                (!head.is_empty()).then(|| format!("{head}/"))
            } else {
                Some(rest.to_string())
            }
        }
        None => Some(cwd),
    }
}

/// A path trimmed to what a line can spare for it: the whole thing while a third
/// of the line fits it, else its last component — the part that says the most —
/// else nothing at all. The prompt and the terminal label both outrank it, and at
/// 44 columns a 37-column path would have left neither of them anything.
fn shorten_path(loc: String, label: &str, room: usize) -> Option<String> {
    let cap = room / 3;
    if width_of(&loc) <= cap {
        return Some(loc);
    }
    let tail = loc.trim_end_matches('/').rsplit('/').next().unwrap_or("");
    (!tail.is_empty() && width_of(tail) <= cap && !echoes_terminal(tail, label))
        .then(|| tail.to_string())
}

fn title_of(s: &Session) -> String {
    clean_text(s.title.as_deref().unwrap_or("(no prompt)"))
}

/// Caret bold, jump digit dim: fifteen bright digits down the left edge compete
/// with the names, and the names are the point.
fn push_prefix(r: &mut RowBuf, p: &Plan, idx: usize, selected: bool) {
    let caret = if selected { '▸' } else { ' ' };
    r.push(
        caret.to_string(),
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    );
    if p.gutter {
        let digit = gutter_char(idx);
        let style = if selected {
            Style::default().fg(Color::Yellow)
        } else {
            dim_style()
        };
        r.push(digit.to_string(), style);
    }
    r.push(" ", Style::default());
}

/// One session, rendered for the given plan. 1 or 2 lines; always exactly
/// `plan.width` columns wide.
pub fn session_item(
    s: &Session,
    p: &Plan,
    ctx: &FleetCtx,
    idx: usize,
    selected: bool,
) -> ListItem<'static> {
    ListItem::new(session_lines(s, p, ctx, idx, selected))
}

/// The lines behind [`session_item`]. Separate so tests can measure their real
/// display width — a rendered buffer pads the cell after a double-wide glyph,
/// which makes the drawn text look one column wider than it is.
pub fn session_lines(
    s: &Session,
    p: &Plan,
    ctx: &FleetCtx,
    idx: usize,
    selected: bool,
) -> Vec<Line<'static>> {
    match p.lines {
        // Identity, then where it lives, then what it's doing — one line each,
        // because at phone widths sharing one leaves the prompt a stub.
        3 => vec![
            head(s, p, ctx, idx, selected),
            where_line(s, p, ctx, selected),
            prompt_line(s, p, selected),
        ],
        2 => vec![head(s, p, ctx, idx, selected), context(s, p, ctx, selected)],
        _ if p.density == Density::Tiny => vec![tiny(s, p, selected)],
        _ => vec![one_row(s, p, ctx, idx, selected)],
    }
}

/// `● name…` plus an age when there's anything left over.
fn tiny(s: &Session, p: &Plan, selected: bool) -> Line<'static> {
    let (dot, _) = status_words(&s.status);
    let mut r = RowBuf::new(p.width);
    let caret = if selected { '▸' } else { ' ' };
    r.push(format!("{caret}{dot} "), status_style(&s.status));
    // Under ~16 columns there is no honest way to show anything but the name.
    let age = if p.width >= 16 {
        format!(" {}", ago(s.updated_at))
    } else {
        String::new()
    };
    let name_room = r.room().saturating_sub(width_of(&age));
    r.push(fit(&s.headline(), name_room), name_style());
    if !age.is_empty() {
        r.fill_to(p.width.saturating_sub(width_of(&age)));
        r.push(age, dim_style());
    }
    r.finish(selected)
}

/// Columnar single line: caret, dot, name, status, age, terminal, dir, prompt.
/// The compact mode `z` switches to when fifteen sessions matter more than
/// reading any one of them.
fn one_row(s: &Session, p: &Plan, ctx: &FleetCtx, idx: usize, selected: bool) -> Line<'static> {
    let (dot, word) = status_words(&s.status);
    let st = status_style(&s.status);
    let mut r = RowBuf::new(p.width);

    push_prefix(&mut r, p, idx, selected);
    r.push(format!("{dot} "), st);
    r.push(
        column(&s.headline(), p.name_width.min(r.room())),
        name_style(),
    );
    r.push(" ", Style::default());
    if p.show_status_word {
        r.push(column(word, WORD_CELL), st);
        r.push(" ", Style::default());
    }
    r.push(format!("{:>4}  ", ago(s.updated_at)), dim_style());
    if ctx.ctx_cell && r.room() > CTX_CELL + 20 {
        let (text, style) = ctx_text(s);
        r.push(format!("{}  ", column(&text, CTX_CELL)), style);
    }

    if r.room() > 8 {
        let label = terminal_label(s);
        let cap = ctx.term_cell.min(r.room());
        r.push(column(&label, cap), term_style(&label));
        r.push(" ", Style::default());
    }
    if r.room() > 12
        && let Some(loc) = location(s, p, ctx)
    {
        // Keep at least 8 columns for the prompt so it never vanishes entirely.
        let cap = r.room().saturating_sub(8).min(30);
        r.push(fit(&loc, cap), Style::default().fg(Color::Blue));
        r.push("  ", Style::default());
    }
    if r.room() > 6 {
        r.push(title_of(s), Style::default().fg(Color::Gray));
    }
    r.finish(selected)
}

/// The right-hand column of line 1, richest first: terminal, status, age. Each
/// cell carries its own trailing gap, so the cells line up down the pane.
fn head_cells(s: &Session, p: &Plan, ctx: &FleetCtx) -> Vec<(String, Style)> {
    let (_, word) = status_words(&s.status);
    let st = status_style(&s.status);
    let mut cells: Vec<(String, Style)> = Vec::new();
    if p.term_on_head {
        let label = terminal_label(s);
        cells.push((
            format!("{}  ", column(&label, ctx.term_cell)),
            term_style(&label),
        ));
    }
    if ctx.ctx_cell {
        let (text, style) = ctx_text(s);
        cells.push((format!("{}  ", column(&text, CTX_CELL)), style));
    }
    if p.show_status_word {
        cells.push((format!("{} ", column(word, WORD_CELL)), st));
    }
    cells.push((format!("{:>4}", ago(s.updated_at)), dim_style()));
    cells
}

fn cells_width(cells: &[(String, Style)]) -> usize {
    cells.iter().map(|(t, _)| width_of(t)).sum()
}

/// Line 1 of a full item: the name, as big as the pane allows, with the state
/// column pinned right.
fn head(s: &Session, p: &Plan, ctx: &FleetCtx, idx: usize, selected: bool) -> Line<'static> {
    let (dot, _) = status_words(&s.status);
    let st = status_style(&s.status);
    let left = indent(p);

    // The right column wins over the name: `needs you` is the whole point of the
    // row, so a long name gets ellipsised rather than the status dropped. On a
    // pane too narrow for all of it the richest cell goes first — the terminal,
    // then the status word — and the age is never dropped at all.
    let mut cells = head_cells(s, p, ctx);
    while cells.len() > 1 && left + cells_width(&cells) + name_floor(p) > p.width {
        cells.remove(0);
    }
    if left + cells_width(&cells) + 4 > p.width {
        cells.clear();
    }
    let right = cells_width(&cells);

    let mut r = RowBuf::new(p.width);
    push_prefix(&mut r, p, idx, selected);
    r.push(format!("{dot} "), st);
    let name_cap = p.name_width.min(p.width.saturating_sub(left + 1 + right));
    let name = fit(&s.headline(), name_cap);
    let used = left + width_of(&name);
    r.push(name, name_style());
    if right > 0 {
        // The block's own width, unless this row's name is long enough to need
        // more — in which case it pushes its own status column right and leaves
        // everyone else's where it was.
        let end = head_width(p, ctx).max(used + 2 + right).min(p.width);
        r.fill_to(end.saturating_sub(right));
        for (text, style) in cells {
            r.push(text, style);
        }
    }
    r.finish(selected)
}

/// Line 2 of the desktop item: what the session is doing, across the rest of the
/// width, with the directory pinned right under line 1's state column.
fn context(s: &Session, p: &Plan, ctx: &FleetCtx, selected: bool) -> Line<'static> {
    let mut r = RowBuf::new(p.width);
    // Line up under the name on line 1: caret + digit + space + dot + space.
    r.fill_to(indent(p));
    // The prompt is why this line exists, so the path only gets what it can spare.
    let loc = location(s, p, ctx).and_then(|l| shorten_path(l, &terminal_label(s), r.room()));
    let reserved = loc.as_deref().map_or(0, |l| width_of(l) + 2);
    let title_cap = r.room().saturating_sub(reserved);
    if title_cap >= 4 {
        r.push(
            fit(&title_of(s), title_cap),
            Style::default().fg(Color::Gray),
        );
    }
    if let Some(loc) = loc {
        // The path lands under line 1's status column, so the two lines share a
        // right edge — unless the prompt already ran past it, in which case it
        // trails the prompt.
        let end = head_width(p, ctx)
            .max(r.used + 2 + width_of(&loc))
            .min(p.width);
        r.fill_to(end.saturating_sub(width_of(&loc)));
        r.push(loc, Style::default().fg(Color::Blue));
    }
    r.finish(selected)
}

/// Line 2 of the phone item: where the session lives. The terminal label gets the
/// line — it is how the user finds the pane by hand, and an iTerm tab title like
/// `✳ PR #42 retry budget persisted state` carries real detail the 24-character
/// headline had to drop. The directory only joins it when it says something the
/// block title and the label don't.
fn where_line(s: &Session, p: &Plan, ctx: &FleetCtx, selected: bool) -> Line<'static> {
    let mut r = RowBuf::new(p.width);
    r.fill_to(indent(p));
    let label = terminal_label(s);
    // What sits at the right edge of this line: the status word when line 1 gave
    // it up (under 40 columns, to keep the name readable — `⏸ needs you` in words
    // is the row the user is hunting for, and one glyph is easy to miss),
    // otherwise the directory, when the directory has anything to add.
    let tail = if p.show_status_word {
        location(s, p, ctx)
            .and_then(|l| shorten_path(l, &label, r.room()))
            .map(|l| (l, Style::default().fg(Color::Blue)))
    } else {
        let (_, word) = status_words(&s.status);
        Some((word.to_string(), status_style(&s.status)))
    }
    // The label outranks whatever that is: it doesn't get squeezed under ~8
    // columns for it.
    .filter(|(t, _)| width_of(t) + 8 <= r.room());

    let reserved = tail.as_ref().map_or(0, |(t, _)| width_of(t) + 2);
    r.push(
        fit(&label, r.room().saturating_sub(reserved)),
        term_style(&label),
    );
    if let Some((tail, style)) = tail {
        r.fill_to(p.width.saturating_sub(width_of(&tail)));
        r.push(tail, style);
    }
    r.finish(selected)
}

/// Line 3 of the phone item: what the session is doing, with a whole line to say
/// it in. Behind the terminal label on line 2 this was down to fifteen columns of
/// a forty-column pane, which is where the redesign started.
fn prompt_line(s: &Session, p: &Plan, selected: bool) -> Line<'static> {
    let mut r = RowBuf::new(p.width);
    r.fill_to(indent(p));
    r.push(
        fit(&title_of(s), r.room()),
        Style::default().fg(Color::Gray),
    );
    r.finish(selected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::dash::layout::plan;

    fn session(name: &str, status: &str) -> Session {
        Session {
            pid: 4242,
            session_id: Some("aaaaaaaa-0000-0000".into()),
            name: Some(name.into()),
            cwd: Some("/home/x/Code/app/branches/api-server".into()),
            status: status.into(),
            updated_at: Some(chrono::Utc::now().timestamp_millis() - 120_000),
            tmux_session: Some("api-server".into()),
            title: Some("make the cache warmup idempotent so a re-run is free".into()),
            ..Default::default()
        }
    }

    fn ctx_of(s: &Session, p: &Plan) -> FleetCtx {
        FleetCtx::of(std::slice::from_ref(s), p)
    }

    /// Render one item into a fresh buffer and read the drawn lines back.
    fn drawn(s: &Session, p: &Plan, idx: usize, selected: bool) -> Vec<String> {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        use ratatui::widgets::List;

        let ctx = ctx_of(s, p);
        let h = p.item_height();
        let mut t = Terminal::new(TestBackend::new(p.width as u16, h)).unwrap();
        t.draw(|f| {
            f.render_widget(
                List::new(vec![session_item(s, p, &ctx, idx, selected)]),
                f.area(),
            );
        })
        .unwrap();
        let buf = t.backend().buffer().clone();
        (0..h)
            .map(|y| {
                (0..p.width as u16)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect()
    }

    const SIZES: [(u16, u16); 9] = [
        (200, 50),
        (170, 40),
        (160, 40),
        (100, 30),
        (80, 24),
        (60, 24),
        (44, 16),
        (32, 12),
        (24, 10),
    ];

    // Claude Code's `shell` (idle, a background shell running) is drawn as idle is.
    #[test]
    fn shell_is_styled_as_idle() {
        assert_eq!(status_style("shell"), status_style("idle"));
        assert_ne!(status_style("shell"), status_style("busy"));
        assert_ne!(status_style("shell"), status_style("unknown"));
    }

    #[test]
    fn rows_never_exceed_the_inner_width() {
        let long = session("cache-warmup-scheduler-and-then-some-more-words", "busy");
        for (w, h) in SIZES {
            for forced in [None, Some(true), Some(false)] {
                let p = plan(w, h, 1, 46, forced);
                let ctx = ctx_of(&long, &p);
                for sel in [false, true] {
                    for line in session_lines(&long, &p, &ctx, 0, sel) {
                        assert_eq!(line.width(), p.width, "{w}x{h} {forced:?} sel={sel}");
                    }
                }
            }
        }
    }

    #[test]
    fn two_rows_are_what_a_desktop_terminal_draws() {
        for (w, h) in [(170u16, 40u16), (200, 50), (100, 30), (72, 24)] {
            let p = plan(w, h, 1, 12, None);
            assert_eq!(p.lines, 2, "{w}x{h}");
            let s = session("cache-warmup", "busy");
            let ctx = ctx_of(&s, &p);
            assert_eq!(session_item(&s, &p, &ctx, 0, true).height(), 2);
            let out = drawn(&s, &p, 0, true);
            // Line 1 = identity + state, line 2 = the prompt.
            assert!(out[0].contains("cache-warmup"), "{out:?}");
            assert!(out[0].contains("working"), "{out:?}");
            assert!(
                out[1].contains("make the cache warmup idempotent"),
                "{out:?}"
            );
        }
    }

    // Context usage rides on line 1 next to the status, at every size the row
    // has room for it — and a fleet with no known usage draws no empty column.
    #[test]
    fn the_context_cell_shows_usage_when_known() {
        let mut s = session("cache-warmup", "busy");
        s.context = Some(crate::core::context::ContextUsage::new(
            124_000,
            200_000,
            Some("claude-x".into()),
        ));
        for (w, h) in [(170u16, 40u16), (100, 30), (80, 24)] {
            let p = plan(w, h, 1, 12, None);
            let out = drawn(&s, &p, 0, false);
            assert!(out[0].contains("ctx 62%"), "{w}x{h} {out:?}");
            assert!(out[0].contains("working"), "{w}x{h} {out:?}");
        }
        // Compact one-line mode keeps it too.
        let p = plan(170, 40, 1, 12, Some(false));
        assert_eq!(p.lines, 1);
        assert!(drawn(&s, &p, 0, false).join("").contains("ctx 62%"));
        // Every size still fits the pane with the extra cell.
        for (w, h) in SIZES {
            let p = plan(w, h, 1, 46, None);
            let ctx = ctx_of(&s, &p);
            for line in session_lines(&s, &p, &ctx, 0, false) {
                assert_eq!(line.width(), p.width, "{w}x{h}");
            }
        }
        s.context = None;
        let p = plan(170, 40, 1, 12, None);
        assert!(!drawn(&s, &p, 0, false)[0].contains("ctx"));
    }

    // The whole point of the headline: a row shows what the session is *doing*.
    // `app-03` and an iTerm tab title both answer "which pane is this" — the
    // terminal label is where that belongs, and it keeps its place on the row.
    #[test]
    fn the_generated_title_outranks_the_session_and_tab_names() {
        let mut s = session("app-03", "busy");
        s.name_source = Some("derived".into());
        s.tab = Some("✳ some tab title".into());
        s.gen_title = Some("upload-retry-limit".into());
        let p = plan(170, 40, 3, width_of("upload-retry-limit"), None);
        let out = drawn(&s, &p, 0, false).join("\n");
        assert!(out.contains("upload-retry-limit"), "{out}");
        assert!(!out.contains("app-03"), "{out}");
        // A name somebody chose *is* the title: it outranks the generated one.
        s.name = Some("retry-cap".into());
        s.name_source = Some("user".into());
        let out = drawn(&s, &p, 0, false).join("\n");
        assert!(out.contains("retry-cap"), "{out}");
        assert!(!out.contains("upload-retry-limit"), "{out}");
    }

    // Until the titling pass answers there is still a row to draw.
    #[test]
    fn a_row_with_no_title_yet_falls_back_to_the_session_name() {
        let s = session("app-03", "busy");
        let p = plan(170, 40, 3, 12, None);
        let out = drawn(&s, &p, 0, false).join("\n");
        assert!(out.contains("app-03"), "{out}");
    }

    // The headline is what the fleet's name column is sized to — a long title
    // must not be clipped to the width of the short session name behind it.
    #[test]
    fn the_name_cell_is_sized_to_the_headline() {
        let mut s = session("app-03", "busy");
        s.name_source = Some("derived".into());
        s.gen_title = Some("results-list-rerender".into());
        let p = plan(170, 40, 3, width_of("results-list-rerender"), None);
        assert_eq!(
            ctx_of(&s, &p).name_cell,
            width_of("results-list-rerender").max(12)
        );
    }

    // The name is the user's orientation anchor: it starts in the same column on
    // every row, right after the caret, digit and dot, and nothing precedes it.
    #[test]
    fn the_name_leads_line_one_at_a_fixed_indent() {
        let p = plan(170, 40, 3, 22, None);
        let s = session("cache-warmup", "busy");
        assert!(drawn(&s, &p, 0, false)[0].starts_with(" 1 ● cache-warmup "));
        assert!(drawn(&s, &p, 2, true)[0].starts_with("▸3 ● cache-warmup "));
    }

    // The whole reason for the two-line shape: a name that a person would
    // actually give a session is never cut on a desktop terminal.
    #[test]
    fn desktop_widths_never_truncate_a_real_name() {
        for name in [
            "resolve-css-conflicts",
            "cache-warmup-scheduler",
            "frontend-bundle-size-audit",
            "tb-fleet-two-row-redesign-and-then-some",
        ] {
            for (w, h) in [(170u16, 40u16), (200, 50), (160, 40), (100, 30)] {
                let p = plan(w, h, 1, width_of(name), None);
                let out = drawn(&session(name, "busy"), &p, 0, false).join("\n");
                assert!(out.contains(name), "{w}x{h}: {out}");
            }
        }
    }

    // …and the prompt gets a line of its own, so it stops being a 20-column stub.
    #[test]
    fn the_prompt_gets_the_second_line_whole() {
        let mut s = session("x", "idle");
        let title = "profile the search index re-render storm and work out which selector \
                     recomputes on every keystroke";
        s.title = Some(title.into());
        let p = plan(170, 40, 1, 12, None);
        let out = drawn(&s, &p, 0, false);
        assert!(out[1].contains(&title[..90]), "{out:?}");
    }

    #[test]
    fn compact_mode_draws_exactly_one_line() {
        let p = plan(170, 40, 1, 22, Some(false));
        assert_eq!(p.lines, 1);
        let s = session("cache-warmup", "busy");
        let ctx = ctx_of(&s, &p);
        assert_eq!(session_item(&s, &p, &ctx, 0, false).height(), 1);
        let out = drawn(&s, &p, 0, false);
        assert_eq!(out.len(), 1);
        assert!(out[0].contains("cache-warmup"), "{out:?}");
        assert!(out[0].contains("⧉ api-server"), "{out:?}");
        assert!(out[0].contains("make the cache warmup"), "{out:?}");
    }

    // The tmux session is the identifier people actually name after the job; the
    // window name is almost always "zsh".
    #[test]
    fn the_tmux_session_is_on_the_row_at_every_size() {
        let check = |w, h, forced| {
            let p = plan(w, h, 1, 12, forced);
            let out = drawn(&session("auth", "busy"), &p, 0, false).join("\n");
            assert!(out.contains("api-server"), "{w}x{h} {forced:?}: {out:?}");
            assert!(out.contains('⧉'), "{w}x{h} {forced:?}: {out:?}");
        };
        // Two-row items keep the terminal down to the 28-column floor…
        for (w, h) in SIZES.iter().take(8).copied() {
            check(w, h, None);
        }
        // …and compact rows keep it as long as the columns exist at all: below
        // ~44 a single line is name, age and prompt, nothing more.
        for (w, h) in SIZES.iter().take(6).copied() {
            check(w, h, Some(false));
        }
    }

    // A desktop pane has the columns to spell the terminal out; truncating
    // `✳ Nightly bac…` was the old layout paying for a squeezed prompt column.
    #[test]
    fn a_desktop_pane_does_not_truncate_the_terminal_label() {
        let mut s = session("x", "idle");
        s.tmux_session = Some("nightly-backup-rotation-v2".into());
        for (w, h) in [(170u16, 40u16), (200, 50)] {
            let p = plan(w, h, 1, 12, None);
            let out = drawn(&s, &p, 0, false).join("\n");
            assert!(
                out.contains("⧉ nightly-backup-rotation-v2"),
                "{w}x{h}: {out}"
            );
        }
    }

    #[test]
    fn waiting_sessions_say_so_in_words() {
        for (w, h) in [(170u16, 40u16), (60, 24), (44, 16)] {
            let p = plan(w, h, 1, 22, None);
            let out = drawn(&session("docs-refresh", "waiting"), &p, 0, false).join("\n");
            assert!(out.contains("needs you"), "{w}x{h}: {out:?}");
            assert!(out.contains('⏸'), "{w}x{h}: {out:?}");
        }
    }

    #[test]
    fn the_gutter_numbers_the_first_ten_rows() {
        let p = plan(60, 24, 12, 22, None);
        assert!(p.gutter);
        let s = session("x", "idle");
        assert!(drawn(&s, &p, 0, false)[0].starts_with(" 1 "));
        assert!(drawn(&s, &p, 0, true)[0].starts_with("▸1 "));
        assert!(drawn(&s, &p, 8, false)[0].starts_with(" 9 "));
        assert!(drawn(&s, &p, 9, false)[0].starts_with(" 0 "));
        // Past the tenth there is no digit to offer.
        assert!(drawn(&s, &p, 10, false)[0].starts_with("   "));
        // …and the digits survive the compact override, which is the mode where
        // fifteen sessions are on screen and jumping matters most.
        let p = plan(60, 24, 12, 22, Some(false));
        assert!(drawn(&s, &p, 4, false)[0].starts_with(" 5 "));
    }

    // Line 2 starts under the name, not under the caret: the indent is what makes
    // the pair read as one item.
    #[test]
    fn the_context_line_is_indented_under_the_name() {
        let p = plan(170, 40, 1, 12, None);
        let out = drawn(&session("x", "idle"), &p, 0, false);
        assert!(out[1].starts_with("     "), "{out:?}");
        assert!(!out[1].starts_with("      "), "{out:?}");
    }

    #[test]
    fn control_characters_in_a_title_are_stripped() {
        let mut s = session("x", "busy");
        s.title = Some("first\nsecond\u{1b}[31m\u{7}third".into());
        let p = plan(170, 40, 1, 12, None);
        let out = drawn(&s, &p, 0, false).join("\n");
        assert!(!out.contains('\u{1b}') && !out.contains('\u{7}'), "{out:?}");
        assert!(out.contains("first second"), "{out:?}");
    }

    #[test]
    fn sessions_with_no_terminal_still_render() {
        let mut s = session("orphan", "idle");
        s.tmux_session = None;
        s.tab = None;
        s.cwd = None;
        s.title = None;
        for (w, h) in [(170u16, 40u16), (60, 24), (24, 10)] {
            let p = plan(w, h, 1, 6, None);
            let ctx = ctx_of(&s, &p);
            for line in session_lines(&s, &p, &ctx, 0, true) {
                assert_eq!(line.width(), p.width);
            }
        }
        // The prompt is missing, not the row: say so rather than draw a blank.
        let p = plan(170, 40, 1, 6, None);
        assert!(drawn(&s, &p, 0, false)[1].contains("(no prompt)"));
    }

    // --- the directory -------------------------------------------------------

    /// An absolute path under the *real* home directory: `home_rel` only rewrites
    /// that one, and these tests are about what a `~`-relative row looks like.
    fn home(rest: &str) -> String {
        format!("{}/{rest}", dirs::home_dir().unwrap().display())
    }

    fn at(dir: &str, tmux: &str) -> Session {
        let mut s = session("x", "idle");
        s.cwd = Some(dir.into());
        s.tmux_session = Some(tmux.into());
        s
    }

    // The fleet's common base is a quorum, not a coincidence: ten sessions under
    // `~/Code/app` name it, three sessions in three repos don't.
    #[test]
    fn the_common_base_needs_a_quorum() {
        let fleet = |dirs: &[&str]| {
            let rows: Vec<Session> = dirs.iter().map(|d| at(d, "t")).collect();
            base_dir(&rows)
        };
        assert_eq!(
            fleet(&[&home("Code/app"), &home("Code/app/branches/a")]).as_deref(),
            Some("~/Code/app")
        );
        // A majority under `branches/` pushes the base one level deeper.
        assert_eq!(
            fleet(&[
                &home("Code/app/branches/a"),
                &home("Code/app/branches/b"),
                &home("Code/app"),
            ])
            .as_deref(),
            Some("~/Code/app/branches")
        );
        // Unrelated repos: there is no base to claim, so every row keeps its path.
        assert_eq!(fleet(&["/tmp/a", "/var/b", "/opt/c"]), None);
        // One session is not a fleet.
        assert_eq!(fleet(&[&home("Code/app")]), None);
        assert_eq!(fleet(&[]), None);
    }

    #[test]
    fn a_session_at_the_common_base_draws_no_path_at_all() {
        let p = plan(170, 40, 3, 22, None);
        let rows = vec![
            at(&home("Code/app"), "review"),
            at(&home("Code/app"), "app-2"),
            at(&home("Code/app/branches/frontend-nav"), "frontend-nav"),
            at(&home("Code/notes"), "notes-repo"),
        ];
        let ctx = FleetCtx::of(&rows, &p);
        assert_eq!(ctx.base.as_deref(), Some("~/Code/app"));
        // At the base: the block title already says it.
        assert_eq!(location(&rows[0], &p, &ctx), None);
        // Under it, and the last component is the terminal name again — so only
        // the directory that contains it is left to say.
        assert_eq!(location(&rows[2], &p, &ctx).as_deref(), Some("branches/"));
        // Outside it: the whole path, because *that* is the news.
        assert_eq!(
            location(&rows[3], &p, &ctx).as_deref(),
            Some("~/Code/notes")
        );
        // Under it, and saying something the terminal doesn't.
        let elsewhere = at(&home("Code/app/clones/api"), "review");
        assert_eq!(
            location(&elsewhere, &p, &ctx).as_deref(),
            Some("clones/api")
        );
        // Directly under the base with nothing but the terminal name to add.
        let sibling = at(&home("Code/app/review"), "review");
        assert_eq!(location(&sibling, &p, &ctx), None);
        // Two sessions with the same terminal name in different directories stay
        // told apart: the containers differ, so the containers are drawn.
        let worktree = at(&home("Code/app/branches/api"), "api");
        let repo = at(&home("Code/app/clones/api"), "api");
        assert_eq!(location(&worktree, &p, &ctx).as_deref(), Some("branches/"));
        assert_eq!(location(&repo, &p, &ctx).as_deref(), Some("clones/"));
    }

    #[test]
    fn the_path_a_row_draws_lands_on_the_row() {
        let p = plan(170, 40, 4, 22, None);
        let rows = vec![
            at(&home("Code/app"), "review"),
            at(&home("Code/app"), "app-2"),
            at(&home("Code/notes"), "notes-repo"),
        ];
        let ctx = FleetCtx::of(&rows, &p);
        let line = |i: usize| {
            let lines = session_lines(&rows[i], &p, &ctx, i, false);
            lines
                .iter()
                .map(|l| {
                    l.spans
                        .iter()
                        .map(|s| s.content.as_ref())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
        };
        assert!(line(2)[1].contains("~/Code/notes"), "{:?}", line(2));
        assert!(!line(0)[1].contains("Code"), "{:?}", line(0));
    }

    // A phone pane elides the base too — its block title has room for ten columns
    // plus a path, and `app` on all eleven rows was the noise the whole rule
    // exists to remove.
    #[test]
    fn a_phone_pane_elides_the_common_base_as_well() {
        let p = plan(44, 16, 3, 22, None);
        assert!(p.show_base && p.show_cwd);
        let rows = vec![
            at(&home("Code/app"), "review"),
            at(&home("Code/app"), "app-2"),
            at(&home("Code/app/branches/frontend-nav"), "frontend-nav"),
            at(&home("Code/notes"), "notes"),
        ];
        let ctx = FleetCtx::of(&rows, &p);
        assert_eq!(ctx.base.as_deref(), Some("~/Code/app"));
        assert_eq!(location(&rows[0], &p, &ctx), None);
        assert_eq!(location(&rows[2], &p, &ctx).as_deref(), Some("branches/"));
        // A session that really is elsewhere still says so, at any width.
        assert_eq!(
            location(&rows[3], &p, &ctx).as_deref(),
            Some("~/Code/notes")
        );
    }

    // …but only while the title can actually print it: a base too long for the
    // pane leaves the rows drawing whole paths, never bare remainders.
    #[test]
    fn a_base_the_title_cannot_print_is_not_elided() {
        let deep = home("Code/app/branches/a-very-long-worktree-directory-name");
        let rows = vec![at(&deep, "one"), at(&deep, "two")];
        let wide = plan(170, 40, 2, 22, None);
        assert!(FleetCtx::of(&rows, &wide).base.is_some());

        let narrow = plan(44, 16, 2, 22, None);
        let ctx = FleetCtx::of(&rows, &narrow);
        assert_eq!(ctx.base, None);
        assert!(
            location(&rows[0], &narrow, &ctx)
                .as_deref()
                .is_some_and(|l| l.starts_with("~/Code/app")),
            "a row must not draw a remainder the title never explained"
        );
        // Tiny panes draw no paths at all, so they claim no base either.
        let tiny = plan(24, 10, 2, 22, None);
        assert_eq!(FleetCtx::of(&rows, &tiny).base, None);
    }

    // The phone item's three lines, one job each — and the prompt getting a whole
    // line is the point of the shape.
    #[test]
    fn a_phone_item_puts_identity_terminal_and_prompt_on_three_lines() {
        let mut s = session("app-03", "idle");
        s.tmux_session = None;
        s.tab = Some("✳ Nightly backup rotation (setup)".into());
        s.title = Some(
            "Set up nightly backups for me. Read the backup notes first. Can you rotate them \
             the same way the laptop does?"
                .into(),
        );
        for (w, h) in [(44u16, 16u16), (50, 20), (60, 24)] {
            let p = plan(w, h, 1, 12, None);
            assert_eq!(p.lines, 3, "{w}x{h}");
            let out = drawn(&s, &p, 0, false);
            assert_eq!(out.len(), 3, "{w}x{h}");
            assert!(out[0].contains("app-03"), "{w}x{h}: {out:?}");
            assert!(out[1].contains("▣ ✳ Nightly backup"), "{w}x{h}: {out:?}");
            assert!(
                out[2].contains("Set up nightly backups"),
                "{w}x{h}: {out:?}"
            );
            // The prompt line is the widest thing in the item: it gets everything
            // past the indent, where sharing line 2 left it a fifteen-column stub.
            let prompt = out[2].trim_end().chars().count() - 5;
            assert!(prompt >= p.width - 6, "{w}x{h}: prompt got {prompt}");
        }
    }

    // Under 40 columns line 1 drops the status word to keep the name readable, so
    // `needs you` has to survive on line 2 — it's the row the user is hunting for.
    #[test]
    fn a_split_phone_pane_keeps_the_status_word_on_line_two() {
        let p = plan(32, 12, 1, 12, None);
        assert!(!p.show_status_word);
        let out = drawn(&session("docs-refresh", "waiting"), &p, 0, false);
        assert!(out[0].contains("docs-refresh"), "{out:?}");
        assert!(!out[0].contains("needs you"), "{out:?}");
        assert!(out[1].contains("needs you"), "{out:?}");
        // …and the name is what got the room back.
        let out = drawn(&session("resolve-css-conflicts", "idle"), &p, 0, false);
        assert!(out[0].contains("resolve-css-conflict"), "{out:?}");
    }

    // The dedup compares against the label the row draws — `terminal_label`
    // shows tmux *or* tab, tmux winning — not against both fields.
    #[test]
    fn the_cwd_is_dropped_only_when_it_repeats_the_drawn_terminal() {
        // Exact repeat: the tmux session is already on the row.
        assert!(echoes_terminal("api-server", "⧉ api-server"));
        // Under-suppression this used to miss: `⧉ app-3  app`.
        assert!(echoes_terminal("app", "⧉ app-3"));
        assert!(echoes_terminal("app-3", "⧉ app"));
        assert!(echoes_terminal("App", "⧉ app-12"));
        // iTerm rows dedup against the tab, which is what they draw.
        assert!(echoes_terminal("logs", "▣ logs"));

        // Over-suppression this used to cause: the row draws the tmux session,
        // so a cwd matching only the (undrawn) window name is not a duplicate.
        assert!(!echoes_terminal("zsh", "⧉ api-server"));
        assert!(!echoes_terminal("frontend", "⧉ api-server"));
        // Nothing to compare against.
        assert!(!echoes_terminal("app", "-"));
        assert!(!echoes_terminal("", "⧉ app"));
    }

    #[test]
    fn a_cwd_that_repeats_the_tmux_session_is_not_drawn_twice() {
        let p = plan(60, 24, 1, 22, None);
        assert!(p.lines >= 2 && p.show_cwd);

        // tmux `app-3` in ~/Code/app — the last component adds nothing.
        let mut s = session("statusline", "idle");
        s.tmux_session = Some("app-3".into());
        s.cwd = Some(home("Code/app"));
        let out = drawn(&s, &p, 0, false).join("\n");
        assert!(out.contains("⧉ app-3"), "{out:?}");
        assert!(!out.contains("app-3  app"), "{out:?}");

        // …but a cwd that says something *is* drawn, even when the window name
        // (which the row never shows) happens to match it.
        let mut s = session("statusline", "idle");
        s.tmux_session = Some("api-server".into());
        s.tab = Some("tools".into());
        s.cwd = Some("/home/x/Code/tools".into());
        let out = drawn(&s, &p, 0, false).join("\n");
        assert!(out.contains("⧉ api-server"), "{out:?}");
        assert!(out.contains("tools"), "{out:?}");
    }

    #[test]
    fn wide_names_do_not_shift_the_columns() {
        let s = session("测试-会话-🚀", "busy");
        assert_eq!(width_of("测试-会话-🚀"), 12);
        for (w, h) in SIZES {
            for forced in [None, Some(false)] {
                let p = plan(w, h, 1, 12, forced);
                let ctx = ctx_of(&s, &p);
                for line in session_lines(&s, &p, &ctx, 0, false) {
                    assert_eq!(line.width(), p.width, "{w}x{h} {forced:?}: {line:?}");
                }
            }
        }
    }

    // The terminal cell is as wide as the fleet's widest label, capped, so the
    // status words behind it line up instead of stepping per row.
    #[test]
    fn the_terminal_cell_is_sized_to_the_fleet() {
        let p = plan(170, 40, 3, 22, None);
        let rows = vec![at("/tmp/a", "api"), at("/tmp/b", "frontend-nav")];
        assert_eq!(
            FleetCtx::of(&rows, &p).term_cell,
            width_of("⧉ frontend-nav")
        );
        // …and never wider than the plan allows.
        let rows = vec![at("/tmp/a", &"x".repeat(80)), at("/tmp/b", "y")];
        assert_eq!(FleetCtx::of(&rows, &p).term_cell, p.term_cell);
        // An empty fleet has no labels to measure.
        assert_eq!(FleetCtx::of(&[], &p).term_cell, 3);
    }
}
