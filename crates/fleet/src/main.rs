use std::io::IsTerminal;

use clap::{CommandFactory, Parser, Subcommand};
use colored::Colorize;

use fleet::cli::{
    answer as answer_cmd, brief as brief_cmd, close as close_cmd, commands, config_cmd,
    effort as effort_cmd, group, hosts as host_cmds, init, jobs as jobs_cmd, mcp,
    messages as messages_cmd, model as model_cmd, prs as prs_cmd, repos as repos_cmd, restore,
    schedule as schedule_cmd, skill, slack as slack_cmd, stack as stack_cmds, tasks as tasks_cmd,
    tmux as tmux_cmds, trust as trust_cmd, usage as usage_cmd, web,
};
use fleet::core::config;
use fleet::core::discovery::Backend;
use fleet::core::hosts::{self, Scope, Target};
use fleet::error::{Error, Result};
use fleet::tui::watch;

#[derive(Parser)]
#[command(
    name = "fleet",
    version = fleet::BUILD_VERSION,
    about = "See, steer and spawn Claude Code sessions and tmux sessions — on this machine or any host you can ssh to",
    after_help = "Bare `fleet` opens the dashboard on a terminal (a one-shot `list` when piped).\n\
Host: -H <name> runs the command on that host over ssh (it needs fleet installed there).\n\
tmux-session and machine commands (tmux, enter, last, new, exec, ssh) default to `defaultHost`; everything else to this machine.\n\
Env: FLEET_CONFIG FLEET_HOST FLEET_DRY_RUN FLEET_DEBUG FLEET_CONNECT_TIMEOUT FLEET_MUX FLEET_REMOTE_TIMEOUT FLEET_CMD FLEET_GH_IDENTITY FLEET_TMUX FLEET_SNAPSHOT NO_COLOR"
)]
struct Cli {
    /// Target host (a configured name, or any ssh destination); env FLEET_HOST
    #[arg(short = 'H', long, global = true, value_name = "NAME")]
    host: Option<String>,

    /// Run on this machine, never over ssh
    #[arg(long, global = true)]
    local: bool,

    /// Print what would run, run nothing
    #[arg(short = 'n', long, global = true)]
    dry_run: bool,

    /// The name the caller knows this machine by (set by remote dispatch)
    #[arg(long, global = true, hide = true, value_name = "NAME")]
    as_host: Option<String>,

    /// Spawned sessions act on GitHub as this identity (`ghIdentities` in the config), not
    /// as you; env FLEET_GH_IDENTITY, which a session passes on to everything it spawns
    #[arg(long, global = true, value_name = "NAME")]
    gh_identity: Option<String>,

    #[command(subcommand)]
    command: Option<Commands>,
}

/// Shared by the `watch` flags and the bare-`fleet` default, so they can't drift.
const DEFAULT_INTERVAL: u64 = 5;
const DEFAULT_STUCK: i64 = 300;

/// `--rows`: how tall one session's item is.
#[derive(Clone, Copy, clap::ValueEnum)]
enum RowsArg {
    #[value(name = "1", alias = "compact")]
    One,
    #[value(name = "2", alias = "full")]
    Two,
    Auto,
}

impl From<RowsArg> for Option<bool> {
    fn from(r: RowsArg) -> Self {
        match r {
            RowsArg::One => Some(false),
            RowsArg::Two => Some(true),
            RowsArg::Auto => None,
        }
    }
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum HarnessArg {
    Claude,
    Codex,
    Pi,
}

impl From<HarnessArg> for fleet::core::harness::Harness {
    fn from(h: HarnessArg) -> Self {
        use fleet::core::harness::Harness;
        match h {
            HarnessArg::Claude => Harness::Claude,
            HarnessArg::Codex => Harness::Codex,
            HarnessArg::Pi => Harness::Pi,
        }
    }
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum BackendArg {
    Iterm,
    Tmux,
}

impl From<BackendArg> for Backend {
    fn from(b: BackendArg) -> Self {
        match b {
            BackendArg::Iterm => Backend::Iterm,
            BackendArg::Tmux => Backend::Tmux,
        }
    }
}

#[derive(Subcommand)]
enum Commands {
    /// List the live Claude sessions
    #[command(visible_alias = "ls")]
    List {
        /// Machine-readable output (a JSON array; every row carries `host`)
        #[arg(long)]
        json: bool,
        /// Every configured host, in parallel
        #[arg(long, short = 'a')]
        all_hosts: bool,
    },

    /// Read what a session is currently showing
    Peek {
        /// generated title, session name (e.g. app-f9), sessionId prefix, or pid;
        /// `pid:<n>` / `id:<session_id>` match exactly, never by fragment
        target: String,
        /// How many trailing lines to show
        #[arg(long, default_value_t = 40)]
        lines: usize,
        /// One JSON object: the screen unclipped (`lines`, `cols`, `rows`, `status`), no header
        #[arg(long)]
        json: bool,
        /// With --json: keep the escape sequences (colours) in `lines`
        #[arg(long, requires = "json")]
        ansi: bool,
    },

    /// A session's conversation from its transcript: prompts, replies, tool calls, questions
    Messages {
        /// generated title, session name, sessionId prefix, or pid
        target: String,
        /// One JSON object: entries, paging flags and a cursor (failures: { ok: false, kind, error })
        #[arg(long)]
        json: bool,
        /// Only entries newer than this cursor, oldest first (poll with the last response's cursor)
        #[arg(long, value_name = "CURSOR")]
        after: Option<String>,
        /// Only entries older than this cursor ("show earlier")
        #[arg(long, value_name = "CURSOR")]
        before: Option<String>,
        /// How many entries (default 100, max 500; --after returns all unless given)
        #[arg(long, value_name = "N")]
        limit: Option<String>,
    },

    /// Answer the prompt a session waits on: a question (--question), a permission or approval prompt (--choice)
    Answer {
        /// generated title, session name, sessionId prefix, or pid
        target: String,
        /// One JSON object (failures: { ok: false, kind, error, detail, pane })
        #[arg(long)]
        json: bool,
        /// Only show the prompt on screen: its answers and id, and whether it can be answered now (types nothing)
        #[arg(long)]
        list: bool,
        /// Pick this numbered answer of the permission / approval prompt on screen
        #[arg(long, value_name = "N")]
        choice: Option<String>,
        /// With --choice: only if the screen still shows this prompt (its id from --list)
        #[arg(long, value_name = "ID")]
        prompt: Option<String>,
        /// The AskUserQuestion to answer (its tool_use id, from `fleet messages`)
        #[arg(long, value_name = "TOOL_USE_ID")]
        question: Option<String>,
        /// With --question: `[{"options":[index…],"text":"…"}]`, one per question, in order
        #[arg(long, value_name = "JSON")]
        answers: Option<String>,
        /// Answer even with a terminal attached to the session (a human could be typing)
        #[arg(long)]
        allow_attached: bool,
    },

    /// Read or answer the untouched folder-trust prompt in an exact local tmux Session host
    #[command(group(clap::ArgGroup::new("mode").required(true).args(["list", "accept", "decline"])))]
    Trust {
        /// Exact tmux session name returned by spawn
        tmux_session: String,
        /// One JSON object, including content-free failure codes
        #[arg(long)]
        json: bool,
        /// Require the host still to hold this pane id (for example %12)
        #[arg(long, value_name = "HANDLE")]
        pane: Option<String>,
        /// Read the dialog without typing or taking a lock
        #[arg(long, conflicts_with = "prompt")]
        list: bool,
        /// Choose Yes, I trust this folder
        #[arg(long, requires = "prompt")]
        accept: bool,
        /// Choose No, exit
        #[arg(long, requires = "prompt")]
        decline: bool,
        /// Only answer this prompt id, returned by --list
        #[arg(long, value_name = "ID")]
        prompt: Option<String>,
        /// After declining, close the unchanged host once its harness has exited
        #[arg(long, requires = "decline", conflicts_with_all = ["list", "accept"])]
        close: bool,
        /// Answer even with a terminal client attached
        #[arg(long)]
        allow_attached: bool,
    },

    /// Type text into a session and submit it — held while its input box holds text
    ///
    /// Under the session's typing lock, fleet first reads the input box off the screen (tmux:
    /// with its text attributes). A placeholder or suggestion, drawn faint, counts as empty.
    /// Text in the box (Claude Code puts an interrupted prompt back there; typed text would go
    /// out with it as one prompt) or a prompt waiting for its answer holds the send: exit 3,
    /// nothing typed. No input box fleet can find, or one it cannot read (iTerm gives no text
    /// attributes; Claude Code's box shows nothing and its caret, which tells blanks typed
    /// there, cannot be found: the terminal's cursor not given, or hidden with no caret Claude
    /// Code painted in the box), types as before, with a warning; --require-empty holds then
    /// too. A failure of kind io says with --json whether anything was typed (`typed`). --force
    /// types without reading the box. After `keys Escape`, wait about a second before sending:
    /// Claude Code puts the stopped prompt back only once the turn has stopped.
    Send {
        /// sessionId prefix, derived name, or pid; `pid:<n>` / `id:<session_id>` match exactly
        target: String,
        /// The message to send
        text: String,
        /// Print one JSON object: `{ ok, target: { session_id, pid }, warning? }`, or `{ ok: false, kind, error }` (held: kind `send_held`, with `reason` and `input` — the box's text, which may be sensitive)
        #[arg(long)]
        json: bool,
        /// Type without reading the input box, whatever it holds
        #[arg(long)]
        force: bool,
        /// Hold also when fleet cannot find the input box or read it, not only when it holds text
        #[arg(long)]
        require_empty: bool,
    },

    /// Run by an agent inside its session: its work is finished and nothing waits on the user.
    /// Fleet shows the session as done (nothing closes it: the user does)
    Done {
        /// One line: what it did (kept with the mark)
        #[arg(long)]
        note: Option<String>,
        /// Take the mark back
        #[arg(long)]
        cancel: bool,
    },

    /// Ask a session to report itself done once its work is finished (`fleet done`)
    AskDone {
        /// sessionId prefix, derived name, or pid
        target: String,
    },

    /// Close a session but keep it dormant: its agent ends (and its tmux window, or the tmux
    /// session when that was its only window), and `fleet restore <target>` resumes it later.
    /// The app calls this Deactivate, and the dormant list Inactive
    #[command(visible_alias = "deactivate")]
    Sleep {
        /// sessionId prefix, derived name, or pid
        target: String,
        /// `{ host, sessionId, title, process, terminal, dormant: DormantView, dryRun }`
        #[arg(long)]
        json: bool,
    },

    /// Press named keys in a session's terminal (Escape interrupts a turn)
    Keys {
        /// sessionId prefix, derived name, or pid; `pid:<n>` / `id:<session_id>` match exactly
        target: String,
        /// Escape, Enter, Up, Down, Left, Right, Tab, BTab, Space or C-c — pressed in order
        #[arg(required = true, value_name = "KEY")]
        keys: Vec<String>,
        /// Print one JSON object: `{ ok, target: { session_id, pid }, keys }`, or `{ ok: false, kind, error }`
        #[arg(long)]
        json: bool,
    },

