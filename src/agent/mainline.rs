//! The main line (PRD UC2): the message list of the conversation with the user, and the loop
//! that sends it to the model and runs the tools the model calls until the turn ends.
//!
//! A turn ends when the model calls `finish` or answers without calling a tool, when it asks
//! for more tool calls than the budget (PRD §8.4), when its arguments still do not parse after
//! the one resend (PRD §11), when the chat API fails (UC2's exception flow), at a boundary
//! (the write path, in `write`), or when the code still does not build after the compile fixes
//! of the budget. If anything was written, the turn ends with `cargo check` (`turn_end`).
//!
//! The main line also keeps the files molt changed in the session and not committed yet, and
//! commits them with the user's approval (UC4, `commit`).

mod commit;
mod turn_end;
mod write;

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde::Deserialize;
use serde_json::{Value, json};

use super::prompts;
use super::subline::{Choice, Done, Resolution, SubLine, bundle};
use super::tools::{Approver, RunCargoArgs, ToolError, Tools};
use crate::llm::types::{check_tool_name, parse_arguments};
use crate::llm::{
    ChatClient, ChatError, ChatRequest, InvalidArguments, Message, Role, Router, ToolCall,
    ToolChoice, ToolDefinition,
};
use crate::profile::status::{ItemKey, Provenance, Status};
use crate::profile::store::{Profile, StoreError, Timestamp};
use crate::session::{self, Event, SessionLog};
use crate::workspace::cargo::{CargoError, CargoOutput};

pub use commit::{CommitOutcome, CommitProposal};

/// Tool calls per turn (PRD §8.4). Every call counts, `finish` too, so that no call the model
/// repeats can keep a turn going.
pub const TOOL_CALL_BUDGET: usize = 40;

/// Compile fixes per turn (PRD §8.4): how many times the model is asked to fix content that does
/// not parse or a project that `cargo check` fails on.
pub const COMPILE_FIX_BUDGET: usize = 3;

/// The tool that ends the turn. It is handled by the loop, not by [`Tools`].
const FINISH: &str = "finish";

/// The tools that write a file. The loop checks their content before [`Tools`] writes it.
const WRITE_FILE: &str = "write_file";
const EDIT_FILE: &str = "edit_file";

/// The tool with which the model stops at a boundary itself. It is handled by the loop.
const REQUEST_BOUNDARY: &str = "request_boundary";

/// The manifest at the project root, which `cargo add` changes.
const MANIFEST: &str = "Cargo.toml";

/// The lockfile at the project root, which cargo updates when the manifest changes.
const LOCKFILE: &str = "Cargo.lock";

/// The tool that commits the files molt changed (UC4). It is handled by the loop, which knows
/// them.
const GIT_COMMIT: &str = "git_commit";

/// The tool that runs cargo. `run_cargo add` changes `Cargo.toml`.
const RUN_CARGO: &str = "run_cargo";

/// The main line on one project.
pub struct MainLine<'a> {
    client: &'a dyn ChatClient,
    router: Router,
    tools: Tools<'a>,
    profile: Profile,
    definitions: Vec<ToolDefinition>,
    messages: Vec<Message>,
    /// The `cargo check` calls molt has put in the message list in its own name, so that each
    /// gets an id of its own.
    molt_checks: usize,
    /// What a turn stopped at a boundary left unchecked, until the request is withdrawn
    /// ([`MainLine::withdraw`]) or the next turn starts.
    held: Option<Held>,
    /// Where the events of the main line are recorded (PRD §8.8).
    log: SessionLog,
    /// Where the profile is saved when the user's choice on the side line changes it.
    profile_path: Option<PathBuf>,
    /// The files molt changed in the session and has not committed, in the order first changed,
    /// each with its content from before molt first changed it (or from its last commit): those
    /// of the turns that ended and of the requests withdrawn (UC4).
    uncommitted: Vec<Change>,
    /// Whether the last `cargo check` molt ran passed: at the end of a turn, on a withdrawal,
    /// before a commit, or on `/done` of ③. `None` before the first.
    last_check: Option<bool>,
    /// What the user typed for the latest turn. Not the latest user message: molt puts the
    /// outcome of the side line there too (PRD §8.5), and the user's request is what the side
    /// line and the commit message quote as the clue to the user's language (PRD §3 Language).
    request: Option<String>,
}

/// A turn held at a boundary: what it did so far, so that it can be withdrawn with `cargo check`
/// on what it changed, or go on in the same turn once the user chose on the side line.
struct Held {
    tool_calls: Vec<String>,
    written: Vec<String>,
    compile_fixes: usize,
    changes: Vec<Change>,
    user_written: Vec<String>,
    progress: Progress,
    /// The items the user chose ② for in this turn (PRD UC3), as [`checked_key`] spells them.
    rewritten: BTreeSet<ItemKey>,
    /// The boundary the turn stopped at, with all its items.
    boundary: Boundary,
    /// The choices made so far on the side lines of `boundary`, in order. Nothing of them applies
    /// until every item of `boundary` has a choice (PRD UC3).
    chosen: Vec<Resolution>,
}

/// What a choice on the side line came to.
#[derive(Debug)]
pub enum Resolved {
    /// Items of the boundary still wait for a choice: these, for the next side line. Nothing has
    /// changed yet: neither the profile nor the main line.
    Pending(Boundary),
    /// Every item of the boundary has a choice, and the turn went on.
    Resumed(Box<Resumed>),
}

impl Resolved {
    /// The resumed turn. Panics when items still wait for a choice.
    #[cfg(test)]
    pub(crate) fn resumed(self) -> Resumed {
        match self {
            Resolved::Resumed(resumed) => *resumed,
            Resolved::Pending(boundary) => panic!("items still wait for a choice: {boundary:?}"),
        }
    }
}

/// How the turn held at a boundary went on after the user's choices on the side lines.
#[derive(Debug)]
pub struct Resumed {
    /// The choices that went into the turn, in the order made.
    pub chosen: Vec<Resolution>,
    /// Saving the profile changed by the choices, when the main line has a path to save it to.
    pub saved: Result<(), StoreError>,
    /// The turn, from its start: the tool calls, files and changes before the boundary count.
    pub turn: Turn,
}

/// What withdrawing a request held at a boundary showed: the files the turn changed before the
/// boundary and the `cargo check` molt ran on them, when there were any.
#[derive(Debug)]
pub struct Withdrawal {
    pub changes: Vec<Change>,
    pub check: Option<Result<CargoOutput, CargoError>>,
}

