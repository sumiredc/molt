//! Runs the cargo subcommands the model may ask for, with a fixed argv (PRD §8.4, §11).
//!
//! The agent sees only [`CargoRunner`], so tests swap in [`FakeCargo`] (PRD §9).

mod fake;

use std::error::Error;
use std::fmt;
use std::io;
use std::process::{Command, Stdio};

use super::Approval;
use super::path::Root;

pub use fake::{CargoCall, FakeCargo};

/// The subcommands that run without approval.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subcommand {
    Check,
    Test,
    Clippy,
    Build,
    Fmt,
}

/// A crate name as crates.io accepts it: ASCII letters, digits, `-` and `_`, starting with a
/// letter, at most 64 characters. It never starts with `-`, so it cannot be read as an option.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrateName(String);

/// What the model asked `run_cargo` for, after the allowlist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CargoRequest {
    Run(Subcommand),
    /// Runs only through [`CargoRunner::add`], which needs an [`Approval`].
    Add(CrateName),
}

/// Why a `run_cargo` request was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestError {
    /// The subcommand is not one of `check` `test` `clippy` `build` `fmt` `add`.
    NotAllowed { subcommand: String },
    /// Arguments were given to a subcommand that takes none, or `add` got other than one.
    UnexpectedArgs {
        subcommand: String,
        args: Vec<String>,
    },
    /// The argument to `add` is not a crate name.
    InvalidCrateName { name: String },
}

/// What cargo printed and whether it succeeded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CargoOutput {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

/// Why cargo could not be run. A cargo that ran and failed is a [`CargoOutput`] instead.
#[derive(Debug)]
pub enum CargoError {
    /// The `cargo` process could not be started.
    Spawn(io::Error),
    /// [`FakeCargo`] has no more queued results.
    FakeExhausted,
}

/// Runs cargo in the project.
pub trait CargoRunner {
    /// Runs a subcommand that needs no approval.
    fn run(&self, subcommand: Subcommand) -> Result<CargoOutput, CargoError>;

    /// Runs `cargo add <crate_name>`. Adding a crate needs the user's approval (PRD §8.4):
    ///
    /// ```compile_fail
    /// use molt::workspace::cargo::{CargoRunner, CargoRequest, FakeCargo};
    ///
    /// let Ok(CargoRequest::Add(name)) = CargoRequest::parse("add", &["serde".to_owned()]) else {
    ///     unreachable!()
    /// };
    /// FakeCargo::new().add(&name);
    /// ```
    fn add(&self, crate_name: &CrateName, approval: Approval) -> Result<CargoOutput, CargoError>;
}

/// The real [`CargoRunner`]: starts `cargo` in the project root with a fixed argv.
#[derive(Debug, Clone)]
pub struct Cargo {
    root: Root,
}

impl Subcommand {
    /// The name cargo knows the subcommand by.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Check => "check",
            Self::Test => "test",
            Self::Clippy => "clippy",
            Self::Build => "build",
            Self::Fmt => "fmt",
        }
    }
}

