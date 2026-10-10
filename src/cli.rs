use std::path::PathBuf;
use std::process::ExitCode;

use clap::error::{ContextKind, ContextValue};
use clap::{Args, CommandFactory, Parser, Subcommand};

mod check;
mod doctor;
mod init;
mod mainline;
mod profile;
pub mod ui;

/// A Rust coding agent that writes code only in the ways its user can understand.
#[derive(Debug, Parser)]
#[command(name = "molt", version)]
pub(crate) struct Cli {
    #[command(flatten)]
    pub(crate) common: CommonFlags,

    /// Without a subcommand, open the main line in the cargo project at cwd
    #[command(subcommand)]
    pub(crate) command: Option<Command>,
}

#[derive(Debug, Args)]
pub(crate) struct CommonFlags {
    /// Read and write the profile at PATH instead of the config directory
    #[arg(long, value_name = "PATH", global = true)]
    pub(crate) profile: Option<PathBuf>,

    /// Replace the model's responses with the script file at PATH
    #[arg(long, value_name = "PATH", global = true)]
    pub(crate) fake: Option<PathBuf>,

    /// Replace the user's inputs, choices, approvals and interrupts with the script file at PATH
    #[arg(long, value_name = "PATH", global = true)]
    pub(crate) ui_script: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// Create the profile through Q&A
    Init {
        /// Take stock of a reference repository
        #[arg(long, value_name = "PATH")]
        repo: Option<PathBuf>,
    },
    /// Show or edit the profile
    #[command(subcommand)]
    Profile(ProfileCommand),
    /// Run the checker alone and print findings (exit 1 if any)
    Check {
        /// Files or directories to check (default: .)
        #[arg(value_name = "PATHS")]
        paths: Vec<PathBuf>,
    },
    /// Check what the configured endpoint and models support
    Doctor,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ProfileCommand {
    /// Print the profile
    Show,
    /// Set the status of one key in the profile
    Set { key: String, status: String },
    /// Print where the profile is read from and written to
    Path,
}

pub fn run() -> ExitCode {
    let cli = Cli::try_parse().unwrap_or_else(|error| with_usage(error).exit());
    match &cli.command {
        Some(Command::Profile(command)) => profile::run(cli.common.profile.as_deref(), command),
        Some(Command::Check { paths }) => check::run(cli.common.profile.as_deref(), paths),
        // --fake の台本は chat の応答の列で、doctor が確かめるモデルの一覧を書けない。
        // 黙って実際の接続先に送らないよう、受けずに止める
        Some(Command::Doctor) if cli.common.fake.is_some() => {
            eprintln!("doctor does not support --fake: its script has no list of models");
            ExitCode::FAILURE
        }
        Some(Command::Doctor) => doctor::main(),
        Some(Command::Init { repo }) => init::run(
            cli.common.profile.as_deref(),
            cli.common.fake.as_deref(),
            cli.common.ui_script.as_deref(),
            repo.as_deref(),
        ),
        None => mainline::run(
            cli.common.profile.as_deref(),
            cli.common.fake.as_deref(),
            cli.common.ui_script.as_deref(),
        ),
    }
}

/// `error` with molt's usage line when it has none, as clap leaves it out of some errors, e.g. a
/// flag given without its value (`molt --fake`, `molt --profile`). `--help` and `--version` keep
/// their own text.
fn with_usage(mut error: clap::Error) -> clap::Error {
    if error.get(ContextKind::Usage).is_none() {
        error.insert(
            ContextKind::Usage,
            ContextValue::StyledStr(Cli::command().render_usage()),
        );
    }
    error
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::error::ErrorKind;

    #[test]
    fn definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn no_subcommand_opens_main_line() {
        let cli = Cli::try_parse_from(["molt"]).unwrap();
        assert!(cli.command.is_none());
    }

    #[test]
    fn init_takes_repo() {
        let cli = Cli::try_parse_from(["molt", "init", "--repo", "r"]).unwrap();
        assert!(
            matches!(cli.command, Some(Command::Init { repo: Some(p) }) if p.as_os_str() == "r")
        );
    }

    #[test]
    fn profile_has_show_set_path() {
        let show = Cli::try_parse_from(["molt", "profile", "show"]).unwrap();
        assert!(matches!(
            show.command,
            Some(Command::Profile(ProfileCommand::Show))
        ));
        let set = Cli::try_parse_from(["molt", "profile", "set", "k", "s"]).unwrap();
        assert!(
            matches!(set.command, Some(Command::Profile(ProfileCommand::Set { key, status })) if key == "k" && status == "s")
        );
        let path = Cli::try_parse_from(["molt", "profile", "path"]).unwrap();
        assert!(matches!(
            path.command,
            Some(Command::Profile(ProfileCommand::Path))
        ));
    }

    #[test]
    fn profile_set_requires_key_and_status() {
        assert!(Cli::try_parse_from(["molt", "profile", "set", "k"]).is_err());
    }

    #[test]
    fn check_takes_paths() {
        let cli = Cli::try_parse_from(["molt", "check", "a.rs", "b.rs"]).unwrap();
        assert!(
            matches!(cli.command, Some(Command::Check { paths }) if paths == [PathBuf::from("a.rs"), PathBuf::from("b.rs")])
        );
    }

    #[test]
    fn doctor_parses() {
        let cli = Cli::try_parse_from(["molt", "doctor"]).unwrap();
        assert!(matches!(cli.command, Some(Command::Doctor)));
    }

    #[test]
    fn common_flags_take_paths_after_subcommand() {
        let cli = Cli::try_parse_from(["molt", "check", "--profile", "p.toml", "--fake", "f.toml"])
            .unwrap();
        assert_eq!(cli.common.profile, Some(PathBuf::from("p.toml")));
        assert_eq!(cli.common.fake, Some(PathBuf::from("f.toml")));
    }

    #[test]
    fn ui_script_takes_a_path() {
        let cli = Cli::try_parse_from(["molt", "--ui-script", "s.toml"]).unwrap();
        assert_eq!(cli.common.ui_script, Some(PathBuf::from("s.toml")));
        assert!(Cli::try_parse_from(["molt", "--ui-script"]).is_err());
    }

    #[test]
    fn fake_requires_path() {
        assert!(Cli::try_parse_from(["molt", "--fake"]).is_err());
    }

    #[test]
    fn fake_script_path_left_out_is_an_error_with_the_usage() {
        let error = with_usage(Cli::try_parse_from(["molt", "--fake"]).unwrap_err());
        assert_eq!(error.kind(), ErrorKind::InvalidValue);
        assert!(error.to_string().contains("Usage: molt"), "{error}");
    }

    #[test]
    fn help_and_version_are_shown_unchanged_without_a_second_usage_line() {
        // `molt profile` without its subcommand shows the help of `profile`.
        for args in [
            ["molt", "--help"],
            ["molt", "--version"],
            ["molt", "profile"],
        ] {
            let error = Cli::try_parse_from(args).unwrap_err();
            let shown = error.to_string();
            assert_eq!(with_usage(error).to_string(), shown, "{args:?}");
        }
    }
}