/// How one turn went.
#[derive(Debug)]
pub struct Turn {
    pub end: TurnEnd,
    /// The tools the model called in this turn, in order, counted against the budget. Calls
    /// with unknown names or broken arguments are included, and so is `finish`.
    pub tool_calls: Vec<String>,
    /// What the model said last: the summary passed to `finish`, or the text of its last reply.
    pub reply: Option<String>,
    /// The files written in this turn, as the model named them, each once, in the order first
    /// written.
    pub written: Vec<String>,
    /// The compile fixes the model was asked for in this turn (PRD §8.4): once for each write
    /// whose content did not parse, and once for each `cargo check` at the end of the turn that
    /// failed. At most [`COMPILE_FIX_BUDGET`].
    pub compile_fixes: usize,
    /// The files molt changed in this turn, in the order first changed: those written, the
    /// `Cargo.toml` at the project root when `cargo add` changed it, and the `Cargo.lock` at the
    /// root when cargo changed it along with `Cargo.toml`.
    pub changes: Vec<Change>,
    /// The files the user changed in ③ on the side line in this turn (UC3), each once: those of
    /// each `/done` in path order, after those of the `/done`s before. They are among `changes` too, as part of the one change of the turn
    /// (UC4).
    pub user_written: Vec<String>,
    /// The crates molt asked the user to add in this turn, in order (PRD UC2).
    pub crate_adds: Vec<CrateAdd>,
    /// The last `cargo check` molt ran at the end of the turn, if it ran one: only when something
    /// was written, and never at a boundary.
    pub check: Option<Result<CargoOutput, CargoError>>,
    /// What came of each `git_commit` the model called in this turn, in order (UC4).
    pub commits: Vec<CommitOutcome>,
}

/// A file molt changed in a turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// The path as the model named it.
    pub path: String,
    /// The content when the turn first changed it, or `None` for a file the turn created.
    pub before: Option<String>,
    /// The content at the end of the turn.
    pub after: String,
}

/// A crate the content written uses that the profile allows (`known`) but the project's
/// `Cargo.toml` does not declare, and what came of asking the user to add it.
#[derive(Debug)]
pub struct CrateAdd {
    pub name: String,
    pub outcome: AddOutcome,
}

/// What came of asking to `cargo add` a crate.
#[derive(Debug)]
pub enum AddOutcome {
    /// The user approved, and cargo ran. It may still have failed: see `success`.
    Ran(CargoOutput),
    /// The user did not approve.
    NotApproved,
    /// The name is not one `cargo add` takes, or cargo could not be run.
    Failed(String),
}

/// Where the main line stopped because the request needs items the profile does not allow
/// (PRD UC2). Nothing was written for it. The user chooses how to go on (UC3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Boundary {
    /// The items the request needs, each once.
    pub items: Vec<ItemKey>,
    pub cause: BoundaryCause,
}

impl Boundary {
    /// The code the main line tried with the items: what was rejected again, or what
    /// `request_boundary` proposed, if it proposed any.
    pub fn attempted_code(&self) -> Option<&str> {
        match &self.cause {
            BoundaryCause::RejectedTwice { content, .. } => Some(content),
            BoundaryCause::Requested { proposed_code, .. } => proposed_code.as_deref(),
        }
    }
}

/// What made the boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoundaryCause {
    /// Content using `items` was rejected again after the model had seen their rejection in the
    /// same turn. `content` is what the model tried to write to `path`.
    RejectedTwice { path: String, content: String },
    /// The model called `request_boundary`.
    Requested {
        reason: String,
        proposed_code: Option<String>,
    },
}

/// Why a turn ended.
#[derive(Debug)]
pub enum TurnEnd {
    /// The model called `finish`, or answered without calling a tool.
    Finished,
    /// The model asked for one more tool call than [`TOOL_CALL_BUDGET`]. That call was not run.
    OverBudget,
    /// The model sent arguments that are not a JSON object again, right after it was asked to
    /// resend.
    InvalidArguments(InvalidArguments),
    /// The chat API failed: authentication, rate limit, network, and so on.
    ApiFailed(ChatError),
    /// The request needs items the profile does not allow.
    Boundary(Boundary),
    /// The content did not parse, or `cargo check` failed, once more after the model had been
    /// asked for [`COMPILE_FIX_BUDGET`] fixes. It was not asked again.
    CompileFixesExhausted,
}

/// What to do after one tool call.
enum Next {
    Continue,
    Stop(TurnEnd),
}

/// What the loop keeps within a turn besides [`Turn`].
#[derive(Default)]
struct Progress {
    /// Whether the previous response had broken arguments and was asked to resend them.
    resend_asked: bool,
    /// Whether this response has broken arguments the model is asked to resend.
    broken: bool,
    /// The items rejected in earlier responses of this turn, in any file. The model has seen
    /// those rejections, so writing one of these items again means it could not avoid it.
    rejected: BTreeSet<ItemKey>,
    /// The items rejected in the response being run. Calls in one response are made before the
    /// model sees any of their results, so these count only from the next response on.
    rejected_now: BTreeSet<ItemKey>,
    /// Whether something was written, or a crate added, since the last `cargo check` at the end
    /// of the turn.
    unchecked: bool,
    /// The project's `Cargo.toml` when the turn started, to show what `cargo add` changed.
    manifest_before: Option<String>,
    /// The project's `Cargo.lock` when the turn started, to show and commit what cargo changed
    /// in it along with `Cargo.toml`.
    lock_before: Option<String>,
    /// The files a `git_commit` in this turn committed, with the content committed, so that the
    /// turn's changes join the uncommitted files from there (UC4).
    committed: Vec<(String, String)>,
    /// The crates asked about in this turn and not added (the user said no, or `cargo add`
    /// failed), with `-` spelled `_`, and the note for the model on each. They are not asked
    /// about again.
    not_added: BTreeMap<String, String>,
}

impl<'a> MainLine<'a> {
    /// A main line with only the system prompt in its message list. Requests go to the
    /// `generate` model of `router`, and what the model writes is checked against `profile`.
    pub fn new(
        client: &'a dyn ChatClient,
        router: Router,
        tools: Tools<'a>,
        profile: Profile,
    ) -> Self {
        Self {
            client,
            router,
            tools,
            profile,
            definitions: tool_definitions(),
            messages: vec![Message::System {
                content: prompts::MAIN_LINE_SYSTEM.to_owned(),
            }],
            molt_checks: 0,
            held: None,
            log: SessionLog::disabled(),
            profile_path: None,
            uncommitted: Vec::new(),
            last_check: None,
            request: None,
        }
    }

