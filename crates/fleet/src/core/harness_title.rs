//! The title a harness gives its own session: step 2 of
//! [`crate::core::title::display_title`], after a name someone chose and before Fleet's
//! generated title.
//!
//! Claude Code titles each session itself. After the first exchange, and again as the work
//! moves on, it appends `{"type":"ai-title","aiTitle":"…"}` to the transcript, and it
//! re-appends the record on resume, compaction and exit, so the last one wins. A record
//! whose `aiTitle` isn't a non-blank string is skipped. Codex's own titles arrive as its
//! thread names ([`crate::core::harness::codex_names`]), which count as a chosen name; pi
//! has none.
//!
//! **Cost.** `list --json` is a new process every couple of seconds, and a long transcript
//! runs to tens of MB, so no call reads what an earlier call has read. A cache beside
//! `fleet-names.json` (`~/.claude/fleet-harness-titles.json`) keeps, per transcript, its
//! inode, the bytes read so far (up to the last complete line) and the last title in them.
//! A call reads only what was appended since, and starts over when the file is shorter
//! than that or another file (inode) took its path. A first read scans the tail, widening
//! it until a title turns up, so it costs the distance from the end to the last title,
//! not the transcript's length.
//!
//! **Failures.** An unreadable or missing transcript, a malformed record or an unreadable
//! cache leaves the title out for that session. Nothing here fails its caller.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::core::discovery::{Session, claude_home, is_fixture};
use crate::core::harness::Harness;

/// The tails a first read scans, widest last; past these it reads the whole file.
const TAILS: [u64; 3] = [256 * 1024, 2 * 1024 * 1024, 8 * 1024 * 1024];

/// A line longer than this is a message, not a title record: it is skipped unread.
const MAX_RECORD: usize = 64 * 1024;

/// An entry not used for this long is dropped when the cache is saved.
const KEEP_MS: i64 = 7 * 24 * 3600 * 1000;

/// How stale an entry's `seen` may get before a call that only reads it writes the
/// cache: an unchanged fleet doesn't rewrite the file every two seconds.
const SEEN_EVERY_MS: i64 = 3600 * 1000;

/// What the cache keeps per transcript.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Scanned {
    /// The file it was read from, so a replaced transcript is read again.
    ino: u64,
    /// The bytes read so far, up to just past the last complete line.
    read: u64,
    /// The last title in them.
    title: Option<String>,
    /// When a call last used it (ms since the epoch).
    seen: i64,
}

/// The on-disk cache, keyed by transcript path.
#[derive(Debug, Default)]
pub struct Cache {
    path: PathBuf,
    entries: HashMap<String, Scanned>,
    dirty: bool,
}

fn cache_path() -> PathBuf {
    claude_home().join("fleet-harness-titles.json")
}

/// One in-flight save from the next in the same process (see `naming::NameCache`).
static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

impl Cache {
    pub fn at(path: PathBuf) -> Self {
        let entries = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        Self {
            path,
            entries,
            dirty: false,
        }
    }

    /// The last title in the transcript at `path`, reading only what no earlier call read.
    pub fn title(&mut self, path: &Path, now: i64) -> Option<String> {
        let meta = std::fs::metadata(path).ok()?;
        let (ino, size) = (meta.ino(), meta.len());
        let key = path.to_string_lossy().into_owned();
        let old = self
            .entries
            .get(&key)
            .filter(|c| c.ino == ino && c.read <= size);
        let next = match old {
            Some(c) if c.read == size => {
                if now - c.seen < SEEN_EVERY_MS {
                    return c.title.clone();
                }
                Scanned {
                    seen: now,
                    ..c.clone()
                }
            }
            // Appended since: read only the new lines; the old title stands unless one is new.
            Some(c) => {
                let (read, title) = scan(path, c.read, size, true)?;
                Scanned {
                    ino,
                    read,
                    title: title.or_else(|| c.title.clone()),
                    seen: now,
                }
            }
            // Never read, shorter than what was read, or another file: from the tail.
            None => {
                let (read, title) = from_tail(path, size)?;
                Scanned {
                    ino,
                    read,
                    title,
                    seen: now,
                }
            }
        };
        let title = next.title.clone();
        self.entries.insert(key, next);
        self.dirty = true;
        title
    }

    /// Write the cache when something changed, dropping entries unused for a week. A
    /// failure is ignored: the next call reads again.
    pub fn save(&mut self, now: i64) {
        if !self.dirty {
            return;
        }
        self.entries.retain(|_, c| now - c.seen < KEEP_MS);
        let Ok(text) = serde_json::to_string(&self.entries) else {
            return;
        };
        let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let mut tmp = self.path.clone().into_os_string();
        tmp.push(format!(".tmp{}-{seq}", std::process::id()));
        let tmp = PathBuf::from(tmp);
        if std::fs::write(&tmp, text).is_ok() && std::fs::rename(&tmp, &self.path).is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        self.dirty = false;
    }
}

