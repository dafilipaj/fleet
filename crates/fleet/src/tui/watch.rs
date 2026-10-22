//! `watch` — live supervision. Polls the registry, detects transitions, fires
//! macOS notifications on finished/stuck, and (on a TTY) renders a ratatui dashboard.
//!
//! The dashboard is used from a 200-column desktop terminal *and* from a phone
//! over mosh, so the interesting logic — key mapping, layout, row assembly — lives
//! in [`crate::tui::dash`] as pure functions with tests. What's left here is the loop.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use crossterm::cursor::Show;
use crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, List, ListState, Paragraph, Wrap};
use serde::{Deserialize, Serialize};

use crate::cli::commands;
use crate::cli::render::{ago, width_of};
use crate::core::backend;
use crate::core::discovery::{Session, claude_home, discover, enrich_iterm_tabs, is_fixture};
use crate::core::naming::{GenOpts, NameCache, NameJob, NameMsg, NamePool, NameSource, Purpose};
use crate::error::{Error, Result};
use crate::tui::dash::keys::{self, Action, RenameAction};
use crate::tui::dash::layout::{self as dashlayout, Density, Plan};
use crate::tui::dash::rows::{self, FleetCtx, session_item};
use crate::tui::notify::notify;

const EVENT_CAP: usize = 20;

/// A session sitting idle on a permission/confirmation dialog is blocked on you.
fn awaiting(text: &str) -> bool {
    const MARKERS: [&str; 5] = [
        "Do you want to proceed",
        "Would you like to",
        "❯ 1. Yes",
        "❯ 1. Allow",
        "1. Yes,",
    ];
    MARKERS.iter().any(|m| text.contains(m))
}

#[derive(Default, Clone, Serialize, Deserialize)]
struct Prev {
    status: String,
    #[serde(default)]
    stuck_notified: bool,
}

type State = HashMap<String, Prev>;

fn state_path() -> PathBuf {
    claude_home().join("fleet-watch-state.json")
}
fn load_state() -> State {
    // Fixture mode is a demo: it must not inherit — or leave behind — transition
    // state for sessions that never existed.
    if is_fixture() {
        return State::default();
    }
    std::fs::read_to_string(state_path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}
fn save_state(s: &State) {
    if is_fixture() {
        return;
    }
    if let Ok(t) = serde_json::to_string(s) {
        let _ = std::fs::write(state_path(), t);
    }
}

/// First 8 characters of a session key. Chars, not bytes: ids come from JSON
/// (the fixture's are user-supplied), and byte-slicing splits a codepoint.
fn short_key(key: &str) -> String {
    key.chars().take(8).collect()
}

struct Ev {
    icon: &'static str,
    time: String,
    msg: String,
}

impl Ev {
    fn now(icon: &'static str, msg: String) -> Self {
        Ev {
            icon,
            time: chrono::Local::now().format("%H:%M:%S").to_string(),
            msg,
        }
    }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// One supervision pass: update `state`, fire notifications, return (rows, new events).
fn tick(state: &mut State, stuck_secs: i64) -> (Vec<Session>, Vec<Ev>) {
    let mut rows = discover();
    enrich_iterm_tabs(&mut rows);
    let mut evs = Vec::new();
    let time = chrono::Local::now().format("%H:%M:%S").to_string();
    let mut push = |icon: &'static str, msg: String| {
        evs.push(Ev {
            icon,
            time: time.clone(),
            msg,
        })
    };

    let mut seen = std::collections::HashSet::new();
    for r in &rows {
        let key = r.key();
        seen.insert(key.clone());
        let prev = state.get(&key).cloned().unwrap_or_default();
        let label = r.label();
        let known = state.contains_key(&key);

        if prev.status == "busy" && r.is_idle() {
            let t = r.title.as_deref().unwrap_or("");
            push(
                "✅",
                format!(
                    "{label} finished — {}",
                    t.chars().take(40).collect::<String>()
                ),
            );
            notify(
                "Session finished",
                &format!("{label}: {}", t.chars().take(60).collect::<String>()),
            );
        } else if known && prev.status != "waiting" && r.is_waiting() {
            // The registry tells us this outright — no screen-scraping needed.
            let what = r.waiting_for.as_deref().unwrap_or("input needed");
            push("⏸", format!("{label} needs you — {what}"));
            notify("Session needs you", &format!("{label}: {what}"));
        } else if known && prev.status != "busy" && r.status == "busy" {
            push("▶", format!("{label} working"));
        } else if !known {
            push("＋", format!("{label} appeared"));
        }

        // Stuck: idle past the threshold and blocked on a prompt — check once per idle episode.
        let idle_secs = r.updated_at.map(|u| (now_ms() - u) / 1000).unwrap_or(0);
        let mut stuck_notified = prev.stuck_notified && r.is_idle();
        // `is_fixture` short-circuits before `peek`: a canned fleet's handles are
        // fabricated, and shelling out to tmux/AppleScript with them is exactly
        // what a demo mode must not do.
        if r.is_idle() && idle_secs > stuck_secs && !stuck_notified && !is_fixture() {
            let screen = backend::peek(r).unwrap_or_default();
            if awaiting(&screen) {
                push(
                    "⚠",
                    format!("{label} waiting for your input ({})", ago(r.updated_at)),
                );
                notify(
                    "Session waiting",
                    &format!("{label} is blocked on a prompt"),
                );
            }
            stuck_notified = true;
        }
        state.insert(
            key,
            Prev {
                status: r.status.clone(),
                stuck_notified: if r.status == "busy" {
                    false
                } else {
                    stuck_notified
                },
            },
        );
    }
    for key in state.keys().cloned().collect::<Vec<_>>() {
        if !seen.contains(&key) {
            push("✕", format!("{} closed", short_key(&key)));
            state.remove(&key);
        }
    }
    save_state(state);
    // A session blocked on the user is the one thing worth seeing first; sort_by_key
    // is stable, so within each group the registry's recency order survives.
    rows.sort_by_key(|r| !r.is_waiting());
    (rows, evs)
}

/// How `watch` was asked to run. `rows`/`mouse` are three-state: absent means
/// "whatever the config says".
#[derive(Default, Clone, Copy)]
pub struct WatchOpts {
    pub interval_secs: u64,
    pub stuck_secs: i64,
    pub quiet: bool,
    /// `None` = consult config; `Some(None)` = auto; `Some(Some(b))` = forced.
    pub rows: Option<Option<bool>>,
    /// `None` = consult config (which defaults to on).
    pub mouse: Option<bool>,
}

pub fn run(o: WatchOpts) -> Result<()> {
    let tui = std::io::IsTerminal::is_terminal(&io::stdout()) && !o.quiet;
    if tui {
        run_tui(o)
    } else {
        run_quiet(o.interval_secs, o.stuck_secs)
    }
}

fn run_quiet(interval_secs: u64, stuck_secs: i64) -> Result<()> {
    println!("fleet watch running (notify-only, every {interval_secs}s). Ctrl-c to stop.");
    let mut state = load_state();
    loop {
        let (_, evs) = tick(&mut state, stuck_secs);
        for e in evs {
            println!("{} {}  {}", e.icon, e.time, e.msg);
        }
        std::thread::sleep(Duration::from_secs(interval_secs));
    }
}

/// Restores the terminal on drop, so an early `?` return — or a panic — can't
/// leave it wedged in raw mode with the mouse still captured.
///
/// Teardown is the exact reverse of setup, and it shows the cursor again:
/// ratatui's `draw` hides it and nothing else ever brings it back, which leaves
/// the shell you return to with an invisible caret.
struct TermGuard {
    mouse: bool,
}
impl Drop for TermGuard {
    fn drop(&mut self) {
        if self.mouse {
            let _ = execute!(io::stdout(), DisableMouseCapture);
        }
        let _ = execute!(io::stdout(), LeaveAlternateScreen, Show);
        let _ = disable_raw_mode();
    }
}

/// `"1"`/`"2"`/`"auto"` from the config, folded with the command-line override.
///
/// `2` and `auto` both mean the full item — two lines on a desktop, three on a
/// phone — so `1`, the compact single line `z` toggles into, is the only value
/// that changes anything today.
fn resolve_rows(flag: Option<Option<bool>>) -> Option<bool> {
    if let Some(explicit) = flag {
        return explicit;
    }
    match commands::ui_config().rows.as_deref() {
        Some("1") => Some(false),
        Some("2") => Some(true),
        _ => None,
    }
}

fn rows_setting(force_two_row: Option<bool>) -> &'static str {
    match force_two_row {
        Some(true) => "2",
        Some(false) => "1",
        None => "auto",
    }
}

/// Everything the dashboard keeps between frames.
struct Dash {
    list: ListState,
    help: bool,
    /// `None` = size-driven, `Some(b)` = the `z` toggle / `--rows` flag.
    force_two_row: Option<bool>,
    /// `None` = size-driven, `Some(b)` = the `e` toggle.
    events: Option<bool>,
    /// Recorded on every draw so a click can be mapped back to a row. Zeroed
    /// while the help overlay is up, so a tap on the overlay can't select and
    /// focus the row underneath it.
    list_area: Rect,
    item_height: u16,
    /// Whether the last frame actually drew the events pane — `e` toggles
    /// relative to what's on screen, not to the stored override.
    events_shown: bool,
    /// Whether the last frame was a Tiny pane, where the row-mode override is
    /// ignored — so `z` doesn't persist a setting the user never saw applied.
    tiny: bool,
    /// `Some` while naming jobs are in flight — drives the header spinner.
    naming: Option<NameProgress>,
    /// Frame counter, only ever used to advance the spinner.
    tick: u64,
    /// Sessions a reboot left dormant on this machine (`fleet restore`), re-read every poll.
    dormant: usize,
}

/// An open rename buffer, pinned to the session it was started on.
struct Rename {
    /// The session's stable key, *not* its row index. Rows are re-polled and
    /// re-sorted while the buffer is open, so an index would happily send
    /// `/rename` into whatever had drifted into that slot.
    key: String,
    buf: String,
}

