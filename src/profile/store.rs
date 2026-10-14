//! The comprehension profile, the only way to change it, and its toml file
//! (PRD §7, §8.3, §8.8).
//!
//! A status changes only through `Profile::transition`, which takes the
//! provenance, accepts only the transitions of §7, and appends to the history.
//! No field of `Profile` can be written from outside, so nothing becomes
//! `known` without a recorded reason (§3).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::de::{self, Deserialize, Deserializer};
use serde::ser::{Serialize, Serializer};
use toml::value::{Date, Datetime, Offset, Time};

use super::element::{Category, Element};
use super::status::{ItemKey, Provenance, Status, named_enum, serde_by_name};

/// A point in time with date, time and offset, as written in the profile file
/// (a toml offset date-time such as `2026-10-10T12:34:56Z`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timestamp(Datetime);

impl Timestamp {
    /// The current time, in UTC to the second.
    pub fn now() -> Timestamp {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        Timestamp::from_unix_seconds(secs)
    }

    /// The time `secs` seconds after 1970-01-01T00:00:00Z, in UTC. For tests
    /// that need a fixed time.
    pub(crate) fn from_unix_seconds(secs: u64) -> Timestamp {
        let days = secs / 86_400;
        let of_day = secs % 86_400;
        let (year, month, day) = civil_from_days(days);
        Timestamp(Datetime {
            date: Some(Date { year, month, day }),
            time: Some(Time {
                hour: (of_day / 3600) as u8,
                minute: (of_day % 3600 / 60) as u8,
                second: Some((of_day % 60) as u8),
                nanosecond: None,
            }),
            offset: Some(Offset::Z),
        })
    }
}

/// Year, month and day of the date `days` days after 1970-01-01 (Howard
/// Hinnant's `civil_from_days`, restricted to dates from 1970).
fn civil_from_days(days: u64) -> (u16, u8, u8) {
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z % 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
    let year = yoe + era * 400 + u64::from(month <= 2);
    (year as u16, month, day)
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl Serialize for Timestamp {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Timestamp {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let datetime = Datetime::deserialize(deserializer)?;
        match datetime {
            Datetime {
                date: Some(_),
                time: Some(_),
                offset: Some(_),
            } => Ok(Timestamp(datetime)),
            _ => Err(de::Error::custom(format!(
                "not a date-time with an offset: {datetime}"
            ))),
        }
    }
}

named_enum! {
    /// What the user reported for a category in `molt init` (PRD UC1).
    pub enum Report {
        /// "have written": the category's elements are asked about in the Q&A.
        HaveWritten => "have_written",
        /// "can read": the category's elements become `seen`.
        CanRead => "can_read",
        /// "don't know": the category's elements stay `unknown`.
        DontKnow => "dont_know",
    }
    /// Every report, for lookup by name.
    pub const ALL;
    /// The name used in the profile file.
    pub fn name;
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The string is not the name of any report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotAReport(pub String);

impl fmt::Display for NotAReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "not a report: {}", self.0)
    }
}

impl std::error::Error for NotAReport {}

impl FromStr for Report {
    type Err = NotAReport;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Report::ALL
            .iter()
            .copied()
            .find(|report| report.name() == s)
            .ok_or_else(|| NotAReport(s.to_owned()))
    }
}

serde_by_name!(Report);

/// The current status of one item, and why and when it became that.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    status: Status,
    provenance: Provenance,
    at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    evidence: Option<String>,
}

impl Entry {
    pub fn status(&self) -> Status {
        self.status
    }

    pub fn provenance(&self) -> Provenance {
        self.provenance
    }

    pub fn at(&self) -> Timestamp {
        self.at
    }

    /// What the change was based on, in words (an answer, a file, a note), if
    /// the caller gave one.
    pub fn evidence(&self) -> Option<&str> {
        self.evidence.as_deref()
    }
}

