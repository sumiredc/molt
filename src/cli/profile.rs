//! `molt profile show | set | path` (PRD UC5).

use std::fmt::Write as _;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use super::ProfileCommand;
use crate::config;
use crate::profile::element::{Category, Element};
use crate::profile::status::{ItemKey, Provenance, Status};
use crate::profile::store::{Profile, StoreError, Timestamp};

/// The name of the profile file in the molt directory.
const PROFILE_FILE_NAME: &str = "profile.toml";

pub(super) fn run(profile_flag: Option<&Path>, command: &ProfileCommand) -> ExitCode {
    let result = profile_path(profile_flag).and_then(|path| match command {
        ProfileCommand::Show => show(&path),
        ProfileCommand::Set { key, status } => set(&path, key, status),
        ProfileCommand::Path => print(&format!("{}\n", path.display())),
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

/// Writes `text` to stdout. A closed pipe (`molt profile show | head`) is not
/// an error: the reader has what it wanted.
fn print(text: &str) -> Result<(), String> {
    match io::stdout().lock().write_all(text.as_bytes()) {
        Err(error) if error.kind() != io::ErrorKind::BrokenPipe => Err(error.to_string()),
        _ => Ok(()),
    }
}

/// Where the profile is read from and written to: `--profile` if given,
/// otherwise `profile.toml` in the molt directory (PRD §8.1).
pub(super) fn profile_path(profile_flag: Option<&Path>) -> Result<PathBuf, String> {
    match profile_flag {
        Some(path) => Ok(path.to_owned()),
        None => config::molt_home(|name| std::env::var_os(name))
            .map(|home| home.join(PROFILE_FILE_NAME))
            .map_err(|error| error.to_string()),
    }
}

/// Reads the profile, turning a missing file into the prompt for `molt init`
/// (UC5's exception flow).
pub(super) fn load(path: &Path) -> Result<Profile, String> {
    Profile::load(path).map_err(|error| match error {
        StoreError::NotFound { path } => format!(
            "no profile at {}\nrun `molt init` to create one",
            path.display()
        ),
        error => error.to_string(),
    })
}

fn show(path: &Path) -> Result<(), String> {
    let profile = load(path)?;
    print(&render(&profile))
}

/// Sets `key` to `status` by hand. An invalid key or status is rejected before
/// the profile is read, so the file is left as it was.
fn set(path: &Path, key: &str, status: &str) -> Result<(), String> {
    let parsed_key = key.parse::<ItemKey>();
    let parsed_status = status.parse::<Status>();
    let (key, status) = match (parsed_key, parsed_status) {
        (Ok(key), Ok(status)) => (key, status),
        (key, status) => {
            let mut message = String::new();
            if let Err(error) = key {
                writeln!(message, "{error}").unwrap();
            }
            if let Err(error) = status {
                writeln!(message, "{error}").unwrap();
            }
            message.push_str(&valid_inputs());
            return Err(message);
        }
    };

    let mut profile = load(path)?;
    profile
        .transition(
            key.clone(),
            status,
            Provenance::Manual,
            Timestamp::now(),
            None,
        )
        .map_err(|error| error.to_string())?;
    profile.save(path).map_err(|error| error.to_string())?;
    print(&format!("{key}: {status} ({})\n", Provenance::Manual))
}

/// The valid keys and statuses of `molt profile set`, shown when one is
/// rejected.
fn valid_inputs() -> String {
    let mut text = String::from("\nvalid keys:\n");
    for category in Category::ALL {
        let names: Vec<_> = elements_of(category).map(Element::name).collect();
        writeln!(text, "  {category}: {}", names.join(", ")).unwrap();
    }
    text.push_str("  crates: crate:<name>\n");
    let statuses: Vec<_> = Status::ALL.iter().map(|status| status.name()).collect();
    writeln!(text, "valid statuses: {}", statuses.join(", ")).unwrap();
    text
}

fn elements_of(category: Category) -> impl Iterator<Item = Element> {
    Element::ALL
        .iter()
        .copied()
        .filter(move |element| element.category() == category)
}

/// The profile as `molt profile show` prints it: per category the status,
/// provenance and time of each element that is not a base element, then the
/// crates, then the base elements together, then the history.
fn render(profile: &Profile) -> String {
    let width = Element::ALL
        .iter()
        .map(|element| element.name().len())
        .chain(profile.entries().map(|(key, _)| key.to_string().len()))
        .max()
        .unwrap_or(0);
    let row = |text: &mut String, key: &ItemKey| {
        let (provenance, at) = match profile.entry(key) {
            Some(entry) => (entry.provenance().name(), entry.at().to_string()),
            None => ("-", "-".to_owned()),
        };
        // The `Display` of `ItemKey` and `Status` ignores the width, so pad
        // their names instead.
        writeln!(
            text,
            "  {:width$}  {:7}  {provenance:14}  {at}",
            key.to_string(),
            profile.status(key).name(),
        )
        .unwrap();
    };

    let mut text = String::new();
    for category in Category::ALL {
        match profile.report(category) {
            Some(report) => writeln!(text, "{category} (report: {report})").unwrap(),
            None => writeln!(text, "{category}").unwrap(),
        }
        for element in elements_of(category).filter(|&e| !profile.is_base(e)) {
            row(&mut text, &ItemKey::Element(element));
        }
    }

    writeln!(text, "crates").unwrap();
    for (key, _) in profile.entries().filter(|(key, _)| is_crate(key)) {
        row(&mut text, key);
    }

    // Their status column does not decide whether generation uses them.
    writeln!(text, "base elements (used regardless of status)").unwrap();
    for &element in Element::ALL.iter().filter(|&&e| profile.is_base(e)) {
        row(&mut text, &ItemKey::Element(element));
    }

    writeln!(text, "history ({})", profile.history().len()).unwrap();
    for change in profile.history() {
        writeln!(
            text,
            "  {}  {}  {} -> {}  {}",
            change.at(),
            change.key(),
            change.from(),
            change.to(),
            change.provenance()
        )
        .unwrap();
    }
    text
}

fn is_crate(key: &ItemKey) -> bool {
    matches!(key, ItemKey::Crate(_))
}
