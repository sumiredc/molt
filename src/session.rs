//! The session log (PRD §3 Observability, §8.8): what happened in a session, as jsonl with one
//! event per line, so that it can be traced later.
//!
//! Only what is passed to [`SessionLog::record`] is written: the request headers and the API key
//! never are (PRD §3 Security). The bodies of the requests sent to the chat API go to a separate
//! file, and only when `MOLT_LOG` is enabled ([`request_bodies_enabled`]).

use std::cell::RefCell;
use std::ffi::OsString;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::llm::ChatRequest;
use crate::llm::openai::request_body;
use crate::profile::status::{Provenance, Status};
use crate::profile::store::Timestamp;

/// The environment variable that enables writing the request bodies.
const MOLT_LOG: &str = "MOLT_LOG";

/// Whether `MOLT_LOG` enables writing the request bodies: set, and neither empty nor `0`.
pub fn request_bodies_enabled(var: impl Fn(&str) -> Option<OsString>) -> bool {
    var(MOLT_LOG).is_some_and(|value| !value.is_empty() && value != "0")
}

/// One event of the session log. The `event` key of its line names the kind.
///
/// Events of the main line carry no `line` key. Those of the side line ([`Event::Choice`] and
/// [`Event::ProfileChange`]) carry `line: "sub"` (PRD §8.8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// The check found items outside the profile in content the model tried to write.
    Detected { path: String, items: Vec<Detected> },
    /// Content the model generated was rejected and not written, for `reason`: it is too large
    /// to be a step (PRD §2, §8.4), or it does not parse. `items` are those not `known` in it.
    Rejected {
        path: String,
        reason: Rejection,
        items: Vec<String>,
        content: String,
    },
    /// molt wrote a step (PRD §2): content with `items` that are not `known`, whose changed
    /// `lines` (1-based) the user is shown at the boundary that follows.
    Step {
        path: String,
        items: Vec<String>,
        lines: Vec<usize>,
    },
    /// molt took the step at `path` back (PRD UC3): ②, ③, a withdrawn request, or the end of
    /// molt at a boundary. `restored` is false when the file had changed since the step and was
    /// left as it was.
    TakenBack { path: String, restored: bool },
    /// The model called a tool. `arguments` are as the model sent them.
    ToolCall { name: String, arguments: String },
    /// molt ran `cargo check` at the end of a turn, when a request was withdrawn, or before a
    /// `git_commit` after a write (UC4). The model's
    /// own `run_cargo` calls are recorded as [`Event::ToolCall`] only.
    CargoCheck(CargoCheck),
    /// The user's choice at a boundary on the side line (PRD UC3) stands: ①, ② or ④ was chosen,
    /// or ③'s `/done` passed. `items` are those the side line was opened for, `policy` is the
    /// one-line policy of ②, and `files` are those the user changed in ③.
    Choice {
        choice: String,
        items: Vec<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        policy: Option<String>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        files: Vec<String>,
    },
    /// The user's choices on the side line changed the status of `item` in the profile.
    ProfileChange {
        item: String,
        from: Status,
        to: Status,
        provenance: Provenance,
    },
}

impl Event {
    /// The line the event happened on: [`SUB`] for the side line, `None` for the main line.
    fn line(&self) -> Option<&'static str> {
        match self {
            Event::Choice { .. } | Event::ProfileChange { .. } => Some(SUB),
            Event::Detected { .. }
            | Event::Rejected { .. }
            | Event::Step { .. }
            | Event::TakenBack { .. }
            | Event::ToolCall { .. }
            | Event::CargoCheck(_) => None,
        }
    }
}

/// The `line` of what happened on the side line, in the events and in the request bodies.
const SUB: &str = "sub";

/// Why content was not written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Rejection {
    /// Over the step limit: the model is asked to split it.
    OverStepLimit,
    /// It does not parse: the model is asked to fix it.
    ParseError,
}

/// An item the check detected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Detected {
    /// 1-based line in the content.
    pub line: usize,
    /// The element name, or `crate:<name>`.
    pub item: String,
    /// The element's category. A crate has none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// The source the item was detected in.
    pub text: String,
}

/// The result of a `cargo check` molt ran.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum CargoCheck {
    Passed {
        stdout: String,
        stderr: String,
    },
    Failed {
        stdout: String,
        stderr: String,
    },
    /// cargo could not be run.
    NotRun {
        error: String,
    },
}

/// One line of the session log.
#[derive(Serialize)]
struct Line<'a, T: Serialize> {
    at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    line: Option<&'static str>,
    #[serde(flatten)]
    body: &'a T,
}

/// Where the events of one session are written. Writing never stops the session: the first
/// error is kept for [`SessionLog::take_error`], and nothing more is written to that file.
#[derive(Debug, Default)]
pub struct SessionLog {
    events: Option<Sink>,
    requests: Option<Sink>,
}

#[derive(Debug)]
struct Sink {
    path: PathBuf,
    file: RefCell<Option<File>>,
    error: RefCell<Option<io::Error>>,
}

impl SessionLog {
    /// A log that writes nothing.
    pub fn disabled() -> Self {
        Self::default()
    }

