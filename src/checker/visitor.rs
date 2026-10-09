//! Parses content with syn and finds the syntax elements in it.
//!
//! Detected so far: the `macros` category, plus the elements whose syntax is
//! a macro invocation or a `derive` (`panic`, `inline_asm`, `derive`), since
//! telling them apart from `macro_call` and `custom_derive` happens here.

use std::mem;

use proc_macro2::Span;
use syn::parse::ParseStream;
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::visit::{self, Visit};
use syn::{Attribute, ItemEnum, ItemMacro, ItemStruct, ItemUnion, Macro, Meta, Path, Token};

use crate::checker::finding::{Finding, ParseError};
use crate::profile::element::Element;
use crate::profile::status::ItemKey;

/// Every element in `source`, base elements included, in line order.
///
/// Content that syn cannot parse as a file, including a `derive` whose
/// arguments are not a list of paths, is a `ParseError`.
pub fn detect(source: &str) -> Result<Vec<Finding>, ParseError> {
    let file = syn::parse_file(source).map_err(|error| to_parse_error(source, &error))?;
    let mut detector = Detector::default();
    detector.visit_file(&file);
    if let Some(error) = detector.error {
        return Err(to_parse_error(source, &error));
    }
    let mut findings = detector.findings;
    findings.sort_by_key(|finding| finding.line);
    Ok(findings)
}

fn to_parse_error(source: &str, error: &syn::Error) -> ParseError {
    let span = error.span();
    // syn reports running out of input at the top level with the call-site
    // span, which has no position in the content; the end of the content is
    // where the input ran out. A token error at the very start also has an
    // empty span at 0, so the message tells the two apart.
    let ran_out = error.to_string().starts_with("unexpected end of input");
    let (line, column) = if ran_out && span.byte_range() == (0..0) {
        let line = source.split('\n').count();
        let last = source.rsplit('\n').next().unwrap_or("");
        (line, last.chars().count() + 1)
    } else {
        let start = span.start();
        (start.line, start.column + 1)
    };
    ParseError {
        line,
        column,
        message: error.to_string(),
    }
}

#[derive(Default)]
struct Detector {
    findings: Vec<Finding>,
    /// The first error met while visiting; the whole content is then a
    /// `ParseError`.
    error: Option<syn::Error>,
    /// Whether an unknown single-name attribute here may be a derive helper
    /// (`#[serde(..)]`) rather than an attribute macro: on fields and variants,
    /// where attribute macros cannot go, and on a type with a custom derive.
    helpers_allowed: bool,
}

impl Detector {
    fn push(&mut self, span: Span, element: Element, text: String) {
        self.findings.push(Finding {
            line: span.start().line,
            key: ItemKey::Element(element),
            text,
        });
    }

    fn attribute(&mut self, attr: &Attribute) {
        match effective_metas(&attr.meta) {
            Ok(metas) => {
                for meta in metas {
                    self.meta(&meta);
                }
            }
            Err(error) => {
                self.error.get_or_insert(error);
            }
        }
    }

    /// One attribute as it applies to the item, with `cfg_attr` unwrapped.
    fn meta(&mut self, meta: &Meta) {
        let path = meta.path();
        if path.is_ident("derive") {
            match derived_traits(meta) {
                Ok(traits) => {
                    for path in traits {
                        let element = if is_std_derive(&path) {
                            Element::Derive
                        } else {
                            Element::CustomDerive
                        };
                        self.push(path.span(), element, path_text(&path));
                    }
                }
                Err(error) => {
                    self.error.get_or_insert(error);
                }
            }
            return;
        }
        if let Some(ident) = path.get_ident() {
            let name = ident.to_string();
            if PROC_MACRO_DEFS.contains(&name.as_str()) {
                self.push(meta.span(), Element::ProcMacroDef, name);
                return;
            }
            if BUILT_IN_ATTRIBUTES.contains(&name.as_str()) || self.helpers_allowed {
                return;
            }
        } else if path.leading_colon.is_none()
            && path
                .segments
                .first()
                .is_some_and(|s| TOOLS.contains(&s.ident.to_string().as_str()))
        {
            return;
        }
        self.push(meta.span(), Element::AttributeMacro, path_text(path));
    }

