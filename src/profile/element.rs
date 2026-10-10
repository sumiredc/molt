//! Syntax elements and the categories they are sorted into (PRD §2, §8.3).
//!
//! An element is the smallest unit of boundary judgement. Syntax outside the
//! 10 categories (`fn`, `struct`, `enum`, `match`, loops other than `for`,
//! modules) is not an element, so the checker never reports it.

use std::fmt;
use std::str::FromStr;

/// One of the 10 groups that syntax elements are sorted into. Explanations are
/// grouped per category, while boundaries are judged per element.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Category {
    Ownership,
    Borrowing,
    Lifetimes,
    TraitsGenerics,
    /// Also holds threads and synchronisation (`thread::spawn`, channels,
    /// atomics, `Send` / `Sync`), which no category covers and which are used
    /// together with `Arc` and `Mutex`.
    SmartPointers,
    ErrorHandling,
    IteratorsClosures,
    Async,
    Macros,
    Unsafe,
}

impl Category {
    /// Every category, in the order the profile is shown.
    pub const ALL: [Category; 10] = [
        Category::Ownership,
        Category::Borrowing,
        Category::Lifetimes,
        Category::TraitsGenerics,
        Category::SmartPointers,
        Category::ErrorHandling,
        Category::IteratorsClosures,
        Category::Async,
        Category::Macros,
        Category::Unsafe,
    ];

    /// The name used in the profile file and in `molt profile show`.
    pub fn name(self) -> &'static str {
        match self {
            Category::Ownership => "ownership",
            Category::Borrowing => "borrowing",
            Category::Lifetimes => "lifetimes",
            Category::TraitsGenerics => "traits_generics",
            Category::SmartPointers => "smart_pointers",
            Category::ErrorHandling => "error_handling",
            Category::IteratorsClosures => "iterators_closures",
            Category::Async => "async",
            Category::Macros => "macros",
            Category::Unsafe => "unsafe",
        }
    }

    /// The elements of this category, in the order of [`Element::ALL`].
    pub fn elements(self) -> impl Iterator<Item = Element> {
        Element::ALL
            .iter()
            .copied()
            .filter(move |element| element.category() == self)
    }
}

