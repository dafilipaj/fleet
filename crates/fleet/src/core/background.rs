//! What a Claude session runs besides its main thread — subagents and background shells —
//! and whether the main thread itself is free.
//!
//! Claude's registry says `busy` while *any* agent of the session generates, background
//! subagents included, and `shell` while only background shells run. Either way the main
//! prompt may be free: its turn ended, input typed now is taken at once. Fleet's consumers
//! (send, the queue, briefs, Slack links) ask exactly that, so [`for_session`] reads the
//! main transcript and `discover` reports such a session as `idle` with its `background`.
//!
//! From the transcript (main thread only, `isSidechain` lines skipped):
//! - a turn ends with a `system` / `turn_duration` entry; any user or assistant entry after
//!   it is a turn in progress;
//! - an `Agent` call answered with `toolUseResult.status: "async_launched"` starts a
//!   background agent (`agentId`, `description`, `resolvedModel`); a Bash call answered
//!   with `toolUseResult.backgroundTaskId` starts a background shell;
//! - a `<task-notification>` (`<task-id>`, `<status>`) ends either — wherever it is
//!   written: a user entry or a queue operation; `SendMessage` to a finished agent starts
//!   it again, `TaskStop` / `KillShell` stop one;
//! - an `Agent` call not answered yet, in a turn in progress, is a foreground agent.
//!
//! None of this is a stable Claude Code API: what is not recognised changes nothing (the
//! registry status stands). Like [`super::prompted`], the first read scans the tail
//! (2 MiB) and later reads only what was appended.

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::Value;

const FIRST_TAIL: u64 = 2 * 1024 * 1024;
const AGENT_TAIL: u64 = 64 * 1024;
const MAX_DONE: usize = 50;

/// A session's subagents and background shells (`background` in `fleet list --json`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Background {
    /// Running subagents: background ones, and foreground ones of the turn in progress.
    pub agents: Vec<Agent>,
    /// Running background shells (`run_in_background` Bash).
    pub shells: u32,
}

impl Background {
    pub fn is_empty(&self) -> bool {
        self.agents.is_empty() && self.shells == 0
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Agent {
    pub id: String,
    pub description: Option<String>,
    /// The subagent type (`general-purpose`, `Explore`, …).
    #[serde(rename = "type")]
    pub agent_type: Option<String>,
    pub model: Option<String>,
    /// `false`: a foreground agent — the main thread waits on it.
    pub background: bool,
    /// Milliseconds since the epoch.
    pub started_at: Option<i64>,
    /// The agent transcript's last write (ms).
    pub last_activity_at: Option<i64>,
    /// Its last tool call, short: `Bash: run the tests`.
    pub activity: Option<String>,
}

/// The main thread and what runs beside it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Main {
    /// When the main thread's last turn ended; `None` while one is in progress (or unknown).
    pub ended_at: Option<i64>,
    pub background: Background,
}

#[derive(Debug, Clone, Default)]
struct State {
    /// Bytes scanned, up to the last complete line.
    offset: u64,
    /// `Some(ts)`: the last turn ended then; `None`: in progress, or not seen.
    ended_at: Option<i64>,
    /// A turn was seen (ended or not) — otherwise nothing is known.
    seen_turn: bool,
    /// Background agents running, by agent id.
    agents: BTreeMap<String, Agent>,
    /// Finished background agents (a `SendMessage` can start one again).
    done: BTreeMap<String, Agent>,
    /// Foreground agent calls not answered yet, by tool_use id.
    foreground: BTreeMap<String, Agent>,
    /// Background shells running: task id → started (ms).
    shells: BTreeMap<String, Option<i64>>,
}

static CACHE: LazyLock<Mutex<HashMap<PathBuf, State>>> = LazyLock::new(Default::default);

/// The main thread's state for a live Claude session. `started_at`: the process start
/// (registry `startedAt`) — what was launched before it is not running any more.
pub fn for_session(
    home: &Path,
    cwd: Option<&str>,
    session_id: &str,
    started_at: Option<i64>,
) -> Option<Main> {
    let path = super::context::locate_transcript(home, cwd, session_id)?;
    let size = std::fs::metadata(&path).ok()?.len();
    let cached = CACHE.lock().ok().and_then(|c| c.get(&path).cloned());
    let state = match cached {
        Some(st) if st.offset == size => st,
        Some(mut st) if st.offset < size => {
            let from = st.offset;
            scan(&path, &mut st, from, size, true)?;
            st
        }
        _ => {
            let mut st = State::default();
            let start = size.saturating_sub(FIRST_TAIL);
            scan(&path, &mut st, start, size, start == 0)?;
            st
        }
    };
    if let Ok(mut c) = CACHE.lock() {
        c.insert(path.clone(), state.clone());
    }
    if !state.seen_turn {
        return None;
    }
    Some(main_of(&state, &path, started_at))
}

fn main_of(st: &State, path: &Path, started_at: Option<i64>) -> Main {
    let alive = |at: Option<i64>| match (at, started_at) {
        (Some(at), Some(start)) => at >= start,
        _ => true,
    };
    let subagents = path.with_extension("").join("subagents");
    let mut agents: Vec<Agent> = st
        .agents
        .values()
        .filter(|a| alive(a.started_at))
        .cloned()
        .collect();
    if st.ended_at.is_none() {
        agents.extend(st.foreground.values().cloned());
    }
    for a in &mut agents {
        if a.id.is_empty() {
            continue;
        }
        let file = subagents.join(format!("agent-{}.jsonl", a.id));
        a.last_activity_at = std::fs::metadata(&file)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64);
        a.activity = last_activity(&file);
        if a.agent_type.is_none() {
            a.agent_type = agent_type(&subagents.join(format!("agent-{}.meta.json", a.id)));
        }
    }
    agents.sort_by_key(|a| a.started_at.unwrap_or(0));
    Main {
        ended_at: st.ended_at,
        background: Background {
            agents,
            shells: st.shells.values().filter(|at| alive(**at)).count() as u32,
        },
    }
}

