//! `fleet stack`: session stacks — sessions that share one context file, the StackBrief
//! (docs/architecture.md → "Session stacks"). The logic lives in `core::stack`; this is argument
//! handling, the text / JSON output, the editor round trip and the sibling spawn — on this host
//! or, with `--on <host>`, on another one (the stack stays here; the sibling gets the shared
//! context through `fleet -H <here> stack …`).

use std::collections::HashSet;
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use colored::Colorize;
use serde_json::{Value, json};

use crate::cli::commands::{self, SpawnOpts, host_label};
use crate::cli::config_cmd;
use crate::cli::render::home_rel;
use crate::core::backend::{self, Prompt};
use crate::core::brief;
use crate::core::config;
use crate::core::discovery::{self, Session};
use crate::core::hosts::{self, Remote, Target, Tty};
use crate::core::naming;
use crate::core::stack::{self, EXIT_AMBIGUOUS, EXIT_NOTHING, Paths, Stack};
use crate::core::title;
use crate::error::{Error, Result};

/// How long a sibling spawn waits for the new session to register (like `handoff`).
const AWAIT: Duration = Duration::from_secs(75);

/// One discovery and the paths, shared by every subcommand.
struct Ctx {
    dir: PathBuf,
    paths: Paths,
    /// This machine's live sessions (host unset), then — with `--input` — other hosts' rows,
    /// each under its `host`.
    rows: Vec<Session>,
    /// `false` when the registry could not be read: nothing may be marked closed then.
    rows_ok: bool,
    /// Session ids dormant on this machine (left by a reboot): never marked closed.
    dormant: HashSet<String>,
    /// This machine's config `self` (what goes into the files).
    me: String,
    /// The name rows are tagged with (the caller's name for this host).
    label: String,
    /// Hosts whose sessions `rows` lists in full: this machine, plus every host an `--input`
    /// answered for. Members of other hosts are never judged.
    hosts_ok: HashSet<String>,
}

impl Ctx {
    fn new() -> Result<Self> {
        Self::load(None)
    }

    /// With `input` (`-` = stdin, else a file): a `list --json` array or a `/api/fleet` body —
    /// the other hosts' live sessions, so their members can be judged (the web server's
    /// merged fleet). Rows of this machine come from discovery regardless.
    fn load(input: Option<&str>) -> Result<Self> {
        let (mut rows, rows_ok) = match discovery::discover_checked() {
            Ok(r) => (r, true),
            Err(e) => {
                eprintln!("{}", format!("warning: {e}").yellow());
                (Vec::new(), false)
            }
        };
        naming::stamp_titles(&mut rows);
        title::stamp_display_titles(&mut rows);
        let me = config::get().self_name();
        let label = host_label();
        let mut hosts_ok: HashSet<String> = [me.clone(), label.clone()].into();
        if let Some(src) = input {
            let text = if src == "-" {
                let mut s = String::new();
                std::io::stdin().read_to_string(&mut s)?;
                s
            } else {
                std::fs::read_to_string(crate::core::tools::expand_tilde(src))
                    .map_err(|e| Error::Other(format!("cannot read {src}: {e}")))?
            };
            let v: Value = serde_json::from_str(&text)
                .map_err(|e| Error::Other(format!("--input is not JSON: {e}")))?;
            let (more, ok) = stack::parse_input(&v, &me).map_err(Error::Other)?;
            let mine = |h: &str| h == me || h == label;
            rows.extend(
                more.into_iter()
                    .filter(|r| r.host.as_deref().is_none_or(|h| !mine(h))),
            );
            hosts_ok.extend(ok.into_iter().filter(|h| !mine(h)));
        }
        Ok(Ctx {
            dir: stack::stacks_dir()?,
            paths: Paths::current(),
            rows,
            rows_ok,
            dormant: crate::core::snapshot::dormant_session_ids(),
            me,
            label,
            hosts_ok,
        })
    }

    fn obs(&self) -> stack::Observed<'_> {
        stack::Observed {
            rows: &self.rows,
            hosts_ok: &self.hosts_ok,
            dormant: &self.dormant,
            local: &self.me,
        }
    }

    /// `host` means this machine: its config name, the caller's name for it, or a local alias.
    fn is_me(&self, host: &str) -> bool {
        let h = host.trim();
        h == self.me
            || h == self.label
            || matches!(h.to_lowercase().as_str(), "local" | "localhost" | "self")
    }

    /// This machine's rows only.
    fn local_rows(&self) -> Vec<Session> {
        self.rows
            .iter()
            .filter(|r| r.host.as_deref().is_none_or(|h| self.is_me(h)))
            .cloned()
            .collect()
    }

    /// Reconcile members with the live sessions (writes only what changed; nothing on a
    /// dry run or when discovery failed). → the ids that changed.
    fn sync(&self) -> Result<Vec<String>> {
        if !self.rows_ok {
            return Ok(Vec::new());
        }
        stack::sync(
            &self.dir,
            &self.obs(),
            &brief::iso_now(),
            &self.paths,
            !hosts::dry_run(),
        )
    }

    fn stacks(&self) -> Vec<Stack> {
        stack::load_all(&self.dir)
    }

    fn path_of(&self, s: &Stack) -> PathBuf {
        self.dir.join(format!("{}.md", s.id()))
    }

    fn view(&self, s: &Stack) -> Value {
        let path = self.path_of(s);
        stack::view(
            s,
            &stack::ViewCtx {
                host_label: &self.label,
                obs: &self.obs(),
                paths: &self.paths,
                path: &path,
            },
        )
    }

    fn resolve(&self, q: &str) -> Result<Stack> {
        let all = self.stacks();
        stack::resolve(&all, &self.local_rows(), q).cloned()
    }

    /// A live session target on this machine — an ambiguous one is exit 2 with the candidates.
    fn session(&self, target: &str) -> Result<Session> {
        session_in(self.local_rows(), target)
    }

    /// A live session target among `rows` (one host's), as [`Ctx::session`].
    fn session_on(&self, host: &str, target: &str, input: bool) -> Result<Session> {
        let rows = if input {
            if !self.hosts_ok.contains(host) {
                return Err(Error::Other(format!(
                    "--input has no sessions for host '{host}' (it did not answer, or is not in it)"
                )));
            }
            self.rows
                .iter()
                .filter(|r| r.host.as_deref() == Some(host))
                .cloned()
                .collect()
        } else {
            remote_rows(host)?
        };
        session_in(rows, target)
    }

    /// Write `s` (stamps `updated`), or say what would be written on a dry run.
    fn save(&self, s: &mut Stack) -> Result<()> {
        let now = brief::iso_now();
        if hosts::dry_run() {
            s.meta.insert("updated".into(), Value::String(now));
            eprintln!("dry-run: would write {}", self.path_of(s).display());
            return Ok(());
        }
        stack::save(&self.dir, s, &now, &self.paths)
    }
}

