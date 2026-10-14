//! The choices on the side line (PRD UC3, §8.6): listed under the explanation, numbered ① to ④,
//! and the one chosen goes back to the main line (§8.5).

use super::names;
use crate::agent::subline::{Choice, SubLine};
use crate::profile::status::ItemKey;

/// What the choices are listed under.
pub(super) const ASK: &str = "How do you want to go on?";

/// The choices `side` offers now, each with its number and what it says.
pub(super) fn offered(side: &SubLine) -> Vec<(Choice, String)> {
    let items = &side.boundary().items;
    side.choices()
        .into_iter()
        .map(|choice| (choice, label(choice, items)))
        .collect()
}

fn label(choice: Choice, items: &[ItemKey]) -> String {
    let names = names(items);
    match choice {
        Choice::Unlock => format!("Understood: the step stays, and molt uses {names} from now on"),
        Choice::Rewrite => {
            format!("Write it without them: molt takes the step back and writes it without {names}")
        }
        Choice::UserWrote => "Write it myself: molt takes the step back, shows where, and checks \
             what I wrote when I type /done"
            .to_owned(),
        Choice::Next => {
            format!("Next: the step stays, and molt explains {names} again when it uses them")
        }
    }
}

/// What the side line says when its boundary recurred after ② in the same turn (UC3).
pub(super) fn recurred(items: &[ItemKey]) -> String {
    format!(
        "This step uses {names} again after you chose to write without them in this turn: the \
         request seems to need them, so writing without them is not offered again.",
        names = names(items)
    )
}

/// ① and ② at the choices of the side line, and what the main line does with them. Named so
/// that `cargo test subline::resolve` selects them.
#[cfg(test)]
mod tests;
