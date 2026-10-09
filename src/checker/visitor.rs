//! Parses content with syn and finds the syntax elements in it.
//!
//! Detected so far: the `macros` and `borrowing` categories, plus the
//! elements whose syntax is a macro invocation or a `derive` (`panic`,
//! `inline_asm`, `derive`), since telling them apart from `macro_call` and
//! `custom_derive` happens here.
//!
//! An element built from other elements is reported together with them, one
//! finding per element (PRD §11): `&[u8]` is both `shared_ref` and `slice`.

mod names;

use std::mem;

use proc_macro2::Span;
use syn::parse::ParseStream;
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::visit::{self, Visit};
use syn::{
    AngleBracketedGenericArguments, AssocType, Attribute, BoundLifetimes, ConstParam, Constraint,
    Expr, ExprBreak, ExprClosure, ExprContinue, ExprForLoop, ExprIndex, ExprMethodCall, ExprRange,
    ExprReference, ExprTry, ExprUnary, GenericArgument, Generics, ImplItem, ImplItemConst,
    ImplItemType, Item, ItemEnum, ItemImpl, ItemMacro, ItemStruct, ItemTrait, ItemTraitAlias,
    ItemUnion, ItemUse, Label, Lifetime, LifetimeParam, Macro, Meta, PatIdent, PatReference, Path,
    PathArguments, PredicateLifetime, PredicateType, QSelf, Receiver, ReceiverKind, Token,
    TraitItem, TraitItemConst, TraitItemType, Type, TypeFnPtr, TypeImplTrait, TypeParam,
    TypeParamBound, TypePath, TypeReference, TypeSlice, TypeTraitObject, UnOp, UseTree,
    WhereClause,
};

use crate::checker::finding::{Finding, ParseError};
use crate::profile::element::Element;
use crate::profile::status::ItemKey;

/// Every element in `source`, base elements included, in line order.
///
/// Content that syn cannot parse as a file, including a `derive` whose
/// arguments are not a list of paths, is a `ParseError`.
pub fn detect(source: &str) -> Result<Vec<Finding>, ParseError> {
    let file = syn::parse_file(source).map_err(|error| to_parse_error(source, &error))?;
    let mut detector = Detector {
        source,
        ..Detector::default()
    };
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
struct Detector<'s> {
    source: &'s str,
    findings: Vec<Finding>,
    /// The first error met while visiting; the whole content is then a
    /// `ParseError`.
    error: Option<syn::Error>,
    /// Whether an unknown single-name attribute here may be a derive helper
    /// (`#[serde(..)]`) rather than an attribute macro: on fields and variants,
    /// where attribute macros cannot go, and on a type with a custom derive.
    helpers_allowed: bool,
    /// The type parameters in scope, so that `T::Item` reads as an associated
    /// type rather than a path into a module `T`.
    type_params: Vec<String>,
    /// What a trait in the bound list being visited is.
    bound_role: BoundRole,
}

/// What a trait in a list of bounds stands for: a bound (`T: Trait`), a
/// supertrait (`trait A: B`), or a part of `impl Trait` / `dyn Trait`, which
/// are elements of their own.
#[derive(Default, Clone, Copy)]
enum BoundRole {
    #[default]
    Bound,
    Supertrait,
    Plain,
}

