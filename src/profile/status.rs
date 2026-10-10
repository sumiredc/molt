//! Status and provenance of a profile item, and the key that names the item
//! (PRD §2, §8.3).
//!
//! Each value has one name, used both in the profile file and on the command
//! line, so the toml form and the `molt profile` input cannot disagree.

use std::fmt;
use std::fmt::Write as _;
use std::str::FromStr;

use serde::de::{self, Deserialize, Deserializer};
use serde::ser::{Serialize, Serializer};

use super::element::{Category, Element};

/// Implements `Serialize` / `Deserialize` through `Display` / `FromStr`, so the
/// toml form is exactly the name.
macro_rules! serde_by_name {
    ($ty:ty) => {
        impl Serialize for $ty {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.collect_str(self)
            }
        }

        impl<'de> Deserialize<'de> for $ty {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                String::deserialize(deserializer)?
                    .parse()
                    .map_err(de::Error::custom)
            }
        }
    };
}
pub(super) use serde_by_name;

/// Declares a field-less enum, its `ALL` and its `name()` from one table, so a
/// variant cannot lack a name or be missing from `ALL`.
macro_rules! named_enum {
    (
        $(#[$enum_doc:meta])*
        pub enum $ty:ident {
            $( $(#[$doc:meta])* $variant:ident => $name:literal, )*
        }
        $(#[$all_doc:meta])*
        $all_vis:vis const ALL;
        $(#[$name_doc:meta])*
        pub fn name;
    ) => {
        $(#[$enum_doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $ty {
            $( $(#[$doc])* $variant, )*
        }

        impl $ty {
            $(#[$all_doc])*
            $all_vis const ALL: &[$ty] = &[ $( $ty::$variant, )* ];

            $(#[$name_doc])*
            pub fn name(self) -> &'static str {
                match self {
                    $( $ty::$variant => $name, )*
                }
            }
        }
    };
}
pub(super) use named_enum;

named_enum! {
    /// How well the user knows an item. Only `Known` lets generation use it.
    pub enum Status {
        /// Can write it. Generation may use it.
        Known => "known",
        /// Can read it, or saw it on the side line. Generation does not use it.
        Seen => "seen",
        /// Does not know it. An item with no entry in the profile is this.
        Unknown => "unknown",
    }
    /// Every status, in the order shown as the valid inputs of `molt profile set`.
    pub const ALL;
    /// The name used in the profile file and as the input of `molt profile set`.
    pub fn name;
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The string is not the name of any status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotAStatus(pub String);

impl fmt::Display for NotAStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "not a status: {}", self.0)
    }
}

impl std::error::Error for NotAStatus {}

impl FromStr for Status {
    type Err = NotAStatus;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Status::ALL
            .iter()
            .copied()
            .find(|status| status.name() == s)
            .ok_or_else(|| NotAStatus(s.to_owned()))
    }
}

serde_by_name!(Status);

named_enum! {
    /// Why an item's status became what it is. Every change of status carries one,
    /// so it can be checked afterwards that nothing became `known` by molt's
    /// judgement alone (PRD §3, §8.8).
    pub enum Provenance {
        /// The user's own report during `molt init`.
        SelfReport => "self_report",
        /// Judged from the user's answers in the Q&A of `molt init`.
        Qa => "qa",
        /// A crate the reference repository depends on.
        ReferenceRepo => "reference_repo",
        /// Understood on the side line.
        SideLine => "side_line",
        /// The user wrote it themselves.
        WroteIt => "wrote_it",
        /// Changed by hand with `molt profile set`.
        Manual => "manual",
    }
    /// Every provenance, for lookup by name.
    const ALL;
    /// The name used in the profile file and in `molt profile show`.
    pub fn name;
}

impl fmt::Display for Provenance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The string is not the name of any provenance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotAProvenance(pub String);

impl fmt::Display for NotAProvenance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "not a provenance: {}", self.0)
    }
}

impl std::error::Error for NotAProvenance {}

impl FromStr for Provenance {
    type Err = NotAProvenance;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Provenance::ALL
            .iter()
            .copied()
            .find(|provenance| provenance.name() == s)
            .ok_or_else(|| NotAProvenance(s.to_owned()))
    }
}

serde_by_name!(Provenance);

/// The prefix that marks a crate key, so a crate never collides with an element
/// of the same name (`arc` the element, `crate:arc` the crate).
const CRATE_PREFIX: &str = "crate:";

/// What the profile holds a status for: a syntax element or an external crate.
///
/// In the profile file an element is its bare name (`arc`) and a crate is
/// `"crate:<name>"` (`"crate:tokio"`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ItemKey {
    Element(Element),
    /// An external crate, by the name it is depended on with. The name is kept
    /// as given: `proc-macro2` and `proc_macro2` are different keys, so the
    /// detector has to settle on one spelling before making the key.
    Crate(String),
}

