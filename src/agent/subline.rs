//! The side line (PRD UC3): where the user, with the main line paused at a boundary, gets the idea
//! of the items explained and asks about them as much as they like.
//!
//! The side line has a message list of its own (PRD §8.1). Nothing said on it reaches the main
//! line's message list: the main line only hands it the boundary and the user's request.

pub(crate) mod bundle;
mod explain;
mod user_wrote;

use std::collections::BTreeMap;
use std::path::Path;

pub use user_wrote::{Done, Failure};

use super::mainline::{Boundary, Change};
use super::prompts;
use super::tools::Tools;
use crate::llm::{ChatClient, ChatError, Message, Role, Router};
use crate::profile::status::ItemKey;
use crate::profile::store::Profile;
use crate::session::SessionLog;

/// What the user chose on the side line (PRD UC3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    /// ① Understand, then delegate: the items become `known`, and the main line uses them.
    Unlock,
    /// ② Use a construct I know: the items become `seen`, and the main line rewrites without them.
    Rewrite,
    /// ③ Write it myself: the items in the user's lines become `known`, and the main line re-reads
    /// the files the user changed.
    UserWrote,
}

impl Choice {
    /// The name in the message to the main line (PRD §8.5).
    pub fn name(self) -> &'static str {
        match self {
            Choice::Unlock => "unlock",
            Choice::Rewrite => "rewrite",
            Choice::UserWrote => "user_wrote",
        }
    }
}

/// The outcome of the side line that goes back to the main line (PRD §8.5): the choice, the items,
/// for ② the one-line coding policy, and for ③ the files the user changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    choice: Choice,
    /// The items of the boundary the side line was opened at, which this is the choice for.
    answers: Vec<ItemKey>,
    items: Vec<ItemKey>,
    policy: Option<String>,
    files: Vec<Change>,
}

impl Resolution {
    pub fn choice(&self) -> Choice {
        self.choice
    }

    /// The items the profile changes for: those of the boundary for ① and ②, and those detected
    /// in the user's lines for ③.
    pub fn items(&self) -> &[ItemKey] {
        &self.items
    }

    /// The items of the boundary the side line was opened at: what this is the choice for. For ③
    /// they need not be among [`Resolution::items`], when the user wrote without them.
    pub fn answers(&self) -> &[ItemKey] {
        &self.answers
    }

    /// The one-line coding policy of ②.
    pub fn policy(&self) -> Option<&str> {
        self.policy.as_deref()
    }

    /// The files the user changed in ③, from the content when the side line opened to the content
    /// at `/done`, by path relative to the root. Empty for ① and ②.
    pub fn files(&self) -> &[Change] {
        &self.files
    }

    /// The one message that enters the main line (PRD §8.5).
    pub fn message(&self) -> String {
        match self.choice {
            Choice::UserWrote => {
                // The lock is not the user's lines, and the model has no use in reading it.
                let paths: Vec<&str> = self
                    .files
                    .iter()
                    .map(|file| file.path.as_str())
                    .filter(|path| *path != LOCK)
                    .collect();
                prompts::user_wrote_resolved(&self.items, &paths)
            }
            _ => {
                prompts::boundary_resolved(self.choice.name(), &self.items, self.policy.as_deref())
            }
        }
    }
}

/// Why [`SubLine::resolve`] gave no [`Resolution`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unresolved {
    /// The choice is not one of [`SubLine::choices`], or is ③, which only `/done` resolves.
    NotOffered,
    /// The user interrupted the wait for the policy of ② (Ctrl-C). Nothing changed.
    Interrupted,
}

/// The side line opened at one boundary.
pub struct SubLine<'a> {
    client: &'a dyn ChatClient,
    router: Router,
    boundary: Boundary,
    /// The user's latest request on the main line, whose language the review of ③ follows.
    request: Option<String>,
    /// Whether the user chose ③ and has not yet sent a `/done` that found a change to check.
    writing: bool,
    /// Whether the boundary recurred after ② in the same turn (PRD UC3), so ② is not offered.
    recurred: bool,
    baseline: BTreeMap<String, String>,
    /// The `Cargo.lock` at the root when the side line opened, if there was one. `cargo check`
    /// on ③'s `/done` changes it after the user adds a crate.
    lock: Option<String>,
    /// What the model is told about the boundary, sent as the first user message.
    context: String,
    /// What the model is asked for the comparison with the explanation, when the main line tried
    /// some code: written when the side line opened, from the profile then.
    compare: Option<String>,
    /// Where the reference repository uses the items, from the stock-taking beside the profile,
    /// shown after the comparison. `None` when there is no stock-taking.
    similar: Option<String>,
    /// The system prompt, then the context and the explanation once it came, then each question
    /// and its answer.
    messages: Vec<Message>,
}

