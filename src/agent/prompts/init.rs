//! What `molt init` says to the model in the Q&A (PRD UC1, §8.7): the `reason` model makes the
//! questions and the `light` model judges the answers.

use crate::profile::element::{Category, Element};
use crate::repo_scan::RepoScan;

/// The system prompt of the questions (the `reason` model).
pub const QUESTION_SYSTEM: &str = "\
You are molt, setting up the user's comprehension profile of Rust: which syntax elements the user understands well enough to read and maintain code that uses them.
The user said they have written code with the elements of one category. Ask them one free-text question whose answer shows, for every element listed, whether they have used it and understand it: for example, ask how and where they used each one, or how they would choose between them.
Ask one question only. Do not explain the elements and do not hint at the answers. Reply with the question only.
Answer in the language of the user's latest input.";

/// The system prompt of the judgement (the `light` model).
pub const JUDGE_SYSTEM: &str = "\
You are molt, judging from the user's own answers which Rust syntax elements the user understands well enough to read and maintain code that uses them.
For each element listed, answer `known` when the answers show that the user has used it and understands it, `unknown` when they show that the user has not or does not, and `unclear` when they do not say enough about it.
Judge only from what the user wrote: an element the answers do not mention is `unclear`, not `known`. Give a short reason for each.
Answer in the language of the user's latest input.";

/// One question molt asked and the user's answer to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exchange {
    pub question: String,
    pub answer: String,
}

/// What the `reason` model is asked for the question about `elements` of `category`: the first
/// one when `exchanges` is empty, otherwise the follow-up after them. `scan` gives the places the
/// reference repository uses each element, and `latest` is the user's latest input, quoted as
/// the clue to the language the system prompt asks for.
pub fn question_request(
    category: Category,
    elements: &[Element],
    scan: Option<&RepoScan>,
    exchanges: &[Exchange],
    latest: Option<&str>,
) -> String {
    let mut text = format!(
        "The category is `{category}`. Its elements to ask about:\n{}\n",
        element_list(elements, scan)
    );
    if scan.is_some() {
        text.push_str(
            "\nThe places are where the user's reference repository uses each element. You may \
             ask about them.\n",
        );
    }
    if exchanges.is_empty() {
        text.push_str("\nAsk the one question that covers all of these elements.");
    } else {
        text.push_str(&format!(
            "\n{}\nThe answers did not make these elements clear. Ask one follow-up question \
             that covers all of them.",
            transcript(exchanges)
        ));
    }
    // The language is left to the system prompt, with the input quoted as the clue: naming it
    // here led small models to answer in another language (see the module doc of `prompts`).
    if let Some(latest) = latest {
        text.push_str(&format!("\n\nThe user's latest input was:\n{latest}"));
    }
    text
}

/// What the `light` model is asked to judge `elements` of `category` from `exchanges`.
pub fn judge_request(category: Category, elements: &[Element], exchanges: &[Exchange]) -> String {
    let names = elements
        .iter()
        .map(|element| format!("- `{}`", element.name()))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "The category is `{category}`. Judge these elements:\n{names}\n\n{}\nGive one judgement \
         per element listed.",
        transcript(exchanges)
    )
}

/// `` - `arc`: used at src/a.rs:3, src/b.rs:9 `` per element, with the places only when `scan` is
/// given.
fn element_list(elements: &[Element], scan: Option<&RepoScan>) -> String {
    elements
        .iter()
        .map(|element| {
            let name = element.name();
            let Some(scan) = scan else {
                return format!("- `{name}`");
            };
            match scan.elements.get(name).filter(|places| !places.is_empty()) {
                Some(places) => {
                    let places = places
                        .iter()
                        .map(|place| format!("{}:{}", place.path, place.line))
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("- `{name}`: used at {places}")
                }
                None => format!("- `{name}`: not used in the reference repository"),
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn transcript(exchanges: &[Exchange]) -> String {
    exchanges
        .iter()
        .map(|exchange| {
            format!(
                "Question: {}\nThe user's answer: {}\n",
                exchange.question, exchange.answer
            )
        })
        .collect()
}
