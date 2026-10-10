//! The confirmation of `molt init` (PRD UC1): before the profile is saved, the user sees it per
//! category and may correct any item, in the form of `molt profile set <key> <status>`. A
//! correction is a transition by `manual`, like `molt profile set`.

#[cfg(test)]
mod tests;

use std::fmt::Write as _;

use crate::cli::profile::parse_setting;
use crate::profile::element::{Category, Element};
use crate::profile::status::{ItemKey, Provenance};
use crate::profile::store::{Profile, Timestamp};
use crate::ui::{Input, Ui};

/// The prompt a correction is typed at.
const PROMPT: &str = "correct> ";

/// How the confirmation ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Confirmed {
    /// The user is done; save the profile.
    Save,
    /// The user interrupted (Ctrl-C, or Ctrl-D); save nothing.
    Interrupted,
}

/// Shows `profile` and takes corrections until an empty line. Each correction changes `profile`
/// at once, so what is saved is what was shown and corrected. An invalid correction is shown why
/// and changes nothing.
pub(crate) fn confirm(ui: &mut dyn Ui, profile: &mut Profile) -> Result<Confirmed, String> {
    ui.show(&table(profile));
    ui.show(
        "To correct an item, type `<key> <status>` as for `molt profile set` (e.g. `clone unknown`). \
         An empty line saves the profile.",
    );
    loop {
        let line = match ui.read_line(PROMPT).map_err(|error| error.to_string())? {
            Input::Line(line) => line,
            Input::Interrupted | Input::Eof => return Ok(Confirmed::Interrupted),
        };
        let words: Vec<&str> = line.split_whitespace().collect();
        match words[..] {
            [] => return Ok(Confirmed::Save),
            [key, status] => match parse_setting(key, status) {
                Ok((key, status)) => {
                    match profile.transition(
                        key.clone(),
                        status,
                        Provenance::Manual,
                        Timestamp::now(),
                        None,
                    ) {
                        Ok(()) => ui.show(&format!("{key}: {status} ({})", Provenance::Manual)),
                        // `manual` may set any status (PRD §7), so this is not reached today;
                        // a rejection would be shown and change nothing, as a bad input is.
                        Err(error) => ui.show(&error.to_string()),
                    }
                }
                Err(message) => ui.show(message.trim_end()),
            },
            _ => {
                ui.show("Type a key and a status, e.g. `clone unknown`, or an empty line to save.")
            }
        }
    }
}

/// The profile per category: each element that is not a base element with its status, its
/// provenance and, for the Q&A, the reason of the judgement; then the crates, if there are any (with
/// no `--repo` there are none), under the reference repository they came from (the same for every
/// crate, so shown once); then the base elements.
fn table(profile: &Profile) -> String {
    let width = Element::ALL
        .iter()
        .map(|element| element.name().len())
        .chain(profile.entries().map(|(key, _)| key.to_string().len()))
        .max()
        .unwrap_or(0);
    let row = |text: &mut String, key: &ItemKey| {
        let entry = profile.entry(key);
        let provenance = entry.map_or("-", |entry| entry.provenance().name());
        let evidence = entry
            .filter(|entry| entry.provenance() == Provenance::Qa)
            .and_then(|entry| entry.evidence())
            .unwrap_or("");
        let line = format!(
            "  {:width$}  {:7}  {provenance:14}  {evidence}",
            key.to_string(),
            profile.status(key).name(),
        );
        writeln!(text, "{}", line.trim_end()).unwrap();
    };

    let mut text = String::from("The profile to save:\n");
    for category in Category::ALL {
        match profile.report(category) {
            Some(report) => writeln!(text, "{category} (report: {report})").unwrap(),
            None => writeln!(text, "{category}").unwrap(),
        }
        let elements = Element::ALL
            .iter()
            .copied()
            .filter(|&element| element.category() == category && !profile.is_base(element));
        for element in elements {
            row(&mut text, &ItemKey::Element(element));
        }
    }
    let crates: Vec<&ItemKey> = profile
        .entries()
        .map(|(key, _)| key)
        .filter(|key| matches!(key, ItemKey::Crate(_)))
        .collect();
    if !crates.is_empty() {
        let repo = profile
            .entries()
            .find(|(_, entry)| entry.provenance() == Provenance::ReferenceRepo)
            .and_then(|(_, entry)| entry.evidence());
        match repo {
            Some(repo) => writeln!(text, "crates (reference_repo: {repo})").unwrap(),
            None => writeln!(text, "crates").unwrap(),
        }
        for key in crates {
            row(&mut text, key);
        }
    }
    writeln!(text, "base elements (used regardless of status)").unwrap();
    for &element in Element::ALL.iter().filter(|&&e| profile.is_base(e)) {
        row(&mut text, &ItemKey::Element(element));
    }
    text.trim_end().to_owned()
}
