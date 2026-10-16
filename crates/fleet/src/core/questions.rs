//! Claude Code's `AskUserQuestion` prompt, as the transcript records it: the questions it
//! asked and what became of them. A port of the web app's `web/lib/questions.mjs`.

use serde::Serialize;
use serde_json::Value;

pub const QUESTION_TOOL: &str = "AskUserQuestion";

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct QuestionOption {
    pub label: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Question {
    pub question: String,
    pub header: String,
    #[serde(rename = "multiSelect")]
    pub multi_select: bool,
    pub options: Vec<QuestionOption>,
}

fn str_of(v: Option<&Value>) -> String {
    v.and_then(Value::as_str).unwrap_or_default().to_string()
}

/// The tool input's questions, normalized; `None` when it isn't a usable question list.
pub fn normalize_questions(input: &Value) -> Option<Vec<Question>> {
    let raw = input.get("questions")?.as_array()?;
    if raw.is_empty() {
        return None;
    }
    let mut out = Vec::with_capacity(raw.len());
    for q in raw {
        if !q.is_object() || str_of(q.get("question")).trim().is_empty() {
            return None;
        }
        let options: Vec<QuestionOption> = q
            .get("options")
            .and_then(Value::as_array)
            .map(|opts| {
                opts.iter()
                    .filter(|o| o.is_object() && !str_of(o.get("label")).trim().is_empty())
                    .map(|o| QuestionOption {
                        label: str_of(o.get("label")),
                        description: str_of(o.get("description")),
                    })
                    .collect()
            })
            .unwrap_or_default();
        if options.is_empty() {
            return None;
        }
        out.push(Question {
            question: str_of(q.get("question")),
            header: str_of(q.get("header")),
            multi_select: q.get("multiSelect") == Some(&Value::Bool(true)),
            options,
        });
    }
    Some(out)
}

/// Plain-markdown rendering, for readers that don't know `kind: "question"`.
pub fn questions_text(questions: &[Question]) -> String {
    questions
        .iter()
        .map(|q| {
            let head = if q.header.is_empty() {
                format!("**{}**", q.question)
            } else {
                format!("**{}: {}**", q.header, q.question)
            };
            std::iter::once(head)
                .chain(q.options.iter().map(|o| format!("- {}", o.label)))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// What became of a prompt, from its `tool_result`.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// One answer per question, in question order (`None`: that question has none), and the
    /// option indexes each one selected (empty for a typed answer).
    Answered {
        answers: Vec<Option<String>>,
        picked: Vec<Vec<usize>>,
    },
    /// Esc, "Chat about this", an interrupt: the tool call failed.
    Declined,
}

impl Outcome {
    pub fn status(&self) -> &'static str {
        match self {
            Outcome::Answered { .. } => "answered",
            Outcome::Declined => "declined",
        }
    }

    /// `{ answers, picked }` (the web's `questionOutcome` plus `picked`) or `{ declined: true }`.
    pub fn to_json(&self) -> Value {
        match self {
            Outcome::Answered { answers, picked } => {
                serde_json::json!({ "answers": answers, "picked": picked })
            }
            Outcome::Declined => serde_json::json!({ "declined": true }),
        }
    }
}

/// The option indexes an answer recorded as text selected. Claude Code records a
/// single-select pick as its label and joins multi-select picks with `", "`; a label may itself
/// contain `", "`, so at each point the longest label that fits (followed by `", "` or the end)
/// wins. A segment no label explains is typed text and selects nothing.
pub fn picked(q: &Question, answer: &str) -> Vec<usize> {
    if let Some(i) = q.options.iter().position(|o| o.label == answer) {
        return vec![i];
    }
    if !q.multi_select {
        return Vec::new();
    }
    let mut by_len: Vec<usize> = (0..q.options.len()).collect();
    by_len.sort_by_key(|&i| std::cmp::Reverse(q.options[i].label.len()));
    let mut out = Vec::new();
    let mut rest = answer;
    while !rest.is_empty() {
        let hit = by_len.iter().find_map(|&i| {
            let after = rest.strip_prefix(q.options[i].label.as_str())?;
            if after.is_empty() {
                Some((i, after))
            } else {
                after.strip_prefix(", ").map(|a| (i, a))
            }
        });
        match hit {
            Some((i, after)) => {
                out.push(i);
                rest = after;
            }
            // Typed text: skip to the next separator.
            None => rest = rest.split_once(", ").map_or("", |(_, a)| a),
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// The outcome of `questions` from the transcript entry holding its `tool_result` block.
pub fn question_outcome(questions: &[Question], entry: &Value, block: &Value) -> Outcome {
    let is_error = block.get("is_error").is_some_and(truthy);
    let answers = entry.get("toolUseResult").and_then(|r| r.get("answers"));
    match answers {
        Some(a) if !is_error && (a.is_object() || a.is_array()) => {
            let answers: Vec<Option<String>> = questions
                .iter()
                .map(|q| {
                    a.get(q.question.as_str())
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .collect();
            let picked = questions
                .iter()
                .zip(&answers)
                .map(|(q, a)| a.as_deref().map(|a| picked(q, a)).unwrap_or_default())
                .collect();
            Outcome::Answered { answers, picked }
        }
        _ => Outcome::Declined,
    }
}

// ------------------------------------------------------------------ answering

/// Longest typed answer accepted for one question.
pub const MAX_ANSWER_TEXT: usize = 2000;

/// One question's validated answer: option indexes (sorted, unique) and/or typed text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    pub options: Vec<usize>,
    pub text: Option<String>,
}

/// Check `answers` (`[{ options: [index…], text? }]`, one per question, in order) against
/// `questions` (`validateAnswer`). Single-select: exactly one option, or text. Multi-select:
/// any options and/or text, at least one.
pub fn validate_answer(answers: &Value, questions: &[Question]) -> Result<Vec<Answer>, String> {
    let list = answers
        .as_array()
        .filter(|a| a.len() == questions.len())
        .ok_or_else(|| format!("answers must be an array of {}", questions.len()))?;
    let mut out = Vec::with_capacity(list.len());
    for (i, (q, a)) in questions.iter().zip(list).enumerate() {
        let empty = Vec::new();
        let opts = a.get("options").and_then(Value::as_array).unwrap_or(&empty);
        let mut options = Vec::with_capacity(opts.len());
        for n in opts {
            match n.as_u64().map(|n| n as usize) {
                Some(n) if n < q.options.len() => options.push(n),
                _ => return Err(format!("answers[{i}].options: unknown option")),
            }
        }
        options.sort_unstable();
        options.dedup();
        let raw = a.get("text").and_then(Value::as_str).unwrap_or("");
        if raw.contains(['\r', '\n']) {
            return Err(format!("answers[{i}].text must be a single line"));
        }
        // Typed into a live terminal: no escape sequences, no control characters (C0, DEL,
        // C1) — they would drive the TUI instead of filling in the answer.
        if raw.chars().any(char::is_control) {
            return Err(format!(
                "answers[{i}].text must not contain control characters or escape sequences"
            ));
        }
        let text = raw.trim();
        if text.chars().count() > MAX_ANSWER_TEXT {
            return Err(format!(
                "answers[{i}].text too long (max {MAX_ANSWER_TEXT})"
            ));
        }
        if q.multi_select {
            if options.is_empty() && text.is_empty() {
                return Err(format!(
                    "answers[{i}]: pick at least one option or type something"
                ));
            }
        } else if options.len() + usize::from(!text.is_empty()) != 1 {
            return Err(format!(
                "answers[{i}]: pick exactly one option or type something"
            ));
        }
        out.push(Answer {
            options,
            text: (!text.is_empty()).then(|| text.to_string()),
        });
    }
    Ok(out)
}

/// One thing to do at the terminal: press a key (tmux key name) or type text literally.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Key(String),
    Text(String),
}

/// Where the screen must be before a step of an answer: the form on question `question`
/// with the cursor on `row`, exactly the rows `ticked` (multi-select), the "Type something"
/// row holding `typed` when set, and each tab answered (☒) or not as `done` says — or the
/// review screen listing exactly `expect` (each question with the answer it got), "Submit
/// answers" focused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum At {
    Form {
        question: usize,
        row: usize,
        ticked: Vec<usize>,
        typed: Option<String>,
        done: Vec<bool>,
    },
    Review {
        expect: Vec<(String, String)>,
    },
}

/// An answer as Claude Code's review screen shows it: the picked labels in option order, then
/// the typed text, joined with `", "`.
pub fn review_text(q: &Question, a: &Answer) -> String {
    a.options
        .iter()
        .map(|&i| q.options[i].label.clone())
        .chain(a.text.clone())
        .collect::<Vec<_>>()
        .join(", ")
}

/// One step of an answer and what the screen must show before it is taken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Planned {
    pub step: Step,
    pub at: At,
}

/// The keystrokes answering `questions` with validated `answers` (`answerSteps`), each with
/// the state the form must be in before it (so every key can be checked against the screen).
///
/// Claude Code's prompt: one tab per question, the cursor on option 1. Single-select: ↓ to an
/// option + Enter picks it and moves on; option N+1 is "Type something." (typing there fills
/// it in, Enter picks it). Multi-select: Enter toggles the option under the cursor; typing on
/// "Type something" fills and checks it; the row after it ("Next" / "Submit") + Enter moves
/// on. Two or more questions end on a review screen with "Submit answers" focused → Enter.
pub fn answer_plan(questions: &[Question], answers: &[Answer]) -> Vec<Planned> {
    /// The form's state while planning one question.
    struct Form<'a> {
        plan: &'a mut Vec<Planned>,
        questions: usize,
        question: usize,
        multi: bool,
        row: usize,
        ticked: Vec<usize>,
        typed: Option<String>,
    }
    impl Form<'_> {
        fn push(&mut self, step: Step) {
            self.plan.push(Planned {
                step,
                at: At::Form {
                    question: self.question,
                    row: self.row,
                    ticked: self.ticked.clone(),
                    typed: self.typed.clone(),
                    // Earlier tabs are answered; a multi-select tab counts as answered as soon
                    // as anything in it is ticked.
                    done: (0..self.questions)
                        .map(|j| {
                            j < self.question
                                || (j == self.question && self.multi && !self.ticked.is_empty())
                        })
                        .collect(),
                },
            });
        }
        fn down_to(&mut self, to: usize) {
            while self.row < to {
                self.push(Step::Key("Down".into()));
                self.row += 1;
            }
        }
    }

    let mut plan = Vec::new();
    for (qi, (q, a)) in questions.iter().zip(answers).enumerate() {
        let other = q.options.len(); // the "Type something" row
        let mut f = Form {
            plan: &mut plan,
            questions: questions.len(),
            question: qi,
            multi: q.multi_select,
            row: 0,
            ticked: Vec::new(),
            typed: None,
        };
        if !q.multi_select {
            match &a.text {
                Some(t) => {
                    f.down_to(other);
                    f.push(Step::Text(t.clone()));
                    f.typed = Some(t.clone());
                }
                None => f.down_to(a.options[0]),
            }
            f.push(Step::Key("Enter".into()));
            continue;
        }
        for &n in &a.options {
            f.down_to(n);
            f.push(Step::Key("Enter".into()));
            f.ticked.push(n);
        }
        if let Some(t) = &a.text {
            f.down_to(other);
            f.push(Step::Text(t.clone()));
            f.typed = Some(t.clone());
            f.ticked.push(other);
        }
        f.down_to(other + 1); // "Next" / "Submit"
        f.push(Step::Key("Enter".into()));
    }
    if questions.len() > 1 {
        plan.push(Planned {
            step: Step::Key("Enter".into()), // review → "Submit answers"
            at: At::Review {
                expect: questions
                    .iter()
                    .zip(answers)
                    .map(|(q, a)| (q.question.trim().to_string(), review_text(q, a)))
                    .collect(),
            },
        });
    }
    plan
}

/// Just the keystrokes of [`answer_plan`].
pub fn answer_steps(questions: &[Question], answers: &[Answer]) -> Vec<Step> {
    answer_plan(questions, answers)
        .into_iter()
        .map(|p| p.step)
        .collect()
}

/// JavaScript truthiness, for flags the transcript may spell loosely.
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn qs() -> Vec<Question> {
        normalize_questions(&json!({
            "questions": [
                { "question": "Which pets do you like?", "header": "Pets", "multiSelect": true,
                  "options": [{ "label": "Dog" }, { "label": "Cat" }, { "label": "Fish" }] },
                { "question": "Which city?", "header": "City", "multiSelect": false,
                  "options": [{ "label": "Paris", "description": "FR" }, { "label": "Rome" }, { "label": "Oslo" }] }
            ]
        }))
        .unwrap()
    }

    #[test]
    fn normalize_keeps_question_header_multiselect_and_labelled_options() {
        let q = qs();
        assert_eq!(q.len(), 2);
        assert_eq!(
            q[1].options[0],
            QuestionOption {
                label: "Paris".into(),
                description: "FR".into()
            }
        );
        assert!(q[0].multi_select);
        assert!(!q[1].multi_select);
        assert_eq!(normalize_questions(&json!({})), None);
        assert_eq!(
            normalize_questions(&json!({ "questions": [{ "question": "Q", "options": [] }] })),
            None
        );
        assert_eq!(
            normalize_questions(
                &json!({ "questions": [{ "question": " ", "options": [{ "label": "a" }] }] })
            ),
            None
        );
        // Unlabelled options are dropped, not fatal.
        let one = normalize_questions(
            &json!({ "questions": [{ "question": "Q", "options": [{ "label": "" }, { "label": "a" }, 3] }] }),
        )
        .unwrap();
        assert_eq!(one[0].options.len(), 1);
    }

    #[test]
    fn questions_text_is_markdown() {
        let q = normalize_questions(
            &json!({ "questions": [{ "question": "Merge it?", "header": "Merge",
            "options": [{ "label": "Yes", "description": "squash" }, { "label": "No" }] }] }),
        )
        .unwrap();
        assert_eq!(questions_text(&q), "**Merge: Merge it?**\n- Yes\n- No");
        let two = qs();
        assert!(questions_text(&two).contains("- Fish\n\n**City: Which city?**"));
    }

    #[test]
    fn outcome_is_answers_in_question_order_or_declined() {
        let q = qs();
        let entry = json!({ "toolUseResult": { "answers": { "Which city?": "Rome", "Which pets do you like?": "Dog, Fish" } } });
        let o = question_outcome(&q, &entry, &json!({ "type": "tool_result" }));
        assert_eq!(
            o,
            Outcome::Answered {
                answers: vec![Some("Dog, Fish".into()), Some("Rome".into())],
                picked: vec![vec![0, 2], vec![1]],
            }
        );
        assert_eq!(
            o.to_json(),
            json!({ "answers": ["Dog, Fish", "Rome"], "picked": [[0, 2], [1]] })
        );
        let rejected = question_outcome(
            &q,
            &json!({ "toolUseResult": "User rejected tool use" }),
            &json!({ "is_error": true }),
        );
        assert_eq!(rejected, Outcome::Declined);
        assert_eq!(rejected.to_json(), json!({ "declined": true }));
        let partial = question_outcome(
            &q,
            &json!({ "toolUseResult": { "answers": { "Which city?": "Oslo" } } }),
            &json!({}),
        );
        assert_eq!(
            partial,
            Outcome::Answered {
                answers: vec![None, Some("Oslo".into())],
                picked: vec![vec![], vec![2]],
            }
        );
    }

    fn keys(steps: &[Step]) -> String {
        steps
            .iter()
            .map(|s| match s {
                Step::Key(k) => k.to_string(),
                Step::Text(t) => format!("\"{t}\""),
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn ans(v: Value, q: &[Question]) -> Vec<Answer> {
        validate_answer(&v, q).unwrap()
    }

    #[test]
    fn steps_single_select_pick_by_arrows_and_a_lone_question_needs_no_review() {
        let q = qs();
        let one = &q[1..];
        assert_eq!(
            keys(&answer_steps(one, &ans(json!([{ "options": [0] }]), one))),
            "Enter"
        );
        assert_eq!(
            keys(&answer_steps(one, &ans(json!([{ "options": [2] }]), one))),
            "Down Down Enter"
        );
        assert_eq!(
            keys(&answer_steps(one, &ans(json!([{ "text": "Zagreb" }]), one))),
            "Down Down Down \"Zagreb\" Enter"
        );
    }

    #[test]
    fn steps_multi_select_toggle_type_next_and_review() {
        let q = qs();
        let steps = answer_steps(
            &q,
            &ans(
                json!([{ "options": [0, 2], "text": "hamster" }, { "text": "Zagreb, obviously" }]),
                &q,
            ),
        );
        assert_eq!(
            keys(&steps),
            [
                "Enter Down Down Enter Down \"hamster\" Down Enter",
                "Down Down Down \"Zagreb, obviously\" Enter",
                "Enter"
            ]
            .join(" ")
        );
        assert_eq!(
            keys(&answer_steps(
                &q,
                &ans(json!([{ "options": [1] }, { "options": [1] }]), &q)
            )),
            "Down Enter Down Down Down Enter Down Enter Enter"
        );
    }

    #[test]
    fn validate_enforces_one_pick_for_single_select_at_least_one_for_multi() {
        let q = qs();
        let bad = |v: Value| validate_answer(&v, &q).is_err();
        assert!(bad(json!([{ "options": [0] }])), "one entry per question");
        assert!(
            bad(json!([{ "options": [] }, { "options": [0] }])),
            "multi needs something"
        );
        assert!(
            bad(json!([{ "options": [0] }, { "options": [0, 1] }])),
            "single takes one"
        );
        assert!(
            bad(json!([{ "options": [0] }, { "options": [0], "text": "x" }])),
            "option or text"
        );
        assert!(
            bad(json!([{ "options": [3] }, { "options": [0] }])),
            "unknown option"
        );
        assert!(
            bad(json!([{ "options": [-1] }, { "options": [0] }])),
            "negative option"
        );
        assert!(
            bad(json!([{ "text": "a\nb" }, { "options": [0] }])),
            "one line"
        );
        assert!(
            bad(json!([{ "text": "x".repeat(2001) }, { "options": [0] }])),
            "too long"
        );
        assert!(bad(json!({ "answers": [] })), "an array");
        assert_eq!(
            validate_answer(
                &json!([{ "options": [2, 0, 2] }, { "text": " Zagreb " }]),
                &q
            ),
            Ok(vec![
                Answer {
                    options: vec![0, 2],
                    text: None
                },
                Answer {
                    options: vec![],
                    text: Some("Zagreb".into())
                },
            ])
        );
    }

    #[test]
    fn typed_text_cannot_carry_control_characters_or_escape_sequences() {
        let q = qs();
        for bad in [
            "Os\u{1b}[Blo", // ESC + an arrow key
            "\u{1b}",       // a lone ESC
            "Oslo\u{7f}",   // DEL
            "Os\u{9b}Blo",  // C1 CSI
            "Os\u{0}lo",    // NUL
            "Os\tlo",       // TAB would move the TUI's focus
            "\u{1b} Oslo",  // before trimming too
        ] {
            let e = validate_answer(&json!([{ "options": [0] }, { "text": bad }]), &q).unwrap_err();
            assert!(e.contains("control characters"), "{bad:?}: {e}");
        }
        assert!(
            validate_answer(&json!([{ "options": [0] }, { "text": "a\nb" }]), &q)
                .unwrap_err()
                .contains("single line")
        );
        assert!(
            validate_answer(
                &json!([{ "options": [0] }, { "text": " Zürich – ok " }]),
                &q
            )
            .is_ok()
        );
    }

    #[test]
    fn the_plan_says_what_the_form_shows_before_each_key() {
        let q = qs();
        let plan = answer_plan(
            &q,
            &ans(
                json!([{ "options": [1], "text": "hamster" }, { "options": [0] }]),
                &q,
            ),
        );
        let form = |question, row, ticked: &[usize], typed: Option<&str>| At::Form {
            question,
            row,
            ticked: ticked.to_vec(),
            typed: typed.map(str::to_string),
            done: vec![question > 0 || !ticked.is_empty(), false],
        };
        let at: Vec<At> = plan.iter().map(|p| p.at.clone()).collect();
        assert_eq!(
            at,
            [
                form(0, 0, &[], None),                // Down to Cat
                form(0, 1, &[], None),                // Enter: tick Cat
                form(0, 1, &[1], None),               // Down
                form(0, 2, &[1], None),               // Down to "Type something"
                form(0, 3, &[1], None),               // type "hamster"
                form(0, 3, &[1, 3], Some("hamster")), // Down to Next
                form(0, 4, &[1, 3], Some("hamster")), // Enter: next question
                form(1, 0, &[], None),                // Enter: Paris
                At::Review {
                    expect: vec![
                        ("Which pets do you like?".into(), "Cat, hamster".into()),
                        ("Which city?".into(), "Paris".into()),
                    ]
                }, // Enter: submit
            ]
        );
        assert_eq!(
            keys(&answer_steps(
                &q,
                &ans(
                    json!([{ "options": [1], "text": "hamster" }, { "options": [0] }]),
                    &q
                )
            )),
            "Down Enter Down Down \"hamster\" Down Enter Enter Enter"
        );
    }

    #[test]
    fn picked_matches_labels_longest_first() {
        let q = normalize_questions(
            &json!({ "questions": [{ "question": "Q", "multiSelect": true,
            "options": [{ "label": "Red" }, { "label": "Red, white" }, { "label": "Blue" }] }] }),
        )
        .unwrap()
        .remove(0);
        assert_eq!(picked(&q, "Red, white"), vec![1]);
        assert_eq!(picked(&q, "Red, white, Blue"), vec![1, 2]);
        assert_eq!(picked(&q, "Red, Blue"), vec![0, 2]);
        assert_eq!(picked(&q, "Blue, Red"), vec![0, 2]);
        // Typed text alongside picks: only the picks count; typed alone selects nothing.
        assert_eq!(picked(&q, "Blue, teal"), vec![2]);
        assert_eq!(picked(&q, "teal"), Vec::<usize>::new());
        let single = normalize_questions(&json!({ "questions": [{ "question": "Q",
            "options": [{ "label": "Yes" }, { "label": "No" }] }] }))
        .unwrap()
        .remove(0);
        assert_eq!(picked(&single, "No"), vec![1]);
        assert_eq!(picked(&single, "Yes, No"), Vec::<usize>::new());
        assert_eq!(picked(&single, "maybe later"), Vec::<usize>::new());
    }
}
