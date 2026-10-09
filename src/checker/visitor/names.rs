//! Elements told apart by name alone: the segments of a path and the name of
//! a called method. syn has no types, so a name is all there is to go on.

use syn::Expr;

use crate::profile::element::Element;

/// How a path is used, for the method named by its last segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PathUse {
    /// Imported by `use`, or the path of a macro: it names no method call.
    NotACall,
    /// Called with this many arguments: `Option::expect(x, "m")` is the
    /// method `expect` with one argument besides the receiver.
    Call(usize),
    /// Anything else, such as a function passed as a value
    /// (`.filter_map(Result::ok)`), where the arguments are not known.
    Other,
}

/// The elements a path names, from its segments (`["std", "mem", "take"]`),
/// each with the index of the segment that decides it. The same rules hold
/// for a path in code and for a path imported by `use`.
pub(super) fn path_elements(segments: &[&str], usage: PathUse) -> Vec<(Element, usize)> {
    let mut found: Vec<(Element, usize)> = Vec::new();
    let mut add = |element: Element, index: usize| {
        if !found.iter().any(|&(e, _)| e == element) {
            found.push((element, index));
        }
    };
    let Some(last_index) = segments.len().checked_sub(1) else {
        return Vec::new();
    };
    let last = segments[last_index];
    let before_last = last_index.checked_sub(1).map(|i| segments[i]);
    let qualified = segments.len() > 1;

    // A type or trait anywhere in the path: `PhantomData<T>`,
    // `std::marker::PhantomData`, `From::from`.
    for (index, &segment) in segments.iter().enumerate() {
        if let Some(element) = type_name(segment) {
            add(element, index);
        }
        // `std::thread::Builder::new().spawn(f)` spawns a thread too.
        if segment == "Builder" && index > 0 && segments[index - 1] == "thread" {
            add(Element::ThreadSpawn, index);
        }
    }

    match (before_last, last) {
        (Some("mem"), "take") => add(Element::MemTake, last_index),
        (Some("mem"), "replace") => add(Element::MemReplace, last_index),
        (Some("mem"), "swap") => add(Element::MemSwap, last_index),
        (Some("mem"), "drop") | (None, "drop") => add(Element::Drop, last_index),
        (Some("thread"), "spawn") => add(Element::ThreadSpawn, last_index),
        (Some("thread"), "scope") => add(Element::ThreadScope, last_index),
        // `Box::pin(x)` makes a `Pin<Box<_>>`.
        (Some("Box"), "pin") => add(Element::Pin, last_index),
        _ => {}
    }
    match last {
        // Distinctive enough to count bare, as after `use std::mem::transmute`.
        "transmute" | "transmute_copy" => add(Element::Transmute, last_index),
        // `String::from(s)`, `u64::from(n)`: a conversion through `From`.
        "from" | "try_from" if qualified => add(Element::FromInto, last_index),
        _ => {}
    }
    // A method called or passed through its path: `Clone::clone(&x)`,
    // `Option::unwrap(x)`, `.filter_map(Result::ok)`. The receiver is not
    // known, so a name both iterators and `Option` have is both. Only after
    // a type or trait (`Option::`, `Iterator::`, `str::`): a function of a
    // module (`repo::find`, `std::cmp::max`) or of the type being written
    // (`Self::all`) is not a method of std.
    let after_type = before_last.is_some_and(|owner| {
        owner != "Self" && (owner.starts_with(char::is_uppercase) || owner == "str")
    });
    if after_type && usage != PathUse::NotACall {
        let arities = match usage {
            PathUse::Call(args) => args.checked_sub(1).into_iter().collect::<Vec<_>>(),
            _ => vec![0, 1, 2],
        };
        for args in arities {
            for element in method_elements(last, args, None) {
                add(element, last_index);
            }
        }
    }
    found
}

/// Modules whose names a glob import brings in without naming them, and the
/// element those names stand for: after `use std::sync::mpsc::*;`, a bare
/// `channel()` is a channel.
pub(super) fn glob_exports(module: &str) -> Option<(Element, &'static [&'static str])> {
    match module {
        "mpsc" => Some((
            Element::Channel,
            &[
                "channel",
                "sync_channel",
                "Sender",
                "SyncSender",
                "Receiver",
            ],
        )),
        "atomic" => Some((Element::Atomic, &["Ordering", "fence", "compiler_fence"])),
        _ => None,
    }
}