impl fmt::Display for ItemKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ItemKey::Element(element) => f.write_str(element.name()),
            ItemKey::Crate(name) => write!(f, "{CRATE_PREFIX}{name}"),
        }
    }
}

/// The string is neither an element name nor `crate:<name>` with a non-empty
/// name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotAnItemKey(pub String);

impl fmt::Display for NotAnItemKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "not an element name or crate:<name>: {}", self.0)
    }
}

impl std::error::Error for NotAnItemKey {}

impl FromStr for ItemKey {
    type Err = NotAnItemKey;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let not_a_key = || NotAnItemKey(s.to_owned());
        match s.strip_prefix(CRATE_PREFIX) {
            Some(name) if !name.is_empty() => Ok(ItemKey::Crate(name.to_owned())),
            Some(_) => Err(not_a_key()),
            None => s.parse().map(ItemKey::Element).map_err(|_| not_a_key()),
        }
    }
}

serde_by_name!(ItemKey);

/// The key and status of `molt profile set <key> <status>`. When either is
/// invalid, the message names what is wrong and lists the valid keys and
/// statuses. `molt init` takes its corrections in the same form.
pub(crate) fn parse_setting(key: &str, status: &str) -> Result<(ItemKey, Status), String> {
    match (key.parse::<ItemKey>(), status.parse::<Status>()) {
        (Ok(key), Ok(status)) => Ok((key, status)),
        (key, status) => {
            let mut message = String::new();
            if let Err(error) = key {
                writeln!(message, "{error}").unwrap();
            }
            if let Err(error) = status {
                writeln!(message, "{error}").unwrap();
            }
            message.push_str(&valid_inputs());
            Err(message)
        }
    }
}

