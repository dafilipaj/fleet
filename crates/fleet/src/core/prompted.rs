//! When a Claude session was last *prompted*: the timestamp of the last main-thread
//! user entry its transcript records as typed by a human (`origin.kind: "human"`;
//! `fleet send` types into the terminal, so it counts too). Tool results, task
//! notifications, meta entries and compact summaries do not.
//!
//! This is `prompted_at` in `fleet list --json` — a calmer ordering key than
//! `updated_at`, which moves on every status change (each turn start and end).
//!
//! **Cost.** The first read scans the transcript's tail (256 KiB, widened to 2 MiB
//! and 8 MiB until a prompt is found); later reads scan only the bytes appended
//! since, so a busy session's growing transcript is never re-read.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

const TAILS: [u64; 3] = [256 * 1024, 2 * 1024 * 1024, 8 * 1024 * 1024];

/// (bytes scanned, up to the last full line; last prompt in ms).
type Scanned = (u64, Option<i64>);
static CACHE: LazyLock<Mutex<HashMap<PathBuf, Scanned>>> = LazyLock::new(Default::default);

/// `prompted_at` for a live Claude session, or None when its transcript holds no prompt.
pub fn for_session(home: &Path, cwd: Option<&str>, session_id: &str) -> Option<i64> {
    let path = super::context::locate_transcript(home, cwd, session_id)?;
    let size = std::fs::metadata(&path).ok()?.len();
    let cached = CACHE.lock().ok().and_then(|c| c.get(&path).copied());
    let (scanned, found) = match cached {
        Some((done, at)) if done == size => return at,
        // Appended since: scan only the new lines, keep the old prompt if none is new.
        Some((done, at)) if done < size && size - done <= TAILS[2] => {
            let (end, new) = scan(&path, done, size, true)?;
            (end, new.or(at))
        }
        _ => from_tail(&path, size)?,
    };
    if let Ok(mut c) = CACHE.lock() {
        c.insert(path, (scanned, found));
    }
    found
}

/// The last prompt in the transcript's tail, widening it until one is found.
fn from_tail(path: &Path, size: u64) -> Option<Scanned> {
    let mut end = size;
    for tail in TAILS {
        let start = size.saturating_sub(tail);
        let (e, found) = scan(path, start, size, start == 0)?;
        end = e;
        if found.is_some() || start == 0 {
            return Some((end, found));
        }
    }
    Some((end, None))
}

/// Scan `[start, size)`: (offset just past its last complete line, last prompt in it).
/// Unless `aligned` (a file start, or where a previous scan ended), `start` is
/// assumed to cut a line, which is skipped.
fn scan(path: &Path, start: u64, size: u64, aligned: bool) -> Option<Scanned> {
    let mut file = File::open(path).ok()?;
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::with_capacity((size - start) as usize);
    file.take(size - start).read_to_end(&mut buf).ok()?;
    // Only whole lines: a line still being written is picked up next time.
    let whole = buf.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
    let text = String::from_utf8_lossy(&buf[..whole]);
    let body = if !aligned {
        text.find('\n').map_or("", |i| &text[i + 1..])
    } else {
        &text[..]
    };
    Some((start + whole as u64, last_prompt(body)))
}

/// The timestamp (ms) of the last human prompt in `text`, newest first.
fn last_prompt(text: &str) -> Option<i64> {
    text.lines().rev().find_map(|line| {
        // Cheap pre-filter: most lines are assistant entries or tool results.
        if !line.contains("\"user\"") || line.contains("\"tool_result\"") {
            return None;
        }
        let v: serde_json::Value = serde_json::from_str(line).ok()?;
        if v["type"] != "user" || v["isSidechain"] == true || v["isMeta"] == true {
            return None;
        }
        // Older transcripts carry no origin: anything but a compact summary was typed.
        let human = match v["origin"]["kind"].as_str() {
            Some(kind) => kind == "human",
            None => v["isCompactSummary"] != true && v["origin"].is_null(),
        };
        if !human {
            return None;
        }
        chrono::DateTime::parse_from_rfc3339(v["timestamp"].as_str()?)
            .ok()
            .map(|t| t.timestamp_millis())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn user(text: &str, at: &str, origin: Option<&str>) -> String {
        let mut v = serde_json::json!({
            "type": "user", "timestamp": at, "isSidechain": false,
            "message": {"role": "user", "content": text}
        });
        if let Some(kind) = origin {
            v["origin"] = serde_json::json!({ "kind": kind });
        }
        v.to_string()
    }

    fn tool_result(at: &str) -> String {
        serde_json::json!({
            "type": "user", "timestamp": at,
            "message": {"role": "user", "content": [{"type": "tool_result", "content": "ok"}]}
        })
        .to_string()
    }

    fn assistant(at: &str) -> String {
        serde_json::json!({"type": "assistant", "timestamp": at, "message": {"role": "assistant"}})
            .to_string()
    }

    const T1: &str = "2026-10-25T10:00:00.000Z";
    const T2: &str = "2026-10-25T10:05:00.000Z";
    const T3: &str = "2026-10-25T10:09:00.000Z";

    fn ms(at: &str) -> i64 {
        chrono::DateTime::parse_from_rfc3339(at)
            .unwrap()
            .timestamp_millis()
    }

    #[test]
    fn only_human_prompts_count() {
        let notif = user("<task-notification>", T3, Some("task-notification"));
        let text = [
            user("first", T1, Some("human")),
            assistant(T2),
            tool_result(T3),
            notif,
        ]
        .join("\n");
        assert_eq!(last_prompt(&text), Some(ms(T1)));
        // No origin (older Claude Code): a plain user entry still counts.
        assert_eq!(last_prompt(&user("old", T2, None)), Some(ms(T2)));
        assert_eq!(last_prompt(&assistant(T1)), None);
    }

    #[test]
    fn appended_prompts_are_picked_up_incrementally() {
        let dir = tempfile::tempdir().unwrap();
        let sid = "abc";
        let cwd = "/tmp/proj";
        let project = dir
            .path()
            .join("projects")
            .join(crate::core::discovery::encode_cwd(cwd));
        std::fs::create_dir_all(&project).unwrap();
        let path = project.join(format!("{sid}.jsonl"));
        let mut f = File::create(&path).unwrap();
        writeln!(f, "{}", user("one", T1, Some("human"))).unwrap();
        writeln!(f, "{}", assistant(T1)).unwrap();
        assert_eq!(for_session(dir.path(), Some(cwd), sid), Some(ms(T1)));

        // The agent keeps working: no new prompt, the old one stands.
        writeln!(f, "{}", tool_result(T2)).unwrap();
        assert_eq!(for_session(dir.path(), Some(cwd), sid), Some(ms(T1)));

        // A half-written prompt is not read until its line is complete.
        let next = user("two", T3, Some("human"));
        write!(f, "{}", &next[..10]).unwrap();
        f.flush().unwrap();
        assert_eq!(for_session(dir.path(), Some(cwd), sid), Some(ms(T1)));
        writeln!(f, "{}", &next[10..]).unwrap();
        assert_eq!(for_session(dir.path(), Some(cwd), sid), Some(ms(T3)));
    }
}