/// Fill `harness_title` on each live Claude row, from its transcript. Inert under a
/// fixture, like the name cache.
pub fn stamp(rows: &mut [Session]) {
    if is_fixture() {
        return;
    }
    let home = claude_home();
    stamp_with(rows, &home, &mut Cache::at(cache_path()));
}

/// [`stamp`] against `home`'s transcripts and `cache`.
pub fn stamp_with(rows: &mut [Session], home: &Path, cache: &mut Cache) {
    let now = chrono::Utc::now().timestamp_millis();
    for r in rows.iter_mut().filter(|r| r.harness == Harness::Claude) {
        let Some(sid) = r.session_id.as_deref() else {
            continue;
        };
        let Some(path) = crate::core::context::locate_transcript(home, r.cwd.as_deref(), sid)
        else {
            continue;
        };
        r.harness_title = cache.title(&path, now);
    }
    cache.save(now);
}

/// The last title in the transcript's tail, widening it until one is found:
/// (bytes read, up to the last complete line; the title).
fn from_tail(path: &Path, size: u64) -> Option<(u64, Option<String>)> {
    for tail in TAILS {
        let start = size.saturating_sub(tail);
        let (read, title) = scan(path, start, size, start == 0)?;
        if title.is_some() || start == 0 {
            return Some((read, title));
        }
    }
    scan(path, 0, size, true)
}

/// Read `[start, size)`: (offset just past its last complete line, the last title in it).
/// Not `aligned`: `start` may fall inside a line, so the first one is skipped.
fn scan(path: &Path, start: u64, size: u64, aligned: bool) -> Option<(u64, Option<String>)> {
    let mut f = File::open(path).ok()?;
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut lines = BufReader::new(f.take(size - start));
    let mut at = start;
    let mut end = start;
    let mut title = None;
    let mut line = Vec::new();
    let mut first = true;
    loop {
        line.clear();
        let n = lines.read_until(b'\n', &mut line).ok()?;
        if n == 0 || line.last() != Some(&b'\n') {
            // The end, or a line still being written: read again next time.
            break;
        }
        at += n as u64;
        end = at;
        let skip = first && !aligned;
        first = false;
        if skip || line.len() > MAX_RECORD {
            continue;
        }
        if let Some(t) = title_record(&line) {
            title = Some(t);
        }
    }
    Some((end, title))
}

