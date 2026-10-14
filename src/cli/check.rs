//! `molt check [PATHS..]` (PRD §8.6): the checker run alone over files.
//!
//! There is no "old" content to compare with, so every file is checked whole,
//! as if it were new. A `Cargo.toml` then reports every dependency that is not
//! `known`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::checker::{self, CheckContext, CheckReport, Finding};
use crate::profile::store::{Profile, StoreError};
use crate::workspace::modules::ModuleScope;

/// The exit code when a path, the profile or a file could not be checked.
const CANNOT_CHECK: u8 = 2;

pub(super) fn run(profile_flag: Option<&Path>, paths: &[PathBuf]) -> ExitCode {
    let profile = match load_profile(profile_flag) {
        Ok(profile) => profile,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(CANNOT_CHECK);
        }
    };

    let mut failed = false;
    let mut files = Vec::new();
    let current_dir = [PathBuf::from(".")];
    let paths = if paths.is_empty() {
        &current_dir
    } else {
        paths
    };
    for path in paths {
        failed |= !collect(path, &mut files);
    }

    // Read once per scope: every file of a directory shares one.
    let mut own_modules: BTreeMap<ModuleScope, BTreeSet<String>> = BTreeMap::new();
    let mut found = false;
    let mut out = io::stdout().lock();
    for file in &files {
        let content = match fs::read_to_string(file) {
            Ok(content) => content,
            Err(error) => {
                eprintln!("{}: {error}", file.display());
                failed = true;
                continue;
            }
        };
        let ctx = CheckContext {
            profile: &profile,
            own_modules: own_modules
                .entry(ModuleScope::of(file))
                .or_insert_with_key(ModuleScope::own_modules),
        };
        match checker::check_file(file, "", &content, &ctx) {
            CheckReport::Clean => {}
            CheckReport::OutOfProfile(findings) => {
                found = true;
                for finding in &findings {
                    // A closed pipe (`molt check | head`) is not an error.
                    let _ = writeln!(out, "{}", line(file, finding));
                }
            }
            CheckReport::ParseError(error) => {
                eprintln!("{}:{error}", file.display());
                failed = true;
            }
        }
    }

    if failed {
        ExitCode::from(CANNOT_CHECK)
    } else if found {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// `path:line: element (category)`, which editors and grep can follow.
fn line(path: &Path, finding: &Finding) -> String {
    let category = finding
        .category()
        .map_or("crates", |category| category.name());
    format!(
        "{}:{}: {} ({category})",
        path.display(),
        finding.line,
        finding.key
    )
}

/// The profile at `--profile` or in the molt directory. Without one, a profile
/// with nothing `known`, and a note on stderr saying so.
fn load_profile(profile_flag: Option<&Path>) -> Result<Profile, String> {
    let path = super::profile::profile_path(profile_flag)?;
    match Profile::load(&path) {
        Ok(profile) => {
            super::profile::note_moved(&path, &profile);
            Ok(profile)
        }
        Err(StoreError::NotFound { path }) => {
            eprintln!(
                "no profile at {}; checking with nothing known",
                path.display()
            );
            Ok(Profile::new())
        }
        Err(error) => Err(error.to_string()),
    }
}

/// Adds `path` to `files`, or, for a directory, every Rust file and
/// `Cargo.toml` under it in name order. Skipped: hidden files and
/// directories (version control, editor lock links), a `target` next to a
/// `Cargo.toml` (build output), and symbolic links to directories (which can
/// loop or leave the project). A link is followed only when its name is one
/// of a file to check, so a broken link to anything else does not matter. A
/// path that cannot be read is reported on stderr and the rest is still
/// collected. Returns whether every path could be read.
fn collect(path: &Path, files: &mut Vec<PathBuf>) -> bool {
    let report = |error: io::Error| {
        eprintln!("{}: {error}", path.display());
        false
    };
    match fs::metadata(path) {
        Err(error) => return report(error),
        Ok(metadata) if !metadata.is_dir() => {
            files.push(path.to_owned());
            return true;
        }
        Ok(_) => {}
    }
    let entries =
        match fs::read_dir(path).and_then(|entries| entries.collect::<io::Result<Vec<_>>>()) {
            Ok(entries) => entries,
            Err(error) => return report(error),
        };
    let mut entries: Vec<_> = entries.iter().map(|entry| entry.path()).collect();
    entries.sort();
    let is_project = path.join("Cargo.toml").is_file();
    let mut ok = true;
    for entry in entries {
        let name = entry.file_name().unwrap_or_default().to_string_lossy();
        // Hidden: version control, and editor files such as Emacs's `.#a.rs`
        // lock links, which point nowhere.
        if name.starts_with('.') {
            continue;
        }
        let is_target = name == "Cargo.toml" || entry.extension().is_some_and(|ext| ext == "rs");
        let file_type = match fs::symlink_metadata(&entry) {
            Ok(metadata) => metadata.file_type(),
            Err(error) => {
                eprintln!("{}: {error}", entry.display());
                ok = false;
                continue;
            }
        };
        if file_type.is_dir() {
            if !(is_project && name == "target") {
                ok &= collect(&entry, files);
            }
        } else if is_target {
            // A link is followed only to a file: the target must be readable.
            match fs::metadata(&entry) {
                Ok(metadata) if metadata.is_dir() => {}
                Ok(_) => files.push(entry),
                Err(error) => {
                    eprintln!("{}: {error}", entry.display());
                    ok = false;
                }
            }
        }
    }
    ok
}
