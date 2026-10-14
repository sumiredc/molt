//! The explanation the user is shown on opening the side line (PRD UC3): the items of the step,
//! each once, with their category (PRD §11), then what they do in the step's lines, or for those
//! the user has seen before a note that their idea is not explained again (PRD §7), then a
//! comparison of the 2 ways to write the lines, the step's lines with the items and the lines the
//! `reason` model wrote without them, then where the reference repository uses the items, from
//! the stock-taking `molt init` kept beside the profile.

use std::fs;
use std::io;
use std::path::Path;

use crate::agent::mainline::Boundary;
use crate::checker;
use crate::checker::crates::crate_key;
use crate::profile::element::Element;
use crate::profile::status::{ItemKey, Status};
use crate::profile::store::Profile;
use crate::repo_scan::{self, MAX_PLACES, RepoScan};

/// Shown in place of the code without the items when the reply has none.
pub(crate) const HARD_WITHIN_PROFILE: &str =
    "Without them, within your profile: hard to write. The model found no good way to.";

/// The items of `items` the user has seen before: `seen` in `profile` (PRD §7). A crate is seen
/// when an entry of any spelling the checker takes for it ([`crate_key`]) is.
pub(crate) fn seen(profile: &Profile, items: &[ItemKey]) -> Vec<ItemKey> {
    items
        .iter()
        .filter(|item| match item {
            ItemKey::Element(_) => profile.status(item) == Status::Seen,
            ItemKey::Crate(name) => profile.entries().any(|(key, entry)| {
                matches!(key, ItemKey::Crate(other) if crate_key(other) == crate_key(name))
                    && entry.status() == Status::Seen
            }),
        })
        .cloned()
        .collect()
}

/// The note in place of the idea of `seen`, items the user has seen before.
pub(crate) fn seen_before(seen: &[ItemKey]) -> String {
    format!(
        "You have seen {} before, so the idea is not explained again.",
        names(seen)
    )
}

/// The first line of the explanation: each item once, then their category, `crates` for crates.
/// The items of one step may span categories (PRD §2, §11).
fn heading(items: &[ItemKey]) -> String {
    let mut once: Vec<ItemKey> = Vec::new();
    let mut categories: Vec<&str> = Vec::new();
    for item in items {
        if once.contains(item) {
            continue;
        }
        once.push(item.clone());
        let category = match item {
            ItemKey::Element(element) => element.category().name(),
            ItemKey::Crate(_) => "crates",
        };
        if !categories.contains(&category) {
            categories.push(category);
        }
    }
    format!("{} ({})", names(&once), categories.join(", "))
}

/// The items `profile` lets the code use, as the checker decides (PRD §8.4): the `known`
/// elements, in the order of [`Element::ALL`], then the `known` crates.
pub(crate) fn allowed(profile: &Profile) -> Vec<ItemKey> {
    let elements = Element::ALL
        .iter()
        .map(|element| ItemKey::Element(*element));
    let crates = profile
        .entries()
        .map(|(key, _)| key)
        .filter(|key| matches!(key, ItemKey::Crate(_)))
        .cloned();
    elements
        .chain(crates)
        .filter(|item| checker::allows(profile, item))
        .collect()
}

/// The explanation the user is shown, from the `reason` model's `reply` to the request for it:
/// the items of `boundary` ([`heading`]), the note on those of them in `seen` ([`seen_before`]),
/// the prose of the reply, then the step's lines at `boundary` and the first code block of the
/// reply, the lines without the items. When the reply has no code block, or an empty one,
/// [`HARD_WITHIN_PROFILE`] stands in for it. `similar`, where the reference repository uses the
/// items ([`similar`]), comes last when there is one.
pub(crate) fn assemble(
    boundary: &Boundary,
    seen: &[ItemKey],
    reply: &str,
    similar: Option<&str>,
) -> String {
    let mut text = heading(&boundary.items);
    text.push_str("\n\n");
    if !seen.is_empty() {
        text.push_str(&seen_before(seen));
        text.push_str("\n\n");
    }
    text.push_str(&compare(boundary, reply));
    if let Some(similar) = similar {
        text.push_str("\n\n");
        text.push_str(similar);
    }
    text
}

/// Where the reference repository uses the elements of `items` ([`similar`]), from the
/// stock-taking `molt init` kept beside the profile at `profile_path`, read as it is: the
/// reference repository is not scanned again. `None` when there is no stock-taking or `items` has
/// no element. When there is one that cannot be read, says so ([`unreadable`]).
pub(crate) fn similar_beside(profile_path: &Path, items: &[ItemKey]) -> Option<String> {
    let path = repo_scan::path_beside(profile_path);
    let scan = match fs::read_to_string(&path) {
        Ok(text) => repo_scan::from_toml(&text),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return None,
        Err(error) => Err(error.to_string()),
    };
    match scan {
        Ok(scan) => similar(&scan, items),
        Err(_) => items
            .iter()
            .any(|item| matches!(item, ItemKey::Element(_)))
            .then(|| unreadable(&path)),
    }
}

