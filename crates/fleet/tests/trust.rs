//! The trust contract (C1–C9), with fake tmux responses and no registered Sessions.
mod common;

use common::{Env, stderr, stdout};
use serde_json::Value;

fn run(env: &Env, args: &[&str]) -> (i32, Value) {
    run_with(env, &[], args)
}

fn run_with(env: &Env, vars: &[(&str, &str)], args: &[&str]) -> (i32, Value) {
    let mut cmd = env.cmd();
    cmd.envs(vars.iter().copied());
    if env.path("bin").exists() {
        cmd.env(
            "PATH",
            format!(
                "{}:{}",
                env.path("bin").display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        );
    }
    let out = cmd.args(args).output().unwrap();
    let v = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("{e}: {} / {}", stdout(&out), stderr(&out)));
    (out.status.code().unwrap(), v)
}

#[test]
fn trust_usage_is_json_and_never_reads_tmux() {
    let env = Env::new();
    for args in [
        vec!["trust", "host", "--json"],
        vec!["trust", "host", "--accept", "--json"],
        vec!["trust", "host", "--list", "--close", "--json"],
        vec![
            "trust",
            "host",
            "--list",
            "--decline",
            "--prompt",
            "t1.abc",
            "--json",
        ],
        vec!["-H", "elsewhere", "trust", "host", "--list", "--json"],
    ] {
        let (code, v) = run(&env, &args);
        assert!([1, 2].contains(&code), "{args:?}: {v}");
        assert_eq!(v["kind"], "usage", "{args:?}: {v}");
        assert_eq!(v["typed"], false);
        assert!(v["pane"].is_null());
        assert!(v["detail"].is_null());
    }
}

use std::os::unix::fs::PermissionsExt;