/// Resolve a pinned rename against the current rows and send it. `None` = the
/// buffer was empty, so there is nothing to say about it.
///
/// `sync_tmux` carries the `[naming] sync_tmux` setting through: a confirmed
/// rename is also where the session's tmux session name gets brought in line.
/// The third field says whether `/rename` actually went out: a session that was
/// waiting on a prompt is *held*, and its pending suggestion has to survive for the next
/// attempt rather than being consumed by one that never happened.
fn commit_rename(
    r: &Rename,
    rows: &[Session],
    sync_tmux: bool,
) -> Option<(&'static str, String, bool)> {
    if r.buf.trim().is_empty() {
        return None;
    }
    // The CLI verb validates and crops; the typed buffer has to as well, or a
    // 300-character line goes out as `/rename <300 chars>` and lands as a
    // 300-character tmux session name.
    let name = match commands::clean_name(&r.buf) {
        Ok(n) => n,
        Err(e) => return Some(("✕", format!("rename dropped: {e}"), false)),
    };
    let name = name.as_str();
    let Some(s) = rows.iter().find(|s| s.key() == r.key) else {
        return Some((
            "✕",
            format!("rename dropped — session {} is gone", short_key(&r.key)),
            true,
        ));
    };
    let opts = commands::RenameOpts {
        sync_tmux,
        // The dashboard has no modifier for it; `tb-fleet rename --force` is the
        // deliberate path for a session that is always waiting.
        force: false,
        require_empty: false,
    };
    Some(match commands::apply_rename(s, name, opts) {
        Ok(outcome @ commands::RenameOutcome::Sent(_)) => match outcome.tmux_note() {
            None => ("✎", format!("{} → {name}", s.headline()), true),
            Some(note) => ("✎", format!("{} → {name}  ({note})", s.headline()), true),
        },
        Ok(commands::RenameOutcome::Held(_, why)) => ("⏸", why, false),
        Err(e) => ("✕", format!("{} → {name} failed: {e}", s.label()), false),
    })
}

// --- naming ------------------------------------------------------------------

/// Header progress for an in-flight naming pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct NameProgress {
    done: usize,
    total: usize,
}

/// Everything the naming feature keeps between frames.
///
/// Two things come out of the same generator here. **Titles** are what every row
/// draws on its first line, and they are automatic: the dashboard titles the
/// whole fleet in the background so a row says what its session is doing without
/// anyone pressing anything. **Suggestions** are the `N` flow — the same title,
/// offered as a new Claude session *name*, which only a confirmed `/rename`
/// applies. A title is display state and costs nothing to be wrong about; a name
/// is typed into a live session, so it keeps its confirmation step.
struct Naming {
    pool: Option<(NamePool, Receiver<NameMsg>)>,
    /// Generated titles by session key — the headline source for every row.
    titles: HashMap<String, String>,
    /// What each title was generated from (the session's first prompt), so a
    /// title made before the user had said anything is regenerated once they do.
    /// The transcript's first prompt is the one input that lands *after* a
    /// session shows up in the registry.
    titled_from: HashMap<String, Option<String>>,
    /// Names generated this run, each still awaiting the user's ⏎.
    suggestions: HashMap<String, String>,
    /// In flight, so a second `N` on the same row doesn't queue it twice.
    queued: HashSet<String>,
    /// Keys the user asked to be *offered* while a background title for them was
    /// already in flight. The job in the channel can't be amended, so the answer
    /// is upgraded on arrival instead — without this, `N` pressed during the
    /// startup titling pass looked like it did nothing at all.
    wanted: HashMap<String, Purpose>,
    /// Already fell back to the heuristic once — the model is having a bad day
    /// and re-queueing it every keypress just burns time.
    exhausted: HashSet<String>,
    pending: usize,
    total: usize,
    enabled: bool,
    /// Title the fleet without being asked. `[naming] auto_title = false` turns
    /// it off, and rows fall back to the Claude session name.
    auto: bool,
    model: String,
    /// The name cache as it was on disk at startup, read once. Seeds titles for
    /// sessions the workers haven't answered for yet.
    disk: NameCache,
}

impl Naming {
    fn new(cfg: &commands::NamingConfig) -> Self {
        // Last run's titles, so the dashboard opens *already* reading as itself
        // instead of showing a screen of `app-9d` until the first model call
        // lands. Every one of them is re-checked by the titling pass below.
        let disk = NameCache::load();
        Naming {
            pool: None,
            titles: HashMap::new(),
            titled_from: HashMap::new(),
            suggestions: HashMap::new(),
            queued: HashSet::new(),
            wanted: HashMap::new(),
            exhausted: HashSet::new(),
            pending: 0,
            total: 0,
            // A canned fleet must never reach the model: its sessions are
            // fabricated, so every call would be paid for and meaningless.
            enabled: cfg.enabled() && !is_fixture(),
            auto: cfg.auto_title(),
            model: cfg.model(),
            disk,
        }
    }

    /// Title everything the fleet is showing, quietly. Runs after every poll and
    /// is a no-op once the fleet is titled.
    fn autotitle(&mut self, rows: &[Session]) {
        if !self.auto || !self.enabled {
            return;
        }
        self.adopt_disk_titles(rows);
        for key in self.due_for_title(rows) {
            let Some(s) = rows.iter().find(|s| s.key() == key).cloned() else {
                continue;
            };
            if self.spawn(&s, Purpose::Title) {
                self.titled_from.insert(key, s.title.clone());
            }
        }
    }

    /// Take last run's titles for rows that have none yet, so the dashboard reads
    /// as itself from the first frame instead of showing `app-9d` until a model
    /// call lands. Adopted titles are still queued below — the worker's cache
    /// read confirms them for free when the input hasn't moved.
    fn adopt_disk_titles(&mut self, rows: &[Session]) {
        for s in rows {
            let key = s.key();
            if self.titles.contains_key(&key) {
                continue;
            }
            if let Some(name) = self.disk.title(&key) {
                self.titles.insert(key, name.to_string());
            }
        }
    }

    /// Which rows are due a title.
    ///
    /// A session is due when this run hasn't titled it, or when its first prompt
    /// has changed since the title it has was generated from — the transcript's
    /// first prompt is the one naming input that lands *after* the session shows
    /// up in the registry, so a session titled at boot from a directory alone
    /// gets a real title the moment the user says what they want.
    ///
    /// Pure, so the policy can be exercised without starting a pool or paying
    /// for a model call.
    fn due_for_title(&self, rows: &[Session]) -> Vec<String> {
        rows.iter()
            .map(|s| (s.key(), s))
            .filter(|(key, s)| {
                !self.queued.contains(key)
                    && !self.exhausted.contains(key)
                    // Titled this run already; only a changed prompt re-opens it.
                    && self.titled_from.get(key).is_none_or(|from| from != &s.title)
            })
            .map(|(key, _)| key)
            .collect()
    }

    /// Queue one session, starting the pool if this is the first job.
    ///
    /// The pool is started on demand rather than at boot because it isn't always
    /// wanted: `[naming] enabled = false` and fixture mode never get here, and
    /// two idle threads plus a channel are not free.
    fn spawn(&mut self, s: &Session, purpose: Purpose) -> bool {
        if self.pool.is_none() {
            // `enabled` is the caller's to check; the TUI has no refresh key,
            // `tb-fleet name --refresh` does.
            self.pool = Some(NamePool::start(self.model.clone(), GenOpts::llm()));
        }
        let Some((pool, _)) = &self.pool else {
            return false;
        };
        let key = s.key();
        if !pool.enqueue(NameJob {
            key: key.clone(),
            session: s.clone(),
            purpose,
        }) {
            return false;
        }
        self.queued.insert(key);
        self.pending += 1;
        self.total += 1;
        true
    }

    fn progress(&self) -> Option<NameProgress> {
        (self.pending > 0).then_some(NameProgress {
            done: self.total.saturating_sub(self.pending),
            total: self.total,
        })
    }

    /// Queue `keys` for naming on the user's behalf (`N` / `Ctrl-N`). Returns the
    /// events to log, plus the keys whose name is already in hand and can be
    /// offered straight away.
    ///
    /// A title generated by the background pass counts as in hand: by the time
    /// anyone presses `N` the fleet is usually titled already, so the ordinary
    /// `N` costs nothing and never queues behind a titling backlog.
    fn request(
        &mut self,
        rows: &[Session],
        keys: &[String],
        purpose: Purpose,
    ) -> (Vec<Ev>, Vec<String>) {
        let mut evs = Vec::new();
        let mut ready = Vec::new();
        if keys.is_empty() {
            evs.push(Ev::now("·", "nothing to name".into()));
            return (evs, ready);
        }
        if !self.enabled {
            evs.push(Ev::now(
                "·",
                "naming is off here (fixture mode, or [naming] enabled = false)".into(),
            ));
            return (evs, ready);
        }
        let mut queued_now = 0usize;
        for key in keys {
            if self.titles.contains_key(key) {
                self.suggestions
                    .entry(key.clone())
                    .or_insert_with(|| self.titles[key].clone());
                ready.push(key.clone());
                continue;
            }
            if self.queued.contains(key) {
                self.wanted.insert(key.clone(), purpose);
                continue;
            }
            let Some(s) = rows.iter().find(|s| &s.key() == key) else {
                continue;
            };
            if self.exhausted.contains(key) {
                evs.push(Ev::now(
                    "·",
                    format!("{} already fell back once this run", s.label()),
                ));
                continue;
            }
            let s = s.clone();
            if self.spawn(&s, purpose) {
                queued_now += 1;
            }
        }
        if queued_now > 0 {
            evs.push(Ev::now(
                "…",
                format!(
                    "naming {queued_now} session{}",
                    if queued_now == 1 { "" } else { "s" }
                ),
            ));
        }
        (evs, ready)
    }

    /// Everything the workers have answered since the last frame. Never blocks —
    /// this runs at the top of the draw loop.
    fn drain(&mut self) -> Vec<NameMsg> {
        let mut out = Vec::new();
        if let Some((_, rx)) = &self.pool {
            while let Ok(msg) = rx.try_recv() {
                out.push(msg);
            }
        }
        out
    }

    /// What the answer to `key` should do now it's here: whatever the job was
    /// queued as, unless the user asked for more while it was in flight.
    fn purpose_of(&mut self, key: &str, queued_as: Purpose) -> Purpose {
        match self.wanted.remove(key) {
            Some(upgraded) if !queued_as.is_interactive() => upgraded,
            _ => queued_as,
        }
    }

    /// Book-keeping for one answered job.
    fn settle(&mut self, key: &str, fell_back: bool) {
        self.queued.remove(key);
        self.pending = self.pending.saturating_sub(1);
        if self.pending == 0 {
            self.total = 0;
        }
        if fell_back {
            self.exhausted.insert(key.to_string());
        }
    }
}

