//! The reusable core. Nothing in here prints to stdout on its own (dry-run
//! output aside) — rendering belongs to the UIs.

pub mod backend;
pub mod background;
pub mod brief;
pub mod config;
pub mod context;
pub mod discovery;
pub mod done;
pub mod gh_identity;
pub mod git;
pub mod grouping;
pub mod harness;
pub mod hosts;
pub mod input;
pub mod jobs;
pub mod lock;
pub mod naming;
pub mod prompt;
pub mod prompted;
pub mod prs;
pub mod questions;
pub mod repos;
pub mod schedule;
pub mod sleep;
pub mod snapshot;
pub mod stack;
pub mod tasks;
pub mod title;
pub mod tmux;
pub mod tools;
pub mod transcript;
pub mod usage;
pub mod usage_codex;
