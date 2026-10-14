//! The choice on the side line (PRD UC3): `/choose` lists the choices the side line offers by
//! number, and the one chosen goes back to the main line (§8.5).

use super::names;
use crate::agent::subline::{Choice, SubLine};
use crate::profile::status::ItemKey;
use crate::ui::{Ui, UiError};

/// What the choice is asked with.
const ASK: &str = "How do you want to go on?";

/// Lets the user choose one of the choices of `side`. `None` when the user interrupted instead.
pub(super) fn choose(ui: &mut dyn Ui, side: &SubLine) -> Result<Option<Choice>, UiError> {
    let items = &side.boundary().items;
    let choices = side.choices();
    let labels: Vec<String> = choices.iter().map(|c| label(*c, items)).collect();
    let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
    Ok(ui.select(ASK, &labels)?.map(|at| choices[at]))
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

/// ① and ② on the side line through `/choose`, and what the main line does with them. Named so
/// that `cargo test subline::resolve` selects them.
#[cfg(test)]
mod tests;