fn scan(path: &Path, st: &mut State, start: u64, size: u64, aligned: bool) -> Option<()> {
    let mut file = File::open(path).ok()?;
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::with_capacity((size - start) as usize);
    file.take(size - start).read_to_end(&mut buf).ok()?;
    let whole = buf.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
    let text = String::from_utf8_lossy(&buf[..whole]);
    let body = if aligned {
        &text[..]
    } else {
        text.find('\n').map_or("", |i| &text[i + 1..])
    };
    for line in body.lines() {
        apply(st, line);
    }
    st.offset = start + whole as u64;
    Some(())
}

fn ms(v: &Value) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(v["timestamp"].as_str()?)
        .ok()
        .map(|t| t.timestamp_millis())
}

fn text(v: &Value) -> Option<String> {
    v.as_str().map(str::to_string)
}

/// One transcript line.
fn apply(st: &mut State, line: &str) {
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return;
    };
    if v["isSidechain"] == true {
        return;
    }
    if line.contains("<task-notification>") {
        for id in notified(line) {
            finish(st, &id);
        }
    }
    let at = ms(&v);
    match v["type"].as_str() {
        Some("system") if v["subtype"] == "turn_duration" => {
            st.seen_turn = true;
            st.ended_at = at.or(Some(0));
            st.foreground.clear();
        }
        Some("assistant") => {
            st.seen_turn = true;
            st.ended_at = None;
            for b in v["message"]["content"].as_array().into_iter().flatten() {
                if b["type"] == "tool_use" {
                    tool_use(st, b, at);
                }
            }
        }
        Some("user") if v["isMeta"] != true => {
            st.seen_turn = true;
            st.ended_at = None;
            for b in v["message"]["content"].as_array().into_iter().flatten() {
                if b["type"] == "tool_result"
                    && let Some(id) = b["tool_use_id"].as_str()
                {
                    let fg = st.foreground.remove(id);
                    launched(st, &v["toolUseResult"], fg, at);
                }
            }
        }
        _ => {}
    }
}

