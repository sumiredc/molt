//! Taking stock of a reference repository for `molt init` (PRD UC1, §8.3): the crates its
//! `Cargo.toml` depends on, which become `known`, and where its code uses each element, kept in
//! `repo-scan.toml` beside the profile as a snapshot taken at init.
//!
//! `repo-scan.toml` exists exactly when the profile beside it was last saved by `molt init` with
//! a usable reference repository. It is written or removed only together with that save, so
//! declining to overwrite, interrupting, or failing before the save leaves it as it was.
//!
//! Nothing here talks to the user, so that both `onboarding`, which writes it, and `agent`, which
//! reads it for the side line's explanation, can use it (PRD §8.2).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use toml_edit::{Array, DocumentMut, InlineTable, Item, Table};

use crate::checker::{crates, visitor};
use crate::profile::element::Element;
use crate::profile::status::{ItemKey, Provenance, Status};
use crate::profile::store::{Profile, Timestamp, write_atomically};

/// The name of the stock-taking file, in the profile's directory.
pub const FILE_NAME: &str = "repo-scan.toml";

/// The most places kept per element.
pub const MAX_PLACES: usize = 3;

/// Where the stock-taking for the profile at `profile_path` is kept: [`FILE_NAME`] in the
/// directory of the profile actually used, so a profile given with `--profile` keeps its own.
pub fn path_beside(profile_path: &Path) -> PathBuf {
    profile_path.with_file_name(FILE_NAME)
}

/// The stock-taking of a reference repository, as saved in [`FILE_NAME`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoScan {
    /// The reference repository, as an absolute path.
    pub repo: String,
    /// When the stock was taken.
    pub at: Timestamp,
    /// The crates its manifests depend on, as the profile keys them (`-` spelled `_`), sorted.
    pub crates: Vec<String>,
    /// The files left out because they are not Rust syn parses or not TOML, relative to `repo`.
    pub skipped: Vec<String>,
    /// Per element name, where the code uses it: at most [`MAX_PLACES`], in file and line order.
    pub elements: BTreeMap<String, Vec<Place>>,
}

/// One place where an element is used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Place {
    /// Relative to the repository, with `/` between components.
    pub path: String,
    /// 1-based.
    pub line: usize,
}

/// Why a path given with `--repo` cannot be used as a reference repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unusable {
    Missing(PathBuf),
    NotCargo(PathBuf),
    Unreadable { path: PathBuf, reason: String },
}

impl fmt::Display for Unusable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Unusable::Missing(path) => write!(f, "{} does not exist", path.display()),
            Unusable::NotCargo(path) => write!(
                f,
                "{} is not a cargo project (no Cargo.toml in it)",
                path.display()
            ),
            Unusable::Unreadable { path, reason } => {
                write!(f, "cannot read {}: {reason}", path.display())
            }
        }
    }
}

/// Takes stock of the cargo project at `repo` at `at`.
///
/// The crates are the dependencies of every dependency table of its `Cargo.toml` and of the
/// `Cargo.toml` of its workspace members (`members`, with a trailing `/*` read as every
/// directory with a `Cargo.toml`), keyed by the crate they name (`package` when renamed).
/// The elements are those `checker::visitor` detects in its `.rs` files, outside `target` and
/// directories whose names start with `.`; symbolic links are not followed.
pub fn scan(repo: &Path, at: Timestamp) -> Result<RepoScan, Unusable> {
    let root = fs::canonicalize(repo).map_err(|error| match error.kind() {
        io::ErrorKind::NotFound => Unusable::Missing(repo.to_owned()),
        _ => Unusable::Unreadable {
            path: repo.to_owned(),
            reason: error.to_string(),
        },
    })?;
    let manifest_path = root.join("Cargo.toml");
    if !root.is_dir() || !manifest_path.is_file() {
        return Err(Unusable::NotCargo(repo.to_owned()));
    }
    let manifest = fs::read_to_string(&manifest_path).map_err(|error| Unusable::Unreadable {
        path: manifest_path.clone(),
        reason: error.to_string(),
    })?;
    let mut crates = dependencies(&manifest).map_err(|reason| Unusable::Unreadable {
        path: manifest_path.clone(),
        reason,
    })?;

    let mut skipped = Vec::new();
    for member in members(&root, &manifest) {
        match fs::read_to_string(&member)
            .map_err(|error| error.to_string())
            .and_then(|text| dependencies(&text))
        {
            Ok(found) => crates.extend(found),
            Err(_) => skipped.push(relative(&root, &member)),
        }
    }

    let mut files = Vec::new();
    rust_files(&root, &mut files);
    let mut elements: BTreeMap<String, Vec<Place>> = BTreeMap::new();
    for file in files {
        let path = relative(&root, &file);
        let Ok(findings) = fs::read_to_string(&file)
            .map_err(|_| ())
            .and_then(|source| visitor::detect(&source).map_err(|_| ()))
        else {
            skipped.push(path);
            continue;
        };
        for finding in findings {
            let ItemKey::Element(element) = finding.key else {
                continue;
            };
            let places = elements.entry(element.name().to_owned()).or_default();
            let place = Place {
                path: path.clone(),
                line: finding.line,
            };
            if places.len() < MAX_PLACES && !places.contains(&place) {
                places.push(place);
            }
        }
    }

    Ok(RepoScan {
        repo: root.to_string_lossy().into_owned(),
        at,
        crates: crates.into_iter().collect(),
        skipped,
        elements,
    })
}