    fn with_helpers_allowed(&mut self, allowed: bool, visit: impl FnOnce(&mut Self)) {
        let outer = mem::replace(&mut self.helpers_allowed, allowed);
        visit(self);
        self.helpers_allowed = outer;
    }
}

impl<'ast> Visit<'ast> for Detector {
    fn visit_macro(&mut self, mac: &'ast Macro) {
        let element = std_name(&mac.path)
            .and_then(|name| std_macro(&name))
            .unwrap_or(Element::MacroCall);
        self.push(
            mac.path.span(),
            element,
            format!("{}!", path_text(&mac.path)),
        );
        visit::visit_macro(self, mac);
    }

    fn visit_item_macro(&mut self, item: &'ast ItemMacro) {
        if !item.mac.path.is_ident("macro_rules") {
            visit::visit_item_macro(self, item);
            return;
        }
        for attr in &item.attrs {
            self.visit_attribute(attr);
        }
        let name = item
            .ident
            .as_ref()
            .map_or_else(String::new, |ident| format!(" {ident}"));
        self.push(
            item.mac.path.span(),
            Element::MacroRules,
            format!("macro_rules!{name}"),
        );
    }

    fn visit_attribute(&mut self, attr: &'ast Attribute) {
        self.attribute(attr);
        visit::visit_attribute(self, attr);
    }

    fn visit_item_struct(&mut self, item: &'ast ItemStruct) {
        let allowed = has_custom_derive(&item.attrs);
        self.with_helpers_allowed(allowed, |v| visit::visit_item_struct(v, item));
    }

    fn visit_item_enum(&mut self, item: &'ast ItemEnum) {
        let allowed = has_custom_derive(&item.attrs);
        self.with_helpers_allowed(allowed, |v| visit::visit_item_enum(v, item));
    }

    fn visit_item_union(&mut self, item: &'ast ItemUnion) {
        let allowed = has_custom_derive(&item.attrs);
        self.with_helpers_allowed(allowed, |v| visit::visit_item_union(v, item));
    }

    fn visit_field(&mut self, field: &'ast syn::Field) {
        self.with_helpers_allowed(true, |v| visit::visit_field(v, field));
    }

    fn visit_variant(&mut self, variant: &'ast syn::Variant) {
        self.with_helpers_allowed(true, |v| visit::visit_variant(v, variant));
    }
}

/// The attributes `meta` stands for: itself, or for `cfg_attr(pred, a, b)`
/// the attributes `a` and `b` (unwrapped in turn). The predicate is not
/// evaluated: the attributes are checked whether or not they apply.
fn effective_metas(meta: &Meta) -> syn::Result<Vec<Meta>> {
    if !meta.path().is_ident("cfg_attr") {
        return Ok(vec![meta.clone()]);
    }
    let attrs = meta.require_list()?.parse_args_with(|input: ParseStream| {
        input.parse::<Meta>()?;
        input.parse::<Token![,]>()?;
        Punctuated::<Meta, Token![,]>::parse_terminated(input)
    })?;
    let mut metas = Vec::new();
    for attr in &attrs {
        metas.extend(effective_metas(attr)?);
    }
    Ok(metas)
}

/// The traits in `derive(A, b::C)`.
fn derived_traits(meta: &Meta) -> syn::Result<Punctuated<Path, Token![,]>> {
    meta.require_list()?
        .parse_args_with(Punctuated::<Path, Token![,]>::parse_terminated)
}

fn is_std_derive(path: &Path) -> bool {
    std_name(path).is_some_and(|name| STD_DERIVES.contains(&name.as_str()))
}

fn has_custom_derive(attrs: &[Attribute]) -> bool {
    attrs
        .iter()
        .filter_map(|attr| effective_metas(&attr.meta).ok())
        .flatten()
        .filter(|meta| meta.path().is_ident("derive"))
        .filter_map(|meta| derived_traits(&meta).ok())
        .flatten()
        .any(|path| !is_std_derive(&path))
}