/// One transition, as recorded in the history.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Change {
    key: ItemKey,
    from: Status,
    to: Status,
    provenance: Provenance,
    at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    evidence: Option<String>,
}

impl Change {
    pub fn key(&self) -> &ItemKey {
        &self.key
    }

    pub fn from(&self) -> Status {
        self.from
    }

    pub fn to(&self) -> Status {
        self.to
    }

    pub fn provenance(&self) -> Provenance {
        self.provenance
    }

    pub fn at(&self) -> Timestamp {
        self.at
    }

    pub fn evidence(&self) -> Option<&str> {
        self.evidence.as_deref()
    }
}

/// The transition is not one of PRD §7 for that provenance. The profile is left
/// unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransitionError {
    pub key: ItemKey,
    pub from: Status,
    pub to: Status,
    pub provenance: Provenance,
}

impl fmt::Display for TransitionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} cannot change from {} to {} by {}",
            self.key, self.from, self.to, self.provenance
        )
    }
}

impl std::error::Error for TransitionError {}

/// Whether PRD §7 allows `from` → `to` with `provenance`.
///
/// `Manual` (`molt profile set`) may set any status, its current one included.
fn is_allowed(from: Status, to: Status, provenance: Provenance) -> bool {
    use Provenance::*;
    use Status::*;
    match provenance {
        Manual => true,
        SelfReport => (from, to) == (Unknown, Seen),
        Qa | ReferenceRepo => (from, to) == (Unknown, Known),
        // ① understood → known, ② write it without and ④ next → seen.
        SideLine => matches!((from, to), (Unknown | Seen, Known) | (Unknown, Seen)),
        WroteIt => matches!((from, to), (Unknown | Seen, Known)),
    }
}

/// The comprehension profile: per item its status, provenance, time and
/// evidence, the report per category, and the history of transitions.
///
/// `Profile::new()` and an empty file are the same profile: no item has an
/// entry, so everything is `unknown`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Profile {
    items: BTreeMap<ItemKey, Entry>,
    reports: BTreeMap<Category, Report>,
    history: Vec<Change>,
    /// The elements an older file lists in `allow` (and not in `deny`),
    /// which reading it made `known` (PRD §8.3). Not saved: the next save holds
    /// them as entries.
    moved_from_allow: Vec<Element>,
}

impl Profile {
    pub fn new() -> Profile {
        Profile::default()
    }

    /// The status of `key`. An item with no entry is `unknown`.
    pub fn status(&self, key: &ItemKey) -> Status {
        self.items.get(key).map_or(Status::Unknown, |e| e.status)
    }

    pub fn entry(&self, key: &ItemKey) -> Option<&Entry> {
        self.items.get(key)
    }

    /// Every item that has an entry, in key order.
    pub fn entries(&self) -> impl Iterator<Item = (&ItemKey, &Entry)> {
        self.items.iter()
    }

    /// The elements reading the file moved from its `allow` to `known`, in
    /// the order of `Element`, so that the user can be told once per read.
    pub fn moved_from_allow(&self) -> &[Element] {
        &self.moved_from_allow
    }

    pub fn report(&self, category: Category) -> Option<Report> {
        self.reports.get(&category).copied()
    }

    /// Records the user's report for `category`. Changes no status: the
    /// statuses that follow from the report are set with `transition`.
    pub fn set_report(&mut self, category: Category, report: Report) {
        self.reports.insert(category, report);
    }

    /// Every transition so far, oldest first. Only `transition` adds to it.
    pub fn history(&self) -> &[Change] {
        &self.history
    }

