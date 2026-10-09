//! The project's own module names, which the checker must not take for external crates
//! (PRD §8.3). They are read from the files of the cargo project a checked file is in, so
//! every caller of `checker::check_file` fills `CheckContext::own_modules` the same way.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// The part of a cargo project a file belongs to, which decides its own modules: the project
/// is the nearest directory above the file with a `Cargo.toml`, and the file is either in the
/// library or main crate under `src/`, or in a crate of its own. Two files of the same scope
/// have the same own modules, so a caller checking many files may read them once per scope.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ModuleScope {
    /// Empty outside a project.
    root: PathBuf,
    in_src: bool,
}

impl ModuleScope {
    /// The scope of `file`, which need not exist yet. A file outside `src/` (`tests/`,
    /// `examples/`, `benches/`) or under `src/bin/` is a crate of its own that reaches the
    /// library only through its name.
    pub fn of(file: &Path) -> Self {
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
        Self { root, in_src }
    }

    /// The own modules of the files in this scope, read from the project as it is now. A file
    /// under `src/` gets the project's top-level modules and the library's names; a crate of
    /// its own gets the library's names only; outside a project there are none.
    pub fn own_modules(&self) -> BTreeSet<String> {
        if self.in_src {
            own_modules(&self.root)
        } else if self.root.as_os_str().is_empty() {
            BTreeSet::new()
        } else {
            own_crate_names(&self.root).into_iter().collect()
        }
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