/// A recorded screen as text: its SGR and OSC 8 left out, so a test can rewrite it.
fn text(screen: &str) -> String {
    screen
        .lines()
        .map(fleet::core::backend::strip_ansi)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

/// The recorded dialog's answers, as drawn, untouched.
const ANSWERS: &str = " ❯ No, exit\n   Yes, I trust this folder\n";

struct Rig {
    env: Env,
}
impl Rig {
    fn new(mode: &str) -> Self {
        let env = Env::new();
        let source =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/screens/live");
        // Claude Code 2.1.311's recorded dialog, as drawn (SGR, the OSC 8 link), and after Down.
        for (name, fixture) in [("initial", "plain-200x50"), ("moved", "plain-200x50-down")] {
            std::fs::copy(
                source.join(format!("claude-trust-{fixture}.ans")),
                env.path(name),
            )
            .unwrap();
        }
        std::fs::copy(env.path("initial"), env.path("screen")).unwrap();
        std::fs::write(env.path("mode"), mode).unwrap();
        std::fs::create_dir_all(env.path("bin")).unwrap();
        std::fs::write(env.path("bin/ps"), "#!/bin/sh\nprintf '1\\n'\n").unwrap();
        std::fs::set_permissions(env.path("bin/ps"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        std::fs::write(env.path("tmux.sh"), r#"#!/bin/sh
[ "$1" = -u ] && shift
d=$(dirname "$0")
mode=$(cat "$d/mode")
if [ -f "$d/expect-lock" ]; then
 perl -MFcntl=:flock -e 'open(my $f, ">>", $ARGV[0]) or die; exit(flock($f, LOCK_EX|LOCK_NB) ? 1 : 0)' "$d/locks/trust-fleet-test-trust.lock" || exit 1
fi
printf '%s\n' "$*" >> "$d/queries"
# The harness runs until an answer made it exit (a decline; any answer in accept-exits).
command=node; [ "$mode" = shell ] && command=zsh
[ -f "$d/done" ] && [ "$mode" != running ] && command=zsh
case "$1" in
 list-panes)
   case "$*" in *'#{pane_id}|#{pane_current_command}'*)
     [ "$mode" = c7-gone ] || printf '%%12|node|99999999|0|/dev/ttys999|fleet-test-trust\n'
     exit 0 ;;
   esac
   case "$*" in *'=fleet-test-trust'*) ;; *) echo "can't find session" >&2; exit 1;; esac
   pane='%12'; [ "$mode" = changed ] && pane='%13'
   [ "$mode" = change-after-key ] && [ -f "$d/keys" ] && pane='%13'
   windows=1; [ "$mode" = shared ] && windows=2
   if [ -f "$d/done" ]; then
     if [ -f "$d/outcome-host" ]; then
       [ "$mode" = close-changed ] && pane='%13'
       [ "$mode" = close-shared ] && windows=2
     else touch "$d/outcome-host"; fi
   fi
   printf '$1\tfleet-test-trust\t%s\t1\t0\t%s\t200\t50\t%s\t99999999\n' "$windows" "$pane" "$command" ;;
 list-clients)
   [ "$mode" = attached ] && echo '/dev/ttys999'
   [ "$mode" = attach-after-key ] && [ -f "$d/keys" ] && echo '/dev/ttys999'
   exit 0 ;;
 capture-pane)
   [ "$mode" = backend ] && { echo 'secret screen text' >&2; exit 1; }
   [ "$mode" = hang ] && exec sleep 30
   [ "$mode" = gone-after-enter ] && [ -f "$d/done" ] && exit 1
   if [ "$mode" = change-before-enter ] && [ -f "$d/keys" ]; then
     if [ -f "$d/after-navigation" ]; then
       sed 's@scratch/plain@scratch/other@' "$d/moved" > "$d/screen"
     else touch "$d/after-navigation"; fi
   fi
   if [ -f "$d/race" ]; then
     if [ -f "$d/race-read" ]; then
       if [ "$(cat "$d/race")" = selection ]; then cp "$d/moved" "$d/screen"
       else sed 's@scratch/plain@scratch/other@' "$d/initial" > "$d/screen"; fi
     else touch "$d/race-read"; fi
   fi
   cat "$d/screen"
   # display-message, in the same tmux command: the pane at that instant. Claude Code keeps
   # the terminal's cursor on the selected answer's ❯ (a test may pin it elsewhere).
   case "$*" in *'; display-message -p -t %12 '*) ;; *) echo 'no pane query' >&2; exit 1 ;; esac
   if [ -f "$d/cursor" ]; then cursor=$(cat "$d/cursor"); else
     cursor=$(perl -CSD -e 'my @l = <STDIN>; my ($x, $y) = (0, 0);
       for my $i (0 .. $#l) { (my $t = $l[$i]) =~ s/\e\[[0-9;]*m|\e\][^\e\a]*(?:\e\\|\a)//g;
         ($x, $y) = (length $1, $i) if $t =~ /^(.*?)\x{276f} *(?:\d+\. )?(?:No, exit|Yes, I trust this folder)\s*$/ }
       printf "%d\t%d\t%d", $x, $y, scalar @l' < "$d/screen")
   fi
   in_mode=0; [ "$mode" = copy-mode ] && in_mode=1
   printf '\001fleet-trust-pane\t%s\t%s\t%s\n' "$cursor" "$in_mode" "$command" ;;
 send-keys)
   for key do :; done
   printf '%s\n' "$key" >> "$d/keys"
   [ "$mode" = send-failed ] && exit 1
   case "$key" in
    Down|Up)
     [ "$mode" = ignore ] && [ ! -f "$d/ignored" ] && { touch "$d/ignored"; exit 0; }
     [ "$mode" = ignore-all ] && exit 0
     [ "$mode" = replace ] && { sed 's@scratch/plain@scratch/other@' "$d/moved" > "$d/screen"; exit 0; }
     [ "$key" = Down ] && cp "$d/moved" "$d/screen"
     [ "$key" = Up ] && cp "$d/initial" "$d/screen" ;;
    Enter)
     [ "$mode" = uncertain ] && exit 0
     [ "$mode" = ignore-enter ] && [ ! -f "$d/enter-ignored" ] && { touch "$d/enter-ignored"; exit 0; }
     # Accepted, Claude Code clears the dialog and runs on; declined, it exits.
     if [ "$mode" != accept-exits ] && grep '❯' "$d/screen" | grep -q 'Yes, I trust this folder'; then
       printf 'trusted\n' > "$d/screen"
     else printf 'finished\n' > "$d/screen"; touch "$d/done"; fi ;;
   esac; exit 0 ;;
 if-shell) printf '%s\n' "$*" > "$d/kill"; touch "$d/closed"; printf 'killed\tfleet-test-trust\n' ;;
 list-sessions) [ -f "$d/closed" ] || printf '$1\n'; exit 0 ;;
 kill-server) exit 0 ;;
 *) echo 'unexpected tmux query' >&2; exit 1 ;;
esac
"#).unwrap();
        std::fs::set_permissions(env.path("tmux.sh"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        Self { env }
    }
    fn list(&self) -> (i32, Value) {
        run(
            &self.env,
            &[
                "trust",
                "fleet-test-trust",
                "--pane",
                "%12",
                "--list",
                "--json",
            ],
        )
    }
    fn id(&self) -> String {
        self.list().1["prompt"]["id"].as_str().unwrap().into()
    }
    fn answer(&self, id: &str, extra: &[&str]) -> (i32, Value) {
        let mut args = vec![
            "trust",
            "fleet-test-trust",
            "--pane",
            "%12",
            "--prompt",
            id,
            "--json",
        ];
        args.extend_from_slice(extra);
        run(&self.env, &args)
    }
    fn keys(&self) -> String {
        std::fs::read_to_string(self.env.path("keys")).unwrap_or_default()
    }
}