    /// End a session alone in its own tmux session: its exit command, then kill that tmux session
    Close {
        /// `id:<session_id>` or `pid:<n>` — exact only, never a fragment
        target: String,
        /// One JSON object: `{ ok, session_id, pid, harness, how, tmux_session, dry_run }`, or `{ ok: false, kind, error }`
        #[arg(long)]
        json: bool,
        /// Seconds the agent gets to exit after its exit command, before its tmux session is killed
        #[arg(long, value_name = "SECONDS", default_value_t = 5, value_parser = clap::value_parser!(u64).range(0..=600))]
        timeout: u64,
        /// Close only if the session's process is still this pid (with --expect-start): a list row's `pid`
        #[arg(long, value_name = "PID", requires = "expect_start")]
        expect_pid: Option<i64>,
        /// Close only if the session's process started then (with --expect-pid): a list row's `proc_start`
        #[arg(long, value_name = "PROC_START", requires = "expect_pid")]
        expect_start: Option<String>,
    },

    /// Switch a Claude Code session's model, as /model <id> in its terminal — confirming the switch when Claude Code asks
    ///
    /// Exact targets only. Under the session's typing lock, before typing, in this order: the
    /// agent owns its pane (as `close` checks it), its status (Claude Code's registry, read
    /// afresh) is idle, the screen shows no working line over the input box, and the box is
    /// empty (as `send --require-empty` reads it). Then it types `/model <id>` and Enter, and
    /// reads the screen and the transcript for up to 5 s: Claude Code's confirmation for that
    /// model (it asks while the prompt cache is warm) gets the key its first choice names, once
    /// the agent is checked to own its pane again and the screen, read again, still shows it;
    /// nothing else is pressed. Done once the transcript says "Set model to <that model>": the
    /// id itself or its label (aliases through `fleet harness list`'s models; `[1m]` and date
    /// suffixes the same model). The model also becomes Claude Code's default for new
    /// sessions, as /model makes it.
    ///
    /// outcome_uncertain: no "Set model to" within the wait, one naming another model or one
    /// fleet cannot match to the id ("Default (recommended)"), "Kept model as …" — the switch
    /// declined in the terminal, the model unchanged — or tmux failing or not answering once
    /// the command was typed.
    Model {
        /// `id:<session_id>` or `pid:<n>` — exact only, never a fragment
        target: String,
        /// The model id (`claude-sonnet-5-5`): letters, digits and . _ [ ] - / : only
        #[arg(value_name = "MODEL_ID")]
        model: String,
        /// One JSON object: `{ ok, host, session_id, pid, harness, model, confirmed, switched_at, dry_run }`, or `{ ok: false, host, kind, error }` (`reason`, `status`, `input` for some kinds; held: kind `send_held`, exit 3)
        #[arg(long)]
        json: bool,
        /// Switch only if the session's process is still this pid (with --expect-start): a list row's `pid`
        #[arg(long, value_name = "PID", requires = "expect_start")]
        expect_pid: Option<i64>,
        /// Switch only if the session's process started then (with --expect-pid): a list row's `proc_start`
        #[arg(long, value_name = "PROC_START", requires = "expect_pid")]
        expect_start: Option<String>,
        /// Hold also when fleet cannot find the input box or read it, as `send --require-empty` does (always on for model: accepted so callers can pass it to send, rename and model alike)
        #[arg(long)]
        require_empty: bool,
    },

    /// Switch a Claude Code session's effort level, as /effort <level> in its terminal — confirming the switch when Claude Code asks
    ///
    /// Exact targets only, with `model`'s checks under the session's typing lock (the agent
    /// owns its pane, its status is idle, no working line, an empty input box). Then it types
    /// `/effort <level>` and Enter, and reads the screen and the transcript for up to 5 s:
    /// Claude Code's "Change effort level?" confirmation for that level (it asks while the
    /// prompt cache is warm) gets the key its first choice names, once the agent is checked to
    /// own its pane again and the screen, read again, still shows it; nothing else is pressed.
    /// Done once the transcript says "Set effort level to <level>" (`Effort level set to auto`
    /// for `auto`). `saved`: the level also became Claude Code's default for new sessions on
    /// the session's model (`low` to `xhigh`; `auto` clears the saved level), not `max`, which
    /// is for this session only.
    ///
    /// outcome_uncertain: no answer within the wait, one setting another level, "Kept effort
    /// level as …" — declined in the terminal, the level unchanged — an answer fleet does not
    /// read as a switch (quoted), or tmux failing or not answering once the command was typed.
    Effort {
        /// `id:<session_id>` or `pid:<n>` — exact only, never a fragment
        target: String,
        /// The level (`low`, `medium`, `high`, `xhigh`, `max`, `auto`): lowercase letters only, at most 16; `fleet harness list --json` lists each model's `efforts`
        #[arg(value_name = "LEVEL")]
        level: String,
        /// One JSON object: `{ ok, host, session_id, pid, harness, effort, confirmed, saved, switched_at, dry_run }`, or `{ ok: false, host, kind, error }` (`reason`, `status`, `input` for some kinds; held: kind `send_held`, exit 3)
        #[arg(long)]
        json: bool,
        /// Switch only if the session's process is still this pid (with --expect-start): a list row's `pid`
        #[arg(long, value_name = "PID", requires = "expect_start")]
        expect_pid: Option<i64>,
        /// Switch only if the session's process started then (with --expect-pid): a list row's `proc_start`
        #[arg(long, value_name = "PROC_START", requires = "expect_pid")]
        expect_start: Option<String>,
        /// Hold also when fleet cannot find the input box or read it, as `send --require-empty` does (always on for effort: accepted so callers can pass it to send, rename, model and effort alike)
        #[arg(long)]
        require_empty: bool,
    },

    /// Rename a session: Claude's own /rename (the title), then its tmux session follows
    Rename {
        /// sessionId prefix, derived name, or pid; `pid:<n>` / `id:<session_id>` match exactly
        target: String,
        /// The new display name
        name: String,
        /// Leave the session's tmux session name alone
        #[arg(long)]
        no_tmux_sync: bool,
        /// Rename even a session waiting on a prompt (the keys land in the prompt — be sure)
        #[arg(long)]
        force: bool,
        /// Hold also when fleet cannot find the input box or read it, not only when it holds text
        #[arg(long)]
        require_empty: bool,
        /// Print one JSON object (result renamed/sent/held, tmux sync); held exits 3
        #[arg(long)]
        json: bool,
    },

    /// Suggest a name for a session from what it is actually working on
    Name {
        /// sessionId prefix, derived name, or pid (omit with --all)
        target: Option<String>,
        /// Every session still carrying Claude's cwd+hash name
        #[arg(long)]
        all: bool,
        /// Send the suggestion as /rename instead of only printing it
        /// (the default is to print what would be renamed; -n/--dry-run says so explicitly)
        #[arg(long)]
        apply: bool,
        /// Leave the session's tmux session name alone
        #[arg(long)]
        no_tmux_sync: bool,
        /// Ignore the cached name and generate (and store) a fresh one
        #[arg(long, alias = "no-cache")]
        refresh: bool,
        /// With --all: skip sessions that have said too little to name — wait for a
        /// second prompt or a brief (what the web server's periodic pass uses)
        #[arg(long, requires = "all")]
        settled: bool,
    },

    /// Sort sessions into work-stream groups (the web Board view) — reads sessions, asks `claude -p`, sends nothing
    Group {
        /// Every configured host, in parallel
        #[arg(long, short = 'a')]
        all_hosts: bool,
        /// Machine-readable output
        #[arg(long)]
        json: bool,
        /// Save the result to the state file (without it: print only)
        #[arg(long)]
        apply: bool,
        /// Forget all groups and regroup every session from scratch
        #[arg(long)]
        refresh: bool,
        /// Run the merge/rename consolidation pass now, even if not due
        #[arg(long)]
        consolidate: bool,
        /// Print the stored groups; no discovery, no model call
        #[arg(long, conflicts_with_all = ["apply", "refresh", "consolidate", "all_hosts", "input"])]
        cached: bool,
        /// Read sessions from a file ("-" = stdin): a `list --json` array or a `/api/fleet` body
        #[arg(long, value_name = "FILE", conflicts_with = "all_hosts")]
        input: Option<String>,
        /// Rename a stored group (by id) to --label; the model never renames it again
        #[arg(long, value_name = "GROUP_ID", requires = "label", conflicts_with_all = ["apply", "refresh", "consolidate", "all_hosts", "input", "cached", "move_session"])]
        rename: Option<String>,
        /// Move a session (`host/sessionId`) to --to <group id>, or to a new group named --label; it stays there
        #[arg(long = "move", value_name = "HOST/ID", conflicts_with_all = ["apply", "refresh", "consolidate", "all_hosts", "input", "cached"])]
        move_session: Option<String>,
        /// With --move: the target group's id
        #[arg(
            long,
            value_name = "GROUP_ID",
            requires = "move_session",
            conflicts_with = "label"
        )]
        to: Option<String>,
        /// With --rename: the new label; with --move: the label of a new group (reused when one has it);
        /// with --create: the new group's label
        #[arg(long)]
        label: Option<String>,
        /// Create an empty group named --label (yours: it stays, empty or not, until --delete)
        #[arg(long, requires = "label", conflicts_with_all = ["apply", "refresh", "consolidate", "all_hosts", "input", "cached", "rename", "move_session", "delete"])]
        create: bool,
        /// Delete an empty group (by id)
        #[arg(long, value_name = "GROUP_ID", conflicts_with_all = ["apply", "refresh", "consolidate", "all_hosts", "input", "cached", "rename", "move_session", "label"])]
        delete: Option<String>,
    },

    /// A session's brief: what it is doing, what it produced, its todos (read, edit, regenerate, open)
    Brief {
        /// generated title, session name, sessionId prefix, or pid — or the full id of a gone
        /// session whose brief is still there
        target: String,
        /// The parsed brief as JSON (the web API's shape, plus `body` and `path`)
        #[arg(long, conflicts_with = "prompt")]
        json: bool,
        /// Only the prompt that continues this session's work in a new one (for piping)
        #[arg(long)]
        prompt: bool,
        /// Edit the body in $VISUAL / $EDITOR (saved as a human edit)
        #[arg(long, conflicts_with_all = ["set", "regenerate", "prompt"])]
        edit: bool,
        /// Save the markdown on stdin as a human edit
        #[arg(long, conflicts_with_all = ["regenerate", "prompt"])]
        set: bool,
        /// With --set: refuse (exit 3) unless the stored brief's `updated` is this ("" = none yet)
        #[arg(long, requires = "set", value_name = "ISO")]
        expect_updated: Option<String>,
        /// Ask this host's web server to regenerate it (in the background; capped per hour)
        #[arg(long, conflicts_with = "prompt")]
        regenerate: bool,
        /// Open the session's git root (else cwd) in VS Code (`code`; `cursor` with
        /// web.editor "cursor") — through Remote-SSH for a session on another host
        #[arg(long, conflicts_with_all = ["json", "prompt", "edit", "set", "regenerate"])]
        open: bool,
    },