    /// The same main line, saving its profile to `path` when the user's choice on the side line
    /// changes it.
    pub fn with_profile_path(mut self, path: PathBuf) -> Self {
        self.profile_path = Some(path);
        self
    }

    /// The same main line, recording its events and requests in `log`.
    pub fn with_log(mut self, log: SessionLog) -> Self {
        self.log = log;
        self
    }

    /// Where the main line records its events.
    pub(crate) fn log(&self) -> &SessionLog {
        &self.log
    }

    /// Opens the side line (PRD UC3) at `boundary`, where this main line stopped, for the items of
    /// the category of its first item ([`bundle::first`]): the items of the other categories get
    /// side lines of their own after it ([`MainLine::resolve`]). It gets those items, the cause of
    /// the boundary and the user's latest request, and nothing else of the main line's message
    /// list; what is said on it does not come back here.
    pub fn side_line(&self, boundary: Boundary) -> SubLine<'a> {
        let boundary = bundle::first(boundary);
        let request = self.latest_request();
        // ② was chosen in this turn for an item of the side line: the boundary recurred.
        let recurred = self.held.as_ref().is_some_and(|held| {
            boundary
                .items
                .iter()
                .any(|item| held.rewritten.contains(&checked_key(item)))
        });
        SubLine::open(
            self.client,
            self.router.clone(),
            &self.tools,
            boundary,
            request,
            &self.profile,
        )
        .recurred(recurred)
        .with_scan_beside(self.profile_path.as_deref())
    }

    /// What the user typed for the latest turn.
    fn latest_request(&self) -> Option<&str> {
        self.request.as_deref()
    }

    /// `/done` of ③ on `side`, opened where this main line stopped (UC3): the user's lines are
    /// checked against this main line's profile, and `cargo check` runs on its project. The
    /// request bodies go to this main line's log (PRD §8.8).
    pub fn done(&self, side: &mut SubLine) -> Done {
        side.done(&self.tools, &self.profile, &self.log)
    }

    /// The profile the main line checks what the model writes against.
    #[cfg(test)]
    pub(crate) fn profile(&self) -> &Profile {
        &self.profile
    }

    /// Takes the user's choice on a side line of the boundary the turn is held at (PRD UC3,
    /// §8.5). While items of the boundary still wait for a choice, the choice is kept, nothing
    /// changes, and those items come back for the next side line ([`Resolved::Pending`]). Once
    /// every item has one, each choice in turn changes the profile (① `known` and ② `seen` with
    /// provenance `side_line`, ③ `known` with `wrote_it`), which is saved, their messages go into
    /// the message list as one message, the only thing from the side lines, and the same turn
    /// goes on. The files the user changed in ③ join the turn's changes. An item counts as chosen
    /// for when a choice was made on a side line opened for it, or when the user's lines of ③
    /// used it, since it becomes `known` then. Once the choices stand, each of them, and each
    /// change of the profile it made, goes to the session log with `line: "sub"` (PRD §8.8); a
    /// choice dropped by withdrawing the request does not. `None` when no turn is held at a
    /// boundary.
    pub fn resolve(
        &mut self,
        resolution: &Resolution,
        approver: &mut dyn Approver,
    ) -> Option<Resolved> {
        let held = self.held.as_mut()?;
        held.chosen.push(resolution.clone());
        let chosen: BTreeSet<ItemKey> = held
            .chosen
            .iter()
            .flat_map(|resolution| {
                let wrote = match resolution.choice() {
                    Choice::UserWrote => resolution.items(),
                    Choice::Unlock | Choice::Rewrite => &[],
                };
                resolution.answers().iter().chain(wrote).map(checked_key)
            })
            .collect();
        let pending: Vec<ItemKey> = held
            .boundary
            .items
            .iter()
            .filter(|item| !chosen.contains(&checked_key(item)))
            .cloned()
            .collect();
        if !pending.is_empty() {
            return Some(Resolved::Pending(Boundary {
                items: pending,
                cause: held.boundary.cause.clone(),
            }));
        }
        let held = self.held.take()?;
        let at = Timestamp::now();
        for resolution in &held.chosen {
            self.log.record(&Event::Choice {
                choice: resolution.choice().name().to_owned(),
                items: resolution
                    .answers()
                    .iter()
                    .map(ToString::to_string)
                    .collect(),
                policy: resolution.policy().map(str::to_owned),
                files: resolution
                    .files()
                    .iter()
                    .map(|file| file.path.clone())
                    .collect(),
            });
            self.apply(resolution, at);
        }
        let saved = match &self.profile_path {
            Some(path) => self.profile.save(path),
            None => Ok(()),
        };
        let mut rewritten = held.rewritten;
        let mut changes = held.changes;
        let mut user_written = held.user_written;
        for resolution in &held.chosen {
            if resolution.choice() == Choice::Rewrite {
                rewritten.extend(resolution.items().iter().map(checked_key));
            }
            for file in resolution.files() {
                let tools = &self.tools;
                if !user_written
                    .iter()
                    .any(|path| tools.same_file(path, &file.path))
                {
                    user_written.push(file.path.clone());
                }
                // A file molt changed earlier in the turn keeps its content from before that.
                match changes
                    .iter_mut()
                    .find(|c| tools.same_file(&c.path, &file.path))
                {
                    Some(change) => change.after = file.after.clone(),
                    None => changes.push(file.clone()),
                }
            }
        }
        self.messages.push(Message::User {
            content: held
                .chosen
                .iter()
                .map(Resolution::message)
                .collect::<Vec<_>>()
                .join("\n\n"),
        });
        let turn = Turn {
            end: TurnEnd::Finished,
            tool_calls: held.tool_calls,
            reply: None,
            written: held.written,
            compile_fixes: held.compile_fixes,
            changes,
            user_written,
            crate_adds: Vec::new(),
            check: None,
            commits: Vec::new(),
        };
        let mut progress = Progress {
            resend_asked: false,
            broken: false,
            ..held.progress
        };
        let wrote = |resolution: &Resolution| resolution.choice() == Choice::UserWrote;
        if held.chosen.last().is_some_and(wrote) {
            // `/done` passes only when `cargo check` passes, and it was the last thing done.
            self.last_check = Some(true);
        } else if held.chosen.iter().any(wrote) {
            // A later side line came after the `/done` that passed, and a `/done` there may have
            // failed on what is on the disk now: the end of the turn checks the build again.
            progress.unchecked = true;
        }
        let turn = self.run(turn, progress, rewritten, approver);
        Some(Resolved::Resumed(Box::new(Resumed {
            chosen: held.chosen,
            saved,
            turn,
        })))
    }

    /// Changes the profile as `resolution` chose, at `at`, and records each change.
    fn apply(&mut self, resolution: &Resolution, at: Timestamp) {
        let (to, provenance) = match resolution.choice() {
            Choice::Unlock => (Status::Known, Provenance::SideLine),
            Choice::Rewrite => (Status::Seen, Provenance::SideLine),
            Choice::UserWrote => (Status::Known, Provenance::WroteIt),
        };
        let mut items: Vec<ItemKey> = resolution.items().to_vec();
        if to == Status::Known {
            // The check allows a crate only when every spelling of it in the profile (`-` or `_`)
            // is `known`, so ① and ③ unlock each of them, or the same boundary would come again.
            let checked: Vec<ItemKey> = items.iter().map(checked_key).collect();
            let spellings: Vec<ItemKey> = self
                .profile
                .entries()
                .map(|(key, _)| key)
                .filter(|spelling| {
                    matches!(spelling, ItemKey::Crate(_))
                        && !items.contains(spelling)
                        && checked.contains(&checked_key(spelling))
                })
                .cloned()
                .collect();
            items.extend(spellings);
        }
        for item in &items {
            // Only these are changes, and PRD §7 allows each with `side_line` and `wrote_it`, so
            // none is refused.
            // ② leaves a `seen` item as it is, and ① a `known` one.
            let from = self.profile.status(item);
            if matches!(
                (from, to),
                (Status::Unknown, _) | (Status::Seen, Status::Known)
            ) && self
                .profile
                .transition(item.clone(), to, provenance, at, None)
                .is_ok()
            {
                self.log.record(&Event::ProfileChange {
                    item: item.to_string(),
                    from,
                    to,
                    provenance,
                });
            }
        }
    }

    /// The message list so far, starting with the system prompt.
    #[cfg(test)]
    fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// Adds `input` to the message list and runs the loop until the turn ends. `approver` is
    /// asked before the tools that need approval.
    ///
    /// Whatever ends the turn, every tool call of the model gets a tool result in the message
    /// list, so the next turn can be sent as it is.
    pub fn turn(&mut self, input: &str, approver: &mut dyn Approver) -> Turn {
        self.held = None;
        self.messages.push(Message::User {
            content: input.to_owned(),
        });
        self.request = Some(input.to_owned());
        let turn = Turn {
            end: TurnEnd::Finished,
            tool_calls: Vec::new(),
            reply: None,
            written: Vec::new(),
            compile_fixes: 0,
            changes: Vec::new(),
            user_written: Vec::new(),
            crate_adds: Vec::new(),
            check: None,
            commits: Vec::new(),
        };
        let progress = Progress {
            manifest_before: self.tools.read(MANIFEST),
            lock_before: self.tools.read(LOCKFILE),
            ..Progress::default()
        };
        self.run(turn, progress, BTreeSet::new(), approver)
    }

    /// Runs the loop of `turn` until the turn ends. At a boundary the turn is held, with the items
    /// the user chose ② for in it (`rewritten`). Otherwise the files the turn changed join the
    /// uncommitted files.
    fn run(
        &mut self,
        mut turn: Turn,
        mut progress: Progress,
        rewritten: BTreeSet<ItemKey>,
        approver: &mut dyn Approver,
    ) -> Turn {
        loop {
            let end = self.converse(&mut turn, &mut progress, approver);
            if let Some(end) = self.end_turn(end, &mut turn, &mut progress, approver) {
                if let TurnEnd::Boundary(boundary) = &end {
                    self.held = Some(Held {
                        tool_calls: turn.tool_calls.clone(),
                        written: turn.written.clone(),
                        compile_fixes: turn.compile_fixes,
                        changes: turn.changes.clone(),
                        user_written: turn.user_written.clone(),
                        progress,
                        rewritten,
                        boundary: boundary.clone(),
                        chosen: Vec::new(),
                    });
                } else {
                    self.absorb(&turn.changes, &progress.committed);
                }
                turn.end = end;
                return turn;
            }
        }
    }

    /// Sends the message list and runs the tools the model calls, until the model is done or the
    /// turn stops. Returns why.
    fn converse(
        &mut self,
        turn: &mut Turn,
        progress: &mut Progress,
        approver: &mut dyn Approver,
    ) -> TurnEnd {
        loop {
            let request = self.request();
            self.log.record_request(&request);
            let response = match self.client.chat(&request) {
                Ok(response) => response,
                Err(error) => return TurnEnd::ApiFailed(error),
            };
            if let Some(content) = response.content.as_ref().filter(|c| !c.trim().is_empty()) {
                turn.reply = Some(content.clone());
            }
            self.messages.push(Message::Assistant {
                content: response.content,
                tool_calls: response.tool_calls.clone(),
            });
            if response.tool_calls.is_empty() {
                return TurnEnd::Finished;
            }

            let mut stop = None;
            progress.broken = false;
            for call in &response.tool_calls {
                let result = if stop.is_some() {
                    prompts::NOT_RUN.to_owned()
                } else {
                    let (result, next) = self.run_call(call, turn, progress, approver);
                    if let Next::Stop(end) = next {
                        stop = Some(end);
                    }
                    result
                };
                self.messages.push(Message::Tool {
                    tool_call_id: call.id.clone(),
                    content: result,
                });
            }
            // Carried over before a stop too: after `finish`, a failed check goes on with the turn.
            progress.resend_asked = progress.broken;
            let rejected_now = std::mem::take(&mut progress.rejected_now);
            progress.rejected.extend(rejected_now);
            if let Some(end) = stop {
                return end;
            }
        }
    }

    /// Runs one tool call and returns its result for the model. `progress.broken` is set when
    /// its arguments are not a JSON object and the model is asked to resend them.
    fn run_call(
        &mut self,
        call: &ToolCall,
        turn: &mut Turn,
        progress: &mut Progress,
        approver: &mut dyn Approver,
    ) -> (String, Next) {
        if turn.tool_calls.len() == TOOL_CALL_BUDGET {
            return (
                prompts::over_budget(TOOL_CALL_BUDGET),
                Next::Stop(TurnEnd::OverBudget),
            );
        }
        turn.tool_calls.push(call.name.clone());
        self.log.record(&Event::ToolCall {
            name: call.name.clone(),
            arguments: call.arguments.clone(),
        });
        if let Err(unknown) = check_tool_name(call, &self.definitions) {
            return (prompts::tool_error(unknown), Next::Continue);
        }
        let arguments = match parse_arguments(call) {
            Ok(arguments) => arguments,
            Err(error) if progress.resend_asked => {
                let result = prompts::tool_error(&error);
                return (result, Next::Stop(TurnEnd::InvalidArguments(error)));
            }
            Err(error) => {
                progress.broken = true;
                return (prompts::resend_arguments(error), Next::Continue);
            }
        };
        match call.name.as_str() {
            WRITE_FILE | EDIT_FILE => return self.write(&call.name, arguments, turn, progress),
            REQUEST_BOUNDARY => return write::request_boundary(&self.profile, arguments),
            GIT_COMMIT => return self.commit_tool(arguments, turn, progress, approver),
            _ => {}
        }
        if call.name == FINISH {
            return match serde_json::from_value::<FinishArgs>(Value::Object(arguments)) {
                Ok(args) => {
                    if let Some(summary) = args.summary.filter(|s| !s.trim().is_empty()) {
                        turn.reply = Some(summary);
                    }
                    (prompts::FINISHED.to_owned(), Next::Stop(TurnEnd::Finished))
                }
                Err(error) => (
                    prompts::tool_error(format!("invalid arguments for tool `{FINISH}`: {error}")),
                    Next::Continue,
                ),
            };
        }
        let result = match self.tools.dispatch(&call.name, &arguments, approver) {
            Ok(output) => {
                if call.name == RUN_CARGO
                    && let Some(RunCargoArgs { subcommand, args }) = cargo_args(&arguments)
                    && subcommand == "add"
                    && let [name] = &args[..]
                {
                    // `cargo add` may have changed `Cargo.toml`, so the build is to be checked
                    // again. A crate it failed on is not asked about again at the end of the turn.
                    progress.unchecked = true;
                    if output.starts_with("failed") {
                        progress.not_added.insert(
                            name.replace('-', "_"),
                            prompts::crate_not_added(name, "see the result of `run_cargo add`"),
                        );
                    }
                }
                output
            }
            Err(error) => {
                // A crate the user said no to is not asked about again at the end of the turn.
                if matches!(error, ToolError::NotApproved)
                    && let Some(RunCargoArgs { subcommand, args }) = cargo_args(&arguments)
                    && subcommand == "add"
                    && let [name] = &args[..]
                {
                    progress
                        .not_added
                        .insert(name.replace('-', "_"), prompts::crate_not_approved(name));
                }
                prompts::tool_error(error)
            }
        };
        (result, Next::Continue)
    }

    fn request(&self) -> ChatRequest {
        let mut request = self.router.request(Role::Generate, self.messages.clone());
        request.tools = self.definitions.clone();
        request.tool_choice = Some(ToolChoice::Auto);
        request
    }
}