    /// Changes the status of `key` to `to` because of `provenance`, at `at`.
    ///
    /// Only the transitions of PRD §7 are accepted; anything else returns
    /// `TransitionError` and changes nothing. On success the entry is
    /// overwritten and one `Change` is appended to the history.
    pub fn transition(
        &mut self,
        key: ItemKey,
        to: Status,
        provenance: Provenance,
        at: Timestamp,
        evidence: Option<String>,
    ) -> Result<(), TransitionError> {
        let from = self.status(&key);
        if !is_allowed(from, to, provenance) {
            return Err(TransitionError {
                key,
                from,
                to,
                provenance,
            });
        }

        self.history.push(Change {
            key: key.clone(),
            from,
            to,
            provenance,
            at,
            evidence: evidence.clone(),
        });
        self.items.insert(
            key,
            Entry {
                status: to,
                provenance,
                at,
                evidence,
            },
        );
        Ok(())
    }

    /// Reads the profile at `path`. Items with no entry are `unknown`; an empty
    /// file is a profile with nothing `known`.
    ///
    /// An older file may hold `allow` and `deny`, the elements the user
    /// added to and removed from those molt used without asking. Each element in `allow` and not in `deny` becomes `known`
    /// (provenance manual), since the user added it by hand to what molt
    /// writes without asking, and `deny` is dropped (PRD §8.3). Neither is written
    /// back.
    ///
    /// A key or field that this version does not know (a renamed element, a
    /// misspelt field) is an error rather than skipped: skipping would drop it
    /// from the file at the next save, history included.
    pub fn load(path: &Path) -> Result<Profile, StoreError> {
        let text = fs::read_to_string(path).map_err(|source| match source.kind() {
            io::ErrorKind::NotFound => StoreError::NotFound {
                path: path.to_owned(),
            },
            _ => StoreError::Io {
                path: path.to_owned(),
                source,
            },
        })?;
        Profile::from_toml(&text).map_err(|source| StoreError::Parse {
            path: path.to_owned(),
            source,
        })
    }

    /// Writes the profile to `path` through a temporary file in the same
    /// directory and a rename, so a failure leaves the previous file as it was.
    ///
    /// When `path` is a symbolic link, the file it points to is replaced and the
    /// link is kept. The directory must already exist.
    pub fn save(&self, path: &Path) -> Result<(), StoreError> {
        let target = resolve_links(path, 0).map_err(|source| StoreError::Io {
            path: path.to_owned(),
            source,
        })?;
        write_atomically(&target, self.to_toml().as_bytes()).map_err(|source| StoreError::Io {
            path: path.to_owned(),
            source,
        })
    }

    fn from_toml(text: &str) -> Result<Profile, toml::de::Error> {
        let file: ProfileFile = toml::from_str(text)?;
        let mut profile = Profile {
            items: file.items,
            reports: file.reports.into_iter().map(|(c, r)| (c.0, r)).collect(),
            history: file.history,
            moved_from_allow: Vec::new(),
        };
        let at = Timestamp::now();
        for ElementName(element) in file.allow {
            let key = ItemKey::Element(element);
            if file.deny.contains(&ElementName(element)) || profile.status(&key) == Status::Known {
                continue;
            }
            profile
                .transition(
                    key,
                    Status::Known,
                    Provenance::Manual,
                    at,
                    Some(MOVED.to_owned()),
                )
                .expect("manual may set any status");
            profile.moved_from_allow.push(element);
        }
        Ok(profile)
    }

    fn to_toml(&self) -> String {
        let file = ProfileFile {
            allow: BTreeSet::new(),
            deny: BTreeSet::new(),
            reports: self
                .reports
                .iter()
                .map(|(&c, &r)| (CategoryName(c), r))
                .collect(),
            items: self.items.clone(),
            history: self.history.clone(),
        };
        toml::to_string(&file).expect("every part of a profile has a toml form")
    }
}

