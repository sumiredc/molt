//! Elements told apart by name alone: the segments of a path and the name of
//! a called method. syn has no types, so a name is all there is to go on.

use crate::profile::element::Element;

/// The elements a path names, from its segments (`["std", "mem", "take"]`).
/// The same rules hold for a path in code and for a path imported by `use`.
pub(super) fn path_elements(segments: &[&str]) -> Vec<Element> {
    let mut found = Vec::new();
    let last = segments.last().copied().unwrap_or("");
    let before_last = segments.len().checked_sub(2).map(|i| segments[i]);
    let qualified = segments.len() > 1;

    match (before_last, last) {
        (Some("mem"), "take") => found.push(Element::MemTake),
        (Some("mem"), "replace") => found.push(Element::MemReplace),
        (Some("mem"), "swap") => found.push(Element::MemSwap),
        (Some("mem"), "drop") | (None, "drop") => found.push(Element::Drop),
        _ => {}
    }
    // `Clone::clone(&x)`, `Rc::clone(&x)`, `str::to_owned(s)`: the method
    // called through its path.
    match last {
        "clone" if qualified => found.push(Element::Clone),
        "to_owned" if qualified => found.push(Element::ToOwned),
        _ => {}
    }
    found
}

/// The elements a method call names, from the method's name and its number
/// of arguments.
pub(super) fn method_elements(name: &str, _args: usize) -> Vec<Element> {
    let element = match name {
        "clone" => Element::Clone,
        "to_owned" => Element::ToOwned,
        _ => return Vec::new(),
    };
    vec![element]
}
