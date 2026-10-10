use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

use crate::config::Config;
use crate::llm::{ChatError, FakeChatClient, Router};
use crate::onboarding::probe::Model;
use crate::onboarding::questionnaire::{Outcome, init};
use crate::profile::element::Category;
use crate::profile::status::{ItemKey, Provenance, Status};
use crate::profile::store::{Profile, Timestamp};
use crate::repo_scan::*;
use crate::ui::{ScriptedUi, Step};

const DONT_KNOW: Step = Step::Select(3);

/// A temporary directory holding a reference repository (`repo/`) and molt's place (`home/`).
struct Setup {
    dir: TempDir,
}

impl Setup {
    fn new() -> Self {
        let dir = TempDir::new().unwrap();
        fs::create_dir(dir.path().join("repo")).unwrap();
        Self { dir }
    }

    fn repo(&self) -> PathBuf {
        self.dir.path().join("repo")
    }

    fn profile(&self) -> PathBuf {
        self.dir.path().join("home").join("profile.toml")
    }

    fn scan_file(&self) -> PathBuf {
        self.dir.path().join("home").join(FILE_NAME)
    }

    /// Writes `contents` at `path` under the repository, creating its directories.
    fn write(&self, path: &str, contents: &str) {
        let path = self.repo().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }
}

/// The steps that answer every category with `step`, and then save at the confirmation (an
/// empty line).
fn every_category(step: Step) -> Vec<Step> {
    let mut steps = vec![step; Category::ALL.len()];
    steps.push(Step::Input(String::new()));
    steps
}

/// Runs `molt init` answering "don't know" everywhere, with `repo` as the reference repository.
fn init_with(setup: &Setup, repo: Option<&Path>, steps: Vec<Step>) -> (ScriptedUi, Outcome) {
    let mut ui = ScriptedUi::new(steps);
    let outcome = init(
        &mut ui,
        &setup.profile(),
        model(&FakeChatClient::new()),
        repo,
    )
    .unwrap();
    (ui, outcome)
}

fn model(client: &FakeChatClient) -> Result<Model<'_>, String> {
    static ROUTER: std::sync::LazyLock<Router> =
        std::sync::LazyLock::new(|| Router::new(&Config::default()));
    Ok(Model {
        client,
        router: &ROUTER,
    })
}

fn crate_entry(profile: &Profile, name: &str) -> (Status, Provenance) {
    let entry = profile
        .entry(&ItemKey::Crate(name.to_owned()))
        .unwrap_or_else(|| panic!("no entry for crate:{name}"));
    (entry.status(), entry.provenance())
}

fn saved_scan(setup: &Setup) -> RepoScan {
    from_toml(&fs::read_to_string(setup.scan_file()).unwrap()).unwrap()
}

fn place(path: &str, line: usize) -> Place {
    Place {
        path: path.to_owned(),
        line,
    }
}

fn at() -> Timestamp {
    Timestamp::from_unix_seconds(1_791_590_400)
}

const MANIFEST: &str = r#"[package]
name = "app"

[dependencies]
tokio = "1"
serde-json = "1"
json5 = { package = "json-five", version = "1" }

[dev-dependencies]
tempfile = "3"
"#;

#[test]
fn the_dependencies_of_cargo_toml_become_known_by_the_reference_repository() {
    let setup = Setup::new();
    setup.write("Cargo.toml", MANIFEST);
    setup.write("src/main.rs", "fn main() {}\n");
    init_with(&setup, Some(&setup.repo()), every_category(DONT_KNOW));

    let profile = Profile::load(&setup.profile()).unwrap();
    let known = (Status::Known, Provenance::ReferenceRepo);
    assert_eq!(crate_entry(&profile, "tokio"), known);
    assert_eq!(crate_entry(&profile, "serde_json"), known);
    assert_eq!(crate_entry(&profile, "json_five"), known);
    assert_eq!(crate_entry(&profile, "tempfile"), known);
    let crates = profile
        .entries()
        .filter(|(key, _)| matches!(key, ItemKey::Crate(_)))
        .count();
    assert_eq!(crates, 4);
}

