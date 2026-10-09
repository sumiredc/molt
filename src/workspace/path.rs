//! Confines every path the tools touch to the project root.

use std::error::Error;
use std::fmt;
use std::io;
use std::path::{Component, Path, PathBuf};

/// The canonicalised project root that every read and write is confined to.
#[derive(Debug, Clone)]
pub struct Root {
    path: PathBuf,
}

#[derive(Debug)]
pub enum PathError {
    /// The root itself could not be canonicalised or is not a directory.
    InvalidRoot { path: PathBuf, source: io::Error },
    /// The path resolves to somewhere outside the root.
    OutsideRoot { path: PathBuf },
    /// An existing part of the path could not be resolved, e.g. a dangling symlink.
    Unresolvable { path: PathBuf, source: io::Error },
    /// The path is under `.git/` or `target/`, which may be read but not written.
    WriteForbidden { path: PathBuf },
}

impl Root {
    /// Canonicalises `path`, which must be an existing directory.
    pub fn new(path: impl AsRef<Path>) -> Result<Self, PathError> {
        let path = path.as_ref();
        let invalid = |source| PathError::InvalidRoot {
            path: path.to_path_buf(),
            source,
        };
        let canonical = path.canonicalize().map_err(invalid)?;
        if !canonical.is_dir() {
            return Err(invalid(io::Error::new(
                io::ErrorKind::NotADirectory,
                "not a directory",
            )));
        }
        Ok(Self { path: canonical })
    }

    /// The canonical root, e.g. the working directory for the commands run in the project.
    pub fn as_path(&self) -> &Path {
        &self.path
    }

    /// Resolves `path` (relative to the root, or absolute) to a canonical path under the root.
    pub fn resolve_read(&self, path: impl AsRef<Path>) -> Result<PathBuf, PathError> {
        self.resolve(path.as_ref()).map(|(resolved, _)| resolved)
    }

    /// Like [`Root::resolve_read`], and also rejects `.git` at any depth, the root's `target`,
    /// and the paths under them.
    pub fn resolve_write(&self, path: impl AsRef<Path>) -> Result<PathBuf, PathError> {
        let path = path.as_ref();
        let (resolved, through_forbidden_symlink) = self.resolve(path)?;
        if through_forbidden_symlink || self.is_write_forbidden(&resolved) {
            return Err(PathError::WriteForbidden {
                path: path.to_path_buf(),
            });
        }
        Ok(resolved)
    }

    /// Whether `path` (already under the root) is git's data or cargo's output. Compared
    /// case-insensitively because `.GIT` is `.git` on a case-insensitive file system.
    fn is_write_forbidden(&self, path: &Path) -> bool {
        let Ok(relative) = path.strip_prefix(&self.path) else {
            return false;
        };
        relative
            .components()
            .enumerate()
            .any(|(depth, component)| match component {
                Component::Normal(name) => name.to_str().is_some_and(|name| {
                    name.eq_ignore_ascii_case(".git")
                        // A module named `target` (`src/target/`) is ordinary source.
                        || (depth == 0 && name.eq_ignore_ascii_case("target"))
                }),
                _ => false,
            })
    }

