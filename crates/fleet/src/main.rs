use std::io::IsTerminal;

use clap::{CommandFactory, Parser, Subcommand};
use colored::Colorize;

use fleet::cli::{
    answer as answer_cmd, brief as brief_cmd, close as close_cmd, commands, config_cmd, group,
    hosts as host_cmds, init, jobs as jobs_cmd, mcp, messages as messages_cmd, prs as prs_cmd,
    repos as repos_cmd, restore, skill, stack as stack_cmds, tmux as tmux_cmds, usage as usage_cmd,
    web,
};
use fleet::core::config;
use fleet::core::discovery::Backend;
use fleet::core::hosts::{self, Scope, Target};
use fleet::error::{Error, Result};
use fleet::tui::watch;

#[derive(Parser)]
#[command(
    name = "fleet",
    version,
    about = "See, steer and spawn Claude Code sessions and tmux sessions — on this machine or any host you can ssh to",
    after_help = "Bare `fleet` opens the dashboard on a terminal (a one-shot `list` when piped).\n\
Host: -H <name> runs the command on that host over ssh (it needs fleet installed there).\n\
tmux-session and machine commands (tmux, enter, last, new, exec, ssh) default to `defaultHost`; everything else to this machine.\n\
Env: FLEET_CONFIG FLEET_HOST FLEET_DRY_RUN FLEET_DEBUG FLEET_CONNECT_TIMEOUT FLEET_MUX FLEET_REMOTE_TIMEOUT FLEET_CMD FLEET_TMUX FLEET_SNAPSHOT NO_COLOR"
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

    /// Spawn a new agent session (Claude, Codex or pi) in a fresh tab/pane
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
        /// Coding agent to run: claude (default), codex or pi
        #[arg(long, value_enum)]
        harness: Option<HarnessArg>,
        /// Open a window in this tmux session instead of a new session per job (tmux only)
        #[arg(long)]
        tmux_session: Option<String>,
        /// Open a new window instead of a tab (iterm backend only)
        #[arg(long)]
        window: bool,
        /// Print one JSON object: where it landed, or `{ ok: false, kind, error }` on failure
        #[arg(long)]
        json: bool,
    },

    /// Coding-agent harnesses on this host (claude, codex, pi): installed, command, models
    Harness {
        #[command(subcommand)]
        cmd: HarnessCmd,
    },

    /// Session stacks: sessions sharing one context file (the StackBrief); spawn siblings
    Stack {
        #[command(subcommand)]
        cmd: StackCmd,
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

    /// Scheduled agent sessions: cron jobs that spawn a session on this host (config `jobs`)
    Jobs {
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
    /// Every job: schedule, next run, last run, live sessions — the default
    List {
        #[arg(long)]
        json: bool,
    },
    /// One job: its settings and latest runs, each with the sessions it spawned
    Show {
        name: String,
        #[arg(long)]
        json: bool,
    },
    /// Start a run now, whatever the schedule says
    Run {
        name: String,
        /// Start even while a session of the job is still working
        #[arg(long)]
        force: bool,
    },
    /// Fire due jobs and close finished sessions (what the service runs every minute)
    Tick {
        /// Print only what happened, timestamped
        #[arg(long, short = 'q')]
        quiet: bool,
    },
    /// Resume a paused job
    Enable { name: String },
    /// Pause a job (its schedule stops firing; live sessions are left alone)
    Disable { name: String },
    /// Run `jobs tick` every minute and at login (launchd)
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

/// How a command relates to hosts.
enum Placement {
    /// Re-run on the target host when it isn't this machine.
    Dispatch(Scope),
    /// Handles `--host` itself (or ignores it).
    Here,
}

fn placement(c: &Commands) -> Placement {
    match c {
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
        } => commands::name(
            target,
            commands::NameOpts {
                all,
                apply,
                dry_run: cli.dry_run,
                no_tmux_sync,
                refresh,
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
            harness,
            tmux_session,
            window,
            json,
        } => {
            let opts = commands::SpawnOpts {
                dir,
                backend: backend.map(Into::into),
                name,
                model,
                harness: harness.map(Into::into).unwrap_or_default(),
                tmux_session,
                window,
            };
            match resume {
                Some(id) => commands::spawn_resume(&id, prompt, from.is_some(), opts, json)?,
                None => commands::spawn(prompt, from, opts, json)?,
            }
        }
        Commands::Harness { cmd } => match cmd {
            HarnessCmd::List { json, no_models } => commands::harness_list(json, !no_models)?,
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
                        harness: harness.map(Into::into).unwrap_or_default(),
                        tmux_session,
                        window,
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
                harness: harness.map(Into::into).unwrap_or_default(),
                tmux_session,
                // A handoff means "over there, out of my way" — a window unless told otherwise.
                window: !tab,
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
                    harness: harness.map(Into::into).unwrap_or_default(),
                    tmux_session,
                    window,
                },
            )?,
        },
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
        Commands::Jobs { cmd, json } => match cmd.unwrap_or(JobsCmd::List { json }) {
            JobsCmd::List { json: j } => jobs_cmd::list(json || j)?,
            JobsCmd::Show { name, json: j } => jobs_cmd::show(&name, json || j)?,
            JobsCmd::Run { name, force } => jobs_cmd::run_now(&name, force)?,
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
    ("peek", 2),
    ("keys", 2),
    ("spawn", 1),
    ("close", 1),
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