fn session_in(rows: Vec<Session>, target: &str) -> Result<Session> {
    let s = discovery::resolve_in(rows, target).map_err(|e| {
        if stack::is_ambiguous_session(&e) {
            Error::exit(EXIT_AMBIGUOUS, e)
        } else {
            Error::Other(e)
        }
    })?;
    if s.session_id
        .as_deref()
        .is_none_or(|id| !brief::is_session_id(id))
    {
        return Err(Error::Other(format!(
            "{} has no session id yet",
            s.headline()
        )));
    }
    Ok(s)
}

/// A configured host other than this machine, by name (an ssh destination as-is otherwise).
fn remote_of(host: &str) -> Result<Remote> {
    match hosts::resolve(config::get(), Some(host), false, hosts::Scope::SelfHost)? {
        Target::Remote(r) => Ok(r),
        Target::Local { .. } => Err(Error::Other(format!(
            "'{host}' is this machine — leave --on out"
        ))),
    }
}

/// `fleet list --json` on `host` (over ssh), rows tagged with it.
fn remote_rows(host: &str) -> Result<Vec<Session>> {
    let r = remote_of(host)?;
    let got = hosts::capture_remote(
        &r,
        &["list".into(), "--json".into()],
        hosts::remote_timeout(),
    )?;
    if !got.ok() {
        return Err(Error::Other(format!("{}: {}", r.name, got.why(&r))));
    }
    let mut rows: Vec<Session> = serde_json::from_str(&got.stdout)
        .map_err(|e| Error::Other(format!("bad `fleet list --json` from {}: {e}", r.name)))?;
    for s in rows.iter_mut() {
        s.host = Some(host.to_string());
    }
    Ok(rows)
}

fn print_json(v: &Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}

fn with(mut v: Value, extra: Value) -> Value {
    if let (Value::Object(o), Value::Object(e)) = (&mut v, extra) {
        o.extend(e);
    }
    v
}

fn counts(s: &Stack) -> String {
    let (live, closed) = s.counts();
    if closed == 0 {
        format!("{live} live")
    } else {
        format!("{live} live · {closed} closed")
    }
}

// ------------------------------------------------------------------ list / show / sync

/// `fleet stack list`
pub fn list(json: bool) -> Result<()> {
    let c = Ctx::new()?;
    c.sync()?;
    let stacks = c.stacks();
    if json {
        return print_json(&json!({
            "host": c.label,
            "stacks": stacks.iter().map(|s| c.view(s)).collect::<Vec<_>>(),
        }));
    }
    if stacks.is_empty() {
        println!(
            "no stacks on {} — start one with `fleet stack spawn <session> \"<prompt>\"`",
            c.label
        );
        return Ok(());
    }
    let mut sorted = stacks;
    sorted.sort_by_key(|s| std::cmp::Reverse(s.meta_str("updated").unwrap_or_default()));
    for s in &sorted {
        println!(
            "{}  {}  {}  {}  {}",
            s.label().bold(),
            s.id().dimmed(),
            counts(s),
            s.meta_str("cwd").unwrap_or_else(|| "-".into()).dimmed(),
            s.meta_str("updated")
                .map(|u| u.chars().take(16).collect::<String>().replace('T', " "))
                .unwrap_or_default()
                .dimmed()
        );
    }
    Ok(())
}

/// `fleet stack show <stack> [--path]`
pub fn show(q: &str, json: bool, path_only: bool) -> Result<()> {
    let c = Ctx::new()?;
    c.sync()?;
    let s = c.resolve(q)?;
    if path_only {
        println!("{}", c.path_of(&s).display());
    } else if json {
        print_json(&c.view(&s))?;
    } else {
        print!("{}", stack::serialize_body(&s, &c.paths));
    }
    Ok(())
}

/// `fleet stack sync [--input <src>]`
pub fn sync(input: Option<&str>, json: bool) -> Result<()> {
    let c = Ctx::load(input)?;
    if !c.rows_ok {
        return Err(Error::Other(
            "cannot read the session registry — nothing synced".into(),
        ));
    }
    let changed = c.sync()?;
    if json {
        let stacks = c.stacks();
        return print_json(&json!({
            "host": c.label,
            "changed": changed,
            "stacks": stacks.iter().map(|s| c.view(s)).collect::<Vec<_>>(),
        }));
    }
    if changed.is_empty() {
        println!("stacks are in sync");
    } else {
        println!("updated {}", changed.join(", "));
    }
    Ok(())
}

// ------------------------------------------------------------------ new / ensure

/// What creating (or finding) a stack yielded.
struct Ensured {
    stack: Stack,
    created: bool,
    generated: bool,
    warning: Option<String>,
}

impl Ensured {
    /// `new` / `ensure --json`: the StackView with `created` (bool: made by this call),
    /// `generated`, `warning` — and, since `created` shadows the StackView's timestamp of the
    /// same name, that timestamp as `createdAt` and the untouched StackView as `stack`.
    fn json(&self, view: Value) -> Value {
        let created_at = view.get("created").cloned().unwrap_or(Value::Null);
        with(
            view.clone(),
            json!({
                "created": self.created,
                "createdAt": created_at,
                "generated": self.generated,
                "warning": self.warning,
                "stack": view,
            }),
        )
    }
}

/// Create a stack around the live session `s`: the model writes Summary / Resources (unless
/// told not to, or it fails — then the skeleton). Never fails over the model.
fn create_around(c: &Ctx, s: &Session, label: Option<&str>, no_llm: bool) -> Result<Ensured> {
    let sid = s.session_id.clone().unwrap_or_default();
    let cwd_abs = s
        .cwd
        .clone()
        .ok_or_else(|| Error::Other(format!("no working directory known for {}", s.headline())))?;
    let cwd = c.paths.tilde(&cwd_abs);
    let git = std::path::Path::new(&cwd_abs)
        .is_dir()
        .then(|| stack::git_line(&cwd_abs, &c.paths))
        .flatten();
    let cfg = &config::get().stacks;
    let allow_llm = !no_llm && !discovery::is_fixture() && cfg.enabled() && !hosts::dry_run();
    let mut warning = None;
    let mut generated = false;
    let content = if allow_llm {
        let input = stack::GenInput {
            title: s.headline(),
            cwd: cwd.clone(),
            host: c.me.clone(),
            git: git.clone(),
            first_prompt: s.title.clone(),
            brief: brief::briefs_dir()
                .ok()
                .and_then(|d| brief::read(&d, &sid).ok().flatten())
                .map(|t| brief::parse_frontmatter(&t).1),
            conversation: crate::core::context::locate_transcript(
                &c.paths.claude_home,
                s.cwd.as_deref(),
                &sid,
            )
            .map(|p| stack::conversation_tail(&p, stack::TAIL_CHARS))
            .unwrap_or_default(),
        };
        match stack::generate(&input, &cfg.model()) {
            Ok(g) => {
                generated = true;
                g
            }
            Err(e) => {
                warning = Some(format!(
                    "StackBrief generation failed ({e}) — wrote the skeleton; edit it with `fleet stack edit`"
                ));
                stack::skeleton(&s.headline(), &cwd, git.as_deref())
            }
        }
    } else {
        stack::skeleton(&s.headline(), &cwd, git.as_deref())
    };
    let label = label
        .and_then(stack::clean_label)
        .or_else(|| content.label.clone())
        .or_else(|| stack::clean_label(&s.headline()))
        .unwrap_or_else(|| "stack".into());
    let now = brief::iso_now();
    let id = stack::new_id(&c.dir);
    let member = stack::member_for(s, &c.me, &now, &c.paths)
        .ok_or_else(|| Error::Other(format!("{} has no session id yet", s.headline())))?;
    let mut st = stack::new_stack(stack::NewStack {
        id: &id,
        label: &label,
        host: &c.me,
        cwd: &cwd,
        now: &now,
        member,
        content: &content,
        generated,
    });
    c.save(&mut st)?;
    Ok(Ensured {
        stack: st,
        created: true,
        generated,
        warning,
    })
}