impl Detector<'_> {
    /// Reports `element` at `span`, with the source `span` covers as the text.
    fn mark(&mut self, span: Span, element: Element) {
        let text = self.snippet(span);
        self.push(span, element, text);
    }

    /// The source `span` covers, cut after its first line.
    fn snippet(&self, span: Span) -> String {
        self.text(span.byte_range())
    }

    fn text(&self, range: std::ops::Range<usize>) -> String {
        let text = self.source.get(range).unwrap_or("").trim();
        match text.split_once('\n') {
            Some((first, _)) => format!("{} ..", first.trim_end()),
            None => text.to_owned(),
        }
    }

    /// Reports `element` on the line where `span` starts. `span` is the token
    /// that makes the element (`?`, a method name, `async`, `impl`), not the
    /// expression around it: `check_file` reports a finding only when its line
    /// changed, so a `?` added to the third line of an existing chain must be
    /// on that line.
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

    /// Reports `element` on the line of `token`, with the source from `token`
    /// to the end of `node` as the text, so that attributes before an item are
    /// left out.
    fn mark_from(&mut self, token: Span, node: Span, element: Element) {
        let start = token.byte_range().start;
        let end = node.byte_range().end.max(token.byte_range().end);
        let text = self.text(start..end);
        self.push(token, element, text);
    }

    /// `type Item = ..;` in an impl or `type Item;` in a trait. With
    /// parameters of its own (`type Item<'a>`) it is also generic.
    fn associated_type(&mut self, token: Span, node: Span, generics: &Generics) {
        self.mark_from(token, node, Element::AssociatedType);
        if !generics.params.is_empty() {
            self.mark_from(token, node, Element::GenericAssociatedType);
        }
    }

    /// A lifetime as a bound (`T: 'a`, `'a: 'b`): `lifetime_bound`, and
    /// `static_lifetime` too for `'static`.
    fn lifetime_bound(&mut self, lifetime: &Lifetime) {
        self.mark(lifetime.span(), Element::LifetimeBound);
        if lifetime.ident == "static" {
            self.mark(lifetime.span(), Element::StaticLifetime);
        }
    }

    fn with_bound_role(&mut self, role: BoundRole, visit: impl FnOnce(&mut Self)) {
        let outer = mem::replace(&mut self.bound_role, role);
        visit(self);
        self.bound_role = outer;
    }

    /// Visits an item, and forgets the type parameters it declared after it.
    fn scoped(&mut self, visit: impl FnOnce(&mut Self)) {
        let outer = self.type_params.len();
        visit(self);
        self.type_params.truncate(outer);
    }

    fn with_helpers_allowed(&mut self, allowed: bool, visit: impl FnOnce(&mut Self)) {
        let outer = mem::replace(&mut self.helpers_allowed, allowed);
        visit(self);
        self.helpers_allowed = outer;
    }
}