fn tool_use(st: &mut State, b: &Value, at: Option<i64>) {
    let input = &b["input"];
    match b["name"].as_str() {
        Some("Agent" | "Task") => {
            if let Some(id) = b["id"].as_str() {
                st.foreground.insert(
                    id.to_string(),
                    Agent {
                        description: text(&input["description"]),
                        agent_type: text(&input["subagent_type"]),
                        model: text(&input["model"]),
                        started_at: at,
                        ..Default::default()
                    },
                );
            }
        }
        Some("SendMessage") => {
            if let Some(to) = input["to"].as_str()
                && let Some(mut a) = st.done.remove(to)
            {
                a.started_at = at;
                st.agents.insert(to.to_string(), a);
            }
        }
        Some("TaskStop" | "KillShell" | "KillBash") => {
            for key in ["task_id", "shell_id", "id"] {
                if let Some(id) = input[key].as_str() {
                    finish(st, id);
                }
            }
        }
        _ => {}
    }
}

/// A tool result: a background agent or shell started.
fn launched(st: &mut State, r: &Value, fg: Option<Agent>, at: Option<i64>) {
    if r["status"] == "async_launched" {
        if let Some(id) = r["agentId"].as_str() {
            let base = fg.unwrap_or_default();
            let a = Agent {
                id: id.to_string(),
                description: text(&r["description"]).or(base.description),
                agent_type: base.agent_type,
                model: text(&r["resolvedModel"]).or(base.model),
                background: true,
                started_at: at.or(base.started_at),
                ..Default::default()
            };
            st.done.remove(id);
            st.agents.insert(id.to_string(), a);
        }
    } else if let Some(id) = r["backgroundTaskId"].as_str() {
        st.shells.insert(id.to_string(), at);
    }
}

fn finish(st: &mut State, id: &str) {
    st.shells.remove(id);
    if let Some(a) = st.agents.remove(id) {
        st.done.insert(id.to_string(), a);
        while st.done.len() > MAX_DONE {
            st.done.pop_first();
        }
    }
}

/// The task ids a line's `<task-notification>`s report as stopped.
fn notified(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = line;
    while let Some(i) = rest.find("<task-notification>") {
        rest = &rest[i + "<task-notification>".len()..];
        let block = rest.split("</task-notification>").next().unwrap_or(rest);
        if let Some(id) = between(block, "<task-id>", "</task-id>") {
            out.push(id.to_string());
        }
    }
    out
}

fn between<'a>(s: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let i = s.find(open)? + open.len();
    let j = s[i..].find(close)? + i;
    Some(s[i..j].trim())
}

fn agent_type(meta: &Path) -> Option<String> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(meta).ok()?).ok()?;
    text(&v["agentType"])
}