#[test]
fn c1_c2_list_uses_exact_host_without_registry_lock_or_keys() {
    let r = Rig::new("attached");
    let (code, v) = r.list();
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["tmux_session"], "fleet-test-trust");
    assert_eq!(v["handle"], "%12");
    assert_eq!(v["prompt"]["harness"], "claude");
    assert_eq!(v["answerable"], true);
    assert_eq!(v["pane"], serde_json::json!({"cols":200,"rows":50}));
    assert_eq!(r.keys(), "");
    assert!(!r.env.path("locks").exists());
    let queries = std::fs::read_to_string(r.env.path("queries")).unwrap();
    assert_eq!(
        queries
            .lines()
            .filter(|l| l.starts_with("capture-pane"))
            .count(),
        1
    );
    assert!(!queries.contains("list-clients"));
    assert!(queries.contains("=fleet-test-trust"));
    let (code, v) = run(&r.env, &["trust", "fleet-test", "--list", "--json"]);
    assert_eq!((code, v["kind"].as_str()), (1, Some("target_not_found")));
}

#[test]
fn c4_accept_checks_every_key_and_retries_an_ignored_down() {
    for (mode, keys) in [
        ("normal", "Down\nEnter\n"),
        ("ignore", "Down\nDown\nEnter\n"),
    ] {
        let r = Rig::new(mode);
        let id = r.id();
        let (code, v) = r.answer(&id, &["--accept"]);
        assert_eq!(code, 0, "{v}");
        assert_eq!(v["answered"], true);
        assert_eq!(v["answer"], "accept");
        assert_eq!(v["id"], id);
        assert!(v["closed"].is_null());
        assert_eq!(r.keys(), keys);
        let q = std::fs::read_to_string(r.env.path("queries")).unwrap();
        let mut read = false;
        for l in q.lines() {
            if l.starts_with("capture-pane") {
                read = true;
            }
            if l.starts_with("send-keys") {
                assert!(read, "{q}");
                read = false;
            }
        }
    }
}

#[test]
fn c4_dry_run_checks_but_does_not_type_or_close() {
    let r = Rig::new("normal");
    let id = r.id();
    for (mode, steps) in [
        (
            "--accept",
            serde_json::json!([{"key":"Down"},{"key":"Enter"}]),
        ),
        ("--decline", serde_json::json!([{"key":"Enter"}])),
    ] {
        let (code, v) = r.answer(&id, &[mode, "-n"]);
        assert_eq!(code, 0, "{v}");
        assert_eq!(v["dry_run"], true);
        assert_eq!(v["answered"], false);
        assert_eq!(v["steps"], steps);
    }
    assert_eq!(r.keys(), "");
    assert!(!r.env.path("closed").exists());
}

#[test]
fn c1_c4_c6_refusals_are_content_free_and_never_type() {
    for (mode, kind, detail) in [
        ("changed", "target_changed", None),
        ("shared", "host_unsupported", None),
        ("attached", "human_attached", None),
        ("shell", "prompt_not_on_screen", Some("shell")),
        ("backend", "backend_failed", None),
    ] {
        let r = Rig::new(mode);
        let (code, v) = r.answer("t1.00000000", &["--accept"]);
        assert_eq!((code, v["kind"].as_str()), (1, Some(kind)), "{v}");
        assert_eq!(v["detail"].as_str(), detail, "{v}");
        assert_eq!(v["typed"], false);
        assert_eq!(r.keys(), "");
        assert!(!v.to_string().contains("secret screen text"));
        if ["changed", "shared"].contains(&mode) {
            assert!(
                !std::fs::read_to_string(r.env.path("queries"))
                    .unwrap()
                    .contains("capture-pane")
            );
        }
    }
    let r = Rig::new("normal");
    let (code, v) = r.answer("t1.00000000", &["--accept"]);
    assert_eq!((code, v["detail"].as_str()), (1, Some("different_prompt")));
    assert_eq!(r.keys(), "");
    for name in ["", "bad:name", "bad.name", "bad\nname"] {
        let (code, v) = run(&r.env, &["trust", name, "--list", "--json"]);
        assert_eq!(
            (code, v["kind"].as_str()),
            (1, Some("target_invalid")),
            "{v}"
        );
    }
}

#[test]
fn c4_changes_after_a_key_never_send_enter() {
    for (mode, detail) in [
        ("replace", "different_prompt"),
        ("ignore-all", "touched"),
        ("change-after-key", "different_prompt"),
    ] {
        let r = Rig::new(mode);
        let id = r.id();
        let (code, v) = r.answer(&id, &["--accept"]);
        assert_eq!(
            (code, v["kind"].as_str()),
            (1, Some("prompt_changed")),
            "{v}"
        );
        assert_eq!(v["detail"], detail, "{v}");
        assert_eq!(v["typed"], true);
        assert!(!r.keys().contains("Enter"));
        assert_eq!(
            v["keys_sent"].as_u64().unwrap(),
            r.keys().lines().count() as u64
        );
    }
}

