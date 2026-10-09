//! Reads the arguments of the std macros whose arguments are Rust
//! expressions, so that the elements and crates inside them are checked.
//!
//! syn leaves the tokens of a macro unparsed, and the inside of a macro is
//! invisible in general (PRD §8.3, §11). The std macros below take arguments
//! of a known shape, and they are parsed here, once, for both `visitor` and
//! `crates` to walk. The parsed nodes keep the spans of the tokens, so a
//! finding inside them is on the line of its own token.

use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::visit::Visit;
use syn::{Expr, Ident, Macro, Pat, Token};

use crate::checker::visitor::{std_macro, std_name};
use crate::profile::element::Element;

/// What `mac`'s arguments are to the checker.
pub enum MacroArgs {
    /// Not a std macro whose arguments are read: any other macro, and `asm!`,
    /// whose arguments are assembly and operands.
    NotRead,
    /// Arguments that do not parse in the shape of the macro. Their inside
    /// is not seen.
    Unparsed,
    Parsed(Args),
}

/// The Rust syntax in the arguments of a std macro. A named argument of a
/// format string (`a = x`) is its expression alone.
#[derive(Default)]
pub struct Args {
    exprs: Vec<Expr>,
    /// The pattern of `matches!`.
    pats: Vec<Pat>,
}

/// The shape of the arguments of a std macro.
#[derive(Clone, Copy)]
enum Shape {
    /// Comma-separated expressions: a format string and its arguments,
    /// after any leading expressions (`println!`, `format!`, `write!`,
    /// `assert_eq!`, `panic!` and the like), or `dbg!`'s expressions. An
    /// argument may be named (`a = x`). `dbg!(a = x)` is an assignment
    /// instead, but its place `a` holds no element.
    Format,
    /// `vec![a, b]` or `vec![x; n]`.
    Vec,
    /// `matches!(expr, pat)` with an optional `if` guard.
    Matches,
}

/// Reads `mac`'s arguments.
pub fn read(mac: &Macro) -> MacroArgs {
    let Some(shape) = std_name(&mac.path)
        .and_then(|name| std_macro(&name))
        .and_then(shape)
    else {
        return MacroArgs::NotRead;
    };
    let parsed = match shape {
        Shape::Format => mac.parse_body_with(format_args),
        Shape::Vec => mac.parse_body_with(vec_args),
        Shape::Matches => mac.parse_body_with(matches_args),
    };
    match parsed {
        Ok(args) => MacroArgs::Parsed(args),
        Err(_) => MacroArgs::Unparsed,
    }
}

/// Walks the arguments with `visitor`.
pub fn visit<'a, V: Visit<'a>>(args: &'a Args, visitor: &mut V) {
    for expr in &args.exprs {
        visitor.visit_expr(expr);
    }
    for pat in &args.pats {
        visitor.visit_pat(pat);
    }
}

/// The shape of the arguments of a std macro, from the element `std_macro`
/// gives it, so that a name added to a family there is read here too.
/// `None` for `asm!` (`inline_asm`), whose arguments are not read. An element
/// added to `std_macro` needs its shape here, or its arguments go unread.
fn shape(element: Element) -> Option<Shape> {
    let shape = match element {
        Element::PrintMacro
        | Element::FormatMacro
        | Element::AssertMacro
        | Element::DbgMacro
        | Element::Panic => Shape::Format,
        Element::VecMacro => Shape::Vec,
        Element::MatchesMacro => Shape::Matches,
        _ => return None,
    };
    Some(shape)
}

/// Comma-separated arguments, each an expression or a named argument of a
/// format string (`a = x`), with an optional trailing comma.
fn format_args(input: ParseStream) -> syn::Result<Args> {
    let args = Punctuated::<FormatArg, Token![,]>::parse_terminated(input)?;
    Ok(Args {
        exprs: args.into_iter().map(|arg| arg.0).collect(),
        pats: Vec::new(),
    })
}

