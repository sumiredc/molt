//! `molt init` (PRD UC1, §8.6), driven through the binary with `--ui-script`.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use assert_cmd::cargo::cargo_bin_cmd;
use tempfile::TempDir;

/// The steps that submit the form with "don't know" for all 10 categories and save at the
/// confirmation.
const ALL_DONT_KNOW: &str = "steps = [\n{ form = [\n{ choice = 3 }, { choice = 3 }, { choice = 3 }, { choice = 3 }, { choice = 3 },\n{ choice = 3 }, { choice = 3 }, { choice = 3 }, { choice = 3 }, { choice = 3 },\n] },\n{ input = \"\" },\n]\n";

/// The form step with `ownership` (the first tab) as given and "don't know" for the 9 others.
fn form_with_ownership(ownership: &str) -> String {
    format!(
        "{{ form = [\n{ownership},\n{{ choice = 3 }}, {{ choice = 3 }}, {{ choice = 3 }}, {{ choice = 3 }}, {{ choice = 3 }},\n{{ choice = 3 }}, {{ choice = 3 }}, {{ choice = 3 }}, {{ choice = 3 }},\n] }}"
    )
}

/// A molt directory with `config.toml` whose `[api]` body is `api`, and a ui script.
struct Setup {
    dir: TempDir,
}

impl Setup {
    fn with_api(api: &str) -> Self {
        let dir = TempDir::new().unwrap();
        let home = dir.path().join("molt-home");
        fs::create_dir(&home).unwrap();
        fs::write(home.join("config.toml"), format!("[api]\n{api}")).unwrap();
        fs::write(dir.path().join("ui.toml"), ALL_DONT_KNOW).unwrap();
        Self { dir }
    }

    fn home(&self) -> PathBuf {
        self.dir.path().join("molt-home")
    }

    fn profile(&self) -> PathBuf {
        self.dir.path().join("profile.toml")
    }

    fn script(&self) -> PathBuf {
        self.dir.path().join("ui.toml")
    }
}

/// Runs `molt init` with the profile at `profile` and the ui script of `setup`.
fn molt_init(setup: &Setup, profile: &Path) -> Output {
    molt_init_with(setup, profile, &[])
}

/// [`molt_init`] with `args` added.
fn molt_init_with(setup: &Setup, profile: &Path, args: &[&OsStr]) -> Output {
    cargo_bin_cmd!("molt")
        .env("MOLT_HOME", setup.home())
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("HOME")
        .env_remove("MOLT_TEST_MISSING_API_KEY")
        // The form in English, whatever the locale of the machine running the tests.
        .env_remove("LC_ALL")
        .env_remove("LC_MESSAGES")
        .env("LANG", "C")
        .arg("init")
        .arg("--profile")
        .arg(profile)
        .arg("--ui-script")
        .arg(setup.script())
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn init_with_every_category_dont_know_saves_the_profile_and_exits_0() {
    let setup = Setup::with_api("base_url = \"http://127.0.0.1:9/v1\"\napi_key_env = \"\"\n");
    let output = molt_init(&setup, &setup.profile());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(output.status.code(), Some(0), "{stdout}");
    assert!(stdout.contains("Saved the profile"), "{stdout}");
    let saved = fs::read_to_string(setup.profile()).unwrap();
    assert!(saved.contains("ownership = \"dont_know\""), "{saved}");
}

#[test]
fn init_without_the_api_key_reports_its_variable_and_creates_no_profile() {
    let setup = Setup::with_api("api_key_env = \"MOLT_TEST_MISSING_API_KEY\"\n");
    let output = molt_init(&setup, &setup.profile());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("`MOLT_TEST_MISSING_API_KEY` is not set"),
        "{stderr}"
    );
    assert!(!setup.profile().exists());
}

#[test]
fn init_interrupted_at_the_form_exits_130_and_creates_no_profile() {
    let setup = Setup::with_api("base_url = \"http://127.0.0.1:9/v1\"\napi_key_env = \"\"\n");
    fs::write(setup.script(), "steps = [\"interrupt\"]\n").unwrap();
    let output = molt_init(&setup, &setup.profile());
    assert_eq!(output.status.code(), Some(130));
    assert!(!setup.profile().exists());
}