/// The lock file at the root, which `cargo check` updates when a crate is added to `Cargo.toml`.
const LOCK: &str = "Cargo.lock";

impl<'a> SubLine<'a> {
    /// Opens the side line at `boundary`. `request` is the user's latest request on the main line,
    /// and `profile` the user's profile, whose allowed items the comparison is to use. The files the checker can check are read from the disk under the root of `tools` now, as
    /// they were when the main line stopped, to tell later what the user changed (UC3 ③).
    pub fn open(
        client: &'a dyn ChatClient,
        router: Router,
        tools: &Tools,
        boundary: Boundary,
        request: Option<&str>,
        profile: &Profile,
    ) -> Self {
        let context = prompts::side_line_context(&boundary, request);
        let compare = boundary
            .attempted_code()
            .map(|_| prompts::compare_request(&explain::allowed(profile)));
        Self {
            client,
            router,
            baseline: tools.checkable_files(),
            lock: tools.read(LOCK),
            boundary,
            request: request.map(str::to_owned),
            writing: false,
            recurred: false,
            context,
            compare,
            similar: None,
            messages: vec![Message::System {
                content: prompts::SIDE_LINE_SYSTEM.to_owned(),
            }],
        }
    }

    /// The same side line, opened at a boundary that recurred after ② in the same turn when
    /// `recurred` is true.
    pub(crate) fn recurred(mut self, recurred: bool) -> Self {
        self.recurred = recurred;
        self
    }

    /// The same side line, whose explanation shows where the reference repository uses the items,
    /// from the stock-taking `molt init` kept beside the profile at `profile_path` (PRD UC3). It
    /// is read now, as it was recorded at init. Without one, that part is left out.
    pub(crate) fn with_scan_beside(mut self, profile_path: Option<&Path>) -> Self {
        self.similar = profile_path
            .and_then(explain::scan_beside)
            .and_then(|scan| explain::similar(&scan, &self.boundary.items));
        self
    }

    /// The boundary the side line was opened at.
    pub fn boundary(&self) -> &Boundary {
        &self.boundary
    }

    /// Whether the boundary recurred after ② in the same turn. The construct is needed then, so
    /// only ① (and ③) are offered (PRD UC3).
    pub fn is_recurrence(&self) -> bool {
        self.recurred
    }