/// The element a type or trait name stands for, wherever it is in a path.
fn type_name(segment: &str) -> Option<Element> {
    let element = match segment {
        "From" | "Into" | "TryFrom" | "TryInto" => Element::FromInto,
        "PhantomData" => Element::PhantomData,
        "Box" => Element::Box,
        "Rc" => Element::Rc,
        "Arc" => Element::Arc,
        "Weak" => Element::Weak,
        "Cell" => Element::Cell,
        "RefCell" => Element::RefCell,
        "Mutex" | "MutexGuard" => Element::Mutex,
        "RwLock" | "RwLockReadGuard" | "RwLockWriteGuard" => Element::RwLock,
        // `std::sync::atomic::Ordering` too: it is only used with atomics.
        "atomic" => Element::Atomic,
        name if name.len() > "Atomic".len() && name.starts_with("Atomic") => Element::Atomic,
        "OnceCell" | "OnceLock" => Element::OnceCell,
        "LazyCell" | "LazyLock" => Element::LazyLock,
        "Cow" => Element::Cow,
        "mpsc" => Element::Channel,
        "Send" | "Sync" => Element::SendSync,
        "Result" | "Ok" | "Err" => Element::Result,
        "Option" | "Some" | "None" => Element::Option,
        "Fn" | "FnMut" | "FnOnce" => Element::FnTrait,
        "Future" | "IntoFuture" => Element::Future,
        "Pin" => Element::Pin,
        _ => return None,
    };
    Some(element)
}

/// What the receiver of a method call is, as far as the calls before it tell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Receiver {
    Iterator,
    OptionOrResult,
}

/// What `expr` evaluates to, judged from the method chain it ends: `None`
/// when the chain does not tell.
pub(super) fn receiver_kind(expr: &Expr) -> Option<Receiver> {
    match expr {
        Expr::MethodCall(call) => {
            let name = call.method.to_string();
            let args = call.args.len();
            if ITER_SOURCES.contains(&name.as_str()) || is_iterator_adapter(&name, args) {
                Some(Receiver::Iterator)
            } else if AMBIGUOUS_ADAPTERS.contains(&name.as_str()) {
                // `map` and the like keep what their receiver is.
                receiver_kind(&call.receiver)
            } else if OPTION_OR_RESULT_SOURCES.contains(&name.as_str()) {
                Some(Receiver::OptionOrResult)
            } else {
                None
            }
        }
        Expr::Call(call) => match &*call.func {
            Expr::Path(path)
                if path
                    .path
                    .segments
                    .last()
                    .is_some_and(|s| ["Some", "Ok", "Err"].iter().any(|n| s.ident == n)) =>
            {
                Some(Receiver::OptionOrResult)
            }
            _ => None,
        },
        Expr::Path(path) if path.path.is_ident("None") => Some(Receiver::OptionOrResult),
        Expr::Range(_) => Some(Receiver::Iterator),
        Expr::Paren(inner) => receiver_kind(&inner.expr),
        _ => None,
    }
}

/// The elements a method call names, from the method's name, its number of
/// arguments and what its receiver is.
pub(super) fn method_elements(name: &str, args: usize, receiver: Option<Receiver>) -> Vec<Element> {
    if AMBIGUOUS_ADAPTERS.contains(&name) {
        // Without types, `x.map(f)` is an iterator adapter or a combinator
        // depending on `x`. When the chain does not tell, both are reported,
        // so that neither is let through unknown.
        return match receiver {
            Some(Receiver::Iterator) => vec![Element::IteratorAdapter],
            Some(Receiver::OptionOrResult) => vec![Element::ResultCombinator],
            None => vec![Element::IteratorAdapter, Element::ResultCombinator],
        };
    }
    let element = match name {
        "iter" | "iter_mut" | "into_iter" => Element::IterMethod,
        name if is_iterator_adapter(name, args) => Element::IteratorAdapter,
        name if is_iterator_consumer(name, args) => Element::IteratorConsumer,
        "clone" => Element::Clone,
        "to_owned" => Element::ToOwned,
        "into" | "try_into" => Element::FromInto,
        "unwrap" => Element::Unwrap,
        "expect" if args == 1 => Element::Expect,
        name if RESULT_COMBINATORS.contains(&name) => Element::ResultCombinator,
        _ => return Vec::new(),
    };
    vec![element]
}

fn is_iterator_adapter(name: &str, args: usize) -> bool {
    // `Option::take()` has no argument; `Iterator::take(n)` has one.
    ITERATOR_ADAPTERS.contains(&name) || (name == "take" && args == 1)
}