    /// The target of a write may not exist yet, so `fs::canonicalize` on the whole path cannot
    /// be used. Instead the path is resolved one component at a time: an existing entry is
    /// canonicalised (following a symlink), a missing one is appended as is. `resolved` stays
    /// canonical up to the first missing entry, so `..` can pop it, and after a `..` the
    /// following entries may exist again and are canonicalised again.
    ///
    /// Also returns whether a symlink was followed from a write-forbidden place (e.g. `target`
    /// pointing to `build/`), since the canonical path alone no longer shows it.
    fn resolve(&self, path: &Path) -> Result<(PathBuf, bool), PathError> {
        let unresolvable = |source| PathError::Unresolvable {
            path: path.to_path_buf(),
            source,
        };
        let mut through_forbidden_symlink = false;
        let mut resolved = PathBuf::new();
        for component in self.path.join(path).components() {
            match component {
                Component::Prefix(_) | Component::RootDir => resolved.push(component),
                Component::CurDir => {}
                Component::ParentDir => {
                    resolved.pop();
                }
                Component::Normal(name) => {
                    resolved.push(name);
                    match resolved.symlink_metadata() {
                        // A dangling symlink fails here; writing through it would create its
                        // target wherever it points.
                        Ok(metadata) => {
                            if metadata.is_symlink() && self.is_write_forbidden(&resolved) {
                                through_forbidden_symlink = true;
                            }
                            resolved = resolved.canonicalize().map_err(unresolvable)?;
                        }
                        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                        Err(err) => return Err(unresolvable(err)),
                    }
                }
            }
        }
        if !resolved.starts_with(&self.path) {
            return Err(PathError::OutsideRoot {
                path: path.to_path_buf(),
            });
        }
        Ok((resolved, through_forbidden_symlink))
    }
}

impl fmt::Display for PathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRoot { path, source } => {
                write!(f, "invalid project root {}: {source}", path.display())
            }
            Self::OutsideRoot { path } => {
                write!(f, "{} is outside the project root", path.display())
            }
            Self::Unresolvable { path, source } => {
                write!(f, "cannot resolve {}: {source}", path.display())
            }
            Self::WriteForbidden { path } => write!(
                f,
                "{} is under .git/ or target/, which may not be written",
                path.display()
            ),
        }
    }
}