#[test]
fn init_with_repo_saves_repo_scan_toml_beside_the_profile_and_marks_its_crates_known() {
    let setup = Setup::with_api("base_url = \"http://127.0.0.1:9/v1\"\napi_key_env = \"\"\n");
    let repo = setup.dir.path().join("repo");
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(repo.join("Cargo.toml"), "[dependencies]\ntokio = \"1\"\n").unwrap();
    fs::write(
        repo.join("src/main.rs"),
        "fn main() -> Option<u8> { None }\n",
    )
    .unwrap();

    let output = molt_init_with(
        &setup,
        &setup.profile(),
        &["--repo".as_ref(), repo.as_os_str()],
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(output.status.code(), Some(0), "{stdout}");
    assert!(stdout.contains("Took stock of"), "{stdout}");
    let saved = fs::read_to_string(setup.profile()).unwrap();
    assert!(saved.contains("\"crate:tokio\""), "{saved}");
    assert!(saved.contains("reference_repo"), "{saved}");
    let scan = fs::read_to_string(setup.dir.path().join("repo-scan.toml")).unwrap();
    assert!(scan.contains("src/main.rs"), "{scan}");
}

#[test]
fn init_with_a_missing_repo_reports_it_and_saves_the_profile_without_a_reference() {
    let setup = Setup::with_api("base_url = \"http://127.0.0.1:9/v1\"\napi_key_env = \"\"\n");
    let missing = setup.dir.path().join("no-such-repo");

    let output = molt_init_with(
        &setup,
        &setup.profile(),
        &["--repo".as_ref(), missing.as_os_str()],
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(output.status.code(), Some(0), "{stdout}");
    assert!(stdout.contains("does not exist"), "{stdout}");
    assert!(setup.profile().exists());
    assert!(!setup.dir.path().join("repo-scan.toml").exists());
}

#[test]
fn init_with_a_have_written_category_judges_its_answer_through_fake_and_saves_the_known_by_qa() {
    let setup = Setup::with_api("base_url = \"http://127.0.0.1:9/v1\"\napi_key_env = \"\"\n");
    let form = form_with_ownership(
        "{ choice = 1, checked = [1, 4], answer = \"I clone strings, and mem::take a Vec.\" }",
    );
    fs::write(
        setup.script(),
        format!("steps = [\n{form},\n{{ input = \"\" }},\n]\n"),
    )
    .unwrap();
    let fake = setup.dir.path().join("fake.toml");
    fs::write(
        &fake,
        r#"[[responses]]
when = "The category is `ownership`"
content = '{"elements": [{"element": "clone", "verdict": "known", "reason": "clones strings"}, {"element": "mem_take", "verdict": "unknown", "reason": "-"}]}'
"#,
    )
    .unwrap();

    let output = molt_init_with(
        &setup,
        &setup.profile(),
        &["--fake".as_ref(), fake.as_os_str()],
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(output.status.code(), Some(0), "{stdout}");
    assert!(
        stdout.contains("ownership: have written; checked: clone, mem_take;"),
        "{stdout}"
    );
    assert!(stdout.contains("ownership: known: clone;"), "{stdout}");
    let saved = fs::read_to_string(setup.profile()).unwrap();
    assert!(saved.contains("provenance = \"qa\""), "{saved}");
    assert!(saved.contains("clones strings"), "{saved}");
}

#[test]
fn init_shows_the_profile_and_saves_a_correction_typed_at_the_confirmation_by_manual() {
    let setup = Setup::with_api("base_url = \"http://127.0.0.1:9/v1\"\napi_key_env = \"\"\n");
    fs::write(
        setup.script(),
        ALL_DONT_KNOW.replace(
            "{ input = \"\" }",
            "{ input = \"clone known\" }, { input = \"\" }",
        ),
    )
    .unwrap();
    let output = molt_init(&setup, &setup.profile());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(output.status.code(), Some(0), "{stdout}");
    assert!(stdout.contains("The profile to save:"), "{stdout}");
    assert!(stdout.contains("correct> clone known"), "{stdout}");
    let saved = fs::read_to_string(setup.profile()).unwrap();
    assert!(saved.contains("[items.clone]"), "{saved}");
    assert!(saved.contains("provenance = \"manual\""), "{saved}");
}

#[test]
fn init_interrupted_at_the_confirmation_exits_130_and_creates_no_profile() {
    let setup = Setup::with_api("base_url = \"http://127.0.0.1:9/v1\"\napi_key_env = \"\"\n");
    fs::write(
        setup.script(),
        format!(
            "steps = [\n{},\n\"interrupt\",\n]\n",
            form_with_ownership("{ choice = 2 }")
        ),
    )
    .unwrap();
    let output = molt_init(&setup, &setup.profile());
    assert_eq!(output.status.code(), Some(130));
    assert!(!setup.profile().exists());
}