/// Follows symbolic links from `path` to the file to write, which may not
/// exist yet (a dangling link is followed to where it points).
fn resolve_links(path: &Path, depth: u32) -> io::Result<PathBuf> {
    const MAX_DEPTH: u32 = 40;
    match fs::canonicalize(path) {
        Ok(resolved) => Ok(resolved),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let is_link = fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink());
            if !is_link {
                return Ok(path.to_owned());
            }
            if depth >= MAX_DEPTH {
                return Err(io::Error::other("too many levels of symbolic links"));
            }
            let link = fs::read_link(path)?;
            let next = match path.parent() {
                Some(dir) => dir.join(link),
                None => link,
            };
            resolve_links(&next, depth + 1)
        }
        Err(error) => Err(error),
    }
}

/// Distinguishes temporary files of concurrent saves within one process.
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Writes `contents` to `target` through a temporary file in the same directory and a rename,
/// so a failure leaves the previous file as it was.
pub(crate) fn write_atomically(target: &Path, contents: &[u8]) -> io::Result<()> {
    let dir = match target.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    let name = target
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "not a file path"))?;
    let temp = dir.join(format!(
        ".{}.tmp-{}-{}",
        name.to_string_lossy(),
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));

    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    let written = file
        .write_all(contents)
        .and_then(|()| file.sync_all())
        .and_then(|()| fs::rename(&temp, target));
    if written.is_err() {
        let _ = fs::remove_file(&temp);
    }
    written
}

/// The profile could not be read or written.
#[derive(Debug)]
pub enum StoreError {
    /// There is no file at the path (`molt init` has not been run).
    NotFound { path: PathBuf },
    /// Reading or writing failed.
    Io { path: PathBuf, source: io::Error },
    /// The file is not toml, or has a key, field or value this version does
    /// not know.
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::NotFound { path } => write!(f, "{}: no profile", path.display()),
            StoreError::Io { path, source } => write!(f, "{}: {source}", path.display()),
            StoreError::Parse { path, source } => {
                write!(f, "{}: invalid profile: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for StoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            StoreError::NotFound { .. } => None,
            StoreError::Io { source, .. } => Some(source),
            StoreError::Parse { source, .. } => Some(source),
        }
    }
}

/// The evidence of an element moved from `allow` to `known` when read.
const MOVED: &str = "in allow of an older profile file";

/// The shape of the profile file. Separate from `Profile` so that only `load`
/// can build a `Profile` from outside data.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfileFile {
    /// Read from an older file, and never written (see [`Profile::load`]).
    #[serde(default, skip_serializing)]
    allow: BTreeSet<ElementName>,
    #[serde(default, skip_serializing)]
    deny: BTreeSet<ElementName>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    reports: BTreeMap<CategoryName, Report>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    items: BTreeMap<ItemKey, Entry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    history: Vec<Change>,
}

/// An element in `allow` / `deny` of an older file, read as its name.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct ElementName(Element);

impl<'de> Deserialize<'de> for ElementName {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map(ElementName)
            .map_err(de::Error::custom)
    }
}

/// A category as a key of `[reports]`, written as its name.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct CategoryName(Category);

impl Serialize for CategoryName {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.0.name())
    }
}