impl<'ast> Visit<'ast> for Detector<'_> {
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

    // borrowing

    fn visit_type_reference(&mut self, ty: &'ast TypeReference) {
        self.mark(ty.span(), reference(ty.mutability.is_some()));
        visit::visit_type_reference(self, ty);
    }

    fn visit_expr_reference(&mut self, expr: &'ast ExprReference) {
        self.mark(expr.span(), reference(expr.mutability.is_some()));
        visit::visit_expr_reference(self, expr);
    }

    fn visit_pat_reference(&mut self, pat: &'ast PatReference) {
        self.mark(pat.span(), reference(pat.mutability.is_some()));
        visit::visit_pat_reference(self, pat);
    }

    fn visit_receiver(&mut self, receiver: &'ast Receiver) {
        if let ReceiverKind::Reference(_, _, mutability) = &receiver.kind {
            self.mark(receiver.span(), reference(mutability.is_some()));
        }
        visit::visit_receiver(self, receiver);
    }

    fn visit_expr_unary(&mut self, expr: &'ast ExprUnary) {
        if let UnOp::Deref(_) = expr.op {
            self.mark(expr.span(), Element::Deref);
        }
        visit::visit_expr_unary(self, expr);
    }

    fn visit_pat_ident(&mut self, pat: &'ast PatIdent) {
        if pat.by_ref.is_some() {
            self.mark(pat.span(), Element::RefPattern);
        }
        // syn reads `None` in a pattern as a binding named `None`.
        if pat.ident == "None" && pat.by_ref.is_none() && pat.subpat.is_none() {
            self.mark(pat.span(), Element::Option);
        }
        visit::visit_pat_ident(self, pat);
    }

    // error_handling

    fn visit_expr_try(&mut self, expr: &'ast ExprTry) {
        self.push(
            expr.question_token.span(),
            Element::QuestionMark,
            "?".to_owned(),
        );
        visit::visit_expr_try(self, expr);
    }

    fn visit_type_slice(&mut self, ty: &'ast TypeSlice) {
        self.mark(ty.span(), Element::Slice);
        visit::visit_type_slice(self, ty);
    }

    fn visit_expr_index(&mut self, expr: &'ast ExprIndex) {
        if let Expr::Range(_) = &*expr.index {
            // On the line of `[`: the indexed expression may start lines above.
            let text = self.snippet(expr.span());
            self.push(expr.bracket_token.span.open(), Element::Slice, text);
        }
        visit::visit_expr_index(self, expr);
    }

    // traits_generics

    fn visit_item(&mut self, item: &'ast Item) {
        self.scoped(|v| visit::visit_item(v, item));
    }

    fn visit_impl_item(&mut self, item: &'ast ImplItem) {
        self.scoped(|v| visit::visit_impl_item(v, item));
    }

    fn visit_trait_item(&mut self, item: &'ast TraitItem) {
        self.scoped(|v| visit::visit_trait_item(v, item));
    }

    fn visit_item_trait(&mut self, item: &'ast ItemTrait) {
        self.mark_from(item.trait_token.span(), item.span(), Element::TraitDef);
        self.with_bound_role(BoundRole::Supertrait, |v| visit::visit_item_trait(v, item));
    }

    fn visit_item_trait_alias(&mut self, item: &'ast ItemTraitAlias) {
        self.mark_from(item.trait_token.span(), item.span(), Element::TraitDef);
        self.with_bound_role(BoundRole::Bound, |v| visit::visit_item_trait_alias(v, item));
    }

    fn visit_item_impl(&mut self, item: &'ast ItemImpl) {
        if let Some((path, _)) = &item.trait_ {
            self.mark_from(item.impl_token.span(), item.span(), Element::TraitImpl);
            let name = path.segments.last().map(|s| s.ident.to_string());
            if let Some(element) = name.as_deref().and_then(implemented_trait) {
                self.mark_from(item.impl_token.span(), item.span(), element);
            }
        }
        visit::visit_item_impl(self, item);
    }

    fn visit_type_param(&mut self, param: &'ast TypeParam) {
        self.type_params.push(param.ident.to_string());
        self.mark(param.span(), Element::GenericParam);
        self.with_bound_role(BoundRole::Bound, |v| visit::visit_type_param(v, param));
    }

    fn visit_const_param(&mut self, param: &'ast ConstParam) {
        self.mark_from(
            param.const_token.span(),
            param.span(),
            Element::ConstGeneric,
        );
        visit::visit_const_param(self, param);
    }

    fn visit_where_clause(&mut self, clause: &'ast WhereClause) {
        if !clause.predicates.is_empty() {
            self.mark_from(
                clause.where_token.span(),
                clause.span(),
                Element::WhereClause,
            );
        }
        visit::visit_where_clause(self, clause);
    }

    fn visit_predicate_type(&mut self, predicate: &'ast PredicateType) {
        self.with_bound_role(BoundRole::Bound, |v| {
            visit::visit_predicate_type(v, predicate)
        });
    }

    fn visit_type_param_bound(&mut self, bound: &'ast TypeParamBound) {
        match bound {
            TypeParamBound::Trait(trait_bound) => match self.bound_role {
                BoundRole::Bound => self.mark(trait_bound.span(), Element::TraitBound),
                BoundRole::Supertrait => self.mark(trait_bound.span(), Element::Supertrait),
                BoundRole::Plain => {}
            },
            // `T: 'a`, `impl Trait + 'a`: a bound, not a lifetime on a type.
            TypeParamBound::Lifetime(lifetime) => {
                self.lifetime_bound(lifetime);
                return;
            }
            _ => {}
        }
        visit::visit_type_param_bound(self, bound);
    }

    fn visit_type_impl_trait(&mut self, ty: &'ast TypeImplTrait) {
        self.mark(ty.span(), Element::ImplTrait);
        self.with_bound_role(BoundRole::Plain, |v| visit::visit_type_impl_trait(v, ty));
    }

    fn visit_type_trait_object(&mut self, ty: &'ast TypeTraitObject) {
        self.mark(ty.span(), Element::DynTrait);
        self.with_bound_role(BoundRole::Plain, |v| visit::visit_type_trait_object(v, ty));
    }

    fn visit_trait_item_type(&mut self, item: &'ast TraitItemType) {
        self.associated_type(item.type_token.span(), item.span(), &item.generics);
        self.with_bound_role(BoundRole::Bound, |v| visit::visit_trait_item_type(v, item));
    }

    fn visit_impl_item_type(&mut self, item: &'ast ImplItemType) {
        self.associated_type(item.type_token.span(), item.span(), &item.generics);
        visit::visit_impl_item_type(self, item);
    }

    fn visit_assoc_type(&mut self, assoc: &'ast AssocType) {
        self.mark(assoc.span(), Element::AssociatedType);
        visit::visit_assoc_type(self, assoc);
    }

    fn visit_constraint(&mut self, constraint: &'ast Constraint) {
        self.mark(constraint.span(), Element::AssociatedType);
        self.with_bound_role(BoundRole::Bound, |v| visit::visit_constraint(v, constraint));
    }

    fn visit_type_path(&mut self, ty: &'ast TypePath) {
        // `Self::Item`, `T::Item` and `<T as Trait>::Item` in a type are
        // associated types. A path into a module (`io::Error`) is not.
        let first = ty.path.segments.first().map(|s| s.ident.to_string());
        let from_param = ty.path.segments.len() > 1
            && first.is_some_and(|first| first == "Self" || self.type_params.contains(&first));
        if ty.qself.is_some() || from_param {
            self.mark(ty.span(), Element::AssociatedType);
        }
        if is_box_dyn_error(ty) {
            self.mark(ty.span(), Element::BoxDynError);
        }
        visit::visit_type_path(self, ty);
    }

    fn visit_qself(&mut self, qself: &'ast QSelf) {
        self.mark(qself.span(), Element::QualifiedPath);
        visit::visit_qself(self, qself);
    }

    fn visit_angle_bracketed_generic_arguments(
        &mut self,
        args: &'ast AngleBracketedGenericArguments,
    ) {
        if let Some(colon2) = &args.colon2_token {
            self.mark_from(colon2.span(), args.span(), Element::Turbofish);
        }
        visit::visit_angle_bracketed_generic_arguments(self, args);
    }

    fn visit_impl_item_const(&mut self, item: &'ast ImplItemConst) {
        self.mark_from(
            item.const_token.span(),
            item.span(),
            Element::AssociatedConst,
        );
        visit::visit_impl_item_const(self, item);
    }

    fn visit_trait_item_const(&mut self, item: &'ast TraitItemConst) {
        self.mark_from(
            item.const_token.span(),
            item.span(),
            Element::AssociatedConst,
        );
        visit::visit_trait_item_const(self, item);
    }

    // lifetimes

    fn visit_lifetime(&mut self, lifetime: &'ast Lifetime) {
        let element = match lifetime.ident.to_string().as_str() {
            "static" => Element::StaticLifetime,
            "_" => Element::AnonymousLifetime,
            _ => Element::LifetimeAnnotation,
        };
        self.mark(lifetime.span(), element);
    }

    fn visit_lifetime_param(&mut self, param: &'ast LifetimeParam) {
        for attr in &param.attrs {
            self.visit_attribute(attr);
        }
        self.mark(param.lifetime.span(), Element::LifetimeParam);
        for bound in &param.bounds {
            self.lifetime_bound(bound);
        }
    }

    fn visit_predicate_lifetime(&mut self, predicate: &'ast PredicateLifetime) {
        for bound in &predicate.bounds {
            self.lifetime_bound(bound);
        }
    }

    fn visit_bound_lifetimes(&mut self, lifetimes: &'ast BoundLifetimes) {
        // The lifetimes `for<'a>` declares are part of this element, not
        // parameters of an item.
        self.mark(lifetimes.span(), Element::HigherRankedBound);
    }

    // A label (`'outer: loop`, `break 'outer`) is written like a lifetime
    // but is not one.

    fn visit_label(&mut self, _label: &'ast Label) {}

    fn visit_expr_break(&mut self, expr: &'ast ExprBreak) {
        for attr in &expr.attrs {
            self.visit_attribute(attr);
        }
        if let Some(value) = &expr.expr {
            self.visit_expr(value);
        }
    }

    fn visit_expr_continue(&mut self, expr: &'ast ExprContinue) {
        for attr in &expr.attrs {
            self.visit_attribute(attr);
        }
    }

    // iterators_closures

    fn visit_expr_for_loop(&mut self, expr: &'ast ExprForLoop) {
        self.mark_from(expr.for_token.span(), expr.span(), Element::ForLoop);
        visit::visit_expr_for_loop(self, expr);
    }

    fn visit_expr_range(&mut self, expr: &'ast ExprRange) {
        // On the line of `..`: in a pattern or an expression the start may be
        // lines above.
        let text = self.snippet(expr.span());
        self.push(expr.limits.span(), Element::Range, text);
        visit::visit_expr_range(self, expr);
    }

    fn visit_expr_closure(&mut self, expr: &'ast ExprClosure) {
        let head = expr
            .asyncness
            .map(|t| t.span())
            .or(expr.capture.map(|t| t.span()))
            .unwrap_or(expr.inputs_begin.span());
        self.mark_from(head, expr.span(), Element::Closure);
        if let Some(capture) = &expr.capture {
            self.mark_from(capture.span(), expr.span(), Element::MoveClosure);
        }
        visit::visit_expr_closure(self, expr);
    }

    fn visit_type_fn_ptr(&mut self, ty: &'ast TypeFnPtr) {
        self.mark_from(ty.fn_token.span(), ty.span(), Element::FnPointer);
        visit::visit_type_fn_ptr(self, ty);
    }

    // Names: paths, imports and method calls

    fn visit_path(&mut self, path: &'ast Path) {
        let segments: Vec<String> = path.segments.iter().map(|s| s.ident.to_string()).collect();
        let segments: Vec<&str> = segments.iter().map(String::as_str).collect();
        for element in names::path_elements(&segments) {
            self.push(path.span(), element, path_text(path));
        }
        visit::visit_path(self, path);
    }

    fn visit_item_use(&mut self, item: &'ast ItemUse) {
        let mut imports = Vec::new();
        imported_paths(&item.tree, &mut Vec::new(), &mut imports);
        for (segments, span) in imports {
            let segments: Vec<&str> = segments.iter().map(String::as_str).collect();
            for element in names::path_elements(&segments) {
                self.push(span, element, segments.join("::"));
            }
        }
        visit::visit_item_use(self, item);
    }

    fn visit_expr_method_call(&mut self, call: &'ast ExprMethodCall) {
        let name = call.method.to_string();
        let text = if call.args.is_empty() {
            format!(".{name}()")
        } else {
            format!(".{name}(..)")
        };
        let receiver = names::receiver_kind(&call.receiver);
        for element in names::method_elements(&name, call.args.len(), receiver) {
            self.push(call.method.span(), element, text.clone());
        }
        visit::visit_expr_method_call(self, call);
    }
}

