//! The judgement of `molt init` (PRD UC1): after the form is submitted, each category sent is
//! judged from its answer by the `light` model, as JSON, at most [`PARALLEL`] at a time (PRD §8.4,
//! §8.7).
//!
//! An element is judged `known` only from the user's answer: one the answer leaves unclear, or
//! that cannot be judged because the judgement does not fit the schema even after the repair, is
//! `unknown`. An API failure or an interruption stops the whole judgement, and nothing judged is
//! returned.

#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;

use serde::Deserialize;
use serde_json::{Value, json};

use super::form::CategoryAnswer;
use super::prompts::{self, Exchange};
use crate::llm::{ChatClient, ChatError, JsonError, Message, Role, Router};
use crate::profile::element::{Category, Element};
use crate::ui::Ui;

/// The judgement requests sent at a time (PRD §8.4).
pub(crate) const PARALLEL: usize = 3;

/// The endpoint the judgement talks to and the roles' models on it.
#[derive(Clone, Copy)]
pub struct Model<'a> {
    pub client: &'a dyn ChatClient,
    pub router: &'a Router,
}

/// The judgement of one element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Judgement {
    pub element: Element,
    /// Whether the answer shows the element is known. Otherwise it stays `unknown`.
    pub known: bool,
    /// Why, in the judging model's words. `None` when the element was not judged.
    pub reason: Option<String>,
}

/// How the judgement failed: the user interrupted, or the API failed (the cause, for the user).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Stopped {
    Interrupted,
    Failed(String),
}

/// Judges the checked elements of each of `answers` (the categories sent) and tells the user the
/// result per category, in the order of `answers`.
pub(crate) fn judge(
    ui: &mut dyn Ui,
    model: Model<'_>,
    answers: &[&CategoryAnswer],
) -> Result<Vec<Judgement>, Stopped> {
    let results = judge_all(model, answers);

    // An interruption wins over a failure that came with it: the user asked to stop.
    let mut failure = None;
    for (answer, result) in answers.iter().zip(&results) {
        match result {
            Some(Err(JsonError::Chat(ChatError::Interrupted))) => return Err(Stopped::Interrupted),
            Some(Err(JsonError::Chat(error))) if failure.is_none() => {
                failure = Some(format!(
                    "the judgement of {} failed: {error}",
                    answer.category
                ));
            }
            _ => {}
        }
    }
    if let Some(cause) = failure {
        return Err(Stopped::Failed(cause));
    }

    let mut judgements = Vec::new();
    for (answer, result) in answers.iter().zip(results) {
        let verdicts = match result {
            Some(Ok(verdicts)) => verdicts,
            Some(Err(JsonError::Invalid { reason, .. })) => {
                ui.show(&format!(
                    "Could not judge the answer about {} ({reason}); its checked elements stay unknown.",
                    answer.category
                ));
                BTreeMap::new()
            }
            // Only left unjudged when another one failed, which returned above.
            Some(Err(JsonError::Chat(_))) | None => BTreeMap::new(),
        };
        let judged = judgements_of(answer, verdicts);
        ui.show(&summary(answer.category, &judged));
        judgements.extend(judged);
    }
    Ok(judgements)
}

/// The verdicts per element of one category.
type Verdicts = BTreeMap<Element, (Verdict, String)>;

/// The result of judging each of `answers`, in their order: at most [`PARALLEL`] requests at a
/// time, and none started after one failed with the API (`None` for those).
fn judge_all(
    model: Model<'_>,
    answers: &[&CategoryAnswer],
) -> Vec<Option<Result<Verdicts, JsonError>>> {
    let next = AtomicUsize::new(0);
    let failed = AtomicBool::new(false);
    let results: Vec<Mutex<Option<Result<Verdicts, JsonError>>>> =
        answers.iter().map(|_| Mutex::new(None)).collect();
    thread::scope(|scope| {
        for _ in 0..PARALLEL.min(answers.len()) {
            scope.spawn(|| {
                while !failed.load(Ordering::SeqCst) {
                    let at = next.fetch_add(1, Ordering::SeqCst);
                    let Some(answer) = answers.get(at) else {
                        break;
                    };
                    let result = judge_one(model, answer);
                    if matches!(result, Err(JsonError::Chat(_))) {
                        failed.store(true, Ordering::SeqCst);
                    }
                    *lock(&results[at]) = Some(result);
                }
            });
        }
    });
    results
        .into_iter()
        .map(|result| {
            result
                .into_inner()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
        })
        .collect()
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Judges the checked elements of `answer` with the `light` model. An element the reply leaves
/// out, or names that are not among them, count as unclear.
fn judge_one(model: Model<'_>, answer: &CategoryAnswer) -> Result<Verdicts, JsonError> {
    let exchange = Exchange {
        question: answer.question.clone(),
        answer: answer.answer.clone(),
    };
    let messages = vec![
        Message::System {
            content: prompts::JUDGE_SYSTEM.to_owned(),
        },
        Message::User {
            content: prompts::judge_request(answer.category, &answer.checked, &[exchange]),
        },
    ];
    let judged: Judged = model.router.chat_json(
        model.client,
        Role::Light,
        messages,
        "judgement",
        &schema(&answer.checked),
    )?;
    Ok(judged
        .elements
        .into_iter()
        .filter_map(|judged| {
            let element = answer
                .checked
                .iter()
                .copied()
                .find(|element| element.name() == judged.element)?;
            Some((element, (judged.verdict, judged.reason)))
        })
        .collect())
}

/// Every element of `answer`'s category: the checked ones as `verdicts` say, the others unknown
/// and not judged.
fn judgements_of(answer: &CategoryAnswer, mut verdicts: Verdicts) -> Vec<Judgement> {
    answer
        .category
        .elements()
        .map(|element| match verdicts.remove(&element) {
            Some((Verdict::Unclear, _)) | None => Judgement {
                element,
                known: false,
                reason: None,
            },
            Some((verdict, reason)) => Judgement {
                element,
                known: verdict == Verdict::Known,
                reason: Some(reason),
            },
        })
        .collect()
}

/// What the judgement says of each element.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Verdict {
    Known,
    Unknown,
    Unclear,
}

/// The JSON the `light` model judges with.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Judged {
    elements: Vec<JudgedElement>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct JudgedElement {
    element: String,
    verdict: Verdict,
    reason: String,
}

/// The JSON Schema of the judgement of `elements`.
fn schema(elements: &[Element]) -> Value {
    let names: Vec<&str> = elements.iter().map(|element| element.name()).collect();
    json!({
        "type": "object",
        "properties": {
            "elements": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "element": {"type": "string", "enum": names},
                        "verdict": {"type": "string", "enum": ["known", "unknown", "unclear"]},
                        "reason": {"type": "string"},
                    },
                    "required": ["element", "verdict", "reason"],
                    "additionalProperties": false,
                },
            },
        },
        "required": ["elements"],
        "additionalProperties": false,
    })
}

/// `ownership: known: clone, to_owned; unknown: box`.
fn summary(category: Category, judgements: &[Judgement]) -> String {
    let names = |known: bool| {
        let names: Vec<_> = judgements
            .iter()
            .filter(|judgement| judgement.known == known)
            .map(|judgement| judgement.element.name())
            .collect();
        if names.is_empty() {
            "none".to_owned()
        } else {
            names.join(", ")
        }
    };
    format!(
        "{category}: known: {}; unknown: {}",
        names(true),
        names(false)
    )
}
