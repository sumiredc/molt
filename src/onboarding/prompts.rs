//! What `molt init` says to the model (PRD UC1, §8.7): the `light` model judges the answers. The
//! questions are fixed, in [`super::form`].

use crate::profile::element::{Category, Element};

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

/// What the `light` model is asked to judge `elements` of `category` from `exchanges`. The last
/// answer is in the transcript, and is quoted again at the end as the user's latest input, the
/// clue to the language the system prompt asks for: with the transcript alone, a small model
/// gave the reasons for a Japanese answer in another language (#133).
pub fn judge_request(category: Category, elements: &[Element], exchanges: &[Exchange]) -> String {
    let names = elements
        .iter()
        .map(|element| format!("- `{}`", element.name()))
        .collect::<Vec<_>>()
        .join("\n");
    let mut text = format!(
        "The category is `{category}`. Judge these elements:\n{names}\n\n{}\nGive one judgement \
         per element listed.",
        transcript(exchanges)
    );
    if let Some(latest) = exchanges.last() {
        text.push_str(&latest_input(&latest.answer));
    }
    text
}

/// The user's latest input, quoted at the end of a request.
fn latest_input(latest: &str) -> String {
    format!("\n\nThe user's latest input was:\n{latest}")
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