/// The valid keys and statuses of `molt profile set`, shown when one is
/// rejected.
fn valid_inputs() -> String {
    let mut text = String::from("\nvalid keys:\n");
    for category in Category::ALL {
        let names: Vec<_> = category.elements().map(Element::name).collect();
        writeln!(text, "  {category}: {}", names.join(", ")).unwrap();
    }
    text.push_str("  crates: crate:<name>\n");
    let statuses: Vec<_> = Status::ALL.iter().map(|status| status.name()).collect();
    writeln!(text, "valid statuses: {}", statuses.join(", ")).unwrap();
    text
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn every_status_name_parses_back_to_the_same_status() {
        for &status in Status::ALL {
            assert_eq!(status.name().parse::<Status>(), Ok(status));
        }
    }

    #[test]
    fn the_status_names_are_known_seen_and_unknown() {
        let names: Vec<_> = Status::ALL.iter().map(|s| s.name()).collect();
        assert_eq!(names, ["known", "seen", "unknown"]);
    }

    #[test]
    fn a_string_that_is_not_a_status_name_is_rejected_with_the_input() {
        assert_eq!(
            "Known".parse::<Status>(),
            Err(NotAStatus("Known".to_owned()))
        );
    }

    #[test]
    fn every_provenance_name_parses_back_to_the_same_provenance() {
        for &provenance in Provenance::ALL {
            assert_eq!(provenance.name().parse::<Provenance>(), Ok(provenance));
        }
    }

    #[test]
    fn the_provenances_are_the_six_kinds_of_prd() {
        let names: Vec<_> = Provenance::ALL.iter().map(|p| p.name()).collect();
        assert_eq!(
            names,
            [
                "self_report",
                "qa",
                "reference_repo",
                "side_line",
                "wrote_it",
                "manual"
            ]
        );
    }

    #[test]
    fn provenance_names_are_distinct() {
        for (i, a) in Provenance::ALL.iter().enumerate() {
            for b in &Provenance::ALL[i + 1..] {
                assert_ne!(a.name(), b.name());
            }
        }
    }

    #[test]
    fn a_string_that_is_not_a_provenance_name_is_rejected_with_the_input() {
        assert_eq!(
            "molt".parse::<Provenance>(),
            Err(NotAProvenance("molt".to_owned()))
        );
    }

    #[test]
    fn an_element_key_is_the_bare_element_name_and_a_crate_key_has_the_crate_prefix() {
        assert_eq!(ItemKey::Element(Element::Arc).to_string(), "arc");
        assert_eq!(
            ItemKey::Crate("tokio".to_owned()).to_string(),
            "crate:tokio"
        );
    }

    #[test]
    fn every_element_key_and_a_crate_key_parse_back_to_the_same_key() {
        let keys = Element::ALL.iter().map(|&e| ItemKey::Element(e)).chain([
            ItemKey::Crate("tokio".to_owned()),
            ItemKey::Crate("serde_json".to_owned()),
            ItemKey::Crate("proc-macro2".to_owned()),
        ]);
        for key in keys {
            assert_eq!(key.to_string().parse::<ItemKey>(), Ok(key));
        }
    }

    #[test]
    fn a_crate_named_like_an_element_is_a_different_key_from_the_element() {
        assert_eq!(
            "crate:arc".parse::<ItemKey>(),
            Ok(ItemKey::Crate("arc".to_owned()))
        );
        assert_ne!("crate:arc".parse::<ItemKey>(), "arc".parse::<ItemKey>());
    }

    #[test]
    fn a_key_that_is_neither_an_element_nor_a_crate_with_a_name_is_rejected_with_the_input() {
        for input in ["not_an_element", "crate:", "tokio:crate", "Arc"] {
            assert_eq!(
                input.parse::<ItemKey>(),
                Err(NotAnItemKey(input.to_owned())),
                "{input}"
            );
        }
    }

    #[test]
    fn item_keys_as_toml_keys_are_written_as_arc_and_crate_tokio_and_read_back() {
        let table = BTreeMap::from([
            (ItemKey::Element(Element::Arc), Status::Known),
            (ItemKey::Crate("tokio".to_owned()), Status::Seen),
        ]);

        let written = toml::to_string(&table).unwrap();
        assert_eq!(written, "arc = \"known\"\n\"crate:tokio\" = \"seen\"\n");

        let read: BTreeMap<ItemKey, Status> = toml::from_str(&written).unwrap();
        assert_eq!(read, table);
    }

    #[test]
    fn provenance_in_toml_is_its_name_and_reads_back() {
        let table = BTreeMap::from([("p".to_owned(), Provenance::ReferenceRepo)]);

        let written = toml::to_string(&table).unwrap();
        assert_eq!(written, "p = \"reference_repo\"\n");

        let read: BTreeMap<String, Provenance> = toml::from_str(&written).unwrap();
        assert_eq!(read, table);
    }

    #[test]
    fn an_invalid_key_in_toml_fails_to_read() {
        let read = toml::from_str::<BTreeMap<ItemKey, Status>>("not_an_element = \"known\"\n");
        assert!(read.unwrap_err().to_string().contains("not_an_element"));
    }
}
