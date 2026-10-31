//! `fleet diff`: a session's Changes per checkout, against temp repos and canned sessions
//! (`FLEET_FIXTURE`), with transcripts under a temp `HOME`. Points cite Lookout's contract
//! (`docs/contracts/fleet-diff.md`, C1–C9).
#![allow(deprecated)]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};

struct T {
    env: common::Env,
    home: PathBuf,
}

impl T {
    fn new(sessions: Value) -> Self {
        let env = common::Env::new();
        let home = env.path("home");
        std::fs::create_dir_all(&home).unwrap();
        common::fixture(env.dir.path(), &sessions.to_string());
        T { env, home }
    }

    fn sessions(&self, sessions: Value) {
        common::fixture(self.env.dir.path(), &sessions.to_string());
    }

    fn run(&self, args: &[&str]) -> (i32, Value) {
        self.run_env(args, &[])
    }

    fn run_env(&self, args: &[&str], extra: &[(&str, &str)]) -> (i32, Value) {
        let mut c = self.env.cmd();
        c.env("HOME", &self.home)
            .env("FLEET_FIXTURE", self.env.path("fleet-fixture.json"))
            .env("FLEET_DIFF_STATE_DIR", self.env.path("diff-state"))
            .env("CODEX_HOME", self.home.join(".codex"));
        for (k, v) in extra {
            c.env(k, v);
        }
        let out = c.args(args).output().unwrap();
        let stdout = common::stdout(&out);
        assert_eq!(stdout.trim().lines().count(), 1, "stdout: {stdout}");
        (
            out.status.code().unwrap(),
            serde_json::from_str(stdout.trim()).expect("one JSON object"),
        )
    }

    /// A transcript for session `id` in `cwd`'s project dir, with `lines`.
    fn transcript(&self, cwd: &Path, id: &str, lines: &[Value]) -> PathBuf {
        let enc: String = cwd
            .display()
            .to_string()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        let p = self
            .home
            .join(".claude/projects")
            .join(enc)
            .join(format!("{id}.jsonl"));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        append(&p, lines);
        p
    }
}