impl Error for PathError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidRoot { source, .. } | Self::Unresolvable { source, .. } => Some(source),
            Self::OutsideRoot { .. } | Self::WriteForbidden { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    /// A project root with `src/main.rs` and `.git/config`, and a separate directory outside it.
    fn project() -> (TempDir, TempDir, Root) {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/main.rs"), "").unwrap();
        fs::create_dir(dir.path().join(".git")).unwrap();
        fs::write(dir.path().join(".git/config"), "").unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret"), "").unwrap();
        let root = Root::new(dir.path()).unwrap();
        (dir, outside, root)
    }

    fn canonical(dir: &TempDir) -> PathBuf {
        fs::canonicalize(dir.path()).unwrap()
    }

    #[test]
    fn root_that_does_not_exist_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let err = Root::new(dir.path().join("missing")).unwrap_err();
        assert!(matches!(err, PathError::InvalidRoot { .. }), "{err:?}");
    }

    #[test]
    fn root_that_is_a_file_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("file"), "").unwrap();
        let err = Root::new(dir.path().join("file")).unwrap_err();
        assert!(matches!(err, PathError::InvalidRoot { .. }), "{err:?}");
    }

    #[test]
    fn existing_file_under_root_resolves_to_its_canonical_path() {
        let (dir, _outside, root) = project();
        let resolved = root.resolve_read("src/main.rs").unwrap();
        assert_eq!(resolved, canonical(&dir).join("src/main.rs"));
    }

    #[test]
    fn new_file_under_new_directories_resolves_for_write() {
        let (dir, _outside, root) = project();
        let resolved = root.resolve_write("src/a/b/new.rs").unwrap();
        assert_eq!(resolved, canonical(&dir).join("src/a/b/new.rs"));
    }

    #[test]
    fn parent_component_that_stays_under_root_resolves() {
        let (dir, _outside, root) = project();
        let resolved = root.resolve_write("src/new/../lib.rs").unwrap();
        assert_eq!(resolved, canonical(&dir).join("src/lib.rs"));
    }

    #[test]
    fn parent_component_escaping_root_is_rejected_for_read_and_write() {
        let (_dir, _outside, root) = project();
        for path in ["../x", "src/../../x"] {
            let err = root.resolve_read(path).unwrap_err();
            assert!(
                matches!(err, PathError::OutsideRoot { .. }),
                "{path}: {err:?}"
            );
            let err = root.resolve_write(path).unwrap_err();
            assert!(
                matches!(err, PathError::OutsideRoot { .. }),
                "{path}: {err:?}"
            );
        }
    }

    #[test]
    fn parent_component_escaping_through_new_directories_is_rejected() {
        let (_dir, _outside, root) = project();
        let err = root.resolve_write("new/../../x").unwrap_err();
        assert!(matches!(err, PathError::OutsideRoot { .. }), "{err:?}");
    }

    #[test]
    fn absolute_path_outside_root_is_rejected_for_read_and_write() {
        let (_dir, outside, root) = project();
        for path in [outside.path().join("secret"), outside.path().join("new")] {
            let err = root.resolve_read(&path).unwrap_err();
            assert!(matches!(err, PathError::OutsideRoot { .. }), "{err:?}");
            let err = root.resolve_write(&path).unwrap_err();
            assert!(matches!(err, PathError::OutsideRoot { .. }), "{err:?}");
        }
    }

    // On macOS the temporary directory is reached through a symlink (/var -> /private/var),
    // so this also checks that a non-canonical spelling of the root is accepted.
    #[test]
    fn absolute_path_under_root_resolves() {
        let (dir, _outside, root) = project();
        let resolved = root.resolve_read(dir.path().join("src/main.rs")).unwrap();
        assert_eq!(resolved, canonical(&dir).join("src/main.rs"));
    }

    #[cfg(unix)]
    #[test]
    fn symlink_to_directory_outside_root_is_rejected_for_read_and_write() {
        let (dir, outside, root) = project();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();
        for path in ["link/secret", "link/new"] {
            let err = root.resolve_read(path).unwrap_err();
            assert!(
                matches!(err, PathError::OutsideRoot { .. }),
                "{path}: {err:?}"
            );
            let err = root.resolve_write(path).unwrap_err();
            assert!(
                matches!(err, PathError::OutsideRoot { .. }),
                "{path}: {err:?}"
            );
        }
    }

    // `new` does not exist, so `..` brings the path back to existing entries, which may be symlinks.
    #[cfg(unix)]
    #[test]
    fn symlink_reached_after_parent_of_new_directory_is_rejected_for_read_and_write() {
        let (dir, outside, root) = project();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();
        let err = root.resolve_read("new/../link/secret").unwrap_err();
        assert!(matches!(err, PathError::OutsideRoot { .. }), "{err:?}");
        let err = root.resolve_write("new/../link/new").unwrap_err();
        assert!(matches!(err, PathError::OutsideRoot { .. }), "{err:?}");
    }

    #[cfg(unix)]
    #[test]
    fn symlink_to_file_outside_root_is_rejected_for_read_and_write() {
        let (dir, outside, root) = project();
        std::os::unix::fs::symlink(outside.path().join("secret"), dir.path().join("link")).unwrap();
        let err = root.resolve_read("link").unwrap_err();
        assert!(matches!(err, PathError::OutsideRoot { .. }), "{err:?}");
        let err = root.resolve_write("link").unwrap_err();
        assert!(matches!(err, PathError::OutsideRoot { .. }), "{err:?}");
    }

    // Writing through a dangling symlink would create its target, which may be outside the root.
    #[cfg(unix)]
    #[test]
    fn dangling_symlink_is_rejected_for_write() {
        let (dir, outside, root) = project();
        std::os::unix::fs::symlink(outside.path().join("new"), dir.path().join("link")).unwrap();
        let err = root.resolve_write("link").unwrap_err();
        assert!(matches!(err, PathError::Unresolvable { .. }), "{err:?}");
    }

    #[cfg(unix)]
    #[test]
    fn symlink_inside_root_resolves_to_its_target() {
        let (dir, _outside, root) = project();
        std::os::unix::fs::symlink(dir.path().join("src"), dir.path().join("link")).unwrap();
        let resolved = root.resolve_write("link/new.rs").unwrap();
        assert_eq!(resolved, canonical(&dir).join("src/new.rs"));
    }

    #[test]
    fn write_under_git_or_target_is_rejected() {
        let (_dir, _outside, root) = project();
        for path in [
            ".git/config",
            ".git/new",
            "target/debug/x",
            "./.git/config",
            "src/../target/x",
        ] {
            let err = root.resolve_write(path).unwrap_err();
            assert!(
                matches!(err, PathError::WriteForbidden { .. }),
                "{path}: {err:?}"
            );
        }
    }

    #[test]
    fn write_to_git_or_target_itself_is_rejected() {
        let (_dir, _outside, root) = project();
        for path in [".git", "target"] {
            let err = root.resolve_write(path).unwrap_err();
            assert!(
                matches!(err, PathError::WriteForbidden { .. }),
                "{path}: {err:?}"
            );
        }
    }

    // On a case-insensitive file system (the macOS default) `.GIT` is the same directory as `.git`.
    #[test]
    fn write_under_git_or_target_in_other_case_is_rejected() {
        let (_dir, _outside, root) = project();
        for path in [".GIT/config", "Target/x"] {
            let err = root.resolve_write(path).unwrap_err();
            assert!(
                matches!(err, PathError::WriteForbidden { .. }),
                "{path}: {err:?}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn write_through_symlink_into_git_is_rejected() {
        let (dir, _outside, root) = project();
        std::os::unix::fs::symlink(dir.path().join(".git"), dir.path().join("link")).unwrap();
        let err = root.resolve_write("link/config").unwrap_err();
        assert!(matches!(err, PathError::WriteForbidden { .. }), "{err:?}");
    }

    // A nested repository's hooks run code when git runs there, as the root's own would.
    #[test]
    fn write_under_nested_git_is_rejected() {
        let (_dir, _outside, root) = project();
        let err = root
            .resolve_write("vendor/x/.git/hooks/pre-commit")
            .unwrap_err();
        assert!(matches!(err, PathError::WriteForbidden { .. }), "{err:?}");
    }

    #[cfg(unix)]
    #[test]
    fn write_through_target_that_is_a_symlink_is_rejected() {
        let (dir, _outside, root) = project();
        fs::create_dir(dir.path().join("build")).unwrap();
        std::os::unix::fs::symlink(dir.path().join("build"), dir.path().join("target")).unwrap();
        let err = root.resolve_write("target/x").unwrap_err();
        assert!(matches!(err, PathError::WriteForbidden { .. }), "{err:?}");
    }

    #[test]
    fn read_under_git_is_allowed() {
        let (dir, _outside, root) = project();
        let resolved = root.resolve_read(".git/config").unwrap();
        assert_eq!(resolved, canonical(&dir).join(".git/config"));
    }

    // Only the root's own `target/` is cargo's output; a module named `target` is ordinary source.
    #[test]
    fn write_to_nested_directory_named_target_is_allowed() {
        let (dir, _outside, root) = project();
        let resolved = root.resolve_write("src/target/mod.rs").unwrap();
        assert_eq!(resolved, canonical(&dir).join("src/target/mod.rs"));
    }

    // Only an entry named exactly `.git` is git's data; `.gitignore` and `.github/` are ordinary files.
    #[test]
    fn write_to_entries_starting_with_git_is_allowed() {
        let (dir, _outside, root) = project();
        for path in [".gitignore", ".github/workflows/ci.yml"] {
            let resolved = root.resolve_write(path).unwrap();
            assert_eq!(resolved, canonical(&dir).join(path));
        }
    }

    // The message goes back to the model, which knows the path it asked for, not the symlink's target.
    #[cfg(unix)]
    #[test]
    fn write_forbidden_reports_the_requested_path() {
        let (dir, _outside, root) = project();
        fs::create_dir(dir.path().join("build")).unwrap();
        std::os::unix::fs::symlink(dir.path().join("build"), dir.path().join("target")).unwrap();
        let err = root.resolve_write("target/x").unwrap_err();
        assert!(
            matches!(&err, PathError::WriteForbidden { path } if path == Path::new("target/x")),
            "{err:?}"
        );
    }
}