    /// Spawn a new agent session (Claude, Codex or pi) in a fresh tab/pane.
    /// Fresh Claude/Codex sessions accept --effort and --skip-permissions for this run only.
    Spawn {
        /// Initial prompt (optional; with --from, added after the continue prompt)
        prompt: Option<String>,
        /// Continue this session's work: same host and cwd, its brief's continue prompt first
        #[arg(long, value_name = "SESSION")]
        from: Option<String>,
        /// Resume this session (its full id) with the harness's own resume, in --dir; no prompt
        #[arg(long, value_name = "SESSION_ID")]
        resume: Option<String>,
        /// Working directory (defaults to the current directory)
        #[arg(long)]
        dir: Option<String>,
        /// Backend to spawn into (default: tmux inside tmux, over ssh or off macOS; else iterm)
        #[arg(long, value_enum)]
        backend: Option<BackendArg>,
        /// Display name for the new session (as shown by `list`/`watch`)
        #[arg(long)]
        name: Option<String>,
        /// Model for the new session (`<harness> --model <id>`; default: the harness's own)
        #[arg(long, value_name = "ID")]
        model: Option<String>,
        /// Effort for this session only (Claude/Codex; lowercase letters, at most 16)
        #[arg(long, value_name = "LEVEL")]
        effort: Option<String>,
        /// Skip permission prompts (Claude/Codex); also disables Codex's sandbox
        #[arg(long)]
        skip_permissions: bool,
        /// Coding agent to run: claude (default), codex or pi
        #[arg(long, value_enum)]
        harness: Option<HarnessArg>,
        /// Open a window in this tmux session instead of a new session per job (tmux only)
        #[arg(long)]
        tmux_session: Option<String>,
        /// Open a new window instead of a tab (iterm backend only)
        #[arg(long)]
        window: bool,
        /// The session reports itself done once its work is finished (`fleet done`; tmux only)
        #[arg(long)]
        ask_done: bool,
        /// Start linked to this Slack thread, channel or @person (tmux, on `slack.host` only):
        /// the prompt goes in with the link's intro, so nothing runs before it knows
        #[arg(long, value_name = "URL", conflicts_with = "from")]
        slack: Option<String>,
        /// How long the --slack link lasts: minutes, or like 30m, 2h, 2d (max 2 days)
        #[arg(
            long,
            value_name = "DURATION",
            default_value = "2h",
            requires = "slack"
        )]
        slack_for: String,
        /// The --slack link's instructions: what it may help with (default: the work of this session)
        #[arg(long, value_name = "TEXT", requires = "slack")]
        slack_task: Option<String>,
        /// The --slack link sees every message and steps in when it should (default: woken when mentioned)
        #[arg(long, requires = "slack")]
        slack_proactive: bool,
        /// Print one JSON object: where it landed, or `{ ok: false, kind, error }` on failure
        #[arg(long, conflicts_with = "slack")]
        json: bool,
    },

    /// Coding-agent harnesses on this host (claude, codex, pi): installed, command, models
    Harness {
        #[command(subcommand)]
        cmd: HarnessCmd,
    },

    /// GitHub identities spawned sessions can act as (`ghIdentities` in the config)
    GhIdentity {
        #[command(subcommand)]
        cmd: GhIdentityCmd,
    },

    /// Session stacks: sessions sharing one context file (the StackBrief); spawn siblings
    Stack {
        #[command(subcommand)]
        cmd: StackCmd,
    },

    /// Slack links: a session talks, for a while, with the people in one Slack thread (`slack.host`)
    Slack {
        #[command(subcommand)]
        cmd: Option<SlackCmd>,
    },

    /// Usage limits: Claude's session (5h), weekly, per-model weekly, extra usage; Codex's 5h and weekly
    Usage {
        /// Machine-readable output (the web API's shape)
        #[arg(long)]
        json: bool,
        /// Skip the 60s cache (a failed read still backs off for 2 minutes)
        #[arg(long)]
        refresh: bool,
    },

    /// Hand the current work off to a fresh session in another window
    Handoff {
        /// The brief for the new session (or use --file / stdin)
        brief: Option<String>,
        /// Read the brief from a file ("-" for stdin)
        #[arg(long)]
        file: Option<String>,
        /// Working directory for the new session (defaults to the current directory)
        #[arg(long)]
        dir: Option<String>,
        /// Backend to hand off into (same default as spawn)
        #[arg(long, value_enum)]
        backend: Option<BackendArg>,
        /// Display name for the new session (as shown by `list`/`watch`)
        #[arg(long)]
        name: Option<String>,
        /// Model for the new session (`<harness> --model <id>`; default: the harness's own)
        #[arg(long, value_name = "ID")]
        model: Option<String>,
        /// Coding agent to run: claude (default), codex or pi
        #[arg(long, value_enum)]
        harness: Option<HarnessArg>,
        /// Open a window in this tmux session instead of a new session per job (tmux only)
        #[arg(long)]
        tmux_session: Option<String>,
        /// Open a tab instead of a new window (iterm backend only)
        #[arg(long)]
        tab: bool,
        /// Return immediately instead of waiting for the new session to register
        #[arg(long)]
        no_wait: bool,
    },

    /// Live dashboard + macOS notifications on finished/stuck sessions (default)
    Watch {
        /// Poll interval in seconds
        #[arg(long, default_value_t = DEFAULT_INTERVAL)]
        interval: u64,
        /// Seconds a session must sit idle-on-a-prompt before it counts as stuck
        #[arg(long, default_value_t = DEFAULT_STUCK)]
        stuck: i64,
        /// Notifications only, no TUI (backgroundable)
        #[arg(long)]
        quiet: bool,
        /// Item height: 2/full (default; 3 lines on a phone), 1/compact, or auto (`z` persists it)
        #[arg(long, value_enum)]
        rows: Option<RowsArg>,
        /// Capture mouse/tap input in the TUI (default; a tap selects and focuses)
        #[arg(long)]
        mouse: bool,
        /// Leave the mouse to the terminal, so selection and copy behave normally
        #[arg(long, conflicts_with = "mouse")]
        no_mouse: bool,
    },

    /// tmux sessions: list, enter, last, new, kill, rename, stale (default: list)
    #[command(visible_alias = "t")]
    Tmux {
        #[command(subcommand)]
        cmd: Option<TmuxCmd>,
    },

    /// Attach to a tmux session by exact > prefix > substring match (= tmux enter)
    #[command(visible_alias = "e")]
    Enter {
        /// Session name or fragment (case-insensitive)
        query: String,
    },

    /// Attach to the tmux session you were in before this one (= tmux last)
    Last,

    /// Sessions a reboot left dormant: list them (default), bring one or all back, or forget them
    /// (--closed: the same for sessions closed recently, within a boot)
    Restore {
        /// Dormant tmux name (exact > prefix > substring), session id (prefix of 4+) or title
        #[arg(conflicts_with_all = ["all", "forget", "forget_all", "list"])]
        target: Option<String>,
        /// Work on the recently closed sessions instead (ended within a boot: Close, kill,
        /// /exit), kept for config restore.keepClosedDays
        #[arg(long, conflicts_with = "all")]
        closed: bool,
        /// List the dormant sessions (the default)
        #[arg(long, conflicts_with_all = ["all", "forget", "forget_all"])]
        list: bool,
        /// Restore every dormant session
        #[arg(long, conflicts_with_all = ["forget", "forget_all"])]
        all: bool,
        /// Drop a dormant session without restoring it
        #[arg(long, value_name = "TARGET", conflicts_with = "forget_all")]
        forget: Option<String>,
        /// Drop every dormant session
        #[arg(long)]
        forget_all: bool,
        /// `{ host, bootId, dormant: [DormantView…], closed: [ClosedView…] }`; with a target or --all
        /// `{ host, restored: […], failed: […] }`; with --forget `{ host, forgotten: […] }`
        #[arg(long)]
        json: bool,
    },

    /// Attach to, or create, a tmux session (= tmux new)
    New(NewArgs),

    /// Run a command on a host — default: defaultHost (exit code passes through)
    Exec {
        /// Working directory (absolute or ~/…; default ~)
        #[arg(short = 'C', long)]
        dir: Option<String>,
        /// Give the command a terminal
        #[arg(short = 't', long)]
        tty: bool,
        /// The command and its arguments (after --)
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        argv: Vec<String>,
    },

    /// A plain login shell on a host — default: defaultHost (or one command), no tmux
    Ssh {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        argv: Vec<String>,
    },

    /// Reachability, tools and versions on this machine and every host, config sanity
    Doctor,

    /// Write this machine's config (interactive, or --yes with flags)
    Init(init::InitArgs),

    /// Read or change the config
    Config {
        #[command(subcommand)]
        action: config_cmd::ConfigAction,
    },

    /// Copy this binary (and the web app) to a remote host's ~/.local
    Install {
        /// Skip the web app
        #[arg(long)]
        no_web: bool,
        /// Install even when the remote OS/CPU differs
        #[arg(long)]
        force: bool,
    },

    /// Open GitHub pull requests that need you: yours, waiting on your review, waiting on their author
    Prs {
        /// Machine-readable output
        #[arg(long)]
        json: bool,
        /// Only this column: waiting-on-me, review-mine, ready-to-merge-mine, draft-mine, waiting-on-author
        #[arg(long)]
        column: Option<String>,
        /// Fetch now, whatever the cache's age
        #[arg(long, conflicts_with = "cached")]
        refresh: bool,
        /// Never fetch: show the cache, however old
        #[arg(long)]
        cached: bool,
    },

    /// One pull request: check it out in a worktree, or start a session on it
    Pr {
        #[command(subcommand)]
        cmd: PrCmd,
    },

    /// Your open Productive tasks: due soon / overdue, in progress, to do
    Tasks {
        /// Machine-readable output
        #[arg(long)]
        json: bool,
        /// Only this column: due, in-progress, to-do
        #[arg(long)]
        column: Option<String>,
        /// Fetch now, whatever the cache's age
        #[arg(long, conflicts_with = "cached")]
        refresh: bool,
        /// Never fetch: show the cache, however old
        #[arg(long)]
        cached: bool,
    },

    /// One Productive task: show it, move it to another status, or start a session on it
    Task {
        #[command(subcommand)]
        cmd: TaskCmd,
    },

    /// Keep the git repos under the configured roots fresh (fetch + fast-forward only)
    Repos {
        #[command(subcommand)]
        cmd: Option<ReposCmd>,
        /// Machine-readable output
        #[arg(long)]
        json: bool,
        /// Include excluded repos (marked `excluded`)
        #[arg(long)]
        all: bool,
    },

    /// One-off sessions started later on this host: at a set time, or on a Productive task's start date
    Schedule {
        #[command(subcommand)]
        cmd: Option<ScheduleCmd>,
        /// Machine-readable output
        #[arg(long)]
        json: bool,
    },

    /// Bots: agent sessions started from a recipe (prompt + schedule) on this host (config `bots`)
    #[command(visible_alias = "jobs")]
    Bots {
        #[command(subcommand)]
        cmd: Option<JobsCmd>,
        /// Machine-readable output
        #[arg(long)]
        json: bool,
    },

    /// The web UI: serve it, or install it as a login service
    Web {
        #[command(subcommand)]
        action: WebCmd,
    },

    /// Manage the Claude Code skill file
    Skill {
        #[command(subcommand)]
        action: skill::SkillAction,
    },

    /// Serve every command as an MCP tool: on stdio (`claude mcp add fleet -- fleet mcp`), or
    /// over HTTP for a remote agent runtime (`--http`)
    Mcp {
        /// Serve Streamable HTTP at http://<bind>:<port>/mcp instead of stdio
        #[arg(long)]
        http: bool,
        /// With --http: address to listen on
        #[arg(long, default_value = "127.0.0.1", requires = "http")]
        bind: String,
        /// With --http: port to listen on
        #[arg(long, default_value_t = 7778, requires = "http")]
        port: u16,
        /// With --http: file holding the bearer token every request must carry (else env FLEET_MCP_TOKEN)
        #[arg(long, value_name = "FILE", requires = "http")]
        token_file: Option<String>,
        /// Only these tools, comma-separated; `stack_*` globs (default: all; over HTTP all but
        /// exec, install, init, config_set, web_build, skill_install and *_install_service)
        #[arg(long, value_delimiter = ',', value_name = "TOOLS")]
        tools: Vec<String>,
        /// Leave these tools out, comma-separated; `stack_*` globs
        #[arg(long, value_delimiter = ',', value_name = "TOOLS")]
        exclude: Vec<String>,
    },

    /// Machine facts as JSON, for `doctor` (internal)
    #[command(name = "_probe", hide = true)]
    Probe,
}