/// The session's stack, or a new one around it.
fn ensure_for(c: &Ctx, s: &Session, label: Option<&str>, no_llm: bool) -> Result<Ensured> {
    let all = c.stacks();
    let sid = s.session_id.as_deref().unwrap_or_default();
    if let Some(st) = stack::stack_of(&all, sid) {
        return Ok(Ensured {
            stack: st.clone(),
            created: false,
            generated: false,
            warning: None,
        });
    }
    create_around(c, s, label, no_llm)
}

fn report_ensured(c: &Ctx, e: &Ensured, s: &Session, json: bool) -> Result<()> {
    if json {
        return print_json(&e.json(c.view(&e.stack)));
    }
    if let Some(w) = &e.warning {
        eprintln!("{}", format!("warning: {w}").yellow());
    }
    let path = c.path_of(&e.stack);
    if e.created {
        println!(
            "created stack {} ({}) around {} — {}",
            e.stack.label().bold(),
            e.stack.id(),
            s.headline().bold(),
            home_rel(&path.display().to_string()).dimmed()
        );
    } else {
        println!(
            "{} is in stack {} ({}) — {}",
            s.headline().bold(),
            e.stack.label().bold(),
            e.stack.id(),
            home_rel(&path.display().to_string()).dimmed()
        );
    }
    Ok(())
}

/// `fleet stack new --from <target>`
pub fn new(from: &str, label: Option<&str>, no_llm: bool, json: bool) -> Result<()> {
    let c = Ctx::new()?;
    c.sync()?;
    let s = c.session(from)?;
    let all = c.stacks();
    if let Some(st) = stack::stack_of(&all, s.session_id.as_deref().unwrap_or_default()) {
        return Err(Error::Other(format!(
            "{} is already in stack {} ({}) — `fleet stack show {}`, or spawn a sibling with `fleet stack spawn`",
            s.headline(),
            st.label(),
            st.id(),
            st.id()
        )));
    }
    let e = create_around(&c, &s, label, no_llm)?;
    report_ensured(&c, &e, &s, json)
}

/// `fleet stack ensure <target>`
pub fn ensure(target: &str, label: Option<&str>, no_llm: bool, json: bool) -> Result<()> {
    let c = Ctx::new()?;
    c.sync()?;
    let s = c.session(target)?;
    let e = ensure_for(&c, &s, label, no_llm)?;
    report_ensured(&c, &e, &s, json)
}

// ------------------------------------------------------------------ add / remove

/// Add live session `s` (running on `host`) to the stack with id `id` (re-read from disk).
/// Refuses when `s` is live in another stack of this host.
fn add_to(c: &Ctx, id: &str, s: &Session, host: &str) -> Result<Stack> {
    let sid = s.session_id.clone().unwrap_or_default();
    let all = c.stacks();
    if let Some(other) = all.iter().find(|st| {
        st.id() != id
            && st
                .member(&sid)
                .is_some_and(|m| m.is_live() && st.member_host(m, &c.me) == host)
    }) {
        return Err(Error::Other(format!(
            "{} is already in stack {} ({}) — `fleet stack remove {} {}` first",
            s.headline(),
            other.label(),
            other.id(),
            other.id(),
            sid
        )));
    }
    let mut st = stack::load(&c.dir, id)?
        .ok_or_else(|| Error::exit(EXIT_NOTHING, format!("stack {id} is gone")))?;
    let m = stack::member_for(s, host, &brief::iso_now(), &c.paths)
        .ok_or_else(|| Error::Other(format!("{} has no session id yet", s.headline())))?;
    if stack::add_member(&mut st, m) {
        c.save(&mut st)?;
    }
    Ok(st)
}

/// `fleet stack add <stack> <target> [--on <host>] [--input <src>]`: a session of this
/// machine, or — `--on` — one running on another host, resolved among that host's rows
/// (`fleet list --json` over ssh, or the ones `--input` carries for it).
pub fn add(q: &str, target: &str, on: Option<&str>, input: Option<&str>, json: bool) -> Result<()> {
    let c = Ctx::load(input)?;
    c.sync()?;
    let st = c.resolve(q)?;
    let (s, host) = match on.map(str::trim).filter(|h| !h.is_empty()) {
        Some(h) if !c.is_me(h) => (c.session_on(h, target, input.is_some())?, h.to_string()),
        _ => (c.session(target)?, c.me.clone()),
    };
    let st = add_to(&c, &st.id(), &s, &host)?;
    if json {
        return print_json(&c.view(&st));
    }
    println!(
        "added {}{} to stack {} ({}, {})",
        s.headline().bold(),
        if host == c.me {
            String::new()
        } else {
            format!(" (on {host})")
        },
        st.label().bold(),
        st.id(),
        counts(&st)
    );
    Ok(())
}

/// `fleet stack rename <stack> <label>`: the label (frontmatter `label:`, the header line
/// follows). A human change, like `set`: `editedAt` is stamped. Nothing is typed anywhere.
pub fn rename(q: &str, label: &str, json: bool) -> Result<()> {
    let label = stack::clean_label(label).ok_or_else(|| {
        Error::Other("the label must be one non-empty line (up to 80 characters)".into())
    })?;
    let c = Ctx::new()?;
    c.sync()?;
    let mut st = c.resolve(q)?;
    let from = st.label();
    if from == label {
        return report_saved(&c, &st, json);
    }
    st.meta.insert("label".into(), Value::String(label.clone()));
    st.meta
        .insert("editedAt".into(), Value::String(brief::iso_now()));
    c.save(&mut st)?;
    if json {
        return print_json(&c.view(&st));
    }
    println!(
        "renamed stack {} → {} ({})",
        from.dimmed(),
        st.label().bold(),
        st.id()
    );
    Ok(())
}