/// Shown in place of the places when the stock-taking at `path` cannot be read. `molt init` stops
/// at such a file rather than replacing it (PRD §8.3), so the user is told to move it away first.
fn unreadable(path: &Path) -> String {
    format!(
        "Where your reference repository uses them: {} could not be read. Move it away and run \
         `molt init` again.",
        path.display()
    )
}

/// Where the reference repository of `scan` uses each element of `items`, in their order: at most
/// [`MAX_PLACES`] places each, as path and line, said to be what `molt init` recorded. Each
/// element comes once, and one the repository does not use is said to be so. `None` when `items`
/// has no element: the stock-taking keeps no places for crates.
pub(crate) fn similar(scan: &RepoScan, items: &[ItemKey]) -> Option<String> {
    let mut elements: Vec<Element> = Vec::new();
    for item in items {
        if let ItemKey::Element(element) = item
            && !elements.contains(element)
        {
            elements.push(*element);
        }
    }
    if elements.is_empty() {
        return None;
    }
    let mut text = format!(
        "Where your reference repository uses them\n\
         As `molt init` recorded {} at {}, not scanned again:",
        scan.repo, scan.at
    );
    for element in elements {
        let name = element.name();
        let places = scan.elements.get(name).map_or(&[][..], Vec::as_slice);
        if places.is_empty() {
            text.push_str(&format!("\n`{name}`: not used there."));
            continue;
        }
        text.push_str(&format!("\n`{name}`:"));
        for place in places.iter().take(MAX_PLACES) {
            text.push_str(&format!("\n  {}:{}", place.path, place.line));
        }
    }
    Some(text)
}

/// The idea and the comparison of [`assemble`].
fn compare(boundary: &Boundary, reply: &str) -> String {
    let attempted = boundary.attempted_code();
    let (prose, code) = split(reply);
    let mut text = String::new();
    if !prose.is_empty() {
        text.push_str(&prose);
        text.push_str("\n\n");
    }
    text.push_str(&format!(
        "Two ways to write it\nWith {} (the step molt wrote):\n{}\n",
        names(&boundary.items),
        rust_block(attempted)
    ));
    match code {
        Some(code) => {
            text.push_str("Without them, within your profile:\n");
            text.push_str(&rust_block(&code));
        }
        None => text.push_str(HARD_WITHIN_PROFILE),
    }
    text
}