impl fmt::Display for Category {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Declares `Element` from one table, so that the variant list, `ALL`,
/// `category()` and `name()` cannot drift apart.
macro_rules! elements {
    ($(
        $category:ident {
            $( $(#[$doc:meta])* $variant:ident => $name:literal, )*
        }
    )*) => {
        /// A syntax element. Closed: the list is decided here and in
        /// `tests/fixtures/elements/`.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum Element {
            $( $( $(#[$doc])* $variant, )* )*
        }

        impl Element {
            /// Every element, grouped by category in `Category::ALL` order.
            pub const ALL: &[Element] = &[ $( $( Element::$variant, )* )* ];

            /// The category the element is explained under.
            pub fn category(self) -> Category {
                match self {
                    $( $( Element::$variant => Category::$category, )* )*
                }
            }

            /// The name used as the toml key and as the input of `molt profile set`.
            pub fn name(self) -> &'static str {
                match self {
                    $( $( Element::$variant => $name, )* )*
                }
            }
        }
    };
}

elements! {
    Ownership {
        /// `.clone()`
        Clone => "clone",
        /// `.to_owned()`
        ToOwned => "to_owned",
        /// An explicit `drop(x)`
        Drop => "drop",
        /// `std::mem::take`
        MemTake => "mem_take",
        /// `std::mem::replace`
        MemReplace => "mem_replace",
        /// `std::mem::swap`
        MemSwap => "mem_swap",
    }
    Borrowing {
        /// A shared reference: `&T`, `&x`, `&self`
        SharedRef => "shared_ref",
        /// A mutable reference: `&mut T`, `&mut x`, `&mut self`
        MutRef => "mut_ref",
        /// An explicit dereference: `*x`
        Deref => "deref",
        /// A `ref` / `ref mut` binding in a pattern
        RefPattern => "ref_pattern",
        /// A slice: `&[T]`, `&v[a..b]`
        Slice => "slice",
    }
    Lifetimes {
        /// A lifetime parameter: `<'a>`
        LifetimeParam => "lifetime_param",
        /// A named lifetime on a reference or a type: `&'a T`, `Foo<'a>`
        LifetimeAnnotation => "lifetime_annotation",
        /// `'static`
        StaticLifetime => "static_lifetime",
        /// The anonymous lifetime: `'_`
        AnonymousLifetime => "anonymous_lifetime",
        /// A lifetime bound: `T: 'a`, `'a: 'b`
        LifetimeBound => "lifetime_bound",
        /// A higher-ranked bound: `for<'a>`
        HigherRankedBound => "higher_ranked_bound",
    }
    TraitsGenerics {
        /// `trait Foo { .. }`
        TraitDef => "trait_def",
        /// `impl Trait for Type { .. }`
        TraitImpl => "trait_impl",
        /// `#[derive(..)]` of a std trait (`Debug`, `Clone`, `Copy`, `PartialEq`, ..)
        Derive => "derive",
        /// A type parameter: `<T>`
        GenericParam => "generic_param",
        /// A trait bound on a type parameter: `T: Trait`
        TraitBound => "trait_bound",
        /// `where` clause
        WhereClause => "where_clause",
        /// `impl Trait` in argument or return position
        ImplTrait => "impl_trait",
        /// `dyn Trait`
        DynTrait => "dyn_trait",
        /// An associated type: `type Item = ..;`, `T::Item`
        AssociatedType => "associated_type",
        /// An associated constant: `const N: usize;` in a trait or impl
        AssociatedConst => "associated_const",
        /// A const generic parameter: `<const N: usize>`
        ConstGeneric => "const_generic",
        /// A supertrait: `trait A: B`
        Supertrait => "supertrait",
        /// A generic associated type: `type Item<'a> where Self: 'a;`
        GenericAssociatedType => "generic_associated_type",
        /// A qualified path: `<T as Trait>::f`
        QualifiedPath => "qualified_path",
        /// Turbofish: `f::<T>()`, `collect::<Vec<_>>()`
        Turbofish => "turbofish",
        /// `From` / `Into` conversions: `impl From<A> for B`, `.into()`
        FromInto => "from_into",
        /// `std::marker::PhantomData`
        PhantomData => "phantom_data",
    }
    SmartPointers {
        /// `Box<T>`
        Box => "box",
        /// `Rc<T>`
        Rc => "rc",
        /// `Arc<T>`
        Arc => "arc",
        /// `Weak<T>`
        Weak => "weak",
        /// `Cell<T>`
        Cell => "cell",
        /// `RefCell<T>`
        RefCell => "ref_cell",
        /// `Mutex<T>`
        Mutex => "mutex",
        /// `RwLock<T>`
        RwLock => "rw_lock",
        /// Atomic types: `AtomicUsize`, `AtomicBool`, ..
        Atomic => "atomic",
        /// `OnceCell<T>` / `OnceLock<T>`
        OnceCell => "once_cell",
        /// `LazyCell<T>` / `LazyLock<T>`
        LazyLock => "lazy_lock",
        /// `Cow<'a, T>`
        Cow => "cow",
        /// `impl Deref for ..` / `impl DerefMut for ..`
        ImplDeref => "impl_deref",
        /// `impl Drop for ..`
        ImplDrop => "impl_drop",
        /// `std::thread::spawn`
        ThreadSpawn => "thread_spawn",
        /// `std::thread::scope`
        ThreadScope => "thread_scope",
        /// Channels: `std::sync::mpsc::channel`, `sync_channel`
        Channel => "channel",
        /// `Send` / `Sync` in a bound or an impl
        SendSync => "send_sync",
    }
    ErrorHandling {
        /// `Result<T, E>`
        Result => "result",
        /// `Option<T>`
        Option => "option",
        /// The `?` operator
        QuestionMark => "question_mark",
        /// `.unwrap()`
        Unwrap => "unwrap",
        /// `.expect(..)`
        Expect => "expect",
        /// Combinators on `Result` / `Option`: `map_err`, `ok_or`, `and_then`, `unwrap_or`, ..
        ResultCombinator => "result_combinator",
        /// `impl std::error::Error for ..`
        ImplError => "impl_error",
        /// `Box<dyn Error>`
        BoxDynError => "box_dyn_error",
        /// `panic!`, `unreachable!`, `todo!`, `unimplemented!`
        Panic => "panic",
    }
    IteratorsClosures {
        /// A `for` loop over an iterator
        ForLoop => "for_loop",
        /// A range: `a..b`, `a..=b`
        Range => "range",
        /// `.iter()`, `.iter_mut()`, `.into_iter()`
        IterMethod => "iter_method",
        /// Iterator adapters: `map`, `filter`, `enumerate`, `zip`, ..
        IteratorAdapter => "iterator_adapter",
        /// Iterator consumers: `collect`, `sum`, `fold`, `count`, ..
        IteratorConsumer => "iterator_consumer",
        /// `impl Iterator for ..`
        ImplIterator => "impl_iterator",
        /// A closure: `|x| x + 1`
        Closure => "closure",
        /// A `move` closure
        MoveClosure => "move_closure",
        /// `Fn` / `FnMut` / `FnOnce` in a bound or a type
        FnTrait => "fn_trait",
        /// A function pointer type: `fn(i32) -> i32`
        FnPointer => "fn_pointer",
    }
    Async {
        /// `async fn`
        AsyncFn => "async_fn",
        /// `.await`
        Await => "await",
        /// `async { .. }` / `async move { .. }`
        AsyncBlock => "async_block",
        /// An async closure: `async || ..`
        AsyncClosure => "async_closure",
        /// `async fn` in a trait
        AsyncFnInTrait => "async_fn_in_trait",
        /// `impl Future for ..` / `Future` in a bound
        Future => "future",
        /// `Pin<P>`
        Pin => "pin",
    }
    Macros {
        /// `print!`, `println!`, `eprint!`, `eprintln!`
        PrintMacro => "print_macro",
        /// `format!`, `write!`, `writeln!`
        FormatMacro => "format_macro",
        /// `vec!`
        VecMacro => "vec_macro",
        /// `assert!`, `assert_eq!`, `assert_ne!`, `debug_assert!`, `debug_assert_eq!`, `debug_assert_ne!`
        AssertMacro => "assert_macro",
        /// `dbg!`
        DbgMacro => "dbg_macro",
        /// `matches!`
        MatchesMacro => "matches_macro",
        /// An invocation of a macro that has no element of its own (every
        /// element whose doc names a `name!` has one). Its inside is invisible
        /// to syn, so the invocation itself is the element
        MacroCall => "macro_call",
        /// `macro_rules!` definition
        MacroRules => "macro_rules",
        /// A procedural macro definition: `#[proc_macro]`, `#[proc_macro_derive]`, `#[proc_macro_attribute]`
        ProcMacroDef => "proc_macro_def",
        /// `#[derive(..)]` of a trait outside std (e.g. `serde::Serialize`)
        CustomDerive => "custom_derive",
        /// An attribute macro: `#[tokio::main]`, ..
        AttributeMacro => "attribute_macro",
    }
    Unsafe {
        /// `unsafe { .. }`
        UnsafeBlock => "unsafe_block",
        /// `unsafe fn`
        UnsafeFn => "unsafe_fn",
        /// `unsafe trait` / `unsafe impl`
        UnsafeTrait => "unsafe_trait",
        /// A raw pointer: `*const T`, `*mut T`
        RawPointer => "raw_pointer",
        /// `static mut`
        StaticMut => "static_mut",
        /// `union`
        Union => "union",
        /// `extern` block or `extern "C" fn`
        Extern => "extern",
        /// `std::mem::transmute`
        Transmute => "transmute",
        /// Inline assembly: `asm!`
        InlineAsm => "inline_asm",
    }
}

impl fmt::Display for Element {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The string is not the name of any element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotAnElement(pub String);

impl fmt::Display for NotAnElement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "not an element name: {}", self.0)
    }
}

impl std::error::Error for NotAnElement {}

impl FromStr for Element {
    type Err = NotAnElement;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Element::ALL
            .iter()
            .copied()
            .find(|e| e.name() == s)
            .ok_or_else(|| NotAnElement(s.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_element_name_parses_back_to_the_same_element() {
        for &element in Element::ALL {
            assert_eq!(element.name().parse::<Element>(), Ok(element));
        }
    }

    #[test]
    fn a_string_that_is_not_an_element_name_is_rejected_with_the_input() {
        assert_eq!(
            "not_an_element".parse::<Element>(),
            Err(NotAnElement("not_an_element".to_owned()))
        );
    }

    #[test]
    fn every_element_name_is_a_toml_bare_key_in_snake_case() {
        for &element in Element::ALL {
            let name = element.name();
            assert!(
                !name.is_empty()
                    && name
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
                "{name}"
            );
        }
    }

    #[test]
    fn every_category_has_at_least_one_element() {
        for category in Category::ALL {
            assert!(
                Element::ALL.iter().any(|e| e.category() == category),
                "{category}"
            );
        }
    }

    #[test]
    fn the_elements_named_in_prd_fall_in_the_categories_their_names_suggest() {
        // Elements named in PRD §2 (examples and base elements) and §8.3. The
        // PRD does not state their categories; the pairs are this list's decision.
        let cases = [
            (Element::Arc, Category::SmartPointers),
            (Element::Mutex, Category::SmartPointers),
            (Element::ImplTrait, Category::TraitsGenerics),
            (Element::DynTrait, Category::TraitsGenerics),
            (Element::Closure, Category::IteratorsClosures),
            (Element::AsyncFn, Category::Async),
            (Element::Await, Category::Async),
            (Element::LifetimeAnnotation, Category::Lifetimes),
            (Element::SharedRef, Category::Borrowing),
            (Element::Result, Category::ErrorHandling),
            (Element::Option, Category::ErrorHandling),
            (Element::QuestionMark, Category::ErrorHandling),
            (Element::PrintMacro, Category::Macros),
            (Element::Clone, Category::Ownership),
            (Element::MacroCall, Category::Macros),
        ];
        for (element, category) in cases {
            assert_eq!(element.category(), category, "{element}");
        }
    }

    #[test]
    fn all_elements_are_listed_in_category_order() {
        let order = |c: Category| Category::ALL.iter().position(|&x| x == c);
        for pair in Element::ALL.windows(2) {
            assert!(
                order(pair[0].category()) <= order(pair[1].category()),
                "{} before {}",
                pair[0],
                pair[1]
            );
        }
    }

    #[test]
    fn category_names_are_distinct() {
        for (i, a) in Category::ALL.iter().enumerate() {
            for b in &Category::ALL[i + 1..] {
                assert_ne!(a.name(), b.name());
            }
        }
    }
}
