//! `molt init` (PRD UC1): the checks before anything is asked, the stock-taking of the reference
//! repository, the form, the judgement, and saving the profile, once confirmed, with the
//! stock-taking.

#[cfg(test)]
mod repo_scan_tests;
#[cfg(test)]
pub(crate) mod test_support;
#[cfg(test)]
mod tests;

use std::fs;
use std::path::Path;

use super::confirm::{self, Confirmed};
use super::form::{self, CategoryAnswer, Language};
use super::judge::{self, Judgement, Model, Stopped};
use crate::profile::element::Category;
use crate::profile::status::{ItemKey, Provenance, Status};
use crate::profile::store::{Profile, Report, Timestamp};
use crate::repo_scan::{self, RepoScan, Stored};
use crate::ui::Ui;

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
/// user says yes, and the reference repository `repo` (`--repo`) is taken stock of; when it cannot
/// be used, that is said and init goes on without it. Then the user fills in the form in
/// `language`, and the categories sent are judged ([`judge`]); when the judgement fails with the
/// API, the cause is shown and the form opens again with everything as submitted. Before saving,
/// the user confirms the profile and may correct it ([`confirm`]).
///
/// The profile and `repo-scan.toml` beside it are written only at the end, together: an error,
/// declining to overwrite or interrupting (also at the confirmation) leaves both as they were.
pub fn init(
    ui: &mut dyn Ui,
    profile_path: &Path,
    model: Result<Model<'_>, String>,
    repo: Option<&Path>,
    language: Language,
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

    let at = Timestamp::now();
    let scan = repo.and_then(|repo| take_stock(ui, repo, at));
    let form = form::build(language, scan.as_ref());

    let mut submitted = None;
    let (answers, judgements) = loop {
        let Some(values) = ui
            .form(&form, submitted.as_ref())
            .map_err(|error| error.to_string())?
        else {
            return Ok(interrupted(ui));
        };
        let answers = form::read(&form, &values);
        let sent: Vec<&CategoryAnswer> = answers.iter().filter(|answer| answer.sent()).collect();
        if sent.is_empty() {
            break (answers, Vec::new());
        }
        ui.show(&format!(
            "Judging the answers about {} categories…",
            sent.len()
        ));
        match judge::judge(ui, model, &sent) {
            Ok(judgements) => break (answers, judgements),
            Err(Stopped::Interrupted) => return Ok(interrupted(ui)),
            Err(Stopped::Failed(cause)) => {
                ui.show(&format!(
                    "{cause}. The form opens again with your answers; submit again, or press Ctrl-C to quit."
                ));
                submitted = Some(values);
            }
        }
    };

    let reports: Vec<(Category, Report)> = answers
        .iter()
        .map(|answer| (answer.category, answer.report))
        .collect();
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

/// A new profile with `reports`: the elements of a category reported "can read" are `seen`, and
/// those of one reported "don't know" stay `unknown` (no entry). Changes made at `at`.
fn from_reports(reports: &[(Category, Report)], at: Timestamp) -> Result<Profile, String> {
    let mut profile = Profile::new();
    for &(category, report) in reports {
        profile.set_report(category, report);
        if report == Report::CanRead {
            for element in category.elements() {
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

/// Takes stock of `repo` and tells the user the result. When `repo` cannot be used, says why and
/// returns `None`: `molt init` goes on without a reference (UC1).
fn take_stock(ui: &mut dyn Ui, repo: &Path, at: Timestamp) -> Option<RepoScan> {
    match repo_scan::scan(repo, at) {
        Ok(scan) => {
            let mut message = format!(
                "Took stock of {}: {} crates it depends on become known, {} elements are used in it.",
                scan.repo,
                scan.crates.len(),
                scan.elements.len()
            );
            if !scan.skipped.is_empty() {
                message.push_str(&format!(
                    " {} files that could not be read were left out.",
                    scan.skipped.len()
                ));
            }
            ui.show(&message);
            Some(scan)
        }
        Err(error) => {
            ui.show(&format!(
                "{error}; going on without a reference repository."
            ));
            None
        }
    }
}
