//! The start of `molt init` (PRD UC1): the checks before anything is asked, the user's report
//! per category, and saving the profile, once confirmed, with the stock-taking of the reference
//! repository.

#[cfg(test)]
mod tests;

use std::fs;
use std::path::Path;

use super::confirm::{self, Confirmed};
use super::probe::{self, Judgement, Model, Stopped};
use super::repo_scan::{self, RepoScan, Stored};
use crate::profile::element::{Category, Element};
use crate::profile::status::{ItemKey, Provenance, Status};
use crate::profile::store::{Profile, Report, Timestamp};
use crate::ui::Ui;

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
/// `model` is the result of creating the client for the configured endpoint. When it could not
/// be created (e.g. the endpoint needs an API key and its environment variable is unset), nothing
/// is asked and no profile is created. Otherwise an existing profile is overwritten only when the
/// user says yes, and the user reports on each category. After the report, the reference
/// repository `repo` (`--repo`) is taken stock of; when it cannot be used, that is said and init
/// goes on without it. The model is called only for the categories reported "have written", in
/// the Q&A ([`probe`]). Before saving, the user confirms the profile and may correct it
/// ([`confirm`]).
///
/// The profile and `repo-scan.toml` beside it are written only at the end, together: an error
/// (including an API failure during the Q&A), declining to overwrite or interrupting (also at the
/// confirmation) leaves both as they were.
pub fn init(
    ui: &mut dyn Ui,
    profile_path: &Path,
    model: Result<Model<'_>, String>,
    repo: Option<&Path>,
) -> Result<Outcome, String> {
    let model = model?;
    // Checked again just before saving; here so that the user is not asked anything in vain.
    repo_scan::check_target(&repo_scan::path_beside(profile_path))
        .map_err(|error| format!("{error}; no profile was saved"))?;

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

    let at = Timestamp::now();
    let scan = repo.and_then(|repo| repo_scan::take_stock(ui, repo, at));

    let have_written: Vec<_> = reports
        .iter()
        .filter(|(_, report)| *report == Report::HaveWritten)
        .map(|&(category, _)| category)
        .collect();
    let judgements = if have_written.is_empty() {
        Vec::new()
    } else {
        match probe::probe(ui, model, &have_written, scan.as_ref()) {
            Ok(judgements) => judgements,
            Err(Stopped::Interrupted) => return Ok(interrupted(ui)),
            Err(Stopped::Failed(cause)) => return Err(format!("{cause}; no profile was saved")),
        }
    };

    let mut profile = from_reports(&reports, at)?;
    mark_judged_known(&mut profile, &judgements, at)?;
    if let Some(scan) = &scan {
        repo_scan::mark_crates_known(&mut profile, scan, at)?;
    }
    if confirm::confirm(ui, &mut profile)? == Confirmed::Interrupted {
        return Ok(interrupted(ui));
    }
    save(ui, &profile, profile_path, scan.as_ref())?;
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

/// Makes each element `judgements` judged known `known`, by the Q&A, at `at`, with the reason of
/// the judgement as its evidence. The others stay `unknown`.
fn mark_judged_known(
    profile: &mut Profile,
    judgements: &[Judgement],
    at: Timestamp,
) -> Result<(), String> {
    for judgement in judgements.iter().filter(|judgement| judgement.known) {
        profile
            .transition(
                ItemKey::Element(judgement.element),
                Status::Known,
                Provenance::Qa,
                at,
                judgement.reason.clone(),
            )
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// Saves `profile` at `path` and makes `repo-scan.toml` beside it match: `scan` when there is
/// one, no file when there is none. The directory is created first: molt's directory may not
/// exist yet on the first `molt init`. A `repo-scan.toml` that molt did not write stops the save
/// before anything is written.
fn save(
    ui: &mut dyn Ui,
    profile: &Profile,
    path: &Path,
    scan: Option<&RepoScan>,
) -> Result<(), String> {
    let scan_path = repo_scan::path_beside(path);
    repo_scan::check_target(&scan_path)
        .map_err(|error| format!("{error}; no profile was saved"))?;
    if let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        fs::create_dir_all(dir)
            .map_err(|error| format!("cannot create {}: {error}", dir.display()))?;
    }
    profile.save(path).map_err(|error| error.to_string())?;
    ui.show(&format!("Saved the profile to {}.", path.display()));
    match repo_scan::store(&scan_path, scan) {
        Ok(Stored::Written) => ui.show(&format!(
            "Saved the stock-taking of the reference repository to {}.",
            scan_path.display()
        )),
        Ok(Stored::Removed) => ui.show(&format!(
            "Removed {}: the new profile has no reference repository.",
            scan_path.display()
        )),
        Ok(Stored::Nothing) => {}
        Err(error) => {
            return Err(format!(
                "the profile was saved, but {} could not be updated: {error}; run `molt init` again",
                scan_path.display()
            ));
        }
    }
    Ok(())
}

fn elements_of(category: Category) -> impl Iterator<Item = Element> {
    Element::ALL
        .iter()
        .copied()
        .filter(move |element| element.category() == category)
}
