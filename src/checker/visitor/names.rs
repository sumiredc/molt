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
        _ => return None,
    };
    Some(element)
}

/// The elements a method call names, from the method's name and its number
/// of arguments.
pub(super) fn method_elements(name: &str, args: usize) -> Vec<Element> {
    let element = match name {
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