/// The title an `ai-title` record carries, trimmed; `None` for any other line, or a record
/// whose `aiTitle` isn't a non-blank string.
fn title_record(line: &[u8]) -> Option<String> {
    let needle = b"\"ai-title\"";
    if !line.windows(needle.len()).any(|w| w == needle) {
        return None;
    }
    let v: serde_json::Value = serde_json::from_slice(line).ok()?;
    if v.get("type")?.as_str()? != "ai-title" {
        return None;
    }
    let t = v.get("aiTitle")?.as_str()?.trim();
    (!t.is_empty()).then(|| t.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn ai(t: &str) -> String {
        format!("{{\"type\":\"ai-title\",\"aiTitle\":\"{t}\",\"sessionId\":\"s1\"}}\n")
    }

    fn user(text: &str) -> String {
        format!("{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":\"{text}\"}}}}\n")
    }

    fn append(path: &Path, text: &str) {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        f.write_all(text.as_bytes()).unwrap();
    }

    #[test]
    fn the_last_title_record_wins_and_a_malformed_one_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let t = dir.path().join("t.jsonl");
        append(&t, &user("Let's resolve merge conflicts"));
        append(&t, &ai("Login redirect loop fix"));
        append(&t, &user("the ai-title words in a prompt are not a record"));
        append(&t, "{\"type\":\"ai-title\",\"aiTitle\":42}\n");
        append(&t, "{\"type\":\"ai-title\",\"aiTitle\":\"   \"}\n");
        append(&t, "{\"type\":\"ai-title\"\n");
        let mut c = Cache::at(dir.path().join("cache.json"));
        assert_eq!(c.title(&t, 1).as_deref(), Some("Login redirect loop fix"));
        append(&t, &ai("  Ship the login redirect fix  "));
        assert_eq!(
            c.title(&t, 2).as_deref(),
            Some("Ship the login redirect fix")
        );
    }

    #[test]
    fn a_transcript_without_a_title_has_none() {
        let dir = tempfile::tempdir().unwrap();
        let t = dir.path().join("t.jsonl");
        append(&t, &user("first"));
        let mut c = Cache::at(dir.path().join("cache.json"));
        assert_eq!(c.title(&t, 1), None);
        assert_eq!(c.title(&dir.path().join("missing.jsonl"), 1), None);
    }

    #[test]
    fn a_later_call_reads_only_what_was_appended() {
        let dir = tempfile::tempdir().unwrap();
        let t = dir.path().join("t.jsonl");
        let cache = dir.path().join("cache.json");
        append(&t, &ai("First title"));
        append(&t, &user("more"));
        let mut c = Cache::at(cache.clone());
        assert_eq!(c.title(&t, 1).as_deref(), Some("First title"));
        c.save(1);
        // A new process: the cache comes from disk, read up to the file's end.
        let mut c = Cache::at(cache.clone());
        let size = std::fs::metadata(&t).unwrap().len();
        let key = t.to_string_lossy().into_owned();
        assert_eq!(c.entries[&key].read, size);
        assert_eq!(c.title(&t, 2).as_deref(), Some("First title"));
        assert!(
            !c.dirty,
            "an unchanged transcript doesn't rewrite the cache"
        );
        // A line still being written isn't read, nor counted.
        append(&t, "{\"type\":\"ai-title\",\"aiTi");
        assert_eq!(c.title(&t, 3).as_deref(), Some("First title"));
        assert_eq!(c.entries[&key].read, size);
        append(&t, "tle\":\"Second title\"}\n");
        assert_eq!(c.title(&t, 4).as_deref(), Some("Second title"));
        // Appended lines without a title keep the last one.
        append(&t, &user("still working"));
        assert_eq!(c.title(&t, 5).as_deref(), Some("Second title"));
    }

    #[test]
    fn a_truncated_or_replaced_transcript_is_read_again() {
        let dir = tempfile::tempdir().unwrap();
        let t = dir.path().join("t.jsonl");
        append(&t, &user("x".repeat(200).as_str()));
        append(&t, &ai("Old title"));
        let mut c = Cache::at(dir.path().join("cache.json"));
        assert_eq!(c.title(&t, 1).as_deref(), Some("Old title"));
        // Shorter than what was read: from the start.
        std::fs::write(&t, ai("Short")).unwrap();
        assert_eq!(c.title(&t, 2).as_deref(), Some("Short"));
        // Another file at the same path, longer: its inode tells it apart.
        let other = dir.path().join("other.jsonl");
        append(&other, &ai("Short"));
        append(&other, &user(&"y".repeat(400)));
        append(&other, &ai("Replaced"));
        std::fs::rename(&other, &t).unwrap();
        assert_eq!(c.title(&t, 3).as_deref(), Some("Replaced"));
    }

    #[test]
    fn a_first_read_widens_its_tail_until_a_title_turns_up() {
        let dir = tempfile::tempdir().unwrap();
        let t = dir.path().join("t.jsonl");
        append(&t, &ai("Early title"));
        // More than the widest tail of prompts after it: the whole file is read.
        let filler = user(&"z".repeat(1000));
        let mut text = String::new();
        while (text.len() as u64) < TAILS[2] + 1024 {
            text.push_str(&filler);
        }
        append(&t, &text);
        let mut c = Cache::at(dir.path().join("cache.json"));
        assert_eq!(c.title(&t, 1).as_deref(), Some("Early title"));
    }

    #[test]
    fn stamp_fills_live_claude_rows_and_saves_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let cwd = "/work/app";
        let proj = home
            .join("projects")
            .join(crate::core::discovery::encode_cwd(cwd));
        std::fs::create_dir_all(&proj).unwrap();
        append(&proj.join("aaaa-1111.jsonl"), &ai("Fix the login loop"));
        let row = |sid: Option<&str>, harness: Harness| Session {
            pid: 1,
            session_id: sid.map(str::to_string),
            cwd: Some(cwd.into()),
            harness,
            ..Default::default()
        };
        let mut rows = vec![
            row(Some("aaaa-1111"), Harness::Claude),
            row(Some("bbbb-2222"), Harness::Claude),
            row(Some("aaaa-1111"), Harness::Codex),
            row(None, Harness::Claude),
        ];
        let cache = home.join("fleet-harness-titles.json");
        stamp_with(&mut rows, home, &mut Cache::at(cache.clone()));
        let titles: Vec<_> = rows.iter().map(|r| r.harness_title.as_deref()).collect();
        assert_eq!(titles, [Some("Fix the login loop"), None, None, None]);
        assert!(cache.is_file());
        assert_eq!(Cache::at(cache).entries.len(), 1);
    }

    #[test]
    fn an_entry_unused_for_a_week_is_dropped_on_save() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a.jsonl"), dir.path().join("b.jsonl"));
        append(&a, &ai("A"));
        append(&b, &ai("B"));
        let cache = dir.path().join("cache.json");
        let mut c = Cache::at(cache.clone());
        c.title(&a, 0);
        c.title(&b, KEEP_MS);
        c.save(KEEP_MS);
        let kept: Vec<_> = Cache::at(cache).entries.into_keys().collect();
        assert_eq!(kept, [b.to_string_lossy().into_owned()]);
    }
}
