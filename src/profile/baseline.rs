//! Base elements: syntax elements that generation may use regardless of the
//! profile, because Rust cannot be written without them (PRD §2, §8.3).
//!
//! The default list lives here. The user adds to it with `allow` and removes
//! from it with `deny`.

use std::collections::BTreeSet;

use super::element::Element;

/// The default base elements.
///
/// Kept to what almost every program needs: shared references, `Result`,
/// `Option`, `?`, the basic std macros, `for` loops (over an iterator or a
/// range; `while` could replace them, but only by indexing by hand) and
/// ranges. Everything else, `clone()`
/// included (the core of the ownership category), has to be `known` before
/// generation uses it.
pub const DEFAULT: &[Element] = &[
    Element::SharedRef,
    Element::Result,
    Element::Option,
    Element::QuestionMark,
    Element::ForLoop,
    Element::Range,
    Element::PrintMacro,
    Element::FormatMacro,
    Element::VecMacro,
    Element::AssertMacro,
];

/// Whether `element` is a base element, given the user's `allow` (additions)
/// and `deny` (removals).
///
/// `deny` wins over `allow`: an element in both is not a base element. Setting
/// an allowed element to `unknown` adds it to `deny` without removing it from
/// `allow` (PRD UC5), and it must become a boundary again. A wrong "not base"
/// only makes a boundary, while a wrong "base" lets generation use something
/// the user may not know.
pub fn is_base(element: Element, allow: &BTreeSet<Element>, deny: &BTreeSet<Element>) -> bool {
    (DEFAULT.contains(&element) || allow.contains(&element)) && !deny.contains(&element)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set<const N: usize>(elements: [Element; N]) -> BTreeSet<Element> {
        BTreeSet::from(elements)
    }

    #[test]
    fn clone_is_not_a_default_base_element() {
        assert!(!DEFAULT.contains(&Element::Clone));
        assert!(!is_base(Element::Clone, &set([]), &set([])));
    }

    #[test]
    fn the_base_elements_named_in_prd_are_in_the_default() {
        for element in [
            Element::SharedRef,
            Element::Result,
            Element::Option,
            Element::QuestionMark,
            Element::PrintMacro,
        ] {
            assert!(DEFAULT.contains(&element), "{element}");
        }
    }

    #[test]
    fn with_no_allow_or_deny_exactly_the_default_elements_are_base() {
        for &element in Element::ALL {
            assert_eq!(
                is_base(element, &set([]), &set([])),
                DEFAULT.contains(&element),
                "{element}"
            );
        }
    }

    #[test]
    fn allow_makes_an_element_outside_the_default_base() {
        assert!(is_base(Element::Arc, &set([Element::Arc]), &set([])));
    }

    #[test]
    fn allow_does_not_make_other_elements_base() {
        assert!(!is_base(Element::Mutex, &set([Element::Arc]), &set([])));
    }

    #[test]
    fn deny_removes_a_default_element_from_the_base() {
        assert!(!is_base(Element::Option, &set([]), &set([Element::Option])));
    }

    #[test]
    fn deny_does_not_remove_other_default_elements() {
        assert!(is_base(Element::Result, &set([]), &set([Element::Option])));
    }

    #[test]
    fn deny_wins_over_allow_for_an_element_in_both() {
        let both = set([Element::Arc]);
        assert!(!is_base(Element::Arc, &both, &both));
    }
}
