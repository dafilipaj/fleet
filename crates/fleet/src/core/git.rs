//! The git checkout a session works in: its branch, root and GitHub repo, read straight
//! from the `.git` files (no `git` process — `list` stamps every row on every poll).
//! Views link a session to a pull request through it (`repo` + `branch`), on any host.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::core::discovery::Session;

/// A session's checkout (`list --json` → `git`; `null` outside one).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitHead {
    /// The checked-out branch; `null` when detached.
    pub branch: Option<String>,
    /// The checkout root, absolute.
    pub root: String,
    /// `true` = a linked worktree, `false` = the repo's main checkout.
    pub linked: bool,
    /// `owner/name` of `origin` on GitHub; `null` when there is none.
    pub repo: Option<String>,
}

/// The checkout `dir` is in (walking up to the first `.git`), or `None`.
pub fn head_of(dir: &Path) -> Option<GitHead> {
    let (root, dot_git) = dir.ancestors().find_map(|d| {
        let g = d.join(".git");
        g.exists().then(|| (d.to_path_buf(), g))
    })?;
    let (git_dir, common, linked) = if dot_git.is_dir() {
        (dot_git.clone(), dot_git, false)
    } else {
        // A linked worktree: `.git` is a file `gitdir: <path>`; that dir's `commondir`
        // points (usually relatively) at the main repo's `.git`.
        let text = fs::read_to_string(&dot_git).ok()?;
        let git_dir = resolve(&root, text.trim().strip_prefix("gitdir:")?.trim());
        let common = fs::read_to_string(git_dir.join("commondir"))
            .map(|c| resolve(&git_dir, c.trim()))
            .unwrap_or_else(|_| git_dir.clone());
        (git_dir, common, true)
    };
    let head = fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let branch = head
        .trim()
        .strip_prefix("ref: refs/heads/")
        .map(str::to_string);
    let repo = fs::read_to_string(common.join("config"))
        .ok()
        .and_then(|c| origin_url(&c))
        .and_then(|u| github_slug(&u));
    Some(GitHead {
        branch,
        root: root.to_string_lossy().into_owned(),
        linked,
        repo,
    })
}

fn resolve(base: &Path, p: &str) -> PathBuf {
    let p = Path::new(p);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        base.join(p)
    }
}

/// `[remote "origin"] url = …` from a git config file.
fn origin_url(config: &str) -> Option<String> {
    let mut in_origin = false;
    for line in config.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_origin = line.replace(' ', "") == "[remote\"origin\"]";
        } else if in_origin
            && let Some((k, v)) = line.split_once('=')
            && k.trim() == "url"
        {
            return Some(v.trim().to_string());
        }
    }
    None
}

/// `owner/name` from a GitHub remote (`git@github.com:o/n.git`, `https://github.com/o/n`,
/// `ssh://git@github.com/o/n`); `None` for any other host.
fn github_slug(url: &str) -> Option<String> {
    let (_, rest) = url.split_once("github.com")?;
    let rest = rest.trim_start_matches([':', '/']);
    let rest = rest.trim_end_matches('/').trim_end_matches(".git");
    let (owner, name) = rest.split_once('/')?;
    (!owner.is_empty() && !name.is_empty() && !name.contains('/'))
        .then(|| format!("{owner}/{name}"))
}

/// Fill `git` on every row (once per distinct cwd).
pub fn stamp_git(rows: &mut [Session]) {
    let mut seen: HashMap<String, Option<GitHead>> = HashMap::new();
    for r in rows.iter_mut() {
        if let Some(cwd) = r.cwd.clone() {
            r.git = seen
                .entry(cwd)
                .or_insert_with_key(|c| head_of(Path::new(c)))
                .clone();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs() {
        assert_eq!(
            github_slug("git@github.com:acme/app.git").as_deref(),
            Some("acme/app")
        );
        assert_eq!(
            github_slug("https://github.com/acme/app").as_deref(),
            Some("acme/app")
        );
        assert_eq!(
            github_slug("ssh://git@github.com/acme/app.git/").as_deref(),
            Some("acme/app")
        );
        assert_eq!(github_slug("https://gitlab.com/acme/app"), None);
    }

    #[test]
    fn origin_from_config() {
        let c = "[core]\n\tbare = false\n[remote \"upstream\"]\n\turl = x\n[remote \"origin\"]\n\turl = git@github.com:acme/app.git\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n";
        assert_eq!(
            origin_url(c).as_deref(),
            Some("git@github.com:acme/app.git")
        );
        assert_eq!(origin_url("[core]\n"), None);
    }

    #[test]
    fn main_checkout_and_linked_worktree() {
        let t = tempfile::tempdir().unwrap();
        let repo = t.path().join("app");
        let git = repo.join(".git");
        fs::create_dir_all(git.join("worktrees/feat")).unwrap();
        fs::write(git.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        fs::write(
            git.join("config"),
            "[remote \"origin\"]\n\turl = https://github.com/acme/app.git\n",
        )
        .unwrap();
        fs::create_dir_all(repo.join("src/deep")).unwrap();

        let h = head_of(&repo.join("src/deep")).unwrap();
        assert_eq!(
            (h.branch.as_deref(), h.linked, h.repo.as_deref()),
            (Some("main"), false, Some("acme/app"))
        );
        assert_eq!(h.root, repo.to_string_lossy());

        let wt = t.path().join("app-feat");
        fs::create_dir_all(&wt).unwrap();
        fs::write(
            wt.join(".git"),
            format!("gitdir: {}\n", git.join("worktrees/feat").display()),
        )
        .unwrap();
        fs::write(git.join("worktrees/feat/HEAD"), "ref: refs/heads/feat-x\n").unwrap();
        fs::write(git.join("worktrees/feat/commondir"), "../..\n").unwrap();
        let h = head_of(&wt).unwrap();
        assert_eq!(
            (h.branch.as_deref(), h.linked, h.repo.as_deref()),
            (Some("feat-x"), true, Some("acme/app"))
        );

        fs::write(git.join("worktrees/feat/HEAD"), "0123abcd\n").unwrap();
        assert_eq!(head_of(&wt).unwrap().branch, None);
        assert!(head_of(t.path()).is_none());
    }
}
