//! The explanation the user is shown on opening the side line (PRD UC3): the idea of the items,
//! then a comparison of the 2 ways to write it, the code the main line tried with the items and
//! the code the `reason` model wrote without them.

use crate::agent::mainline::Boundary;
use crate::checker;
use crate::profile::element::Element;
use crate::profile::status::ItemKey;
use crate::profile::store::Profile;

/// Shown in place of the code without the items when the reply has none.
pub(crate) const HARD_WITHIN_PROFILE: &str =
    "Without them, within your profile: hard to write. The model found no good way to.";

/// Shown in place of the comparison when the main line tried no code: there is nothing to compare.
pub(crate) const NO_COMPARISON: &str =
    "The main line showed no code for it, so there is no comparison of ways to write it.";

/// The items `profile` lets the code use, as the checker decides (PRD §8.4): the base elements and
/// the `known` elements, in the order of [`Element::ALL`], then the `known` crates.
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
/// the prose of the reply, then the code the main line tried at `boundary` and the first code block
/// of the reply, the code without the items. When the reply has no code block, or an empty one,
/// [`HARD_WITHIN_PROFILE`] stands in for it. When the main line tried no code, the model was not
/// asked for any, and [`NO_COMPARISON`] follows the reply as it is.
pub(crate) fn assemble(boundary: &Boundary, reply: &str) -> String {
    let Some(attempted) = boundary.attempted_code() else {
        let reply = reply.trim();
        return if reply.is_empty() {
            NO_COMPARISON.to_owned()
        } else {
            format!("{reply}\n\n{NO_COMPARISON}")
        };
    };
    let (prose, code) = split(reply);
    let mut text = String::new();
    if !prose.is_empty() {
        text.push_str(&prose);
        text.push_str("\n\n");
    }
    text.push_str(&format!(
        "Two ways to write it\nWith {} (what the main line tried):\n{}\n",
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

    use crate::agent::mainline::{Boundary, BoundaryCause};
    use crate::agent::subline::SubLine;
    use crate::agent::tools::Tools;
    use crate::config::Config;
    use crate::llm::{ChatError, ChatResponse, FakeChatClient, Message, Router};
    use crate::profile::element::Element;
    use crate::profile::status::{ItemKey, Provenance, Status};
    use crate::profile::store::{Profile, Timestamp};
    use crate::session::SessionLog;
    use crate::workspace::cargo::FakeCargo;
    use crate::workspace::path::Root;

    use super::{HARD_WITHIN_PROFILE, NO_COMPARISON};

    const WITH_DBG: &str = "fn main() {\n    dbg!(1);\n}\n";
    const WITHOUT_DBG: &str = "fn main() {\n    eprintln!(\"{}\", 1);\n}";
    /// The words of the instruction for the comparison that the rule rests on.
    const REWRITE: &str = "rewrite the code the main line tried without these items";
    const ONLY_ALLOWED: &str =
        "using only the syntax elements and crates the user's profile allows";
    const NO_BLOCK: &str = "write no code block";

    fn project() -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"demo\"\n",
        )
        .unwrap();
        dir
    }

    fn rejected_twice() -> Boundary {
        Boundary {
            items: vec!["dbg_macro".parse().unwrap()],
            cause: BoundaryCause::RejectedTwice {
                path: "src/main.rs".to_owned(),
                content: WITH_DBG.to_owned(),
            },
        }
    }

    fn requested(proposed_code: Option<&str>) -> Boundary {
        Boundary {
            items: vec!["dbg_macro".parse().unwrap()],
            cause: BoundaryCause::Requested {
                reason: "printing a value and returning it is what dbg! is for".to_owned(),
                proposed_code: proposed_code.map(str::to_owned),
            },
        }
    }

    /// A profile where each of `items` has `status`.
    fn profile_with(items: &[(&str, Status)]) -> Profile {
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
    fn on_side_line(
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
        let (_, sent) = explain(rejected_twice(), "idea");

        assert!(sent.contains("dbg!(1);"), "the attempted code: {sent}");
        let rewrite = sent.find(REWRITE);
        assert!(rewrite.is_some(), "{sent}");
        assert!(sent.find("dbg!(1);") < rewrite, "{sent}");
        assert!(sent.contains(ONLY_ALLOWED), "{sent}");
        assert!(sent.contains(NO_BLOCK), "{sent}");
    }

    #[test]
    fn the_request_for_the_explanation_names_the_base_and_known_items_and_not_the_others() {
        let base = ItemKey::Element(Element::ForLoop).to_string();
        let profile = profile_with(&[
            ("arc", Status::Known),
            ("crate:serde", Status::Known),
            ("mutex", Status::Seen),
            ("crate:tokio", Status::Seen),
        ]);
        let mut sent = String::new();

        on_side_line(rejected_twice(), &profile, &[Some("idea")], |side| {
            side.explain(&SessionLog::disabled()).unwrap();
        })
        .into_iter()
        .for_each(|text| sent = text);

        let allowed = &sent[sent.find(ONLY_ALLOWED).expect("the instruction")..];
        for name in [base.as_str(), "arc", "crate:serde"] {
            assert!(allowed.contains(&format!("`{name}`")), "{name}: {allowed}");
        }
        for name in ["mutex", "crate:tokio", "dbg_macro"] {
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

        on_side_line(rejected_twice(), &profile, &[Some("idea")], |side| {
            side.explain(&SessionLog::disabled()).unwrap();
        })
        .into_iter()
        .for_each(|text| sent = text);

        let allowed = &sent[sent.find(ONLY_ALLOWED).expect("the instruction")..];
        assert!(!allowed.contains("serde"), "{allowed}");
    }

    #[test]
    fn without_attempted_code_the_request_asks_for_no_code() {
        let (_, sent) = explain(requested(None), "idea");

        assert!(!sent.contains(REWRITE), "{sent}");
        assert!(!sent.contains(ONLY_ALLOWED), "{sent}");
        assert!(!sent.contains("code block"), "{sent}");
    }

    #[test]
    fn a_question_before_the_explanation_came_does_not_ask_for_the_comparison() {
        let sent = on_side_line(
            rejected_twice(),
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

        let (explanation, _) = explain(rejected_twice(), &reply);

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
        let (explanation, _) = explain(rejected_twice(), "dbg! prints a value and returns it.");

        assert!(
            explanation.starts_with("dbg! prints a value and returns it."),
            "{explanation}"
        );
        assert!(explanation.contains("    dbg!(1);"), "{explanation}");
        assert!(explanation.contains(HARD_WITHIN_PROFILE), "{explanation}");
    }

    #[test]
    fn a_reply_with_an_empty_code_block_says_it_is_hard_to_write_within_the_profile() {
        let (explanation, _) = explain(rejected_twice(), "dbg! prints a value.\n\n```rust\n```");

        assert!(explanation.contains(HARD_WITHIN_PROFILE), "{explanation}");
    }

    #[test]
    fn the_proposed_code_of_request_boundary_is_the_attempted_code() {
        let (explanation, sent) = explain(
            requested(Some("fn main() {\n    dbg!(2);\n}")),
            "dbg! prints a value.",
        );

        assert!(sent.contains(REWRITE), "{sent}");
        assert!(explanation.contains("    dbg!(2);"), "{explanation}");
        assert!(explanation.contains(HARD_WITHIN_PROFILE), "{explanation}");
    }

    #[test]
    fn without_attempted_code_the_explanation_says_there_is_no_comparison() {
        let (explanation, _) = explain(requested(None), "dbg! prints a value.");

        assert_eq!(
            explanation,
            format!("dbg! prints a value.\n\n{NO_COMPARISON}")
        );
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
            rejected_twice(),
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