/// `fleet stack remove <stack> <target|session_id>`
pub fn remove(q: &str, target: &str, json: bool) -> Result<()> {
    let c = Ctx::new()?;
    c.sync()?;
    let mut st = c.resolve(q)?;
    let t = target.trim().to_lowercase();
    let by_id: Vec<String> = st
        .members
        .iter()
        .filter(|m| {
            let id = m.session.to_lowercase();
            id == t || (t.len() >= 4 && id.starts_with(&t))
        })
        .map(|m| m.session.clone())
        .collect();
    let sid = match by_id.len() {
        1 => by_id[0].clone(),
        n if n > 1 => {
            return Err(Error::exit(
                EXIT_AMBIGUOUS,
                format!(
                    "\"{target}\" matches {n} members: {} — be more specific",
                    by_id.join(", ")
                ),
            ));
        }
        _ => {
            let by_name: Vec<String> = st
                .members
                .iter()
                .filter(|m| m.name.as_deref().is_some_and(|n| n.to_lowercase() == t))
                .map(|m| m.session.clone())
                .collect();
            if by_name.len() == 1 {
                by_name[0].clone()
            } else {
                let s = c.session(target)?;
                s.session_id.unwrap_or_default()
            }
        }
    };
    let Some(pos) = st.members.iter().position(|m| m.session == sid) else {
        return Err(Error::exit(
            EXIT_NOTHING,
            format!(
                "{target} is not a member of stack {} ({})",
                st.label(),
                st.id()
            ),
        ));
    };
    let gone = st.members.remove(pos);
    c.save(&mut st)?;
    if json {
        return print_json(&c.view(&st));
    }
    println!(
        "removed {} from stack {} ({})",
        gone.name.unwrap_or(gone.session).bold(),
        st.label().bold(),
        counts(&st)
    );
    Ok(())
}

// ------------------------------------------------------------------ set / edit / rm

/// Save `text` as a human edit of the stack `id`. `expect_updated`: refuse (exit 3, nothing
/// written) unless the stored `updated` still is this.
fn save_edit(c: &Ctx, id: &str, text: &str, expect_updated: Option<&str>) -> Result<Stack> {
    let before = stack::load(&c.dir, id)?
        .ok_or_else(|| Error::exit(EXIT_NOTHING, format!("stack {id} is gone")))?;
    let current = before.meta_str("updated").unwrap_or_default();
    if let Some(want) = expect_updated
        && want != current
    {
        return Err(Error::exit(
            EXIT_NOTHING,
            format!(
                "stack {} ({id}) changed since it was opened (updated {}) — not saved",
                before.label(),
                if current.is_empty() { "-" } else { &current }
            ),
        ));
    }
    let now = brief::iso_now();
    let mut st = stack::human_edit(&before, text, &now)?;
    if hosts::dry_run() {
        st.meta.insert("updated".into(), Value::String(now));
        eprintln!("dry-run: would write {}:", c.path_of(&st).display());
        print!("{}", stack::serialize_stack(&st, &c.paths));
        return Ok(st);
    }
    stack::save(&c.dir, &mut st, &now, &c.paths)?;
    Ok(st)
}

fn report_saved(c: &Ctx, st: &Stack, json: bool) -> Result<()> {
    if hosts::dry_run() {
        return Ok(());
    }
    if json {
        return print_json(&c.view(st));
    }
    println!("saved stack {} ({})", st.label().bold(), st.id());
    Ok(())
}

/// `fleet stack set <stack> [--expect-updated <iso>]` — markdown on stdin.
pub fn set(q: &str, expect_updated: Option<&str>, json: bool) -> Result<()> {
    let mut text = String::new();
    std::io::stdin().read_to_string(&mut text)?;
    if text.trim().is_empty() {
        return Err(Error::Other(
            "empty StackBrief on stdin — nothing saved (pipe the markdown in)".into(),
        ));
    }
    let c = Ctx::new()?;
    let st = c.resolve(q)?;
    let id = st.id();
    match save_edit(&c, &id, &text, expect_updated) {
        Ok(saved) => report_saved(&c, &saved, json),
        Err(Error::Exit(EXIT_NOTHING, msg)) if json => {
            // A conflict (or the file vanished): the stored `updated` on stdout, so a caller
            // (the web PUT → 409 `{ error, updated }`) can reload and retry.
            let updated = stack::load(&c.dir, &id)
                .ok()
                .flatten()
                .and_then(|s| s.meta_str("updated"));
            print_json(&json!({ "error": msg, "id": id, "updated": updated }))?;
            Err(Error::Exit(EXIT_NOTHING, msg))
        }
        Err(e) => Err(e),
    }
}

/// Put `body` in a temp file, open the editor, read it back. → (text, temp path).
fn edit_text(body: &str, id: &str) -> Result<(String, PathBuf)> {
    use std::os::unix::fs::OpenOptionsExt;
    let path = std::env::temp_dir().join(format!(
        "fleet-{id}-{}-{}.md",
        std::process::id(),
        chrono::Local::now().format("%H%M%S")
    ));
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    f.write_all(body.as_bytes())?;
    drop(f);
    config_cmd::open_editor(&path)?;
    let text = std::fs::read_to_string(&path)?;
    Ok((text, path))
}

fn kept(path: &Path) -> String {
    format!("your edit is kept in {}", path.display())
}

/// `fleet stack edit <stack>`: the body in `$VISUAL` / `$EDITOR`, saved as a human edit. On
/// another host: fetched with `fleet stack show --json` there, edited here, written back with
/// `fleet stack set <id> --expect-updated <updated>`.
pub fn edit(t: &Target, q: &str, json: bool) -> Result<i32> {
    match t {
        Target::Local { .. } => edit_local(q, json).map(|_| 0),
        Target::Remote(r) => edit_remote(r, q, json),
    }
}

fn edit_local(q: &str, json: bool) -> Result<()> {
    let c = Ctx::new()?;
    c.sync()?;
    let st = c.resolve(q)?;
    if hosts::dry_run() {
        println!(
            "dry-run: would open {} on stack {} ({})",
            config_cmd::editor(),
            st.label(),
            c.path_of(&st).display()
        );
        return Ok(());
    }
    let opened = st.meta_str("updated").unwrap_or_default();
    let body = stack::serialize_body(&st, &c.paths);
    let (text, tmp) = edit_text(&body, &st.id())?;
    if text == body {
        let _ = std::fs::remove_file(&tmp);
        println!("no changes");
        return Ok(());
    }
    match save_edit(&c, &st.id(), &text, Some(&opened)) {
        Ok(saved) => {
            let _ = std::fs::remove_file(&tmp);
            report_saved(&c, &saved, json)
        }
        Err(Error::Exit(code, msg)) => Err(Error::Exit(code, format!("{msg}; {}", kept(&tmp)))),
        Err(e) => Err(Error::Other(format!("{e}; {}", kept(&tmp)))),
    }
}

