//! `fleet peek --json` against a fake agent on a private tmux server: an 80×24 pane showing
//! known content — a coloured line and one longer than the pane is wide.

mod common;

use common::fake::FakeFleet;
use common::{stderr, stdout};

const SID: &str = "cccccccc-1111-2222-3333-444444444444";

fn long_line() -> String {
    format!("L{}", "0123456789".repeat(15))
}

/// A fake agent that prints its screen and then sits there; returns `(fleet, pid)`.
fn painted() -> Option<(FakeFleet, i64)> {
    let f = FakeFleet::new()?;
    let script = format!(
        "printf 'first line\\n'; printf '\\033[31mred text\\033[0m\\n'; printf '%s\\n' '{}'; \
         printf 'last line\\n'; exec sleep 300",
        long_line()
    );
    let pid = f.agent("peek", SID, 80, 24, &script);
    f.wait_for("the screen to be painted", || {
        let out = f.tmux(&["capture-pane", "-p", "-t", "fleet-test-peek"]);
        stdout(&out).contains("last line")
    });
    Some((f, pid))
}

#[test]
fn peek_json_is_the_screen_whole() {
    let Some((f, pid)) = painted() else { return };
    let target = format!("pid:{pid}");
    let out = f.fleet(&["peek", &target, "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["ok"], true, "{v}");
    assert_eq!(v["target"]["pid"], pid);
    assert_eq!(v["target"]["session_id"], SID);
    assert_eq!(v["backend"], "tmux");
    assert_eq!(v["status"], "idle");
    assert_eq!(v["cols"], 80);
    assert_eq!(v["rows"], 24);
    // No header, nothing clipped, the wrapped line joined back, no trailing blank rows,
    // no escape sequences.
    assert_eq!(
        v["lines"],
        serde_json::json!(["first line", "red text", long_line(), "last line"]),
        "{v}"
    );

    // --lines counts from the bottom of what is there.
    let out = f.fleet(&["peek", &format!("id:{SID}"), "--json", "--lines", "2"]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["lines"], serde_json::json!([long_line(), "last line"]));

    // --ansi keeps the colour.
    let out = f.fleet(&["peek", &target, "--json", "--ansi"]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let red = v["lines"][1].as_str().unwrap();
    assert!(
        red.contains("\u{1b}[31m") && red.contains("red text"),
        "{red:?}"
    );
    // tmux re-states the colour where the line starts; the text is still whole.
    let long = v["lines"][2].as_str().unwrap();
    assert!(long.ends_with(&long_line()), "{long:?}");
}

#[test]
fn peek_text_output_is_unchanged() {
    let Some((f, pid)) = painted() else { return };
    let out = f.fleet(&["peek", &format!("pid:{pid}")]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    // The header line, then the screen cut to the terminal width (80 when not a tty).
    assert!(text.contains("▼ peek (tmux, idle)"), "{text}");
    assert!(text.contains("first line"), "{text}");
    assert!(!text.contains(&long_line()), "text peek clips: {text}");
}

#[test]
fn peek_json_reports_a_missing_target() {
    let Some((f, pid)) = painted() else { return };
    let out = f.fleet(&["peek", &format!("pid:{}", pid + 100_000), "--json"]);
    assert_eq!(out.status.code(), Some(1));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["ok"], false);
    assert_eq!(v["kind"], "target_not_found");
    assert!(
        v["error"].as_str().unwrap().contains("no live session"),
        "{v}"
    );
    // --ansi only means something with --json.
    let out = f.fleet(&["peek", &format!("pid:{pid}"), "--ansi"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
}

fn huge_line() -> String {
    format!("H{}", "0123456789".repeat(400))
}

// A 4,001-character line on an 80×24 pane fills 51 rows, so its start is far up in the
// scrollback: `--lines N` must still return it whole, however few rows N alone would reach.
#[test]
fn a_logical_line_longer_than_the_capture_comes_back_whole() {
    let Some(f) = FakeFleet::new() else { return };
    let print = "perl -e 'print \"before\\n\", \"H\", \"0123456789\" x 400, \"\\n\"'";
    let mid = f.agent(
        "mid",
        "dddddddd-1111-2222-3333-444444444444",
        80,
        24,
        &format!("{print}; printf 'after\\n'; exec sleep 300"),
    );
    let last = f.agent(
        "last",
        "eeeeeeee-1111-2222-3333-444444444444",
        80,
        24,
        &format!("{print}; exec sleep 300"),
    );
    for name in ["fleet-test-mid", "fleet-test-last"] {
        f.wait_for("the screen to be painted", || {
            let out = f.tmux(&["capture-pane", "-p", "-t", name]);
            stdout(&out).contains("0123456789")
        });
    }
    std::thread::sleep(std::time::Duration::from_millis(200));
    let lines = |pid: i64, n: &str| {
        let out = f.fleet(&["peek", &format!("pid:{pid}"), "--json", "--lines", n]);
        assert!(out.status.success(), "{}", stderr(&out));
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        v["lines"].clone()
    };
    assert_eq!(lines(last, "1"), serde_json::json!([huge_line()]));
    assert_eq!(lines(mid, "2"), serde_json::json!([huge_line(), "after"]));
    assert_eq!(
        lines(mid, "3"),
        serde_json::json!(["before", huge_line(), "after"])
    );
    // More than there is: everything, nothing invented.
    assert_eq!(
        lines(mid, "40"),
        serde_json::json!(["before", huge_line(), "after"])
    );
    let out = f.fleet(&["peek", &format!("pid:{mid}"), "--json"]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["truncated"], false, "{v}");
}

// A line longer than the capture cap (2,000 rows; here 200,001 characters, 2,501 rows on an
// 80-column pane) is not chased through the whole scrollback on every poll: what fits comes
// back, and `truncated` says its start is missing.
#[test]
fn the_capture_stops_at_its_cap_and_says_so() {
    let Some(f) = FakeFleet::new() else { return };
    let _first = f.agent(
        "first",
        "ffffffff-1111-2222-3333-444444444444",
        80,
        24,
        "exec sleep 300",
    );
    // Only panes created after this keep that much scrollback.
    f.tmux(&["set-option", "-g", "history-limit", "10000"]);
    let pid = f.agent(
        "giant",
        "abababab-1111-2222-3333-444444444444",
        80,
        24,
        "perl -e 'print \"G\", \"9\" x 200000, \"\\n\"'; printf 'after\\n'; exec sleep 300",
    );
    f.wait_for("the screen to be painted", || {
        let out = f.tmux(&["capture-pane", "-p", "-t", "fleet-test-giant"]);
        stdout(&out).contains("after")
    });
    let out = f.fleet(&["peek", &format!("pid:{pid}"), "--json", "--lines", "2"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["truncated"], true);
    assert_eq!(v["lines"][1], "after");
    let cut = v["lines"][0].as_str().unwrap();
    assert!(
        !cut.starts_with('G') && cut.chars().all(|c| c == '9'),
        "{}",
        &cut[..40]
    );
    assert!(cut.len() >= 150_000 && cut.len() < 200_000, "{}", cut.len());
}