/// The name `path` gives to something of std: the last segment when the path
/// is a bare name or starts with `std`, `core` or `alloc`. `None` for a path
/// into any other crate or module (`my_crate::vec`), which cannot be std.
fn std_name(path: &Path) -> Option<String> {
    let first = path.segments.first()?;
    let last = path.segments.last()?;
    let bare = path.leading_colon.is_none() && path.segments.len() == 1;
    let in_std = path.segments.len() > 1
        && ["std", "core", "alloc"].contains(&first.ident.to_string().as_str());
    (bare || in_std).then(|| last.ident.to_string())
}

/// The element of a std macro that has one of its own.
fn std_macro(name: &str) -> Option<Element> {
    let element = match name {
        "print" | "println" | "eprint" | "eprintln" => Element::PrintMacro,
        "format" | "write" | "writeln" => Element::FormatMacro,
        "vec" => Element::VecMacro,
        "assert" | "assert_eq" | "assert_ne" | "debug_assert" | "debug_assert_eq"
        | "debug_assert_ne" => Element::AssertMacro,
        "dbg" => Element::DbgMacro,
        "matches" => Element::MatchesMacro,
        "panic" | "unreachable" | "todo" | "unimplemented" => Element::Panic,
        "asm" => Element::InlineAsm,
        _ => return None,
    };
    Some(element)
}

fn path_text(path: &Path) -> String {
    let segments: Vec<String> = path
        .segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect();
    let leading = if path.leading_colon.is_some() {
        "::"
    } else {
        ""
    };
    format!("{leading}{}", segments.join("::"))
}

/// The traits `#[derive]` can implement from std.
const STD_DERIVES: &[&str] = &[
    "Debug",
    "Clone",
    "Copy",
    "PartialEq",
    "Eq",
    "PartialOrd",
    "Ord",
    "Hash",
    "Default",
];

/// Attributes that define a procedural macro.
const PROC_MACRO_DEFS: &[&str] = &["proc_macro", "proc_macro_derive", "proc_macro_attribute"];

/// Attributes built into the language (the Reference's built-in attributes
/// index), which run no user code. `unsafe` is the `#[unsafe(no_mangle)]`
/// wrapper. `derive` and the proc-macro attributes are handled separately.
const BUILT_IN_ATTRIBUTES: &[&str] = &[
    "cfg",
    "cfg_attr",
    "test",
    "ignore",
    "should_panic",
    "automatically_derived",
    "macro_export",
    "macro_use",
    "allow",
    "expect",
    "warn",
    "deny",
    "forbid",
    "deprecated",
    "must_use",
    "link",
    "link_name",
    "link_ordinal",
    "no_link",
    "repr",
    "crate_type",
    "no_main",
    "export_name",
    "link_section",
    "no_mangle",
    "used",
    "crate_name",
    "inline",
    "cold",
    "naked",
    "no_builtins",
    "target_feature",
    "track_caller",
    "instruction_set",
    "doc",
    "no_std",
    "no_implicit_prelude",
    "path",
    "recursion_limit",
    "type_length_limit",
    "panic_handler",
    "global_allocator",
    "windows_subsystem",
    "feature",
    "non_exhaustive",
    "debugger_visualizer",
    "collapse_debuginfo",
    "unsafe",
];

