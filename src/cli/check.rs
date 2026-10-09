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

    let mut own_modules = OwnModules::default();
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
            own_modules: own_modules.of(file),
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
/// of base elements only, and a note on stderr saying so.
fn load_profile(profile_flag: Option<&Path>) -> Result<Profile, String> {
    let path = super::profile::profile_path(profile_flag)?;
    match Profile::load(&path) {
        Ok(profile) => Ok(profile),
        Err(StoreError::NotFound { path }) => {
            eprintln!(
                "no profile at {}; checking with base elements only",
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

/// The own modules of each cargo project, read once per project.
#[derive(Default)]
struct OwnModules {
    by_root: BTreeMap<(PathBuf, bool), BTreeSet<String>>,
}

impl OwnModules {
    /// The own modules of the project `file` is in: the nearest directory
    /// above it with a `Cargo.toml`. Outside a project there are none. A file
    /// outside `src/` (`tests/`, `examples/`, `benches/`) or under `src/bin/`
    /// is a crate of its own that reaches the library only through its name,
    /// so it gets the library's names and not the modules under `src/`.
    fn of(&mut self, file: &Path) -> &BTreeSet<String> {
        let file = std::path::absolute(file).unwrap_or_default();
        let root = file
            .ancestors()
            .skip(1)
            .find(|dir| dir.join("Cargo.toml").is_file())
            .map(Path::to_owned)
            .unwrap_or_default();
        let in_src = !root.as_os_str().is_empty()
            && file.starts_with(root.join("src"))
            && !file.starts_with(root.join("src/bin"));
        self.by_root
            .entry((root, in_src))
            .or_insert_with_key(|(root, in_src)| {
                if *in_src {
                    own_modules(root)
                } else if root.as_os_str().is_empty() {
                    BTreeSet::new()
                } else {
                    own_crate_names(root).into_iter().collect()
                }
            })
    }
}

/// The project's top-level modules: the files (`util.rs`) and module
/// directories (`util/mod.rs`) directly under `src/`, and the `mod`
/// declarations in `src/lib.rs` and `src/main.rs`, which also name modules
/// kept elsewhere with `#[path]`. Also the project's own library crate, by
/// its `[lib] name` and its `[package] name` with `-` as `_`, which a binary
/// or test of the same package names like a crate (`molt::cli::run()`), but
/// which is not an external crate. Nothing for an empty `root` (no project).
fn own_modules(root: &Path) -> BTreeSet<String> {
    let mut modules = BTreeSet::new();
    if root.as_os_str().is_empty() {
        return modules;
    }
    let src = root.join("src");
    for entry in fs::read_dir(&src).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.join("mod.rs").is_file() {
                modules.extend(
                    path.file_name()
                        .map(|name| name.to_string_lossy().into_owned()),
                );
            }
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            modules.extend(
                path.file_stem()
                    .map(|stem| stem.to_string_lossy().into_owned())
                    .filter(|stem| stem != "lib" && stem != "main"),
            );
        }
    }
    for crate_root in ["lib.rs", "main.rs"] {
        let Ok(content) = fs::read_to_string(src.join(crate_root)) else {
            continue;
        };
        let Ok(file) = syn::parse_file(&content) else {
            continue;
        };
        for item in file.items {
            if let syn::Item::Mod(item) = item {
                modules.insert(item.ident.to_string());
            }
        }
    }
    modules.extend(own_crate_names(root));
    modules
}

/// `[lib] name` and `[package] name` (with `-` as `_`) of `root/Cargo.toml`.
/// None when it has no `[package]` (a workspace root) or does not parse.
fn own_crate_names(root: &Path) -> Vec<String> {
    let Ok(content) = fs::read_to_string(root.join("Cargo.toml")) else {
        return Vec::new();
    };
    let Ok(manifest) = content.parse::<toml::Table>() else {
        return Vec::new();
    };
    ["lib", "package"]
        .into_iter()
        .filter_map(|table| manifest.get(table)?.get("name")?.as_str())
        .map(|name| name.replace('-', "_"))
        .collect()
}