#[test]
fn c4_enter_without_a_verified_outcome_is_uncertain() {
    let r = Rig::new("uncertain");
    let id = r.id();
    let (code, v) = r.answer(&id, &["--accept"]);
    assert_eq!(
        (code, v["kind"].as_str()),
        (1, Some("outcome_uncertain")),
        "{v}"
    );
    assert_eq!(v["typed"], true);
    assert_eq!(v["screen"]["prompt"]["id"], id);
    // After a Down landed the dialog listens: an unchanged screen is a slow redraw, and a
    // second Enter could answer whatever Claude Code draws next. One Enter only.
    assert_eq!(r.keys(), "Down\nEnter\n");
}

#[test]
fn c4_lock_covers_read_type_verify_and_list_does_not_take_it() {
    let r = Rig::new("normal");
    let id = r.id();
    std::fs::create_dir_all(r.env.path("locks")).unwrap();
    let f = std::fs::File::create(r.env.path("locks/trust-fleet-test-trust.lock")).unwrap();
    f.lock().unwrap();
    assert_eq!(r.list().0, 0);
    let (code, v) = r.answer(&id, &["--accept"]);
    assert_eq!((code, v["kind"].as_str()), (1, Some("busy")), "{v}");
    assert_eq!(r.keys(), "");
    drop(f);
    std::fs::write(r.env.path("expect-lock"), "").unwrap();
    assert_eq!(r.answer(&id, &["--accept"]).0, 0);
}

#[test]
fn c5_decline_closes_only_after_the_harness_exits() {
    for (mode, closed) in [("normal", true), ("running", false)] {
        let r = Rig::new(mode);
        let id = r.id();
        let (code, v) = r.answer(&id, &["--decline", "--close"]);
        assert_eq!(code, 0, "{v}");
        assert_eq!(v["answered"], true);
        assert_eq!(v["closed"], closed);
        if !closed {
            assert_eq!(v["close_detail"], "still_running");
        }
        assert_eq!(r.keys(), "Enter\n");
        assert_eq!(r.env.path("closed").exists(), closed);
        if closed {
            // Checked and killed in one tmux command, by the ids read before answering.
            let kill = std::fs::read_to_string(r.env.path("kill")).unwrap();
            assert!(kill.starts_with("if-shell -F -t $1 "), "{kill}");
            assert!(kill.contains("#{pane_id},%12}"), "{kill}");
            assert!(kill.contains("kill-session -t '$1'"), "{kill}");
        }
    }
}

#[test]
fn c2_c6_every_screen_refusal_is_shared_by_list_and_answer() {
    let initial = Rig::new("normal");
    let id = initial.id();
    let screen = text(&std::fs::read_to_string(initial.env.path("initial")).unwrap());
    // The dialog without the launch line and the blank under it: its rule first.
    let dialog = screen.lines().skip(2).collect::<Vec<_>>().join("\n");
    for (screen, kind, detail) in [
        ("finished".into(), "prompt_not_on_screen", "no_prompt"),
        (
            screen.replace(ANSWERS, "   No, exit\n ❯ Yes, I trust this folder\n"),
            "prompt_not_on_screen",
            "touched",
        ),
        (
            format!("⏺ Quoted dialog:\n{dialog}"),
            "prompt_not_on_screen",
            "not_alone",
        ),
        (
            dialog.lines().skip(2).collect::<Vec<_>>().join("\n"),
            "prompt_incomplete",
            "clipped",
        ),
        (
            screen.replace("Enter to confirm · Esc to cancel", "Unknown footer"),
            "prompt_incomplete",
            "layout_unsupported",
        ),
    ] {
        let r = Rig::new("normal");
        std::fs::write(r.env.path("screen"), screen).unwrap();
        let (code, v) = r.list();
        assert_eq!(code, 0, "{v}");
        assert_eq!(v["answerable"], false);
        assert_eq!(v["detail"], detail);
        if kind == "prompt_incomplete" {
            assert_eq!(v["prompt"]["incomplete"], true);
            for k in ["id", "lines", "choices", "selected"] {
                assert!(v["prompt"][k].is_null(), "{v}");
            }
            assert_eq!(v["prompt"]["untouched"], false);
        }
        let (code, v) = r.answer(&id, &["--accept"]);
        assert_eq!((code, v["kind"].as_str()), (1, Some(kind)), "{v}");
        assert_eq!(v["detail"], detail);
        assert_eq!(v["typed"], false);
        assert_eq!(r.keys(), "");
        // The codes and the words never quote the screen.
        let error = v["error"].as_str().unwrap();
        assert!(error.ends_with("; nothing typed"), "{error}");
        for word in [
            "trustcap",
            "Quoted",
            "Unknown footer",
            "trust this folder",
            "No, exit",
        ] {
            assert!(!error.contains(word), "{error}");
        }
    }
}

