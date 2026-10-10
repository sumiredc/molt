//! The session log (PRD §3 Observability, §8.8): what happened in a session, as jsonl with one
//! event per line, so that it can be traced later.
//!
//! Only what is passed to [`SessionLog::record`] is written: the request headers and the API key
//! never are (PRD §3 Security). The bodies of the requests sent to the chat API go to a separate
//! file, and only when `MOLT_LOG` is enabled ([`request_bodies_enabled`]).

use std::cell::RefCell;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::llm::ChatRequest;
use crate::llm::openai::request_body;
use crate::profile::store::Timestamp;

/// The environment variable that enables writing the request bodies.
const MOLT_LOG: &str = "MOLT_LOG";

/// Whether `MOLT_LOG` enables writing the request bodies: set, and neither empty nor `0`.
pub fn request_bodies_enabled(var: impl Fn(&str) -> Option<OsString>) -> bool {
    var(MOLT_LOG).is_some_and(|value| !value.is_empty() && value != "0")
}

/// One event of the session log. The `event` key of its line names the kind.
///
/// Events of the main line carry no `line` key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// The check found items outside the profile in content the model tried to write.
    Detected { path: String, items: Vec<Detected> },
    /// Content the model generated was rejected and not written. `boundary` is true when the
    /// rejection stopped the turn at a boundary: the same item was rejected before in the turn.
    Rejected {
        path: String,
        items: Vec<String>,
        content: String,
        boundary: bool,
    },
    /// The model called a tool. `arguments` are as the model sent them.
    ToolCall { name: String, arguments: String },
    /// molt ran `cargo check` at the end of a turn, when a request was withdrawn, or before a
    /// `git_commit` after a write (UC4). The model's
    /// own `run_cargo` calls are recorded as [`Event::ToolCall`] only.
    CargoCheck(CargoCheck),
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
        fs::create_dir_all(dir)?;
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

    /// The file the events are written to, unless the log is disabled. Only tests read it
    /// until molt shows where the log is.
    #[cfg(test)]
    pub(crate) fn path(&self) -> Option<&Path> {
        self.events.as_ref().map(|sink| sink.path.as_path())
    }

    /// The file the request bodies are written to, when `MOLT_LOG` enabled it.
    #[cfg(test)]
    pub(crate) fn requests_path(&self) -> Option<&Path> {
        self.requests.as_ref().map(|sink| sink.path.as_path())
    }

    /// Appends `event` as one line.
    pub fn record(&self, event: &Event) {
        if let Some(sink) = &self.events {
            sink.write(event);
        }
    }

    /// Appends the body of `request` as it is sent to the chat API, when `MOLT_LOG` enabled it.
    /// The headers, and the API key in them, are not part of it.
    pub fn record_request(&self, request: &ChatRequest) {
        if let Some(sink) = &self.requests {
            sink.write(&request_body(request));
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

impl Sink {
    fn create(path: PathBuf) -> io::Result<Self> {
        let file = OpenOptions::new()
            .create_new(true)
            .append(true)
            .open(&path)?;
        Ok(Self {
            path,
            file: RefCell::new(Some(file)),
            error: RefCell::new(None),
        })
    }

    fn write(&self, body: &impl Serialize) {
        let mut file = self.file.borrow_mut();
        let Some(writer) = file.as_mut() else {
            return;
        };
        let line = Line {
            at: Timestamp::now().to_string(),
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