fn append(p: &Path, lines: &[Value]) {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(p)
        .unwrap();
    for l in lines {
        writeln!(f, "{l}").unwrap();
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "init.defaultBranch=main",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn write(p: &Path, text: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

/// A repo at `dir` with one commit on `main`, and `origin/HEAD` → `origin/main` at it.
fn repo(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-q"]);
    write(&dir.join("a.txt"), "one\ntwo\nthree\n");
    write(&dir.join("old.rs"), "fn main() {}\n// a\n// b\n// c\n");
    write(&dir.join("gone.txt"), "bye\n");
    write(&dir.join("run.sh"), "echo hi\n");
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-qm", "init"]);
    let head = git(dir, &["rev-parse", "HEAD"]);
    git(dir, &["update-ref", "refs/remotes/origin/main", &head]);
    git(
        dir,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
}

fn real(p: &Path) -> String {
    std::fs::canonicalize(p).unwrap().display().to_string()
}

fn session(pid: i64, id: Option<&str>, cwd: &Path, harness: &str) -> Value {
    json!({ "pid": pid, "session_id": id, "cwd": cwd.display().to_string(), "status": "idle",
            "backend": "tmux", "harness": harness })
}

fn edit(path: &Path, ts: &str) -> Value {
    json!({ "type": "assistant", "timestamp": ts, "message": { "role": "assistant", "content": [
        { "type": "tool_use", "id": "t1", "name": "Edit", "input": { "file_path": path.display().to_string() } }
    ]}})
}

fn cand<'a>(v: &'a Value, root: &str) -> &'a Value {
    v["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["root"] == root)
        .unwrap_or_else(|| panic!("no candidate {root} in {v}"))
}

fn file<'a>(c: &'a Value, path: &str) -> &'a Value {
    c["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["path"] == path)
        .unwrap_or_else(|| panic!("no file {path} in {c}"))
}

#[test]
fn c4_feature_worktree_counts_commits_and_untracked_with_kinds() {
    let t0 = T::new(json!([]));
    let main = t0.home.join("p/app");
    repo(&main);
    let wt = t0.home.join("p/app-wt/feat");
    git(
        &main,
        &["worktree", "add", "-q", "-b", "feat", wt.to_str().unwrap()],
    );
    write(&wt.join("a.txt"), "one\nTWO\nthree\nfour\n");
    git(&wt, &["commit", "-qam", "edit a"]);
    write(&wt.join("new.txt"), "fresh\n");
    git(&wt, &["mv", "old.rs", "moved.rs"]);
    std::fs::remove_file(wt.join("gone.txt")).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(wt.join("run.sh"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
    }
    std::fs::write(wt.join("logo.png"), [0u8, 159, 146, 150, 0, 1, 2]).unwrap();
    let t = t0;
    t.sessions(json!([session(11, Some("s-11"), &wt, "claude")]));

    let (code, v) = t.run(&["--local", "diff", "id:s-11", "--json"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["ok"], true);
    let c = cand(&v, &real(&wt));
    assert_eq!(c["why"], json!(["cwd"]));
    assert_eq!(c["branch"], "feat");
    assert_eq!(c["on_default_branch"], false);
    assert_eq!(c["base"]["ref"], "origin/main");
    assert_eq!(c["repo"]["linked_worktree"], true);
    assert_eq!(c["repo"]["common_dir"], real(&main.join(".git")));
    let a = file(c, "a.txt");
    assert_eq!(
        (
            a["kind"].as_str(),
            a["additions"].as_u64(),
            a["deletions"].as_u64()
        ),
        (Some("modified"), Some(2), Some(1))
    );
    assert_eq!(
        a["uncommitted"], false,
        "committed on the branch, still counted"
    );
    assert_eq!(file(c, "new.txt")["kind"], "added");
    assert_eq!(file(c, "new.txt")["uncommitted"], true);
    assert_eq!(file(c, "moved.rs")["kind"], "renamed");
    assert_eq!(file(c, "moved.rs")["old_path"], "old.rs");
    assert_eq!(file(c, "gone.txt")["kind"], "deleted");
    assert_eq!(file(c, "run.sh")["kind"], "mode_changed");
    assert_eq!(file(c, "logo.png")["binary"], true);
    assert_eq!(file(c, "logo.png")["additions"], 0);
    assert_eq!(c["stats"]["files"], 6);
    assert_eq!(c["more_files"], 0);
}

#[test]
fn c4_default_branch_ahead_of_origin_counts_uncommitted_only() {
    let t0 = T::new(json!([]));
    let dir = t0.home.join("p/app");
    repo(&dir);
    write(&dir.join("a.txt"), "local commit\n");
    git(&dir, &["commit", "-qam", "ahead of origin"]);
    write(&dir.join("b.txt"), "uncommitted\n");
    let t = t0;
    t.sessions(json!([session(12, Some("s-12"), &dir, "claude")]));
    let (_, v) = t.run(&["--local", "diff", "pid:12", "--json"]);
    let c = cand(&v, &real(&dir));
    assert_eq!(c["on_default_branch"], true);
    assert_eq!(c["base"]["sha"], git(&dir, &["rev-parse", "HEAD"]));
    assert_eq!(c["stats"]["files"], 1, "{c}");
    assert_eq!(file(c, "b.txt")["uncommitted"], true);
}

#[test]
fn c4_unborn_repo_and_no_default_branch() {
    let t0 = T::new(json!([]));
    let dir = t0.home.join("p/fresh");
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "trunk"]);
    write(&dir.join("x.txt"), "1\n2\n");
    let t = t0;
    t.sessions(json!([session(13, Some("s-13"), &dir, "claude")]));
    let (_, v) = t.run(&["--local", "diff", "id:s-13", "--json"]);
    let c = cand(&v, &real(&dir));
    assert_eq!(c["head"], Value::Null);
    assert_eq!(c["base"]["ref"], Value::Null);
    assert_eq!(c["on_default_branch"], false);
    assert_eq!(file(c, "x.txt")["additions"], 2);
    assert_eq!(file(c, "x.txt")["uncommitted"], true);
}

#[test]
fn c4_since_unchanged_then_changed_by_a_commit_of_the_whole_tree() {
    let t0 = T::new(json!([]));
    let dir = t0.home.join("p/app");
    repo(&dir);
    git(&dir, &["checkout", "-qb", "feat"]);
    write(&dir.join("a.txt"), "changed\n");
    let t = t0;
    t.sessions(json!([session(14, Some("s-14"), &dir, "claude")]));
    let (_, v1) = t.run(&["--local", "diff", "id:s-14", "--json"]);
    let fp = v1["fingerprint"].as_str().unwrap().to_string();
    assert_eq!(fp.len(), 32);
    let (_, v2) = t.run(&["--local", "diff", "id:s-14", "--json", "--since", &fp]);
    assert_eq!(v2["unchanged"], true);
    assert!(v2.get("candidates").is_none());
    git(&dir, &["commit", "-qam", "same tree, new HEAD"]);
    let (_, v3) = t.run(&["--local", "diff", "id:s-14", "--json", "--since", &fp]);
    assert_eq!(
        v3["unchanged"], false,
        "HEAD moved: the uncommitted marks changed"
    );
    assert_eq!(file(cand(&v3, &real(&dir)), "a.txt")["uncommitted"], false);
}

#[test]
fn c3_edited_checkouts_from_transcripts_and_subagents_gone_and_incremental() {
    let t0 = T::new(json!([]));
    let cwd = t0.home.join("p/work");
    repo(&cwd);
    let other = t0.home.join("p/other");
    repo(&other);
    git(&other, &["checkout", "-qb", "x"]);
    write(&other.join("a.txt"), "edited\n");
    let wt = t0.home.join("p/other-wt");
    git(
        &other,
        &["worktree", "add", "-q", "-b", "y", wt.to_str().unwrap()],
    );
    write(&wt.join("a.txt"), "edited too\n");
    let third = t0.home.join("p/third");
    repo(&third);
    git(&third, &["checkout", "-qb", "z"]);
    write(&third.join("a.txt"), "by a subagent\n");
    let t = t0;
    t.sessions(json!([session(15, Some("s-15"), &cwd, "claude")]));
    let tr = t.transcript(
        &cwd,
        "s-15",
        &[
            edit(&other.join("a.txt"), "2026-10-31T10:00:00Z"),
            edit(&wt.join("a.txt"), "2026-10-31T11:00:00Z"),
            json!({ "type": "assistant", "timestamp": "2026-10-31T09:00:00Z", "message": { "content": [
                { "type": "tool_use", "name": "Read", "input": { "file_path": "/nowhere/read.rs" } }
            ]}}),
        ],
    );
    let sub = tr.with_extension("").join("subagents/agent-abc.jsonl");
    std::fs::create_dir_all(sub.parent().unwrap()).unwrap();
    append(&sub, &[edit(&third.join("a.txt"), "2026-10-31T12:00:00Z")]);

    let (_, v) = t.run(&["--local", "diff", "id:s-15", "--json"]);
    let roots: Vec<&str> = v["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["root"].as_str().unwrap())
        .collect();
    assert_eq!(
        roots,
        [real(&cwd), real(&third), real(&wt), real(&other)],
        "cwd first, then most recent edit"
    );
    assert_eq!(
        cand(&v, &real(&cwd))["stats"]["files"],
        0,
        "a candidate without Changes is still reported"
    );

    // The worktree goes: reported as gone, never as the repo around it.
    let wt_real = real(&wt);
    git(
        &other,
        &["worktree", "remove", "--force", wt.to_str().unwrap()],
    );
    // Incremental: an appended edit is picked up, the earlier ones are kept.
    let fourth = t.home.join("p/fourth");
    repo(&fourth);
    git(&fourth, &["checkout", "-qb", "w"]);
    write(&fourth.join("a.txt"), "late\n");
    append(&tr, &[edit(&fourth.join("a.txt"), "2026-10-31T13:00:00Z")]);
    let (_, v) = t.run(&["--local", "diff", "id:s-15", "--json"]);
    let skipped = v["skipped"].as_array().unwrap();
    assert_eq!(skipped.len(), 1, "{v}");
    assert_eq!(skipped[0]["root"], wt_real);
    assert_eq!(skipped[0]["detail"], "gone");
    assert_eq!(v["candidates"][1]["root"], real(&fourth));
    let state: Value =
        serde_json::from_slice(&std::fs::read(t.env.path("diff-state/s-15.json")).unwrap())
            .unwrap();
    assert_eq!(
        state["transcripts"][tr.display().to_string()]["offset"],
        std::fs::metadata(&tr).unwrap().len()
    );
}

#[test]
fn c3_partial_last_line_waits_and_replaced_transcript_is_reread() {
    let t0 = T::new(json!([]));
    let cwd = t0.home.join("p/work");
    repo(&cwd);
    let other = t0.home.join("p/other");
    repo(&other);
    git(&other, &["checkout", "-qb", "x"]);
    write(&other.join("a.txt"), "edited\n");
    let t = t0;
    t.sessions(json!([session(16, Some("s-16"), &cwd, "claude")]));
    let tr = t.transcript(&cwd, "s-16", &[]);
    let line = edit(&other.join("a.txt"), "2026-10-31T10:00:00Z").to_string();
    std::fs::write(&tr, &line[..line.len() / 2]).unwrap();
    let (_, v) = t.run(&["--local", "diff", "id:s-16", "--json"]);
    assert_eq!(
        v["candidates"].as_array().unwrap().len(),
        1,
        "half a line isn't read"
    );
    // Completed: read to its end. Then replaced by a new file (another inode) at least as long:
    // only the inode says it must be read from the start.
    std::fs::write(
        &tr,
        format!("{}\n", edit(&cwd.join("a.txt"), "2026-10-31T09:00:00Z")),
    )
    .unwrap();
    let (_, v) = t.run(&["--local", "diff", "id:s-16", "--json"]);
    assert_eq!(v["candidates"].as_array().unwrap().len(), 1, "{v}");
    let fresh = tr.with_extension("new");
    std::fs::write(&fresh, format!("{line}\n")).unwrap();
    std::fs::rename(&fresh, &tr).unwrap();
    let (_, v) = t.run(&["--local", "diff", "id:s-16", "--json"]);
    assert_eq!(v["candidates"][1]["root"], real(&other), "{v}");
}

#[test]
fn c6_conflicted_merge_is_unsupported() {
    let t0 = T::new(json!([]));
    let dir = t0.home.join("p/app");
    repo(&dir);
    git(&dir, &["checkout", "-qb", "a"]);
    write(&dir.join("a.txt"), "side a\n");
    git(&dir, &["commit", "-qam", "a"]);
    git(&dir, &["checkout", "-q", "main"]);
    git(&dir, &["checkout", "-qb", "b"]);
    write(&dir.join("a.txt"), "side b\n");
    git(&dir, &["commit", "-qam", "b"]);
    let _ = Command::new("git")
        .arg("-C")
        .arg(&dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "merge",
            "-q",
            "a",
        ])
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output();
    let t = t0;
    t.sessions(json!([session(17, Some("s-17"), &dir, "claude")]));
    let (_, v) = t.run(&["--local", "diff", "id:s-17", "--json"]);
    assert_eq!(v["skipped"][0]["detail"], "unsupported", "{v}");
    assert!(v["candidates"].as_array().unwrap().is_empty());
}

#[test]
fn c7_no_hook_runs_and_the_real_index_is_untouched() {
    let t0 = T::new(json!([]));
    let dir = t0.home.join("p/app");
    repo(&dir);
    git(&dir, &["checkout", "-qb", "feat"]);
    let marker = t0.home.join("hook-ran");
    let hook = dir.join(".git/hooks/post-index-change");
    write(&hook, &format!("#!/bin/sh\ntouch '{}'\n", marker.display()));
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    write(&dir.join("new.txt"), "untracked\n");
    // An fsmonitor hook that would say so if git asked it.
    let fsmonitor = t0.home.join("fsmonitor.sh");
    let asked = t0.home.join("fsmonitor-ran");
    write(
        &fsmonitor,
        &format!("#!/bin/sh\ntouch '{}'\n", asked.display()),
    );
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fsmonitor, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    git(
        &dir,
        &["config", "core.fsmonitor", fsmonitor.to_str().unwrap()],
    );
    let index = std::fs::read(dir.join(".git/index")).unwrap();
    let mtime = std::fs::metadata(dir.join(".git/index"))
        .unwrap()
        .modified()
        .unwrap();
    // Another git holds the index lock: the snapshot never takes it, so it still answers.
    write(&dir.join(".git/index.lock"), "");
    let t = t0;
    t.sessions(json!([session(18, Some("s-18"), &dir, "claude")]));
    let (_, v) = t.run(&["--local", "diff", "id:s-18", "--json"]);
    assert_eq!(cand(&v, &real(&dir))["stats"]["files"], 1, "{v}");
    assert!(!marker.exists(), "post-index-change ran");
    assert!(!asked.exists(), "fsmonitor ran");
    assert_eq!(std::fs::read(dir.join(".git/index")).unwrap(), index);
    assert_eq!(
        std::fs::metadata(dir.join(".git/index"))
            .unwrap()
            .modified()
            .unwrap(),
        mtime
    );
    assert!(dir.join(".git/index.lock").exists());
}

#[test]
fn c6_c7_split_index_writes_no_shared_index() {
    let t0 = T::new(json!([]));
    let dir = t0.home.join("p/app");
    repo(&dir);
    git(&dir, &["config", "core.splitIndex", "true"]);
    git(&dir, &["config", "splitIndex.maxPercentChange", "0"]);
    git(&dir, &["checkout", "-qb", "feat"]);
    git(&dir, &["update-index", "--split-index"]);
    write(&dir.join("a.txt"), "dirty tracked\n");
    let shared = |d: &Path| -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(d.join(".git"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("sharedindex."))
            .collect();
        v.sort();
        v
    };
    let before = shared(&dir);
    assert!(!before.is_empty(), "the repo uses a split index");
    let t = t0;
    t.sessions(json!([session(20, Some("s-20"), &dir, "claude")]));
    let (_, v) = t.run(&["--local", "diff", "id:s-20", "--json"]);
    assert_eq!(
        file(cand(&v, &real(&dir)), "a.txt")["kind"],
        "modified",
        "{v}"
    );
    assert_eq!(shared(&dir), before, "no new shared index in the repo");
}

#[test]
fn c6_sparse_checkout_is_unsupported() {
    let t0 = T::new(json!([]));
    let dir = t0.home.join("p/app");
    repo(&dir);
    git(&dir, &["config", "core.sparseCheckout", "true"]);
    let t = t0;
    t.sessions(json!([session(21, Some("s-21"), &dir, "claude")]));
    let (_, v) = t.run(&["--local", "diff", "id:s-21", "--json"]);
    assert_eq!(v["skipped"][0]["detail"], "unsupported", "{v}");
}

#[test]
fn c2_type_change_and_submodule_in_a_real_repo() {
    let t0 = T::new(json!([]));
    let sub = t0.home.join("p/lib");
    repo(&sub);
    let dir = t0.home.join("p/app");
    repo(&dir);
    git(&dir, &["checkout", "-qb", "feat"]);
    git(
        &dir,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            sub.to_str().unwrap(),
            "lib",
        ],
    );
    std::fs::remove_file(dir.join("a.txt")).unwrap();
    std::os::unix::fs::symlink("run.sh", dir.join("a.txt")).unwrap();
    let t = t0;
    t.sessions(json!([session(22, Some("s-22"), &dir, "claude")]));
    let (_, v) = t.run(&["--local", "diff", "id:s-22", "--json"]);
    let c = cand(&v, &real(&dir));
    assert_eq!(file(c, "a.txt")["kind"], "type_changed", "{c}");
    assert_eq!(file(c, "lib")["kind"], "submodule", "{c}");
    assert_eq!(file(c, "lib")["additions"], 0);
}

#[test]
fn c2_more_than_500_files_lists_500() {
    let t0 = T::new(json!([]));
    let dir = t0.home.join("p/app");
    repo(&dir);
    git(&dir, &["checkout", "-qb", "feat"]);
    for i in 0..510 {
        write(&dir.join(format!("many/f{i:03}.txt")), "x\n");
    }
    let t = t0;
    t.sessions(json!([session(23, Some("s-23"), &dir, "claude")]));
    let (_, v) = t.run(&["--local", "diff", "id:s-23", "--json"]);
    let c = cand(&v, &real(&dir));
    assert_eq!(c["files"].as_array().unwrap().len(), 500);
    assert_eq!(c["more_files"], 10);
    assert_eq!(c["stats"]["files"], 510);
    assert_eq!(
        c["stats"]["additions"], 510,
        "counted over every file, not the 500 listed"
    );
}

#[test]
fn c4_sha256_repo() {
    let t0 = T::new(json!([]));
    let dir = t0.home.join("p/app");
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "--object-format=sha256"]);
    write(&dir.join("a.txt"), "one\n");
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-qm", "init"]);
    git(&dir, &["checkout", "-qb", "feat"]);
    write(&dir.join("a.txt"), "two\n");
    let t = t0;
    t.sessions(json!([session(24, Some("s-24"), &dir, "claude")]));
    let (_, v) = t.run(&["--local", "diff", "id:s-24", "--json"]);
    let c = cand(&v, &real(&dir));
    assert_eq!(c["base"]["sha"].as_str().unwrap().len(), 64, "{c}");
    assert_eq!(file(c, "a.txt")["additions"], 1);
}