#[test]
fn the_dependencies_of_workspace_members_become_known_too() {
    let setup = Setup::new();
    setup.write(
        "Cargo.toml",
        "[workspace]\nmembers = [\"cli\", \"crates/*\"]\n\n[workspace.dependencies]\nanyhow = \"1\"\n",
    );
    setup.write("cli/Cargo.toml", "[dependencies]\nclap = \"4\"\n");
    setup.write("crates/core/Cargo.toml", "[dependencies]\nregex = \"1\"\n");
    setup.write("unlisted/Cargo.toml", "[dependencies]\nrand = \"0.9\"\n");

    let scan = scan(&setup.repo(), at()).unwrap();
    assert_eq!(scan.crates, ["anyhow", "clap", "regex"]);
}

#[test]
fn a_missing_path_is_reported_and_init_goes_on_without_a_reference() {
    let setup = Setup::new();
    let missing = setup.dir.path().join("no-such-repo");
    let (ui, outcome) = init_with(&setup, Some(&missing), every_category(DONT_KNOW));

    assert_eq!(outcome, Outcome::Saved);
    assert!(
        ui.shown().iter().any(|line| line.contains("does not exist")
            && line.contains("going on without a reference repository")),
        "{:?}",
        ui.shown()
    );
    let profile = Profile::load(&setup.profile()).unwrap();
    assert_eq!(profile.entries().count(), 0);
    assert!(!setup.scan_file().exists());
}

#[test]
fn a_path_that_is_not_a_cargo_project_is_reported_and_init_goes_on_without_a_reference() {
    let setup = Setup::new();
    setup.write("src/main.rs", "fn main() {}\n");
    let (ui, outcome) = init_with(&setup, Some(&setup.repo()), every_category(DONT_KNOW));

    assert_eq!(outcome, Outcome::Saved);
    assert!(
        ui.shown()
            .iter()
            .any(|line| line.contains("is not a cargo project")
                && line.contains("going on without a reference repository")),
        "{:?}",
        ui.shown()
    );
    let profile = Profile::load(&setup.profile()).unwrap();
    assert_eq!(profile.entries().count(), 0);
    assert!(!setup.scan_file().exists());
}

#[test]
fn a_cargo_toml_that_is_not_toml_is_reported_and_init_goes_on_without_a_reference() {
    let setup = Setup::new();
    setup.write("Cargo.toml", "[dependencies\ntokio = \"1\"\n");
    let (ui, outcome) = init_with(&setup, Some(&setup.repo()), every_category(DONT_KNOW));

    assert_eq!(outcome, Outcome::Saved);
    assert!(
        ui.shown()
            .iter()
            .any(|line| line.contains("cannot read") && line.contains("Cargo.toml")),
        "{:?}",
        ui.shown()
    );
    let profile = Profile::load(&setup.profile()).unwrap();
    assert_eq!(profile.entries().count(), 0);
}

#[test]
fn repo_scan_toml_keeps_the_path_and_line_of_up_to_three_places_per_element() {
    let setup = Setup::new();
    setup.write("Cargo.toml", MANIFEST);
    setup.write(
        "src/a.rs",
        "use std::sync::Arc;\n\nfn one() -> Arc<u8> {\n    Arc::new(1)\n}\n",
    );
    setup.write(
        "src/b.rs",
        "use std::sync::Arc;\nfn two() -> Arc<u8> {\n    Arc::new(2)\n}\n",
    );
    init_with(&setup, Some(&setup.repo()), every_category(DONT_KNOW));

    let scan = saved_scan(&setup);
    assert_eq!(
        scan.elements["arc"],
        [
            place("src/a.rs", 1),
            place("src/a.rs", 3),
            place("src/a.rs", 4)
        ]
    );
    assert!(
        scan.elements
            .values()
            .all(|places| places.len() <= MAX_PLACES)
    );
    assert_eq!(
        scan.repo,
        fs::canonicalize(setup.repo()).unwrap().to_string_lossy()
    );
    assert_eq!(
        scan.crates,
        ["json_five", "serde_json", "tempfile", "tokio"]
    );
}

