//! `fleet` — see, steer and spawn Claude Code sessions and tmux sessions across
//! your machines.
//!
//! Layout:
//! - [`core`] is the reusable library: config, Claude-session discovery and
//!   backends (iTerm/tmux), LLM naming, tmux sessions, host resolution and ssh
//!   dispatch. It prints nothing; any UI (this CLI, the TUI, a web server) builds
//!   on it.
//! - [`cli`] renders the command-line verbs on top of `core`.
//! - [`tui`] is the `watch` dashboard.
//!
//! The Claude-session discovery, backends, naming and dashboard are derived from
//! `tb-fleet` in productiveio/cli-toolbox (MIT); the tmux-session and host
//! commands port a standalone shell tool with the same job.

pub mod cli;
pub mod core;
pub mod error;
pub mod tui;

/// This binary's version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// [`VERSION`] and the commit it was built from, `0.1.0 (5e2afd8)`, or [`VERSION`] alone when
/// built where git could not say (build.rs): what `fleet --version` prints after the name.
pub const BUILD_VERSION: &str = env!("FLEET_BUILD_VERSION");
