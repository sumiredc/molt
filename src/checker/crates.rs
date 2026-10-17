//! Finds the external crates content uses (PRD §8.3): the dependency keys a
//! `Cargo.toml` gains, and the first segment of the paths in Rust source.
//!
//! A crate is keyed by its name with `-` spelled `_` (`crate_key`): source can
//! only spell `serde_json`, while `Cargo.toml` may say `serde-json`, and Cargo
//! takes the two spellings for one crate.

use std::collections::{BTreeMap, BTreeSet};

use syn::ext::IdentExt;
use syn::parse::ParseStream;
use syn::punctuated::Punctuated;
use syn::visit::{self, Visit};
use syn::{
    Attribute, ExprPath, ExprStruct, Ident, ItemExternCrate, ItemUse, Macro, Meta, PatStruct,
    PatTupleStruct, Path, QSelf, Token, TypePath, UseTree,
};
use toml_edit::{Document, TableLike};

use crate::checker::finding::{Finding, ParseError};
use crate::checker::macro_args::{self, MacroArgs};
use crate::profile::status::ItemKey;

/// The key a crate is checked under: its name with every `-` turned into `_`.
pub fn crate_key(name: &str) -> ItemKey {
    ItemKey::Crate(name.replace('-', "_"))
}

/// The dependencies `new` declares that `old` does not, one finding per
/// dependency, in line order. A dependency is a key of any dependency table
/// (`[dependencies]`, `[dev-dependencies]`, `[build-dependencies]`, their
/// `[target.*]` forms and `[workspace.dependencies]`) together with the crate
/// it names: its `package` when renamed (`json = { package = "serde_json" }`),
/// the key otherwise. A dependency is new when that pair is not in `old`, so
/// pointing an existing key at another package is reported. The finding is
/// keyed by the crate; names that differ only in `-` and `_` are the same.
///
/// `new` that is not TOML is a `ParseError`, and so is `new` with a key or a
/// `package` that is no crate name Cargo accepts (`is_crate_name`). `old` that
/// is not TOML declares nothing, so every dependency of `new` is reported.
pub fn in_manifest(old: &str, new: &str) -> Result<Vec<Finding>, ParseError> {
    let new_doc = Document::parse(new).map_err(|error| toml_parse_error(new, &error))?;
    let new_deps = dependencies(new_doc.as_table());
    for dep in &new_deps {
        let names = [(dep.key.as_str(), dep.offset)].into_iter().chain(
            dep.package
                .as_deref()
                .map(|package| (package, dep.package_offset)),
        );
        for (name, offset) in names {
            if !is_crate_name(name) {
                let (line, column) = line_col(new, offset);
                // The name is left out: the message reaches the terminal, and the name may hold
                // anything a quoted TOML key can.
                return Err(ParseError {
                    line,
                    column,
                    message: "not a crate name Cargo accepts".to_owned(),
                });
            }
        }
    }
    let old_deps: BTreeSet<(ItemKey, ItemKey)> = Document::parse(old)
        .map(|doc| {
            dependencies(doc.as_table())
                .iter()
                .map(Dependency::identity)
                .collect()
        })
        .unwrap_or_default();
    let mut findings: Vec<Finding> = new_deps
        .into_iter()
        .filter(|dep| !old_deps.contains(&dep.identity()))
        .map(|dep| Finding {
            line: line_col(new, dep.offset).0,
            key: crate_key(dep.package.as_deref().unwrap_or(&dep.key)),
            text: match &dep.package {
                Some(package) => format!("{} (package {package})", dep.key),
                None => dep.key,
            },
        })
        .collect();
    findings.sort_by_key(|finding| finding.line);
    Ok(findings)
}

/// The keys of every dependency table of `manifest`, as crate keys: the names source code
/// spells the dependencies by. A manifest that is not TOML declares nothing.
pub fn declared(manifest: &str) -> BTreeSet<ItemKey> {
    Document::parse(manifest)
        .map(|doc| {
            dependencies(doc.as_table())
                .iter()
                .map(|dep| crate_key(&dep.key))
                .collect()
        })
        .unwrap_or_default()
}

/// One key of a dependency table, as written.
struct Dependency {
    key: String,
    /// The `package` it renames, if any.
    package: Option<String>,
    /// Byte offset of the key.
    offset: usize,
    /// Byte offset of the `package` value, or of the key when there is none.
    package_offset: usize,
}

impl Dependency {
    /// The key and the crate it names, each as a crate key.
    fn identity(&self) -> (ItemKey, ItemKey) {
        let package = self.package.as_deref().unwrap_or(&self.key);
        (crate_key(&self.key), crate_key(package))
    }
}

/// The tables whose keys are dependencies, at the top level and under each
/// `[target.*]`.
const DEPENDENCY_TABLES: &[&str] = &[
    "dependencies",
    "dev-dependencies",
    "dev_dependencies",
    "build-dependencies",
    "build_dependencies",
];

/// Each dependency of the manifest.
fn dependencies(root: &dyn TableLike) -> Vec<Dependency> {
    let mut tables: Vec<&dyn TableLike> = Vec::new();
    let mut parents = vec![root];
    if let Some(targets) = root.get("target").and_then(|item| item.as_table_like()) {
        parents.extend(targets.iter().filter_map(|(_, item)| item.as_table_like()));
    }
    for parent in parents {
        tables.extend(
            DEPENDENCY_TABLES
                .iter()
                .filter_map(|name| parent.get(name).and_then(|item| item.as_table_like())),
        );
    }
    if let Some(table) = root
        .get("workspace")
        .and_then(|item| item.get("dependencies"))
        .and_then(|item| item.as_table_like())
    {
        tables.push(table);
    }
    let mut found = Vec::new();
    for table in tables {
        for (name, item) in table.iter() {
            let span = table.get_key_value(name).and_then(|(key, _)| key.span());
            let offset = span.map_or(0, |span| span.start);
            let package = item.get("package");
            found.push(Dependency {
                key: name.to_owned(),
                package: package
                    .and_then(|package| package.as_str())
                    .map(str::to_owned),
                offset,
                package_offset: package
                    .and_then(|package| package.span())
                    .map_or(offset, |span| span.start),
            });
        }
    }
    found
}