/// Every path a `use` tree imports, as its segments and the span of its last
/// name. `self` in a group stands for the path before it, and a glob imports
/// no name.
fn imported_paths(tree: &UseTree, prefix: &mut Vec<String>, out: &mut Vec<(Vec<String>, Span)>) {
    let mut leaf = |prefix: &[String], ident: &syn::Ident| {
        let mut segments = prefix.to_vec();
        if ident != "self" {
            segments.push(ident.to_string());
        }
        out.push((segments, ident.span()));
    };
    match tree {
        UseTree::Path(path) => {
            prefix.push(path.ident.to_string());
            imported_paths(&path.tree, prefix, out);
            prefix.pop();
        }
        UseTree::Name(name) => leaf(prefix, &name.ident),
        UseTree::Rename(rename) => leaf(prefix, &rename.ident),
        UseTree::Glob(_) => {}
        UseTree::Group(group) => {
            for tree in &group.items {
                imported_paths(tree, prefix, out);
            }
        }
    }
}

/// `Box<dyn Error>`, also with more bounds (`Box<dyn Error + Send + Sync>`)
/// or a path to the trait (`Box<dyn std::error::Error>`).
fn is_box_dyn_error(ty: &TypePath) -> bool {
    let Some(last) = ty.path.segments.last() else {
        return false;
    };
    let PathArguments::AngleBracketed(args) = &last.arguments else {
        return false;
    };
    last.ident == "Box"
        && args.args.iter().any(|arg| {
            let GenericArgument::Type(Type::TraitObject(object)) = arg else {
                return false;
            };
            object.bounds.iter().any(|bound| {
                matches!(bound, TypeParamBound::Trait(t)
                    if t.path.segments.last().is_some_and(|s| s.ident == "Error"))
            })
        })
}