/// The prose of `reply`, everything outside its first code block, and the code in that block,
/// `None` when there is no block or nothing in it. A block left open runs to the end.
fn split(reply: &str) -> (String, Option<String>) {
    let lines: Vec<&str> = reply.lines().collect();
    let is_fence = |line: &&str| line.trim_start().starts_with("```");
    let Some(open) = lines.iter().position(is_fence) else {
        return (reply.trim().to_owned(), None);
    };
    let close = lines[open + 1..]
        .iter()
        .position(is_fence)
        .map_or(lines.len(), |at| open + 1 + at);
    let before = lines[..open].join("\n");
    let after = lines.get(close + 1..).unwrap_or_default().join("\n");
    let prose = [before.trim(), after.trim()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    let code = lines[open + 1..close].join("\n");
    let code = (!code.trim().is_empty()).then(|| code.trim_end().to_owned());
    (prose, code)
}

/// `code` in a ```` ```rust ```` block.
fn rust_block(code: &str) -> String {
    format!("```rust\n{}\n```", code.trim_end())
}

/// `` `dbg_macro`, `crate:tokio` ``.
fn names(items: &[ItemKey]) -> String {
    items
        .iter()
        .map(|item| format!("`{item}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Tests of what the `reason` model is asked for the comparison, and of the comparison in the
/// explanation. Named so that `cargo test explain::compare` selects them.
#[cfg(test)]
mod compare {
    use std::fs;

    use tempfile::TempDir;

    use crate::agent::mainline::{Boundary, Step};
    use crate::agent::subline::SubLine;
    use crate::agent::tools::Tools;
    use crate::config::Config;
    use crate::llm::{ChatError, ChatResponse, FakeChatClient, Message, Router};
    use crate::profile::status::{Provenance, Status};
    use crate::profile::store::{Profile, Timestamp};
    use crate::session::SessionLog;
    use crate::workspace::cargo::FakeCargo;
    use crate::workspace::path::Root;

    use super::HARD_WITHIN_PROFILE;

    const WITH_DBG: &str = "fn main() {\n    dbg!(1);\n}\n";
    const WITHOUT_DBG: &str = "fn main() {\n    eprintln!(\"{}\", 1);\n}";
    /// The words of the instruction for the comparison that the rule rests on.
    pub(super) const REWRITE: &str = "rewrite these lines without these items";
    const ONLY_ALLOWED: &str =
        "using only the syntax elements and crates the user has written before";
    const NO_BLOCK: &str = "write no code block";

    pub(super) fn project() -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"demo\"\n",
        )
        .unwrap();
        dir
    }

    pub(super) fn step_with_dbg() -> Boundary {
        Boundary {
            items: vec!["dbg_macro".parse().unwrap()],
            step: Step::new("src/main.rs".to_owned(), None, WITH_DBG.to_owned()),
        }
    }

    /// A boundary on `dbg_macro` after a step that wrote `content` to a new `src/main.rs`.
    pub(super) fn step_of(content: &str) -> Boundary {
        Boundary {
            items: vec!["dbg_macro".parse().unwrap()],
            step: Step::new("src/main.rs".to_owned(), None, content.to_owned()),
        }
    }

    /// A profile where each of `items` has `status`.
    pub(super) fn profile_with(items: &[(&str, Status)]) -> Profile {
        let mut profile = Profile::new();
        for (item, status) in items {
            profile
                .transition(
                    item.parse().unwrap(),
                    *status,
                    Provenance::Manual,
                    Timestamp::from_unix_seconds(0),
                    None,
                )
                .unwrap();
        }
        profile
    }

    /// Opens a side line at `boundary` with `profile`, whose `reason` model answers with each of
    /// `replies` in turn, runs `act` on it, and returns the text of the user message of every
    /// request it sent.
    pub(super) fn on_side_line(
        boundary: Boundary,
        profile: &Profile,
        replies: &[Option<&str>],
        act: impl FnOnce(&mut SubLine),
    ) -> Vec<String> {
        let dir = project();
        let cargo = FakeCargo::new();
        let tools = Tools::new(Root::new(dir.path()).unwrap(), &cargo);
        let client = FakeChatClient::new();
        for reply in replies {
            client.push_chat(
                reply
                    .ok_or(ChatError::FakeExhausted)
                    .map(|text| ChatResponse {
                        content: Some(text.to_owned()),
                        tool_calls: Vec::new(),
                    }),
            );
        }
        let mut side = SubLine::open(
            &client,
            Router::new(&Config::default()),
            &tools,
            boundary,
            Some("debug 1"),
            profile,
        );
        act(&mut side);
        client
            .requests()
            .iter()
            .map(|request| {
                assert_eq!(request.model, Config::default().models.reason);
                match request.messages.last() {
                    Some(Message::User { content }) => content.clone(),
                    last => panic!("not a user message last: {last:?}"),
                }
            })
            .collect()
    }

    /// The explanation of a side line at `boundary` whose `reason` model replies `reply`, and the
    /// text of the request for it.
    fn explain(boundary: Boundary, reply: &str) -> (String, String) {
        let mut explanation = String::new();
        let sent = on_side_line(boundary, &Profile::new(), &[Some(reply)], |side| {
            explanation = side.explain(&SessionLog::disabled()).unwrap();
        });
        (explanation, sent.into_iter().next().unwrap())
    }

    #[test]
    fn the_request_for_the_explanation_asks_to_rewrite_the_attempted_code_without_the_items() {
        let (_, sent) = explain(step_with_dbg(), "idea");

        assert!(sent.contains("dbg!(1);"), "the attempted code: {sent}");
        let rewrite = sent.find(REWRITE);
        assert!(rewrite.is_some(), "{sent}");
        assert!(sent.find("dbg!(1);") < rewrite, "{sent}");
        assert!(sent.contains(ONLY_ALLOWED), "{sent}");
        assert!(sent.contains(NO_BLOCK), "{sent}");
    }

    #[test]
    fn the_request_for_the_explanation_names_the_known_items_and_not_the_others() {
        let profile = profile_with(&[
            ("arc", Status::Known),
            ("crate:serde", Status::Known),
            ("mutex", Status::Seen),
            ("crate:tokio", Status::Seen),
        ]);
        let mut sent = String::new();

        on_side_line(step_with_dbg(), &profile, &[Some("idea")], |side| {
            side.explain(&SessionLog::disabled()).unwrap();
        })
        .into_iter()
        .for_each(|text| sent = text);

        let allowed = &sent[sent.find(ONLY_ALLOWED).expect("the instruction")..];
        for name in ["arc", "crate:serde"] {
            assert!(allowed.contains(&format!("`{name}`")), "{name}: {allowed}");
        }
        for name in ["mutex", "crate:tokio", "dbg_macro", "for_loop"] {
            assert!(!allowed.contains(&format!("`{name}`")), "{name}: {allowed}");
        }
    }

    #[test]
    fn a_crate_with_a_spelling_that_is_not_known_is_not_named_as_the_checker_stops_it() {
        let profile = profile_with(&[
            ("crate:serde-json", Status::Known),
            ("crate:serde_json", Status::Seen),
        ]);
        let mut sent = String::new();

        on_side_line(step_with_dbg(), &profile, &[Some("idea")], |side| {
            side.explain(&SessionLog::disabled()).unwrap();
        })
        .into_iter()
        .for_each(|text| sent = text);

        let allowed = &sent[sent.find(ONLY_ALLOWED).expect("the instruction")..];
        assert!(!allowed.contains("serde"), "{allowed}");
    }

    #[test]
    fn a_question_before_the_explanation_came_does_not_ask_for_the_comparison() {
        let sent = on_side_line(
            step_with_dbg(),
            &Profile::new(),
            &[None, Some("answer")],
            |side| {
                assert!(side.explain(&SessionLog::disabled()).is_err());
                side.ask("what is it?", &SessionLog::disabled()).unwrap();
            },
        );

        assert!(sent[0].contains(REWRITE), "the explanation: {}", sent[0]);
        assert!(sent[1].ends_with("what is it?"), "{}", sent[1]);
        assert!(!sent[1].contains(REWRITE), "the question: {}", sent[1]);
    }

    #[test]
    fn the_explanation_compares_the_attempted_code_with_the_code_of_the_reply_without_the_items() {
        let reply = format!("dbg! prints a value and returns it.\n\n```rust\n{WITHOUT_DBG}\n```\n");

        let (explanation, _) = explain(step_with_dbg(), &reply);

        let idea = explanation.find("dbg! prints a value and returns it.");
        let tried = explanation.find("    dbg!(1);");
        let within = explanation.find("    eprintln!(\"{}\", 1);");
        assert!(
            idea.is_some() && tried.is_some() && within.is_some(),
            "{explanation}"
        );
        assert!(idea < tried && tried < within, "{explanation}");
        assert!(explanation.contains("`dbg_macro`"), "{explanation}");
        assert!(!explanation.contains(HARD_WITHIN_PROFILE), "{explanation}");
    }

    #[test]
    fn a_reply_without_code_says_it_is_hard_to_write_within_the_profile() {
        let (explanation, _) = explain(step_with_dbg(), "dbg! prints a value and returns it.");

        assert!(
            explanation.starts_with("`dbg_macro` (macros)\n\ndbg! prints a value and returns it."),
            "{explanation}"
        );
        assert!(explanation.contains("    dbg!(1);"), "{explanation}");
        assert!(explanation.contains(HARD_WITHIN_PROFILE), "{explanation}");
    }

    #[test]
    fn a_reply_with_an_empty_code_block_says_it_is_hard_to_write_within_the_profile() {
        let (explanation, _) = explain(step_with_dbg(), "dbg! prints a value.\n\n```rust\n```");

        assert!(explanation.contains(HARD_WITHIN_PROFILE), "{explanation}");
    }

    #[test]
    fn the_lines_of_the_step_are_the_attempted_code() {
        let (explanation, sent) = explain(
            step_of("fn main() {\n    dbg!(2);\n}"),
            "dbg! prints a value.",
        );

        assert!(sent.contains(REWRITE), "{sent}");
        assert!(explanation.contains("    dbg!(2);"), "{explanation}");
        assert!(explanation.contains(HARD_WITHIN_PROFILE), "{explanation}");
    }

    #[test]
    fn the_side_line_keeps_the_reply_as_the_model_wrote_it() {
        let reply = "idea\n\n```rust\nfn main() {}\n```";
        let dir = project();
        let cargo = FakeCargo::new();
        let tools = Tools::new(Root::new(dir.path()).unwrap(), &cargo);
        let client = FakeChatClient::new();
        for text in [reply, "answer"] {
            client.push_chat(Ok(ChatResponse {
                content: Some(text.to_owned()),
                tool_calls: Vec::new(),
            }));
        }
        let mut side = SubLine::open(
            &client,
            Router::new(&Config::default()),
            &tools,
            step_with_dbg(),
            Some("debug 1"),
            &Profile::new(),
        );
        side.explain(&SessionLog::disabled()).unwrap();

        side.ask("why?", &SessionLog::disabled()).unwrap();

        let request = client.requests().pop().unwrap();
        let Some(Message::Assistant { content, .. }) = request.messages.get(2) else {
            panic!("no explanation in the message list: {:?}", request.messages);
        };
        assert_eq!(content.as_deref(), Some(reply));
    }
}

/// Tests of where the reference repository uses the items, in the explanation. The stock-taking
/// is built by hand. Named so that `cargo test explain::similar` selects them.
#[cfg(test)]
mod similar {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::{Path, PathBuf};

    use serde_json::json;
    use tempfile::TempDir;

    use crate::agent::mainline::{MainLine, TurnEnd};
    use crate::agent::tools::{ApprovalRequest, Approver, Tools};
    use crate::config::Config;
    use crate::llm::{ChatResponse, FakeChatClient, Router, ToolCall};
    use crate::profile::status::ItemKey;
    use crate::profile::store::{Profile, Timestamp};
    use crate::repo_scan::{self, Place, RepoScan};
    use crate::session::SessionLog;
    use crate::workspace::cargo::FakeCargo;
    use crate::workspace::path::Root;

    use super::{similar, similar_beside};

    const WITH_DBG: &str = "fn main() {\n    dbg!(1);\n}\n";
    /// The heading of the part, which is there exactly when the part is.
    const HEADING: &str = "Where your reference repository uses them";

    struct Refuse;

    impl Approver for Refuse {
        fn confirm(&mut self, _request: &ApprovalRequest) -> bool {
            false
        }
    }

    fn places(path: &str, lines: &[usize]) -> Vec<Place> {
        lines
            .iter()
            .map(|line| Place {
                path: path.to_owned(),
                line: *line,
            })
            .collect()
    }

    /// A stock-taking of a repository that is not on the disk, where each element of `elements`
    /// has its places.
    fn scan(elements: &[(&str, Vec<Place>)]) -> RepoScan {
        RepoScan {
            repo: "/no/such/reference".to_owned(),
            at: Timestamp::from_unix_seconds(0),
            crates: Vec::new(),
            skipped: Vec::new(),
            elements: elements
                .iter()
                .map(|(name, places)| ((*name).to_owned(), places.clone()))
                .collect::<BTreeMap<_, _>>(),
        }
    }

    fn items(names: &[&str]) -> Vec<ItemKey> {
        names.iter().map(|name| name.parse().unwrap()).collect()
    }

    /// The lines of `text` from the heading of the part.
    fn part(text: &str) -> Vec<&str> {
        let at = text
            .find(HEADING)
            .unwrap_or_else(|| panic!("no part: {text}"));
        text[at..].lines().collect()
    }

    /// The explanation of the side line at the boundary that the model's two `dbg!` writes stop a
    /// turn at, with the profile saved at `profile_path`, and the `reason` model replying `reply`.
    fn explain_beside(profile_path: &Path, reply: &str) -> String {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"demo\"\n",
        )
        .unwrap();
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        let client = FakeChatClient::new();
        let cargo = FakeCargo::new();
        let tools = Tools::new(Root::new(dir.path()).unwrap(), &cargo);
        let mut main_line = MainLine::new(
            &client,
            Router::new(&Config::default()),
            tools,
            Profile::new(),
        )
        .with_profile_path(profile_path.to_owned());
        {
            let id = "first";
            client.push_chat(Ok(ChatResponse {
                content: None,
                tool_calls: vec![ToolCall {
                    id: id.to_owned(),
                    name: "write_file".to_owned(),
                    arguments: json!({ "path": "src/main.rs", "content": WITH_DBG }).to_string(),
                }],
            }));
        }
        let boundary = match main_line.turn("debug 1", &mut Refuse).end {
            TurnEnd::Boundary(boundary) => boundary,
            end => panic!("the turn did not stop at a boundary: {end:?}"),
        };
        let mut side = main_line.side_line(boundary);
        client.push_chat(Ok(ChatResponse {
            content: Some(reply.to_owned()),
            tool_calls: Vec::new(),
        }));
        side.explain(&SessionLog::disabled()).unwrap()
    }

    /// Where the profile is saved, in a directory of its own, with `repo_scan_toml` beside it when
    /// there is one.
    fn profile_dir(repo_scan_toml: Option<&str>) -> (TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        if let Some(text) = repo_scan_toml {
            fs::write(dir.path().join(repo_scan::FILE_NAME), text).unwrap();
        }
        let path = dir.path().join("profile.toml");
        (dir, path)
    }

    #[test]
    fn the_places_kept_beside_the_profile_follow_the_comparison_as_the_record_of_init() {
        let kept = scan(&[("dbg_macro", places("src/lib.rs", &[3, 17]))]);
        let (_dir, profile) = profile_dir(Some(&toml::to_string(&kept).unwrap()));

        let explanation = explain_beside(
            &profile,
            "dbg! prints a value.\n\n```rust\nfn main() {}\n```",
        );

        let within = explanation
            .find("Without them, within your profile:")
            .unwrap();
        assert!(within < explanation.find(HEADING).unwrap(), "{explanation}");
        assert_eq!(
            part(&explanation),
            [
                HEADING,
                "As `molt init` recorded /no/such/reference at 1970-01-01T00:00:00Z, not scanned again:",
                "`dbg_macro`:",
                "  src/lib.rs:3",
                "  src/lib.rs:17",
            ]
        );
    }

    #[test]
    fn without_a_stock_taking_beside_the_profile_the_part_is_left_out() {
        let (_dir, profile) = profile_dir(None);

        let explanation = explain_beside(&profile, "dbg! prints a value.");

        assert!(!explanation.contains(HEADING), "{explanation}");
        assert!(!explanation.contains("molt init"), "{explanation}");
    }

    #[test]
    fn a_stock_taking_that_cannot_be_read_is_said_so_at_the_end_and_the_explanation_still_comes() {
        let (_dir, profile) = profile_dir(Some("not = [toml"));

        let explanation = explain_beside(&profile, "dbg! prints a value.");

        assert!(
            explanation.starts_with("`dbg_macro` (macros)\n\ndbg! prints a value."),
            "{explanation}"
        );
        let path = repo_scan::path_beside(&profile);
        assert!(
            explanation.ends_with(&format!(
                "\n\nWhere your reference repository uses them: {} could not be read. Move it \
                 away and run `molt init` again.",
                path.display()
            )),
            "{explanation}"
        );
    }

    #[test]
    fn a_stock_taking_with_a_key_that_is_no_element_is_said_not_to_be_readable() {
        let (_dir, profile) = profile_dir(Some(
            "repo = \"/r\"\nat = 1970-01-01T00:00:00Z\ncrates = []\nskipped = []\n\n\
             [elements]\nnot_an_element = []\n",
        ));

        let text = similar_beside(&profile, &items(&["dbg_macro"])).unwrap();

        assert!(text.contains("could not be read"), "{text}");
    }

    #[test]
    fn a_stock_taking_that_cannot_be_read_says_nothing_at_a_boundary_of_crates_only() {
        let (_dir, profile) = profile_dir(Some("not = [toml"));

        assert_eq!(similar_beside(&profile, &items(&["crate:tokio"])), None);
    }

    #[test]
    fn at_most_three_places_of_an_element_are_shown() {
        let kept = scan(&[("dbg_macro", places("src/lib.rs", &[1, 2, 3, 4, 5]))]);

        let text = similar(&kept, &items(&["dbg_macro"])).unwrap();

        assert_eq!(
            part(&text)[2..],
            [
                "`dbg_macro`:",
                "  src/lib.rs:1",
                "  src/lib.rs:2",
                "  src/lib.rs:3"
            ]
        );
    }

    #[test]
    fn each_element_of_a_step_gets_its_own_places_once_in_the_order_of_the_boundary() {
        let kept = scan(&[
            ("arc", places("src/a.rs", &[10])),
            ("mutex", places("src/m.rs", &[20, 21])),
        ]);

        let text = similar(&kept, &items(&["mutex", "arc", "mutex"])).unwrap();

        assert_eq!(
            part(&text)[2..],
            [
                "`mutex`:",
                "  src/m.rs:20",
                "  src/m.rs:21",
                "`arc`:",
                "  src/a.rs:10",
            ]
        );
    }

    #[test]
    fn an_element_the_reference_repository_does_not_use_is_said_not_to_be_used() {
        let kept = scan(&[("arc", places("src/a.rs", &[10]))]);

        let text = similar(&kept, &items(&["arc", "mutex"])).unwrap();

        assert_eq!(part(&text)[4..], ["`mutex`: not used there."]);
    }

    #[test]
    fn a_boundary_of_crates_only_has_no_part() {
        let kept = scan(&[("arc", places("src/a.rs", &[10]))]);

        assert_eq!(similar(&kept, &items(&["crate:tokio"])), None);
    }
}

/// Tests of the shorter explanation of items the user has seen before (PRD §7). Named so that
/// `cargo test explain::short` selects them.
#[cfg(test)]
mod short {

    use crate::agent::mainline::{Boundary, Step};

    use crate::profile::status::{ItemKey, Status};

    use crate::session::SessionLog;

    use super::compare::{REWRITE, on_side_line, profile_with, step_with_dbg};
    use super::seen_before;

    const WITHOUT_DBG: &str = "```rust\nfn main() {\n    eprintln!(\"{}\", 1);\n}\n```";
    /// The instruction for the idea of every item.
    const IDEA: &str = "Explain these items as they are used in these lines, in one paragraph.";
    /// The words every instruction for an idea has.
    const ANY_IDEA: &str = "Explain these items";

    fn dbg_macro() -> ItemKey {
        "dbg_macro".parse().unwrap()
    }

    /// The explanation of a side line at `boundary` with a profile of `statuses`, whose `reason`
    /// model replies `reply`, and the text of each request it sent.
    fn explain_with(
        boundary: Boundary,
        statuses: &[(&str, Status)],
        reply: &str,
    ) -> (String, Vec<String>) {
        let mut explanation = String::new();
        let sent = on_side_line(boundary, &profile_with(statuses), &[Some(reply)], |side| {
            explanation = side.explain(&SessionLog::disabled()).unwrap();
        });
        (explanation, sent)
    }

    #[test]
    fn the_request_for_a_seen_item_asks_for_no_idea_but_for_the_comparison() {
        let (_, sent) = explain_with(step_with_dbg(), &[("dbg_macro", Status::Seen)], WITHOUT_DBG);

        let [sent] = &sent[..] else {
            panic!("not one request: {sent:?}")
        };
        assert!(!sent.contains(ANY_IDEA), "{sent}");
        assert!(!sent.contains("After the paragraph"), "{sent}");
        assert!(sent.contains("do not explain their idea"), "{sent}");
        assert!(sent.contains("Write no paragraph."), "{sent}");
        assert!(sent.contains(REWRITE), "{sent}");
    }

    #[test]
    fn the_explanation_of_a_seen_item_is_the_note_that_it_was_seen_then_the_comparison() {
        let (explanation, _) =
            explain_with(step_with_dbg(), &[("dbg_macro", Status::Seen)], WITHOUT_DBG);

        let note = seen_before(&[dbg_macro()]);
        assert!(
            explanation.starts_with(&format!(
                "`dbg_macro` (macros)\n\n{note}\n\nTwo ways to write it\n"
            )),
            "{explanation}"
        );
        assert!(
            explanation.contains("    eprintln!(\"{}\", 1);"),
            "{explanation}"
        );
    }

    #[test]
    fn an_item_not_seen_is_asked_for_its_idea_and_has_no_note() {
        let (explanation, sent) = explain_with(
            step_with_dbg(),
            &[("dbg_macro", Status::Unknown)],
            "dbg! prints a value.",
        );

        assert!(sent[0].contains(IDEA), "{}", sent[0]);
        assert!(!explanation.contains("You have seen"), "{explanation}");
    }

    #[test]
    fn in_a_step_with_two_items_only_the_idea_of_the_seen_item_is_left_out() {
        let boundary = Boundary {
            items: vec!["arc".parse().unwrap(), "mutex".parse().unwrap()],
            step: Step::new("src/main.rs".to_owned(), None, "fn main() {}\n".to_owned()),
        };

        let (explanation, sent) = explain_with(boundary, &[("arc", Status::Seen)], "Mutex locks.");

        assert!(
            sent[0].contains(
                "The user has seen `arc` before: do not explain the idea of those items again. \
                 Explain the idea of the other items in one paragraph."
            ),
            "{}",
            sent[0]
        );
        assert!(sent[0].contains("After the paragraph"), "{}", sent[0]);
        assert!(
            explanation.starts_with(&format!(
                "`arc`, `mutex` (smart_pointers)\n\n{}\n\nMutex locks.",
                seen_before(&["arc".parse().unwrap()])
            )),
            "{explanation}"
        );
    }

    #[test]
    fn a_crate_seen_in_the_other_spelling_is_not_asked_for_its_idea() {
        let boundary = Boundary {
            items: vec!["crate:serde-json".parse().unwrap()],
            step: Step::new("src/main.rs".to_owned(), None, "fn main() {}".to_owned()),
        };

        let (explanation, sent) =
            explain_with(boundary, &[("crate:serde_json", Status::Seen)], WITHOUT_DBG);

        assert!(!sent[0].contains(ANY_IDEA), "{}", sent[0]);
        assert!(
            explanation.starts_with(&format!(
                "`crate:serde-json` (crates)\n\n{}\n\nTwo ways to write it\n",
                seen_before(&["crate:serde-json".parse().unwrap()])
            )),
            "{explanation}"
        );
    }

    #[test]
    fn a_crate_seen_in_the_spelling_with_a_hyphen_is_not_asked_for_its_idea() {
        let boundary = Boundary {
            items: vec!["crate:serde_json".parse().unwrap()],
            step: Step::new("src/main.rs".to_owned(), None, "fn main() {}".to_owned()),
        };

        let (explanation, sent) =
            explain_with(boundary, &[("crate:serde-json", Status::Seen)], WITHOUT_DBG);

        assert!(!sent[0].contains(ANY_IDEA), "{}", sent[0]);
        assert!(
            explanation.contains(&seen_before(&["crate:serde_json".parse().unwrap()])),
            "{explanation}"
        );
    }
}

/// Tests of one explanation for the items of one step on its side line (PRD §11, UC3).
/// Named so that `cargo test explain::merge` selects them.
#[cfg(test)]
mod merge {
    use std::fs;

    use serde_json::json;

    use crate::agent::mainline::{Boundary, MainLine, Step, TurnEnd};
    use crate::agent::tools::{ApprovalRequest, Approver, Tools};
    use crate::config::Config;
    use crate::llm::{ChatResponse, FakeChatClient, Router, ToolCall};
    use crate::profile::store::Profile;
    use crate::session::SessionLog;
    use crate::workspace::cargo::FakeCargo;
    use crate::workspace::path::Root;

    use super::assemble;

    /// `Arc` on 3 lines, `Mutex` on 2.
    const SHARED: &str = "use std::sync::{Arc, Mutex};\n\nfn main() {\n    let _a = Arc::new(Mutex::new(0));\n    let _b = Arc::new(1);\n}\n";

    struct Refuse;

    impl Approver for Refuse {
        fn confirm(&mut self, _request: &ApprovalRequest) -> bool {
            false
        }
    }

    #[test]
    fn two_items_of_one_category_one_on_several_lines_get_one_explanation_naming_each_once() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"demo\"\n",
        )
        .unwrap();
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        let client = FakeChatClient::new();
        let cargo = FakeCargo::new();
        let tools = Tools::new(Root::new(dir.path()).unwrap(), &cargo);
        let mut main_line = MainLine::new(
            &client,
            Router::new(&Config::default()),
            tools,
            Profile::new(),
        );
        {
            let id = "first";
            client.push_chat(Ok(ChatResponse {
                content: None,
                tool_calls: vec![ToolCall {
                    id: id.to_owned(),
                    name: "write_file".to_owned(),
                    arguments: json!({ "path": "src/main.rs", "content": SHARED }).to_string(),
                }],
            }));
        }
        let boundary = match main_line.turn("share a counter", &mut Refuse).end {
            TurnEnd::Boundary(boundary) => boundary,
            end => panic!("the turn did not stop at a boundary: {end:?}"),
        };
        let names: Vec<String> = boundary.items.iter().map(ToString::to_string).collect();
        assert_eq!(
            names,
            ["arc", "mutex"],
            "each once, though arc is on several lines"
        );
        let before = client.requests().len();
        let mut side = main_line.side_line(boundary);
        client.push_chat(Ok(ChatResponse {
            content: Some("Arc shares, Mutex locks.".to_owned()),
            tool_calls: Vec::new(),
        }));

        let explanation = side.explain(&SessionLog::disabled()).unwrap();

        assert_eq!(client.requests().len(), before + 1, "one explanation");
        assert_eq!(
            explanation.lines().next(),
            Some("`arc`, `mutex` (smart_pointers)"),
            "{explanation}"
        );
        assert_eq!(
            explanation.matches("Arc shares, Mutex locks.").count(),
            1,
            "{explanation}"
        );
    }

    #[test]
    fn an_item_given_twice_is_named_once() {
        let boundary = Boundary {
            items: vec![
                "mutex".parse().unwrap(),
                "arc".parse().unwrap(),
                "mutex".parse().unwrap(),
            ],
            step: Step::new("src/main.rs".to_owned(), None, "fn main() {}\n".to_owned()),
        };

        let explanation = assemble(&boundary, &[], "idea", None);

        assert_eq!(
            explanation.lines().next(),
            Some("`mutex`, `arc` (smart_pointers)")
        );
    }

    #[test]
    fn crates_are_named_with_their_category_crates() {
        let boundary = Boundary {
            items: vec![
                "crate:tokio".parse().unwrap(),
                "crate:serde".parse().unwrap(),
            ],
            step: Step::new("src/main.rs".to_owned(), None, "fn main() {}\n".to_owned()),
        };

        let explanation = assemble(&boundary, &[], "idea", None);

        assert_eq!(
            explanation.lines().next(),
            Some("`crate:tokio`, `crate:serde` (crates)")
        );
    }
}