/// Whether Cargo accepts `name` as a crate name: a Unicode XID start character
/// or `_`, then XID continue characters and `-` (PRD §9). Control characters
/// are none of these.
fn is_crate_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| unicode_ident::is_xid_start(first) || first == '_')
        && chars.all(|c| unicode_ident::is_xid_continue(c) || c == '-')
}

fn toml_parse_error(source: &str, error: &toml_edit::TomlError) -> ParseError {
    let (line, column) = line_col(source, error.span().map_or(0, |span| span.start));
    ParseError {
        line,
        column,
        message: error.message().to_owned(),
    }
}

/// The 1-based line and column (in characters) of a byte offset.
fn line_col(source: &str, offset: usize) -> (usize, usize) {
    let before = source.get(..offset).unwrap_or(source);
    let line = before.matches('\n').count() + 1;
    let column = before.rsplit('\n').next().unwrap_or("").chars().count() + 1;
    (line, column)
}

/// The crates the paths in `source` start from, one finding per crate and
/// line. `text` is the crate's name as the source spells it.
///
/// The first segment of a path is a crate when it is none of `std`, `core`,
/// `alloc`, `proc_macro`, `test`, `crate`, `self`, `super` and `own_modules`,
/// and, unless the path starts with `::` or is an `extern crate`, when also:
///
/// - the path has two segments or more, or is the root of a `use`
/// - it is no module, type, trait or `macro_rules!` declared in `source`, and
///   no name a `use` in `source` brings in
/// - it is no primitive type (`u8::MAX`), and, outside the root of a `use`,
///   does not start with an uppercase letter (`Vec::new`), since crate names
///   are snake_case
///
/// A first segment that is an alias of a crate (`use tokio as rt;`) names
/// that crate. A `use` reports its crate on every line of its tree.
/// The paths in the arguments of a std macro that takes Rust expressions
/// (`println!("{}", serde_json::to_string(&x))`) are read too
/// (`macro_args`).
///
/// Content syn cannot parse has no crates here: `visitor::detect` reports it.
pub fn in_source(source: &str, own_modules: &BTreeSet<String>) -> Vec<Finding> {
    let Ok(file) = syn::parse_file(source) else {
        return Vec::new();
    };
    let mut locals = Locals::default();
    locals.visit_file(&file);
    let mut finder = Finder {
        locals,
        own_modules: own_modules
            .iter()
            .map(|module| module.replace('-', "_"))
            .collect(),
        not_rooted: None,
        seen: BTreeSet::new(),
        findings: Vec::new(),
    };
    finder.visit_file(&file);
    finder.findings
}

/// First segments that are never a crate: the path keywords, and the crates
/// that come with the toolchain rather than from `Cargo.toml`.
const NOT_CRATES: &[&str] = &[
    "std",
    "core",
    "alloc",
    "proc_macro",
    "test",
    "crate",
    "self",
    "super",
];

/// Primitive types, whose associated items look like paths into a crate.
const PRIMITIVES: &[&str] = &[
    "bool", "char", "str", "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64",
    "u128", "usize", "f16", "f32", "f64", "f128",
];

/// Tool namespaces of attributes (`#[rustfmt::skip]`), which are not crates.
const TOOLS: &[&str] = &["rustfmt", "clippy", "rustdoc", "diagnostic"];

/// The names `source` declares or imports, which a path may start from
/// without naming a crate, and the aliases of crate roots. Collected over the
/// whole content, whatever the scope: a name declared anywhere hides a crate
/// of that name everywhere in the file.
#[derive(Default)]
struct Locals {
    names: BTreeSet<String>,
    /// `rt` → (`tokio`, whether `tokio` can only be a crate) for
    /// `use tokio as rt;`, `use tokio::{self as rt};` and
    /// `extern crate tokio as rt;`.
    aliases: BTreeMap<String, (String, bool)>,
}

impl Locals {
    fn insert(&mut self, ident: &Ident) {
        self.names.insert(ident.unraw().to_string());
    }

    fn alias(&mut self, alias: &Ident, root: &Ident, absolute: bool) {
        self.aliases.insert(
            alias.unraw().to_string(),
            (root.unraw().to_string(), absolute),
        );
    }

    /// The names a `use` brings in. Below a root (`use a::..`), each name, the
    /// parent of a `self` and each rename are local names, except what stands
    /// for the root itself: `use tokio::{self}` brings in the crate, and
    /// `use tokio::{self as rt}` or `use tokio as rt` makes an alias of it.
    /// The `tokio` of `use tokio;` is the crate itself, not a local name.
    fn use_tree(&mut self, tree: &UseTree, parent: Option<&Ident>, root: bool, absolute: bool) {
        match tree {
            UseTree::Path(path) => {
                self.use_tree(&path.tree, Some(&path.ident), parent.is_none(), absolute);
            }
            UseTree::Name(name) if name.ident == "self" => {
                if let Some(parent) = parent
                    && !root
                {
                    self.insert(parent);
                }
            }
            UseTree::Name(name) => {
                if parent.is_some() {
                    self.insert(&name.ident);
                }
            }
            UseTree::Rename(rename) => match parent {
                None => self.alias(&rename.rename, &rename.ident, absolute),
                Some(parent) if root && rename.ident == "self" => {
                    self.alias(&rename.rename, parent, absolute);
                }
                Some(_) => self.insert(&rename.rename),
            },
            UseTree::Group(group) => {
                for tree in &group.items {
                    self.use_tree(tree, parent, root, absolute);
                }
            }
            UseTree::Glob(_) => {}
        }
    }
}

