//! The start of `molt init` (PRD UC1): the checks before anything is asked, and the user's report
//! per category.

#[cfg(test)]
mod tests;

use std::fs;
use std::path::Path;

use crate::cli::ui::Ui;
use crate::llm::ChatClient;
use crate::profile::element::{Category, Element};
use crate::profile::status::{ItemKey, Provenance, Status};
use crate::profile::store::{Profile, Report, Timestamp};

/// The answers offered for each category, in the order of [`REPORTS`].
const ANSWERS: [&str; 3] = ["have written", "can read", "don't know"];

/// The report each of [`ANSWERS`] stands for.
const REPORTS: [Report; 3] = [Report::HaveWritten, Report::CanRead, Report::DontKnow];

/// How `molt init` ended without an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The new profile was saved.
    Saved,
    /// A profile already existed and the user declined to overwrite it. Nothing changed.
    Kept,
    /// The user interrupted. Nothing was saved.
    Interrupted,
}

/// Runs `molt init` up to saving the profile at `profile_path`.
///
/// `client` is the result of creating the client for the configured endpoint. When it could not
/// be created (e.g. the endpoint needs an API key and its environment variable is unset), nothing
/// is asked and no profile is created. Otherwise an existing profile is overwritten only when the
/// user says yes, and the user reports on each category. The model is called only for the
/// categories reported "have written".
///
/// An error leaves the profile as it was.
pub fn init(
    ui: &mut dyn Ui,
    profile_path: &Path,
    client: Result<&dyn ChatClient, String>,
) -> Result<Outcome, String> {
    // The Q&A (#67) asks the categories reported "have written" through this client.
    let _client = client?;

    if profile_path.exists() {
        let question = format!(
            "A profile already exists at {}. Overwrite it?",
            profile_path.display()
        );
        match ui.confirm(&question).map_err(|error| error.to_string())? {
            Some(true) => {}
            Some(false) => {
                ui.show("Kept the existing profile.");
                return Ok(Outcome::Kept);
            }
            None => return Ok(interrupted(ui)),
        }
    }

    let Some(reports) = ask(ui)? else {
        return Ok(interrupted(ui));
    };

    let have_written: Vec<_> = reports
        .iter()
        .filter(|(_, report)| *report == Report::HaveWritten)
        .map(|(category, _)| category.name())
        .collect();
    if !have_written.is_empty() {
        // TODO(#67): ask about these categories and judge their elements from the answers.
        return Err(format!(
            "the questions for the categories reported \"have written\" ({}) are not implemented yet; no profile was saved",
            have_written.join(", ")
        ));
    }

    let profile = from_reports(&reports, Timestamp::now())?;
    save(&profile, profile_path)?;
    ui.show(&format!("Saved the profile to {}.", profile_path.display()));
    Ok(Outcome::Saved)
}

fn interrupted(ui: &mut dyn Ui) -> Outcome {
    ui.show("Interrupted. The profile was not changed.");
    Outcome::Interrupted
}

/// Asks the user, category by category, to report how well they know it. Returns `None` when the
/// user interrupted.
fn ask(ui: &mut dyn Ui) -> Result<Option<Vec<(Category, Report)>>, String> {
    ui.show("For each category, choose how well you know it.");
    let mut reports = Vec::with_capacity(Category::ALL.len());
    for category in Category::ALL {
        let names: Vec<_> = elements_of(category).map(Element::name).collect();
        ui.show(&format!("{category}: {}", names.join(", ")));
        let prompt = format!("How well do you know {category}?");
        match ui
            .select(&prompt, &ANSWERS)
            .map_err(|error| error.to_string())?
        {
            Some(index) => reports.push((category, REPORTS[index])),
            None => return Ok(None),
        }
    }
    Ok(Some(reports))
}

/// A new profile with `reports`: the elements of a category reported "can read" are `seen`, and
/// those of one reported "don't know" stay `unknown` (no entry). Changes made at `at`.
fn from_reports(reports: &[(Category, Report)], at: Timestamp) -> Result<Profile, String> {
    let mut profile = Profile::new();
    for &(category, report) in reports {
        profile.set_report(category, report);
        if report == Report::CanRead {
            for element in elements_of(category) {
                profile
                    .transition(
                        ItemKey::Element(element),
                        Status::Seen,
                        Provenance::SelfReport,
                        at,
                        None,
                    )
                    .map_err(|error| error.to_string())?;
            }
        }
    }
    Ok(profile)
}

/// Saves `profile` at `path`, creating its directory first: molt's directory may not exist yet
/// on the first `molt init`.
fn save(profile: &Profile, path: &Path) -> Result<(), String> {
    if let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        fs::create_dir_all(dir)
            .map_err(|error| format!("cannot create {}: {error}", dir.display()))?;
    }
    profile.save(path).map_err(|error| error.to_string())
}

fn elements_of(category: Category) -> impl Iterator<Item = Element> {
    Element::ALL
        .iter()
        .copied()
        .filter(move |element| element.category() == category)
}