fn edit_remote(r: &Remote, q: &str, json: bool) -> Result<i32> {
    let fetch = vec![
        "stack".to_string(),
        "show".into(),
        q.to_string(),
        "--json".into(),
    ];
    if hosts::dry_run() {
        let c = hosts::ssh_command(&r.dest, &hosts::remote_fleet_command(r, &fetch), Tty::Never);
        println!("{}", hosts::display_command(&c));
        println!(
            "dry-run: then {} on the body here, and `fleet stack set <id>` on {} with it on stdin",
            config_cmd::editor(),
            r.name
        );
        return Ok(0);
    }
    let got = hosts::capture_remote(r, &fetch, hosts::remote_timeout())?;
    if !got.ok() {
        let code = got
            .code
            .filter(|c| [EXIT_AMBIGUOUS, EXIT_NOTHING].contains(c));
        return Err(Error::exit(
            code.unwrap_or(1),
            format!("{}: {}", r.name, got.why(r)),
        ));
    }
    let v: Value = serde_json::from_str(&got.stdout).map_err(|e| {
        Error::Other(format!(
            "{}: unexpected `fleet stack show --json` output ({e}) — is fleet up to date there? (fleet install --host {})",
            r.name, r.name
        ))
    })?;
    let id = v["id"].as_str().unwrap_or_default().to_string();
    if !stack::is_stack_id(&id) {
        return Err(Error::Other(format!(
            "{}: no stack id in the answer",
            r.name
        )));
    }
    let body = v["body"].as_str().unwrap_or_default().to_string();
    let opened = v["updated"].as_str().unwrap_or_default().to_string();
    let (text, tmp) = edit_text(&body, &id)?;
    if text == body {
        let _ = std::fs::remove_file(&tmp);
        println!("no changes");
        return Ok(0);
    }
    let mut args = vec![
        "stack".to_string(),
        "set".into(),
        id.clone(),
        "--expect-updated".into(),
        opened,
    ];
    if json {
        args.push("--json".into());
    }
    let c = hosts::ssh_command(&r.dest, &hosts::remote_fleet_command(r, &args), Tty::Never);
    hosts::debug(&format!("[{} set] {}", r.name, hosts::display_command(&c)));
    let put = hosts::capture_input(c, Some(text.as_bytes()), hosts::remote_timeout())?;
    if !put.ok() {
        let code = if put.code == Some(EXIT_NOTHING) {
            EXIT_NOTHING
        } else {
            1
        };
        return Err(Error::exit(
            code,
            format!("{}: {}; {}", r.name, put.why(r), kept(&tmp)),
        ));
    }
    let _ = std::fs::remove_file(&tmp);
    print!("{}", put.stdout);
    Ok(0)
}

/// Ask on the controlling terminal. `Err` (exit 3) when there is none.
fn confirm(prompt: &str) -> Result<bool> {
    // `fleet mcp` runs commands with no one at the terminal to answer.
    if crate::core::tools::env_flag("FLEET_NO_TTY") {
        return Err(Error::exit(
            EXIT_NOTHING,
            "no terminal to confirm on (use -f)",
        ));
    }
    let tty = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .map_err(|_| Error::exit(EXIT_NOTHING, "no terminal to confirm on (use -f)"))?;
    let mut w = &tty;
    write!(w, "{prompt} [y/N] ")?;
    w.flush()?;
    let mut line = String::new();
    std::io::BufReader::new(&tty).read_line(&mut line)?;
    Ok(matches!(line.trim(), "y" | "Y" | "yes" | "YES" | "Yes"))
}

/// `fleet stack rm <stack> [-f]`
pub fn rm(q: &str, force: bool, json: bool) -> Result<()> {
    if json && !force {
        return Err(Error::Other(
            "`fleet stack rm --json` does not prompt — pass -f".into(),
        ));
    }
    let c = Ctx::new()?;
    let st = c.resolve(q)?;
    let id = st.id();
    if hosts::dry_run() {
        println!("dry-run: would delete {}", c.path_of(&st).display());
        return Ok(());
    }
    if !force
        && !confirm(&format!(
            "delete stack {} ({id}, {})?",
            st.label(),
            counts(&st)
        ))?
    {
        println!("kept");
        return Ok(());
    }
    stack::remove(&c.dir, &id)?;
    if json {
        return print_json(&json!({ "removed": id }));
    }
    println!("deleted stack {} ({id})", st.label().bold());
    Ok(())
}

// ------------------------------------------------------------------ spawn a sibling

pub struct SpawnArgs {
    pub target: String,
    pub prompt: Option<String>,
    pub label: Option<String>,
    pub no_llm: bool,
    pub opts: SpawnOpts,
    pub wait: bool,
    pub json: bool,
    /// `--on <host>`: the sibling runs there (the stack stays on this host).
    pub on: Option<String>,
}

/// `... (attach: tmux attach -t <session>)` → `<session>`.
fn tmux_session_of(desc: &str) -> Option<String> {
    let rest = desc.split("tmux attach -t ").nth(1)?;
    let s = rest.split(')').next()?.trim();
    (!s.is_empty()).then(|| s.to_string())
}