impl<'ast> Visit<'ast> for Locals {
    fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
        self.insert(&item.ident);
        visit::visit_item_mod(self, item);
    }

    fn visit_item_struct(&mut self, item: &'ast syn::ItemStruct) {
        self.insert(&item.ident);
        visit::visit_item_struct(self, item);
    }

    fn visit_item_enum(&mut self, item: &'ast syn::ItemEnum) {
        self.insert(&item.ident);
        visit::visit_item_enum(self, item);
    }

    fn visit_item_union(&mut self, item: &'ast syn::ItemUnion) {
        self.insert(&item.ident);
        visit::visit_item_union(self, item);
    }

    fn visit_item_type(&mut self, item: &'ast syn::ItemType) {
        self.insert(&item.ident);
        visit::visit_item_type(self, item);
    }

    fn visit_item_trait(&mut self, item: &'ast syn::ItemTrait) {
        self.insert(&item.ident);
        visit::visit_item_trait(self, item);
    }

    /// A `macro_rules!` may be re-exported by name (`pub(crate) use m;`).
    fn visit_item_macro(&mut self, item: &'ast syn::ItemMacro) {
        if let Some(ident) = &item.ident {
            self.insert(ident);
        }
        visit::visit_item_macro(self, item);
    }

    fn visit_item_extern_crate(&mut self, item: &'ast ItemExternCrate) {
        if let Some((_, rename)) = &item.rename {
            self.alias(rename, &item.ident, true);
        }
        visit::visit_item_extern_crate(self, item);
    }

    fn visit_item_use(&mut self, item: &'ast ItemUse) {
        self.use_tree(&item.tree, None, false, item.leading_colon.is_some());
        visit::visit_item_use(self, item);
    }

    fn visit_macro(&mut self, mac: &'ast Macro) {
        visit::visit_macro(self, mac);
        if let MacroArgs::Parsed(args) = macro_args::read(mac) {
            macro_args::visit(&args, self);
        }
    }
}

struct Finder {
    locals: Locals,
    own_modules: BTreeSet<String>,
    /// The path being visited whose first segment follows a qualified self
    /// type with no trait (`<T>::assoc::x`), and so is not a crate. Compared
    /// by address only.
    not_rooted: Option<*const Path>,
    seen: BTreeSet<(usize, ItemKey)>,
    findings: Vec<Finding>,
}

impl Finder {
    /// Reports `ident` as a crate unless it is one of the names that are not.
    /// `absolute` is for a segment that can only name a crate: after a
    /// leading `::`, and in `extern crate`.
    fn first_segment(&mut self, ident: &Ident, absolute: bool) {
        if let Some(name) = self.crate_name(ident, absolute, false) {
            self.report(ident.span().start().line, name);
        }
    }

    /// The crate `ident` names as a first segment, or `None` when it is one of
    /// the names that are not crates. An alias names the crate it stands for.
    /// `use_root` is for the root of a `use`, where an uppercase name is
    /// taken for a crate: an enum or type there is declared or imported in
    /// the file, and so already a local name.
    fn crate_name(&self, ident: &Ident, absolute: bool, use_root: bool) -> Option<String> {
        let name = ident.unraw().to_string();
        if !absolute && let Some((root, root_absolute)) = self.locals.aliases.get(&name) {
            return self.classify(root.clone(), *root_absolute, true);
        }
        self.classify(name, absolute, use_root)
    }

    fn classify(&self, name: String, absolute: bool, use_root: bool) -> Option<String> {
        if NOT_CRATES.contains(&name.as_str()) || self.own_modules.contains(&name) {
            return None;
        }
        if !absolute
            && (self.locals.names.contains(&name)
                || PRIMITIVES.contains(&name.as_str())
                || (!use_root && name.starts_with(|c: char| c.is_uppercase())))
        {
            return None;
        }
        Some(name)
    }

    fn report(&mut self, line: usize, name: String) {
        let key = crate_key(&name);
        if self.seen.insert((line, key.clone())) {
            self.findings.push(Finding {
                line,
                key,
                text: name,
            });
        }
    }

    /// The root of each path in a `use`. A crate is reported on the line of
    /// its root and on every line of the rest of the tree, so that a name
    /// added on its own line to an existing `use tokio::{..}` is reported
    /// although the root's line is unchanged.
    fn use_roots(&mut self, tree: &UseTree, absolute: bool) {
        match tree {
            UseTree::Path(path) => {
                if let Some(name) = self.crate_name(&path.ident, absolute, true) {
                    self.report(path.ident.span().start().line, name.clone());
                    let mut lines = BTreeSet::new();
                    use_tree_lines(&path.tree, &mut lines);
                    for line in lines {
                        self.report(line, name.clone());
                    }
                }
            }
            UseTree::Name(name) => self.use_root(&name.ident, absolute),
            UseTree::Rename(rename) => self.use_root(&rename.ident, absolute),
            UseTree::Group(group) => {
                for tree in &group.items {
                    self.use_roots(tree, absolute);
                }
            }
            UseTree::Glob(_) => {}
        }
    }

    fn use_root(&mut self, ident: &Ident, absolute: bool) {
        if let Some(name) = self.crate_name(ident, absolute, true) {
            self.report(ident.span().start().line, name);
        }
    }

    /// Visits a node with a path that may follow a qualified self type: with
    /// no trait (`<T>::x`, position 0) the path does not start from a crate.
    fn with_qself(&mut self, qself: Option<&QSelf>, path: &Path, visit: impl FnOnce(&mut Self)) {
        let outer = self.not_rooted;
        if qself.is_some_and(|qself| qself.position == 0) {
            self.not_rooted = Some(path);
        }
        visit(self);
        self.not_rooted = outer;
    }
}