    /// The choices the user has, in order.
    pub fn choices(&self) -> &'static [Choice] {
        if self.recurred {
            &[Choice::Unlock, Choice::UserWrote]
        } else {
            &[Choice::Unlock, Choice::Rewrite, Choice::UserWrote]
        }
    }

    /// The outcome of ① or ② to hand back to the main line (PRD §8.5). ③'s outcome only
    /// [`SubLine::done`] makes, so ③ is not offered here. For ② the `light` model makes the
    /// one-line policy from the side line's message list; when it cannot, a fixed policy stands
    /// in, so ② fails only when the user interrupts the wait for the policy. That request body
    /// goes to `log` (PRD §8.8).
    pub fn resolve(&self, choice: Choice, log: &SessionLog) -> Result<Resolution, Unresolved> {
        if !self.choices().contains(&choice) {
            return Err(Unresolved::NotOffered);
        }
        let items = self.boundary.items.clone();
        let policy = match choice {
            Choice::Unlock => None,
            Choice::Rewrite => Some(
                self.policy(log)?
                    .unwrap_or_else(|| prompts::fixed_policy(&items)),
            ),
            Choice::UserWrote => return Err(Unresolved::NotOffered),
        };
        Ok(Resolution {
            choice,
            answers: items.clone(),
            items,
            policy,
            files: Vec::new(),
        })
    }

    /// Asks the `light` model for the one-line policy of ②. `None` when the request fails or the
    /// reply has no line of text.
    fn policy(&self, log: &SessionLog) -> Result<Option<String>, Unresolved> {
        let mut messages = self.messages.clone();
        let instruction = prompts::policy_request(&self.boundary.items);
        let content = if self.explained() {
            instruction
        } else {
            format!("{}\n\n{instruction}", self.context)
        };
        messages.push(Message::User { content });
        let request = self.router.request(Role::Light, messages);
        log.record_sub_request(&request);
        let content = match self.client.chat(&request) {
            Ok(response) => response.content,
            Err(ChatError::Interrupted) => return Err(Unresolved::Interrupted),
            Err(_) => None,
        };
        Ok(content.and_then(|reply| {
            reply
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .map(str::to_owned)
        }))
    }

    /// The `.rs` files and `Cargo.toml`s under the root when the side line opened, with their
    /// content, by path relative to the root.
    pub fn baseline(&self) -> &BTreeMap<String, String> {
        &self.baseline
    }

    /// Whether the explanation has come.
    pub fn explained(&self) -> bool {
        self.messages.len() > 1
    }

    /// Asks the `reason` model to explain the idea of the items, and returns the explanation. When
    /// the request fails, nothing is kept, and the user can still ask (see [`SubLine::ask`]). The
    /// request body goes to `log` (PRD §8.8).
    pub fn explain(&mut self, log: &SessionLog) -> Result<String, ChatError> {
        // Only the explanation asks for the comparison: a question or the policy of ② asked
        // before it came goes with the context alone.
        let content = match &self.compare {
            Some(compare) => format!("{} {compare}", self.context),
            None => self.context.clone(),
        };
        let user = Message::User { content };
        let answer = self.send(user.clone(), log)?;
        let explanation = explain::assemble(&self.boundary, &answer, self.similar.as_deref());
        self.messages.push(user);
        self.messages.push(Message::Assistant {
            content: Some(answer),
            tool_calls: Vec::new(),
        });
        Ok(explanation)
    }

    /// Asks the `reason` model `question` and returns the answer. The question and the answer join
    /// the side line's message list only when the request succeeds. Before the explanation has
    /// come, the question goes with what the model is told about the boundary. The request body
    /// goes to `log` (PRD §8.8).
    pub fn ask(&mut self, question: &str, log: &SessionLog) -> Result<String, ChatError> {
        let content = if self.explained() {
            question.to_owned()
        } else {
            format!(
                "{}\n\nInstead, answer this question:\n{question}",
                self.context
            )
        };
        self.exchange(content, log)
    }

    /// Sends `content` as the user's next message to the `reason` model and returns the answer.
    /// Both join the side line's message list only when the request succeeds.
    fn exchange(&mut self, content: String, log: &SessionLog) -> Result<String, ChatError> {
        let user = Message::User { content };
        let answer = self.send(user.clone(), log)?;
        self.messages.push(user);
        self.messages.push(Message::Assistant {
            content: Some(answer.clone()),
            tool_calls: Vec::new(),
        });
        Ok(answer)
    }

    /// Sends the message list with `user` after it, and returns the text of the reply.
    fn send(&self, user: Message, log: &SessionLog) -> Result<String, ChatError> {
        let mut messages = self.messages.clone();
        messages.push(user);
        let request = self.router.request(Role::Reason, messages);
        log.record_sub_request(&request);
        let response = self.client.chat(&request)?;
        Ok(response.content.unwrap_or_default())
    }
}

/// Tests of ① and ② and what the main line does with them. Named so that
/// `cargo test subline::resolve` selects them.
#[cfg(test)]
mod resolve;

/// Tests of what the side line gives the model to answer in the user's language. Named so that
/// `cargo test subline::language` selects them.
#[cfg(test)]
mod language;

/// Opening the side line at a boundary, its explanation and questions, and that none of it
/// reaches the main line. Named so that `cargo test subline::open` selects them.
#[cfg(test)]
mod open {
    use std::fs;

    use serde_json::json;
    use tempfile::TempDir;

    use super::*;
    use crate::agent::mainline::{MainLine, TurnEnd};
    use crate::agent::tools::{ApprovalRequest, Approver};
    use crate::config::Config;
    use crate::llm::{ChatRequest, ChatResponse, FakeChatClient, ToolCall};
    use crate::profile::store::Profile;
    use crate::workspace::cargo::FakeCargo;
    use crate::workspace::path::Root;

    const WITH_DBG: &str = "fn main() {\n    dbg!(1);\n}\n";

    struct Refuse;

    impl Approver for Refuse {
        fn confirm(&mut self, _request: &ApprovalRequest) -> bool {
            false
        }
    }