/// `fleet stack spawn <target> [prompt]`: `ensure` on the target, then a new session in its
/// cwd whose first prompt is the context line + `prompt` (through a prompt file), then wait for
/// it to register and add it. With `--on <host>`: [`spawn_on`].
pub fn spawn(a: SpawnArgs) -> Result<()> {
    let c = Ctx::new()?;
    c.sync()?;
    let src = c.session(&a.target)?;
    let on =
        a.on.as_deref()
            .map(str::trim)
            .filter(|h| !h.is_empty() && !c.is_me(h))
            .map(str::to_string);
    if let Some(h) = on {
        return spawn_on(&c, &src, &h, a);
    }
    let mut opts = a.opts;
    if opts.dir.is_none() {
        opts.dir = Some(src.cwd.clone().ok_or_else(|| {
            Error::Other(format!(
                "no working directory known for {} — pass --dir",
                src.headline()
            ))
        })?);
    }
    // Validate where it lands before creating anything.
    let (dir, backend_kind, name) = opts.resolve()?;
    let model = opts
        .model
        .as_deref()
        .map(commands::clean_model)
        .transpose()?
        .flatten();
    let e = ensure_for(&c, &src, a.label.as_deref(), a.no_llm)?;
    let id = e.stack.id();
    let path = c.path_of(&e.stack);
    let text = stack::stack_prompt(&path, a.prompt.as_deref().unwrap_or_default());
    let hex = id.trim_start_matches("st-");
    let prompt_file = commands::handoff_dir().join(format!(
        "{}-stack-{hex}.md",
        chrono::Local::now().format("%Y%m%d-%H%M%S")
    ));
    let pf = prompt_file.display().to_string();
    let launcher = commands::with_model(&opts.harness.command(), model.as_deref());
    if hosts::dry_run() {
        let line = backend::launch_command(
            &dir,
            Prompt::File(&pf),
            name.as_deref(),
            &launcher,
            opts.harness,
        );
        if a.json {
            return print_json(&json!({
                "dryRun": true,
                "stack": c.view(&e.stack),
                "created": e.created,
                "generated": e.generated,
                "warning": e.warning,
                "spawned": { "desc": format!("dry-run: {line}"), "dir": dir, "promptFile": pf, "tmuxSession": null },
                "session": null,
                "prompt": text,
                "contextLine": stack::context_line(&path),
            }));
        }
        if e.created {
            println!(
                "dry-run: would create stack {} ({}) around {} — not written",
                e.stack.label(),
                id,
                src.headline()
            );
        }
        println!(
            "dry-run: would spawn a {} sibling in stack {}: {line}",
            backend_kind.label(),
            e.stack.label()
        );
        println!(
            "dry-run: first prompt ({} — not written):\n{text}",
            home_rel(&pf)
        );
        return Ok(());
    }
    if let Some(w) = &e.warning
        && !a.json
    {
        eprintln!("{}", format!("warning: {w}").yellow());
    }
    std::fs::create_dir_all(commands::handoff_dir())?;
    std::fs::write(&prompt_file, &text)?;
    let before: HashSet<String> = discovery::discover().iter().map(Session::key).collect();
    let desc = backend::spawn(
        backend_kind,
        &dir,
        Prompt::File(&pf),
        name.as_deref(),
        opts.tmux_session.as_deref(),
        opts.window,
        &launcher,
        opts.harness,
    )?;
    if !a.json {
        println!(
            "{} {desc} in {} — sibling in stack {} ({}) (prompt: {})",
            "→".cyan(),
            home_rel(&dir),
            e.stack.label().bold(),
            id,
            home_rel(&pf).dimmed()
        );
    }
    let mut joined: Option<Session> = None;
    let mut add_error = None;
    if a.wait {
        match commands::await_new_session(&before, &dir, AWAIT) {
            Some(mut s) => {
                let mut one = vec![s.clone()];
                naming::stamp_titles(&mut one);
                title::stamp_display_titles(&mut one);
                s = one.remove(0);
                match add_to(&c, &id, &s, &c.me) {
                    Ok(_) => {}
                    Err(err) => add_error = Some(err.to_string()),
                }
                joined = Some(s);
            }
            None => {
                add_error = Some(format!(
                    "the new session has not registered yet — add it later: fleet stack add {id} <session>"
                ));
            }
        }
    }
    let hint = format!(
        "fleet stack add {id} {}",
        name.clone().unwrap_or_else(|| "<new session>".into())
    );
    // Fresh state for the report (the new member, synced names).
    let c = Ctx::new()?;
    c.sync()?;
    let st = stack::load(&c.dir, &id)?.unwrap_or(e.stack.clone());
    if a.json {
        return print_json(&json!({
            "stack": c.view(&st),
            "created": e.created,
            "generated": e.generated,
            "warning": e.warning,
            "spawned": {
                "desc": desc.desc,
                "dir": dir,
                "promptFile": pf,
                "tmuxSession": desc.tmux_session,
            },
            "session": joined.as_ref().map(|s| json!({
                "session_id": s.session_id, "pid": s.pid, "display_title": s.headline(),
            })),
            "addError": add_error,
            "addCommand": (!a.wait).then_some(hint.clone()),
            "contextLine": stack::context_line(&path),
        }));
    }
    match (&joined, &add_error) {
        (Some(s), None) => println!(
            "  joined as {} — steer it with `fleet send {} \"…\"`",
            s.headline().bold(),
            s.label()
        ),
        (_, Some(err)) => println!("  {}", err.yellow()),
        (None, None) => println!("  once it has registered, add it: {}", hint.bold()),
    }
    Ok(())
}

// ------------------------------------------------------------------ over the web (no ssh)

/// What `fleet -H <host> stack …` can do through that host's web server (`hosts.<host>.web`)
/// when there is no ssh destination for it — the sheds reach each other over HTTP only, and a
/// sibling's remote context line says `fleet -H <home> stack show` / `set`.
pub enum WebOp {
    List {
        json: bool,
    },
    Show {
        stack: String,
        json: bool,
        path: bool,
    },
    Set {
        stack: String,
        expect_updated: Option<String>,
        json: bool,
    },
    Rename {
        stack: String,
        label: String,
        json: bool,
    },
    Rm {
        stack: String,
        force: bool,
        json: bool,
    },
}

/// The `curl` for one stack request to `base` (no HTTP client in the crate, like `brief
/// --regenerate`): `-w` appends the status code on its own line; a body goes in on stdin.
fn curl(base: &str, host: &str, method: &str, path: &str, body: bool) -> std::process::Command {
    let url = format!(
        "{}/api/hosts/{}/{path}",
        base.trim_end_matches('/'),
        crate::cli::brief::path_segment(host)
    );
    let mut c = std::process::Command::new("curl");
    c.args(["-sS", "--max-time", "30", "-X", method]);
    if body {
        c.args([
            "-H",
            "Content-Type: application/json",
            "--data-binary",
            "@-",
        ]);
    }
    c.args(["-w", "\n%{http_code}", &url]);
    c
}

/// Run the curl, split off the status; a transport failure or `000` is "cannot reach".
fn fetch(
    c: std::process::Command,
    input: Option<&[u8]>,
    base: &str,
    host: &str,
) -> Result<(String, Value)> {
    let got = hosts::capture_input(c, input, Duration::from_secs(35))
        .map_err(|e| Error::Other(format!("{e} — curl is needed to reach {host} over the web")))?;
    let unreachable = || {
        Error::exit(
            hosts::EXIT_UNREACHABLE,
            format!(
                "cannot reach {host}'s web server at {base} (hosts.{host}.web) — is fleet web running there?"
            ),
        )
    };
    if got.code != Some(0) {
        return Err(unreachable());
    }
    let (body, status) = got
        .stdout
        .trim_end()
        .rsplit_once('\n')
        .map(|(b, s)| (b.to_string(), s.trim().to_string()))
        .unwrap_or_else(|| (String::new(), got.stdout.trim().to_string()));
    if status == "000" {
        return Err(unreachable());
    }
    let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    Ok((status, v))
}