impl<'ast> Visit<'ast> for Finder {
    fn visit_path(&mut self, path: &'ast Path) {
        let rooted = self.not_rooted != Some(path as *const Path);
        if rooted
            && (path.leading_colon.is_some() || path.segments.len() > 1)
            && let Some(first) = path.segments.first()
        {
            self.first_segment(&first.ident, path.leading_colon.is_some());
        }
        visit::visit_path(self, path);
    }

    fn visit_item_use(&mut self, item: &'ast ItemUse) {
        self.use_roots(&item.tree, item.leading_colon.is_some());
        visit::visit_item_use(self, item);
    }

    /// The path of the macro, and the arguments of a std macro whose
    /// arguments are Rust (`macro_args`).
    fn visit_macro(&mut self, mac: &'ast Macro) {
        visit::visit_macro(self, mac);
        if let MacroArgs::Parsed(args) = macro_args::read(mac) {
            macro_args::visit(&args, self);
        }
    }

    fn visit_item_extern_crate(&mut self, item: &'ast ItemExternCrate) {
        self.first_segment(&item.ident, true);
        visit::visit_item_extern_crate(self, item);
    }

    /// The path of each attribute (with `cfg_attr` unwrapped) and the traits
    /// of a `derive`. Tool attributes (`#[rustfmt::skip]`) are skipped whole.
    fn visit_attribute(&mut self, attr: &'ast Attribute) {
        for meta in effective_metas(&attr.meta) {
            if is_tool(meta.path()) {
                continue;
            }
            visit_owned_meta(self, &meta);
            if meta.path().is_ident("derive")
                && let Ok(traits) = meta.require_list().and_then(|list| {
                    list.parse_args_with(Punctuated::<Path, Token![,]>::parse_terminated)
                })
            {
                for path in &traits {
                    visit_owned_path(self, path);
                }
            }
        }
    }

    fn visit_expr_path(&mut self, expr: &'ast ExprPath) {
        self.with_qself(expr.qself.as_ref(), &expr.path, |v| {
            visit::visit_expr_path(v, expr)
        });
    }

    fn visit_expr_struct(&mut self, expr: &'ast ExprStruct) {
        self.with_qself(expr.qself.as_ref(), &expr.path, |v| {
            visit::visit_expr_struct(v, expr)
        });
    }

    fn visit_pat_struct(&mut self, pat: &'ast PatStruct) {
        self.with_qself(pat.qself.as_ref(), &pat.path, |v| {
            visit::visit_pat_struct(v, pat)
        });
    }

    fn visit_pat_tuple_struct(&mut self, pat: &'ast PatTupleStruct) {
        self.with_qself(pat.qself.as_ref(), &pat.path, |v| {
            visit::visit_pat_tuple_struct(v, pat)
        });
    }

    fn visit_type_path(&mut self, ty: &'ast TypePath) {
        self.with_qself(ty.qself.as_ref(), &ty.path, |v| {
            visit::visit_type_path(v, ty)
        });
    }
}

/// Visits a meta made by unwrapping `cfg_attr`, which does not live as long
/// as the file.
fn visit_owned_meta(finder: &mut Finder, meta: &Meta) {
    finder.visit_meta(meta);
}

fn visit_owned_path(finder: &mut Finder, path: &Path) {
    finder.visit_path(path);
}

/// The attributes `meta` stands for: itself, or for `cfg_attr(pred, a, b)`
/// the attributes `a` and `b` (unwrapped in turn). Arguments that do not
/// parse are left to `visitor::detect`, which reports them.
fn effective_metas(meta: &Meta) -> Vec<Meta> {
    if !meta.path().is_ident("cfg_attr") {
        return vec![meta.clone()];
    }
    let attrs = meta.require_list().and_then(|list| {
        list.parse_args_with(|input: ParseStream| {
            input.parse::<Meta>()?;
            input.parse::<Token![,]>()?;
            Punctuated::<Meta, Token![,]>::parse_terminated(input)
        })
    });
    match attrs {
        Ok(attrs) => attrs.iter().flat_map(effective_metas).collect(),
        Err(_) => Vec::new(),
    }
}

/// The lines of the names in a `use` tree (segments, names, renames, `*`).
fn use_tree_lines(tree: &UseTree, lines: &mut BTreeSet<usize>) {
    match tree {
        UseTree::Path(path) => {
            lines.insert(path.ident.span().start().line);
            use_tree_lines(&path.tree, lines);
        }
        UseTree::Name(name) => {
            lines.insert(name.ident.span().start().line);
        }
        UseTree::Rename(rename) => {
            lines.insert(rename.ident.span().start().line);
        }
        UseTree::Glob(glob) => {
            lines.insert(glob.star_token.span.start().line);
        }
        UseTree::Group(group) => {
            for tree in &group.items {
                use_tree_lines(tree, lines);
            }
        }
    }
}