    /// A log whose events go to `path`, an existing file opened read-only, so that every write
    /// fails.
    #[cfg(test)]
    pub(crate) fn unwritable(path: &Path) -> Self {
        Self {
            events: Some(Sink {
                path: path.to_owned(),
                file: RefCell::new(Some(File::open(path).unwrap())),
                error: RefCell::new(None),
            }),
            requests: None,
        }
    }

    /// A log that records no events and whose request bodies go to `path`, an existing file
    /// opened read-only, so that every write of a request body fails.
    #[cfg(test)]
    pub(crate) fn unwritable_requests(path: &Path) -> Self {
        Self {
            events: None,
            requests: Some(Sink {
                path: path.to_owned(),
                file: RefCell::new(Some(File::open(path).unwrap())),
                error: RefCell::new(None),
            }),
        }
    }

    /// Creates the log of a new session in `dir`, creating `dir` when it does not exist:
    /// `<time>-<pid>.jsonl`, and `<time>-<pid>.requests.jsonl` for the request bodies when
    /// `request_bodies` is true. `<time>` is UTC, such as `20261010T123456Z`.
    pub fn create(dir: &Path, request_bodies: bool) -> io::Result<Self> {
        create_dir(dir)?;
        let time = Timestamp::now().to_string().replace([':', '-'], "");
        let pid = std::process::id();
        // A session never writes into another one's file: the name of a later session in the
        // same second gets a number.
        let (stem, events) = (0..)
            .map(|n| match n {
                0 => format!("{time}-{pid}"),
                n => format!("{time}-{pid}-{n}"),
            })
            .find_map(
                |stem| match Sink::create(dir.join(format!("{stem}.jsonl"))) {
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => None,
                    result => Some(result.map(|sink| (stem, sink))),
                },
            )
            .expect("an unused name")?;
        let requests = if request_bodies {
            Some(Sink::create(dir.join(format!("{stem}.requests.jsonl")))?)
        } else {
            None
        };
        Ok(Self {
            events: Some(events),
            requests,
        })
    }

    /// The file the events are written to, unless the log is disabled.
    pub(crate) fn path(&self) -> Option<&Path> {
        self.events.as_ref().map(|sink| sink.path.as_path())
    }

    /// The file the request bodies are written to, when `MOLT_LOG` enabled it.
    pub(crate) fn requests_path(&self) -> Option<&Path> {
        self.requests.as_ref().map(|sink| sink.path.as_path())
    }

    /// Appends `event` as one line, with `line: "sub"` when it is an event of the side line.
    pub fn record(&self, event: &Event) {
        if let Some(sink) = &self.events {
            sink.write(event.line(), event);
        }
    }

    /// Appends the body of `request`, sent to the chat API by the main line, when `MOLT_LOG`
    /// enabled it. The headers, and the API key in them, are not part of it.
    pub fn record_request(&self, request: &ChatRequest) {
        if let Some(sink) = &self.requests {
            sink.write(None, &request_body(request));
        }
    }

    /// Appends the body of `request`, sent by the side line, as [`SessionLog::record_request`]
    /// does but with `line: "sub"` beside it, so that it tells from the main line's even when
    /// both lines use the same model.
    pub fn record_sub_request(&self, request: &ChatRequest) {
        if let Some(sink) = &self.requests {
            sink.write(Some(SUB), &request_body(request));
        }
    }

    /// The first error in writing either file since the last call, with the file's path.
    pub fn take_error(&self) -> Option<(PathBuf, io::Error)> {
        [&self.events, &self.requests]
            .into_iter()
            .flatten()
            .find_map(|sink| {
                let error = sink.error.borrow_mut().take()?;
                Some((sink.path.clone(), error))
            })
    }
}

/// Creates `dir` for the logs unless it exists, with mode 0700 on Unix (PRD §8.8). Only `dir`
/// gets the mode: its parents, such as `~/.config`, are not molt's alone and are created as
/// `create_dir_all` does. An existing `dir` keeps the mode the user gave it.
fn create_dir(dir: &Path) -> io::Result<()> {
    if let Some(parent) = dir.parent() {
        fs::create_dir_all(parent)?;
    }
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut builder = DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    match builder.create(dir) {
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists && dir.is_dir() => Ok(()),
        result => result,
    }
}

impl Sink {
    /// Creates the file at `path`, with mode 0600 on Unix (PRD §8.8): a log holds the contents
    /// of files the agent read, and a mode set after creation would leave them readable until
    /// then.
    fn create(path: PathBuf) -> io::Result<Self> {
        let mut options = OpenOptions::new();
        options.create_new(true).append(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let file = options.open(&path)?;
        Ok(Self {
            path,
            file: RefCell::new(Some(file)),
            error: RefCell::new(None),
        })
    }

    fn write(&self, line: Option<&'static str>, body: &impl Serialize) {
        let mut file = self.file.borrow_mut();
        let Some(writer) = file.as_mut() else {
            return;
        };
        let line = Line {
            at: Timestamp::now().to_string(),
            line,
            body,
        };
        let result = serde_json::to_string(&line)
            .map_err(io::Error::other)
            .and_then(|text| writer.write_all(format!("{text}\n").as_bytes()));
        if let Err(error) = result {
            *file = None;
            *self.error.borrow_mut() = Some(error);
        }
    }
}

#[cfg(test)]
mod tests;