/// Makes each crate of `scan` `known` in `profile`, by the reference repository, at `at`.
/// A crate that already has another status is left as it is.
pub fn mark_crates_known(
    profile: &mut Profile,
    scan: &RepoScan,
    at: Timestamp,
) -> Result<(), String> {
    for name in &scan.crates {
        let key = ItemKey::Crate(name.clone());
        if profile.status(&key) != Status::Unknown {
            continue;
        }
        profile
            .transition(
                key,
                Status::Known,
                Provenance::ReferenceRepo,
                at,
                Some(scan.repo.clone()),
            )
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// Checks, before anything is saved, that [`store`] may replace or remove the file at `path`:
/// there is none, or it is a regular file in the shape of a stock-taking. Anything else (a file
/// of another shape, a symbolic link) is taken for the user's and kept. Ownership is judged by
/// shape because molt keeps no record of what it wrote, and the file has one fixed name.
pub fn check_target(path: &Path) -> Result<(), String> {
    if is_link(path) {
        return Err(format!(
            "{} is a symbolic link; move it away and run `molt init` again",
            path.display()
        ));
    }
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
    };
    from_toml(&text).map(drop).map_err(|reason| {
        format!(
            "{} is not a stock-taking molt wrote ({reason}); move it away and run `molt init` again",
            path.display()
        )
    })
}

/// What [`store`] did to the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stored {
    Written,
    Removed,
    Nothing,
}

/// Makes the file at `path` match the profile just saved: `scan` when there is one, and no file
/// when there is none. Call [`check_target`] first.
///
/// When writing fails, the earlier file is removed if it can be, so that what remains is no
/// stock-taking rather than one of another profile.
pub fn store(path: &Path, scan: Option<&RepoScan>) -> io::Result<Stored> {
    match scan {
        Some(scan) => write_atomically(path, to_toml(scan).as_bytes())
            .map(|()| Stored::Written)
            .map_err(|error| {
                let earlier = match fs::remove_file(path) {
                    Ok(()) => "the earlier one was removed".to_owned(),
                    Err(removing) if removing.kind() == io::ErrorKind::NotFound => {
                        "there was no earlier one".to_owned()
                    }
                    Err(removing) => format!("the earlier one is left ({removing})"),
                };
                io::Error::new(error.kind(), format!("{error}; {earlier}"))
            }),
        None => match fs::remove_file(path) {
            Ok(()) => Ok(Stored::Removed),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Stored::Nothing),
            Err(error) => Err(error),
        },
    }
}

/// The file's text: the places of an element as one array, one place per line, rather than a
/// `[[elements.<name>]]` table per place.
pub(crate) fn to_toml(scan: &RepoScan) -> String {
    let body = toml::to_string(scan).expect("every part of a stock-taking has a toml form");
    let mut doc: DocumentMut = body.parse().expect("toml writes toml");
    let mut elements = Table::new();
    for (name, places) in &scan.elements {
        let mut array = Array::new();
        for place in places {
            let mut table = InlineTable::new();
            table.insert("path", place.path.as_str().into());
            table.insert("line", (place.line as i64).into());
            array.push(table);
        }
        for place in array.iter_mut() {
            place.decor_mut().set_prefix("\n    ");
        }
        array.set_trailing_comma(true);
        array.set_trailing("\n");
        elements.insert(name, Item::Value(array.into()));
    }
    doc.insert("elements", Item::Table(elements));
    format!("# The stock-taking of the reference repository by `molt init` (PRD UC1).\n{doc}")
}

/// Reads a stock-taking. Every key of `elements` must name an element.
pub(crate) fn from_toml(text: &str) -> Result<RepoScan, String> {
    let scan: RepoScan = toml::from_str(text).map_err(|error| error.message().to_owned())?;
    if let Some(name) = scan
        .elements
        .keys()
        .find(|name| name.parse::<Element>().is_err())
    {
        return Err(format!("not an element: {name}"));
    }
    Ok(scan)
}

fn is_link(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink())
}

/// The crates `manifest` depends on, keyed as the profile keys them.
fn dependencies(manifest: &str) -> Result<BTreeSet<String>, String> {
    let findings = crates::in_manifest("", manifest).map_err(|error| error.to_string())?;
    Ok(findings
        .into_iter()
        .filter_map(|finding| match finding.key {
            ItemKey::Crate(name) => Some(name),
            ItemKey::Element(_) => None,
        })
        .collect())
}

/// The `Cargo.toml` of each workspace member that `manifest`, the root manifest, lists and
/// that exists, in a stable order.
fn members(root: &Path, manifest: &str) -> Vec<PathBuf> {
    let Ok(table) = manifest.parse::<toml::Table>() else {
        return Vec::new();
    };
    let listed = table
        .get("workspace")
        .and_then(|workspace| workspace.get("members"))
        .and_then(|members| members.as_array())
        .into_iter()
        .flatten()
        .filter_map(|member| member.as_str());
    let mut found = BTreeSet::new();
    for member in listed {
        let dirs: Vec<PathBuf> = match member.strip_suffix("/*") {
            Some(parent) => {
                let mut dirs: Vec<PathBuf> = fs::read_dir(root.join(parent))
                    .into_iter()
                    .flatten()
                    .flatten()
                    .map(|entry| entry.path())
                    .collect();
                dirs.sort();
                dirs
            }
            None => vec![root.join(member)],
        };
        for dir in dirs {
            let path = dir.join("Cargo.toml");
            if path.is_file() && path != root.join("Cargo.toml") {
                found.insert(path);
            }
        }
    }
    found.into_iter().collect()
}

/// The `.rs` files under `dir`, in path order, skipping `target`, directories whose names start
/// with `.`, and symbolic links.
fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if kind.is_dir() {
            if !name.starts_with('.') && name != "target" {
                rust_files(&path, out);
            }
        } else if kind.is_file() && path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// `path` relative to `root`, with `/` between components.
fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}