/// A path in a tool namespace (`rustfmt::skip`).
fn is_tool(path: &Path) -> bool {
    path.leading_colon.is_none()
        && path.segments.len() > 1
        && path
            .segments
            .first()
            .is_some_and(|segment| TOOLS.contains(&segment.ident.to_string().as_str()))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::checker::{CheckContext, CheckReport, check_file};
    use crate::profile::element::Element;
    use crate::profile::status::{Provenance, Status};
    use crate::profile::store::{Profile, Timestamp};

    fn krate(name: &str) -> ItemKey {
        ItemKey::Crate(name.to_owned())
    }

    /// `(line, crate)` of each crate found in `source`, with no own modules.
    fn source(source: &str) -> Vec<(usize, ItemKey)> {
        source_with(source, &[])
    }

    fn source_with(source: &str, own_modules: &[&str]) -> Vec<(usize, ItemKey)> {
        let own_modules = own_modules.iter().map(|m| (*m).to_owned()).collect();
        in_source(source, &own_modules)
            .into_iter()
            .map(|f| (f.line, f.key))
            .collect()
    }

    fn manifest(old: &str, new: &str) -> Vec<(usize, ItemKey)> {
        in_manifest(old, new)
            .unwrap()
            .into_iter()
            .map(|f| (f.line, f.key))
            .collect()
    }

    // --- the key ---

    #[test]
    fn a_crate_key_spells_hyphens_as_underscores() {
        assert_eq!(crate_key("serde-json"), krate("serde_json"));
        assert_eq!(crate_key("serde_json"), krate("serde_json"));
        assert_eq!(crate_key("tokio"), krate("tokio"));
    }

    // --- Cargo.toml ---

    const OLD_MANIFEST: &str = "\
[package]
name = \"app\"

[dependencies]
serde = \"1\"
";

    #[test]
    fn a_dependency_added_to_the_manifest_is_reported_on_its_line() {
        let new = format!("{OLD_MANIFEST}tokio = \"1\"\n");
        assert_eq!(manifest(OLD_MANIFEST, &new), [(6, krate("tokio"))]);
    }

    #[test]
    fn a_dependency_already_in_the_old_manifest_is_not_reported() {
        let new = OLD_MANIFEST.replace("serde = \"1\"", "serde = \"1.0.200\"");
        assert_eq!(manifest(OLD_MANIFEST, &new), []);
    }

    #[test]
    fn a_dependency_respelled_with_underscores_is_not_a_new_dependency() {
        let old = "[dependencies]\nserde-json = \"1\"\n";
        let new = "[dependencies]\nserde_json = \"1\"\n";
        assert_eq!(manifest(old, new), []);
    }

    #[test]
    fn a_hyphenated_dependency_is_reported_under_its_underscored_key_and_as_written() {
        let new = "[dependencies]\nproc-macro2 = \"1\"\n";
        let findings = in_manifest("", new).unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].key, krate("proc_macro2"));
        assert_eq!(findings[0].text, "proc-macro2");
    }

    #[test]
    fn dependencies_in_every_dependency_table_are_reported() {
        let new = "\
[dependencies]
a = \"1\"
[dev-dependencies]
b = \"1\"
[build-dependencies]
c = \"1\"
[dev_dependencies]
d = \"1\"
[build_dependencies]
e = \"1\"
[target.'cfg(unix)'.dependencies]
f = \"1\"
[target.'cfg(unix)'.dev-dependencies]
g = \"1\"
[workspace.dependencies]
h = \"1\"
";
        assert_eq!(
            manifest("", new),
            [
                (2, krate("a")),
                (4, krate("b")),
                (6, krate("c")),
                (8, krate("d")),
                (10, krate("e")),
                (12, krate("f")),
                (14, krate("g")),
                (16, krate("h")),
            ]
        );
    }

    #[test]
    fn a_renamed_dependency_is_reported_as_its_package() {
        let new = "[dependencies]\njson = { package = \"serde-json\", version = \"1\" }\n";
        let findings = in_manifest("", new).unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(
            (
                findings[0].line,
                &findings[0].key,
                findings[0].text.as_str()
            ),
            (2, &krate("serde_json"), "json (package serde-json)")
        );
    }

    #[test]
    fn an_existing_dependency_pointed_at_another_package_is_reported() {
        let new = OLD_MANIFEST.replace(
            "serde = \"1\"",
            "serde = { package = \"other\", version = \"1\" }",
        );
        assert_eq!(manifest(OLD_MANIFEST, &new), [(5, krate("other"))]);
    }

    #[test]
    fn a_dependency_in_a_table_of_its_own_is_reported_on_its_header() {
        let new = "[package]\nname = \"app\"\n\n[dependencies.tokio]\nversion = \"1\"\n";
        assert_eq!(manifest("", new), [(4, krate("tokio"))]);
    }

    #[test]
    fn a_dotted_dependency_key_is_reported() {
        let new = "dependencies.tokio.version = \"1\"\n";
        assert_eq!(manifest("", new), [(1, krate("tokio"))]);
    }

    #[test]
    fn keys_outside_the_dependency_tables_are_not_crates() {
        let new = "\
[package]
name = \"app\"
[features]
tokio = []
[patch.crates-io]
serde = { path = \"../serde\" }
[lints.clippy]
all = \"warn\"
";
        assert_eq!(manifest("", new), []);
    }

    #[test]
    fn a_manifest_that_is_not_toml_is_a_parse_error_with_its_position() {
        let new = "[dependencies]\ntokio = \n";
        let error = in_manifest("", new).unwrap_err();
        assert_eq!((error.line, error.column), (2, 9));
    }

    #[test]
    fn a_dependency_name_cargo_does_not_accept_is_a_parse_error_without_the_name() {
        for name in [
            "a\\u001b[31mplanted",
            ".planted",
            "1planted",
            "-planted",
            "plan ted",
            "",
        ] {
            for new in [
                format!("[dependencies]\n\"{name}\" = \"1\"\n"),
                format!("[dependencies]\nx = {{ version = \"1\", package = \"{name}\" }}\n"),
            ] {
                let error = in_manifest("", &new).unwrap_err();
                assert_eq!(error.line, 2, "{new}");
                assert!(!error.message.contains("planted"), "{}", error.message);
            }
        }
    }

    #[test]
    fn a_package_cargo_does_not_accept_is_an_error_at_the_package() {
        let new = "[dependencies.x]\nversion = \"1\"\npackage = \"-planted\"\n";
        let error = in_manifest("", new).unwrap_err();
        assert_eq!((error.line, error.column), (3, 11));
    }

    #[test]
    fn crate_names_cargo_accepts_are_read_as_before() {
        let new = "[dependencies]\nserde_json = \"1\"\ntokio-util = \"1\"\n_private = \"1\"\n\
                   \"日本語\" = { path = \"../ja\" }\nx = { version = \"1\", package = \"café\" }\n";
        assert_eq!(
            manifest("", new),
            [
                (2, krate("serde_json")),
                (3, krate("tokio_util")),
                (4, krate("_private")),
                (5, krate("日本語")),
                (6, krate("café")),
            ]
        );
    }

    #[test]
    fn when_the_old_manifest_is_not_toml_every_dependency_is_new() {
        let new = "[dependencies]\nserde = \"1\"\n";
        assert_eq!(manifest("[dependencies\n", new), [(2, krate("serde"))]);
    }

    // --- Rust source: where a crate shows ---

    #[test]
    fn the_root_of_a_use_is_a_crate() {
        assert_eq!(source("use tokio::sync::Mutex;\n"), [(1, krate("tokio"))]);
    }

    #[test]
    fn each_root_in_a_grouped_use_is_a_crate() {
        assert_eq!(
            source("use {serde::Serialize, tokio::spawn};\n"),
            [(1, krate("serde")), (1, krate("tokio"))]
        );
    }

    #[test]
    fn a_use_of_the_crate_alone_is_a_crate() {
        assert_eq!(
            source("use serde_json;\nuse anyhow as ah;\n"),
            [(1, krate("serde_json")), (2, krate("anyhow"))]
        );
    }

    #[test]
    fn a_path_from_the_root_in_an_expression_is_a_crate() {
        assert_eq!(
            source("fn f() {\n    ::tokio::spawn(async {});\n}\n"),
            [(2, krate("tokio"))]
        );
    }

    #[test]
    fn a_path_in_an_expression_without_use_is_a_crate() {
        assert_eq!(
            source("fn f() {\n    let v = serde_json::json_value();\n}\n"),
            [(2, krate("serde_json"))]
        );
    }

    #[test]
    fn a_path_in_a_type_is_a_crate() {
        assert_eq!(
            source("fn f(\n    v: serde_json::Value,\n) -> anyhow::Result<()> {\n    Ok(())\n}\n"),
            [(2, krate("serde_json")), (3, krate("anyhow"))]
        );
    }

    #[test]
    fn the_path_of_a_macro_is_a_crate() {
        assert_eq!(
            source("fn f() {\n    anyhow::bail!(\"no\");\n}\n"),
            [(2, krate("anyhow"))]
        );
    }

    #[test]
    fn the_path_of_an_attribute_is_a_crate() {
        assert_eq!(
            source("#[tokio::main]\nasync fn main() {}\n"),
            [(1, krate("tokio"))]
        );
    }

    #[test]
    fn a_derive_through_a_crate_path_is_a_crate() {
        assert_eq!(
            source("#[derive(Debug, serde::Serialize)]\nstruct S;\n"),
            [(1, krate("serde"))]
        );
    }

    #[test]
    fn an_attribute_inside_cfg_attr_is_a_crate() {
        assert_eq!(
            source("#[cfg_attr(test, derive(serde::Serialize), tokio::main)]\nstruct S;\n"),
            [(1, krate("serde")), (1, krate("tokio"))]
        );
    }

    #[test]
    fn a_trait_path_after_a_qualified_self_type_is_a_crate() {
        assert_eq!(
            source("fn f<T>(t: &T) {\n    <T as serde::Serialize>::serialize(t);\n}\n"),
            [(2, krate("serde"))]
        );
    }

    #[test]
    fn an_extern_crate_is_a_crate() {
        assert_eq!(
            source("extern crate rand;\nextern crate serde as sd;\n"),
            [(1, krate("rand")), (2, krate("serde"))]
        );
    }

    #[test]
    fn a_crate_used_on_several_lines_is_reported_on_each_line_once() {
        let content = "fn f() {\n    tokio::spawn(tokio::task::yield_now());\n    tokio::spawn(async {});\n}\n";
        assert_eq!(source(content), [(2, krate("tokio")), (3, krate("tokio"))]);
    }

    #[test]
    fn a_crate_in_source_is_reported_under_its_name_as_written() {
        let findings = in_source("use tokio::sync::Mutex;\n", &BTreeSet::new());
        assert_eq!(findings[0].text, "tokio");
    }

    #[test]
    fn a_raw_identifier_is_reported_without_its_prefix() {
        assert_eq!(source("use r#async::x;\n"), [(1, krate("async"))]);
    }

    // --- Rust source: first segments that are not crates ---

    #[test]
    fn the_crates_of_the_toolchain_are_not_crates() {
        let content = "use std::fmt;\nuse core::mem;\nuse alloc::vec::Vec;\nextern crate alloc;\nfn f() { ::std::process::exit(0); }\nuse proc_macro::TokenStream;\nextern crate test;\n";
        assert_eq!(source(content), []);
    }

    #[test]
    fn crate_self_super_and_self_type_are_not_crates() {
        let content = "use crate::a::B;\nuse self::c::D;\nuse super::e::F;\nimpl G { fn f() -> Self::H { Self::new() } }\n";
        assert_eq!(source(content), []);
    }

    #[test]
    fn the_projects_own_modules_are_not_crates() {
        let content = "use my_app::config;\nfn f() { checker::run(); }\n";
        assert_eq!(source_with(content, &["my_app", "checker"]), []);
    }

    #[test]
    fn an_own_module_given_with_hyphens_matches_its_underscored_path() {
        assert_eq!(source_with("use my_app::config;\n", &["my-app"]), []);
    }

    #[test]
    fn a_module_or_type_declared_in_the_content_is_not_a_crate() {
        let content = "\
mod inner { pub fn f() {} }
enum color { Red }
struct st;
union un { a: u8 }
type ty = u8;
trait tr { fn new() -> u8; }
fn g() {
    inner::f();
    let _ = color::Red;
    st::f();
    un::f();
    ty::f();
    tr::new();
}
";
        assert_eq!(source(content), []);
    }

    #[test]
    fn a_macro_rules_reexported_by_a_use_is_not_a_crate() {
        let content = "macro_rules! named {\n    () => {};\n}\npub(crate) use named;\n";
        assert_eq!(source(content), []);
    }

    #[test]
    fn a_name_brought_in_by_a_use_is_not_a_crate() {
        let content = "use std::fmt;\nuse std::io as stdio;\nimpl fmt::Display for S {}\nfn f() { stdio::stdout(); }\n";
        assert_eq!(source(content), []);
    }

    #[test]
    fn a_path_from_an_alias_of_a_crate_is_reported_as_that_crate() {
        let content = "use tokio as rt;\nfn f() {\n    rt::spawn(async {});\n}\n";
        assert_eq!(source(content), [(1, krate("tokio")), (3, krate("tokio"))]);
    }

    #[test]
    fn a_path_from_a_self_alias_of_a_crate_is_reported_as_that_crate() {
        let content = "use tokio::{self as rt};\nfn f() {\n    rt::spawn(async {});\n}\n";
        assert_eq!(source(content), [(1, krate("tokio")), (3, krate("tokio"))]);
    }

    #[test]
    fn a_path_from_an_extern_crate_alias_is_reported_as_that_crate() {
        let content = "extern crate serde as sd;\nfn f() {\n    sd::x();\n}\n";
        assert_eq!(source(content), [(1, krate("serde")), (3, krate("serde"))]);
    }

    #[test]
    fn an_alias_of_a_crate_starting_with_an_uppercase_letter_names_that_crate() {
        let content = "use Inflector as inf;\nfn f() {\n    inf::x();\n}\n";
        assert_eq!(
            source(content),
            [(1, krate("Inflector")), (3, krate("Inflector"))]
        );
    }

    #[test]
    fn an_alias_of_a_path_from_the_root_names_the_crate_even_with_a_local_name() {
        let content = "mod tokio {}\nuse ::tokio as rt;\nfn f() {\n    rt::spawn();\n}\n";
        assert_eq!(source(content), [(2, krate("tokio")), (4, krate("tokio"))]);
    }

    #[test]
    fn an_alias_of_std_is_not_a_crate() {
        let content = "use std as s;\nfn f() {\n    s::mem::drop(1);\n}\n";
        assert_eq!(source(content), []);
    }

    #[test]
    fn a_crate_brought_in_with_self_stays_a_crate() {
        let content =
            "use serde_json::{self, Value};\nfn f() {\n    serde_json::to_string(&1);\n}\n";
        assert_eq!(
            source(content),
            [(1, krate("serde_json")), (3, krate("serde_json"))]
        );
    }

    #[test]
    fn a_module_brought_in_with_self_below_the_root_is_not_a_crate() {
        let content = "use std::io::{self};\nfn f() {\n    io::stdout();\n}\n";
        assert_eq!(source(content), []);
    }

    #[test]
    fn a_type_parameter_does_not_hide_a_crate() {
        let content = "fn f<serde>() {\n    serde::x();\n}\n";
        assert_eq!(source(content), [(2, krate("serde"))]);
    }

    #[test]
    fn the_root_of_a_use_starting_with_an_uppercase_letter_is_a_crate() {
        assert_eq!(source("use Inflector::cases;\n"), [(1, krate("Inflector"))]);
    }

    #[test]
    fn a_single_segment_path_is_not_a_crate() {
        let content = "fn f(value: Value) {\n    let x = helper(value);\n}\n";
        assert_eq!(source(content), []);
    }

    #[test]
    fn a_first_segment_starting_with_an_uppercase_letter_is_not_a_crate() {
        let content = "fn f() {\n    let v = Vec::new();\n    let o = Option::Some(1);\n    let _ = Ordering::Less;\n}\n";
        assert_eq!(source(content), []);
    }

    #[test]
    fn a_primitive_type_is_not_a_crate() {
        let content = "fn f() {\n    let _ = u8::MAX;\n    let _ = str::from_utf8(b\"\");\n    let _ = f64::consts;\n}\n";
        assert_eq!(source(content), []);
    }

    #[test]
    fn a_path_after_a_qualified_self_type_without_a_trait_is_not_a_crate() {
        let content = "fn f() {\n    let _ = <Thing>::assoc::value;\n}\n";
        assert_eq!(source(content), []);
    }

    #[test]
    fn a_tool_attribute_is_not_a_crate() {
        let content = "#[rustfmt::skip]\n#[clippy::msrv = \"1.70\"]\n#[diagnostic::on_unimplemented(message = \"x\")]\ntrait T {}\n";
        assert_eq!(source(content), []);
    }

    #[test]
    fn a_path_from_the_root_is_a_crate_even_with_a_local_name_or_uppercase() {
        let content = "mod tokio {}\nfn f() {\n    ::tokio::spawn();\n    ::Inflector::x();\n}\n";
        assert_eq!(
            source(content),
            [(3, krate("tokio")), (4, krate("Inflector"))]
        );
    }

    #[test]
    fn content_that_does_not_parse_has_no_crates() {
        assert_eq!(source("use tokio::\n"), []);
    }

    // --- check_file ---

    fn check(profile: &Profile, path: &str, old: &str, new: &str) -> CheckReport {
        let own_modules = BTreeSet::new();
        let ctx = CheckContext {
            profile,
            own_modules: &own_modules,
        };
        check_file(Path::new(path), old, new, &ctx)
    }

    fn with_crate(name: &str, status: Status) -> Profile {
        with_crates(&[(name, status)])
    }

    fn with_crates(crates: &[(&str, Status)]) -> Profile {
        let mut profile = Profile::new();
        for (name, status) in crates {
            profile
                .transition(
                    krate(name),
                    *status,
                    Provenance::Manual,
                    Timestamp::from_unix_seconds(0),
                    None,
                )
                .unwrap();
        }
        profile
    }

    fn reported(report: &CheckReport) -> Vec<(usize, ItemKey)> {
        match report {
            CheckReport::OutOfProfile(findings) => {
                findings.iter().map(|f| (f.line, f.key.clone())).collect()
            }
            other => panic!("expected OutOfProfile, got {other:?}"),
        }
    }

    #[test]
    fn check_file_reports_a_crate_used_in_source_even_if_the_manifest_has_it() {
        // The manifest is not an input of a source check: a dependency already
        // in Cargo.toml is stopped where the source first uses it.
        let report = check(
            &Profile::new(),
            "src/main.rs",
            "",
            "use tokio::sync::Notify;\n",
        );
        assert_eq!(reported(&report), [(1, krate("tokio"))]);
    }

    #[test]
    fn check_file_does_not_report_a_known_crate() {
        let profile = with_crate("tokio", Status::Known);
        let report = check(&profile, "src/main.rs", "", "use tokio::sync::Notify;\n");
        assert_eq!(report, CheckReport::Clean);
    }

    #[test]
    fn check_file_reports_a_seen_crate() {
        let profile = with_crate("tokio", Status::Seen);
        let report = check(&profile, "src/main.rs", "", "use tokio::sync::Notify;\n");
        assert_eq!(reported(&report), [(1, krate("tokio"))]);
    }

    #[test]
    fn check_file_takes_a_crate_known_under_its_hyphenated_name_as_known() {
        let profile = with_crate("serde-json", Status::Known);
        let report = check(&profile, "src/main.rs", "", "use serde_json::Value;\n");
        assert_eq!(report, CheckReport::Clean);
    }

    #[test]
    fn check_file_reports_a_crate_one_of_whose_spellings_is_not_known() {
        let profile = with_crates(&[("serde-json", Status::Known), ("serde_json", Status::Seen)]);
        let report = check(&profile, "src/main.rs", "", "use serde_json::Value;\n");
        assert_eq!(reported(&report), [(1, krate("serde_json"))]);
    }

    #[test]
    fn check_file_does_not_report_a_crate_of_the_projects_own_modules() {
        let own_modules: BTreeSet<String> = ["my_app".to_owned()].into();
        let profile = Profile::new();
        let ctx = CheckContext {
            profile: &profile,
            own_modules: &own_modules,
        };
        let report = check_file(Path::new("src/main.rs"), "", "use my_app::run;\n", &ctx);
        assert_eq!(report, CheckReport::Clean);
    }

    #[test]
    fn check_file_reports_crates_and_elements_in_line_order() {
        let new = "use tokio::spawn;\nfn f() {\n    dbg!(1);\n    anyhow::bail!(\"no\");\n}\n";
        let report = check(&Profile::new(), "src/lib.rs", "", new);
        assert_eq!(
            reported(&report),
            [
                (1, krate("tokio")),
                (3, ItemKey::Element(Element::DbgMacro)),
                (4, ItemKey::Element(Element::MacroCall)),
                (4, krate("anyhow")),
            ]
        );
    }

    #[test]
    fn check_file_does_not_report_a_crate_on_a_line_unchanged_from_old() {
        let old = "use tokio::spawn;\nfn f() {\n    a();\n}\n";
        let new = "use tokio::spawn;\nfn f() {\n    b();\n}\n";
        let report = check(&Profile::new(), "src/main.rs", old, new);
        assert_eq!(report, CheckReport::Clean);
    }

    #[test]
    fn check_file_reports_a_crate_on_a_new_line_even_if_existing_lines_use_it() {
        let old = "fn f() {\n    tokio::spawn(a());\n}\n";
        let new = "fn f() {\n    tokio::spawn(a());\n    tokio::spawn(b());\n}\n";
        let report = check(&Profile::new(), "src/main.rs", old, new);
        assert_eq!(reported(&report), [(3, krate("tokio"))]);
    }

    #[test]
    fn check_file_reports_a_name_added_to_an_existing_multi_line_use_of_a_crate() {
        let old = "use tokio::{\n    spawn,\n};\n";
        let new = "use tokio::{\n    spawn,\n    select,\n};\n";
        let report = check(&Profile::new(), "src/main.rs", old, new);
        assert_eq!(reported(&report), [(3, krate("tokio"))]);
    }

    #[test]
    fn a_multi_line_use_of_a_crate_is_reported_on_each_line_of_its_names() {
        let content =
            "use tokio::{\n    spawn,\n    sync::{\n        Mutex as M,\n        *,\n    },\n};\n";
        assert_eq!(
            source(content),
            [
                (1, krate("tokio")),
                (2, krate("tokio")),
                (3, krate("tokio")),
                (4, krate("tokio")),
                (5, krate("tokio")),
            ]
        );
    }

    #[test]
    fn check_file_compares_a_cargo_toml_by_dependency_keys_not_by_lines() {
        // Line 2 is unchanged, but `tokio` only now sits in a dependency table.
        let old = "[features]\ntokio = \"1\"\n";
        let new = "[dependencies]\ntokio = \"1\"\n";
        let report = check(&Profile::new(), "Cargo.toml", old, new);
        assert_eq!(reported(&report), [(2, krate("tokio"))]);
    }

    #[test]
    fn check_file_checks_a_cargo_toml_for_added_dependencies() {
        let old = "[dependencies]\nserde = \"1\"\n";
        let new = "[dependencies]\nserde = \"1\"\ntokio = \"1\"\n";
        let report = check(&Profile::new(), "Cargo.toml", old, new);
        assert_eq!(reported(&report), [(3, krate("tokio"))]);
    }

    #[test]
    fn check_file_does_not_report_a_known_crate_added_to_a_cargo_toml() {
        let profile = with_crate("tokio", Status::Known);
        let new = "[dependencies]\ntokio = \"1\"\n";
        let report = check(&profile, "crates/app/Cargo.toml", "", new);
        assert_eq!(report, CheckReport::Clean);
    }

    #[test]
    fn check_file_reports_a_cargo_toml_that_is_not_toml_as_a_parse_error() {
        // Rust that syn parses, but not TOML.
        let report = check(&Profile::new(), "Cargo.toml", "", "fn main() {}\n");
        assert!(matches!(report, CheckReport::ParseError(_)), "{report:?}");
    }
}