/// `item` as the check compares it: a crate with every `-` turned into `_`, an element as it is.
fn checked_key(item: &ItemKey) -> ItemKey {
    match item {
        ItemKey::Crate(name) => ItemKey::Crate(name.replace('-', "_")),
        ItemKey::Element(_) => item.clone(),
    }
}

/// The event of a `cargo check` molt ran.
fn check_event(check: &Result<CargoOutput, CargoError>) -> Event {
    Event::CargoCheck(match check {
        Ok(output) if output.success => session::CargoCheck::Passed {
            stdout: output.stdout.clone(),
            stderr: output.stderr.clone(),
        },
        Ok(output) => session::CargoCheck::Failed {
            stdout: output.stdout.clone(),
            stderr: output.stderr.clone(),
        },
        Err(error) => session::CargoCheck::NotRun {
            error: error.to_string(),
        },
    })
}

/// The arguments of a `run_cargo` call, if they fit.
fn cargo_args(arguments: &serde_json::Map<String, Value>) -> Option<RunCargoArgs> {
    serde_json::from_value(Value::Object(arguments.clone())).ok()
}

/// `finish`: ends the turn.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FinishArgs {
    summary: Option<String>,
}

/// The function calling schema (PRD §8.4): the tools [`Tools::dispatch`] runs, and the ones the
/// loop runs itself: `write_file` and `edit_file` through the check, `request_boundary` and
/// `finish`.
fn tool_definitions() -> Vec<ToolDefinition> {
    let tool =
        |name: &str, description: &str, properties: Value, required: &[&str]| ToolDefinition {
            name: name.to_owned(),
            description: description.to_owned(),
            parameters: json!({
                "type": "object",
                "properties": properties,
                "required": required,
                "additionalProperties": false,
            }),
        };
    vec![
        tool(
            "list_files",
            prompts::LIST_FILES,
            json!({ "path": { "type": "string", "description": prompts::LIST_FILES_PATH } }),
            &[],
        ),
        tool(
            "read_file",
            prompts::READ_FILE,
            json!({ "path": { "type": "string", "description": prompts::READ_FILE_PATH } }),
            &["path"],
        ),
        tool(
            WRITE_FILE,
            prompts::WRITE_FILE,
            json!({
                "path": { "type": "string", "description": prompts::WRITE_FILE_PATH },
                "content": { "type": "string", "description": prompts::WRITE_FILE_CONTENT },
            }),
            &["path", "content"],
        ),
        tool(
            EDIT_FILE,
            prompts::EDIT_FILE,
            json!({
                "path": { "type": "string", "description": prompts::EDIT_FILE_PATH },
                "old_string": { "type": "string", "description": prompts::EDIT_FILE_OLD_STRING },
                "new_string": { "type": "string", "description": prompts::EDIT_FILE_NEW_STRING },
            }),
            &["path", "old_string", "new_string"],
        ),
        tool(
            "run_cargo",
            prompts::RUN_CARGO,
            json!({
                "subcommand": {
                    "type": "string",
                    "enum": ["check", "test", "clippy", "build", "fmt", "add"],
                    "description": prompts::RUN_CARGO_SUBCOMMAND,
                },
                "args": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": prompts::RUN_CARGO_ARGS,
                },
            }),
            &["subcommand"],
        ),
        tool("git_status", prompts::GIT_STATUS, json!({}), &[]),
        tool(
            "git_diff",
            prompts::GIT_DIFF,
            json!({ "staged": { "type": "boolean", "description": prompts::GIT_DIFF_STAGED } }),
            &[],
        ),
        tool(
            GIT_COMMIT,
            prompts::GIT_COMMIT,
            json!({ "message": { "type": "string", "description": prompts::GIT_COMMIT_MESSAGE } }),
            &["message"],
        ),
        tool(
            REQUEST_BOUNDARY,
            prompts::REQUEST_BOUNDARY,
            json!({
                "items": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": prompts::REQUEST_BOUNDARY_ITEMS,
                },
                "reason": { "type": "string", "description": prompts::REQUEST_BOUNDARY_REASON },
                "proposed_code": {
                    "type": "string",
                    "description": prompts::REQUEST_BOUNDARY_PROPOSED_CODE,
                },
            }),
            &["items", "reason"],
        ),
        tool(
            FINISH,
            prompts::FINISH,
            json!({ "summary": { "type": "string", "description": prompts::FINISH_SUMMARY } }),
            &[],
        ),
    ]
}