/// Bring a suggestion to the user.
///
/// Prefills the pinned rename buffer when the session can actually take a
/// `/rename` — and holds it, with a reason, when it can't. A session sitting on
/// a permission prompt would read the rename as its answer (see
/// [`crate::core::title::Hold`]), and a vanished session is dropped rather than
/// retargeted at whoever took its row.
fn offer(key: &str, name: &str, rows: &[Session], renaming: &mut Option<Rename>, bulk: bool) -> Ev {
    let Some(s) = rows.iter().find(|s| s.key() == key) else {
        return Ev::now(
            "✕",
            format!("suggestion dropped — session {} is gone", short_key(key)),
        );
    };
    let label = s.label();
    if crate::core::title::hold(s).is_some() {
        return Ev::now(
            "⏸",
            format!("{label} → {name} held ({}) — N when it's idle", s.status),
        );
    }
    // A bulk pass must not open twelve buffers in a row, and a buffer already
    // open belongs to whatever the user is typing right now.
    if bulk || renaming.is_some() {
        return Ev::now("✎", format!("{label} → {name} — press N to apply"));
    }
    *renaming = Some(Rename {
        key: key.to_string(),
        buf: name.to_string(),
    });
    Ev::now("✎", format!("{label} → {name}? ⏎ applies"))
}

fn spinner_frame(tick: u64) -> &'static str {
    const FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    FRAMES[(tick as usize) % FRAMES.len()]
}

/// What one terminal event means to an open rename buffer.
///
/// Everything that isn't a key press maps to [`RenameAction::None`] — mouse
/// events in particular. They used to fall through to `mouse_action`, which
/// moved the selection *and* focused another terminal, and then the pending
/// `/rename` was typed into that session.
fn rename_event(ev: &Event) -> RenameAction {
    match ev {
        Event::Key(k) if k.kind != KeyEventKind::Release => keys::rename_action(*k),
        _ => RenameAction::None,
    }
}

/// A `last_poll` stamp that makes the next poll land `after` from now.
/// `Instant::now() - interval` panics on a machine booted less than `interval`
/// ago, which a generous `--interval` makes reachable.
fn poll_again_in(interval: Duration, after: Duration) -> Instant {
    let back = interval.saturating_sub(after);
    Instant::now()
        .checked_sub(back)
        .unwrap_or_else(Instant::now)
}

fn run_tui(o: WatchOpts) -> Result<()> {
    // One read of config.toml for the whole startup: `[ui]`, `[naming]` and the
    // parse error all come out of it.
    let loaded = commands::load_all();
    let mouse = o
        .mouse
        .or(loaded.cfg.ui.mouse)
        // On by default: a tap is the only pointing device on a phone.
        .unwrap_or(true);

    enable_raw_mode().map_err(Error::Io)?;
    execute!(io::stdout(), EnterAlternateScreen).map_err(Error::Io)?;
    let _guard = TermGuard { mouse };
    if mouse {
        execute!(io::stdout(), EnableMouseCapture).map_err(Error::Io)?;
    }
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout())).map_err(Error::Io)?;

    let mut state = load_state();
    let mut log: Vec<Ev> = Vec::new();
    let mut rows: Vec<Session> = Vec::new();
    let mut last_poll: Option<Instant> = None;
    // Track selection by session key, not index, so it stays put as rows re-sort.
    let mut selected: Option<String> = None;
    // Some(..) while typing a new name for a pinned session.
    let mut renaming: Option<Rename> = None;
    let interval = Duration::from_secs(o.interval_secs);
    let mut dash = Dash {
        list: ListState::default(),
        help: false,
        force_two_row: resolve_rows(o.rows),
        events: None,
        list_area: Rect::default(),
        item_height: 1,
        events_shown: false,
        tiny: false,
        naming: None,
        tick: 0,
        dormant: 0,
    };
    let mut naming = Naming::new(&loaded.cfg.naming);
    let sync_tmux = loaded.cfg.naming.sync_tmux();
    // A config that doesn't parse degrades to defaults in silence — say so once,
    // otherwise `z` appearing not to stick has no visible explanation.
    if let Some(problem) = loaded.problem {
        log.push(Ev::now("⚠", problem));
    }

    loop {
        // The naming workers answer on their own schedule; take whatever they
        // have before drawing, so a result never waits a frame to show up.
        for msg in naming.drain() {
            let ev = match msg {
                // Said once per pool by contract — `claude` missing, logged out
                // or rate-limited is one line, not one line per session.
                NameMsg::Unavailable(why) => Some(Ev::now("⚠", format!("naming fell back: {why}"))),
                NameMsg::Failed {
                    key,
                    label,
                    err,
                    purpose,
                } => {
                    let purpose = naming.purpose_of(&key, purpose);
                    naming.settle(&key, true);
                    // A background title that didn't work out is not news: the
                    // row keeps its session name and `Unavailable` has already
                    // said whatever systemic thing went wrong, once.
                    purpose
                        .is_interactive()
                        .then(|| Ev::now("✕", format!("no name for {label}: {err}")))
                }
                NameMsg::Named {
                    key,
                    name,
                    source,
                    purpose,
                    ..
                } => {
                    let purpose = naming.purpose_of(&key, purpose);
                    naming.settle(&key, source == NameSource::Heuristic);
                    // Every answer becomes the row's headline, however it was
                    // asked for — one generation serves the display and the
                    // rename flow both.
                    naming.titles.insert(key.clone(), name.clone());
                    purpose.is_interactive().then(|| {
                        naming.suggestions.insert(key.clone(), name.clone());
                        offer(&key, &name, &rows, &mut renaming, purpose == Purpose::Bulk)
                    })
                }
            };
            if let Some(ev) = ev {
                log.insert(0, ev);
                log.truncate(EVENT_CAP);
            }
        }
        dash.naming = naming.progress();

        if last_poll.map(|t| t.elapsed() >= interval).unwrap_or(true) {
            let (r, mut evs) = tick(&mut state, o.stuck_secs);
            rows = r;
            evs.reverse(); // newest first when prepended
            for e in evs {
                log.insert(0, e);
            }
            log.truncate(EVENT_CAP);
            last_poll = Some(Instant::now());
            dash.dormant = crate::core::snapshot::dormant_count();
            // New sessions get a title, and a session whose first prompt has
            // landed since gets a better one. A no-op once the fleet is titled.
            naming.autotitle(&rows);
        }
        // The registry knows nothing about titles, and `rows` came from it.
        apply_titles(&mut rows, &naming.titles);
        if selected.is_none() && !rows.is_empty() {
            selected = Some(rows[0].key());
        }
        // The selected session can vanish between polls; fall back to the top.
        let sel = selected
            .as_ref()
            .and_then(|k| rows.iter().position(|r| &r.key() == k))
            .unwrap_or(0);
        // ListState::offset survives a shrinking list and can point past the end.
        clamp_offset(&mut dash, rows.len());

        terminal
            .draw(|f| draw(f, &rows, &log, sel, renaming.as_ref(), &o, &mut dash))
            .map_err(Error::Io)?;

        if !event::poll(Duration::from_millis(250)).map_err(Error::Io)? {
            continue;
        }
        let ev = event::read().map_err(Error::Io)?;

        // Renaming swallows the whole input stream until it's applied or
        // cancelled — mouse events included. A stray tap used to fall through to
        // `mouse_action`, move the selection *and* focus another terminal, and
        // the pending `/rename` then landed in that session.
        if renaming.is_some() {
            if let Event::Resize(_, _) = ev {
                clamp_offset(&mut dash, rows.len());
            }
            match rename_event(&ev) {
                RenameAction::None => {}
                RenameAction::Insert(c) => {
                    if let Some(r) = renaming.as_mut() {
                        r.buf.push(c);
                    }
                }
                RenameAction::Backspace => {
                    if let Some(r) = renaming.as_mut() {
                        r.buf.pop();
                    }
                }
                RenameAction::Cancel => renaming = None,
                RenameAction::Quit => break,
                RenameAction::Commit => {
                    if let Some(r) = renaming.take()
                        && let Some((icon, msg, sent)) = commit_rename(&r, &rows, sync_tmux)
                    {
                        if sent {
                            naming.suggestions.remove(&r.key);
                        }
                        log.insert(0, Ev::now(icon, msg));
                        log.truncate(EVENT_CAP);
                        // Claude writes the new name on its next status update.
                        last_poll = Some(poll_again_in(interval, Duration::from_secs(2)));
                    }
                }
            }
            continue;
        }

        let action = match ev {
            Event::Key(k) if k.kind != KeyEventKind::Release => keys::action(k),
            Event::Mouse(m) => {
                keys::mouse_action(m, dash.list_area, dash.list.offset(), dash.item_height)
            }
            // A resize needs a redraw (the loop does that anyway) and a re-clamp.
            Event::Resize(_, _) => {
                clamp_offset(&mut dash, rows.len());
                Action::None
            }
            _ => Action::None,
        };

        let focus = |idx: usize, log: &mut Vec<Ev>| {
            if let Some(r) = rows.get(idx)
                && let Err(e) = backend::focus(r)
            {
                log.insert(0, Ev::now("✕", format!("focus failed: {e}")));
            }
        };

        match action {
            Action::None => {}
            // Esc doubles as "close the overlay" before it means "quit".
            Action::Quit if dash.help => dash.help = false,
            Action::Quit => break,
            Action::Refresh => last_poll = None,
            // Pin the target now: `sel` is recomputed from the live rows every
            // iteration, so by the time ⏎ lands it may point at someone else.
            Action::Rename => {
                if let Some(r) = rows.get(sel) {
                    renaming = Some(Rename {
                        key: r.key(),
                        buf: String::new(),
                    });
                }
            }
            Action::ToggleHelp => dash.help = !dash.help,
            Action::ToggleEvents => dash.events = Some(!dash.events_shown),
            // A Tiny pane has no second line to give, so `plan` ignores the
            // override there — persisting it would strand the user in a mode they
            // never chose the next time they open a wide terminal.
            Action::ToggleRows if dash.tiny => {
                log.insert(0, Ev::now("·", "the row layout needs a wider pane".into()));
                log.truncate(EVENT_CAP);
            }
            Action::ToggleRows => {
                // Flip relative to what's on screen, then remember it.
                dash.force_two_row = Some(dash.item_height != 2);
                if let Err(e) = commands::persist_ui("rows", rows_setting(dash.force_two_row)) {
                    log.insert(0, Ev::now("✕", format!("layout not remembered: {e}")));
                    log.truncate(EVENT_CAP);
                }
            }
            Action::Up if !rows.is_empty() => {
                selected = Some(rows[sel.saturating_sub(1)].key());
            }
            Action::Down if !rows.is_empty() => {
                selected = Some(rows[(sel + 1).min(rows.len() - 1)].key());
            }
            Action::Up | Action::Down => {}
            Action::Focus => focus(sel, &mut log),
            // Digit and tap indices are raw — re-clamp before trusting them.
            Action::JumpTo(idx) if idx < rows.len() => {
                selected = Some(rows[idx].key());
                focus(idx, &mut log);
            }
            Action::JumpTo(_) => {}
            // Suggest-and-confirm, never auto-apply: `N` generates, the pinned
            // rename buffer is what actually sends.
            Action::SuggestName | Action::SuggestNameAll => {
                let bulk = action == Action::SuggestNameAll;
                let purpose = if bulk { Purpose::Bulk } else { Purpose::Offer };
                let keys: Vec<String> = if bulk {
                    // Only the cwd+hash names — a name a human or an earlier
                    // pass chose is not ours to replace in bulk.
                    rows.iter()
                        .filter(|r| r.is_derived_name())
                        .map(Session::key)
                        .collect()
                } else {
                    rows.get(sel)
                        .map(|r| vec![r.key()])
                        .into_iter()
                        .flatten()
                        .collect()
                };
                let (evs, ready) = naming.request(&rows, &keys, purpose);
                for e in evs {
                    log.insert(0, e);
                }
                // Already generated this run (typically by a `Ctrl-N` pass):
                // offer it straight away instead of paying for it twice.
                for key in ready {
                    if let Some(name) = naming.suggestions.get(&key).cloned() {
                        log.insert(0, offer(&key, &name, &rows, &mut renaming, bulk));
                    }
                }
                log.truncate(EVENT_CAP);
            }
        }
    }
    Ok(())
}

