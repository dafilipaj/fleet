//! `fleet --version` names the commit fleet was built from: `fleet 0.1.0 (5e2afd8)`.
//!
//! The revision is HEAD's commit, cut to 7 hex digits, asked of git in this crate's directory.
//! That is a clone or a linked worktree, and also the checkout `cargo install --git … --rev …`
//! builds from (cargo clones it with its `.git`). Built where git can't answer (no git, not a
//! repository), it is plain `fleet 0.1.0`; the build never fails over it. Uncommitted changes
//! are not marked: such a build reads as the commit it started from.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let dir = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap_or_default());
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let line = match revision(&dir) {
        Some(rev) => format!("{version} ({rev})"),
        None => version,
    };
    println!("cargo:rustc-env=FLEET_BUILD_VERSION={line}");
    for p in moves_with_head(&dir) {
        println!("cargo:rerun-if-changed={}", p.display());
    }
}

/// stdout of `git <args>` run in `dir`, trimmed, when it succeeds and says something.
fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?;
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_string())
}

/// HEAD's commit, its first 7 hex digits (always 7, even where git would need more to be
/// unambiguous).
fn revision(dir: &Path) -> Option<String> {
    let sha = git(dir, &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"])?;
    (sha.len() >= 7 && sha.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| sha[..7].to_ascii_lowercase())
}

/// The files that change when HEAD moves: HEAD itself (a checkout, a detached commit) and the
/// branch it is on, as a loose ref or in `packed-refs` (a commit, a reset), and HEAD's reflog.
/// The reflog is the one that does not go stale: after `git gc` packs the branch, the next
/// commit writes a loose ref that did not exist to be watched, while HEAD and `packed-refs`
/// stay as they were; but every commit, reset and checkout appends to `logs/HEAD`.
/// `--git-path` finds each where it lives, also in a linked worktree, whose `.git` is a file,
/// whose HEAD and reflog are in its own git dir and whose branches are in the main repository.
/// Only existing files: cargo reruns on every build for a missing one.
fn moves_with_head(dir: &Path) -> Vec<PathBuf> {
    let mut names = vec![
        "HEAD".to_string(),
        "packed-refs".to_string(),
        "logs/HEAD".to_string(),
    ];
    if let Some(branch) = git(dir, &["symbolic-ref", "-q", "HEAD"]) {
        names.push(branch);
    }
    names
        .iter()
        .filter_map(|n| git(dir, &["rev-parse", "--git-path", n]))
        // Relative to where git ran (an absolute one stays as it is).
        .map(|p| dir.join(p))
        .filter(|p| p.is_file())
        .collect()
}
