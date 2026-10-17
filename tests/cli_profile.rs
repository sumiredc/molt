//! `molt profile show | set | path` (PRD UC5, §8.6), driven through the binary.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use assert_cmd::cargo::cargo_bin_cmd;
use tempfile::TempDir;

/// Runs `molt --profile <profile> <args..>` with no molt directory in the
/// environment, so only `--profile` can decide where the profile is.
fn molt_with_profile(profile: &Path, args: &[&str]) -> Output {
    cargo_bin_cmd!("molt")
        .env_remove("MOLT_HOME")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("HOME")
        .arg("--profile")
        .arg(profile)
        .args(args)
        .output()
        .unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

/// A temporary directory with an empty profile file in it (a profile of base
/// elements only).
fn empty_profile() -> (TempDir, PathBuf) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("profile.toml");
    fs::write(&path, "").unwrap();
    (dir, path)
}

/// A profile file that saving would rewrite (the comment would be dropped), so
/// comparing its bytes shows whether the file was written again.
const HAND_WRITTEN_PROFILE: &str = "# written by hand\n[items.arc]\nstatus = \"known\"\nprovenance = \"manual\"\nat = 2026-10-10T12:34:56Z\n";

fn hand_written_profile() -> (TempDir, PathBuf) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("profile.toml");
    fs::write(&path, HAND_WRITTEN_PROFILE).unwrap();
    (dir, path)
}

/// The line of `show` output that lists `key`, with its columns split on
/// whitespace.
fn row<'a>(show: &'a str, key: &str) -> Vec<&'a str> {
    show.lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>())
        .find(|columns| columns.first() == Some(&key))
        .unwrap_or_else(|| panic!("no row for {key} in:\n{show}"))
}

/// The number of changes `show` reports in its history heading.
fn history_len(show: &str) -> usize {
    let heading = show
        .lines()
        .find(|line| line.starts_with("history"))
        .unwrap_or_else(|| panic!("no history heading in:\n{show}"));
    heading
        .trim_start_matches("history (")
        .trim_end_matches(')')
        .parse()
        .unwrap_or_else(|_| panic!("history heading is not `history (N)`: {heading}"))
}

/// Asserts that `stderr` lists every element name, the crate form and every
/// status, i.e. the valid inputs of `profile set`.
fn assert_lists_valid_inputs(stderr: &str) {
    for name in ["clone", "arc", "shared_ref", "unsafe_block", "crate:<name>"] {
        assert!(stderr.contains(name), "{name} not listed in:\n{stderr}");
    }
    assert!(
        stderr.contains("known, seen, unknown"),
        "statuses not listed in:\n{stderr}"
    );
}

#[test]
fn set_with_an_element_name_that_does_not_exist_is_rejected_with_the_valid_inputs_and_the_file_unchanged()
 {
    let (_dir, path) = hand_written_profile();

    let output = molt_with_profile(&path, &["profile", "set", "not_an_element", "known"]);

    assert!(!output.status.success());
    let stderr = stderr(&output);
    assert!(stderr.contains("not_an_element"), "{stderr}");
    assert_lists_valid_inputs(&stderr);
    assert_eq!(fs::read_to_string(&path).unwrap(), HAND_WRITTEN_PROFILE);
}

#[test]
fn set_with_an_invalid_status_is_rejected_with_the_valid_inputs_and_the_file_unchanged() {
    let (_dir, path) = hand_written_profile();

    let output = molt_with_profile(&path, &["profile", "set", "clone", "Known"]);

    assert!(!output.status.success());
    let stderr = stderr(&output);
    assert!(stderr.contains("Known"), "{stderr}");
    assert_lists_valid_inputs(&stderr);
    assert_eq!(fs::read_to_string(&path).unwrap(), HAND_WRITTEN_PROFILE);
}

#[test]
fn set_then_show_shows_the_new_status_with_provenance_manual_and_one_more_history_entry() {
    let (_dir, path) = empty_profile();
    let before = molt_with_profile(&path, &["profile", "show"]);
    assert!(before.status.success(), "{}", stderr(&before));
    assert_eq!(row(&stdout(&before), "clone")[1], "unknown");

    let set = molt_with_profile(&path, &["profile", "set", "clone", "known"]);
    assert!(set.status.success(), "{}", stderr(&set));

    let after = molt_with_profile(&path, &["profile", "show"]);
    assert!(after.status.success(), "{}", stderr(&after));
    let after = stdout(&after);
    assert_eq!(row(&after, "clone")[1..3], ["known", "manual"]);
    assert_eq!(history_len(&after), history_len(&stdout(&before)) + 1);
}

#[test]
fn set_accepts_a_crate_key() {
    let (_dir, path) = empty_profile();

    let set = molt_with_profile(&path, &["profile", "set", "crate:tokio", "seen"]);
    assert!(set.status.success(), "{}", stderr(&set));

    let show = stdout(&molt_with_profile(&path, &["profile", "show"]));
    assert_eq!(row(&show, "crate:tokio")[1..3], ["seen", "manual"]);
}