impl<'de> Deserialize<'de> for CategoryName {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        Category::ALL
            .iter()
            .copied()
            .find(|c| c.name() == name)
            .map(CategoryName)
            .ok_or_else(|| de::Error::custom(format!("not a category: {name}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use Provenance::*;
    use Status::*;

    fn at(secs: u64) -> Timestamp {
        Timestamp::from_unix_seconds(secs)
    }

    fn el(element: Element) -> ItemKey {
        ItemKey::Element(element)
    }

    fn krate(name: &str) -> ItemKey {
        ItemKey::Crate(name.to_owned())
    }

    /// A profile with every part filled: entries of elements and a crate,
    /// reports and a history of several transitions.
    fn filled_profile() -> Profile {
        let mut p = Profile::new();
        p.set_report(Category::Ownership, Report::HaveWritten);
        p.set_report(Category::Async, Report::CanRead);
        p.transition(
            el(Element::Arc),
            Known,
            Qa,
            at(1_791_635_696),
            Some("answered".to_owned()),
        )
        .unwrap();
        p.transition(
            el(Element::AsyncFn),
            Seen,
            SelfReport,
            at(1_791_635_697),
            None,
        )
        .unwrap();
        p.transition(
            krate("tokio"),
            Known,
            ReferenceRepo,
            at(1_791_635_698),
            None,
        )
        .unwrap();
        p.transition(
            el(Element::Option),
            Unknown,
            Manual,
            at(1_791_635_699),
            None,
        )
        .unwrap();
        p
    }

    #[test]
    fn a_profile_written_to_toml_and_read_back_is_the_same_profile() {
        let p = filled_profile();
        assert_eq!(Profile::from_toml(&p.to_toml()).unwrap(), p);
    }

    #[test]
    fn a_profile_saved_to_a_file_and_loaded_is_the_same_profile() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profile.toml");
        let p = filled_profile();

        p.save(&path).unwrap();

        assert_eq!(Profile::load(&path).unwrap(), p);
    }

    #[test]
    fn the_toml_has_element_and_crate_keys_and_offset_date_times() {
        let mut p = Profile::new();
        p.transition(el(Element::Arc), Known, Qa, at(1_791_635_696), None)
            .unwrap();
        p.transition(
            krate("tokio"),
            Known,
            ReferenceRepo,
            at(1_791_635_696),
            None,
        )
        .unwrap();

        let text = p.to_toml();

        assert!(text.contains("[items.arc]"), "{text}");
        assert!(text.contains("[items.\"crate:tokio\"]"), "{text}");
        assert!(text.contains("at = 2026-10-10T12:34:56Z"), "{text}");
        assert!(text.contains("[[history]]"), "{text}");
    }

    #[test]
    fn a_transition_updates_status_provenance_time_and_evidence_of_the_entry() {
        let mut p = Profile::new();

        p.transition(
            el(Element::Arc),
            Known,
            Qa,
            at(100),
            Some("explained Arc".to_owned()),
        )
        .unwrap();

        let entry = p.entry(&el(Element::Arc)).unwrap();
        assert_eq!(entry.status(), Known);
        assert_eq!(entry.provenance(), Qa);
        assert_eq!(entry.at(), at(100));
        assert_eq!(entry.evidence(), Some("explained Arc"));
        assert_eq!(p.status(&el(Element::Arc)), Known);
    }

    #[test]
    fn a_transition_overwrites_the_previous_entry() {
        let mut p = Profile::new();
        p.transition(el(Element::Arc), Seen, SelfReport, at(100), None)
            .unwrap();

        p.transition(el(Element::Arc), Known, WroteIt, at(200), None)
            .unwrap();

        let entry = p.entry(&el(Element::Arc)).unwrap();
        assert_eq!(
            (entry.status(), entry.provenance(), entry.at()),
            (Known, WroteIt, at(200))
        );
    }

    #[test]
    fn a_transition_appends_exactly_one_change_with_from_to_provenance_and_time() {
        let mut p = Profile::new();
        p.transition(el(Element::Arc), Seen, SelfReport, at(100), None)
            .unwrap();

        p.transition(
            el(Element::Arc),
            Known,
            SideLine,
            at(200),
            Some("understood".to_owned()),
        )
        .unwrap();

        assert_eq!(p.history().len(), 2);
        let change = &p.history()[1];
        assert_eq!(change.key(), &el(Element::Arc));
        assert_eq!((change.from(), change.to()), (Seen, Known));
        assert_eq!(change.provenance(), SideLine);
        assert_eq!(change.at(), at(200));
        assert_eq!(change.evidence(), Some("understood"));
        assert_eq!(p.history()[0].to(), Seen);
    }

    #[test]
    fn an_item_with_no_entry_is_unknown() {
        let p = Profile::from_toml(
            "[items.arc]\nstatus = \"known\"\nprovenance = \"qa\"\nat = 2026-10-10T12:34:56Z\n",
        )
        .unwrap();

        assert_eq!(p.status(&el(Element::Arc)), Known);
        assert_eq!(p.status(&el(Element::Mutex)), Unknown);
        assert_eq!(p.status(&krate("tokio")), Unknown);
        assert_eq!(p.entry(&el(Element::Mutex)), None);
    }

    #[test]
    fn an_empty_toml_is_a_profile_with_nothing_known() {
        let p = Profile::from_toml("").unwrap();

        assert_eq!(p, Profile::new());
        assert_eq!(p.entries().count(), 0);
        assert!(p.history().is_empty());
        for &element in Element::ALL {
            assert_eq!(p.status(&el(element)), Unknown, "{element}");
        }
    }

    #[test]
    fn an_empty_profile_file_loads_as_a_profile_with_nothing_known() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profile.toml");
        fs::write(&path, "").unwrap();

        assert_eq!(Profile::load(&path).unwrap(), Profile::new());
    }

    #[test]
    fn an_element_in_allow_and_not_in_deny_is_read_as_known_by_hand_and_said_so() {
        let p = Profile::from_toml(
            "allow = [\"rc\", \"arc\", \"mutex\"]\ndeny = [\"mutex\", \"option\"]\n",
        )
        .unwrap();

        assert_eq!(p.moved_from_allow(), [Element::Rc, Element::Arc]);
        for element in [Element::Arc, Element::Rc] {
            let entry = p.entry(&el(element)).unwrap();
            assert_eq!(entry.status, Known, "{element}");
            assert_eq!(entry.provenance, Manual, "{element}");
            assert_eq!(entry.evidence.as_deref(), Some(MOVED), "{element}");
        }
        // In deny too: the user took it back by hand.
        assert_eq!(p.status(&el(Element::Mutex)), Unknown);
        // Only in deny: an element that was allowed by default and that the
        // user took back, now unknown like every element with no entry.
        assert_eq!(p.status(&el(Element::Option)), Unknown);
        assert_eq!(p.history().len(), 2);
    }

    #[test]
    fn an_element_in_allow_that_is_known_already_is_left_as_it_is() {
        let p = Profile::from_toml(
            "allow = [\"arc\"]\n[items.arc]\nstatus = \"known\"\nprovenance = \"qa\"\nat = 2026-10-10T12:34:56Z\n",
        )
        .unwrap();

        assert!(p.moved_from_allow().is_empty());
        assert_eq!(p.entry(&el(Element::Arc)).unwrap().provenance, Qa);
        assert!(p.history().is_empty());
    }

    #[test]
    fn allow_and_deny_are_not_written_back() {
        let p = Profile::from_toml("allow = [\"arc\"]\ndeny = [\"option\"]\n").unwrap();

        let text = p.to_toml();

        assert!(!text.contains("allow ="), "{text}");
        assert!(!text.contains("deny ="), "{text}");
        let again = Profile::from_toml(&text).unwrap();
        assert!(again.moved_from_allow().is_empty());
        assert_eq!(again.status(&el(Element::Arc)), Known);
    }

    /// The transitions of PRD §7, plus `Manual` to the current status (UC5).
    fn prd_transitions() -> Vec<(Status, Status, Provenance)> {
        let mut allowed = vec![
            (Unknown, Seen, SelfReport),
            (Unknown, Seen, SideLine),
            (Unknown, Seen, Manual),
            (Unknown, Known, Qa),
            (Unknown, Known, ReferenceRepo),
            (Unknown, Known, SideLine),
            (Unknown, Known, WroteIt),
            (Unknown, Known, Manual),
            (Seen, Known, SideLine),
            (Seen, Known, WroteIt),
            (Seen, Known, Manual),
            (Known, Unknown, Manual),
            (Known, Seen, Manual),
            (Seen, Unknown, Manual),
        ];
        allowed.extend(Status::ALL.iter().map(|&s| (s, s, Manual)));
        allowed
    }

    /// A profile where `key` has status `from`, reached by `Manual`.
    fn profile_with(key: &ItemKey, from: Status) -> Profile {
        let mut p = Profile::new();
        p.transition(key.clone(), from, Manual, at(1), None)
            .unwrap();
        p
    }

    #[test]
    fn exactly_the_transitions_of_prd_section_7_are_accepted() {
        let key = el(Element::Arc);
        let allowed = prd_transitions();
        for &from in Status::ALL {
            for &to in Status::ALL {
                for provenance in [SelfReport, Qa, ReferenceRepo, SideLine, WroteIt, Manual] {
                    let mut p = profile_with(&key, from);
                    let accepted = p
                        .transition(key.clone(), to, provenance, at(2), None)
                        .is_ok();
                    assert_eq!(
                        accepted,
                        allowed.contains(&(from, to, provenance)),
                        "{from} -> {to} by {provenance}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_rejected_transition_returns_the_attempt_and_changes_nothing() {
        let key = el(Element::Option);
        let mut p = profile_with(&key, Known);
        let before = p.clone();

        let result = p.transition(key.clone(), Unknown, Qa, at(2), None);

        assert_eq!(
            result,
            Err(TransitionError {
                key,
                from: Known,
                to: Unknown,
                provenance: Qa
            })
        );
        assert_eq!(p, before);
    }

    #[test]
    fn nothing_becomes_known_by_self_report() {
        let key = el(Element::Arc);
        let mut p = Profile::new();

        assert!(
            p.transition(key.clone(), Known, SelfReport, at(1), None)
                .is_err()
        );
        assert_eq!(p.status(&key), Unknown);
        assert!(p.history().is_empty());
    }

    #[test]
    fn a_report_is_kept_per_category_and_changes_no_status() {
        let mut p = Profile::new();

        p.set_report(Category::Borrowing, Report::CanRead);

        assert_eq!(p.report(Category::Borrowing), Some(Report::CanRead));
        assert_eq!(p.report(Category::Ownership), None);
        assert_eq!(p.entries().count(), 0);
        assert!(p.history().is_empty());
    }

    #[test]
    fn a_key_that_is_not_an_element_or_crate_fails_to_load_with_its_name() {
        let text = "[items.renamed_element]\nstatus = \"known\"\nprovenance = \"qa\"\nat = 2026-10-10T12:34:56Z\n";
        let error = Profile::from_toml(text).unwrap_err();
        assert!(error.to_string().contains("renamed_element"), "{error}");
    }

    #[test]
    fn an_unknown_element_in_allow_or_deny_fails_to_load() {
        for text in ["allow = [\"nope\"]\n", "deny = [\"nope\"]\n"] {
            let error = Profile::from_toml(text).unwrap_err();
            assert!(error.to_string().contains("nope"), "{text}: {error}");
        }
    }

    #[test]
    fn an_unknown_category_in_reports_fails_to_load() {
        let error = Profile::from_toml("[reports]\nconcurrency = \"can_read\"\n").unwrap_err();
        assert!(error.to_string().contains("concurrency"), "{error}");
    }

    #[test]
    fn an_unknown_field_fails_to_load() {
        for text in [
            "stray = 1\n",
            "[items.arc]\nstatus = \"known\"\nprovenance = \"qa\"\nat = 2026-10-10T12:34:56Z\nstauts = \"seen\"\n",
        ] {
            assert!(Profile::from_toml(text).is_err(), "{text}");
        }
    }

    #[test]
    fn a_time_without_date_time_and_offset_fails_to_load() {
        for value in ["2026-10-10", "12:34:56", "2026-10-10T12:34:56"] {
            let text =
                format!("[items.arc]\nstatus = \"known\"\nprovenance = \"qa\"\nat = {value}\n");
            assert!(Profile::from_toml(&text).is_err(), "{value}");
        }
    }

    #[test]
    fn loading_a_missing_file_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profile.toml");

        assert!(matches!(
            Profile::load(&path),
            Err(StoreError::NotFound { path: p }) if p == path
        ));
    }

    #[test]
    fn saving_replaces_the_previous_file_and_leaves_no_temporary_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profile.toml");
        fs::write(&path, "old").unwrap();

        filled_profile().save(&path).unwrap();

        assert_eq!(Profile::load(&path).unwrap(), filled_profile());
        let names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["profile.toml"]);
    }

    #[cfg(unix)]
    #[test]
    fn saving_through_a_symbolic_link_keeps_the_link_and_updates_its_target() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("real.toml");
        let link = dir.path().join("profile.toml");
        fs::write(&target, "").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        filled_profile().save(&link).unwrap();

        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(Profile::load(&target).unwrap(), filled_profile());
    }

    #[cfg(unix)]
    #[test]
    fn saving_through_a_dangling_symbolic_link_keeps_the_link_and_creates_its_target() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("real.toml");
        let link = dir.path().join("profile.toml");
        std::os::unix::fs::symlink("real.toml", &link).unwrap();

        filled_profile().save(&link).unwrap();

        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(Profile::load(&target).unwrap(), filled_profile());
    }

