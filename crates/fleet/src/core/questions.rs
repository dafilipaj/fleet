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