/// Tests of one turn of the loop. The module is named so that `cargo test mainline::turn`
/// selects them.
#[cfg(test)]
mod turn {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::Path;

    use serde_json::Map;
    use tempfile::TempDir;

    use super::*;
    use crate::agent::tools::{ApprovalRequest, ToolError};
    use crate::config::Config;
    use crate::llm::{ChatResponse, FakeChatClient};
    use crate::workspace::cargo::{CargoCall, CargoOutput, FakeCargo, Subcommand};
    use crate::workspace::path::Root;

    /// Answers no to every approval.
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

    /// Every file under `dir` with its content.
    fn snapshot(dir: &Path) -> BTreeMap<String, Vec<u8>> {
        let mut files = BTreeMap::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(current) = stack.pop() {
            for entry in fs::read_dir(&current).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    let relative = path.strip_prefix(dir).unwrap().display().to_string();
                    files.insert(relative, fs::read(&path).unwrap());
                }
            }
        }
        files
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

    fn call(id: &str, name: &str, arguments: &str) -> ToolCall {
        ToolCall {
            id: id.to_owned(),
            name: name.to_owned(),
            arguments: arguments.to_owned(),
        }
    }

    fn calls(tool_calls: Vec<ToolCall>) -> ChatResponse {
        ChatResponse {
            content: None,
            tool_calls,
        }
    }

    fn reply(text: &str) -> ChatResponse {
        ChatResponse {
            content: Some(text.to_owned()),
            tool_calls: Vec::new(),
        }
    }

    fn cargo_check(id: &str) -> ChatResponse {
        calls(vec![call(id, "run_cargo", r#"{"subcommand": "check"}"#)])
    }

    fn passed() -> CargoOutput {
        CargoOutput {
            success: true,
            stdout: String::new(),
            stderr: String::new(),
        }
    }

    /// The tool results in the message list, by tool call id.
    fn results(main_line: &MainLine) -> Vec<(String, String)> {
        main_line
            .messages()
            .iter()
            .filter_map(|message| match message {
                Message::Tool {
                    tool_call_id,
                    content,
                } => Some((tool_call_id.clone(), content.clone())),
                _ => None,
            })
            .collect()
    }

    fn result_of(main_line: &MainLine, id: &str) -> String {
        results(main_line)
            .into_iter()
            .find(|(call_id, _)| call_id == id)
            .map(|(_, content)| content)
            .unwrap_or_else(|| panic!("no tool result for {id}"))
    }

    #[test]
    fn the_41st_tool_call_is_not_run_and_the_turn_stops_with_the_calls_so_far() {
        let dir = project();
        let client = FakeChatClient::new();
        let cargo = FakeCargo::new();
        for i in 0..=TOOL_CALL_BUDGET {
            client.push_chat(Ok(cargo_check(&format!("call_{i}"))));
            cargo.push(Ok(passed()));
        }
        client.push_chat(Ok(reply("never sent")));
        let mut main_line = main_line(&client, &cargo, &dir);

        let turn = main_line.turn("check it many times", &mut Refuse);

        assert!(matches!(turn.end, TurnEnd::OverBudget), "{:?}", turn.end);
        assert_eq!(cargo.calls(), vec![CargoCall::Run(Subcommand::Check); 40]);
        assert_eq!(turn.tool_calls, vec!["run_cargo"; 40]);
        assert_eq!(client.requests().len(), 41, "no request after the stop");
        assert_eq!(
            result_of(&main_line, "call_40"),
            prompts::over_budget(TOOL_CALL_BUDGET)
        );
    }

    #[test]
    fn the_40th_tool_call_is_still_run() {
        let dir = project();
        let client = FakeChatClient::new();
        let cargo = FakeCargo::new();
        for i in 0..TOOL_CALL_BUDGET {
            client.push_chat(Ok(cargo_check(&format!("call_{i}"))));
            cargo.push(Ok(passed()));
        }
        client.push_chat(Ok(reply("checked 40 times")));
        let mut main_line = main_line(&client, &cargo, &dir);

        let turn = main_line.turn("check it many times", &mut Refuse);

        assert!(matches!(turn.end, TurnEnd::Finished), "{:?}", turn.end);
        assert_eq!(cargo.calls().len(), 40);
        assert_eq!(turn.reply.as_deref(), Some("checked 40 times"));
    }

    #[test]
    fn calls_over_the_budget_in_one_response_are_not_run_and_each_gets_a_result() {
        let dir = project();
        let client = FakeChatClient::new();
        let cargo = FakeCargo::new();
        for i in 0..TOOL_CALL_BUDGET - 1 {
            client.push_chat(Ok(cargo_check(&format!("call_{i}"))));
        }
        client.push_chat(Ok(calls(vec![
            call("last", "run_cargo", r#"{"subcommand": "check"}"#),
            call("over", "run_cargo", r#"{"subcommand": "check"}"#),
            call("after", "run_cargo", r#"{"subcommand": "check"}"#),
        ])));
        for _ in 0..TOOL_CALL_BUDGET {
            cargo.push(Ok(passed()));
        }
        let mut main_line = main_line(&client, &cargo, &dir);

        let turn = main_line.turn("check it many times", &mut Refuse);

        assert!(matches!(turn.end, TurnEnd::OverBudget), "{:?}", turn.end);
        assert_eq!(cargo.calls().len(), 40);
        assert_eq!(
            result_of(&main_line, "over"),
            prompts::over_budget(TOOL_CALL_BUDGET)
        );
        assert_eq!(result_of(&main_line, "after"), prompts::NOT_RUN);
    }

    #[test]
    fn a_finish_after_40_tool_calls_is_over_the_budget_too() {
        let dir = project();
        let client = FakeChatClient::new();
        let cargo = FakeCargo::new();
        for i in 0..TOOL_CALL_BUDGET {
            client.push_chat(Ok(cargo_check(&format!("call_{i}"))));
            cargo.push(Ok(passed()));
        }
        client.push_chat(Ok(calls(vec![call(
            "done",
            "finish",
            r#"{"summary": "checked"}"#,
        )])));
        let mut main_line = main_line(&client, &cargo, &dir);

        let turn = main_line.turn("check it many times", &mut Refuse);

        assert!(matches!(turn.end, TurnEnd::OverBudget), "{:?}", turn.end);
        assert_eq!(turn.reply, None);
        assert_eq!(
            result_of(&main_line, "done"),
            prompts::over_budget(TOOL_CALL_BUDGET)
        );
    }

    #[test]
    fn a_finish_repeated_with_arguments_that_do_not_fit_still_stops_at_the_budget() {
        let dir = project();
        let client = FakeChatClient::new();
        let cargo = FakeCargo::new();
        for i in 0..=TOOL_CALL_BUDGET {
            client.push_chat(Ok(calls(vec![call(
                &format!("call_{i}"),
                "finish",
                r#"{"summary": 1}"#,
            )])));
        }
        client.push_chat(Ok(reply("never sent")));
        let mut main_line = main_line(&client, &cargo, &dir);

        let turn = main_line.turn("finish", &mut Refuse);

        assert!(matches!(turn.end, TurnEnd::OverBudget), "{:?}", turn.end);
        assert_eq!(client.requests().len(), 41);
    }

    #[test]
    fn broken_arguments_after_a_call_that_parsed_get_a_resend_request_again() {
        let dir = project();
        let client = FakeChatClient::new();
        let cargo = FakeCargo::new();
        client.push_chat(Ok(calls(vec![call("first", "read_file", "{")])));
        client.push_chat(Ok(calls(vec![call(
            "fixed",
            "read_file",
            r#"{"path": "src/main.rs"}"#,
        )])));
        client.push_chat(Ok(calls(vec![call("later", "read_file", "{")])));
        client.push_chat(Ok(reply("done")));
        let mut main_line = main_line(&client, &cargo, &dir);

        let turn = main_line.turn("read main", &mut Refuse);

        assert!(matches!(turn.end, TurnEnd::Finished), "{:?}", turn.end);
        assert!(result_of(&main_line, "later").contains("Send this tool call again"));
    }

    #[test]
    fn every_broken_call_in_one_response_gets_the_resend_request_and_the_turn_goes_on() {
        let dir = project();
        let client = FakeChatClient::new();
        let cargo = FakeCargo::new();
        client.push_chat(Ok(calls(vec![
            call("a", "read_file", "{"),
            call("b", "list_files", "["),
        ])));
        client.push_chat(Ok(reply("done")));
        let mut main_line = main_line(&client, &cargo, &dir);

        let turn = main_line.turn("look around", &mut Refuse);

        assert!(matches!(turn.end, TurnEnd::Finished), "{:?}", turn.end);
        for id in ["a", "b"] {
            assert!(result_of(&main_line, id).contains("Send this tool call again"));
        }
    }

    #[test]
    fn the_system_prompt_tells_the_model_to_answer_in_the_language_of_the_latest_input() {
        let dir = project();
        let client = FakeChatClient::new();
        let cargo = FakeCargo::new();
        client.push_chat(Ok(reply("こんにちは")));
        let mut main_line = main_line(&client, &cargo, &dir);

        main_line.turn("こんにちは", &mut Refuse);

        let requests = client.requests();
        let Message::System { content } = &requests[0].messages[0] else {
            panic!("the first message is not the system prompt: {requests:?}");
        };
        assert!(
            content.contains("Answer in the language of the user's latest input."),
            "{content}"
        );
    }

    #[test]
    fn an_api_error_ends_the_turn_with_its_cause_and_leaves_the_files_unchanged() {
        let dir = project();
        let before = snapshot(dir.path());
        let client = FakeChatClient::new();
        let cargo = FakeCargo::new();
        client.push_chat(Ok(calls(vec![call(
            "call_0",
            "read_file",
            r#"{"path": "src/main.rs"}"#,
        )])));
        client.push_chat(Err(ChatError::Status {
            status: 429,
            body: "rate limited".to_owned(),
        }));
        let mut main_line = main_line(&client, &cargo, &dir);

        let turn = main_line.turn("rewrite main", &mut Refuse);

        let TurnEnd::ApiFailed(error) = &turn.end else {
            panic!("{:?}", turn.end);
        };
        assert!(error.to_string().contains("HTTP 429"), "{error}");
        assert_eq!(snapshot(dir.path()), before);
        assert!(cargo.calls().is_empty());
    }

    #[test]
    fn broken_arguments_get_one_resend_request_and_broken_ones_again_stop_the_turn() {
        let dir = project();
        let client = FakeChatClient::new();
        let cargo = FakeCargo::new();
        client.push_chat(Ok(calls(vec![call("first", "read_file", r#"{"path": "#)])));
        client.push_chat(Ok(calls(vec![call("second", "read_file", r#"{"path"#)])));
        client.push_chat(Ok(reply("never sent")));
        let mut main_line = main_line(&client, &cargo, &dir);

        let turn = main_line.turn("read main", &mut Refuse);

        let TurnEnd::InvalidArguments(error) = &turn.end else {
            panic!("{:?}", turn.end);
        };
        assert_eq!(error.id, "second");
        let resend_requests = results(&main_line)
            .into_iter()
            .filter(|(_, content)| content.contains("Send this tool call again"))
            .count();
        assert_eq!(resend_requests, 1);
        assert!(result_of(&main_line, "first").contains("Send this tool call again"));
        assert_eq!(client.requests().len(), 2, "no request after the stop");
    }

    #[test]
    fn a_resent_call_that_parses_is_run_and_the_turn_goes_on() {
        let dir = project();
        let client = FakeChatClient::new();
        let cargo = FakeCargo::new();
        client.push_chat(Ok(calls(vec![call("first", "read_file", r#"{"path": "#)])));
        client.push_chat(Ok(calls(vec![call(
            "second",
            "read_file",
            r#"{"path": "src/main.rs"}"#,
        )])));
        client.push_chat(Ok(reply("main is empty")));
        let mut main_line = main_line(&client, &cargo, &dir);

        let turn = main_line.turn("read main", &mut Refuse);

        assert!(matches!(turn.end, TurnEnd::Finished), "{:?}", turn.end);
        assert_eq!(result_of(&main_line, "second"), "fn main() {}\n");
        assert_eq!(turn.reply.as_deref(), Some("main is empty"));
    }

    #[test]
    fn an_unknown_tool_gets_the_valid_names_as_its_result_and_the_turn_goes_on() {
        let dir = project();
        let client = FakeChatClient::new();
        let cargo = FakeCargo::new();
        client.push_chat(Ok(calls(vec![call(
            "call_0",
            "delete_file",
            r#"{"path": "src/main.rs"}"#,
        )])));
        client.push_chat(Ok(reply("cannot delete")));
        let mut main_line = main_line(&client, &cargo, &dir);

        let turn = main_line.turn("delete main", &mut Refuse);

        let result = result_of(&main_line, "call_0");
        assert!(result.contains("unknown tool `delete_file`"), "{result}");
        for name in [
            "list_files",
            "read_file",
            "write_file",
            "edit_file",
            "run_cargo",
            "git_status",
            "git_diff",
            "git_commit",
            "request_boundary",
            "finish",
        ] {
            assert!(result.contains(name), "{result}");
        }
        assert!(matches!(turn.end, TurnEnd::Finished), "{:?}", turn.end);
        assert!(dir.path().join("src/main.rs").exists());
    }

    #[test]
    fn finish_ends_the_turn_with_its_summary_and_later_calls_are_not_run() {
        let dir = project();
        let client = FakeChatClient::new();
        let cargo = FakeCargo::new();
        client.push_chat(Ok(calls(vec![
            call("done", "finish", r#"{"summary": "nothing to do"}"#),
            call("after", "run_cargo", r#"{"subcommand": "check"}"#),
        ])));
        let mut main_line = main_line(&client, &cargo, &dir);

        let turn = main_line.turn("anything?", &mut Refuse);

        assert!(matches!(turn.end, TurnEnd::Finished), "{:?}", turn.end);
        assert_eq!(turn.reply.as_deref(), Some("nothing to do"));
        assert_eq!(result_of(&main_line, "after"), prompts::NOT_RUN);
        assert!(cargo.calls().is_empty());
        assert_eq!(client.requests().len(), 1);
    }

    #[test]
    fn a_reply_without_tool_calls_ends_the_turn_with_the_reply() {
        let dir = project();
        let client = FakeChatClient::new();
        let cargo = FakeCargo::new();
        client.push_chat(Ok(reply("a slice is a view into a sequence")));
        let mut main_line = main_line(&client, &cargo, &dir);

        let turn = main_line.turn("what is a slice?", &mut Refuse);

        assert!(matches!(turn.end, TurnEnd::Finished), "{:?}", turn.end);
        assert_eq!(
            turn.reply.as_deref(),
            Some("a slice is a view into a sequence")
        );
        assert!(turn.tool_calls.is_empty());
    }

    #[test]
    fn a_tool_error_is_returned_to_the_model_and_the_turn_goes_on() {
        let dir = project();
        let client = FakeChatClient::new();
        let cargo = FakeCargo::new();
        client.push_chat(Ok(calls(vec![call(
            "call_0",
            "read_file",
            r#"{"path": "missing.rs"}"#,
        )])));
        client.push_chat(Ok(reply("no such file")));
        let mut main_line = main_line(&client, &cargo, &dir);

        let turn = main_line.turn("read missing.rs", &mut Refuse);

        assert!(result_of(&main_line, "call_0").starts_with("error: missing.rs"));
        assert!(matches!(turn.end, TurnEnd::Finished), "{:?}", turn.end);
    }

    #[test]
    fn the_next_turn_sends_the_whole_message_list_so_far() {
        let dir = project();
        let client = FakeChatClient::new();
        let cargo = FakeCargo::new();
        client.push_chat(Ok(reply("first answer")));
        client.push_chat(Ok(reply("second answer")));
        let mut main_line = main_line(&client, &cargo, &dir);

        main_line.turn("first", &mut Refuse);
        main_line.turn("second", &mut Refuse);

        let sent = &client.requests()[1].messages;
        assert_eq!(sent.len(), 4, "{sent:?}");
        assert_eq!(
            sent[1],
            Message::User {
                content: "first".to_owned()
            }
        );
        assert_eq!(
            sent[3],
            Message::User {
                content: "second".to_owned()
            }
        );
    }

    #[test]
    fn the_request_offers_every_tool_of_the_write_path_to_the_generate_model() {
        let dir = project();
        let client = FakeChatClient::new();
        let cargo = FakeCargo::new();
        client.push_chat(Ok(reply("ok")));
        let mut main_line = main_line(&client, &cargo, &dir);

        main_line.turn("hello", &mut Refuse);

        let request = &client.requests()[0];
        let names: Vec<_> = request
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect();
        assert_eq!(
            names,
            [
                "list_files",
                "read_file",
                "write_file",
                "edit_file",
                "run_cargo",
                "git_status",
                "git_diff",
                "git_commit",
                "request_boundary",
                "finish"
            ]
        );
        assert_eq!(request.model, Config::default().models.generate);
        assert_eq!(request.tool_choice, Some(ToolChoice::Auto));
    }

    #[test]
    fn every_offered_tool_the_loop_does_not_run_itself_is_one_the_tools_dispatch() {
        let dir = project();
        let cargo = FakeCargo::new();
        let tools = Tools::new(Root::new(dir.path()).unwrap(), &cargo);
        for definition in tool_definitions() {
            if [FINISH, WRITE_FILE, EDIT_FILE, REQUEST_BOUNDARY, GIT_COMMIT]
                .contains(&definition.name.as_str())
            {
                continue;
            }
            let result = tools.dispatch(&definition.name, &Map::new(), &mut Refuse);
            assert!(
                !matches!(result, Err(ToolError::UnknownTool(_))),
                "{}",
                definition.name
            );
        }
    }
}