    #[test]
    fn a_save_that_fails_to_create_the_temporary_file_leaves_the_file_unchanged() {
        // The temporary file's name is longer than the target's, so a target
        // name near the 255-byte limit makes creating it fail even for root.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p".repeat(250));
        let before = "# kept\n";
        fs::write(&path, before).unwrap();

        let result = filled_profile().save(&path);

        assert!(matches!(result, Err(StoreError::Io { .. })), "{result:?}");
        assert_eq!(fs::read_to_string(&path).unwrap(), before);
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn every_report_name_parses_back_and_the_names_are_the_three_answers_of_uc1() {
        let names: Vec<_> = Report::ALL.iter().map(|r| r.name()).collect();
        assert_eq!(names, ["have_written", "can_read", "dont_know"]);
        for &report in Report::ALL {
            assert_eq!(report.name().parse::<Report>(), Ok(report));
        }
        assert_eq!(
            "known".parse::<Report>(),
            Err(NotAReport("known".to_owned()))
        );
    }

    /// Whether this process can write into a directory without write
    /// permission (root can), judged by the owner of a file it creates.
    #[cfg(unix)]
    fn writes_ignore_permissions(dir: &Path) -> bool {
        use std::os::unix::fs::MetadataExt;
        let probe = dir.join("owner-probe");
        fs::write(&probe, "").unwrap();
        let uid = fs::metadata(&probe).unwrap().uid();
        fs::remove_file(&probe).unwrap();
        uid == 0
    }

    #[cfg(unix)]
    #[test]
    fn saving_into_a_read_only_directory_fails_and_leaves_the_file_unchanged() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        if writes_ignore_permissions(dir.path()) {
            eprintln!("skipped: running as root, which can write into a read-only directory");
            return;
        }
        let path = dir.path().join("profile.toml");
        let before = Profile::new().to_toml() + "# kept\n";
        fs::write(&path, &before).unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o555)).unwrap();

        let result = filled_profile().save(&path);

        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(result, Err(StoreError::Io { .. })), "{result:?}");
        assert_eq!(fs::read_to_string(&path).unwrap(), before);
        let names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["profile.toml"]);
    }

    #[test]
    fn a_timestamp_from_unix_seconds_is_the_utc_date_time() {
        assert_eq!(at(0).to_string(), "1970-01-01T00:00:00Z");
        assert_eq!(at(1_709_251_199).to_string(), "2024-02-29T23:59:59Z");
        assert_eq!(at(1_791_635_696).to_string(), "2026-10-10T12:34:56Z");
    }
}