/// One argument of a format macro: the expression, without the name of a
/// named argument. `a == b` is an expression, not a named argument.
struct FormatArg(Expr);

impl Parse for FormatArg {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        if input.peek(Ident) && input.peek2(Token![=]) && !input.peek2(Token![==]) {
            input.parse::<Ident>()?;
            input.parse::<Token![=]>()?;
        }
        input.parse().map(FormatArg)
    }
}

/// `vec![]`, `vec![a, b]` or `vec![x; n]`.
fn vec_args(input: ParseStream) -> syn::Result<Args> {
    if input.is_empty() {
        return Ok(Args::default());
    }
    let mut exprs = vec![input.parse()?];
    if input.peek(Token![;]) {
        input.parse::<Token![;]>()?;
        exprs.push(input.parse()?);
    } else if !input.is_empty() {
        input.parse::<Token![,]>()?;
        exprs.extend(Punctuated::<Expr, Token![,]>::parse_terminated(input)?);
    }
    Ok(Args {
        exprs,
        pats: Vec::new(),
    })
}

/// `matches!(expr, pat)` or `matches!(expr, pat if guard)`, with an optional
/// trailing comma.
fn matches_args(input: ParseStream) -> syn::Result<Args> {
    let mut exprs = vec![input.parse()?];
    input.parse::<Token![,]>()?;
    let pat = Pat::parse_multi_with_leading_vert(input)?;
    if input.peek(Token![if]) {
        input.parse::<Token![if]>()?;
        exprs.push(input.parse()?);
    }
    if !input.is_empty() {
        input.parse::<Token![,]>()?;
    }
    Ok(Args {
        exprs,
        pats: vec![pat],
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::Path;

    use crate::checker::{CheckContext, CheckReport, check_file};
    use crate::profile::element::Element;
    use crate::profile::status::{ItemKey, Provenance, Status};
    use crate::profile::store::{Profile, Timestamp};

    /// What `check_file` reports for `new` against `old`, sorted by line and
    /// then by key.
    fn check_with(profile: &Profile, old: &str, new: &str) -> Vec<(usize, ItemKey)> {
        let own_modules = BTreeSet::new();
        let ctx = CheckContext {
            profile,
            own_modules: &own_modules,
        };
        let mut reported: Vec<(usize, ItemKey)> =
            match check_file(Path::new("src/lib.rs"), old, new, &ctx) {
                CheckReport::Clean => Vec::new(),
                CheckReport::OutOfProfile(findings) => {
                    findings.into_iter().map(|f| (f.line, f.key)).collect()
                }
                CheckReport::ParseError(error) => panic!("parse error {error:?}"),
            };
        reported.sort();
        reported
    }

    /// What `check_file` reports for `new` as a new file, with an empty
    /// profile.
    fn check(new: &str) -> Vec<(usize, ItemKey)> {
        check_with(&Profile::new(), "", new)
    }

    /// `findings` sorted as `check_with` sorts them.
    fn sorted<const N: usize>(findings: [(usize, ItemKey); N]) -> Vec<(usize, ItemKey)> {
        let mut findings = findings.to_vec();
        findings.sort();
        findings
    }

    /// `body` as the body of a function, on line 2.
    fn in_fn(body: &str) -> String {
        format!("fn f() {{\n    {body}\n}}\n")
    }

    /// A profile in which the base elements `denied` are `unknown`, and so
    /// reported.
    fn denying(denied: &[Element]) -> Profile {
        let mut profile = Profile::new();
        for element in denied {
            profile
                .transition(
                    ItemKey::Element(*element),
                    Status::Unknown,
                    Provenance::Manual,
                    Timestamp::from_unix_seconds(0),
                    None,
                )
                .unwrap();
        }
        profile
    }

    fn krate(name: &str) -> ItemKey {
        ItemKey::Crate(name.to_owned())
    }

    fn element(element: Element) -> ItemKey {
        ItemKey::Element(element)
    }

    #[test]
    fn a_crate_in_the_arguments_of_each_std_macro_is_reported() {
        let cases = [
            "println!(\"{}\", serde_json::to_string(&1));",
            "let _ = format!(\"{}\", serde_json::to_string(&1));",
            "let _ = write!(out, \"{}\", serde_json::to_string(&1));",
            "let _ = vec![serde_json::to_string(&1)];",
            "assert_eq!(serde_json::to_string(&1), x);",
        ];
        for body in cases {
            assert_eq!(check(&in_fn(body)), [(2, krate("serde_json"))], "{body}");
        }
    }

    #[test]
    fn a_crate_and_a_question_mark_in_the_arguments_are_both_reported() {
        let profile = denying(&[Element::QuestionMark]);
        let source = "fn f() -> R {\n    println!(\"{}\", serde_json::to_string(&x)?);\n}\n";
        assert_eq!(
            check_with(&profile, "", source),
            sorted([
                (2, element(Element::QuestionMark)),
                (2, krate("serde_json")),
            ])
        );
    }

    #[test]
    fn an_element_in_the_arguments_of_a_std_macro_is_reported() {
        assert_eq!(
            check(&in_fn("let _ = format!(\"{}\", x.clone());")),
            [(2, element(Element::Clone))]
        );
    }

    #[test]
    fn an_element_in_the_arguments_is_reported_on_the_line_of_its_token() {
        let old =
            "fn f() {\n    println!(\n        \"{} {}\",\n        a,\n        b,\n    );\n}\n";
        let new = "fn f() {\n    println!(\n        \"{} {}\",\n        a,\n        b.clone(),\n    );\n}\n";
        assert_eq!(
            check_with(&Profile::new(), old, new),
            [(5, element(Element::Clone))]
        );
    }

    #[test]
    fn the_arguments_of_a_std_macro_inside_another_are_checked() {
        assert_eq!(
            check(&in_fn("println!(\"{}\", format!(\"{}\", x.clone()));")),
            [(2, element(Element::Clone))]
        );
    }

    #[test]
    fn the_arguments_of_a_std_macro_named_through_std_are_checked() {
        assert_eq!(
            check(&in_fn("std::println!(\"{}\", x.clone());")),
            [(2, element(Element::Clone))]
        );
    }

    #[test]
    fn the_arguments_of_each_family_of_std_macros_are_checked() {
        let cases = [
            "print!(\"{}\", x.clone());",
            "eprint!(\"{}\", x.clone());",
            "eprintln!(\"{}\", x.clone());",
            "assert_ne!(x.clone(), y);",
            "debug_assert!(x.clone() == y);",
            "debug_assert_eq!(x.clone(), y);",
            "let _ = writeln!(out, \"{}\", x.clone());",
            "assert!(x.clone() == y);",
            "debug_assert_ne!(x.clone(), y, \"{}\", z);",
            "panic!(\"{}\", x.clone());",
            "unreachable!(\"{}\", x.clone());",
            "todo!(\"{}\", x.clone());",
            "unimplemented!(\"{}\", x.clone());",
        ];
        for body in cases {
            let reported = check(&in_fn(body));
            assert!(
                reported.contains(&(2, element(Element::Clone))),
                "{body}: {reported:?}"
            );
        }
    }

    #[test]
    fn the_arguments_of_dbg_are_checked() {
        assert_eq!(
            check(&in_fn("dbg!(x.clone());")),
            sorted([
                (2, element(Element::DbgMacro)),
                (2, element(Element::Clone)),
            ])
        );
    }

    #[test]
    fn the_inline_arguments_of_a_format_string_are_not_elements() {
        assert_eq!(check(&in_fn("println!(\"{x} {y:?} {:>width$}\", z);")), []);
    }

    #[test]
    fn the_expression_of_a_named_argument_is_checked() {
        assert_eq!(
            check(&in_fn("println!(\"{a}\", a = x.clone());")),
            [(2, element(Element::Clone))]
        );
    }

    #[test]
    fn the_name_of_a_named_argument_is_not_an_element() {
        // As an expression, `Rc = 1` would be an assignment to the path `Rc`.
        assert_eq!(check(&in_fn("println!(\"{Rc}\", Rc = 1);")), []);
    }

    #[test]
    fn a_comparison_in_the_arguments_is_not_a_named_argument() {
        assert_eq!(
            check(&in_fn("assert!(a == x.clone());")),
            [(2, element(Element::Clone))]
        );
    }

    #[test]
    fn both_sides_of_a_repeated_vec_are_checked() {
        assert_eq!(
            check(&in_fn("let _ = vec![x.clone(); serde_json::LEN];")),
            sorted([(2, element(Element::Clone)), (2, krate("serde_json"))])
        );
    }

    #[test]
    fn the_pattern_of_matches_is_checked() {
        assert_eq!(
            check(&in_fn("let _ = matches!(v, serde_json::Value::Null);")),
            sorted([
                (2, element(Element::MatchesMacro)),
                (2, krate("serde_json")),
            ])
        );
    }

    #[test]
    fn the_guard_of_matches_is_checked() {
        assert_eq!(
            check(&in_fn("let _ = matches!(v, Some(x) if x.clone() > 0);")),
            sorted([
                (2, element(Element::MatchesMacro)),
                (2, element(Element::Clone)),
            ])
        );
    }

    #[test]
    fn the_scrutinee_of_matches_is_checked() {
        assert_eq!(
            check(&in_fn("let _ = matches!(v.clone(), A | B);")),
            sorted([
                (2, element(Element::MatchesMacro)),
                (2, element(Element::Clone)),
            ])
        );
    }

    #[test]
    fn arguments_that_do_not_parse_in_the_shape_of_the_macro_make_it_a_macro_call() {
        let cases = [
            "println!(\"{}\" x);",
            "let _ = vec![1; 2; 3];",
            "let _ = matches!(v);",
        ];
        for body in cases {
            let reported = check(&in_fn(body));
            assert!(
                reported.contains(&(2, element(Element::MacroCall))),
                "{body}: {reported:?}"
            );
        }
    }

    #[test]
    fn std_macro_arguments_that_parse_do_not_make_it_a_macro_call() {
        let cases = [
            "println!();",
            "println!(\"{}\", x,);",
            "let _ = vec![];",
            "let _ = vec![1, 2,];",
            "let _ = matches!(v, | A | B,);",
            "let _ = format!(\"{a}\", a = 1,);",
        ];
        for body in cases {
            let reported = check(&in_fn(body));
            assert!(
                !reported.contains(&(2, element(Element::MacroCall))),
                "{body}: {reported:?}"
            );
        }
    }

    #[test]
    fn a_name_imported_by_a_use_in_the_arguments_is_read_through_it() {
        // The `use` is `mem_take` on line 2; the call is `mem_take` on line 3
        // only when the `use` is read.
        assert_eq!(
            check(&in_fn(
                "println!(\"{:?}\", { use std::mem::take;\n        take(v) });"
            )),
            [
                (2, element(Element::MemTake)),
                (3, element(Element::MemTake))
            ]
        );
    }

    #[test]
    fn a_name_brought_in_by_a_use_in_the_arguments_is_not_a_crate() {
        assert_eq!(
            check(&in_fn(
                "println!(\"{}\", { use helpers::json; json::x() });"
            )),
            [(2, krate("helpers"))]
        );
    }

    #[test]
    fn the_arguments_of_a_macro_that_is_not_a_std_macro_are_not_checked() {
        assert_eq!(
            check(&in_fn("m!(\"{}\", x.clone());")),
            [(2, element(Element::MacroCall))]
        );
    }

    #[test]
    fn the_arguments_of_asm_are_not_read() {
        assert_eq!(
            check(&in_fn("unsafe { asm!(\"nop\", x = in(reg) 1); }")),
            sorted([
                (2, element(Element::UnsafeBlock)),
                (2, element(Element::InlineAsm)),
            ])
        );
    }
}
