//! Elements told apart by name alone: the segments of a path and the name of
//! a called method. syn has no types, so a name is all there is to go on.

use syn::Expr;

use crate::profile::element::Element;

/// The elements a path names, from its segments (`["std", "mem", "take"]`).
/// The same rules hold for a path in code and for a path imported by `use`.
pub(super) fn path_elements(segments: &[&str]) -> Vec<Element> {
    let mut found = Vec::new();
    let last = segments.last().copied().unwrap_or("");
    let before_last = segments.len().checked_sub(2).map(|i| segments[i]);
    let qualified = segments.len() > 1;

    // A type or trait anywhere in the path: `PhantomData<T>`,
    // `std::marker::PhantomData`, `From::from`.
    for &segment in segments {
        if let Some(element) = type_name(segment)
            && !found.contains(&element)
        {
            found.push(element);
        }
    }

    match (before_last, last) {
        (Some("mem"), "take") => found.push(Element::MemTake),
        (Some("mem"), "replace") => found.push(Element::MemReplace),
        (Some("mem"), "swap") => found.push(Element::MemSwap),
        (Some("mem"), "drop") | (None, "drop") => found.push(Element::Drop),
        (Some("thread"), "spawn") => found.push(Element::ThreadSpawn),
        (Some("thread"), "scope") => found.push(Element::ThreadScope),
        // `Box::pin(x)` makes a `Pin<Box<_>>`.
        (Some("Box"), "pin") => found.push(Element::Pin),
        _ => {}
    }
    // `Clone::clone(&x)`, `Rc::clone(&x)`, `str::to_owned(s)`: the method
    // called through its path.
    match last {
        "clone" if qualified => found.push(Element::Clone),
        "to_owned" if qualified => found.push(Element::ToOwned),
        // `String::from(s)`, `u64::from(n)`: a conversion through `From`.
        "from" if qualified && !found.contains(&Element::FromInto) => found.push(Element::FromInto),
        _ => {}
    }
    found
}

/// The element a type or trait name stands for, wherever it is in a path.
fn type_name(segment: &str) -> Option<Element> {
    let element = match segment {
        "From" | "Into" => Element::FromInto,
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
        "into" => Element::FromInto,
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
