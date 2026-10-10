//! The Q&A of `molt init` (PRD UC1): for each category reported "have written", questions that
//! cover its elements and a judgement of each element from the answers.
//!
//! The budget is 1 question per category and at most 1 follow-up (PRD §8.4). The `reason` model
//! makes the questions and the `light` model judges the answers as JSON (PRD §8.7). An element is
//! judged `known` only from the user's answers: one the answers leave unclear, or that cannot be
//! judged because the judgement does not fit the schema even after the repair, is `unknown`.

#[cfg(test)]
mod tests;

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::agent::prompts::init::{self as prompts, Exchange};
use crate::llm::{ChatClient, ChatError, JsonError, Message, Role, Router};
use crate::profile::element::{Category, Element};
use crate::repo_scan::RepoScan;
use crate::ui::{Input, Ui};

/// The questions asked per category at most: 1 question and 1 follow-up (PRD §8.4).
pub(crate) const QUESTIONS_PER_CATEGORY: usize = 2;

/// The prompt the user answers a question at.
const ANSWER_PROMPT: &str = "answer> ";

/// The endpoint the Q&A talks to and the roles' models on it.
#[derive(Clone, Copy)]
pub struct Model<'a> {
    pub client: &'a dyn ChatClient,
    pub router: &'a Router,
}

/// The judgement of one element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Judgement {
    pub element: Element,
    /// Whether the answers show the element is known. Otherwise it stays `unknown`.
    pub known: bool,
    /// Why, in the judging model's words. `None` when the element was not judged.
    pub reason: Option<String>,
}

/// How the Q&A failed: the user interrupted, or the API failed (the cause, for the user).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Stopped {
    Interrupted,
    Failed(String),
}

/// Asks about each of `categories` in turn and judges every element of them. `scan` is the
/// stock-taking of the reference repository, given to the questions as material.
///
/// An interruption or an API failure (authentication, rate limit, network) stops the Q&A, and
/// nothing judged so far is returned.
pub(crate) fn probe(
    ui: &mut dyn Ui,
    model: Model<'_>,
    categories: &[Category],
    scan: Option<&RepoScan>,
) -> Result<Vec<Judgement>, Stopped> {
    let mut latest: Option<String> = None;
    let mut judgements = Vec::new();
    for &category in categories {
        judgements.extend(probe_category(ui, model, category, scan, &mut latest)?);
    }
    Ok(judgements)
}

fn probe_category(
    ui: &mut dyn Ui,
    model: Model<'_>,
    category: Category,
    scan: Option<&RepoScan>,
    latest: &mut Option<String>,
) -> Result<Vec<Judgement>, Stopped> {
    let elements: Vec<Element> = Element::ALL
        .iter()
        .copied()
        .filter(|element| element.category() == category)
        .collect();
    let mut open = elements.clone();
    let mut judged: BTreeMap<Element, (bool, String)> = BTreeMap::new();
    let mut exchanges: Vec<Exchange> = Vec::new();

    ui.show(&format!("About {category}:"));
    for _ in 0..QUESTIONS_PER_CATEGORY {
        let request =
            prompts::question_request(category, &open, scan, &exchanges, latest.as_deref());
        let Some(question) = ask_question(model, request).map_err(|e| stopped(category, e))? else {
            ui.show(&format!(
                "The model gave no question about {category}; its remaining elements stay unknown."
            ));
            break;
        };
        ui.show(&question);
        let answer = match ui.read_line(ANSWER_PROMPT) {
            Ok(Input::Line(answer)) => answer,
            Ok(Input::Interrupted | Input::Eof) => return Err(Stopped::Interrupted),
            Err(error) => return Err(Stopped::Failed(error.to_string())),
        };
        *latest = Some(answer.clone());
        exchanges.push(Exchange { question, answer });

        match judge(model, category, &open, &exchanges) {
            Ok(verdicts) => {
                for (element, verdict) in verdicts {
                    match verdict.verdict {
                        Verdict::Known => {
                            judged.insert(element, (true, verdict.reason));
                        }
                        Verdict::Unknown => {
                            judged.insert(element, (false, verdict.reason));
                        }
                        Verdict::Unclear => {}
                    }
                }
                open.retain(|element| !judged.contains_key(element));
            }
            Err(JsonError::Chat(error)) => return Err(stopped(category, error)),
            Err(JsonError::Invalid { reason, .. }) => {
                ui.show(&format!(
                    "Could not judge the answers about {category} ({reason}); its remaining elements stay unknown."
                ));
                break;
            }
        }
        if open.is_empty() {
            break;
        }
    }

    let judgements: Vec<Judgement> = elements
        .into_iter()
        .map(|element| match judged.remove(&element) {
            Some((known, reason)) => Judgement {
                element,
                known,
                reason: Some(reason),
            },
            None => Judgement {
                element,
                known: false,
                reason: None,
            },
        })
        .collect();
    ui.show(&summary(category, &judgements));
    Ok(judgements)
}

/// The question the `reason` model makes from `request`, or `None` when it gives no text.
fn ask_question(model: Model<'_>, request: String) -> Result<Option<String>, ChatError> {
    let messages = vec![
        Message::System {
            content: prompts::QUESTION_SYSTEM.to_owned(),
        },
        Message::User { content: request },
    ];
    let response = model
        .client
        .chat(&model.router.request(Role::Reason, messages))?;
    Ok(response
        .content
        .map(|content| content.trim().to_owned())
        .filter(|content| !content.is_empty()))
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

struct ElementVerdict {
    verdict: Verdict,
    reason: String,
}

/// Judges `elements` of `category` from `exchanges` with the `light` model. An element the reply
/// leaves out, or names that are not among `elements`, count as unclear.
fn judge(
    model: Model<'_>,
    category: Category,
    elements: &[Element],
    exchanges: &[Exchange],
) -> Result<Vec<(Element, ElementVerdict)>, JsonError> {
    let messages = vec![
        Message::System {
            content: prompts::JUDGE_SYSTEM.to_owned(),
        },
        Message::User {
            content: prompts::judge_request(category, elements, exchanges),
        },
    ];
    let judged: Judged = model.router.chat_json(
        model.client,
        Role::Light,
        messages,
        "judgement",
        &schema(elements),
    )?;
    Ok(judged
        .elements
        .into_iter()
        .filter_map(|judged| {
            let element = elements
                .iter()
                .copied()
                .find(|element| element.name() == judged.element)?;
            Some((
                element,
                ElementVerdict {
                    verdict: judged.verdict,
                    reason: judged.reason,
                },
            ))
        })
        .collect())
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

fn stopped(category: Category, error: ChatError) -> Stopped {
    match error {
        ChatError::Interrupted => Stopped::Interrupted,
        error => Stopped::Failed(format!("the Q&A about {category} failed: {error}")),
    }
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