/// The CLI's exit codes for the web API's statuses (ARCHITECTURE.md → Session stacks).
fn web_error(host: &str, status: &str, v: &Value) -> Error {
    let msg = v["error"].as_str().unwrap_or("request failed").to_string();
    let code = match status {
        "404" | "409"
            if v.get("updated").is_some()
                || msg.contains("no stack")
                || msg.contains("not found") =>
        {
            EXIT_NOTHING
        }
        "409" => EXIT_AMBIGUOUS,
        "404" => EXIT_NOTHING,
        _ => 1,
    };
    Error::exit(code, format!("{host}: {msg} (HTTP {status})"))
}

/// `fleet -H <host> stack …` for a host with `web` but no `ssh`: the same answers, fetched from
/// its web server. `<stack>` must be a stack id here (the routes take ids). Dry run: the curl.
pub fn via_web(host: &str, base: &str, op: WebOp) -> Result<()> {
    let id_of = |q: &str| -> Result<String> {
        let q = q.trim();
        let id = if stack::is_stack_id(q) {
            q.to_string()
        } else {
            format!("st-{q}")
        };
        if !stack::is_stack_id(&id) {
            return Err(Error::Other(format!(
                "over the web a stack is addressed by its id (st-xxxxxxxx), not \"{q}\" — `fleet -H {host} stack list` shows them"
            )));
        }
        Ok(id)
    };
    let dry = |c: &std::process::Command| {
        if hosts::dry_run() {
            println!("{}", hosts::display_command(c));
            true
        } else {
            false
        }
    };
    match op {
        WebOp::List { json } => {
            let c = curl(base, host, "GET", "stacks", false);
            if dry(&c) {
                return Ok(());
            }
            let (status, v) = fetch(c, None, base, host)?;
            if status != "200" {
                return Err(web_error(host, &status, &v));
            }
            if json {
                return print_json(&v);
            }
            let stacks = v["stacks"].as_array().cloned().unwrap_or_default();
            if stacks.is_empty() {
                println!("no stacks on {host}");
                return Ok(());
            }
            for s in &stacks {
                let members = s["members"].as_array().cloned().unwrap_or_default();
                let live = members
                    .iter()
                    .filter(|m| m["live"].as_bool() != Some(false) && m["closed"].is_null())
                    .count();
                let closed = members.len() - live;
                println!(
                    "{}  {}  {}  {}  {}",
                    s["label"].as_str().unwrap_or("-").bold(),
                    s["id"].as_str().unwrap_or("-").dimmed(),
                    if closed == 0 {
                        format!("{live} live")
                    } else {
                        format!("{live} live · {closed} closed")
                    },
                    s["cwd"].as_str().unwrap_or("-").dimmed(),
                    s["updated"]
                        .as_str()
                        .map(|u| u.chars().take(16).collect::<String>().replace('T', " "))
                        .unwrap_or_default()
                        .dimmed()
                );
            }
            Ok(())
        }
        WebOp::Show { stack, json, path } => {
            let id = id_of(&stack)?;
            let c = curl(base, host, "GET", &format!("stacks/{id}"), false);
            if dry(&c) {
                return Ok(());
            }
            let (status, v) = fetch(c, None, base, host)?;
            if status != "200" {
                return Err(web_error(host, &status, &v));
            }
            if path {
                println!("{}", v["path"].as_str().unwrap_or_default());
            } else if json {
                print_json(&v)?;
            } else {
                print!("{}", v["body"].as_str().unwrap_or_default());
            }
            Ok(())
        }
        WebOp::Set {
            stack,
            expect_updated,
            json,
        } => {
            let id = id_of(&stack)?;
            let mut text = String::new();
            std::io::stdin().read_to_string(&mut text)?;
            if text.trim().is_empty() {
                return Err(Error::Other(
                    "empty StackBrief on stdin — nothing saved (pipe the markdown in)".into(),
                ));
            }
            if text.len() > stack::MAX_EDIT_BYTES {
                return Err(Error::Other(format!(
                    "StackBrief too long (max {} kB)",
                    stack::MAX_EDIT_BYTES / 1024
                )));
            }
            let body = json!({ "markdown": text, "expectUpdated": expect_updated }).to_string();
            let c = curl(base, host, "PUT", &format!("stacks/{id}"), true);
            if dry(&c) {
                return Ok(());
            }
            let (status, v) = fetch(c, Some(body.as_bytes()), base, host)?;
            if status != "200" {
                if status == "409" && json {
                    print_json(&json!({ "error": v["error"], "id": id, "updated": v["updated"] }))?;
                }
                return Err(web_error(host, &status, &v));
            }
            if json {
                return print_json(&v);
            }
            println!(
                "saved stack {} ({id}) on {host}",
                v["label"].as_str().unwrap_or(&id).bold()
            );
            Ok(())
        }
        WebOp::Rename { stack, label, json } => {
            let id = id_of(&stack)?;
            let label = stack::clean_label(&label).ok_or_else(|| {
                Error::Other("the label must be one non-empty line (up to 80 characters)".into())
            })?;
            let body = json!({ "label": label }).to_string();
            let c = curl(base, host, "POST", &format!("stacks/{id}/rename"), true);
            if dry(&c) {
                return Ok(());
            }
            let (status, v) = fetch(c, Some(body.as_bytes()), base, host)?;
            if status != "200" {
                return Err(web_error(host, &status, &v));
            }
            if json {
                return print_json(&v);
            }
            println!(
                "renamed stack {} → {} ({id}) on {host}",
                stack.dimmed(),
                label.bold()
            );
            Ok(())
        }
        WebOp::Rm { stack, force, json } => {
            let id = id_of(&stack)?;
            if !force && !hosts::dry_run() && !confirm(&format!("delete stack {id} on {host}?"))? {
                println!("kept");
                return Ok(());
            }
            let c = curl(base, host, "DELETE", &format!("stacks/{id}"), false);
            if dry(&c) {
                return Ok(());
            }
            let (status, v) = fetch(c, None, base, host)?;
            if status != "200" {
                return Err(web_error(host, &status, &v));
            }
            if json {
                return print_json(&v);
            }
            println!("deleted stack {id} on {host}");
            Ok(())
        }
    }
}

/// `cwd` on another host is `dir` (`~/…` or absolute, as passed to `spawn --dir` there).
fn same_remote_dir(cwd: &str, dir: &str) -> bool {
    let cwd = cwd.trim_end_matches('/');
    let dir = dir.trim_end_matches('/');
    match dir.strip_prefix('~') {
        Some("") => true,
        Some(rest) => cwd.ends_with(rest),
        None => cwd == dir,
    }
}

