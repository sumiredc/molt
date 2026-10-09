//! What the checker takes and returns (PRD §8.3): the context of a check, the
//! detected items and the report.

use std::collections::BTreeSet;
use std::fmt;

use crate::profile::element::Category;
use crate::profile::status::ItemKey;
use crate::profile::store::Profile;

/// What a check needs besides the content: assembled by the caller, so that
/// the check itself reads no file and no environment variable.
#[derive(Debug, Clone, Copy)]
pub struct CheckContext<'a> {
    /// Decides which items may be written: base elements and `known` items.
    pub profile: &'a Profile,
    /// The project's own top-level modules. The first segment of a path naming
    /// one of them is not an external crate.
    pub own_modules: &'a BTreeSet<String>,
}

/// One element or crate in the checked content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// 1-based line in the new content where the item starts.
    pub line: usize,
    pub key: ItemKey,
    /// The source the item was detected in. For a macro invocation it is the
    /// macro's path with `!` (`tokio::select!`), since syn cannot see its
    /// inside and the name is all there is to explain.
    pub text: String,
}

impl Finding {
    /// The category the item is explained under. A crate has none.
    pub fn category(&self) -> Option<Category> {
        match &self.key {
            ItemKey::Element(element) => Some(element.category()),
            ItemKey::Crate(_) => None,
        }
    }
}

/// The content is not Rust that syn can parse. A compile-type failure for the
/// model to fix, not a boundary (PRD §8.3, §11).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    /// 1-based line.
    pub line: usize,
    /// 1-based column, counted in characters.
    pub column: usize,
    pub message: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}: {}", self.line, self.column, self.message)
    }
}

/// The result of checking one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckReport {
    /// Every item is a base element or `known`.
    Clean,
    /// The items that are neither base elements nor `known`, in line order.
    /// Never empty.
    OutOfProfile(Vec<Finding>),
    ParseError(ParseError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::element::Element;

    fn finding(key: ItemKey) -> Finding {
        Finding {
            line: 1,
            key,
            text: String::new(),
        }
    }

    #[test]
    fn an_element_finding_is_in_the_category_of_its_element() {
        let finding = finding(ItemKey::Element(Element::MacroCall));
        assert_eq!(finding.category(), Some(Category::Macros));
    }

    #[test]
    fn a_crate_finding_has_no_category() {
        let finding = finding(ItemKey::Crate("tokio".to_owned()));
        assert_eq!(finding.category(), None);
    }
}