#[derive(clap::Args, Clone)]
struct NewArgs {
    /// Session name (default: main); sanitised to [A-Za-z0-9_-]
    name: Option<String>,
    /// Create it and stay where you are
    #[arg(short = 'd', long)]
    detach: bool,
    /// Start it in this directory (absolute or ~/… for another host)
    #[arg(short = 'C', long)]
    dir: Option<String>,
    /// Command to run in the new session (after --)
    #[arg(last = true)]
    cmd: Vec<String>,
}

#[derive(Subcommand, Clone)]
enum HarnessCmd {
    /// Each harness: id, label, launch command, installed, models for the picker
    List {
        #[arg(long)]
        json: bool,
        /// Skip the model lists (pi's needs a `pi --list-models` run)
        #[arg(long)]
        no_models: bool,
    },
}

#[derive(Subcommand)]
enum SlackCmd {
    /// The bridge (connected?) and the active links
    List {
        #[arg(long)]
        json: bool,
    },
    /// Link a session to a Slack thread, channel or DM for a while (default: this session)
    Link {
        /// A Slack link: a message (its thread), or a channel / DM (all of it); or a person
        /// (@name or their user id): the app's DM with them
        url: String,
        /// How long: minutes, or like 30m, 2h, 1h30m, 2d (max 2 days)
        #[arg(long = "for", value_name = "DURATION")]
        duration: String,
        /// The session to link (default: the one this command runs in, by $TMUX_PANE)
        #[arg(long, value_name = "TARGET")]
        session: Option<String>,
        /// Your instructions: what it may help with, tone, when to step in (default: the work
        /// of this session)
        #[arg(long)]
        task: Option<String>,
        /// See every message and step in when it should (default: woken only when someone
        /// mentions the app or DMs it, or writes in a thread it posted in; the rest goes along as
        /// context)
        #[arg(long)]
        proactive: bool,
        #[arg(long)]
        json: bool,
    },
    /// End a link now
    Unlink {
        /// The link id (sl-xxxxxx)
        link: String,
        #[arg(long)]
        json: bool,
    },
    /// Post in a linked thread — what a linked session runs to answer there
    Reply {
        /// The link id (sl-xxxxxx)
        link: String,
        /// The message (Slack mrkdwn)
        text: String,
        /// For a channel link: the thread to answer in (a message ts)
        #[arg(long, value_name = "TS")]
        thread: Option<String>,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum GhIdentityCmd {
    /// Each identity: name, GitHub login, the gh wrapper it runs
    List {
        #[arg(long)]
        json: bool,
    },
    /// The launch command of a session with this identity (writes its shim; the web server's)
    Launcher {
        name: String,
        /// The launcher to wrap (default: the Claude launcher, `FLEET_CMD` / config `claude`)
        #[arg(long)]
        launcher: Option<String>,
    },
}

#[derive(Subcommand, Clone)]
enum StackCmd {
    /// Stacks on this host (after a membership sync)
    #[command(visible_alias = "ls")]
    List {
        /// `{ host, stacks: [StackView…] }`
        #[arg(long)]
        json: bool,
    },
    /// Print a stack's StackBrief (body, no frontmatter)
    Show {
        /// stack id, label (or a fragment of it), or a member session target
        stack: String,
        /// The StackView JSON
        #[arg(long, conflicts_with = "path")]
        json: bool,
        /// Only the file's path
        #[arg(long)]
        path: bool,
    },
    /// Create a stack around a live session (one model call writes Summary / Resources)
    New {
        /// The session the stack starts from
        #[arg(long, value_name = "TARGET")]
        from: String,
        /// Stack label (default: the model's, else the session's title)
        #[arg(long)]
        label: Option<String>,
        /// Skip the model call: write the skeleton StackBrief
        #[arg(long)]
        no_llm: bool,
        #[arg(long)]
        json: bool,
    },
    /// The session's stack, created like `new` when it has none
    Ensure {
        target: String,
        #[arg(long)]
        label: Option<String>,
        #[arg(long)]
        no_llm: bool,
        #[arg(long)]
        json: bool,
    },
    /// Spawn a sibling: a new session in <target>'s stack (created if needed), same cwd
    Spawn {
        /// The session whose stack the sibling joins
        target: String,
        /// The sibling's first prompt (after the stack's context line)
        prompt: Option<String>,
        /// Display name for the new session
        #[arg(long)]
        name: Option<String>,
        /// Model for the new session (`<harness> --model <id>`)
        #[arg(long, value_name = "ID")]
        model: Option<String>,
        /// Coding agent to run: claude (default), codex or pi
        #[arg(long, value_enum)]
        harness: Option<HarnessArg>,
        /// Working directory (default: <target>'s cwd)
        #[arg(long)]
        dir: Option<String>,
        /// Backend to spawn into (same default as spawn)
        #[arg(long, value_enum)]
        backend: Option<BackendArg>,
        /// Open a window in this tmux session instead of a new session (tmux only)
        #[arg(long)]
        tmux_session: Option<String>,
        /// Open a new window instead of a tab (iterm only)
        #[arg(long)]
        window: bool,
        /// Don't wait for the new session to register (prints the `fleet stack add` to run)
        #[arg(long)]
        no_wait: bool,
        /// Start the sibling on another host (the stack stays on this one; --dir is a path there)
        #[arg(long, value_name = "HOST")]
        on: Option<String>,
        /// Label for a stack created by this spawn
        #[arg(long)]
        label: Option<String>,
        /// Skip the model call when a stack is created
        #[arg(long)]
        no_llm: bool,
        #[arg(long)]
        json: bool,
    },
    /// Add a live session to a stack (one of this host's, or --on another host's)
    Add {
        stack: String,
        /// A session target; with --on, resolved among that host's sessions
        target: String,
        /// The host the session runs on (default: this one); the stack stays here
        #[arg(long, value_name = "HOST")]
        on: Option<String>,
        /// Other hosts' sessions: a `list --json` array or an /api/fleet body (`-` = stdin)
        #[arg(long, value_name = "SRC")]
        input: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Drop a member (a session target, or a full session id for a gone one)
    Remove {
        stack: String,
        target: String,
        #[arg(long)]
        json: bool,
    },
    /// Rename a stack (its label; the StackBrief header follows)
    Rename {
        stack: String,
        /// The new label (one line, up to 80 characters)
        label: String,
        #[arg(long)]
        json: bool,
    },
    /// Edit the StackBrief in $VISUAL / $EDITOR (saved as a human edit)
    Edit {
        stack: String,
        #[arg(long)]
        json: bool,
    },
    /// Save the markdown on stdin as a human edit
    Set {
        stack: String,
        /// Refuse (exit 3) unless the stored `updated` is this
        #[arg(long, value_name = "ISO")]
        expect_updated: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Reconcile members with the live sessions (gone → closed)
    Sync {
        /// Other hosts' sessions too: a `list --json` array or an /api/fleet body (`-` = stdin)
        #[arg(long, value_name = "SRC")]
        input: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Delete a stack's file
    Rm {
        stack: String,
        /// Don't ask
        #[arg(short, long)]
        force: bool,
        /// `{ removed: id }` (needs -f)
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand, Clone)]
enum TmuxCmd {
    /// Sessions, most recently attached first
    #[command(visible_alias = "ls")]
    List {
        /// Names only
        #[arg(short, long)]
        quiet: bool,
        /// JSON array of sessions
        #[arg(long)]
        json: bool,
    },
    /// Attach by exact > prefix > substring match (case-insensitive)
    #[command(visible_alias = "e")]
    Enter { query: String },
    /// Attach to the session you were in before this one
    Last,
    /// Attach to, or create, a session
    New(NewArgs),
    /// Kill a session (asks first)
    Kill {
        query: String,
        /// Don't ask
        #[arg(short, long)]
        force: bool,
    },
    /// Rename a tmux session (a Claude session's own name: `fleet rename`)
    Rename { query: String, new_name: String },
    /// Idle shells nothing is using (lists only, without --kill)
    Stale {
        /// Kill the candidates (each confirmed, unless -f)
        #[arg(long)]
        kill: bool,
        /// Kill without asking per session
        #[arg(short, long)]
        force: bool,
        /// Candidate names only
        #[arg(short, long)]
        quiet: bool,
        /// The report as JSON
        #[arg(long)]
        json: bool,
        /// Skip the live-Claude cross-check
        #[arg(long)]
        no_fleet_check: bool,
        /// Idle threshold: 30m, 12h, 7d, or hours
        #[arg(long, default_value = "24h")]
        older_than: String,
    },
}

#[derive(Subcommand, Clone)]
enum PrCmd {
    /// Check the PR out in a worktree under prs.worktrees (or find the one it is in)
    Checkout {
        /// owner/repo#123, repo#123 (from your cached PRs) or the PR's URL
        pr: String,
        #[arg(long)]
        json: bool,
    },
    /// Mark the PR's @mentions of you seen (out of the Unseen mentions column)
    Seen {
        /// owner/repo#123, repo#123 (from your cached PRs) or the PR's URL
        pr: String,
        #[arg(long)]
        json: bool,
    },
    /// Remove the worktrees under prs.worktrees whose PR merged or closed (lists them without --apply)
    CleanupWorktrees {
        /// Remove them (default: only list what would go and why the rest stays)
        #[arg(long)]
        apply: bool,
        #[arg(long)]
        json: bool,
    },
    /// Check the PR out, then start a session in its worktree that is told what the PR is
    Spawn {
        /// owner/repo#123, repo#123 (from your cached PRs) or the PR's URL
        pr: String,
        /// The first prompt (after the PR's context line)
        prompt: Option<String>,
        /// Backend to spawn into (default: tmux inside tmux, over ssh or off macOS; else iterm)
        #[arg(long, value_enum)]
        backend: Option<BackendArg>,
        /// Display name for the new session
        #[arg(long)]
        name: Option<String>,
        /// Model for the new session (`<harness> --model <id>`)
        #[arg(long, value_name = "ID")]
        model: Option<String>,
        /// Coding agent to run: claude (default), codex or pi
        #[arg(long, value_enum)]
        harness: Option<HarnessArg>,
        /// Open a window in this tmux session instead of a new session per job (tmux only)
        #[arg(long)]
        tmux_session: Option<String>,
        /// Open a new window instead of a tab (iterm backend only)
        #[arg(long)]
        window: bool,
    },
}

#[derive(Subcommand, Clone)]
enum TaskCmd {
    /// The task: project, status, due date, description
    Show {
        /// The task's id or its Productive URL
        task: String,
        #[arg(long)]
        json: bool,
    },
    /// Move the task to another status of its workflow (lists them without STATUS)
    Status {
        /// The task's id or its Productive URL
        task: String,
        /// A status name (or a unique prefix of one) or id
        status: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Start a session on the task's start date (follows the date if it moves; = schedule add --task)
    Schedule {
        /// The task's id or its Productive URL
        task: String,
        /// The first prompt (after the task's context line)
        prompt: Option<String>,
        /// The time on the start date (HH:MM, default 08:00)
        #[arg(long)]
        time: Option<String>,
        #[command(flatten)]
        spawn: ScheduleSpawnArgs,
    },
    /// Start a session that is told what the task is
    Spawn {
        /// The task's id or its Productive URL
        task: String,
        /// The first prompt (after the task's context line)
        prompt: Option<String>,
        /// Directory to start in (default: the current one)
        #[arg(short = 'C', long)]
        dir: Option<String>,
        /// Backend to spawn into (default: tmux inside tmux, over ssh or off macOS; else iterm)
        #[arg(long, value_enum)]
        backend: Option<BackendArg>,
        /// Display name for the new session
        #[arg(long)]
        name: Option<String>,
        /// Model for the new session (`<harness> --model <id>`)
        #[arg(long, value_name = "ID")]
        model: Option<String>,
        /// Coding agent to run: claude (default), codex or pi
        #[arg(long, value_enum)]
        harness: Option<HarnessArg>,
        /// Open a window in this tmux session instead of a new session per job (tmux only)
        #[arg(long)]
        tmux_session: Option<String>,
        /// Open a new window instead of a tab (iterm backend only)
        #[arg(long)]
        window: bool,
    },
}

#[derive(Subcommand, Clone)]
enum ScheduleCmd {
    /// Pending sessions, then the latest finished ones — the default
    List {
        #[arg(long)]
        json: bool,
        /// Every finished one kept, not just the latest 5
        #[arg(long)]
        all: bool,
    },
    /// Start a session later: at a time (`--at`), or on a task's start date (`--task`)
    Add {
        /// The first prompt (after the task's context line with --task)
        prompt: Option<String>,
        /// When: 'YYYY-MM-DD HH:MM', 'HH:MM' (the next one), 'tomorrow 09:00', 'mon 09:00', 'in 2h'
        #[arg(long, required_unless_present = "task")]
        at: Option<String>,
        /// A Productive task (id or URL): start on its start date, following it if it moves
        #[arg(long, conflicts_with = "at")]
        task: Option<String>,
        /// With --task: the time on the start date (HH:MM, default 08:00)
        #[arg(long, requires = "task")]
        time: Option<String>,
        #[command(flatten)]
        spawn: ScheduleSpawnArgs,
    },
    /// Cancel a pending session (id, or a unique prefix of one)
    #[command(visible_alias = "rm")]
    Cancel { id: String },
}

/// How a scheduled session starts — as for `fleet spawn` (always in tmux).
#[derive(clap::Args, Clone)]
struct ScheduleSpawnArgs {
    /// Directory to start in (default: the current one)
    #[arg(short = 'C', long)]
    dir: Option<String>,
    /// Display name for the new session (default with a task: task-<number>)
    #[arg(long)]
    name: Option<String>,
    /// Model for the new session (`<harness> --model <id>`)
    #[arg(long, value_name = "ID")]
    model: Option<String>,
    /// Coding agent to run: claude (default), codex or pi
    #[arg(long, value_enum)]
    harness: Option<HarnessArg>,
    #[arg(long)]
    json: bool,
}

#[derive(Subcommand, Clone)]
enum ReposCmd {
    /// Each repo's branch, position and last sync (local only, no network) — the default
    Status {
        #[arg(long)]
        json: bool,
        /// Include excluded repos (marked `excluded`)
        #[arg(long)]
        all: bool,
    },
    /// Fetch, then fast-forward the checked-out branch and the remote's default branch
    Sync {
        /// Repo names or ~/paths (default: every repo)
        names: Vec<String>,
        /// Only repos whose interval (repos.every / repos.overrides) is up
        #[arg(long)]
        due: bool,
        /// macOS notification when a repo newly diverges, is blocked, or keeps failing to fetch
        #[arg(long)]
        notify: bool,
        /// Print only updates and problems, timestamped
        #[arg(long, short = 'q')]
        quiet: bool,
        #[arg(long)]
        json: bool,
    },
    /// Run `repos sync --due` every 10 minutes and at login (launchd)
    InstallService {
        /// Remove the service instead
        #[arg(long)]
        uninstall: bool,
        /// Write the file but don't load/unload it
        #[arg(long)]
        no_load: bool,
        /// Only print the service definition
        #[arg(long)]
        print: bool,
    },
}

#[derive(Subcommand, Clone)]
enum JobsCmd {
    /// Every bot: schedule, next run, last run, live sessions — the default
    List {
        #[arg(long)]
        json: bool,
    },
    /// One bot: its settings and latest runs, each with the sessions it spawned
    Show {
        name: String,
        #[arg(long)]
        json: bool,
    },
    /// Start a run now, whatever the schedule says
    Run {
        name: String,
        /// Start even while a session of the bot is still working
        #[arg(long)]
        force: bool,
        /// A task for this run, added after the recipe's prompt — dir, launcher, model, GitHub
        /// identity and keep stay the recipe's
        #[arg(long, value_name = "TEXT")]
        prompt: Option<String>,
    },
    /// Fire due bots and close finished sessions (what the service runs every minute)
    Tick {
        /// Print only what happened, timestamped
        #[arg(long, short = 'q')]
        quiet: bool,
    },
    /// Resume a paused bot
    Enable { name: String },
    /// Pause a bot (its schedule stops firing; live sessions are left alone)
    Disable { name: String },
    /// Run `bots tick` every minute and at login (launchd)
    InstallService {
        /// Remove the service instead
        #[arg(long)]
        uninstall: bool,
        /// Write the file but don't load/unload it
        #[arg(long)]
        no_load: bool,
        /// Only print the service definition
        #[arg(long)]
        print: bool,
    },
}

#[derive(Subcommand, Clone)]
enum WebCmd {
    /// Run the web server here (node <web.dir>/server.mjs, with this config)
    Serve {
        #[arg(long)]
        port: Option<u16>,
        #[arg(long)]
        bind: Option<String>,
        /// Web app directory (default: config web.dir, else auto-detected)
        #[arg(long)]
        dir: Option<String>,
    },
    /// Build the React UI (<web.dir>/ui → ui/dist): npm ci when needed, then npm run build
    Build {
        /// Web app directory (default: config web.dir, else auto-detected)
        #[arg(long)]
        dir: Option<String>,
        /// Reinstall dependencies (npm ci) even if node_modules exists
        #[arg(long)]
        install: bool,
    },
    /// Keep `fleet web serve` running at login (launchd; prints a systemd unit elsewhere)
    InstallService {
        /// Remove the service instead
        #[arg(long)]
        uninstall: bool,
        /// Write the file but don't load/unload it
        #[arg(long)]
        no_load: bool,
        /// Only print the service definition
        #[arg(long)]
        print: bool,
    },
}

/// Bare `fleet` opens the dashboard for a human at a terminal, but stays a
/// one-shot `list` when stdout is piped — scripts and agents read that output.
fn default_command() -> Commands {
    if std::io::stdout().is_terminal() {
        Commands::Watch {
            interval: DEFAULT_INTERVAL,
            stuck: DEFAULT_STUCK,
            quiet: false,
            rows: None,
            mouse: false,
            no_mouse: false,
        }
    } else {
        Commands::List {
            json: false,
            all_hosts: false,
        }
    }
}

impl ScheduleSpawnArgs {
    fn into_opts(
        self,
        at: Option<String>,
        task: Option<String>,
        time: Option<String>,
        prompt: Option<String>,
        json: bool,
    ) -> schedule_cmd::AddOpts {
        schedule_cmd::AddOpts {
            at,
            task,
            time,
            prompt,
            dir: self.dir,
            name: self.name,
            model: self.model,
            harness: self.harness.map(Into::into).unwrap_or_default(),
            json: json || self.json,
        }
    }
}

/// How a command relates to hosts.
enum Placement {
    /// Re-run on the target host when it isn't this machine.
    Dispatch(Scope),
    /// Handles `--host` itself (or ignores it).
    Here,
}

fn placement(c: &Commands) -> Placement {
    match c {
        Commands::Trust { .. } => Placement::Here,
        Commands::Tmux { .. } | Commands::Enter { .. } | Commands::Last | Commands::New(_) => {
            Placement::Dispatch(Scope::DefaultHost)
        }
        Commands::Brief { edit: true, .. }
        | Commands::Brief { open: true, .. }
        | Commands::Stack {
            cmd: StackCmd::Edit { .. },
        } => Placement::Here,
        Commands::List {
            all_hosts: true, ..
        }
        | Commands::Doctor
        | Commands::Install { .. }
        | Commands::Exec { .. }
        | Commands::Ssh { .. }
        | Commands::Mcp { .. }
        | Commands::Done { .. }
        | Commands::Probe => Placement::Here,
        _ => Placement::Dispatch(Scope::SelfHost),
    }
}

fn target(cli_host: Option<&str>, local: bool, scope: Scope) -> Result<Target> {
    let env_host = std::env::var("FLEET_HOST")
        .ok()
        .filter(|h| !h.trim().is_empty());
    let requested = cli_host.map(str::to_string).or(env_host);
    hosts::resolve(config::get(), requested.as_deref(), local, scope)
}

/// A `--json` command whose session or stack target did not resolve (or that `FLEET_PATH_ONLY`
/// refused: `stack spawn --on` another host) still answers with one object, `{ ok: false,
/// host, kind, error }` (the exit code is the command's own).
fn json_lookup<T>(json: bool, r: Result<T>) -> Result<T> {
    match r {
        Err(e) if json && (e.is_lookup_failure() || fleet::core::tools::is_refusal(&e)) => {
            Err(commands::json_failure(e))
        }
        r => r,
    }
}

/// A `FLEET_PATH_ONLY` refusal before the command runs (no `PATH`, another host): a command
/// run with `--json` still answers with its failure object on stdout — `spawn`'s own shape
/// for `spawn` — as its own failures do.
fn refused(e: Error, command: &Commands) -> Error {
    if !json_requested() {
        return e;
    }
    match command {
        Commands::Spawn { .. } => commands::spawn_json_failure(e, &commands::host_label()),
        Commands::Answer { .. } => commands::json_failure_with(e, answer_cmd::early_failure_fields),
        Commands::Trust { .. } => commands::json_failure_with(e, trust_cmd::early_failure_fields),
        _ => commands::json_failure(e),
    }
}

/// Does the command line ask for `--json`, at any level (`list --json`, `stack show x --json`)?
fn json_requested() -> bool {
    let Ok(m) = Cli::command().try_get_matches_from(std::env::args_os()) else {
        return false;
    };
    let mut m = &m;
    loop {
        if matches!(m.try_get_one::<bool>("json"), Ok(Some(true))) {
            return true;
        }
        match m.subcommand() {
            Some((_, sub)) => m = sub,
            None => return false,
        }
    }
}

fn run(cli: Cli) -> Result<i32> {
    if cli.dry_run {
        hosts::set_dry_run();
    }
    if let Some(h) = &cli.as_host {
        hosts::set_as_host(h);
    }
    if let Some(id) = &cli.gh_identity {
        fleet::core::gh_identity::adopt(id)?;
    }
    let command = cli.command.unwrap_or_else(default_command);
    if let Err(e) = fleet::core::tools::require_path() {
        return Err(refused(e, &command));
    }

    // `enter` with no host named looks on the default host, then everywhere else.
    let host_named = cli.host.is_some()
        || cli.local
        || cli.as_host.is_some()
        || std::env::var("FLEET_HOST").is_ok_and(|h| !h.trim().is_empty());
    if !host_named
        && let Commands::Enter { query }
        | Commands::Tmux {
            cmd: Some(TmuxCmd::Enter { query }),
        } = &command
    {
        return tmux_cmds::enter_anywhere(query);
    }

    // `fleet -H <host> stack …` for a host this machine has no ssh to, only its web server
    // (the sheds): the stack routes of that server answer instead. Not under FLEET_PATH_ONLY:
    // dispatch below refuses the host.
    if let Commands::Stack { cmd } = &command
        && let Some(h) = cli.host.as_deref().map(str::trim).filter(|h| !h.is_empty())
        && !cli.local
        && !fleet::core::tools::path_only()
        && let Some(hc) = config::get().host(h)
        && h != config::get().self_name()
        && hc.ssh.as_deref().map(str::trim).is_none_or(str::is_empty)
        && let Some(web) = hc.web.as_deref().map(str::trim).filter(|w| !w.is_empty())
    {
        let op = match cmd {
            StackCmd::List { json } => stack_cmds::WebOp::List { json: *json },
            StackCmd::Show { stack, json, path } => stack_cmds::WebOp::Show {
                stack: stack.clone(),
                json: *json,
                path: *path,
            },
            StackCmd::Set {
                stack,
                expect_updated,
                json,
            } => stack_cmds::WebOp::Set {
                stack: stack.clone(),
                expect_updated: expect_updated.clone(),
                json: *json,
            },
            StackCmd::Rename { stack, label, json } => stack_cmds::WebOp::Rename {
                stack: stack.clone(),
                label: label.clone(),
                json: *json,
            },
            StackCmd::Rm { stack, force, json } => stack_cmds::WebOp::Rm {
                stack: stack.clone(),
                force: *force,
                json: *json,
            },
            _ => {
                return Err(Error::Other(format!(
                    "host '{h}' has no ssh destination — over its web server only `stack list / show / set / rename / rm` work; \
set one with: fleet config set hosts.{h}.ssh <alias>"
                )));
            }
        };
        stack_cmds::via_web(h, web, op)?;
        return Ok(0);
    }

    if let Placement::Dispatch(scope) = placement(&command) {
        // `spawn --json` promises one object on stdout whatever happens, the hop included.
        let spawn_json = matches!(&command, Commands::Spawn { json: true, .. });
        let requested = || {
            cli.host
                .clone()
                .or_else(|| std::env::var("FLEET_HOST").ok())
                .unwrap_or_else(commands::host_label)
        };
        let t = match target(cli.host.as_deref(), cli.local, scope) {
            Ok(t) => t,
            Err(e) if spawn_json => {
                return Err(commands::spawn_json_failure(
                    e.tag("host_unknown"),
                    &requested(),
                ));
            }
            Err(e) if fleet::core::tools::is_refusal(&e) => return Err(refused(e, &command)),
            Err(e) => return Err(e),
        };
        hosts::debug(&format!("target: {t:?}"));
        if let Target::Remote(r) = t {
            if let Commands::Spawn {
                effort,
                skip_permissions,
                harness,
                resume,
                from,
                ..
            } = &command
            {
                commands::SpawnOpts {
                    effort: effort.clone(),
                    skip_permissions: *skip_permissions,
                    harness: harness.map(Into::into).unwrap_or_default(),
                    ..Default::default()
                }
                .validate_launch_options(resume.is_some() || from.is_some())
                .map_err(|e| {
                    if spawn_json {
                        commands::spawn_json_failure(e, &r.name)
                    } else {
                        e
                    }
                })?;
            }
            let raw: Vec<String> = std::env::args().skip(1).collect();
            let args = match hosts::remap_dir_args(&hosts::strip_host_flags(&raw)) {
                Ok(a) => a,
                Err(e) if spawn_json => {
                    return Err(commands::spawn_json_failure(e.tag("dir_invalid"), &r.name));
                }
                Err(e) => return Err(e),
            };
            if spawn_json {
                return commands::spawn_remote_json(&r, &args);
            }
            return hosts::run_remote(&r, &args, false);
        }
    }

    match command {
        Commands::List { json, all_hosts } => {
            if all_hosts {
                host_cmds::list_all_hosts(json)?;
            } else {
                commands::list(json)?;
            }
        }
        Commands::Peek {
            target,
            lines,
            json,
            ansi,
        } => {
            if json {
                commands::peek_json(&target, lines, ansi)?
            } else {
                commands::peek(&target, lines)?
            }
        }
        Commands::Usage { json, refresh } => usage_cmd::run(json, refresh)?,
        Commands::Slack { cmd } => match cmd.unwrap_or(SlackCmd::List { json: false }) {
            SlackCmd::List { json } => slack_cmd::list(json)?,
            SlackCmd::Link {
                url,
                duration,
                session,
                task,
                proactive,
                json,
            } => slack_cmd::link(
                &url,
                &duration,
                session.as_deref(),
                task.as_deref(),
                proactive,
                json,
            )?,
            SlackCmd::Unlink { link, json } => slack_cmd::unlink(&link, json)?,
            SlackCmd::Reply {
                link,
                text,
                thread,
                json,
            } => slack_cmd::reply(&link, &text, thread.as_deref(), json)?,
        },
        Commands::Messages {
            target,
            json,
            after,
            before,
            limit,
        } => messages_cmd::run(messages_cmd::Opts {
            target,
            json,
            after,
            before,
            limit,
        })?,
        Commands::Trust {
            tmux_session,
            json,
            pane,
            list,
            accept,
            decline: _,
            prompt,
            close,
            allow_attached,
        } => {
            // As other commands name a host: `-H local`, this machine's name or `--local` are
            // here; anything else (another host, or a name that doesn't resolve) is refused.
            let remote = !matches!(
                target(cli.host.as_deref(), cli.local, Scope::SelfHost),
                Ok(Target::Local { .. })
            );
            trust_cmd::run(
                fleet::core::trust::Opts {
                    session: tmux_session,
                    pane,
                    list,
                    accept,
                    prompt,
                    close,
                    allow_attached,
                    dry_run: hosts::dry_run(),
                },
                json,
                remote,
            )?;
        }
        Commands::Answer {
            target,
            json,
            list,
            choice,
            prompt,
            question,
            answers,
            allow_attached,
        } => answer_cmd::run(answer_cmd::Opts {
            target,
            json,
            list,
            choice,
            prompt,
            question,
            answers,
            allow_attached,
        })?,
        Commands::Send {
            target,
            text,
            json,
            force,
            require_empty,
        } => commands::send(&target, &text, json, force, require_empty)?,
        Commands::Done { note, cancel } => commands::done(note, cancel)?,
        Commands::AskDone { target } => commands::ask_done(&target)?,
        Commands::Sleep { target, json } => commands::sleep(&target, json)?,
        Commands::Keys { target, keys, json } => commands::keys(&target, &keys, json)?,
        Commands::Close {
            target,
            json,
            timeout,
            expect_pid,
            expect_start,
        } => close_cmd::run(close_cmd::Opts {
            target,
            json,
            timeout,
            expect: expect_pid
                .zip(expect_start)
                .map(|(pid, start)| close_cmd::Expect { pid, start }),
        })?,
        Commands::Model {
            target,
            model,
            json,
            expect_pid,
            expect_start,
            require_empty: _,
        } => model_cmd::run(model_cmd::Opts {
            target,
            model,
            json,
            expect: expect_pid
                .zip(expect_start)
                .map(|(pid, start)| close_cmd::Expect { pid, start }),
        })?,
        Commands::Effort {
            target,
            level,
            json,
            expect_pid,
            expect_start,
            require_empty: _,
        } => effort_cmd::run(effort_cmd::Opts {
            target,
            level,
            json,
            expect: expect_pid
                .zip(expect_start)
                .map(|(pid, start)| close_cmd::Expect { pid, start }),
        })?,
        Commands::Rename {
            target,
            name,
            no_tmux_sync,
            force,
            require_empty,
            json,
        } => commands::rename(&target, &name, no_tmux_sync, force, require_empty, json)?,
        Commands::Name {
            target,
            all,
            apply,
            no_tmux_sync,
            refresh,
            settled,
        } => commands::name(
            target,
            commands::NameOpts {
                all,
                apply,
                dry_run: cli.dry_run,
                no_tmux_sync,
                refresh,
                settled,
            },
        )?,
        Commands::Group {
            all_hosts,
            json,
            apply,
            refresh,
            consolidate,
            cached,
            input,
            rename,
            move_session,
            to,
            label,
            create,
            delete,
        } => {
            let edit = match (rename, move_session) {
                _ if create => Some(group::Edit::Create {
                    label: label.unwrap_or_default(),
                }),
                _ if delete.is_some() => Some(group::Edit::Delete {
                    id: delete.unwrap_or_default(),
                }),
                (Some(id), _) => Some(group::Edit::Rename {
                    id,
                    label: label.unwrap_or_default(),
                }),
                (None, Some(session)) => Some(group::Edit::Move {
                    session,
                    to: match (to, label) {
                        (Some(g), _) => fleet::core::grouping::MoveTo::Group(g),
                        (None, Some(l)) => fleet::core::grouping::MoveTo::New(l),
                        (None, None) => {
                            return Err(Error::Other(
                                "--move needs --to <group id> or --label <new group>".into(),
                            ));
                        }
                    },
                }),
                _ => None,
            };
            group::run(group::GroupOpts {
                all_hosts,
                json,
                apply,
                refresh,
                consolidate,
                cached,
                input,
                dry_run: cli.dry_run,
                edit,
            })?
        }
        Commands::Brief {
            target: session,
            json,
            prompt,
            edit,
            set,
            expect_updated,
            regenerate,
            open,
        } => {
            if open {
                let t = target(cli.host.as_deref(), cli.local, Scope::SelfHost)?;
                brief_cmd::open(&t, &session)?
            } else if edit {
                let t = target(cli.host.as_deref(), cli.local, Scope::SelfHost)?;
                return json_lookup(json, brief_cmd::edit(&t, &session, json));
            } else if set {
                json_lookup(
                    json,
                    brief_cmd::set(&session, json, expect_updated.as_deref()),
                )?
            } else if regenerate {
                json_lookup(json, brief_cmd::regenerate(&session, json))?
            } else {
                json_lookup(json, brief_cmd::show(&session, json, prompt))?
            }
        }
        Commands::Spawn {
            prompt,
            from,
            resume,
            dir,
            backend,
            name,
            model,
            effort,
            skip_permissions,
            harness,
            tmux_session,
            window,
            ask_done,
            slack,
            slack_for,
            slack_task,
            slack_proactive,
            json,
        } => {
            let opts = commands::SpawnOpts {
                dir,
                backend: backend.map(Into::into),
                name,
                model,
                effort,
                skip_permissions,
                harness: harness.map(Into::into).unwrap_or_default(),
                tmux_session,
                window,
                ask_done,
                slack: slack.map(|url| slack_cmd::SpawnLink {
                    url,
                    duration: slack_for,
                    task: slack_task,
                    proactive: slack_proactive,
                }),
            };
            match resume {
                Some(id) => commands::spawn_resume(&id, prompt, from.is_some(), opts, json)?,
                None => commands::spawn(prompt, from, opts, json)?,
            }
        }
        Commands::Harness { cmd } => match cmd {
            HarnessCmd::List { json, no_models } => commands::harness_list(json, !no_models)?,
        },
        Commands::GhIdentity { cmd } => match cmd {
            GhIdentityCmd::List { json } => commands::gh_identity_list(json)?,
            GhIdentityCmd::Launcher { name, launcher } => {
                commands::gh_identity_launcher(&name, launcher.as_deref())?
            }
        },
        Commands::Stack { cmd } => match cmd {
            StackCmd::List { json } => stack_cmds::list(json)?,
            StackCmd::Show { stack, json, path } => {
                json_lookup(json, stack_cmds::show(&stack, json, path))?
            }
            StackCmd::New {
                from,
                label,
                no_llm,
                json,
            } => json_lookup(json, stack_cmds::new(&from, label.as_deref(), no_llm, json))?,
            StackCmd::Ensure {
                target,
                label,
                no_llm,
                json,
            } => json_lookup(
                json,
                stack_cmds::ensure(&target, label.as_deref(), no_llm, json),
            )?,
            StackCmd::Spawn {
                target,
                prompt,
                name,
                model,
                harness,
                dir,
                backend,
                tmux_session,
                window,
                no_wait,
                on,
                label,
                no_llm,
                json,
            } => json_lookup(
                json,
                stack_cmds::spawn(stack_cmds::SpawnArgs {
                    target,
                    prompt,
                    label,
                    no_llm,
                    on,
                    opts: commands::SpawnOpts {
                        dir,
                        backend: backend.map(Into::into),
                        name,
                        model,
                        effort: None,
                        skip_permissions: false,
                        harness: harness.map(Into::into).unwrap_or_default(),
                        tmux_session,
                        window,
                        ask_done: false,
                        slack: None,
                    },
                    wait: !no_wait,
                    json,
                }),
            )?,
            StackCmd::Add {
                stack,
                target,
                on,
                input,
                json,
            } => json_lookup(
                json,
                stack_cmds::add(&stack, &target, on.as_deref(), input.as_deref(), json),
            )?,
            StackCmd::Remove {
                stack,
                target,
                json,
            } => json_lookup(json, stack_cmds::remove(&stack, &target, json))?,
            StackCmd::Rename { stack, label, json } => {
                json_lookup(json, stack_cmds::rename(&stack, &label, json))?
            }
            StackCmd::Edit { stack, json } => {
                let t = target(cli.host.as_deref(), cli.local, Scope::SelfHost)?;
                return json_lookup(json, stack_cmds::edit(&t, &stack, json));
            }
            StackCmd::Set {
                stack,
                expect_updated,
                json,
            } => json_lookup(
                json,
                stack_cmds::set(&stack, expect_updated.as_deref(), json),
            )?,
            StackCmd::Sync { input, json } => stack_cmds::sync(input.as_deref(), json)?,
            StackCmd::Rm { stack, force, json } => {
                json_lookup(json, stack_cmds::rm(&stack, force, json))?
            }
        },
        Commands::Handoff {
            brief,
            file,
            dir,
            backend,
            name,
            model,
            harness,
            tmux_session,
            tab,
            no_wait,
        } => commands::handoff(
            brief,
            file,
            commands::SpawnOpts {
                dir,
                backend: backend.map(Into::into),
                name,
                model,
                effort: None,
                skip_permissions: false,
                harness: harness.map(Into::into).unwrap_or_default(),
                tmux_session,
                // A handoff means "over there, out of my way" — a window unless told otherwise.
                window: !tab,
                ask_done: false,
                slack: None,
            },
            !no_wait,
        )?,
        Commands::Watch {
            interval,
            stuck,
            quiet,
            rows,
            mouse,
            no_mouse,
        } => watch::run(watch::WatchOpts {
            interval_secs: interval,
            stuck_secs: stuck,
            quiet,
            rows: rows.map(Into::into),
            // Neither flag given means "whatever the config says".
            mouse: if no_mouse {
                Some(false)
            } else if mouse {
                Some(true)
            } else {
                None
            },
        })?,
        Commands::Tmux { cmd } => match cmd.unwrap_or(TmuxCmd::List {
            quiet: false,
            json: false,
        }) {
            TmuxCmd::List { quiet, json } => tmux_cmds::list(quiet, json)?,
            TmuxCmd::Enter { query } => tmux_cmds::enter(&query)?,
            TmuxCmd::Last => tmux_cmds::last()?,
            TmuxCmd::New(a) => {
                tmux_cmds::new(a.name.as_deref(), a.detach, a.dir.as_deref(), &a.cmd)?
            }
            TmuxCmd::Kill { query, force } => tmux_cmds::kill(&query, force)?,
            TmuxCmd::Rename { query, new_name } => tmux_cmds::rename(&query, &new_name)?,
            TmuxCmd::Stale {
                kill,
                force,
                quiet,
                json,
                no_fleet_check,
                older_than,
            } => tmux_cmds::stale(tmux_cmds::StaleOpts {
                kill,
                force,
                quiet,
                json,
                claude_check: !no_fleet_check,
                older_than,
            })?,
        },
        Commands::Enter { query } => tmux_cmds::enter(&query)?,
        Commands::Last => tmux_cmds::last()?,
        Commands::Restore {
            target,
            closed,
            list: _,
            all,
            forget,
            forget_all,
            json,
        } => restore::run(restore::RestoreOpts {
            target,
            closed,
            all,
            forget,
            forget_all,
            json,
        })?,
        Commands::New(a) => tmux_cmds::new(a.name.as_deref(), a.detach, a.dir.as_deref(), &a.cmd)?,
        Commands::Exec { dir, tty, argv } => {
            let t = target(cli.host.as_deref(), cli.local, Scope::DefaultHost)?;
            return host_cmds::exec(&t, dir.as_deref(), tty, &argv);
        }
        Commands::Ssh { argv } => {
            let t = target(cli.host.as_deref(), cli.local, Scope::DefaultHost)?;
            return host_cmds::ssh(&t, &argv);
        }
        Commands::Doctor => {
            let only = cli.host.clone().or_else(|| {
                std::env::var("FLEET_HOST")
                    .ok()
                    .filter(|h| !h.trim().is_empty())
            });
            host_cmds::doctor(only.as_deref())?
        }
        Commands::Init(a) => init::run(a)?,
        Commands::Config { action } => config_cmd::run(&action)?,
        Commands::Install { no_web, force } => {
            if cli.host.is_none() && std::env::var("FLEET_HOST").is_err() {
                return Err(Error::exit(1, "usage: fleet install --host <name>"));
            }
            let t = target(cli.host.as_deref(), false, Scope::SelfHost)?;
            host_cmds::install(
                &t,
                host_cmds::InstallOpts {
                    web: !no_web,
                    force,
                },
            )?
        }
        Commands::Prs {
            json,
            column,
            refresh,
            cached,
        } => prs_cmd::run(prs_cmd::Opts {
            json,
            column,
            mode: if refresh {
                fleet::core::prs::Mode::Refresh
            } else if cached {
                fleet::core::prs::Mode::Cached
            } else {
                fleet::core::prs::Mode::Auto
            },
        })?,
        Commands::Pr { cmd } => match cmd {
            PrCmd::CleanupWorktrees { apply, json } => prs_cmd::cleanup_worktrees(apply, json)?,
            PrCmd::Seen { pr, json } => prs_cmd::seen(&pr, json)?,
            PrCmd::Checkout { pr, json } => {
                prs_cmd::checkout(&pr, json)?;
            }
            PrCmd::Spawn {
                pr,
                prompt,
                backend,
                name,
                model,
                harness,
                tmux_session,
                window,
            } => prs_cmd::spawn(
                &pr,
                prompt,
                commands::SpawnOpts {
                    dir: None,
                    backend: backend.map(Into::into),
                    name,
                    model,
                    effort: None,
                    skip_permissions: false,
                    harness: harness.map(Into::into).unwrap_or_default(),
                    tmux_session,
                    window,
                    ask_done: false,
                    slack: None,
                },
            )?,
        },
        Commands::Tasks {
            json,
            column,
            refresh,
            cached,
        } => tasks_cmd::run(tasks_cmd::Opts {
            json,
            column,
            mode: if refresh {
                fleet::core::tasks::Mode::Refresh
            } else if cached {
                fleet::core::tasks::Mode::Cached
            } else {
                fleet::core::tasks::Mode::Auto
            },
        })?,
        Commands::Task { cmd } => match cmd {
            TaskCmd::Show { task, json } => tasks_cmd::show(&task, json)?,
            TaskCmd::Status { task, status, json } => {
                tasks_cmd::status(&task, status.as_deref(), json)?
            }
            TaskCmd::Schedule {
                task,
                prompt,
                time,
                spawn,
            } => schedule_cmd::add(spawn.into_opts(None, Some(task), time, prompt, false))?,
            TaskCmd::Spawn {
                task,
                prompt,
                dir,
                backend,
                name,
                model,
                harness,
                tmux_session,
                window,
            } => tasks_cmd::spawn(
                &task,
                prompt,
                commands::SpawnOpts {
                    dir,
                    backend: backend.map(Into::into),
                    name,
                    model,
                    effort: None,
                    skip_permissions: false,
                    harness: harness.map(Into::into).unwrap_or_default(),
                    tmux_session,
                    window,
                    ask_done: false,
                    slack: None,
                },
            )?,
        },
        Commands::Schedule { cmd, json } => {
            match cmd.unwrap_or(ScheduleCmd::List { json, all: false }) {
                ScheduleCmd::List { json: j, all } => schedule_cmd::list(json || j, all)?,
                ScheduleCmd::Add {
                    prompt,
                    at,
                    task,
                    time,
                    spawn,
                } => schedule_cmd::add(spawn.into_opts(at, task, time, prompt, json))?,
                ScheduleCmd::Cancel { id } => schedule_cmd::cancel(&id)?,
            }
        }
        Commands::Repos { cmd, json, all } => match cmd.unwrap_or(ReposCmd::Status { json, all }) {
            ReposCmd::Status { json: j, all: a } => repos_cmd::status(json || j, all || a)?,
            ReposCmd::Sync {
                names,
                due,
                notify,
                quiet,
                json: j,
            } => repos_cmd::sync(repos_cmd::SyncOpts {
                names,
                due,
                notify,
                quiet,
                json: json || j,
            })?,
            ReposCmd::InstallService {
                uninstall,
                no_load,
                print,
            } => repos_cmd::install_service(web::ServiceOpts {
                uninstall,
                no_load,
                print,
            })?,
        },
        Commands::Bots { cmd, json } => match cmd.unwrap_or(JobsCmd::List { json }) {
            JobsCmd::List { json: j } => jobs_cmd::list(json || j)?,
            JobsCmd::Show { name, json: j } => jobs_cmd::show(&name, json || j)?,
            JobsCmd::Run {
                name,
                force,
                prompt,
            } => jobs_cmd::run_now(&name, force, prompt)?,
            JobsCmd::Tick { quiet } => jobs_cmd::tick(quiet)?,
            JobsCmd::Enable { name } => jobs_cmd::set_enabled(&name, true)?,
            JobsCmd::Disable { name } => jobs_cmd::set_enabled(&name, false)?,
            JobsCmd::InstallService {
                uninstall,
                no_load,
                print,
            } => jobs_cmd::install_service(web::ServiceOpts {
                uninstall,
                no_load,
                print,
            })?,
        },
        Commands::Web { action } => match action {
            WebCmd::Serve { port, bind, dir } => web::serve(web::ServeOpts { port, bind, dir })?,
            WebCmd::Build { dir, install } => web::build(web::BuildOpts { dir, install })?,
            WebCmd::InstallService {
                uninstall,
                no_load,
                print,
            } => web::install_service(web::ServiceOpts {
                uninstall,
                no_load,
                print,
            })?,
        },
        Commands::Skill { action } => skill::run(&action)?,
        Commands::Mcp {
            http,
            bind,
            port,
            token_file,
            tools,
            exclude,
        } => {
            let http = if http {
                let token = match token_file {
                    Some(f) => std::fs::read_to_string(&f)
                        .map_err(|e| Error::Other(format!("--token-file {f}: {e}")))?,
                    None => std::env::var("FLEET_MCP_TOKEN").map_err(|_| {
                        Error::Other(
                            "fleet mcp --http needs --token-file or FLEET_MCP_TOKEN".into(),
                        )
                    })?,
                };
                Some(mcp::HttpOpts { bind, port, token })
            } else {
                None
            };
            mcp::serve(
                Cli::command(),
                mcp::ServeOpts {
                    // `fleet -H <host> mcp`: that host is every call's default.
                    default_host: cli.host.clone().filter(|_| !cli.local),
                    tools,
                    exclude,
                    http,
                },
            )?
        }
        Commands::Probe => host_cmds::probe()?,
    }
    Ok(0)
}

/// Commands whose `--json` failures are one `{ ok: false, host, kind, error }` object — usage
/// errors clap catches included — and the exit code such a usage error gets: clap's 2, but
/// `spawn`'s and `close`'s failures (as `docs/cli.md` promises them) all exit 1.
const JSON_FAILURE_COMMANDS: &[(&str, i32)] = &[
    ("messages", 2),
    ("send", 2),
    ("answer", 2),
    ("trust", 2),
    ("peek", 2),
    ("keys", 2),
    ("spawn", 1),
    ("close", 1),
    ("model", 1),
    ("effort", 1),
];

/// The subcommand of [`JSON_FAILURE_COMMANDS`] the command line names with `--json`, read by
/// clap itself with the error set aside — global flags in any spelling (`-nH host`,
/// `--host=x`) included.
fn json_failure_command() -> Option<String> {
    json_failure_command_in(std::env::args_os().collect())
}

/// [`json_failure_command`] of `args`. Clap's error-tolerant parse still stops reading at an
/// argument it does not know (`spawn --resume --bad --json` never reached `--json`), at an
/// option whose value it rejects (`close pid:1 --timeout x --json`), or at one given twice
/// or against another it conflicts with (`close pid:1 --timeout 3 --timeout 4 --json`), so
/// each such argument — with its value — is set aside first and the rest parsed again:
/// `--json` counts wherever it stands, as clap reads it — not as an option's value
/// (`--name=--json`), nor after `--`.
fn json_failure_command_in(mut args: Vec<std::ffi::OsString>) -> Option<String> {
    use clap::error::{ContextKind, ContextValue, ErrorKind};
    for _ in 0..args.len() {
        let Err(e) = Cli::command().try_get_matches_from(&args) else {
            break;
        };
        // An unknown `--flag` (or `--flag=v`) goes alone; a `--opt V` whose value clap
        // rejected takes that value with it (the very value: an option missing its value
        // leaves the next argument alone); of an option given twice, or two that conflict,
        // the first given of the one clap names goes, with its value when it takes one (clap
        // names it `--opt <VALUE>`).
        enum Value {
            None,
            Exactly(std::ffi::OsString),
            Any,
        }
        let (bad, value) = match (e.kind(), e.get(ContextKind::InvalidArg)) {
            (ErrorKind::UnknownArgument, Some(ContextValue::String(bad))) => (bad, Value::None),
            (
                ErrorKind::ValueValidation | ErrorKind::InvalidValue,
                Some(ContextValue::String(bad)),
            ) if bad.starts_with('-') => match e.get(ContextKind::InvalidValue) {
                Some(ContextValue::String(v)) if !v.is_empty() => (bad, Value::Exactly(v.into())),
                _ => break,
            },
            (ErrorKind::ArgumentConflict, Some(ContextValue::String(bad)))
                if bad.starts_with('-') =>
            {
                (
                    bad,
                    if bad.contains(' ') {
                        Value::Any
                    } else {
                        Value::None
                    },
                )
            }
            _ => break,
        };
        let flag = bad
            .split_once(['=', ' '])
            .map_or(bad.as_str(), |(flag, _)| flag);
        // A short flag clap names alone (`-x`) may stand in a cluster (`-xy`, `-nx`): that
        // whole argument goes.
        let short = flag
            .strip_prefix('-')
            .filter(|c| c.chars().count() == 1 && *c != "-")
            .and_then(|c| c.chars().next());
        let Some(at) = args
            .iter()
            .skip(1)
            .take_while(|a| *a != "--")
            .position(|a| {
                a.to_str().is_some_and(|a| {
                    a == flag
                        || a.strip_prefix(flag).is_some_and(|v| v.starts_with('='))
                        || short.is_some_and(|c| {
                            a.strip_prefix('-')
                                .is_some_and(|rest| !rest.starts_with('-') && rest.contains(c))
                        })
                })
            })
        else {
            break;
        };
        let separate = args[at + 1] == flag
            && match &value {
                Value::None => false,
                Value::Exactly(v) => args.get(at + 2).is_some_and(|a| a == v),
                Value::Any => args.get(at + 2).is_some(),
            };
        args.remove(at + 1);
        if separate {
            args.remove(at + 1);
        }
    }
    let m = Cli::command()
        .ignore_errors(true)
        .try_get_matches_from(&args)
        .ok()?;
    let (name, sub) = m.subcommand()?;
    let json = matches!(sub.try_get_one::<bool>("json"), Ok(Some(true)));
    (json && JSON_FAILURE_COMMANDS.iter().any(|(c, _)| *c == name)).then(|| name.to_string())
}

/// A usage error for a `--json` command of [`JSON_FAILURE_COMMANDS`]: the failure object on
/// stdout (`spawn`'s own shape, with `host` and `partial`), clap's message on stderr, and the
/// command's exit code: 2 (clap's; usage errors fleet finds itself, past parsing, exit 1), 1 for
/// `spawn`. Anything else (and help) is clap's as usual.
fn parse_failure(e: clap::Error) -> ! {
    use clap::error::ErrorKind;
    let shown = matches!(
        e.kind(),
        ErrorKind::DisplayHelp
            | ErrorKind::DisplayVersion
            | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
    );
    if !shown && let Some(cmd) = json_failure_command() {
        let rendered = e.render().to_string();
        // The message is everything before clap's `Usage:` block, on one line.
        let first = rendered
            .lines()
            .take_while(|l| !l.starts_with("Usage:") && !l.starts_with("For more information"))
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
            .trim_start_matches("error: ")
            .to_string();
        let code = JSON_FAILURE_COMMANDS
            .iter()
            .find(|(c, _)| *c == cmd)
            .map_or(2, |(_, code)| *code);
        let host = commands::host_label();
        let v = if cmd == "spawn" {
            commands::spawn_error_json(&Error::tagged("usage", first), &host)
        } else {
            let mut v =
                serde_json::json!({ "ok": false, "host": host, "kind": "usage", "error": first });
            if cmd == "answer" {
                answer_cmd::early_failure_fields(&mut v);
            }
            if cmd == "trust" {
                trust_cmd::early_failure_fields(&mut v);
            }
            v
        };
        println!("{v}");
        eprint!("{rendered}");
        std::process::exit(code);
    }
    e.exit()
}

fn main() {
    let cli = Cli::try_parse().unwrap_or_else(|e| parse_failure(e));
    match run(cli) {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            let msg = e.to_string();
            if !msg.is_empty() {
                eprintln!("{} {msg}", "Error:".red().bold());
            }
            std::process::exit(e.code());
        }
    }
}