impl CrateName {
    pub fn new(name: &str) -> Result<Self, RequestError> {
        let mut chars = name.chars();
        let valid = name.len() <= 64
            && chars.next().is_some_and(|c| c.is_ascii_alphabetic())
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        if !valid {
            return Err(RequestError::InvalidCrateName {
                name: name.to_owned(),
            });
        }
        Ok(Self(name.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl CargoRequest {
    /// Checks the subcommand and arguments the model passed to `run_cargo` against the
    /// allowlist. `check` `test` `clippy` `build` `fmt` take no arguments, and `add` takes exactly
    /// one crate name, so the model cannot add flags, `--`, or a subcommand of its own.
    pub fn parse(subcommand: &str, args: &[String]) -> Result<Self, RequestError> {
        let unexpected_args = || RequestError::UnexpectedArgs {
            subcommand: subcommand.to_owned(),
            args: args.to_vec(),
        };
        let run = match subcommand {
            "check" => Subcommand::Check,
            "test" => Subcommand::Test,
            "clippy" => Subcommand::Clippy,
            "build" => Subcommand::Build,
            "fmt" => Subcommand::Fmt,
            "add" => {
                let [name] = args else {
                    return Err(unexpected_args());
                };
                return CrateName::new(name).map(Self::Add);
            }
            _ => {
                return Err(RequestError::NotAllowed {
                    subcommand: subcommand.to_owned(),
                });
            }
        };
        if !args.is_empty() {
            return Err(unexpected_args());
        }
        Ok(Self::Run(run))
    }
}

impl Cargo {
    pub fn new(root: Root) -> Self {
        Self { root }
    }

    fn execute(&self, argv: &[&str]) -> Result<CargoOutput, CargoError> {
        // Plain output: the side line reads the places of errors from it (PRD UC3 ③).
        let output = Command::new("cargo")
            .args(argv)
            .env("CARGO_TERM_COLOR", "never")
            .current_dir(self.root.as_path())
            .stdin(Stdio::null())
            .output()
            .map_err(CargoError::Spawn)?;
        Ok(CargoOutput {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

impl CargoRunner for Cargo {
    fn run(&self, subcommand: Subcommand) -> Result<CargoOutput, CargoError> {
        self.execute(&[subcommand.as_str()])
    }

    fn add(&self, crate_name: &CrateName, _approval: Approval) -> Result<CargoOutput, CargoError> {
        self.execute(&add_argv(crate_name))
    }
}

fn add_argv(crate_name: &CrateName) -> [&str; 2] {
    ["add", crate_name.as_str()]
}

impl fmt::Display for RequestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAllowed { subcommand } => write!(
                f,
                "cargo {subcommand} is not allowed: use one of check, test, clippy, build, fmt, add"
            ),
            Self::UnexpectedArgs { subcommand, args } if subcommand == "add" => write!(
                f,
                "cargo add takes exactly one crate name, but got {args:?}"
            ),
            Self::UnexpectedArgs { subcommand, args } => {
                write!(f, "cargo {subcommand} takes no arguments, but got {args:?}")
            }
            Self::InvalidCrateName { name } => write!(
                f,
                "{name:?} is not a crate name: use ASCII letters, digits, - and _, starting with a letter, without a version or features"
            ),
        }
    }
}

impl Error for RequestError {}

impl fmt::Display for CargoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn(source) => write!(f, "cannot run cargo: {source}"),
            Self::FakeExhausted => write!(f, "the fake cargo has no more queued results"),
        }
    }
}

impl Error for CargoError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Spawn(source) => Some(source),
            Self::FakeExhausted => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn args(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| (*arg).to_owned()).collect()
    }

    #[test]
    fn allowed_subcommands_without_arguments_are_accepted() {
        for (name, subcommand) in [
            ("check", Subcommand::Check),
            ("test", Subcommand::Test),
            ("clippy", Subcommand::Clippy),
            ("build", Subcommand::Build),
            ("fmt", Subcommand::Fmt),
        ] {
            assert_eq!(
                CargoRequest::parse(name, &[]),
                Ok(CargoRequest::Run(subcommand)),
                "{name}"
            );
        }
    }

    #[test]
    fn each_subcommand_runs_as_the_cargo_subcommand_of_the_same_name() {
        for subcommand in [
            Subcommand::Check,
            Subcommand::Test,
            Subcommand::Clippy,
            Subcommand::Build,
            Subcommand::Fmt,
        ] {
            assert_eq!(
                CargoRequest::parse(subcommand.as_str(), &[]),
                Ok(CargoRequest::Run(subcommand))
            );
        }
    }

    #[test]
    fn add_runs_cargo_add_with_only_the_crate_name() {
        let name = CrateName::new("serde").unwrap();

        assert_eq!(add_argv(&name), ["add", "serde"]);
    }

    #[test]
    fn add_with_one_crate_name_is_accepted() {
        assert_eq!(
            CargoRequest::parse("add", &args(&["serde_json"])),
            Ok(CargoRequest::Add(CrateName("serde_json".to_owned())))
        );
    }

    #[test]
    fn subcommands_outside_the_allowlist_are_rejected() {
        for name in ["run", "install", "publish", "clean", "", "Check", "check "] {
            assert_eq!(
                CargoRequest::parse(name, &[]),
                Err(RequestError::NotAllowed {
                    subcommand: name.to_owned()
                }),
                "{name:?}"
            );
        }
    }

    #[test]
    fn arguments_to_subcommands_that_take_none_are_rejected() {
        for (name, given) in [
            ("test", args(&["--", "--nocapture"])),
            ("check", args(&["--all-targets"])),
            ("build", args(&["--config", "build.rustc-wrapper='sh'"])),
            ("clippy", args(&["--", "-D", "warnings"])),
            ("fmt", args(&["--", "src/main.rs"])),
        ] {
            assert_eq!(
                CargoRequest::parse(name, &given),
                Err(RequestError::UnexpectedArgs {
                    subcommand: name.to_owned(),
                    args: given.clone()
                }),
                "{name} {given:?}"
            );
        }
    }

