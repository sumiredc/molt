//! Several items at one boundary (PRD UC3's exception flow): the items of one category are bundled
//! into one side line, whose one choice holds for all of them, and the other categories get a side
//! line each, in the order their first item came. Crates count as one category. The main line
//! goes on only once every item has a choice ([`MainLine::resolve`]).
//!
//! [`MainLine::resolve`]: crate::agent::mainline::MainLine::resolve

use crate::agent::mainline::Boundary;
use crate::profile::element::Category;
use crate::profile::status::ItemKey;

/// The category `item` is bundled by: an element's category, or `None` for a crate.
fn category(item: &ItemKey) -> Option<Category> {
    match item {
        ItemKey::Element(element) => Some(element.category()),
        ItemKey::Crate(_) => None,
    }
}

/// The boundary of the side line to open first at `boundary`: its items of the category of its
/// first item, in their order, with the same cause.
pub(crate) fn first(boundary: Boundary) -> Boundary {
    let Some(head) = boundary.items.first().map(category) else {
        return boundary;
    };
    Boundary {
        items: boundary
            .items
            .into_iter()
            .filter(|item| category(item) == head)
            .collect(),
        cause: boundary.cause,
    }
}

/// Tests of bundling the items of one boundary, of choosing for each bundle before the main line
/// goes on, and of the side-line events in the session log. Named so that
/// `cargo test subline::bundle` selects them.
#[cfg(test)]
mod tests;