    fn project() -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"demo\"\n",
        )
        .unwrap();
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        dir
    }

    fn main_line<'a>(
        client: &'a FakeChatClient,
        cargo: &'a FakeCargo,
        dir: &TempDir,
    ) -> MainLine<'a> {
        let tools = Tools::new(Root::new(dir.path()).unwrap(), cargo);
        MainLine::new(
            client,
            Router::new(&Config::default()),
            tools,
            Profile::new(),
        )
    }

    fn write_main(id: &str) -> ChatResponse {
        ChatResponse {
            content: None,
            tool_calls: vec![ToolCall {
                id: id.to_owned(),
                name: "write_file".to_owned(),
                arguments: json!({ "path": "src/main.rs", "content": WITH_DBG }).to_string(),
            }],
        }
    }

    fn reply(text: &str) -> ChatResponse {
        ChatResponse {
            content: Some(text.to_owned()),
            tool_calls: Vec::new(),
        }
    }

    /// Runs a turn of `main_line` on "debug 1" that the model's two `dbg!` writes stop at a
    /// boundary, and returns the boundary.
    fn stop_at_a_boundary(client: &FakeChatClient, main_line: &mut MainLine) -> Boundary {
        client.push_chat(Ok(write_main("first")));
        client.push_chat(Ok(write_main("second")));
        match main_line.turn("debug 1", &mut Refuse).end {
            TurnEnd::Boundary(boundary) => boundary,
            end => panic!("the turn did not stop at a boundary: {end:?}"),
        }
    }

    /// Every text in the messages of `request`, in order.
    fn texts(request: &ChatRequest) -> Vec<String> {
        request
            .messages
            .iter()
            .filter_map(|message| match message {
                Message::System { content }
                | Message::User { content }
                | Message::Tool { content, .. } => Some(content.clone()),
                Message::Assistant { content, .. } => content.clone(),
            })
            .collect()
    }

    #[test]
    fn the_main_line_requests_carry_none_of_the_side_line_question_explanation_or_answer() {
        let dir = project();
        let client = FakeChatClient::new();
        let cargo = FakeCargo::new();
        let mut main_line = main_line(&client, &cargo, &dir);
        let boundary = stop_at_a_boundary(&client, &mut main_line);
        let mut side = main_line.side_line(boundary);
        client.push_chat(Ok(reply("EXPLANATION: dbg! prints a value.")));
        client.push_chat(Ok(reply("ANSWER: to stderr.")));
        side.explain(&SessionLog::disabled()).unwrap();
        side.ask("QUESTION: where does it print?", &SessionLog::disabled())
            .unwrap();
        main_line.withdraw();
        client.push_chat(Ok(reply("done")));

        main_line.turn("print 1 instead", &mut Refuse);

        let requests = client.requests();
        assert_eq!(requests.len(), 5);
        let generate = Config::default().models.generate;
        let main: Vec<&ChatRequest> = requests.iter().filter(|r| r.model == generate).collect();
        assert_eq!(main.len(), 3, "the two writes and the next request");
        for request in main {
            for text in texts(request) {
                for said in ["EXPLANATION", "QUESTION", "ANSWER"] {
                    assert!(
                        !text.contains(said),
                        "{said} in a main-line request: {text}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_explanation_is_asked_of_the_reason_model_with_the_boundary_and_the_request_only() {
        let dir = project();
        let client = FakeChatClient::new();
        let cargo = FakeCargo::new();
        let mut main_line = main_line(&client, &cargo, &dir);
        let boundary = stop_at_a_boundary(&client, &mut main_line);
        let mut side = main_line.side_line(boundary);
        client.push_chat(Ok(reply("dbg! prints a value.")));

        let explanation = side.explain(&SessionLog::disabled()).unwrap();

        assert!(
            explanation.starts_with("dbg! prints a value.\n\n"),
            "{explanation}"
        );

        let request = client.requests().pop().unwrap();
        assert_eq!(request.model, Config::default().models.reason);
        assert!(request.tools.is_empty());
        let [
            Message::System { content: system },
            Message::User { content },
        ] = &request.messages[..]
        else {
            panic!(
                "not a system prompt and one user message: {:?}",
                request.messages
            );
        };
        assert_eq!(system, prompts::SIDE_LINE_SYSTEM);
        assert!(content.contains("- `dbg_macro` (macros)"), "{content}");
        assert!(
            content.contains("dbg!(1);"),
            "the attempted code: {content}"
        );
        assert!(content.contains("debug 1"), "the user's request: {content}");
        assert!(
            !content.contains("was not written"),
            "a main-line tool result: {content}"
        );
    }

    #[test]
    fn each_question_goes_with_the_explanation_and_the_questions_and_answers_before_it() {
        let dir = project();
        let client = FakeChatClient::new();
        let cargo = FakeCargo::new();
        let mut main_line = main_line(&client, &cargo, &dir);
        let boundary = stop_at_a_boundary(&client, &mut main_line);
        let mut side = main_line.side_line(boundary);
        for text in ["explanation", "answer 1", "answer 2", "answer 3"] {
            client.push_chat(Ok(reply(text)));
        }
        side.explain(&SessionLog::disabled()).unwrap();

        for (question, answer) in [("q1", "answer 1"), ("q2", "answer 2"), ("q3", "answer 3")] {
            assert_eq!(side.ask(question, &SessionLog::disabled()).unwrap(), answer);
        }

        let texts = texts(&client.requests().pop().unwrap());
        assert_eq!(
            texts[2..],
            ["explanation", "q1", "answer 1", "q2", "answer 2", "q3"]
        );
    }

    #[test]
    fn a_failed_explanation_keeps_nothing_and_a_question_then_goes_with_the_boundary() {
        let dir = project();
        let client = FakeChatClient::new();
        let cargo = FakeCargo::new();
        let mut main_line = main_line(&client, &cargo, &dir);
        let boundary = stop_at_a_boundary(&client, &mut main_line);
        let mut side = main_line.side_line(boundary);
        client.push_chat(Err(ChatError::FakeExhausted));
        client.push_chat(Ok(reply("answer")));

        assert!(side.explain(&SessionLog::disabled()).is_err());
        assert!(!side.explained());
        side.ask("what is it?", &SessionLog::disabled()).unwrap();

        let request = client.requests().pop().unwrap();
        let [Message::System { .. }, Message::User { content }] = &request.messages[..] else {
            panic!(
                "not a system prompt and one user message: {:?}",
                request.messages
            );
        };
        assert!(content.contains("dbg_macro"), "{content}");
        assert!(content.ends_with("what is it?"), "{content}");
    }

    #[test]
    fn the_request_bodies_of_the_side_line_go_to_the_log_when_molt_log_enables_them() {
        let dir = project();
        let logs = tempfile::tempdir().unwrap();
        let log = SessionLog::create(logs.path(), true).unwrap();
        let client = FakeChatClient::new();
        let cargo = FakeCargo::new();
        let mut main_line = main_line(&client, &cargo, &dir);
        let boundary = stop_at_a_boundary(&client, &mut main_line);
        let mut side = main_line.side_line(boundary);
        client.push_chat(Ok(reply("explanation")));
        client.push_chat(Ok(reply("answer")));

        side.explain(&log).unwrap();
        side.ask("where does it print?", &log).unwrap();

        let bodies = fs::read_to_string(log.requests_path().unwrap()).unwrap();
        let lines: Vec<&str> = bodies.lines().collect();
        assert_eq!(lines.len(), 2, "{bodies}");
        let reason = Config::default().models.reason;
        assert!(lines.iter().all(|line| line.contains(&reason)), "{bodies}");
        assert!(lines[1].contains("where does it print?"), "{bodies}");
    }

    #[test]
    fn a_failed_question_is_not_kept() {
        let dir = project();
        let client = FakeChatClient::new();
        let cargo = FakeCargo::new();
        let mut main_line = main_line(&client, &cargo, &dir);
        let boundary = stop_at_a_boundary(&client, &mut main_line);
        let mut side = main_line.side_line(boundary);
        client.push_chat(Ok(reply("explanation")));
        client.push_chat(Err(ChatError::FakeExhausted));
        client.push_chat(Ok(reply("answer")));
        side.explain(&SessionLog::disabled()).unwrap();

        assert!(side.ask("lost", &SessionLog::disabled()).is_err());
        side.ask("again", &SessionLog::disabled()).unwrap();

        let texts = texts(&client.requests().pop().unwrap());
        assert_eq!(texts[2..], ["explanation", "again"]);
    }

    #[test]
    fn the_baseline_is_the_checkable_files_as_they_were_when_the_side_line_opened() {
        let dir = project();
        fs::write(dir.path().join("README.md"), "# demo\n").unwrap();
        fs::create_dir_all(dir.path().join("target/debug")).unwrap();
        fs::write(dir.path().join("target/debug/build.rs"), "fn main() {}\n").unwrap();
        let client = FakeChatClient::new();
        let cargo = FakeCargo::new();
        let mut main_line = main_line(&client, &cargo, &dir);
        let boundary = stop_at_a_boundary(&client, &mut main_line);

        let side = main_line.side_line(boundary.clone());
        fs::write(dir.path().join("src/main.rs"), "fn main() { todo!() }\n").unwrap();
        fs::write(dir.path().join("src/lib.rs"), "pub fn f() {}\n").unwrap();

        let baseline: Vec<(&str, &str)> = side
            .baseline()
            .iter()
            .map(|(path, content)| (path.as_str(), content.as_str()))
            .collect();
        assert_eq!(
            baseline,
            [
                ("Cargo.toml", "[package]\nname = \"demo\"\n"),
                ("src/main.rs", "fn main() {}\n"),
            ]
        );
        assert_eq!(side.boundary(), &boundary);
    }
}