/// Stamp each row with its generated title, so the dashboard draws headlines
/// instead of `app-9d`.
///
/// Rows are rebuilt from Claude's registry on every poll and the registry has no
/// idea what a title is, so this runs per frame rather than per poll — a title
/// that arrives between polls shows up on the next draw, not the next poll.
/// A row keeps whatever title it arrived with when we have none of our own —
/// `discover` never sets one, but a fixture can, and a demo fleet that lost its
/// titles to an empty map would draw `app-9d` on every row.
fn apply_titles(rows: &mut [Session], titles: &HashMap<String, String>) {
    for r in rows {
        if let Some(title) = titles.get(&r.key()) {
            r.gen_title = Some(title.clone());
        }
    }
}

/// `ListState::offset` is not bounded by the widget, so a shrinking fleet (or a
/// resize) can leave it pointing at nothing and the list renders blank.
fn clamp_offset(dash: &mut Dash, len: usize) {
    let max = len.saturating_sub(1);
    if dash.list.offset() > max {
        *dash.list.offset_mut() = max;
    }
    if let Some(s) = dash.list.selected()
        && s > max
    {
        dash.list.select(Some(max));
    }
}

// --- drawing -----------------------------------------------------------------

fn draw(
    f: &mut ratatui::Frame,
    rows: &[Session],
    log: &[Ev],
    sel: usize,
    renaming: Option<&Rename>,
    o: &WatchOpts,
    dash: &mut Dash,
) {
    let area = f.area();
    dash.tick = dash.tick.wrapping_add(1);
    let naming = dash.naming;
    let tick = dash.tick;
    let longest = rows
        .iter()
        .map(|r| width_of(&r.headline()))
        .max()
        .unwrap_or(0);
    let mut plan = dashlayout::plan(
        area.width,
        area.height,
        rows.len(),
        longest,
        dash.force_two_row,
    );

    // `e` overrides the height policy in both directions. Turning events on in a
    // pane too short for both hands them the whole thing below the header.
    let events_height = match dash.events {
        Some(false) => 0,
        Some(true) if plan.events_height == 0 => area.height.saturating_sub(1),
        _ => plan.events_height,
    };
    let events_height = events_fit(events_height, log.len(), plan.borders);
    let events_height = balance_spare_row(
        events_height,
        area.height,
        plan.borders,
        plan.item_height(),
        rows.len(),
        log.len(),
    );
    plan.events_height = events_height;
    dash.item_height = plan.item_height();
    dash.events_shown = events_height > 0;
    dash.tiny = plan.density == Density::Tiny;

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(events_height),
        ])
        .split(area);

    if let Some(r) = renaming {
        // The pinned session can vanish mid-buffer; name it by key rather than
        // silently addressing nobody.
        let who = rows
            .iter()
            .find(|s| s.key() == r.key)
            .map(Session::label)
            .unwrap_or_else(|| short_key(&r.key));
        render_rename(f, chunks[0], &who, &r.buf);
    } else {
        render_header(f, chunks[0], rows, o, &plan, (naming, dash.dormant), tick);
    }
    let ctx = FleetCtx::of(rows, &plan);
    render_sessions(f, chunks[1], rows, sel, &plan, &ctx, dash);
    if events_height > 0 {
        render_events(f, chunks[2], log, &plan);
    }
    if dash.help {
        render_help(f);
        // The overlay owns every cell it covers: without this a tap on it maps
        // straight through to the row underneath and focuses that terminal.
        dash.list_area = Rect::default();
    }
}

/// Trim the events pane to what it actually has to show. An empty log used to
/// hold 8 of a 30-row frame open for one line of "waiting for something to
/// happen…" while the sessions list rendered blank rows; the surplus belongs to
/// the list. Returns 0 when what's left can't fit a single event line.
fn events_fit(height: u16, entries: usize, borders: bool) -> u16 {
    let chrome = if borders { 2 } else { 0 };
    // The placeholder is one line, so an empty log still asks for one.
    let want = (entries.max(1) as u16).saturating_add(chrome);
    let height = height.min(want);
    if height <= chrome { 0 } else { height }
}

/// Hand the sessions list's unusable remainder to the events pane.
///
/// Two-row items can't fill an odd row, so a 16-row phone frame left one blank
/// under the list. A row is worth real money at that size — an extra event line
/// beats a gap. No-op when the events pane is hidden, and stable: the list only
/// ever gives up rows it could not have drawn an item into.
fn balance_spare_row(
    events_height: u16,
    area_h: u16,
    borders: bool,
    item_h: u16,
    rows: usize,
    entries: usize,
) -> u16 {
    let chrome = if borders { 2 } else { 1 };
    // Only worth moving when the events pane has something more to show with it;
    // otherwise the blank row just changes frames.
    let starved = entries as u16 > events_height.saturating_sub(chrome);
    if events_height == 0 || item_h <= 1 || !starved {
        return events_height;
    }
    let inner = area_h
        .saturating_sub(1 + events_height)
        .saturating_sub(chrome);
    let hint = u16::from(
        u16::try_from(rows)
            .unwrap_or(u16::MAX)
            .saturating_mul(item_h)
            > inner,
    );
    let spare = inner.saturating_sub(hint) % item_h;
    events_height.saturating_add(spare)
}

fn render_rename(f: &mut ratatui::Frame, area: Rect, who: &str, buf: &str) {
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                format!(" rename {who} → "),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{buf}▏"),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "  ⏎ apply · esc cancel",
                Style::default().fg(Color::DarkGray),
            ),
        ])),
        area,
    );
}

