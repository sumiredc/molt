//! The form step of a ui script for `molt init`, for the tests of `onboarding`.

use crate::profile::element::{Category, Element};
use crate::profile::store::Report;
use crate::ui::{FormTabStep, Step};

/// The form submitted with `given` (category, report, checked elements, answer), and "don't know"
/// for every other category.
pub(crate) fn form(given: &[(Category, Report, &[Element], &str)]) -> Step {
    Step::Form(
        Category::ALL
            .iter()
            .map(
                |&category| match given.iter().find(|(c, ..)| *c == category) {
                    Some(&(_, report, checked, answer)) => tab(category, report, checked, answer),
                    None => tab(category, Report::DontKnow, &[], ""),
                },
            )
            .collect(),
    )
}

/// The form submitted with `report` for every category, nothing checked.
pub(crate) fn every(report: Report) -> Step {
    Step::Form(
        Category::ALL
            .iter()
            .map(|&category| tab(category, report, &[], ""))
            .collect(),
    )
}

/// One tab as a script gives it: the report and the checked elements numbered from 1.
pub(crate) fn tab(
    category: Category,
    report: Report,
    checked: &[Element],
    answer: &str,
) -> FormTabStep {
    let elements: Vec<Element> = category.elements().collect();
    FormTabStep {
        choice: match report {
            Report::HaveWritten => 1,
            Report::CanRead => 2,
            Report::DontKnow => 3,
        },
        checked: checked
            .iter()
            .map(|element| {
                1 + elements
                    .iter()
                    .position(|e| e == element)
                    .unwrap_or_else(|| panic!("{} is not in {category}", element.name()))
            })
            .collect(),
        answer: answer.to_owned(),
    }
}
