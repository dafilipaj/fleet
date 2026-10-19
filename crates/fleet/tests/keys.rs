//! `fleet keys` against a fake agent on the test's private tmux server: the agent reads its
//! raw tty and logs every read with the time it arrived, so the test sees exactly what was
//! pressed, and when.

mod common;

use common::fake::FakeFleet;
use common::{stderr, stdout};

const SID: &str = "aaaaaaaa-1111-2222-3333-444444444444";

/// A fake agent logging each read as `<unix seconds> <hex bytes>`; `(fleet, pid, log path)`.
/// It draws Claude Code's empty input box first (testdata/screens/live, a live capture of a
/// 110x34 pane): `send` types only into one.
fn recorder() -> Option<(FakeFleet, i64, std::path::PathBuf)> {
    let f = FakeFleet::new()?;
    let rec = f.env.path("received.log");
    let empty_box = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata/screens/live/claude-empty.txt");
    let script = format!(
        "cat '{}'; stty raw -echo; exec perl -MTime::HiRes=time -e '\
         open(my $f, \">\", $ARGV[0]) or die; select((select($f), $| = 1)[0]); \
         while (sysread(STDIN, my $b, 64)) {{ printf $f \"%.4f %s\\n\", time, unpack(\"H*\", $b) }}' \
         '{}'",
        empty_box.display(),
        rec.display()
    );
    let pid = f.agent("keys", SID, 110, 34, &script);
    // The log appears once `stty raw` has run and perl owns the tty.
    f.wait_for("the recorder to start", || rec.exists());
    Some((f, pid, rec))
}

/// Each read: (arrival time, bytes).
fn reads(rec: &std::path::Path) -> Vec<(f64, Vec<u8>)> {
    std::fs::read_to_string(rec)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let (t, hex) = l.split_once(' ')?;
            let bytes = (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
                .collect();
            Some((t.parse().ok()?, bytes))
        })
        .collect()
}

fn received(rec: &std::path::Path) -> Vec<u8> {
    reads(rec).into_iter().flat_map(|(_, b)| b).collect()
}