#[test]
fn show_lists_every_element_under_its_category_shared_ref_too() {
    let (_dir, path) = empty_profile();

    let show = stdout(&molt_with_profile(&path, &["profile", "show"]));

    let position = |text: &str| {
        show.find(text)
            .unwrap_or_else(|| panic!("{text:?} not in:\n{show}"))
    };
    assert!(!show.contains("regardless of status"), "{show}");
    // shared_ref was once used without asking, and listed apart; now it is
    // listed once, under borrowing.
    assert_eq!(show.matches("\n  shared_ref ").count(), 1, "{show}");
    let shared_ref = position("\n  shared_ref ");
    assert!(position("\nborrowing\n") < shared_ref, "{show}");
    assert!(shared_ref < position("\nlifetimes\n"), "{show}");
    assert_eq!(row(&show, "shared_ref")[1], "unknown");
}

#[test]
fn an_element_in_allow_of_an_older_profile_is_shown_known_with_a_note() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("profile.toml");
    let older = "allow = [\"arc\", \"rc\"]\ndeny = [\"option\"]\n";
    fs::write(&path, older).unwrap();

    let output = molt_with_profile(&path, &["profile", "show"]);

    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(row(&stdout(&output), "arc")[1..3], ["known", "manual"]);
    assert_eq!(row(&stdout(&output), "option")[1], "unknown");
    assert!(
        stderr(&output).contains("rc, arc were in `allow`"),
        "{}",
        stderr(&output)
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), older, "show only reads");
}

#[test]
fn set_on_an_older_profile_saves_the_moved_element_and_drops_allow_and_deny() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("profile.toml");
    fs::write(&path, "allow = [\"arc\"]\ndeny = [\"option\"]\n").unwrap();

    let set = molt_with_profile(&path, &["profile", "set", "clone", "known"]);

    assert!(set.status.success(), "{}", stderr(&set));
    assert!(
        stderr(&set).contains("arc was in `allow`"),
        "{}",
        stderr(&set)
    );
    let saved = fs::read_to_string(&path).unwrap();
    assert!(saved.contains("[items.arc]"), "{saved}");
    assert!(
        !saved.contains("allow =") && !saved.contains("deny ="),
        "{saved}"
    );
    assert_eq!(saved.matches("[[history]]").count(), 2, "{saved}");
    let again = molt_with_profile(&path, &["profile", "show"]);
    assert!(stderr(&again).is_empty(), "{}", stderr(&again));
    assert_eq!(row(&stdout(&again), "arc")[1..3], ["known", "manual"]);
}

#[test]
fn show_without_a_profile_prompts_for_molt_init() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("profile.toml");

    let output = molt_with_profile(&path, &["profile", "show"]);

    assert!(!output.status.success());
    assert!(stderr(&output).contains("molt init"), "{}", stderr(&output));
}

#[test]
fn set_without_a_profile_prompts_for_molt_init_and_creates_no_file() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("profile.toml");

    let output = molt_with_profile(&path, &["profile", "set", "clone", "known"]);

    assert!(!output.status.success());
    assert!(stderr(&output).contains("molt init"), "{}", stderr(&output));
    assert!(!path.exists());
}

#[test]
fn show_of_a_profile_that_cannot_be_read_fails_without_prompting_for_molt_init() {
    let (_dir, path) = empty_profile();
    fs::write(&path, "not_a_field = 1\n").unwrap();

    let output = molt_with_profile(&path, &["profile", "show"]);

    assert!(!output.status.success());
    let stderr = stderr(&output);
    assert!(stderr.contains("not_a_field"), "{stderr}");
    assert!(!stderr.contains("molt init"), "{stderr}");
}

#[test]
fn path_prints_the_path_given_with_the_profile_flag() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("elsewhere.toml");

    let output = molt_with_profile(&path, &["profile", "path"]);

    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(stdout(&output), format!("{}\n", path.display()));
}

#[test]
fn path_without_the_profile_flag_is_profile_toml_in_the_molt_directory() {
    let dir = TempDir::new().unwrap();

    let output = cargo_bin_cmd!("molt")
        .env("MOLT_HOME", dir.path())
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("HOME")
        .args(["profile", "path"])
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        format!("{}\n", dir.path().join("profile.toml").display())
    );
}

#[test]
fn show_without_the_profile_flag_reads_profile_toml_in_the_molt_directory() {
    let dir = TempDir::new().unwrap();
    fs::write(
        dir.path().join("profile.toml"),
        "[items.clone]\nstatus = \"seen\"\nprovenance = \"manual\"\nat = 2026-10-10T12:34:56Z\n",
    )
    .unwrap();

    let output = cargo_bin_cmd!("molt")
        .env("MOLT_HOME", dir.path())
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("HOME")
        .args(["profile", "show"])
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        row(&stdout(&output), "clone")[1..4],
        ["seen", "manual", "2026-10-10T12:34:56Z"]
    );
}

#[test]
fn show_does_not_print_control_characters_in_a_crate_key() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("profile.toml");
    fs::write(
        &path,
        "[items.\"crate:a\\u001b[31mplanted\"]\nstatus = \"seen\"\nprovenance = \"manual\"\nat = 2026-10-10T12:34:56Z\n",
    )
    .unwrap();
    let output = molt_with_profile(&path, &["profile", "show"]);
    let show = stdout(&output);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(show.contains("crate:a[31mplanted"), "{show:?}");
    assert!(!show.contains('\u{1b}'), "{show:?}");
}