/// The header degrades hard: the keybinding list moved into the `?` overlay
/// because at 40 columns it was 150 characters of invisible text, on one
/// un-wrapped line. What's left is emitted in priority order, and any segment
/// that doesn't fit is skipped rather than clipped mid-word.
#[allow(clippy::too_many_arguments)]
fn render_header(
    f: &mut ratatui::Frame,
    area: Rect,
    rows: &[Session],
    o: &WatchOpts,
    plan: &Plan,
    (naming, dormant): (Option<NameProgress>, usize),
    tick: u64,
) {
    let busy = rows.iter().filter(|r| r.status == "busy").count();
    let waiting = rows.iter().filter(|r| r.is_waiting()).count();
    let n = rows.len();
    let wide = plan.density == Density::Wide;
    let tiny = plan.density == Density::Tiny;

    let name = Style::default()
        .fg(Color::White)
        .add_modifier(Modifier::BOLD);
    let body = Style::default().fg(Color::Gray);
    let dim = Style::default().fg(Color::DarkGray);
    let alarm = Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD);
    let clock = chrono::Local::now();

    // Most important first — a skipped segment doesn't stop the shorter ones
    // behind it from landing.
    let mut segs: Vec<(String, Style)> = vec![
        (" fleet ".into(), name),
        if tiny {
            (format!("· {n}·{busy} "), body)
        } else if wide {
            (format!("· {n} live · {busy} working "), body)
        } else {
            (format!("· {n} live · {busy} busy "), body)
        },
    ];
    if waiting > 0 {
        segs.push(if wide {
            (format!("· {waiting} need you "), alarm)
        } else {
            (format!("· {waiting}⏸ "), alarm)
        });
    }
    // A reboot's leftovers, waiting for `fleet restore` — easy to miss otherwise.
    if dormant > 0 {
        segs.push(if wide {
            (format!("· {dormant} dormant "), dim)
        } else {
            (format!("· {dormant}◌ "), dim)
        });
    }
    // A model call is seconds long, so the fleet has to say it's working on one
    // — otherwise `N` looks like it did nothing at all.
    if let Some(p) = naming {
        let spin = spinner_frame(tick);
        segs.push((
            if wide {
                format!("· {spin} naming {}/{} ", p.done, p.total)
            } else {
                // Seven cells. At 40 columns this costs the `?` hint for as long
                // as the pass runs, which is the right trade: the hint is static,
                // the spinner is the only sign the model is still thinking.
                format!("· {spin}{}/{} ", p.done, p.total)
            },
            Style::default().fg(Color::Cyan),
        ));
    }
    segs.push(if wide {
        ("· ? help ".into(), dim)
    } else {
        ("· ? ".into(), dim)
    });
    segs.push(if wide {
        (format!("· {} ", clock.format("%H:%M:%S")), dim)
    } else {
        (format!("· {} ", clock.format("%H:%M")), dim)
    });
    if wide {
        segs.push((
            format!("· every {}s · stuck>{}s ", o.interval_secs, o.stuck_secs),
            dim,
        ));
    }

    let budget = area.width as usize;
    let mut used = 0;
    let mut spans = Vec::new();
    for (text, style) in segs {
        let w = width_of(&text);
        if used + w > budget {
            continue;
        }
        used += w;
        spans.push(Span::styled(text, style));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// ` Sessions · ~/Code/app `: the block's title carries the directory the fleet
/// has in common, which is what earns the rows the right to leave it out.
///
/// `FleetCtx` already dropped the base if this pane couldn't print it, so the two
/// can't disagree — a row never draws a relative path the title failed to explain.
fn sessions_title(ctx: &FleetCtx) -> String {
    ctx.base
        .as_deref()
        .and_then(rows::base_title)
        .unwrap_or_else(|| " Sessions ".to_string())
}

fn render_sessions(
    f: &mut ratatui::Frame,
    area: Rect,
    rows: &[Session],
    sel: usize,
    plan: &Plan,
    ctx: &FleetCtx,
    dash: &mut Dash,
) {
    if area.height == 0 || area.width == 0 {
        dash.list_area = Rect::default();
        return;
    }
    // The directory the fleet shares is named here, once, instead of on every row.
    let title = sessions_title(ctx);
    let block = if plan.borders {
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(title)
    } else {
        // No columns to spare for a frame: one title line carries the same info.
        Block::default().title(Span::styled(title, Style::default().fg(Color::DarkGray)))
    };
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height == 0 || inner.width == 0 {
        dash.list_area = Rect::default();
        return;
    }

    if rows.is_empty() {
        dash.list_area = Rect::default();
        f.render_widget(
            Paragraph::new(Span::styled(
                "no running claude sessions",
                Style::default().fg(Color::DarkGray),
            )),
            inner,
        );
        return;
    }

    let item_h = plan.item_height();
    // Reserve a line for the overflow hint, the same way a paged list does — and round
    // the list down to whole items, so the hint sits directly under the last one.
    // Left at `inner.height - 1` it drifted a row up or down with the pane's
    // parity: at 44x16 a blank line opened up above it, at 32x12 it didn't.
    let overflows = (rows.len() as u16).saturating_mul(item_h) > inner.height;
    let (list_area, hint_y) = if overflows && inner.height > item_h {
        let height = ((inner.height - 1) / item_h) * item_h;
        (Rect { height, ..inner }, Some(inner.y + height))
    } else {
        (inner, None)
    };

    let items: Vec<_> = rows
        .iter()
        .enumerate()
        .map(|(i, r)| session_item(r, plan, ctx, i, i == sel))
        .collect();
    dash.list.select(Some(sel));
    dash.list_area = list_area;
    f.render_stateful_widget(List::new(items), list_area, &mut dash.list);

    if let Some(y) = hint_y {
        let shown = dash.list.offset() + (list_area.height / item_h.max(1)) as usize;
        let remaining = rows.len().saturating_sub(shown);
        if remaining > 0 {
            f.render_widget(
                Paragraph::new(Span::styled(
                    format!(" +{remaining} more ↓"),
                    Style::default().add_modifier(Modifier::DIM),
                )),
                Rect {
                    y,
                    height: 1,
                    ..inner
                },
            );
        }
    }
}

fn render_events(f: &mut ratatui::Frame, area: Rect, log: &[Ev], plan: &Plan) {
    if area.height == 0 {
        return;
    }
    let ev_lines: Vec<Line> = if log.is_empty() {
        vec![Line::from(Span::styled(
            " waiting for something to happen…",
            Style::default().fg(Color::DarkGray),
        ))]
    } else {
        log.iter()
            .map(|e| {
                // The icon set is mixed-width — `＋` and `✅` draw two columns,
                // the rest one — so pad to a constant two and keep the times
                // (and everything behind them) in one column.
                let icon = format!(
                    " {}{} ",
                    e.icon,
                    " ".repeat(2usize.saturating_sub(width_of(e.icon)))
                );
                Line::from(vec![
                    Span::raw(icon),
                    Span::styled(format!("{} ", e.time), Style::default().fg(Color::DarkGray)),
                    Span::raw(e.msg.clone()),
                ])
            })
            .collect()
    };
    let block = if plan.borders {
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(" Events ")
    } else {
        Block::default()
    };
    f.render_widget(Paragraph::new(ev_lines).block(block), area);
}

fn render_help(f: &mut ratatui::Frame) {
    const BINDS: [(&str, &str); 12] = [
        ("1-9 0", "jump to that row and focus it"),
        ("⏎ ␣ l o", "focus the selected session"),
        ("↑↓ jk", "move the selection"),
        ("tap", "select + focus (mouse/touch)"),
        ("n", "rename the selected session"),
        ("r", "refresh now"),
        ("z", "compact 1-line rows (remembered)"),
        ("e", "show/hide the events pane"),
        ("N", "suggest a name for this session"),
        ("^N", "suggest names for unnamed ones"),
        ("?", "toggle this help"),
        ("q Esc", "quit"),
    ];
    let area = centered_rect(48, BINDS.len() as u16 + 2, f.area());
    f.render_widget(Clear, area);
    let lines: Vec<Line> = BINDS
        .iter()
        .map(|(k, v)| {
            Line::from(vec![
                Span::styled(format!(" {k:<9}"), Style::default().fg(Color::Cyan)),
                Span::raw(*v),
            ])
        })
        .collect();
    f.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .title(" Keys "),
            )
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    Rect {
        x: area.x + area.width.saturating_sub(w) / 2,
        y: area.y + area.height.saturating_sub(h) / 2,
        width: w,
        height: h,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyModifiers};
    use ratatui::backend::TestBackend;

    #[test]
    fn detects_awaiting_prompt() {
        assert!(awaiting("│ Do you want to proceed? │"));
        assert!(awaiting("  ❯ 1. Yes  "));
        assert!(!awaiting("✻ Working for 12s"));
    }

    #[test]
    fn row_mode_round_trips_through_the_config_value() {
        for v in [None, Some(true), Some(false)] {
            assert_eq!(resolve_rows(Some(v)), v);
        }
        assert_eq!(rows_setting(None), "auto");
        assert_eq!(rows_setting(Some(true)), "2");
        assert_eq!(rows_setting(Some(false)), "1");
    }

    #[test]
    fn offsets_are_clamped_when_the_fleet_shrinks() {
        let mut dash = test_dash();
        *dash.list.offset_mut() = 12;
        dash.list.select(Some(12));
        clamp_offset(&mut dash, 3);
        assert_eq!(dash.list.offset(), 2);
        assert_eq!(dash.list.selected(), Some(2));
        // An empty fleet must not underflow.
        clamp_offset(&mut dash, 0);
        assert_eq!(dash.list.offset(), 0);
    }

    fn test_dash() -> Dash {
        Dash {
            list: ListState::default(),
            help: false,
            force_two_row: None,
            events: None,
            list_area: Rect::default(),
            item_height: 1,
            events_shown: false,
            tiny: false,
            naming: None,
            tick: 0,
            dormant: 0,
        }
    }

    // --- golden buffer -------------------------------------------------------

    fn fixture() -> Vec<Session> {
        let json = r#"[
          {"pid":1,"session_id":"aaaaaaaa-1111","name":"cache-warmup-scheduler",
           "cwd":"/tmp/wt/api-server","status":"busy","tmux_session":"api-server",
           "backend":"tmux","title":"make the cache warmup idempotent so a re-run is free"},
          {"pid":2,"session_id":"bbbbbbbb-2222","name":"docs-refresh",
           "cwd":"/tmp/wt/frontend","status":"waiting","waiting_for":"input needed",
           "tmux_session":"frontend","backend":"tmux","title":"refresh the install docs"},
          {"pid":3,"session_id":"cccccccc-3333","name":"app-9d","name_source":"derived",
           "cwd":"/tmp/app","status":"idle","tab":"app","backend":"iterm",
           "title":"why is the statusline blank"}
        ]"#;
        let mut rows: Vec<Session> = serde_json::from_str(json).unwrap();
        for r in &mut rows {
            r.updated_at = Some(now_ms() - 120_000);
        }
        rows.sort_by_key(|r| !r.is_waiting());
        rows
    }

    /// Every drawn line must fill the frame exactly — measured in cells, since a
    /// rendered double-wide glyph leaves the following cell as a space.
    fn assert_cells(lines: &[String], width: usize) {
        for line in lines {
            assert_eq!(line.chars().count(), width, "{line:?}");
        }
    }

    /// Render the whole dashboard at `w`x`h` and give back the drawn lines.
    fn screen(w: u16, h: u16, rows_mode: Option<bool>) -> Vec<String> {
        let log = vec![Ev::now("▶", "cache-warmup-scheduler working".into())];
        screen_of(w, h, rows_mode, &fixture(), &log)
    }

    fn screen_of(
        w: u16,
        h: u16,
        rows_mode: Option<bool>,
        rows: &[Session],
        log: &[Ev],
    ) -> Vec<String> {
        let o = WatchOpts {
            interval_secs: 5,
            stuck_secs: 300,
            ..Default::default()
        };
        let mut dash = test_dash();
        dash.force_two_row = rows_mode;
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| draw(f, rows, log, 0, None, &o, &mut dash))
            .unwrap();
        let buf = t.backend().buffer().clone();
        (0..h)
            .map(|y| (0..w).map(|x| buf[(x, y)].symbol()).collect::<String>())
            .collect()
    }

    fn many(n: usize) -> Vec<Session> {
        (0..n)
            .map(|i| Session {
                name: Some(format!("session-{i:02}")),
                session_id: Some(format!("id-{i}")),
                status: "idle".into(),
                updated_at: Some(now_ms()),
                tmux_session: Some(format!("tmux-{i:02}")),
                ..Default::default()
            })
            .collect()
    }

    // The title is the only thing explaining the rows' relative paths, so it is
    // either drawn whole or not at all — and `FleetCtx` is where that is decided,
    // so the rows and the title can't disagree.
    #[test]
    fn the_block_title_carries_the_common_base_only_when_it_fits() {
        let at = |dir: &str| Session {
            cwd: Some(dir.to_string()),
            session_id: Some(dir.to_string()),
            status: "idle".into(),
            ..Default::default()
        };
        let home = |rest: &str| format!("{}/{rest}", dirs::home_dir().unwrap().display());
        let title = |w: u16, rows: &[Session]| {
            let plan = dashlayout::plan(w, 40, rows.len(), 22, None);
            sessions_title(&FleetCtx::of(rows, &plan))
        };

        let shared = [at(&home("Code/app")), at(&home("Code/app/clones/api"))];
        assert_eq!(title(170, &shared), " Sessions · ~/Code/app ");
        // A phone pane names it too: ten columns plus the path is all it takes.
        assert_eq!(title(44, &shared), " Sessions · ~/Code/app ");
        // Nothing in common, nothing to name.
        assert_eq!(title(170, &[at("/tmp/a"), at("/var/b")]), " Sessions ");
        // Too long for the pane: the title stays plain, and the rows then draw
        // whole paths rather than remainders of a base nobody was told about.
        let deep = home("Code/app/branches/a-very-long-worktree-directory-name");
        let long = [at(&deep), at(&deep)];
        assert_eq!(title(40, &long), " Sessions ");
        assert!(title(170, &long).contains("branches"));
    }

    #[test]
    fn golden_desktop() {
        // 170 columns is a common wide terminal to watch the fleet in.
        let s = screen(170, 40, None);
        let all = s.join("\n");
        // Header: counts, and the waiting session called out.
        assert!(s[0].contains("fleet"), "{:?}", s[0]);
        assert!(s[0].contains("3 live"), "{:?}", s[0]);
        assert!(s[0].contains("1 need you"), "{:?}", s[0]);
        assert!(s[0].contains("? help"), "{:?}", s[0]);
        // The directory the fleet shares is named once, in the block's title —
        // which is what buys the rows the right to leave it off.
        assert!(s[1].contains("Sessions · /tmp/wt"), "{:?}", s[1]);
        assert!(all.contains("Events"), "{all}");
        // Two lines per session: identity on the first, the prompt on the second.
        assert!(s[2].contains("docs-refresh"), "{:?}", s[2]);
        assert!(s[2].contains("needs you"), "{:?}", s[2]);
        assert!(s[2].contains("⧉ frontend"), "{:?}", s[2]);
        assert!(s[3].contains("refresh the install docs"), "{:?}", s[3]);
        assert!(s[3].trim_matches(['│', ' ']).len() > 10, "{:?}", s[3]);
        // Full names, and the prompt no longer competes for the same line.
        assert!(s[4].contains("cache-warmup-scheduler"), "{:?}", s[4]);
        assert!(
            s[5].contains("make the cache warmup idempotent so a re-run is free"),
            "{:?}",
            s[5]
        );
        // iTerm sessions fall back to their tab title, and the one session that
        // isn't under the common base draws its whole path.
        assert!(all.contains("▣ app"), "{all}");
        assert!(all.contains("/tmp/app"), "{all}");
        assert_cells(&s, 170);
    }

    // The same shape has to hold across the widths a desktop terminal actually
    // gets resized to, borders and all.
    #[test]
    fn golden_desktop_widths_all_draw_two_line_items() {
        for (w, h) in [(160u16, 40u16), (200, 50), (120, 40), (100, 30)] {
            let s = screen(w, h, None);
            let all = s.join("\n");
            assert!(s[1].contains("Sessions"), "{w}x{h}: {:?}", s[1]);
            // Row 1 is the hoisted waiting session; row 2 is its prompt.
            assert!(s[2].contains("docs-refresh"), "{w}x{h}: {:?}", s[2]);
            assert!(s[2].contains("needs you"), "{w}x{h}: {:?}", s[2]);
            assert!(
                s[3].contains("refresh the install docs"),
                "{w}x{h}: {:?}",
                s[3]
            );
            // …and the next session starts two lines down, numbered 2.
            assert!(s[4].contains(" 2 "), "{w}x{h}: {:?}", s[4]);
            assert!(
                s[4].contains("cache-warmup-scheduler"),
                "{w}x{h}: {:?}",
                s[4]
            );
            assert!(all.contains("⧉ api-server"), "{w}x{h}: {all}");
            assert_cells(&s, w as usize);
        }
    }

    // `z`: one line per session, for when fifteen of them matter more than
    // reading any one of them.
    #[test]
    fn golden_desktop_compact() {
        let s = screen(170, 40, Some(false));
        let all = s.join("\n");
        // Three sessions, three consecutive rows.
        assert!(s[2].contains("docs-refresh"), "{:?}", s[2]);
        assert!(s[3].contains("cache-warmup-scheduler"), "{:?}", s[3]);
        assert!(s[4].contains("why-is-the"), "{:?}", s[4]);
        // Still numbered: the digits are how a phone client acts on a row.
        assert!(s[2].contains("▸1"), "{:?}", s[2]);
        // The prompt shares the line again, but the repeated base directory does
        // not — it stays in the title.
        assert!(s[3].contains("make the cache warmup"), "{:?}", s[3]);
        assert!(!s[3].contains("/tmp/wt/api-server"), "{:?}", s[3]);
        assert!(all.contains("Sessions · /tmp/wt"), "{all}");
        assert_cells(&s, 170);
    }

    #[test]
    fn golden_phone() {
        let s = screen(50, 20, None);
        let all = s.join("\n");
        // Three lines, one job each: identity, terminal, prompt.
        assert!(s[2].contains("▸1"), "{:?}", s[2]);
        assert!(s[2].contains("docs-refresh"), "{:?}", s[2]);
        assert!(s[2].contains("needs you"), "{:?}", s[2]);
        assert!(s[3].contains("⧉ frontend"), "{:?}", s[3]);
        assert!(s[4].contains("refresh the install docs"), "{:?}", s[4]);
        // A phone pane names the shared directory too, so the rows stop repeating
        // it — and the one session outside it still draws its path.
        assert!(s[1].contains("Sessions · /tmp/wt"), "{:?}", s[1]);
        assert!(!s[3].contains("/tmp"), "{:?}", s[3]);
        assert!(s[9].contains("/tmp/app"), "{:?}", s[9]);
        // Second session starts three lines down, numbered 2.
        assert!(s[5].contains(" 2"), "{:?}", s[5]);
        assert!(s[5].contains("cache-warmup-scheduler"), "{:?}", s[5]);
        // The prompt gets the whole line: behind the terminal label it used to be
        // a ~15-column stub on a 50-column pane.
        assert!(
            s[7].contains("make the cache warmup idempotent so a re-r"),
            "{:?}",
            s[7]
        );
        // Header drops the interval/stuck detail but keeps the counts.
        assert!(s[0].contains("3 live"), "{:?}", s[0]);
        assert!(!s[0].contains("stuck>"), "{:?}", s[0]);
        // Events still fit at 20 rows.
        assert!(all.contains("Events"), "{all}");
        assert_cells(&s, 50);
    }

    #[test]
    fn golden_phone_split() {
        let s = screen(40, 16, None);
        let all = s.join("\n");
        // Borderless at 40 columns — the frame's two columns go to the content.
        assert!(!s[1].starts_with('╭'), "{:?}", s[1]);
        assert!(s[1].contains("Sessions"), "{:?}", s[1]);
        assert!(s[2].contains("docs-refresh"), "{:?}", s[2]);
        assert!(s[3].contains("⧉ frontend"), "{:?}", s[3]);
        assert!(s[4].contains("refresh the install docs"), "{:?}", s[4]);
        assert!(all.contains("cache-warmup-schedu"), "{all}");
        assert_cells(&s, 40);
    }

    // 32 columns: line 1 hands the status word to line 2 so the name keeps its
    // room, and the third session still has to be on screen.
    #[test]
    fn golden_phone_narrow_split() {
        let s = screen(32, 12, None);
        assert!(s[2].contains("docs-refresh"), "{:?}", s[2]);
        assert!(!s[2].contains("needs you"), "{:?}", s[2]);
        assert!(s[3].contains("needs you"), "{:?}", s[3]);
        assert!(s[3].contains("⧉ frontend"), "{:?}", s[3]);
        assert!(s[4].contains("refresh the install docs"), "{:?}", s[4]);
        // Three items in twelve rows — the events pane gives the row up.
        assert!(s[8].contains("why-is"), "{:?}", s);
        assert_cells(&s, 32);
    }

    #[test]
    fn golden_forced_one_row_on_a_phone() {
        let s = screen(50, 20, Some(false));
        // One line per session, so all three land on consecutive rows.
        assert!(s[2].contains("docs-refresh"), "{:?}", s[2]);
        assert!(s[3].contains("cache-warmup"), "{:?}", s[3]);
        assert!(s[4].contains("why-is-the"), "{:?}", s[4]);
        assert_cells(&s, 50);
    }

    #[test]
    fn a_short_pane_hides_the_events_and_still_lists_sessions() {
        let s = screen(60, 9, None);
        let all = s.join("\n");
        assert!(!all.contains("Events"), "{all}");
        assert!(all.contains("docs-refresh"), "{all}");
    }

    // 15 sessions in a short pane: the selection has to stay on screen and the
    // hidden remainder has to be advertised.
    #[test]
    fn overflow_is_advertised() {
        let mut rows = Vec::new();
        for i in 0..15 {
            let mut s = Session {
                name: Some(format!("session-{i:02}")),
                status: "idle".into(),
                ..Default::default()
            };
            s.session_id = Some(format!("id-{i}"));
            s.updated_at = Some(now_ms());
            rows.push(s);
        }
        let o = WatchOpts::default();
        let mut dash = test_dash();
        let mut t = Terminal::new(TestBackend::new(60, 12)).unwrap();
        t.draw(|f| draw(f, &rows, &[], 0, None, &o, &mut dash))
            .unwrap();
        let buf = t.backend().buffer().clone();
        let all: String = (0..12)
            .map(|y| (0..60u16).map(|x| buf[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(all.contains("more ↓"), "{all}");
    }

    // --- rename buffer -------------------------------------------------------

    /// The rename is aimed at a session key, so a poll that re-sorts (or drops)
    /// the rows under the open buffer can't redirect it at whoever is on top.
    #[test]
    fn a_rename_resolves_the_session_it_was_pinned_to() {
        let mut rows = fixture();
        let target = rows[2].key();
        let label = rows[2].label();
        let r = Rename {
            key: target,
            buf: "  fresh-name  ".into(),
        };
        // Re-sort under the buffer: row 0 is now someone else entirely.
        rows.reverse();
        let (_, msg, _) = commit_rename(&r, &rows, false).expect("a non-empty buffer acts");
        // These fixture sessions have no controllable handle, so the send fails —
        // but it fails naming the *pinned* session, which is the point.
        assert!(msg.contains(&label), "{msg}");
    }

    #[test]
    fn a_rename_whose_session_vanished_is_dropped_not_redirected() {
        let rows = fixture();
        let r = Rename {
            key: "no-such-session".into(),
            buf: "fresh-name".into(),
        };
        let (icon, msg, _) =
            commit_rename(&r, &rows, false).expect("a vanished target is worth saying");
        assert_eq!(icon, "✕");
        assert!(msg.contains("gone"), "{msg}");
        // Nobody else was renamed in its place.
        for s in &rows {
            assert!(!msg.contains(&s.label()), "{msg}");
        }
    }

    // The CLI verb validates and crops what it is given; the typed buffer never
    // did, so `/rename <300 chars>` went into a live TUI and came back out as a
    // 300-character tmux session name.
    #[test]
    fn a_typed_name_is_capped_like_the_cli_verb_caps_it() {
        let rows = fixture();
        let r = Rename {
            key: rows[0].key(),
            buf: "z".repeat(300),
        };
        let (_, msg, _) = commit_rename(&r, &rows, false).expect("a non-empty buffer acts");
        let longest = msg
            .split_whitespace()
            .map(|w| w.chars().count())
            .max()
            .unwrap_or(0);
        assert!(
            longest <= commands::MAX_RENAME,
            "an uncapped {longest}-character name went out: {msg}"
        );
    }

    // `N`/`^N` are what the naming feature is; listing them below "quit" reads
    // as an afterthought.
    #[test]
    fn the_help_overlay_lists_the_naming_keys_before_quit() {
        let mut t = Terminal::new(TestBackend::new(80, 24)).unwrap();
        t.draw(render_help).unwrap();
        let buf = t.backend().buffer().clone();
        let text: Vec<String> = (0..24)
            .map(|y| (0..80u16).map(|x| buf[(x, y)].symbol()).collect::<String>())
            .collect();
        let row = |needle: &str| {
            text.iter()
                .position(|l| l.contains(needle))
                .unwrap_or_else(|| panic!("{needle} is not in the overlay:\n{}", text.join("\n")))
        };
        assert!(row("suggest a name") < row("quit"));
        assert!(row("suggest names for unnamed") < row("quit"));
    }

    #[test]
    fn an_empty_rename_buffer_does_nothing() {
        let r = Rename {
            key: fixture()[0].key(),
            buf: "   ".into(),
        };
        assert!(commit_rename(&r, &fixture(), false).is_none());
    }

    // Mouse capture is on by default, so a stray tap arrives mid-rename. It must
    // not reach `mouse_action`: that moved the selection *and* focused another
    // terminal, and the pending `/rename` then went to that session.
    #[test]
    fn the_rename_buffer_swallows_mouse_events() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let at = |kind| {
            Event::Mouse(MouseEvent {
                kind,
                column: 5,
                row: 4,
                modifiers: KeyModifiers::NONE,
            })
        };
        for kind in [
            MouseEventKind::Down(MouseButton::Left),
            MouseEventKind::ScrollUp,
            MouseEventKind::ScrollDown,
        ] {
            assert_eq!(rename_event(&at(kind)), RenameAction::None);
        }
        // Keys still get through — including Termius' LF-as-Return.
        assert_eq!(
            rename_event(&Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Char('j'),
                KeyModifiers::CONTROL
            ))),
            RenameAction::Commit
        );
    }

    // --- naming --------------------------------------------------------------

    fn naming_with(enabled: bool, auto: bool) -> Naming {
        Naming {
            pool: None,
            titles: HashMap::new(),
            titled_from: HashMap::new(),
            suggestions: HashMap::new(),
            queued: HashSet::new(),
            wanted: HashMap::new(),
            exhausted: HashSet::new(),
            pending: 0,
            total: 0,
            enabled,
            auto,
            model: "haiku".into(),
            // Never the real file: a unit test must not read (or seed itself
            // from) whatever this machine's fleet happens to be called.
            disk: NameCache::at(std::path::PathBuf::new()),
        }
    }

    fn naming_off() -> Naming {
        naming_with(false, true)
    }

    // --- titles --------------------------------------------------------------

    // The point of the whole feature: a row's first line is the generated title,
    // not the Claude session name (`app-9d`) and not the terminal tab name.
    #[test]
    fn a_title_replaces_the_session_name_on_the_row() {
        let mut rows = fixture();
        let derived = rows.iter().position(Session::is_derived_name).unwrap();
        let key = rows[derived].key();
        // The row as it stands: a derived name and an iTerm tab called `app`.
        assert_eq!(rows[derived].tab.as_deref(), Some("app"));
        let titles = HashMap::from([(key, "statusline-blank".to_string())]);
        apply_titles(&mut rows, &titles);

        let drawn = screen_of(120, 20, None, &rows, &[]).join("\n");
        assert!(drawn.contains("statusline-blank"), "{drawn}");
        assert!(!drawn.contains("app-9d"), "{drawn}");
    }

    // …and until one exists the row still has to say *something*: the
    // heuristic slug of the first prompt (`display_title`), not `app-9d`.
    #[test]
    fn an_untitled_row_falls_back_to_the_heuristic_title() {
        let mut rows = fixture();
        apply_titles(&mut rows, &HashMap::new());
        let drawn = screen_of(120, 20, None, &rows, &[]).join("\n");
        assert!(drawn.contains("why-is-the-statusline-blank"), "{drawn}");
    }

    // Titling is automatic, so the pass has to be a no-op in the steady state:
    // twelve `claude` children per poll is not a background feature.
    #[test]
    fn the_titling_pass_asks_for_each_session_once() {
        let rows = fixture();
        let mut n = naming_with(true, true);
        assert_eq!(n.due_for_title(&rows).len(), rows.len());

        for s in &rows {
            n.titled_from.insert(s.key(), s.title.clone());
        }
        assert!(n.due_for_title(&rows).is_empty());
    }

    // A session shows up in the registry before its first prompt is in the
    // transcript, so the title it gets at boot is made from a directory and not
    // much else. When the prompt lands, that title is owed a second look.
    #[test]
    fn a_prompt_that_lands_later_re_opens_the_title() {
        let mut rows = fixture();
        let mut n = naming_with(true, true);
        rows[0].title = None;
        n.titled_from.insert(rows[0].key(), None);
        for s in rows.iter().skip(1) {
            n.titled_from.insert(s.key(), s.title.clone());
        }
        assert!(n.due_for_title(&rows).is_empty());

        rows[0].title = Some("profile the search index re-render storm".into());
        assert_eq!(n.due_for_title(&rows), vec![rows[0].key()]);
    }

    // In flight and already fell back are both "leave it alone": the first would
    // pay twice, the second re-runs a call that is failing today.
    #[test]
    fn the_titling_pass_skips_queued_and_exhausted_sessions() {
        let rows = fixture();
        let mut n = naming_with(true, true);
        n.queued.insert(rows[0].key());
        n.exhausted.insert(rows[1].key());
        assert_eq!(n.due_for_title(&rows), vec![rows[2].key()]);
    }

    // `[naming] auto_title = false` (and fixture mode) start no pool at all.
    #[test]
    fn auto_title_off_starts_nothing() {
        let rows = fixture();
        for mut n in [naming_with(true, false), naming_with(false, true)] {
            n.autotitle(&rows);
            assert!(n.pool.is_none());
            assert_eq!(n.pending, 0);
            assert!(n.titles.is_empty());
        }
    }

    // A background title is display state: it must not open a rename buffer, and
    // it must not put a line in the event log for every session in the fleet.
    #[test]
    fn a_background_title_neither_offers_nor_logs() {
        let mut n = naming_with(true, true);
        let key = fixture()[0].key();
        let purpose = n.purpose_of(&key, Purpose::Title);
        assert_eq!(purpose, Purpose::Title);
        assert!(!purpose.is_interactive());
    }

    // …unless the user pressed `N` while it was in flight. The job in the channel
    // can't be amended, so the *answer* is upgraded instead.
    #[test]
    fn n_pressed_during_a_titling_pass_still_offers() {
        let rows = fixture();
        let mut n = naming_with(true, true);
        let key = rows[0].key();
        n.queued.insert(key.clone());

        let (evs, ready) = n.request(&rows, std::slice::from_ref(&key), Purpose::Offer);
        // Nothing new was queued and nothing was ready — but the want was recorded.
        assert!(ready.is_empty());
        assert!(evs.is_empty(), "{}", evs.len());
        assert_eq!(n.purpose_of(&key, Purpose::Title), Purpose::Offer);
        // Consumed: the next answer for that key is a title again.
        assert_eq!(n.purpose_of(&key, Purpose::Title), Purpose::Title);
    }

    // A title already in hand is what `N` offers, so the ordinary keypress never
    // queues behind the titling backlog and never pays for the same name twice.
    #[test]
    fn n_offers_the_title_the_row_is_already_showing() {
        let rows = fixture();
        let mut n = naming_with(true, true);
        let key = rows[0].key();
        n.titles.insert(key.clone(), "upload-retry-limit".into());

        let (_, ready) = n.request(&rows, std::slice::from_ref(&key), Purpose::Offer);
        assert_eq!(ready, vec![key.clone()]);
        assert!(n.pool.is_none());
        assert_eq!(
            n.suggestions.get(&key).map(String::as_str),
            Some("upload-retry-limit")
        );
    }

    // Fixture mode (and `[naming] enabled = false`) must never reach the model:
    // no pool, no child, one honest line in the log.
    #[test]
    fn naming_disabled_says_so_and_starts_nothing() {
        let rows = fixture();
        let mut n = naming_off();
        let (evs, ready) = n.request(&rows, &[rows[0].key()], Purpose::Offer);
        assert!(n.pool.is_none());
        assert_eq!(n.pending, 0);
        assert!(ready.is_empty());
        assert!(evs[0].msg.contains("naming is off"), "{}", evs[0].msg);
    }

    // A generated name is a *suggestion*: it prefills the pinned rename buffer
    // and waits for ⏎. Nothing is sent by the act of generating it.
    #[test]
    fn a_suggestion_prefills_the_rename_buffer_rather_than_sending() {
        let rows = fixture();
        // app-9d is the idle, derived-name row.
        let target = rows.iter().find(|r| r.is_derived_name()).unwrap().key();
        let mut renaming = None;
        let ev = offer(&target, "statusline-blank", &rows, &mut renaming, false);
        let r = renaming.expect("the buffer opens");
        assert_eq!(r.key, target);
        assert_eq!(r.buf, "statusline-blank");
        assert!(ev.msg.contains("⏎ applies"), "{}", ev.msg);
    }

    // `backend::send` types into a live Claude TUI. A session sitting on a
    // permission prompt would read `/rename foo` as its answer; a busy one runs
    // it mid-turn as a local command (core::title::Hold), so it gets the buffer.
    #[test]
    fn a_waiting_session_is_held_a_busy_one_is_prompted() {
        let rows = fixture();
        let s = rows.iter().find(|r| r.status == "waiting").unwrap();
        let mut renaming = None;
        let ev = offer(&s.key(), "some-name", &rows, &mut renaming, false);
        assert!(renaming.is_none(), "waiting opened a buffer");
        assert!(ev.msg.contains("held"), "{}", ev.msg);

        let s = rows.iter().find(|r| r.status == "busy").unwrap();
        let mut renaming = None;
        offer(&s.key(), "some-name", &rows, &mut renaming, false);
        assert!(renaming.is_some(), "busy is renameable");
    }

    // The job outlives the row it was started on. It must be dropped, never
    // re-aimed at whoever drifted into that slot.
    #[test]
    fn a_suggestion_for_a_vanished_session_is_dropped() {
        let rows = fixture();
        let mut renaming = None;
        let ev = offer("no-such-session", "orphan", &rows, &mut renaming, false);
        assert!(renaming.is_none());
        assert!(ev.msg.contains("gone"), "{}", ev.msg);
        for s in &rows {
            assert!(!ev.msg.contains(&s.label()), "{}", ev.msg);
        }
    }

    // A bulk pass reports into the event log; twelve rename buffers in a row is
    // not a confirmation flow.
    #[test]
    fn a_bulk_result_logs_instead_of_opening_a_buffer() {
        let rows = fixture();
        let target = rows.iter().find(|r| r.is_derived_name()).unwrap().key();
        let mut renaming = None;
        let ev = offer(&target, "statusline-blank", &rows, &mut renaming, true);
        assert!(renaming.is_none());
        assert!(ev.msg.contains("press N to apply"), "{}", ev.msg);
    }

    // …and an open buffer is whatever the user is typing right now: a result
    // arriving mid-edit must not overwrite it.
    #[test]
    fn a_result_never_clobbers_an_open_rename_buffer() {
        let rows = fixture();
        let target = rows.iter().find(|r| r.is_derived_name()).unwrap().key();
        let mut renaming = Some(Rename {
            key: rows[0].key(),
            buf: "half-typed".into(),
        });
        offer(&target, "statusline-blank", &rows, &mut renaming, false);
        let r = renaming.unwrap();
        assert_eq!(r.buf, "half-typed");
        assert_eq!(r.key, rows[0].key());
    }

    #[test]
    fn progress_counts_answers_not_jobs() {
        let mut n = naming_off();
        n.pending = 3;
        n.total = 3;
        assert_eq!(n.progress(), Some(NameProgress { done: 0, total: 3 }));
        n.settle("a", false);
        assert_eq!(n.progress(), Some(NameProgress { done: 1, total: 3 }));
        n.settle("b", true);
        n.settle("c", false);
        // Back to idle: the counter resets so the next pass starts from 0.
        assert_eq!(n.progress(), None);
        assert_eq!(n.total, 0);
        // The one that fell back is not retried this run.
        assert!(n.exhausted.contains("b"));
        assert!(!n.exhausted.contains("a"));
        // …and it can't underflow if a stray message arrives late.
        n.settle("d", false);
        assert_eq!(n.pending, 0);
    }

    // The spinner and the count have to be visible on a phone-sized header, not
    // just a 120-column one.
    #[test]
    fn the_header_shows_naming_progress() {
        let rows = fixture();
        let o = WatchOpts::default();
        for (w, h) in [(120u16, 40u16), (50, 20), (40, 16)] {
            let mut dash = test_dash();
            dash.naming = Some(NameProgress { done: 1, total: 3 });
            let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
            t.draw(|f| draw(f, &rows, &[], 0, None, &o, &mut dash))
                .unwrap();
            let buf = t.backend().buffer().clone();
            let head: String = (0..w).map(|x| buf[(x, 0)].symbol()).collect();
            assert!(head.contains("1/3"), "{w}x{h}: {head:?}");
        }
    }

    // --- small hazards -------------------------------------------------------

    #[test]
    fn short_key_counts_characters_not_bytes() {
        // A fixture id is user-supplied JSON; byte-slicing this panics.
        assert_eq!(short_key("čćžšđčćžšđ"), "čćžšđčćž");
        assert_eq!(short_key("abc"), "abc");
    }

    #[test]
    fn poll_again_in_survives_a_freshly_booted_machine() {
        // `Instant::now() - interval` aborts the TUI here.
        let far = Duration::from_secs(60 * 60 * 24 * 365 * 100);
        let _ = poll_again_in(far, Duration::from_secs(2));
        let t = poll_again_in(Duration::from_secs(30), Duration::from_secs(2));
        assert!(t <= Instant::now());
    }

    // --- vertical space ------------------------------------------------------

    #[test]
    fn an_empty_event_log_does_not_reserve_the_pane() {
        // 3 lines with borders (one line + the frame), not the policy's 10.
        assert_eq!(events_fit(10, 0, true), 3);
        assert_eq!(events_fit(10, 1, true), 3);
        assert_eq!(events_fit(10, 20, true), 10);
        assert_eq!(events_fit(5, 0, false), 1);
        // Nothing to draw into is nothing worth reserving.
        assert_eq!(events_fit(2, 5, true), 0);
        assert_eq!(events_fit(0, 5, false), 0);
    }

    #[test]
    fn the_surplus_of_an_empty_log_goes_to_the_sessions_list() {
        let rows = many(15);
        let s = screen_of(120, 30, None, &rows, &[]);
        let all = s.join("\n");
        assert!(all.contains("waiting for something to happen"), "{all}");
        // The events pane holds one line plus its frame, so the list gets the
        // other 24 rows — eleven two-line items and the overflow hint, not the
        // eight a 10-row events pane would have left room for.
        assert!(all.contains("session-10"), "{all}");
        assert!(all.contains("more ↓"), "{all}");
        // One line per session and the whole fleet fits with room to spare.
        let s = screen_of(120, 30, Some(false), &rows, &[]);
        let all = s.join("\n");
        assert!(all.contains("session-14"), "{all}");
        assert!(!all.contains("more ↓"), "{all}");
    }

    // The hint used to float a row above the list at 44x16 but not at 32x12,
    // purely from the pane's parity.
    #[test]
    fn the_overflow_hint_hugs_the_list() {
        let rows = many(15);
        for (w, h) in [(44u16, 16u16), (32, 12), (50, 20), (40, 18)] {
            let s = screen_of(w, h, None, &rows, &[Ev::now("▶", "x".into())]);
            let i = s
                .iter()
                .position(|l| l.contains("more ↓"))
                .unwrap_or_else(|| panic!("{w}x{h} lost the overflow hint:\n{}", s.join("\n")));
            let above = &s[i - 1];
            assert!(
                !above.trim_matches(['│', ' ']).is_empty(),
                "{w}x{h}: blank row above the hint:\n{}",
                s.join("\n")
            );
        }
    }

    #[test]
    fn a_spare_row_under_two_row_items_goes_to_the_events_pane() {
        // 16 rows: header 1 + events 3 leaves 12, i.e. 10 inside the borders —
        // 4 items and the hint use 9, so the odd row is handed to the events.
        assert_eq!(balance_spare_row(3, 16, true, 2, 15, 8), 4);
        // Nothing to fill it with: the row stays where it is.
        assert_eq!(balance_spare_row(3, 16, true, 2, 15, 1), 3);
        // One-row items use every row, and a hidden pane stays hidden.
        assert_eq!(balance_spare_row(3, 16, true, 1, 15, 8), 3);
        assert_eq!(balance_spare_row(0, 16, true, 2, 15, 8), 0);
    }

    // `＋` and `✅` are two columns wide, the rest of the icon set is one; the
    // times behind them used to sit a column apart.
    #[test]
    fn event_icons_of_mixed_width_line_up() {
        let log = vec![
            Ev::now("＋", "a appeared".into()),
            Ev::now("✕", "b closed".into()),
            Ev::now("✅", "c finished".into()),
            Ev::now("⏸", "d needs you".into()),
        ];
        let s = screen_of(120, 40, None, &fixture(), &log);
        // Every event line starts its timestamp in the same column, whatever the
        // icon's width. Measured in cells: a wide glyph blanks the cell after it.
        let times: Vec<usize> = s
            .iter()
            .filter(|l| {
                ["a appeared", "b closed", "c finished", "d needs you"]
                    .iter()
                    .any(|m| l.contains(m))
            })
            .map(|l| l.chars().position(|c| c.is_ascii_digit()).unwrap())
            .collect();
        assert_eq!(times.len(), 4, "{s:?}");
        assert!(times.iter().all(|c| *c == times[0]), "{times:?}\n{s:?}");
    }

    // The overlay owns its cells: `dash.list_area` has to be empty while it's up,
    // or a tap on the panel selects and focuses the row behind it.
    #[test]
    fn the_help_overlay_swallows_taps() {
        let rows = fixture();
        let o = WatchOpts::default();
        let mut dash = test_dash();
        dash.help = true;
        let mut t = Terminal::new(TestBackend::new(80, 24)).unwrap();
        t.draw(|f| draw(f, &rows, &[], 0, None, &o, &mut dash))
            .unwrap();
        assert_eq!(dash.list_area, Rect::default());
        assert_eq!(
            keys::mouse_action(
                crossterm::event::MouseEvent {
                    kind: crossterm::event::MouseEventKind::Down(
                        crossterm::event::MouseButton::Left
                    ),
                    column: 40,
                    row: 12,
                    modifiers: KeyModifiers::NONE,
                },
                dash.list_area,
                0,
                1
            ),
            Action::None
        );

        // With the overlay down the same tap lands on a row again.
        dash.help = false;
        t.draw(|f| draw(f, &rows, &[], 0, None, &o, &mut dash))
            .unwrap();
        assert_ne!(dash.list_area, Rect::default());
    }

    // The rename header names the pinned session, not whatever row is selected.
    #[test]
    fn the_rename_header_names_the_pinned_session() {
        let rows = fixture();
        let r = Rename {
            key: rows[2].key(),
            buf: "new".into(),
        };
        let o = WatchOpts::default();
        let mut dash = test_dash();
        let mut t = Terminal::new(TestBackend::new(80, 24)).unwrap();
        t.draw(|f| draw(f, &rows, &[], 0, Some(&r), &o, &mut dash))
            .unwrap();
        let buf = t.backend().buffer().clone();
        let head: String = (0..80u16).map(|x| buf[(x, 0)].symbol()).collect();
        assert!(head.contains(&rows[2].label()), "{head:?}");
        assert!(!head.contains(&rows[0].label()), "{head:?}");
        assert!(head.contains("⏎ apply"), "{head:?}");
    }

    #[test]
    fn degenerate_sizes_do_not_panic() {
        for (w, h) in [(1u16, 1u16), (10, 2), (20, 3), (28, 5), (200, 2)] {
            let s = screen(w, h, None);
            assert_eq!(s.len(), h as usize);
        }
    }
}