#[test]
fn keys_reach_the_terminal_as_the_terminal_would_send_them() {
    let Some((f, pid, rec)) = recorder() else {
        return;
    };
    let target = format!("pid:{pid}");
    let out = f.fleet(&[
        "keys", &target, "Escape", "Up", "Down", "Right", "Left", "Tab", "BTab", "Space", "C-c",
        "Enter", "--json",
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["ok"], true, "{v}");
    assert_eq!(v["target"]["pid"], pid);
    assert_eq!(v["target"]["session_id"], SID);
    assert_eq!(
        v["keys"],
        serde_json::json!([
            "Escape", "Up", "Down", "Right", "Left", "Tab", "BTab", "Space", "C-c", "Enter"
        ])
    );
    let want: &[u8] = b"\x1b\x1b[A\x1b[B\x1b[C\x1b[D\t\x1b[Z \x03\r";
    f.wait_for("every key to arrive", || received(&rec).len() >= want.len());
    assert_eq!(received(&rec), want);

    // Text output, by the exact session id this time.
    let out = f.fleet(&["keys", &format!("id:{SID}"), "Escape"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("(tmux): Escape"), "{}", stdout(&out));
    f.wait_for("the second Escape", || received(&rec).len() > want.len());
}

// One burst would let a TUI read `Escape` + `Up` as Alt-Up: each key arrives on its own,
// at least most of the 80 ms gap after the one before.
#[test]
fn keys_arrive_one_at_a_time_with_a_gap() {
    let Some((f, pid, rec)) = recorder() else {
        return;
    };
    let out = f.fleet(&["keys", &format!("pid:{pid}"), "Escape", "Up", "Enter"]);
    assert!(out.status.success(), "{}", stderr(&out));
    f.wait_for("three keys", || received(&rec).len() >= 5);
    let r = reads(&rec);
    let keys: Vec<&[u8]> = r.iter().map(|(_, b)| b.as_slice()).collect();
    assert_eq!(
        keys,
        [&b"\x1b"[..], b"\x1b[A", b"\r"],
        "one read per key: {r:?}"
    );
    for pair in r.windows(2) {
        let gap = pair[1].0 - pair[0].0;
        assert!(gap >= 0.06, "keys {gap:.3}s apart: {r:?}");
    }
}

// A failure stops the call where it happens: the key before it stays pressed (it cannot be
// taken back), the keys after it are never sent, and the error says how far it got.
#[test]
fn a_failing_key_stops_the_rest() {
    let Some((f, pid, rec)) = recorder() else {
        return;
    };
    // A tmux that fails its second `send-keys` (everything else goes through).
    let count = f.env.path("send-keys.count");
    let flaky = f.env.path("flaky-tmux.sh");
    std::fs::write(
        &flaky,
        format!(
            "#!/bin/sh\nfor a in \"$@\"; do\n  if [ \"$a\" = send-keys ]; then\n    \
             n=$(( $(cat '{c}' 2>/dev/null || echo 0) + 1 )); echo $n > '{c}'\n    \
             [ $n -eq 2 ] && {{ echo 'injected failure' >&2; exit 1; }}\n  fi\ndone\n\
             exec '{w}' \"$@\"\n",
            c = count.display(),
            w = f.env.tmux_wrapper().display()
        ),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&flaky, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let out = f
        .env
        .cmd()
        .env("FLEET_TMUX", &flaky)
        .args([
            "keys",
            &format!("pid:{pid}"),
            "Escape",
            "Up",
            "Down",
            "--json",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["kind"], "backend_failed", "{v}");
    let err = v["error"].as_str().unwrap();
    assert!(err.contains("injected failure"), "{err}");
    assert!(err.contains("1 of 3 keys pressed before it"), "{err}");
    std::thread::sleep(std::time::Duration::from_millis(400));
    assert_eq!(received(&rec), b"\x1b", "only the key before the failure");
    assert_eq!(std::fs::read_to_string(&count).unwrap().trim(), "2");
}

#[test]
fn a_key_outside_the_allowlist_sends_nothing() {
    let Some((f, pid, rec)) = recorder() else {
        return;
    };
    let target = format!("pid:{pid}");
    // `Escape` is valid, `C-d` is not: the whole call is refused before anything is typed.
    let out = f.fleet(&["keys", &target, "Escape", "C-d", "--json"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["ok"], false);
    assert_eq!(v["kind"], "invalid_key");
    assert!(v["error"].as_str().unwrap().contains("C-d"), "{v}");
    let out = f.fleet(&["keys", &target, "escape"]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "key names are tmux's, case and all"
    );
    assert!(stderr(&out).contains("unknown key"), "{}", stderr(&out));
    // A target that is not there, exactly.
    let out = f.fleet(&[
        "keys",
        &format!("pid:{}", pid + 100_000),
        "Escape",
        "--json",
    ]);
    assert_eq!(out.status.code(), Some(1));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["kind"], "target_not_found", "{v}");
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(received(&rec).is_empty(), "nothing was typed");
}

#[test]
fn keys_need_at_least_one_key() {
    let out = common::Env::new()
        .cmd()
        .args(["keys", "pid:1"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2), "clap usage error");
}

// `fleet send --json` shares `keys`'s object: the target, and the host it ran on.
#[test]
fn send_json_says_where_the_text_went() {
    let Some((f, pid, rec)) = recorder() else {
        return;
    };
    let out = f.fleet(&["send", "--json", &format!("pid:{pid}"), "hi"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(|k| k.as_str()).collect();
    keys.sort();
    assert_eq!(keys, ["host", "ok", "target"], "{v}");
    assert_eq!(v["ok"], true, "{v}");
    assert!(v["host"].as_str().is_some_and(|h| !h.is_empty()), "{v}");
    assert_eq!(v["target"]["pid"], pid);
    assert_eq!(v["target"]["session_id"], SID);
    f.wait_for("the text and its Enter", || received(&rec).ends_with(b"\r"));
    assert_eq!(received(&rec), b"hi\r");
}