#[test]
fn files_under_target_and_hidden_directories_are_not_read() {
    let setup = Setup::new();
    setup.write("Cargo.toml", MANIFEST);
    setup.write("target/debug/build/out.rs", "static mut A: u8 = 0;\n");
    setup.write(".git/hooks/x.rs", "static mut A: u8 = 0;\n");
    setup.write("src/lib.rs", "pub fn f() {}\n");

    let scan = scan(&setup.repo(), at()).unwrap();
    assert!(
        !scan.elements.contains_key("static_mut"),
        "{:?}",
        scan.elements
    );
    assert!(
        scan.elements
            .values()
            .flatten()
            .all(|p| p.path == "src/lib.rs")
    );
}

#[test]
fn a_file_that_does_not_parse_is_left_out_and_listed_as_skipped() {
    let setup = Setup::new();
    setup.write("Cargo.toml", MANIFEST);
    setup.write("src/broken.rs", "fn (\n");
    setup.write("src/lib.rs", "pub fn f() -> Option<u8> { None }\n");

    let scan = scan(&setup.repo(), at()).unwrap();
    assert_eq!(scan.skipped, ["src/broken.rs"]);
    assert_eq!(scan.elements["option"], [place("src/lib.rs", 1)]);
}

#[test]
fn repo_scan_toml_is_beside_the_profile_given_with_profile() {
    let profile = Path::new("/somewhere/else/my-profile.toml");
    assert_eq!(
        path_beside(profile),
        Path::new("/somewhere/else/repo-scan.toml")
    );
}

#[test]
fn a_saved_repo_scan_toml_reads_back_the_same() {
    let setup = Setup::new();
    setup.write("Cargo.toml", MANIFEST);
    setup.write("src/lib.rs", "pub fn f() -> Option<u8> { None }\n");
    let scan = scan(&setup.repo(), at()).unwrap();
    assert_eq!(from_toml(&to_toml(&scan)).unwrap(), scan);
}

// The file is written or removed only together with a saved profile.

/// A `repo-scan.toml` from an earlier `molt init`, and the profile saved with it.
fn earlier_init(setup: &Setup) -> String {
    setup.write("Cargo.toml", MANIFEST);
    setup.write("src/lib.rs", "pub fn f() -> Option<u8> { None }\n");
    init_with(setup, Some(&setup.repo()), every_category(DONT_KNOW));
    fs::read_to_string(setup.scan_file()).unwrap()
}

#[test]
fn declining_to_overwrite_the_profile_leaves_repo_scan_toml_as_it_was() {
    let setup = Setup::new();
    let earlier = earlier_init(&setup);
    setup.write("Cargo.toml", "[dependencies]\nrand = \"0.9\"\n");

    let (_, outcome) = init_with(&setup, Some(&setup.repo()), vec![Step::Confirm(false)]);
    assert_eq!(outcome, Outcome::Kept);
    assert_eq!(fs::read_to_string(setup.scan_file()).unwrap(), earlier);
}

#[test]
fn interrupting_init_leaves_repo_scan_toml_as_it_was() {
    let setup = Setup::new();
    let earlier = earlier_init(&setup);
    setup.write("Cargo.toml", "[dependencies]\nrand = \"0.9\"\n");

    let mut steps = vec![Step::Confirm(true)];
    steps.extend(every_category(DONT_KNOW).into_iter().take(3));
    steps.push(Step::Interrupt);
    let (_, outcome) = init_with(&setup, Some(&setup.repo()), steps);
    assert_eq!(outcome, Outcome::Interrupted);
    assert_eq!(fs::read_to_string(setup.scan_file()).unwrap(), earlier);
}

#[test]
fn a_failure_before_the_save_leaves_repo_scan_toml_as_it_was() {
    let setup = Setup::new();
    let earlier = earlier_init(&setup);
    let earlier_profile = fs::read_to_string(setup.profile()).unwrap();
    setup.write("Cargo.toml", "[dependencies]\nrand = \"0.9\"\n");

    // "have written" for the first category, whose question fails at the API.
    let mut steps = vec![Step::Confirm(true), Step::Select(1)];
    steps.extend(every_category(DONT_KNOW).into_iter().skip(1));
    let client = FakeChatClient::new();
    client.push_chat(Err(ChatError::Status {
        status: 429,
        body: "rate limited".to_owned(),
    }));
    let mut ui = ScriptedUi::new(steps);
    let result = init(
        &mut ui,
        &setup.profile(),
        model(&client),
        Some(&setup.repo()),
    );
    let error = result.unwrap_err();
    assert!(error.contains("429"), "{error}");
    assert_eq!(fs::read_to_string(setup.scan_file()).unwrap(), earlier);
    assert_eq!(
        fs::read_to_string(setup.profile()).unwrap(),
        earlier_profile
    );
}