    #[test]
    fn add_without_exactly_one_argument_is_rejected() {
        for given in [args(&[]), args(&["serde", "tokio"]), args(&["--", "serde"])] {
            assert_eq!(
                CargoRequest::parse("add", &given),
                Err(RequestError::UnexpectedArgs {
                    subcommand: "add".to_owned(),
                    args: given.clone()
                }),
                "{given:?}"
            );
        }
    }

    #[test]
    fn add_with_an_argument_that_is_not_a_crate_name_is_rejected() {
        let too_long = format!("a{}", "b".repeat(64));
        for name in [
            "--",
            "--path=../x",
            "-F",
            "serde@1.0",
            "serde/derive",
            "../x",
            "1serde",
            "_serde",
            "",
            too_long.as_str(),
        ] {
            assert_eq!(
                CargoRequest::parse("add", &args(&[name])),
                Err(RequestError::InvalidCrateName {
                    name: name.to_owned()
                }),
                "{name:?}"
            );
        }
    }

    #[test]
    fn crate_name_of_64_characters_is_accepted() {
        let name = format!("a{}", "b".repeat(63));
        assert_eq!(CrateName::new(&name), Ok(CrateName(name.clone())), "{name}");
    }

    #[test]
    fn rejecting_add_without_a_crate_name_asks_for_one() {
        let error = CargoRequest::parse("add", &[]).unwrap_err();

        assert_eq!(
            error.to_string(),
            "cargo add takes exactly one crate name, but got []"
        );
    }

    #[test]
    fn rejecting_arguments_to_other_subcommands_says_they_take_none() {
        let error = CargoRequest::parse("check", &args(&["--all-targets"])).unwrap_err();

        assert_eq!(
            error.to_string(),
            "cargo check takes no arguments, but got [\"--all-targets\"]"
        );
    }

    #[test]
    fn rejecting_a_crate_name_with_a_version_says_versions_are_not_taken() {
        let error = CargoRequest::parse("add", &args(&["serde@1.0"])).unwrap_err();

        assert!(
            error.to_string().contains("without a version or features"),
            "{error}"
        );
    }

    /// A tiny binary crate made by `cargo init`, so the real runner has something to build.
    ///
    /// Give each test its own `name`: under a shared `CARGO_TARGET_DIR`, crates of the same name
    /// share one fingerprint, so cargo may take one test's crate as already checked for another.
    fn project(name: &str, main_rs: &str) -> (tempfile::TempDir, Cargo) {
        let dir = tempfile::tempdir().unwrap();
        let status = Command::new("cargo")
            .args(["init", "--quiet", "--vcs", "none", "--name", name])
            .current_dir(dir.path())
            .status()
            .unwrap();
        assert!(status.success());
        fs::write(dir.path().join("src/main.rs"), main_rs).unwrap();
        let cargo = Cargo::new(Root::new(dir.path()).unwrap());
        (dir, cargo)
    }

    #[test]
    fn real_cargo_check_succeeds_on_a_crate_that_compiles() {
        let (_dir, cargo) = project("probe_compiles", "fn main() {}\n");

        let output = cargo.run(Subcommand::Check).unwrap();

        assert!(output.success, "{}", output.stderr);
    }

    #[test]
    fn real_cargo_check_reports_the_compile_error_on_a_crate_that_does_not_compile() {
        let (_dir, cargo) = project(
            "probe_does_not_compile",
            "fn main() { let x: u8 = \"no\"; }\n",
        );

        let output = cargo.run(Subcommand::Check).unwrap();

        assert!(!output.success);
        assert!(
            output.stderr.contains("mismatched types"),
            "{}",
            output.stderr
        );
    }

    #[test]
    fn real_cargo_runs_in_the_project_root() {
        let (dir, cargo) = project("probe_fmt", "fn main(){}\n");

        let output = cargo.run(Subcommand::Fmt).unwrap();

        assert!(output.success, "{}", output.stderr);
        assert_eq!(
            fs::read_to_string(dir.path().join("src/main.rs")).unwrap(),
            "fn main() {}\n"
        );
    }
}