#[test]
fn c4_allow_attached_and_upward_navigation() {
    let r = Rig::new("attached");
    let id = r.id();
    assert_eq!(r.answer(&id, &["--decline", "--allow-attached"]).0, 0);
    assert_eq!(r.keys(), "Enter\n");
    let r = Rig::new("normal");
    let original = text(&std::fs::read_to_string(r.env.path("initial")).unwrap());
    let reversed = original.replace(ANSWERS, " ❯ Yes, I trust this folder\n   No, exit\n");
    let untouched = original.replace(ANSWERS, "   Yes, I trust this folder\n ❯ No, exit\n");
    std::fs::write(r.env.path("screen"), untouched).unwrap();
    // The fake draws this accepting first row after Up.
    std::fs::write(r.env.path("initial"), reversed).unwrap();
    let id = r.id();
    let (code, v) = r.answer(&id, &["--accept"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(r.keys(), "Up\nEnter\n");
}

#[test]
fn c4_backend_errors_after_typing_and_lock_io_are_explicit() {
    let r = Rig::new("send-failed");
    let id = r.id();
    let (code, v) = r.answer(&id, &["--accept"]);
    assert_eq!(
        (code, v["kind"].as_str()),
        (1, Some("backend_failed")),
        "{v}"
    );
    assert_eq!(v["typed"], true);
    assert_eq!(v["keys_sent"], 1);
    assert_eq!(r.keys(), "Down\n");
    let r = Rig::new("normal");
    let id = r.id();
    std::fs::write(r.env.path("locks"), "not a directory").unwrap();
    let (code, v) = r.answer(&id, &["--accept"]);
    assert_eq!((code, v["kind"].as_str()), (1, Some("io")), "{v}");
    assert_eq!(v["typed"], false);
}

#[test]
fn c5_a_changed_host_or_a_shell_with_children_is_never_closed() {
    let r = Rig::new("change-after-key");
    let id = r.id();
    let (code, v) = r.answer(&id, &["--decline", "--close"]);
    // Once Enter went out an unreadable/replaced host cannot prove the answer landed.
    assert_eq!(
        (code, v["kind"].as_str()),
        (1, Some("outcome_uncertain")),
        "{v}"
    );
    assert!(!r.env.path("closed").exists());
    let r = Rig::new("normal");
    std::fs::write(r.env.path("bin/ps"), "#!/bin/sh\nprintf '99999999\\n'\n").unwrap();
    let id = r.id();
    let (code, v) = r.answer(&id, &["--decline", "--close"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["closed"], false);
    assert_eq!(v["close_detail"], "still_running");
    assert!(!r.env.path("closed").exists());
}

#[test]
fn c7_starting_resumes_name_only_the_stamped_existing_pane() {
    for (mode, pane, shell, expected) in [
        (
            "normal",
            serde_json::json!("%12"),
            99999999,
            serde_json::json!({"tmux_session":"fleet-test-trust","handle":"%12"}),
        ),
        ("normal", Value::Null, 99999999, Value::Null),
        ("c7-gone", serde_json::json!("%12"), 99999999, Value::Null),
        ("normal", serde_json::json!("%12"), 88888888, Value::Null),
    ] {
        let r = Rig::new(mode);
        std::fs::create_dir_all(r.env.path("locks")).unwrap();
        let at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        std::fs::write(
            r.env.path("locks/resume-claude-example.stamp"),
            serde_json::json!({"at":at,"pane":pane,"shell":shell}).to_string(),
        )
        .unwrap();
        let dir = r.env.path("home");
        let (code, v) = run(
            &r.env,
            &[
                "spawn",
                "--resume",
                "example",
                "--dir",
                dir.to_str().unwrap(),
                "--json",
            ],
        );
        assert_eq!(
            (code, v["kind"].as_str()),
            (1, Some("session_starting")),
            "{v}"
        );
        assert_eq!(
            v.as_object().unwrap().get("starting"),
            Some(&expected),
            "{v}"
        );
        assert_eq!(r.keys(), "");
    }
}

#[test]
fn c4_a_change_on_the_last_read_never_sends_enter() {
    let r = Rig::new("change-before-enter");
    let id = r.id();
    let (code, v) = r.answer(&id, &["--accept"]);
    assert_eq!(
        (code, v["kind"].as_str()),
        (1, Some("prompt_changed")),
        "{v}"
    );
    assert_eq!(v["detail"], "different_prompt");
    assert_eq!(r.keys(), "Down\n");
    assert_eq!(v["keys_sent"], 1);
}

#[test]
fn c4_a_change_before_any_key_is_a_refusal_without_typing() {
    for (race, detail) in [("", "different_prompt"), ("selection", "touched")] {
        let r = Rig::new("normal");
        let id = r.id();
        std::fs::write(r.env.path("race"), race).unwrap();
        let (code, v) = r.answer(&id, &["--accept"]);
        assert_eq!(
            (code, v["kind"].as_str()),
            (1, Some("prompt_not_on_screen")),
            "{v}"
        );
        assert_eq!(v["detail"], detail);
        assert_eq!(v["typed"], false);
        assert_eq!(r.keys(), "");
    }
}

#[test]
fn c4_enter_retries_only_the_same_selected_dialog_and_unreadable_is_null() {
    let r = Rig::new("ignore-enter");
    let id = r.id();
    let (code, v) = r.answer(&id, &["--decline"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(r.keys(), "Enter\nEnter\n");
    let r = Rig::new("gone-after-enter");
    let id = r.id();
    let (code, v) = r.answer(&id, &["--decline"]);
    assert_eq!(
        (code, v["kind"].as_str()),
        (1, Some("outcome_uncertain")),
        "{v}"
    );
    assert!(v["screen"].is_null());
    assert_eq!(r.keys(), "Enter\n");
}

#[test]
fn c5_host_replaced_or_split_after_answer_is_left_alone() {
    for mode in ["close-changed", "close-shared"] {
        let r = Rig::new(mode);
        let id = r.id();
        let (code, v) = r.answer(&id, &["--decline", "--close"]);
        assert_eq!(code, 0, "{v}");
        assert_eq!(v["answered"], true);
        assert_eq!(v["closed"], false);
        assert_eq!(v["close_detail"], "pane_changed");
        assert!(!r.env.path("closed").exists());
    }
}

// After Esc Claude Code exits and leaves the dialog's text above the shell's prompt, the
// terminal's cursor on that prompt. A foreground fleet doesn't know for a shell (or a wrapper
// still running) must not make it a live prompt: `no_prompt`, and nothing typed.
#[test]
fn c3_a_dialog_left_above_the_shell_is_never_answered() {
    let source =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/screens/live");
    for (mode, detail) in [("normal", "no_prompt"), ("shell", "shell")] {
        let r = Rig::new("normal");
        let id = r.id();
        std::fs::write(r.env.path("mode"), mode).unwrap();
        std::fs::copy(
            source.join("claude-trust-plain-200x50-after-esc.ans"),
            r.env.path("screen"),
        )
        .unwrap();
        // The recorded cursor: (42, 18), on the shell's prompt; 50 rows.
        std::fs::write(r.env.path("cursor"), "42\t18\t50").unwrap();
        let (code, v) = r.list();
        assert_eq!(code, 0, "{v}");
        assert!(v["prompt"].is_null(), "{v}");
        assert_eq!(
            (v["answerable"].as_bool(), v["detail"].as_str()),
            (Some(false), Some(detail))
        );
        for answer in ["--accept", "--decline"] {
            let (code, v) = r.answer(&id, &[answer]);
            assert_eq!(
                (code, v["kind"].as_str(), v["detail"].as_str()),
                (1, Some("prompt_not_on_screen"), Some(detail)),
                "{v}"
            );
            assert_eq!(v["typed"], false);
        }
        assert_eq!(r.keys(), "");
    }
}

// An accept whose harness then exits to its shell did not trust the folder: uncertain, while
// a decline that ends in the shell is the decline answered.
#[test]
fn c4_an_accept_that_ends_in_the_shell_is_uncertain() {
    let r = Rig::new("accept-exits");
    let id = r.id();
    let (code, v) = r.answer(&id, &["--accept"]);
    assert_eq!(
        (code, v["kind"].as_str()),
        (1, Some("outcome_uncertain")),
        "{v}"
    );
    assert_eq!(v["typed"], true);
    assert_eq!(v["screen"]["detail"], "shell", "{v}");
    assert_eq!(r.keys(), "Down\nEnter\n");
    let r = Rig::new("accept-exits");
    let id = r.id();
    let (code, v) = r.answer(&id, &["--decline"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["answered"], true);
}

#[test]
fn c4_a_terminal_attached_between_keys_stops_them() {
    let r = Rig::new("attach-after-key");
    let id = r.id();
    let (code, v) = r.answer(&id, &["--accept"]);
    assert_eq!(
        (code, v["kind"].as_str()),
        (1, Some("human_attached")),
        "{v}"
    );
    assert_eq!(v["typed"], true);
    assert_eq!(v["keys_sent"], 1);
    assert_eq!(r.keys(), "Down\n");
    // Checked right before each key.
    let q = std::fs::read_to_string(r.env.path("queries")).unwrap();
    let q: Vec<_> = q.lines().collect();
    for (i, _) in q
        .iter()
        .enumerate()
        .filter(|(_, l)| l.starts_with("send-keys"))
    {
        assert!(q[i - 1].starts_with("list-clients"), "{q:?}");
    }
}

#[test]
fn c3_a_pane_in_copy_mode_is_not_alone() {
    let r = Rig::new("copy-mode");
    let (code, v) = r.list();
    assert_eq!(code, 0, "{v}");
    assert_eq!(
        (v["answerable"].as_bool(), v["detail"].as_str()),
        (Some(false), Some("not_alone"))
    );
    let id = v["prompt"]["id"].as_str().unwrap().to_string();
    let (code, v) = r.answer(&id, &["--accept"]);
    assert_eq!((code, v["detail"].as_str()), (1, Some("not_alone")), "{v}");
    assert_eq!(r.keys(), "");
}

#[test]
fn c6_a_tmux_that_does_not_answer_is_backend_failed() {
    let r = Rig::new("hang");
    let started = std::time::Instant::now();
    let (code, v) = r.list();
    assert_eq!(
        (code, v["kind"].as_str()),
        (1, Some("backend_failed")),
        "{v}"
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(20));
}

// `-H` is resolved as for other commands: this machine by any of its names is local.
#[test]
fn c1_this_machine_by_any_name_is_local() {
    let r = Rig::new("normal");
    r.env
        .write_config(&serde_json::json!({ "version": 1, "self": "laptop" }));
    let list = ["trust", "fleet-test-trust", "--list", "--json"];
    for (vars, host) in [
        (vec![], vec!["-H", "local"]),
        (vec![], vec!["-H", "laptop"]),
        (vec![("FLEET_HOST", "laptop")], vec![]),
        (vec![("FLEET_HOST", "elsewhere")], vec!["--local"]),
    ] {
        let args: Vec<_> = host.iter().chain(list.iter()).copied().collect();
        let (code, v) = run_with(&r.env, &vars, &args);
        assert_eq!(code, 0, "{vars:?} {host:?}: {v}");
        assert_eq!(v["prompt"]["harness"], "claude", "{v}");
    }
    for (vars, host) in [
        (vec![], vec!["-H", "elsewhere"]),
        (vec![("FLEET_HOST", "elsewhere")], vec![]),
    ] {
        let args: Vec<_> = host.iter().chain(list.iter()).copied().collect();
        let (code, v) = run_with(&r.env, &vars, &args);
        assert_eq!(
            (code, v["kind"].as_str()),
            (1, Some("usage")),
            "{vars:?} {host:?}: {v}"
        );
    }
}

/// A stand-in for Claude Code 2.1.311 at its trust prompt, for the end-to-end tests below, drawn
/// as recorded (`testdata/screens/live/claude-trust-*`): a full-width rule, "Accessing
/// workspace:", the folder, the answers with "No, exit" preselected, the footer, and the
/// terminal's cursor hidden on the selected answer's `❯`. It drops the first key it reads (the
/// dialog is drawn before it listens for keys) and moves on Down and Up. On Enter it accepts
/// (clears the dialog and idles) or declines: exits, leaving the dialog's text above the shell.
const FAKE_CLAUDE: &str = r#"#!/usr/bin/perl
use strict;
use warnings;
binmode STDOUT, ':utf8';
$| = 1;
my (undef, $cols) = split ' ', (`stty size 2>/dev/null` || '24 80');
$cols = 80 unless $cols && $cols > 0;
my @body = ("\e[38;5;220m" . ("\x{2500}" x $cols) . "\e[39m",
    " \e[1m\e[38;5;220mAccessing workspace:\e[0m", "", " \e[1m$ENV{PWD}\e[0m", "",
    " Quick safety check: Is this a project you created or one you trust?", "",
    " Claude Code'll be able to read, edit, and execute files here.", "",
    " \e[38;5;246mSecurity guide\e[39m", "");
my @answers = ("No, exit", "Yes, I trust this folder");
my $sel = 0;
sub redraw {
    my $up = @body + $_[0];
    print "\e[${up}A";
    draw();
}
sub draw {
    print "\r\e[J";
    print "$_\r\n" for @body;
    for my $i (0 .. $#answers) {
        print $i == $sel ? " \e[38;5;153m\x{276f} $answers[$i]\e[39m\r\n" : "   $answers[$i]\r\n";
    }
    print "\r\n \e[38;5;246mEnter to confirm \x{b7} Esc to cancel\e[39m\r\n";
    # From the row under the footer up to the selected answer, onto its ❯.
    my $back = 4 - $sel;
    print "\e[?25l\e[${back}A\e[2G";
}
system('stty raw -echo');
draw();
my $first = 1;
while (sysread(STDIN, my $key, 64)) {
    if ($first) { $first = 0; next; }
    if ($key eq "\e[B" && !$sel) { $sel = 1; redraw(0); }
    elsif ($key eq "\e[A" && $sel) { $sel = 0; redraw(1); }
    elsif ($key eq "\r") {
        if ($sel) {
            my $up = @body + $sel;
            print "\e[${up}A\r\e[J\e[?25htrusted\r\n";
            sleep 1 while 1;
        }
        print "\e[4B\r\e[?25h";
        system('stty sane');
        exit 0;
    }
}
"#;

/// `fleet` against this test's own tmux server (`Env`), launching [`FAKE_CLAUDE`].
struct Live {
    env: Env,
}

impl Live {
    fn new() -> Option<Self> {
        if fleet::core::tools::find_binary("tmux").is_none() {
            eprintln!("SKIPPED: tmux is not installed, so this trust test checks nothing");
            return None;
        }
        let env = Env::new();
        for d in ["bin", "work"] {
            std::fs::create_dir_all(env.path(d)).unwrap();
        }
        std::fs::write(env.path("bin/claude"), FAKE_CLAUDE).unwrap();
        std::fs::set_permissions(
            env.path("bin/claude"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        Some(Live { env })
    }

    fn fleet(&self, args: &[&str]) -> (i32, Value) {
        let out = self
            .env
            .cmd()
            .env("FLEET_CMD", self.env.path("bin/claude"))
            .env("SHELL", "/bin/sh")
            .timeout(std::time::Duration::from_secs(30))
            .args(args)
            .output()
            .unwrap();
        let v = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("{e}: {} / {}", stdout(&out), stderr(&out)));
        (out.status.code().unwrap(), v)
    }

    /// A Session host `spawn` made, waiting at the trust prompt: its handle and `--list`.
    fn spawn(&self, name: &str) -> (String, Value) {
        let work = self.env.path("work").display().to_string();
        let (code, v) = self.fleet(&[
            "spawn",
            "--backend",
            "tmux",
            "--json",
            "--dir",
            &work,
            "--name",
            name,
        ]);
        assert_eq!(code, 0, "{v}");
        assert_eq!(v["tmux_session"], name, "{v}");
        let handle = v["handle"].as_str().unwrap().to_string();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        loop {
            let (code, v) = self.fleet(&["trust", name, "--pane", &handle, "--list", "--json"]);
            assert_eq!(code, 0, "{v}");
            if v["answerable"] == true {
                return (handle, v);
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the trust prompt never became answerable: {v}"
            );
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }

    fn has_session(&self, name: &str) -> bool {
        self.env
            .tmux(&["has-session", "-t", &format!("={name}")])
            .status
            .success()
    }
}

// C2–C4 against a real tmux: spawn's launch line above the dialog, the ignored first Down
// pressed again, Enter once the cursor is on "Yes", and the prompt gone after it.
#[test]
fn live_accept_in_a_spawned_session_host() {
    let Some(l) = Live::new() else { return };
    let name = "fleet-test-trust-accept";
    let (handle, list) = l.spawn(name);
    let p = &list["prompt"];
    assert_eq!(p["harness"], "claude", "{list}");
    assert_eq!(p["lines"][0], "Accessing workspace:", "{list}");
    assert_eq!(
        p["lines"].as_array().unwrap().last().unwrap(),
        "Security guide"
    );
    assert_eq!(p["selected"], 0);
    assert_eq!(p["untouched"], true);
    assert_eq!(list["handle"], handle.as_str());
    let id = p["id"].as_str().unwrap();
    let (code, v) = l.fleet(&[
        "trust", name, "--pane", &handle, "--accept", "--prompt", id, "--json",
    ]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["answered"], true, "{v}");
    assert_eq!(v["answer"], "accept");
    assert_eq!(
        v["steps"],
        serde_json::json!([{"key":"Down"},{"key":"Down"},{"key":"Enter"}]),
        "{v}"
    );
    assert!(v["closed"].is_null());
    let (code, v) = l.fleet(&["trust", name, "--list", "--json"]);
    assert_eq!(code, 0, "{v}");
    assert!(v["prompt"].is_null(), "{v}");
    assert_eq!(v["detail"], "no_prompt");
    assert!(l.has_session(name));
}

// C5 against a real tmux: the ignored first Enter pressed again, the harness exits, the
// tmux session is killed; and a stale --pane is refused before anything is read.
#[test]
fn live_decline_and_close_a_spawned_session_host() {
    let Some(l) = Live::new() else { return };
    let name = "fleet-test-trust-decline";
    let (handle, list) = l.spawn(name);
    let id = list["prompt"]["id"].as_str().unwrap();
    let (code, v) = l.fleet(&[
        "trust",
        name,
        "--pane",
        "%999",
        "--decline",
        "--prompt",
        id,
        "--json",
    ]);
    assert_eq!(
        (code, v["kind"].as_str()),
        (1, Some("target_changed")),
        "{v}"
    );
    assert_eq!(v["typed"], false);
    let (code, v) = l.fleet(&[
        "trust",
        name,
        "--pane",
        &handle,
        "--decline",
        "--close",
        "--prompt",
        id,
        "--json",
    ]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["answered"], true, "{v}");
    assert_eq!(v["answer"], "decline");
    assert_eq!(
        v["steps"],
        serde_json::json!([{"key":"Enter"},{"key":"Enter"}]),
        "{v}"
    );
    assert_eq!(v["closed"], true, "{v}");
    assert!(!l.has_session(name));
}