/// `fleet stack spawn <target> --on <host>`: `ensure` on the target (here), then
/// `fleet --local spawn` on `host` over ssh in `--dir` (default: the target's cwd as `~/…`,
/// the same place on that machine) with the remote context line + `prompt`, then wait for it
/// to register there (`fleet list --json` over ssh) and add it as a member of that host.
fn spawn_on(c: &Ctx, src: &Session, host: &str, a: SpawnArgs) -> Result<()> {
    let r = remote_of(host)?;
    let opts = a.opts;
    let dir = match opts.dir.as_deref().map(str::trim).filter(|d| !d.is_empty()) {
        Some(d) => d.to_string(),
        None => c.paths.tilde(src.cwd.as_deref().ok_or_else(|| {
            Error::Other(format!(
                "no working directory known for {} — pass --dir",
                src.headline()
            ))
        })?),
    };
    let name = opts
        .name
        .as_deref()
        .map(crate::core::title::clean_title)
        .transpose()?;
    let model = opts
        .model
        .as_deref()
        .map(commands::clean_model)
        .transpose()?
        .flatten();
    let e = ensure_for(c, src, a.label.as_deref(), a.no_llm)?;
    let id = e.stack.id();
    let line = stack::remote_context_line(&id, &e.stack.home(&c.me));
    let text = stack::prompt_after(&line, a.prompt.as_deref().unwrap_or_default());
    let mut args: Vec<String> = vec!["spawn".into(), "--dir".into(), dir.clone()];
    if let Some(n) = &name {
        args.extend(["--name".into(), n.clone()]);
    }
    if let Some(m) = &model {
        args.extend(["--model".into(), m.clone()]);
    }
    if opts.harness != Default::default() {
        args.extend(["--harness".into(), opts.harness.id().to_string()]);
    }
    if let Some(b) = opts.backend {
        args.extend(["--backend".into(), b.label().to_string()]);
    }
    if let Some(t) = &opts.tmux_session {
        args.extend(["--tmux-session".into(), t.clone()]);
    }
    if opts.window {
        args.push("--window".into());
    }
    args.extend(["--".into(), text.clone()]);
    let cmd = hosts::ssh_command(&r.dest, &hosts::remote_fleet_command(&r, &args), Tty::Never);
    let shown = hosts::display_command(&cmd);
    if hosts::dry_run() {
        if a.json {
            return print_json(&json!({
                "dryRun": true,
                "stack": c.view(&e.stack),
                "created": e.created,
                "generated": e.generated,
                "warning": e.warning,
                "spawned": { "desc": format!("dry-run: {shown}"), "dir": dir, "host": host, "promptFile": null, "tmuxSession": null },
                "session": null,
                "prompt": text,
                "contextLine": line,
            }));
        }
        if e.created {
            println!(
                "dry-run: would create stack {} ({}) around {} — not written",
                e.stack.label(),
                id,
                src.headline()
            );
        }
        println!(
            "dry-run: would spawn a sibling on {} in stack {}: {shown}",
            r.name,
            e.stack.label()
        );
        println!("dry-run: first prompt:\n{text}");
        return Ok(());
    }
    if let Some(w) = &e.warning
        && !a.json
    {
        eprintln!("{}", format!("warning: {w}").yellow());
    }
    let before: HashSet<String> = remote_rows(host)
        .unwrap_or_default()
        .iter()
        .map(Session::key)
        .collect();
    hosts::debug(&format!("[{} spawn] {shown}", r.name));
    let got = hosts::capture(cmd, Duration::from_secs(60))?;
    if !got.ok() {
        return Err(Error::Other(format!("{}: {}", r.name, got.why(&r))));
    }
    let desc = got.stdout.trim().to_string();
    if !a.json {
        println!(
            "{} {desc} on {} — sibling in stack {} ({})",
            "→".cyan(),
            r.name.bold(),
            e.stack.label().bold(),
            id
        );
    }
    let mut joined: Option<Session> = None;
    let mut add_error = None;
    if a.wait {
        let deadline = std::time::Instant::now() + AWAIT;
        let mut found = None;
        while std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_secs(3));
            let Ok(rows) = remote_rows(host) else {
                continue;
            };
            if let Some(s) = rows.into_iter().find(|s| {
                !before.contains(&s.key())
                    && s.session_id.as_deref().is_some_and(brief::is_session_id)
                    && s.cwd
                        .as_deref()
                        .is_some_and(|cwd| same_remote_dir(cwd, &dir))
            }) {
                found = Some(s);
                break;
            }
        }
        match found {
            Some(s) => {
                match add_to(c, &id, &s, host) {
                    Ok(_) => {}
                    Err(err) => add_error = Some(err.to_string()),
                }
                joined = Some(s);
            }
            None => {
                add_error = Some(format!(
                    "the new session has not registered on {host} yet — add it later: fleet stack add {id} <session> --on {host}"
                ));
            }
        }
    }
    let hint = format!(
        "fleet stack add {id} {} --on {host}",
        name.clone().unwrap_or_else(|| "<new session>".into())
    );
    let c = Ctx::new()?;
    c.sync()?;
    let st = stack::load(&c.dir, &id)?.unwrap_or(e.stack.clone());
    if a.json {
        return print_json(&json!({
            "stack": c.view(&st),
            "created": e.created,
            "generated": e.generated,
            "warning": e.warning,
            "spawned": {
                "desc": desc,
                "dir": dir,
                "host": host,
                "promptFile": null,
                "tmuxSession": tmux_session_of(&desc),
            },
            "session": joined.as_ref().map(|s| json!({
                "session_id": s.session_id, "pid": s.pid, "display_title": s.headline(), "host": host,
            })),
            "addError": add_error,
            "addCommand": (!a.wait).then_some(hint.clone()),
            "contextLine": line,
        }));
    }
    match (&joined, &add_error) {
        (Some(s), None) => println!(
            "  joined as {} on {host} — steer it with `fleet -H {host} send {} \"…\"`",
            s.headline().bold(),
            s.label()
        ),
        (_, Some(err)) => println!("  {}", err.yellow()),
        (None, None) => println!("  once it has registered, add it: {}", hint.bold()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_dirs_match_by_home_relative_suffix() {
        assert!(same_remote_dir("/Users/x/Code/app", "~/Code/app"));
        assert!(same_remote_dir("/home/y/Code/app/", "~/Code/app"));
        assert!(!same_remote_dir("/home/y/Code/app2", "~/Code/app"));
        assert!(same_remote_dir("/srv/app", "/srv/app/"));
        assert!(!same_remote_dir("/srv/app", "/srv/other"));
    }

    #[test]
    fn tmux_session_is_read_off_the_spawn_description() {
        assert_eq!(
            tmux_session_of(
                "spawned tmux session login-tests  (attach: tmux attach -t login-tests)"
            )
            .as_deref(),
            Some("login-tests")
        );
        assert_eq!(tmux_session_of("spawned iTerm tab"), None);
    }
}
