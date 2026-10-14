//! `molt check [PATHS..]` (PRD §8.6), driven through the binary.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use assert_cmd::cargo::cargo_bin_cmd;
use tempfile::TempDir;

/// Runs `molt --profile <profile> check <args..>` in `dir`, with no molt
/// directory in the environment, so only `--profile` can decide where the
/// profile is.
fn check(dir: &Path, profile: &Path, args: &[&str]) -> Output {
    cargo_bin_cmd!("molt")
        .current_dir(dir)
        .env_remove("MOLT_HOME")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("HOME")
        .arg("--profile")
        .arg(profile)
        .arg("check")
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

/// Writes `files` (relative path, content) under a new temporary directory.
fn project(files: &[(&str, &str)]) -> TempDir {
    let dir = TempDir::new().unwrap();
    for (path, content) in files {
        let path = dir.path().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }
    dir
}

/// A profile file in `dir` holding `content`.
fn profile(dir: &TempDir, content: &str) -> PathBuf {
    let path = dir.path().join("profile.toml");
    fs::write(&path, content).unwrap();
    path
}

const NO_ELEMENT: &str = "fn main() {\n    let _ = 1;\n}\n";
const USES_DBG: &str = "fn main() {\n    dbg!(1);\n}\n";

#[test]
fn a_finding_is_printed_as_path_line_element_category_and_exits_1() {
    let dir = project(&[("a.rs", USES_DBG)]);
    let profile = profile(&dir, "");
    let output = check(dir.path(), &profile, &["a.rs"]);
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert_eq!(stdout(&output), "a.rs:2: dbg_macro (macros)\n");
}

#[test]
fn content_with_no_finding_prints_nothing_and_exits_0() {
    let dir = project(&[("a.rs", NO_ELEMENT)]);
    let profile = profile(&dir, "");
    let output = check(dir.path(), &profile, &["a.rs"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(stdout(&output), "");
}

#[test]
fn the_whole_file_is_checked_as_new_content() {
    let dir = project(&[(
        "a.rs",
        "fn f() {}\n\n\nfn main() {\n    dbg!(1);\n    dbg!(2);\n}\n",
    )]);
    let profile = profile(&dir, "");
    let output = check(dir.path(), &profile, &["a.rs"]);
    assert_eq!(
        stdout(&output),
        "a.rs:5: dbg_macro (macros)\na.rs:6: dbg_macro (macros)\n"
    );
}

#[test]
fn a_crate_is_printed_under_crates() {
    let dir = project(&[("a.rs", "fn main() {\n    tokio::spawn(f());\n}\n")]);
    let profile = profile(&dir, "");
    let output = check(dir.path(), &profile, &["a.rs"]);
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert_eq!(stdout(&output), "a.rs:2: crate:tokio (crates)\n");
}

#[test]
fn the_profile_decides_what_is_reported() {
    let dir = project(&[("a.rs", USES_DBG)]);
    let profile = profile(
        &dir,
        "[items.dbg_macro]\nstatus = \"known\"\nprovenance = \"manual\"\nat = 2026-10-10T12:34:56Z\n",
    );
    let output = check(dir.path(), &profile, &["a.rs"]);
    assert_eq!(output.status.code(), Some(0), "{}", stdout(&output));
}

#[test]
fn an_element_in_allow_of_an_older_profile_is_known_and_the_note_says_so() {
    let dir = project(&[("a.rs", USES_DBG)]);
    let older = "allow = [\"dbg_macro\"]\n";
    let profile = profile(&dir, older);
    let output = check(dir.path(), &profile, &["a.rs"]);
    assert_eq!(output.status.code(), Some(0), "{}", stdout(&output));
    let note = stderr(&output);
    assert!(
        note.contains("dbg_macro was in `allow`") && note.contains("it is `known` now (manual)"),
        "{note}"
    );
    assert_eq!(
        fs::read_to_string(&profile).unwrap(),
        older,
        "check only reads"
    );
}

#[test]
fn without_a_profile_it_says_so_on_stderr_and_checks_with_nothing_known() {
    let dir = project(&[("base.rs", NO_ELEMENT), ("dbg.rs", USES_DBG)]);
    let missing = dir.path().join("missing.toml");

    let base = check(dir.path(), &missing, &["base.rs"]);
    assert_eq!(base.status.code(), Some(0), "{}", stdout(&base));
    assert!(
        stderr(&base).contains("checking with nothing known"),
        "{}",
        stderr(&base)
    );
    assert!(stderr(&base).contains("missing.toml"), "{}", stderr(&base));

    let dbg = check(dir.path(), &missing, &["dbg.rs"]);
    assert_eq!(dbg.status.code(), Some(1));
    assert_eq!(stdout(&dbg), "dbg.rs:2: dbg_macro (macros)\n");
    assert!(!missing.exists(), "check must not create the profile");
}

#[test]
fn a_profile_that_cannot_be_read_stops_the_check_with_exit_2() {
    let dir = project(&[("a.rs", NO_ELEMENT)]);
    let profile = profile(&dir, "not toml [");
    let output = check(dir.path(), &profile, &["a.rs"]);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(stdout(&output), "");
}

#[test]
fn a_file_name_directly_under_src_is_an_own_module() {
    let dir = project(&[
        ("Cargo.toml", "[package]\nname = \"p\"\n"),
        ("src/util.rs", "pub fn f() {}\n"),
        (
            "src/a.rs",
            "pub fn g() {\n    util::f();\n    other::f();\n}\n",
        ),
    ]);
    let profile = profile(&dir, "");
    let output = check(dir.path(), &profile, &["src/a.rs"]);
    assert_eq!(stdout(&output), "src/a.rs:3: crate:other (crates)\n");
}

#[test]
fn a_directory_module_directly_under_src_is_an_own_module() {
    let dir = project(&[
        ("Cargo.toml", "[package]\nname = \"p\"\n"),
        ("src/util/mod.rs", "pub fn f() {}\n"),
        ("src/a.rs", "pub fn g() {\n    util::f();\n}\n"),
    ]);
    let profile = profile(&dir, "");
    let output = check(dir.path(), &profile, &["src/a.rs"]);
    assert_eq!(output.status.code(), Some(0), "{}", stdout(&output));
}

#[test]
fn a_mod_declared_in_the_crate_root_is_an_own_module() {
    let dir = project(&[
        ("Cargo.toml", "[package]\nname = \"p\"\n"),
        (
            "src/main.rs",
            "#[path = \"impls/x.rs\"]\nmod renamed;\nmod a;\n",
        ),
        ("src/impls/x.rs", "pub fn f() {}\n"),
        ("src/a.rs", "pub fn g() {\n    renamed::f();\n}\n"),
    ]);
    let profile = profile(&dir, "");
    let output = check(dir.path(), &profile, &["src/a.rs"]);
    assert_eq!(output.status.code(), Some(0), "{}", stdout(&output));
}

#[test]
fn the_own_package_name_with_dashes_as_underscores_is_not_a_crate() {
    let dir = project(&[
        ("Cargo.toml", "[package]\nname = \"my-pkg\"\n"),
        ("src/lib.rs", "pub fn f() {}\n"),
        ("src/main.rs", "fn main() {\n    my_pkg::f();\n}\n"),
    ]);
    let profile = profile(&dir, "");
    let output = check(dir.path(), &profile, &["src/main.rs"]);
    assert_eq!(output.status.code(), Some(0), "{}", stdout(&output));
}

#[test]
fn the_own_lib_name_is_not_a_crate() {
    let dir = project(&[
        (
            "Cargo.toml",
            "[package]\nname = \"p\"\n\n[lib]\nname = \"core_lib\"\n",
        ),
        ("src/lib.rs", "pub fn f() {}\n"),
        ("src/main.rs", "fn main() {\n    core_lib::f();\n}\n"),
    ]);
    let profile = profile(&dir, "");
    let output = check(dir.path(), &profile, &["src/main.rs"]);
    assert_eq!(output.status.code(), Some(0), "{}", stdout(&output));
}

#[test]
fn a_file_outside_src_gets_the_package_name_but_not_the_modules_under_src() {
    let dir = project(&[
        ("Cargo.toml", "[package]\nname = \"p\"\n"),
        ("src/lib.rs", "pub mod config;\n"),
        ("src/config.rs", "pub fn f() {}\n"),
        (
            "tests/t.rs",
            "fn t() {\n    p::config::f();\n    config::load();\n}\n",
        ),
    ]);
    let profile = profile(&dir, "");
    let output = check(dir.path(), &profile, &["tests/t.rs"]);
    assert_eq!(stdout(&output), "tests/t.rs:3: crate:config (crates)\n");
}

#[test]
fn a_binary_under_src_bin_gets_the_package_name_but_not_the_modules_under_src() {
    let dir = project(&[
        ("Cargo.toml", "[package]\nname = \"p\"\n"),
        ("src/lib.rs", "pub mod config;\n"),
        ("src/config.rs", "pub fn f() {}\n"),
        (
            "src/bin/tool.rs",
            "fn main() {\n    p::config::f();\n    config::load();\n}\n",
        ),
    ]);
    let profile = profile(&dir, "");
    let output = check(dir.path(), &profile, &["src/bin/tool.rs"]);
    assert_eq!(
        stdout(&output),
        "src/bin/tool.rs:3: crate:config (crates)\n"
    );
}

#[test]
fn own_modules_are_found_from_a_path_given_inside_the_project() {
    let dir = project(&[
        ("Cargo.toml", "[package]\nname = \"p\"\n"),
        ("src/util.rs", "pub fn f() {}\n"),
        ("src/a.rs", "pub fn g() {\n    util::f();\n}\n"),
    ]);
    let profile = profile(&dir, "");
    let output = check(&dir.path().join("src"), &profile, &["a.rs"]);
    assert_eq!(output.status.code(), Some(0), "{}", stdout(&output));
}

#[test]
fn every_dependency_of_a_cargo_toml_is_new_content() {
    let dir = project(&[(
        "Cargo.toml",
        "[package]\nname = \"p\"\n\n[dependencies]\nserde = \"1\"\n",
    )]);
    let profile = profile(&dir, "");
    let output = check(dir.path(), &profile, &["Cargo.toml"]);
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert_eq!(stdout(&output), "Cargo.toml:5: crate:serde (crates)\n");
}

#[test]
fn a_directory_is_checked_through_its_rust_files_and_manifests() {
    let dir = project(&[
        (
            "Cargo.toml",
            "[package]\nname = \"p\"\n\n[dependencies]\nserde = \"1\"\n",
        ),
        ("src/b.rs", USES_DBG),
        ("src/a.rs", USES_DBG),
        ("src/notes.txt", "dbg!(1)\n"),
        ("target/debug/build.rs", USES_DBG),
        (".git/x.rs", USES_DBG),
    ]);
    let profile = profile(&dir, "");
    let output = check(dir.path(), &profile, &["."]);
    let expected = [
        "./Cargo.toml:5: crate:serde (crates)",
        "./src/a.rs:2: dbg_macro (macros)",
        "./src/b.rs:2: dbg_macro (macros)",
    ];
    assert_eq!(stdout(&output), expected.map(|l| format!("{l}\n")).concat());
}

#[test]
fn a_target_directory_is_skipped_only_next_to_a_cargo_toml() {
    let dir = project(&[
        ("Cargo.toml", "[package]\nname = \"p\"\n"),
        ("target/debug/build.rs", USES_DBG),
        ("src/target/mod.rs", USES_DBG),
    ]);
    let profile = profile(&dir, "");
    let output = check(dir.path(), &profile, &["."]);
    assert_eq!(
        stdout(&output),
        "./src/target/mod.rs:2: dbg_macro (macros)\n"
    );
}

#[cfg(unix)]
#[test]
fn a_symbolic_link_to_a_directory_is_not_followed() {
    let dir = project(&[("src/a.rs", USES_DBG)]);
    std::os::unix::fs::symlink("..", dir.path().join("src/up")).unwrap();
    let profile = profile(&dir, "");
    let output = check(dir.path(), &profile, &["."]);
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert_eq!(stdout(&output), "./src/a.rs:2: dbg_macro (macros)\n");
}

#[cfg(unix)]
#[test]
fn an_entry_that_cannot_be_read_is_reported_and_the_rest_is_still_checked() {
    let dir = project(&[("src/a.rs", USES_DBG), ("src/z.rs", USES_DBG)]);
    std::os::unix::fs::symlink("nowhere", dir.path().join("src/m.rs")).unwrap();
    let profile = profile(&dir, "");
    let output = check(dir.path(), &profile, &["."]);
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("src/m.rs"), "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        "./src/a.rs:2: dbg_macro (macros)\n./src/z.rs:2: dbg_macro (macros)\n"
    );
}

#[cfg(unix)]
#[test]
fn a_broken_link_that_is_not_a_checked_file_does_not_stop_the_check() {
    let dir = project(&[("src/a.rs", USES_DBG)]);
    std::os::unix::fs::symlink("nowhere", dir.path().join("README.md")).unwrap();
    std::os::unix::fs::symlink("user@host.1:2", dir.path().join("src/.#a.rs")).unwrap();
    let profile = profile(&dir, "");
    let output = check(dir.path(), &profile, &["."]);
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert_eq!(stdout(&output), "./src/a.rs:2: dbg_macro (macros)\n");
}

#[test]
fn without_paths_the_current_directory_is_checked() {
    let dir = project(&[("src/a.rs", USES_DBG)]);
    let profile = profile(&dir, "");
    let output = check(dir.path(), &profile, &[]);
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert_eq!(stdout(&output), "./src/a.rs:2: dbg_macro (macros)\n");
}

#[test]
fn a_file_that_does_not_parse_is_reported_on_stderr_with_exit_2() {
    let dir = project(&[("bad.rs", "fn main( {\n"), ("a.rs", USES_DBG)]);
    let profile = profile(&dir, "");
    let output = check(dir.path(), &profile, &["bad.rs", "a.rs"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(
        stderr(&output).starts_with("bad.rs:"),
        "{}",
        stderr(&output)
    );
    assert_eq!(stdout(&output), "a.rs:2: dbg_macro (macros)\n");
}

#[test]
fn a_missing_path_is_reported_on_stderr_with_exit_2() {
    let dir = project(&[("a.rs", NO_ELEMENT)]);
    let profile = profile(&dir, "");
    let output = check(dir.path(), &profile, &["nope.rs", "a.rs"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("nope.rs"), "{}", stderr(&output));
}