fn is_iterator_consumer(name: &str, args: usize) -> bool {
    // `a.max(b)` is `Ord::max`; the iterator's `max()` has no argument.
    ITERATOR_CONSUMERS.contains(&name) || (["max", "min"].contains(&name) && args == 0)
}

/// Methods that give an iterator, besides `iter_method`. They are no element
/// of their own; they tell what a `map` after them is.
const ITER_SOURCES: &[&str] = &[
    "iter",
    "iter_mut",
    "into_iter",
    "chars",
    "char_indices",
    "bytes",
    "lines",
    "split_whitespace",
    "keys",
    "values",
    "values_mut",
    "drain",
    "windows",
    "chunks",
];

/// Iterator adapters that `Option` and `Result` do not have.
const ITERATOR_ADAPTERS: &[&str] = &[
    "enumerate",
    "skip",
    "skip_while",
    "take_while",
    "step_by",
    "chain",
    "rev",
    "peekable",
    "flat_map",
    "filter_map",
    "map_while",
    "scan",
    "fuse",
    "cycle",
];

/// Adapters that both iterators and `Option` / `Result` have.
const AMBIGUOUS_ADAPTERS: &[&str] = &[
    "map", "filter", "flatten", "zip", "inspect", "copied", "cloned",
];

/// Iterator consumers. Some names are shared with other types (`last` of a
/// slice, `find` of a `str`); those are reported as consumers too.
const ITERATOR_CONSUMERS: &[&str] = &[
    "collect",
    "sum",
    "product",
    "fold",
    "count",
    "for_each",
    "reduce",
    "all",
    "any",
    "find",
    "find_map",
    "position",
    "rposition",
    "max_by",
    "min_by",
    "max_by_key",
    "min_by_key",
    "last",
    "nth",
    "next",
    "partition",
    "unzip",
    "try_fold",
    "try_for_each",
];

/// Methods that give an `Option` or a `Result`, so that a `map` after them is
/// a combinator.
const OPTION_OR_RESULT_SOURCES: &[&str] = &[
    "ok",
    "err",
    "ok_or",
    "ok_or_else",
    "and_then",
    "or_else",
    "map_err",
    "and",
    "or",
    "xor",
    "as_deref",
    "as_deref_mut",
    "transpose",
    "inspect_err",
    "next",
    "find",
    "find_map",
    "position",
    "max_by",
    "min_by",
    "max_by_key",
    "min_by_key",
    "last",
    "nth",
    "reduce",
    "get",
    "get_mut",
    "first",
    "pop",
    "parse",
];