/// Namespaces of tool attributes (`#[rustfmt::skip]`, `#[clippy::..]`), which
/// are not macros.
const TOOLS: &[&str] = &["rustfmt", "clippy", "rustdoc", "diagnostic"];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::element::Element;
    use crate::profile::status::ItemKey;

    fn elements(source: &str) -> Vec<(usize, Element, String)> {
        detect(source)
            .unwrap()
            .into_iter()
            .map(|f| match f.key {
                ItemKey::Element(element) => (f.line, element, f.text),
                ItemKey::Crate(name) => panic!("unexpected crate {name}"),
            })
            .collect()
    }

    fn only(source: &str) -> Vec<Element> {
        elements(source).into_iter().map(|(_, e, _)| e).collect()
    }

    // ParseError

    #[test]
    fn content_that_does_not_parse_is_a_parse_error_with_line_and_column() {
        let error = detect("fn main() {\n    let x = ;\n}\n").unwrap_err();
        assert_eq!((error.line, error.column), (2, 13));
        assert!(!error.message.is_empty());
    }

    #[test]
    fn input_that_runs_out_inside_braces_is_a_parse_error_at_the_closing_brace() {
        let error = detect("fn f() {\n    let x = 1 +\n}\nfn g() {}\n").unwrap_err();
        assert_eq!((error.line, error.column), (3, 1));
    }

    #[test]
    fn a_token_error_at_the_very_start_is_a_parse_error_at_the_start() {
        let error = detect("}\nfn f() {}\n").unwrap_err();
        assert_eq!((error.line, error.column), (1, 1));
    }

    #[test]
    fn an_unclosed_delimiter_is_a_parse_error_at_the_delimiter() {
        let error = detect("fn main() {\n    let x = 1;\n").unwrap_err();
        assert_eq!((error.line, error.column), (1, 11));
    }

    #[test]
    fn content_that_ends_too_early_is_a_parse_error_at_the_end_of_the_content() {
        let error = detect("fn f() {}\n\nfn").unwrap_err();
        assert_eq!((error.line, error.column), (3, 3));
    }

    #[test]
    fn a_derive_whose_arguments_do_not_parse_is_a_parse_error() {
        let error = detect("#[derive(Debug +)]\nstruct S;\n").unwrap_err();
        assert_eq!(error.line, 1);
    }

    // Macro invocations

    #[test]
    fn a_macro_without_an_element_of_its_own_is_macro_call_with_its_name() {
        assert_eq!(
            elements("fn f() {\n    tokio::select! {}\n}\n"),
            [(2, Element::MacroCall, "tokio::select!".to_owned())]
        );
    }

    #[test]
    fn a_std_macro_without_an_element_of_its_own_is_macro_call() {
        assert_eq!(
            elements("const S: &str = concat!(\"a\", \"b\");\n"),
            [(1, Element::MacroCall, "concat!".to_owned())]
        );
    }

    #[test]
    fn std_macros_with_an_element_of_their_own_are_that_element() {
        let cases = [
            ("print", Element::PrintMacro),
            ("println", Element::PrintMacro),
            ("eprint", Element::PrintMacro),
            ("eprintln", Element::PrintMacro),
            ("format", Element::FormatMacro),
            ("write", Element::FormatMacro),
            ("writeln", Element::FormatMacro),
            ("vec", Element::VecMacro),
            ("assert", Element::AssertMacro),
            ("assert_eq", Element::AssertMacro),
            ("assert_ne", Element::AssertMacro),
            ("debug_assert", Element::AssertMacro),
            ("debug_assert_eq", Element::AssertMacro),
            ("debug_assert_ne", Element::AssertMacro),
            ("dbg", Element::DbgMacro),
            ("matches", Element::MatchesMacro),
            ("panic", Element::Panic),
            ("unreachable", Element::Panic),
            ("todo", Element::Panic),
            ("unimplemented", Element::Panic),
            ("asm", Element::InlineAsm),
        ];
        for (name, element) in cases {
            let source = format!("fn f() {{\n    {name}!();\n}}\n");
            assert_eq!(
                elements(&source),
                [(2, element, format!("{name}!"))],
                "{name}"
            );
        }
    }

    #[test]
    fn a_std_macro_named_through_std_core_or_alloc_is_still_that_element() {
        let cases = [
            ("std::println", Element::PrintMacro),
            ("::std::println", Element::PrintMacro),
            ("core::panic", Element::Panic),
            ("alloc::vec", Element::VecMacro),
        ];
        for (path, element) in cases {
            let source = format!("fn f() {{ {path}!(); }}");
            assert_eq!(only(&source), [element], "{path}");
        }
    }

    #[test]
    fn a_macro_of_another_crate_with_a_std_name_is_macro_call() {
        assert_eq!(
            elements("fn f() { my_crate::vec![1]; }"),
            [(1, Element::MacroCall, "my_crate::vec!".to_owned())]
        );
    }

    #[test]
    fn macro_invocations_are_found_in_every_position() {
        let source = "\
my_item!();
fn f(my_pat!(): u8) -> my_type!() {
    let x = my_expr!();
    my_stmt!();
}
struct S;
impl S {
    my_impl_item!();
}
trait T {
    my_trait_item!();
}
#[doc = my_attr_value!()]
fn g() {}
";
        let found: Vec<(usize, String)> = elements(source)
            .into_iter()
            .map(|(line, element, text)| {
                assert_eq!(element, Element::MacroCall, "{text}");
                (line, text)
            })
            .collect();
        assert_eq!(
            found,
            [
                (1, "my_item!".to_owned()),
                (2, "my_pat!".to_owned()),
                (2, "my_type!".to_owned()),
                (3, "my_expr!".to_owned()),
                (4, "my_stmt!".to_owned()),
                (8, "my_impl_item!".to_owned()),
                (11, "my_trait_item!".to_owned()),
                (13, "my_attr_value!".to_owned()),
            ]
        );
    }

    #[test]
    fn a_macro_rules_definition_is_macro_rules_and_not_a_macro_call() {
        assert_eq!(
            elements("macro_rules! twice {\n    ($e:expr) => { $e; $e };\n}\n"),
            [(1, Element::MacroRules, "macro_rules! twice".to_owned())]
        );
    }

    #[test]
    fn a_macro_path_with_a_leading_double_colon_names_a_crate_and_is_macro_call() {
        assert_eq!(
            elements("fn f() { ::vec![1]; }"),
            [(1, Element::MacroCall, "::vec!".to_owned())]
        );
    }

    #[test]
    fn attributes_on_a_macro_rules_definition_are_still_checked() {
        assert_eq!(
            elements("#[my_attr]\nmacro_rules! m {\n    () => {};\n}\n"),
            [
                (1, Element::AttributeMacro, "my_attr".to_owned()),
                (2, Element::MacroRules, "macro_rules! m".to_owned()),
            ]
        );
    }

    #[test]
    fn findings_are_in_line_order_even_where_syn_visits_out_of_order() {
        // syn visits a signature's generics, `where` clause included, before
        // its return type.
        let source = "fn f()\n-> ret!()\nwhere\n    bound!(): Copy,\n{}\n";
        let lines: Vec<usize> = elements(source).into_iter().map(|(l, _, _)| l).collect();
        assert_eq!(lines, [2, 4]);
    }

    // Attributes

    #[test]
    fn a_derive_of_std_traits_is_derive_and_of_other_traits_is_custom_derive() {
        assert_eq!(
            elements(
                "#[derive(Debug, std::hash::Hash, Serialize, serde::Deserialize)]\nstruct S;\n"
            ),
            [
                (1, Element::Derive, "Debug".to_owned()),
                (1, Element::Derive, "std::hash::Hash".to_owned()),
                (1, Element::CustomDerive, "Serialize".to_owned()),
                (1, Element::CustomDerive, "serde::Deserialize".to_owned()),
            ]
        );
    }

    #[test]
    fn every_derivable_std_trait_is_derive() {
        let source = "#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]\nstruct S;\n";
        assert_eq!(only(source), [Element::Derive; 9]);
    }

    #[test]
    fn an_attribute_that_is_not_built_in_is_attribute_macro() {
        assert_eq!(
            elements("#[tokio::main]\nasync fn main() {}\n\n#[my_attr]\nfn f() {}\n"),
            [
                (1, Element::AttributeMacro, "tokio::main".to_owned()),
                (4, Element::AttributeMacro, "my_attr".to_owned()),
            ]
        );
    }

    #[test]
    fn built_in_and_tool_attributes_are_not_attribute_macros() {
        let source = "\
#![allow(dead_code)]
/// doc comment
#[cfg(test)]
#[inline]
#[must_use]
#[rustfmt::skip]
#[clippy::cognitive_complexity = \"100\"]
#[unsafe(no_mangle)]
#[test]
#[repr(C)]
#[non_exhaustive]
fn f() {}
";
        assert_eq!(only(source), []);
    }

    #[test]
    fn helper_attributes_of_a_custom_derive_are_not_attribute_macros() {
        let source = "\
#[derive(Serialize)]
#[serde(rename_all = \"camelCase\")]
enum E {
    #[serde(rename = \"a\")]
    A {
        #[serde(default)]
        x: u8,
    },
}
";
        assert_eq!(
            elements(source),
            [(1, Element::CustomDerive, "Serialize".to_owned())]
        );
    }

    #[test]
    fn an_unknown_attribute_on_a_type_with_only_std_derives_is_attribute_macro() {
        assert_eq!(
            elements("#[derive(Debug)]\n#[my_attr]\nstruct S;\n"),
            [
                (1, Element::Derive, "Debug".to_owned()),
                (2, Element::AttributeMacro, "my_attr".to_owned()),
            ]
        );
    }

    #[test]
    fn unknown_attributes_on_fields_are_helpers_even_without_a_custom_derive() {
        assert_eq!(
            elements("#[my_attr]\nstruct S {\n    #[helper]\n    x: u8,\n}\n"),
            [(1, Element::AttributeMacro, "my_attr".to_owned())]
        );
    }

    #[test]
    fn a_tool_namespace_after_a_leading_double_colon_is_a_crate_and_attribute_macro() {
        assert_eq!(
            elements("#[::clippy::thing]\nfn f() {}\n"),
            [(1, Element::AttributeMacro, "::clippy::thing".to_owned())]
        );
    }

    #[test]
    fn derives_and_attribute_macros_inside_cfg_attr_are_found() {
        let source = "\
#[cfg_attr(feature = \"serde\", derive(Debug, Serialize))]
struct S;
#[cfg_attr(all(), cfg_attr(test, tokio::main), inline)]
async fn main() {}
";
        assert_eq!(
            elements(source),
            [
                (1, Element::Derive, "Debug".to_owned()),
                (1, Element::CustomDerive, "Serialize".to_owned()),
                (3, Element::AttributeMacro, "tokio::main".to_owned()),
            ]
        );
    }

    #[test]
    fn a_custom_derive_inside_cfg_attr_allows_its_helper_attributes() {
        let source =
            "#[cfg_attr(feature = \"serde\", derive(Serialize))]\n#[serde(default)]\nstruct S;\n";
        assert_eq!(
            elements(source),
            [(1, Element::CustomDerive, "Serialize".to_owned())]
        );
    }

    #[test]
    fn a_cfg_attr_whose_arguments_do_not_parse_is_a_parse_error() {
        let error = detect("#[cfg_attr(test derive(Debug))]\nstruct S;\n").unwrap_err();
        assert_eq!(error.line, 1);
    }

    #[test]
    fn a_proc_macro_definition_is_proc_macro_def() {
        let source = "\
#[proc_macro]
pub fn a(input: TokenStream) -> TokenStream { input }
#[proc_macro_derive(Thing)]
pub fn b(input: TokenStream) -> TokenStream { input }
#[proc_macro_attribute]
pub fn c(attr: TokenStream, item: TokenStream) -> TokenStream { item }
";
        assert_eq!(
            elements(source),
            [
                (1, Element::ProcMacroDef, "proc_macro".to_owned()),
                (3, Element::ProcMacroDef, "proc_macro_derive".to_owned()),
                (5, Element::ProcMacroDef, "proc_macro_attribute".to_owned()),
            ]
        );
    }

    #[test]
    fn plain_rust_without_elements_yields_nothing() {
        assert_eq!(
            only(
                "struct S { x: u8 }\nfn f(s: S) -> u8 {\n    match s.x {\n        0 => 1,\n        n => n,\n    }\n}\n"
            ),
            []
        );
    }
}