/// A subagent's last tool call, from its transcript's tail: `Bash: run the tests`.
fn last_activity(file: &Path) -> Option<String> {
    let mut f = File::open(file).ok()?;
    let size = f.metadata().ok()?.len();
    let start = size.saturating_sub(AGENT_TAIL);
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    text.lines().rev().find_map(|line| {
        if !line.contains("\"tool_use\"") {
            return None;
        }
        let v: Value = serde_json::from_str(line).ok()?;
        let b = v["message"]["content"]
            .as_array()?
            .iter()
            .rev()
            .find(|b| b["type"] == "tool_use")?;
        let name = b["name"].as_str()?;
        let i = &b["input"];
        let what = [
            "description",
            "command",
            "pattern",
            "file_path",
            "url",
            "query",
            "prompt",
        ]
        .iter()
        .find_map(|k| i[*k].as_str())
        .map(|s| s.lines().next().unwrap_or("").trim().to_string());
        let mut out = match what {
            Some(w) if !w.is_empty() => format!("{name}: {w}"),
            _ => name.to_string(),
        };
        if out.chars().count() > 100 {
            out = out.chars().take(99).collect::<String>() + "…";
        }
        Some(out)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;

    const T0: &str = "2026-10-27T11:00:00.000Z";
    const T1: &str = "2026-10-27T11:01:00.000Z";
    const T2: &str = "2026-10-27T11:02:00.000Z";
    const T3: &str = "2026-10-27T11:03:00.000Z";
    const T4: &str = "2026-10-27T11:04:00.000Z";

    fn at(t: &str) -> i64 {
        chrono::DateTime::parse_from_rfc3339(t)
            .unwrap()
            .timestamp_millis()
    }
    fn prompt(t: &str) -> Value {
        json!({"type":"user","timestamp":t,"message":{"role":"user","content":"do it"}})
    }
    fn agent_call(t: &str, id: &str, desc: &str, bg: bool) -> Value {
        json!({"type":"assistant","timestamp":t,"message":{"role":"assistant","content":[
            {"type":"tool_use","id":id,"name":"Agent","input":{"description":desc,"subagent_type":"general-purpose","prompt":"…","run_in_background":bg}}]}})
    }
    fn result(t: &str, id: &str, r: Value) -> Value {
        json!({"type":"user","timestamp":t,"message":{"role":"user","content":[
            {"type":"tool_result","tool_use_id":id,"content":"…"}]},"toolUseResult":r})
    }
    fn end(t: &str) -> Value {
        json!({"type":"system","subtype":"turn_duration","timestamp":t})
    }
    fn notification(t: &str, task: &str) -> Value {
        json!({"type":"queue-operation","operation":"enqueue","timestamp":t,
            "content":format!("<task-notification>\n<task-id>{task}</task-id>\n<status>completed</status>\n</task-notification>")})
    }

    fn write(dir: &Path, sid: &str, lines: &[Value]) {
        let p = dir.join(format!("{sid}.jsonl"));
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(p)
            .unwrap();
        for l in lines {
            writeln!(f, "{l}").unwrap();
        }
    }

    /// A Claude home with one project dir; returns (home, cwd).
    fn home() -> (tempfile::TempDir, String) {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = "/work/repo".to_string();
        std::fs::create_dir_all(
            tmp.path()
                .join("projects")
                .join(crate::core::discovery::encode_cwd(&cwd)),
        )
        .unwrap();
        (tmp, cwd)
    }

    fn proj(h: &Path, cwd: &str) -> PathBuf {
        h.join("projects")
            .join(crate::core::discovery::encode_cwd(cwd))
    }

    #[test]
    fn a_background_agent_leaves_the_main_thread_free_until_its_notification() {
        let (h, cwd) = home();
        let dir = proj(h.path(), &cwd);
        let sid = "s-bg";
        write(
            &dir,
            sid,
            &[
                prompt(T0),
                agent_call(T0, "tu1", "analysis", true),
                result(
                    T1,
                    "tu1",
                    json!({"isAsync":true,"status":"async_launched","agentId":"a1","description":"Terra analysis","resolvedModel":"claude-opus-5-5"}),
                ),
                end(T1),
            ],
        );
        // Its transcript, with a last tool call.
        let sub = dir.join(sid).join("subagents");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(
            sub.join("agent-a1.jsonl"),
            format!(
                "{}\n",
                json!({"type":"assistant","isSidechain":true,"message":{"content":[
                    {"type":"tool_use","name":"Bash","input":{"command":"tb-backyard traces --since 1h\nmore","description":"Fetch the traces"}}]}})
            ),
        )
        .unwrap();

        let m = for_session(h.path(), Some(&cwd), sid, Some(at(T0))).unwrap();
        assert_eq!(m.ended_at, Some(at(T1)));
        assert_eq!(m.background.agents.len(), 1);
        let a = &m.background.agents[0];
        assert_eq!(a.id, "a1");
        assert_eq!(a.description.as_deref(), Some("Terra analysis"));
        assert_eq!(a.agent_type.as_deref(), Some("general-purpose"));
        assert_eq!(a.model.as_deref(), Some("claude-opus-5-5"));
        assert!(a.background);
        assert_eq!(a.activity.as_deref(), Some("Bash: Fetch the traces"));
        assert!(a.last_activity_at.is_some());

        // Appended: the notification (a queue operation) ends it.
        write(&dir, sid, &[notification(T2, "a1")]);
        let m = for_session(h.path(), Some(&cwd), sid, Some(at(T0))).unwrap();
        assert!(m.background.is_empty());
        assert_eq!(m.ended_at, Some(at(T1)));

        // SendMessage starts it again; the turn that sent it is in progress, then ends.
        write(
            &dir,
            sid,
            &[
                json!({"type":"user","timestamp":T3,"message":{"content":"<task-notification><task-id>a1</task-id></task-notification>"}}),
                json!({"type":"assistant","timestamp":T3,"message":{"content":[{"type":"tool_use","id":"tu2","name":"SendMessage","input":{"to":"a1","message":"go on"}}]}}),
            ],
        );
        let m = for_session(h.path(), Some(&cwd), sid, Some(at(T0))).unwrap();
        assert_eq!(m.ended_at, None);
        assert_eq!(m.background.agents.len(), 1);
        write(&dir, sid, &[end(T4)]);
        let m = for_session(h.path(), Some(&cwd), sid, Some(at(T0))).unwrap();
        assert_eq!(m.ended_at, Some(at(T4)));
        assert_eq!(m.background.agents[0].started_at, Some(at(T3)));
    }

    #[test]
    fn foreground_agents_and_shells() {
        let (h, cwd) = home();
        let dir = proj(h.path(), &cwd);
        let sid = "s-fg";
        write(
            &dir,
            sid,
            &[
                prompt(T0),
                json!({"type":"assistant","timestamp":T0,"message":{"content":[{"type":"tool_use","id":"b1","name":"Bash","input":{"command":"ci-wait","run_in_background":true}}]}}),
                result(T0, "b1", json!({"backgroundTaskId":"bt1"})),
                agent_call(T1, "tu9", "explore the code", false),
            ],
        );
        let m = for_session(h.path(), Some(&cwd), sid, Some(at(T0))).unwrap();
        assert_eq!(m.ended_at, None, "the turn waits on the foreground agent");
        assert_eq!(m.background.shells, 1);
        assert_eq!(m.background.agents.len(), 1);
        assert!(!m.background.agents[0].background);
        assert_eq!(
            m.background.agents[0].description.as_deref(),
            Some("explore the code")
        );

        write(
            &dir,
            sid,
            &[result(T2, "tu9", json!({"status":"completed"})), end(T2)],
        );
        let m = for_session(h.path(), Some(&cwd), sid, Some(at(T0))).unwrap();
        assert_eq!(m.ended_at, Some(at(T2)));
        assert!(m.background.agents.is_empty());
        assert_eq!(m.background.shells, 1);

        // TaskStop stops the shell.
        write(
            &dir,
            sid,
            &[
                prompt(T3),
                json!({"type":"assistant","timestamp":T3,"message":{"content":[{"type":"tool_use","id":"k","name":"TaskStop","input":{"task_id":"bt1"}}]}}),
                end(T4),
            ],
        );
        let m = for_session(h.path(), Some(&cwd), sid, Some(at(T0))).unwrap();
        assert_eq!(m.background.shells, 0);
    }

    #[test]
    fn launched_before_the_process_started_is_not_running() {
        let (h, cwd) = home();
        let dir = proj(h.path(), &cwd);
        write(
            &dir,
            "s-old",
            &[
                prompt(T0),
                agent_call(T0, "tu1", "old", true),
                result(T0, "tu1", json!({"status":"async_launched","agentId":"a9"})),
                end(T1),
            ],
        );
        let m = for_session(h.path(), Some(&cwd), "s-old", Some(at(T2))).unwrap();
        assert!(m.background.is_empty());
    }

    #[test]
    fn nothing_recognised_is_none_and_sidechains_do_not_count() {
        let (h, cwd) = home();
        let dir = proj(h.path(), &cwd);
        write(&dir, "s-none", &[json!({"type":"summary","summary":"x"})]);
        assert_eq!(for_session(h.path(), Some(&cwd), "s-none", None), None);
        write(
            &dir,
            "s-side",
            &[
                prompt(T0),
                end(T1),
                json!({"type":"user","isSidechain":true,"timestamp":T2,"message":{"content":"sub"}}),
            ],
        );
        let m = for_session(h.path(), Some(&cwd), "s-side", None).unwrap();
        assert_eq!(m.ended_at, Some(at(T1)));
    }

    #[test]
    fn notified_reads_every_task_id() {
        let line = r#"{"content":"<task-notification>\n<task-id>a1</task-id>\n</task-notification> and <task-notification><task-id> b2 </task-id></task-notification>"}"#;
        assert_eq!(notified(line), vec!["a1", "b2"]);
    }
}