#[test]
fn init_again_without_a_reference_removes_the_earlier_repo_scan_toml() {
    let setup = Setup::new();
    earlier_init(&setup);

    let mut steps = vec![Step::Confirm(true)];
    steps.extend(every_category(DONT_KNOW));
    let (ui, outcome) = init_with(&setup, None, steps);
    assert_eq!(outcome, Outcome::Saved);
    assert!(!setup.scan_file().exists());
    assert!(
        ui.shown().iter().any(|line| line.starts_with("Removed")),
        "{:?}",
        ui.shown()
    );
    let profile = Profile::load(&setup.profile()).unwrap();
    assert_eq!(profile.entries().count(), 0);
}

#[test]
fn init_again_with_a_repo_that_cannot_be_used_removes_the_earlier_repo_scan_toml() {
    let setup = Setup::new();
    earlier_init(&setup);
    let missing = setup.dir.path().join("no-such-repo");

    let mut steps = vec![Step::Confirm(true)];
    steps.extend(every_category(DONT_KNOW));
    let (_, outcome) = init_with(&setup, Some(&missing), steps);
    assert_eq!(outcome, Outcome::Saved);
    assert!(!setup.scan_file().exists());
}

#[test]
fn init_again_with_a_reference_replaces_the_earlier_repo_scan_toml() {
    let setup = Setup::new();
    earlier_init(&setup);
    setup.write("Cargo.toml", "[dependencies]\nrand = \"0.9\"\n");

    let mut steps = vec![Step::Confirm(true)];
    steps.extend(every_category(DONT_KNOW));
    init_with(&setup, Some(&setup.repo()), steps);
    assert_eq!(saved_scan(&setup).crates, ["rand"]);
}

#[test]
fn a_repo_scan_toml_molt_did_not_write_stops_init_before_anything_is_saved() {
    let setup = Setup::new();
    setup.write("Cargo.toml", MANIFEST);
    fs::create_dir_all(setup.scan_file().parent().unwrap()).unwrap();
    let theirs = "# my own notes\nfoo = 1\n";
    fs::write(setup.scan_file(), theirs).unwrap();

    let mut ui = ScriptedUi::new(every_category(DONT_KNOW));
    let result = init(
        &mut ui,
        &setup.profile(),
        model(&FakeChatClient::new()),
        Some(&setup.repo()),
    );
    let error = result.unwrap_err();
    assert!(error.contains("not a stock-taking molt wrote"), "{error}");
    assert!(ui.asked().is_empty(), "{:?}", ui.asked());
    assert_eq!(fs::read_to_string(setup.scan_file()).unwrap(), theirs);
    assert!(!setup.profile().exists());
}

#[cfg(unix)]
#[test]
fn a_repo_scan_toml_that_is_a_symbolic_link_stops_init_before_anything_is_saved() {
    let setup = Setup::new();
    setup.write("Cargo.toml", MANIFEST);
    let target = setup.dir.path().join("elsewhere.toml");
    fs::write(&target, "kept\n").unwrap();
    fs::create_dir_all(setup.scan_file().parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&target, setup.scan_file()).unwrap();

    let mut ui = ScriptedUi::new(every_category(DONT_KNOW));
    let result = init(
        &mut ui,
        &setup.profile(),
        model(&FakeChatClient::new()),
        Some(&setup.repo()),
    );
    let error = result.unwrap_err();
    assert!(error.contains("symbolic link"), "{error}");
    assert!(
        fs::symlink_metadata(setup.scan_file())
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read_to_string(&target).unwrap(), "kept\n");
    assert!(!setup.profile().exists());
}
