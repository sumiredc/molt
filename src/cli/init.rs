//! `molt init [--repo PATH]` (PRD UC1).

use std::path::Path;
use std::process::ExitCode;

use super::mainline::{INTERRUPTED, connect, open_ui};
use super::profile;
use crate::onboarding::questionnaire::{self, Outcome};

pub(super) fn run(
    profile_flag: Option<&Path>,
    fake: Option<&Path>,
    ui_script: Option<&Path>,
    repo: Option<&Path>,
) -> ExitCode {
    let opened = profile::profile_path(profile_flag)
        .and_then(|path| open_ui(ui_script).map(|ui| (path, ui)));
    let (path, mut ui) = match opened {
        Ok(opened) => opened,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::FAILURE;
        }
    };
    let connected = connect(fake);
    let client = connected
        .as_ref()
        .map(|(_, _, client)| client.as_ref())
        .map_err(Clone::clone);
    match questionnaire::init(ui.as_mut(), &path, client, repo) {
        Ok(Outcome::Saved | Outcome::Kept) => ExitCode::SUCCESS,
        Ok(Outcome::Interrupted) => ExitCode::from(INTERRUPTED),
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}