/// The element of implementing a trait, by the trait's last name, besides
/// `trait_impl`.
fn implemented_trait(name: &str) -> Option<Element> {
    let element = match name {
        "Deref" | "DerefMut" => Element::ImplDeref,
        "Drop" => Element::ImplDrop,
        "Error" => Element::ImplError,
        "Iterator" | "IntoIterator" => Element::ImplIterator,
        _ => return None,
    };
    Some(element)
}

/// `&` or `&mut`, in a type, an expression, a pattern or a receiver.
fn reference(mutable: bool) -> Element {
    if mutable {
        Element::MutRef
    } else {
        Element::SharedRef
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
            elements("fn f() { concat!(\"a\", \"b\"); }\n"),
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
        // `impl S` and `trait T` are only there to hold the macros; the
        // `trait_def` it yields is not what this test is about.
        let found: Vec<(usize, String)> = elements(source)
            .into_iter()
            .filter(|(_, element, _)| *element != Element::TraitDef)
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
        // its return type: `where_clause` (3) and `trait_bound` and `bound!`
        // (4) are visited before `ret!` (2).
        let source = "fn f()\n-> ret!()\nwhere\n    bound!(): Copy,\n{}\n";
        let lines: Vec<usize> = elements(source).into_iter().map(|(l, _, _)| l).collect();
        assert_eq!(lines, [2, 3, 4, 4]);
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

    // borrowing

    #[test]
    fn references_are_shared_ref_or_mut_ref_in_types_expressions_patterns_and_receivers() {
        let source = "\
impl S {
    fn f(&mut self, a: &u8) {
        let &b = a;
        g(&mut 1);
    }
}
";
        assert_eq!(
            elements(source),
            [
                (2, Element::MutRef, "&mut self".to_owned()),
                (2, Element::SharedRef, "&u8".to_owned()),
                (3, Element::SharedRef, "&b".to_owned()),
                (4, Element::MutRef, "&mut 1".to_owned()),
            ]
        );
    }

    #[test]
    fn an_index_by_a_range_is_slice_and_an_index_by_a_number_is_not() {
        let found = only("fn f(v: Vec<u8>) -> u8 {\n    v[1..3][0]\n}\n");
        let slices = found.iter().filter(|&&e| e == Element::Slice).count();
        assert_eq!(slices, 1);
    }

    #[test]
    fn a_finding_on_several_lines_has_its_first_line_as_the_text() {
        let found = elements("fn f(v: Vec<u8>) -> u8 {\n    *v\n        .first()\n}\n");
        assert_eq!(found, [(2, Element::Deref, "*v ..".to_owned())]);
    }

    #[test]
    fn an_index_by_a_range_is_on_the_line_of_its_bracket() {
        let found = elements("fn f(v: Vec<u8>) -> usize {\n    v\n        [1..3].len()\n}\n");
        let lines: Vec<usize> = found
            .iter()
            .filter(|(_, e, _)| *e == Element::Slice)
            .map(|(line, _, _)| *line)
            .collect();
        assert_eq!(lines, [3]);
    }

    // ownership

    #[test]
    fn a_method_in_a_chain_is_on_the_line_of_its_name() {
        let source = "fn f(s: String) -> String {\n    s\n        .trim()\n        .to_owned()\n        .clone()\n}\n";
        assert_eq!(
            elements(source),
            [
                (4, Element::ToOwned, ".to_owned()".to_owned()),
                (5, Element::Clone, ".clone()".to_owned()),
            ]
        );
    }

    #[test]
    fn a_method_called_through_its_path_is_that_element() {
        let found = only(
            "fn f(a: A) -> A {\n    Clone::clone(a)\n}\nfn g(s: S) -> S {\n    S::to_owned(s)\n}\n",
        );
        assert_eq!(found, [Element::Clone, Element::ToOwned]);
    }

    #[test]
    fn drop_is_the_bare_name_or_through_mem_and_not_a_function_of_another_module() {
        let found = only(
            "fn f(a: A, b: B, c: C) {\n    drop(a);\n    std::mem::drop(b);\n    pool::drop(c);\n}\n",
        );
        assert_eq!(found, [Element::Drop, Element::Drop]);
    }

    #[test]
    fn a_function_imported_by_use_is_found_at_the_import() {
        let source =
            "use std::mem::{self, replace, swap as exchange};\nuse std::mem::take;\nuse std::*;\n";
        assert_eq!(
            elements(source),
            [
                (1, Element::MemReplace, "std::mem::replace".to_owned()),
                (1, Element::MemSwap, "std::mem::swap".to_owned()),
                (2, Element::MemTake, "std::mem::take".to_owned()),
            ]
        );
    }

    #[test]
    fn a_function_named_take_outside_mem_is_not_mem_take() {
        assert_eq!(
            only("fn f(r: R) {\n    io::take(r);\n    take(r);\n}\n"),
            []
        );
    }

    // traits_generics

    #[test]
    fn a_trait_in_a_bound_is_trait_bound_but_in_impl_trait_dyn_trait_or_supertraits_is_not() {
        let source = "\
fn f<T: Copy>(a: impl Copy, b: &dyn Copy) where T: Clone {}
trait A: Copy {}
";
        let found = only(source);
        let bounds = found.iter().filter(|&&e| e == Element::TraitBound).count();
        assert_eq!(bounds, 2, "{found:?}");
        assert!(found.contains(&Element::Supertrait));
    }

    #[test]
    fn a_trait_bound_inside_a_dyn_trait_inside_a_bound_is_a_bound_again() {
        let found = only(
            "fn f<T: Into<Box<dyn Fn() + Send>>>() {}\nfn g<U>() where U: Iterator<Item: Copy> {}\n",
        );
        let bounds = found.iter().filter(|&&e| e == Element::TraitBound).count();
        // `T: Into<..>`, `U: Iterator<..>` and `Item: Copy`; not `Fn` or `Send`.
        assert_eq!(bounds, 3, "{found:?}");
    }

    #[test]
    fn a_path_from_a_type_parameter_or_self_in_a_type_is_associated_type() {
        let source = "\
fn f<T>(x: T::Item) {}
fn g(x: T::Item, e: io::Error) {}
impl S { fn h() -> Self::Output {} }
";
        let lines: Vec<usize> = elements(source)
            .into_iter()
            .filter(|(_, e, _)| *e == Element::AssociatedType)
            .map(|(line, _, _)| line)
            .collect();
        assert_eq!(lines, [1, 3]);
    }

    #[test]
    fn a_qualified_path_in_a_type_is_also_associated_type_and_in_an_expression_is_not() {
        assert_eq!(
            only("fn f(x: <T as Iterator>::Item) {}\n"),
            [Element::AssociatedType, Element::QualifiedPath]
        );
        assert_eq!(
            only("fn f() {\n    <u8 as Default>::default();\n}\n"),
            [Element::QualifiedPath]
        );
    }

    #[test]
    fn generic_arguments_after_double_colon_are_turbofish_and_in_a_type_are_not() {
        let source =
            "fn f(v: Vec<u8>) {\n    v.iter()\n        .sum::<u8>();\n    Vec::<u8>::new();\n}\n";
        let lines: Vec<usize> = elements(source)
            .into_iter()
            .filter(|(_, e, _)| *e == Element::Turbofish)
            .map(|(line, _, _)| line)
            .collect();
        assert_eq!(lines, [3, 4]);
    }

    #[test]
    fn an_item_is_on_the_line_of_its_keyword_after_its_attributes() {
        let source = "#[doc = \"x\"]\nimpl Copy for S {}\n#[doc = \"y\"]\ntrait T {}\n";
        assert_eq!(
            elements(source),
            [
                (2, Element::TraitImpl, "impl Copy for S {}".to_owned()),
                (4, Element::TraitDef, "trait T {}".to_owned()),
            ]
        );
    }

    #[test]
    fn a_conversion_through_from_or_into_is_from_into() {
        let found = only("fn f(n: u8) -> u64 {\n    let a = u64::from(n);\n    n.into()\n}\n");
        assert_eq!(found, [Element::FromInto, Element::FromInto]);
    }

    // lifetimes

    #[test]
    fn a_label_is_not_a_lifetime() {
        let source = "fn f() {\n    'outer: loop {\n        'inner: while true {\n            continue 'outer;\n        }\n        break 'outer;\n    }\n}\n";
        assert_eq!(only(source), []);
    }

    #[test]
    fn a_lifetime_in_a_bound_is_lifetime_bound_and_not_lifetime_annotation() {
        let source = "fn f<'a, 'b: 'a, T: 'a>() where 'a: 'b {}\n";
        let found = only(source);
        assert_eq!(
            found
                .iter()
                .filter(|&&e| e == Element::LifetimeBound)
                .count(),
            3,
            "{found:?}"
        );
        assert!(!found.contains(&Element::LifetimeAnnotation), "{found:?}");
    }

    #[test]
    fn static_as_a_bound_is_both_lifetime_bound_and_static_lifetime() {
        let found = only("fn f<T: 'static>(t: T) {}\n");
        assert!(found.contains(&Element::LifetimeBound), "{found:?}");
        assert!(found.contains(&Element::StaticLifetime), "{found:?}");
    }

    #[test]
    fn a_lifetime_on_a_type_is_annotation_static_or_anonymous_by_its_name() {
        assert_eq!(
            only("struct S {\n    a: Foo<'a>,\n    b: Foo<'static>,\n    c: Foo<'_>,\n}\n"),
            [
                Element::LifetimeAnnotation,
                Element::StaticLifetime,
                Element::AnonymousLifetime,
            ]
        );
    }

    // smart_pointers

    #[test]
    fn a_smart_pointer_named_anywhere_in_a_path_or_an_import_is_that_element() {
        let source = "\
use std::sync::{Arc, Mutex};
fn f(g: MutexGuard<u8>, a: AtomicUsize) -> Arc<u8> {
    std::rc::Rc::new(1);
}
";
        assert_eq!(
            only(source),
            [
                Element::Arc,
                Element::Mutex,
                Element::Mutex,
                Element::Atomic,
                Element::Arc,
                Element::Rc,
            ]
        );
    }

    #[test]
    fn a_name_that_only_starts_like_a_smart_pointer_is_not_one() {
        assert_eq!(only("fn f(a: Boxed, b: Atomic, c: Arcade) {}\n"), []);
    }

    #[test]
    fn spawn_and_scope_are_thread_elements_only_through_thread() {
        let source = "fn f() {\n    thread::spawn(g);\n    std::thread::scope(g);\n    tokio::spawn(g);\n    spawn(g);\n}\n";
        assert_eq!(only(source), [Element::ThreadSpawn, Element::ThreadScope]);
    }

    // error_handling

    #[test]
    fn a_question_mark_in_a_chain_is_on_its_own_line() {
        let source =
            "fn f(s: S) -> R {\n    let n = s\n        .first()?\n        .second()?;\n}\n";
        let lines: Vec<usize> = elements(source)
            .into_iter()
            .filter(|(_, e, _)| *e == Element::QuestionMark)
            .map(|(line, _, _)| line)
            .collect();
        assert_eq!(lines, [3, 4]);
    }

    #[test]
    fn none_in_a_pattern_is_option_and_a_binding_with_another_name_is_not() {
        let source =
            "fn f(x: X) {\n    match x {\n        None => {}\n        none => {}\n    }\n}\n";
        assert_eq!(elements(source), [(3, Element::Option, "None".to_owned())]);
    }

    #[test]
    fn expect_with_a_message_is_expect_and_a_method_of_another_arity_is_not() {
        assert_eq!(
            only("fn f(x: X) {\n    x.expect(\"msg\");\n    x.expect();\n}\n"),
            [Element::Expect]
        );
    }

    #[test]
    fn a_box_of_a_dyn_error_with_more_bounds_is_box_dyn_error() {
        let found = only("fn f() -> Box<dyn std::error::Error + Send + Sync> {}\n");
        assert!(found.contains(&Element::BoxDynError), "{found:?}");
        let other = only("fn f() -> Box<dyn Display> {}\n");
        assert!(!other.contains(&Element::BoxDynError), "{other:?}");
    }

    // iterators_closures

    fn adapters_and_combinators(source: &str) -> Vec<Element> {
        only(source)
            .into_iter()
            .filter(|e| matches!(e, Element::IteratorAdapter | Element::ResultCombinator))
            .collect()
    }

    #[test]
    fn map_after_an_iterator_is_an_adapter_even_after_other_ambiguous_methods() {
        let source = "fn f(v: V) {\n    v.iter().filter(p).map(g);\n    (0..3).map(g);\n}\n";
        assert_eq!(
            adapters_and_combinators(source),
            [Element::IteratorAdapter; 3]
        );
    }

    #[test]
    fn map_after_an_option_or_a_result_is_a_combinator() {
        let source =
            "fn f(v: V) {\n    v.first().map(g);\n    Some(1).map(g);\n    s.parse().map(g);\n}\n";
        assert_eq!(
            adapters_and_combinators(source),
            [Element::ResultCombinator; 3]
        );
    }

    #[test]
    fn map_on_a_receiver_the_chain_does_not_tell_is_both_adapter_and_combinator() {
        assert_eq!(
            adapters_and_combinators("fn f(x: X) {\n    x.map(g);\n}\n"),
            [Element::IteratorAdapter, Element::ResultCombinator]
        );
    }

    #[test]
    fn take_with_a_count_is_an_adapter_and_max_with_an_argument_is_not_a_consumer() {
        let source =
            "fn f(x: X, a: u8) {\n    x.take(2);\n    x.take();\n    a.max(3);\n    x.max();\n}\n";
        assert_eq!(
            only(source),
            [Element::IteratorAdapter, Element::IteratorConsumer]
        );
    }

    #[test]
    fn a_range_is_on_the_line_of_its_dots() {
        let found = elements("fn f() {\n    let r = start\n        ..end;\n}\n");
        assert_eq!(found, [(3, Element::Range, "start ..".to_owned())]);
    }

    #[test]
    fn a_move_closure_is_both_closure_and_move_closure() {
        assert_eq!(
            only("fn f() {\n    let g = move || 1;\n}\n"),
            [Element::Closure, Element::MoveClosure]
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