#[test]
fn c4_since_changes_when_the_cwd_checkout_becomes_edited() {
    let t0 = T::new(json!([]));
    let dir = t0.home.join("p/app");
    repo(&dir);
    write(&dir.join("a.txt"), "dirty on main\n");
    let t = t0;
    t.sessions(json!([session(25, Some("s-25"), &dir, "claude")]));
    let tr = t.transcript(&dir, "s-25", &[]);
    let (_, v1) = t.run(&["--local", "diff", "id:s-25", "--json"]);
    assert_eq!(cand(&v1, &real(&dir))["why"], json!(["cwd"]));
    let fp = v1["fingerprint"].as_str().unwrap().to_string();
    // Same tree, but now the session edited there: a consumer counts it, so the answer changed.
    append(&tr, &[edit(&dir.join("a.txt"), "2026-10-31T10:00:00Z")]);
    let (_, v2) = t.run(&["--local", "diff", "id:s-25", "--json", "--since", &fp]);
    assert_eq!(v2["unchanged"], false, "{v2}");
    assert_eq!(cand(&v2, &real(&dir))["why"], json!(["cwd", "edited"]));
}

#[test]
fn c3_corrupt_state_is_read_from_scratch() {
    let t0 = T::new(json!([]));
    let cwd = t0.home.join("p/work");
    repo(&cwd);
    let other = t0.home.join("p/other");
    repo(&other);
    git(&other, &["checkout", "-qb", "x"]);
    write(&other.join("a.txt"), "edited\n");
    let t = t0;
    t.sessions(json!([session(26, Some("s-26"), &cwd, "claude")]));
    t.transcript(
        &cwd,
        "s-26",
        &[edit(&other.join("a.txt"), "2026-10-31T10:00:00Z")],
    );
    write(&t.env.path("diff-state/s-26.json"), "{not json");
    let (code, v) = t.run(&["--local", "diff", "id:s-26", "--json"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(cand(&v, &real(&other))["why"], json!(["edited"]));
}

#[test]
fn c1_c8_codex_by_pid_cwd_only_and_other_hosts_refused() {
    let t0 = T::new(json!([]));
    let dir = t0.home.join("p/app");
    repo(&dir);
    write(&dir.join("a.txt"), "dirty on main\n");
    let t = t0;
    t.sessions(json!([session(19, None, &dir, "codex")]));
    let (code, v) = t.run(&["--local", "diff", "pid:19", "--json"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["session_id"], Value::Null);
    assert_eq!(v["harness"], "codex");
    assert_eq!(cand(&v, &real(&dir))["why"], json!(["cwd"]));
    assert!(!t.env.path("diff-state").join("null.json").exists());

    let (code, v) = t.run_env(
        &["diff", "pid:19", "--json"],
        &[("FLEET_HOST", "elsewhere.example")],
    );
    assert_eq!(code, 1);
    assert_eq!(v["kind"], "usage");
    let (code, v) = t.run(&["--local", "diff", "pid:999", "--json"]);
    assert_eq!(code, 1);
    assert_eq!(v["kind"], "target_not_found");
}