/// Methods of `Result` / `Option` that no iterator has. The ones an iterator
/// also has (`map`, `filter`, ..) are told apart by the receiver.
const RESULT_COMBINATORS: &[&str] = &[
    "map_err",
    "ok_or",
    "ok_or_else",
    "and_then",
    "or_else",
    "and",
    "or",
    "xor",
    "unwrap_or",
    "unwrap_or_else",
    "unwrap_or_default",
    "map_or",
    "map_or_else",
    "ok",
    "err",
    "is_some",
    "is_none",
    "is_ok",
    "is_err",
    "is_some_and",
    "is_none_or",
    "is_ok_and",
    "is_err_and",
    "as_deref",
    "as_deref_mut",
    "transpose",
    "get_or_insert",
    "get_or_insert_with",
    "unwrap_err",
    "expect_err",
    "inspect_err",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_type_or_trait_name_is_its_element() {
        let cases = [
            ("From", Element::FromInto),
            ("Into", Element::FromInto),
            ("TryFrom", Element::FromInto),
            ("TryInto", Element::FromInto),
            ("PhantomData", Element::PhantomData),
            ("Box", Element::Box),
            ("Rc", Element::Rc),
            ("Arc", Element::Arc),
            ("Weak", Element::Weak),
            ("Cell", Element::Cell),
            ("RefCell", Element::RefCell),
            ("Mutex", Element::Mutex),
            ("MutexGuard", Element::Mutex),
            ("RwLock", Element::RwLock),
            ("RwLockReadGuard", Element::RwLock),
            ("RwLockWriteGuard", Element::RwLock),
            ("atomic", Element::Atomic),
            ("AtomicBool", Element::Atomic),
            ("OnceCell", Element::OnceCell),
            ("OnceLock", Element::OnceCell),
            ("LazyCell", Element::LazyLock),
            ("LazyLock", Element::LazyLock),
            ("Cow", Element::Cow),
            ("mpsc", Element::Channel),
            ("Send", Element::SendSync),
            ("Sync", Element::SendSync),
            ("Result", Element::Result),
            ("Ok", Element::Result),
            ("Err", Element::Result),
            ("Option", Element::Option),
            ("Some", Element::Option),
            ("None", Element::Option),
            ("Fn", Element::FnTrait),
            ("FnMut", Element::FnTrait),
            ("FnOnce", Element::FnTrait),
            ("Future", Element::Future),
            ("IntoFuture", Element::Future),
            ("Pin", Element::Pin),
        ];
        for (name, element) in cases {
            assert_eq!(type_name(name), Some(element), "{name}");
        }
    }

    #[test]
    fn every_method_name_in_the_tables_is_its_element() {
        let mut cases: Vec<(&str, usize, Element)> = vec![
            ("iter", 0, Element::IterMethod),
            ("iter_mut", 0, Element::IterMethod),
            ("into_iter", 0, Element::IterMethod),
            ("take", 1, Element::IteratorAdapter),
            ("max", 0, Element::IteratorConsumer),
            ("min", 0, Element::IteratorConsumer),
            ("clone", 0, Element::Clone),
            ("to_owned", 0, Element::ToOwned),
            ("into", 0, Element::FromInto),
            ("try_into", 0, Element::FromInto),
            ("unwrap", 0, Element::Unwrap),
            ("expect", 1, Element::Expect),
        ];
        cases.extend(
            ITERATOR_ADAPTERS
                .iter()
                .map(|&n| (n, 1, Element::IteratorAdapter)),
        );
        cases.extend(
            ITERATOR_CONSUMERS
                .iter()
                .map(|&n| (n, 1, Element::IteratorConsumer)),
        );
        cases.extend(
            RESULT_COMBINATORS
                .iter()
                .map(|&n| (n, 1, Element::ResultCombinator)),
        );
        for (name, args, element) in cases {
            assert_eq!(method_elements(name, args, None), [element], "{name}");
        }
    }

    #[test]
    fn a_method_after_every_iterator_source_or_option_source_is_told_apart() {
        for &source in ITER_SOURCES.iter().chain(ITERATOR_ADAPTERS) {
            let expr: Expr = syn::parse_str(&format!("x.{source}(a)")).unwrap();
            assert_eq!(receiver_kind(&expr), Some(Receiver::Iterator), "{source}");
        }
        for &source in OPTION_OR_RESULT_SOURCES {
            let expr: Expr = syn::parse_str(&format!("x.{source}(a)")).unwrap();
            assert_eq!(
                receiver_kind(&expr),
                Some(Receiver::OptionOrResult),
                "{source}"
            );
        }
    }

    #[test]
    fn transmute_and_its_copy_variant_are_transmute_even_bare() {
        for name in ["transmute", "transmute_copy"] {
            let found = path_elements(&[name], PathUse::Call(1));
            assert!(
                found.iter().any(|&(e, _)| e == Element::Transmute),
                "{name}"
            );
        }
    }

    #[test]
    fn every_ambiguous_adapter_is_both_without_a_receiver_and_one_with_it() {
        for &name in AMBIGUOUS_ADAPTERS {
            assert_eq!(
                method_elements(name, 1, None),
                [Element::IteratorAdapter, Element::ResultCombinator],
                "{name}"
            );
            assert_eq!(
                method_elements(name, 1, Some(Receiver::Iterator)),
                [Element::IteratorAdapter],
                "{name}"
            );
            assert_eq!(
                method_elements(name, 1, Some(Receiver::OptionOrResult)),
                [Element::ResultCombinator],
                "{name}"
            );
        }
    }

    #[test]
    fn every_name_a_glob_of_mpsc_or_atomic_brings_in_is_listed() {
        let (element, names) = glob_exports("mpsc").unwrap();
        assert_eq!(element, Element::Channel);
        assert_eq!(
            names,
            [
                "channel",
                "sync_channel",
                "Sender",
                "SyncSender",
                "Receiver"
            ]
        );
        let (element, names) = glob_exports("atomic").unwrap();
        assert_eq!(element, Element::Atomic);
        assert_eq!(names, ["Ordering", "fence", "compiler_fence"]);
        assert_eq!(glob_exports("mem"), None);
    }

    #[test]
    fn a_method_through_a_path_counts_only_after_a_type_or_trait() {
        let call = PathUse::Call(1);
        let consumers = |segments: &[&str]| {
            path_elements(segments, call)
                .iter()
                .any(|&(e, _)| e == Element::IteratorConsumer)
        };
        assert!(consumers(&["Iterator", "count"]));
        assert!(!consumers(&["repo", "count"]));
        assert!(!consumers(&["Self", "count"]));
    }
}
