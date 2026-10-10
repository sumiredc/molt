//! The modules depend on each other only in the directions of PRD §8.2.
//!
//! The allowed edges are read from the figure in PRD §8.2 itself, so the figure and the code
//! cannot drift apart. Every file under `src/` is read with syn, tests included, so a test cannot
//! pull a module upwards either. A dependency is a path that reaches a top-level module: through
//! `crate::`, or through as many `super::` as the module is deep, in `use` items, in other paths
//! and in the tokens of macro invocations, which syn does not parse. A glob of the crate root is
//! reported as a dependency on `*`. The figure is read as one `a --> b` per line, without labels
//! on the arrows or chains.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use proc_macro2::{Delimiter, TokenStream, TokenTree};
use syn::visit::{self, Visit};

/// The base, which every module above it may use (PRD §8.2). It is not drawn in the figure.
const BASE: [&str; 3] = ["config", "error", "ui"];

/// The dependencies inside the base.
const INSIDE_BASE: [(&str, &str); 1] = [("config", "error")];

/// The top, which may use every module directly (PRD §8.2).
const TOP: &str = "cli";

/// What a glob of the crate root depends on: any module, so it is never allowed.
const ROOT_GLOB: &str = "*";

type Edge = (String, String);

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// The edges of the mermaid figure under `### 8.2` in `document`.
fn figure_edges(document: &str) -> BTreeSet<Edge> {
    let text = fs::read_to_string(root().join(document)).unwrap();
    let section = text
        .split("\n### ")
        .find(|section| section.starts_with("8.2 "))
        .unwrap_or_else(|| panic!("{document} has no section 8.2"));
    let figure = section
        .split("```mermaid")
        .nth(1)
        .and_then(|rest| rest.split("```").next())
        .unwrap_or_else(|| panic!("section 8.2 of {document} has no mermaid figure"));
    let edges: BTreeSet<_> = figure
        .lines()
        .filter_map(|line| line.split_once("-->"))
        .map(|(from, to)| (node(from), node(to)))
        .collect();
    assert!(!edges.is_empty(), "the figure of {document} has no edges");
    edges
}

/// The ID of a node written as `id` or `id["label"]`.
fn node(text: &str) -> String {
    text.trim().split('[').next().unwrap().to_owned()
}

/// Each dependency between two top-level modules under `src/`, with the first file it was
/// seen in.
fn dependencies() -> BTreeSet<(String, String, String)> {
    let src = root().join("src");
    let mut found = BTreeSet::new();
    let mut seen = BTreeSet::new();
    for path in rust_files(&src) {
        let relative = path.strip_prefix(&src).unwrap();
        let mut components: Vec<_> = relative
            .components()
            .map(|component| component.as_os_str().to_str().unwrap().to_owned())
            .collect();
        let file = components.pop().unwrap();
        if components.is_empty() && (file == "lib.rs" || file == "main.rs") {
            continue;
        }
        if file != "mod.rs" {
            components.push(file.trim_end_matches(".rs").to_owned());
        }
        let module = components[0].clone();
        let syntax = syn::parse_file(&fs::read_to_string(&path).unwrap())
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        let mut paths = TopLevelPaths {
            depth: components.len(),
            modules: BTreeSet::new(),
        };
        paths.visit_file(&syntax);
        for target in paths.modules {
            if target != module && seen.insert((module.clone(), target.clone())) {
                found.insert((module.clone(), target, relative.display().to_string()));
            }
        }
    }
    found
}

fn rust_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files.extend(rust_files(&path));
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
    files.sort();
    files
}

/// The top-level modules the paths of one file reach.
struct TopLevelPaths {
    /// How deep the code being visited is below the crate root.
    depth: usize,
    modules: BTreeSet<String>,
}

impl TopLevelPaths {
    /// The top-level module `segments` reaches, when it starts at the crate root.
    fn reach(&mut self, segments: &[String]) {
        let supers = segments
            .iter()
            .take_while(|segment| *segment == "super")
            .count();
        let start = if segments.first().is_some_and(|segment| segment == "crate") {
            1
        } else if supers > 0 && supers == self.depth {
            supers
        } else {
            return;
        };
        // `use crate::{self, ..}` names the root itself, not a module.
        if let Some(module) = segments.get(start)
            && module != "self"
        {
            self.modules.insert(module.clone());
        }
    }

    /// Each path a `use` tree names, from its root to its last segment.
    fn use_paths(tree: &syn::UseTree, prefix: &mut Vec<String>, out: &mut Vec<Vec<String>>) {
        match tree {
            syn::UseTree::Path(path) => {
                prefix.push(path.ident.to_string());
                Self::use_paths(&path.tree, prefix, out);
                prefix.pop();
            }
            syn::UseTree::Name(name) => {
                let mut path = prefix.clone();
                path.push(name.ident.to_string());
                out.push(path);
            }
            syn::UseTree::Rename(rename) => {
                let mut path = prefix.clone();
                path.push(rename.ident.to_string());
                out.push(path);
            }
            syn::UseTree::Glob(_) => {
                // A glob of the crate root (`use crate::*;`) reaches modules without naming them.
                let mut path = prefix.clone();
                path.push(ROOT_GLOB.to_owned());
                out.push(path);
            }
            syn::UseTree::Group(group) => {
                for tree in &group.items {
                    Self::use_paths(tree, prefix, out);
                }
            }
        }
    }

    /// The paths in the tokens of a macro: `a::b::c`, and `a::{b, c}` as `a::b` and `a::c`.
    fn in_tokens(&mut self, tokens: TokenStream) {
        let tokens: Vec<_> = tokens.into_iter().collect();
        for (index, token) in tokens.iter().enumerate() {
            match token {
                TokenTree::Group(group) => self.in_tokens(group.stream()),
                TokenTree::Ident(ident)
                    if index < 2
                        || !(is_colon(&tokens[index - 1]) && is_colon(&tokens[index - 2])) =>
                {
                    let mut segments = vec![ident.to_string()];
                    let mut next = index + 1;
                    let mut braces = None;
                    while tokens.get(next).is_some_and(is_colon)
                        && tokens.get(next + 1).is_some_and(is_colon)
                    {
                        match tokens.get(next + 2) {
                            Some(TokenTree::Ident(segment)) => segments.push(segment.to_string()),
                            Some(TokenTree::Group(group))
                                if group.delimiter() == Delimiter::Brace =>
                            {
                                braces = Some(group.stream());
                                break;
                            }
                            _ => break,
                        }
                        next += 3;
                    }
                    match braces {
                        Some(stream) => {
                            for head in group_heads(stream) {
                                let mut path = segments.clone();
                                path.push(head);
                                self.reach(&path);
                            }
                        }
                        None => self.reach(&segments),
                    }
                }
                _ => {}
            }
        }
    }
}

fn is_colon(token: &TokenTree) -> bool {
    matches!(token, TokenTree::Punct(punct) if punct.as_char() == ':')
}

/// The first identifier of each comma-separated item in `{..}`.
fn group_heads(tokens: TokenStream) -> Vec<String> {
    let mut heads = Vec::new();
    let mut at_start = true;
    for token in tokens {
        match &token {
            TokenTree::Ident(head) if at_start => {
                heads.push(head.to_string());
                at_start = false;
            }
            TokenTree::Punct(punct) if punct.as_char() == ',' => at_start = true,
            _ => at_start = false,
        }
    }
    heads
}

impl<'ast> Visit<'ast> for TopLevelPaths {
    fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
        let mut paths = Vec::new();
        Self::use_paths(&item.tree, &mut Vec::new(), &mut paths);
        for path in paths {
            self.reach(&path);
        }
    }

    fn visit_path(&mut self, path: &'ast syn::Path) {
        let segments: Vec<_> = path
            .segments
            .iter()
            .map(|segment| segment.ident.to_string())
            .collect();
        self.reach(&segments);
        visit::visit_path(self, path);
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        self.in_tokens(mac.tokens.clone());
        visit::visit_macro(self, mac);
    }

    fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
        self.depth += 1;
        visit::visit_item_mod(self, item);
        self.depth -= 1;
    }
}

fn allowed(edges: &BTreeSet<Edge>, from: &str, to: &str) -> bool {
    if BASE.contains(&from) {
        return INSIDE_BASE.contains(&(from, to));
    }
    from == TOP || BASE.contains(&to) || edges.contains(&(from.to_owned(), to.to_owned()))
}

#[test]
fn every_dependency_between_modules_is_an_edge_of_prd_8_2_or_goes_to_the_base_or_from_cli() {
    let edges = figure_edges("PRD.md");
    let wrong: Vec<_> = dependencies()
        .into_iter()
        .filter(|(from, to, _)| !allowed(&edges, from, to))
        .map(|(from, to, file)| format!("{from} -> {to} (src/{file})"))
        .collect();
    assert!(
        wrong.is_empty(),
        "dependencies not in PRD §8.2 (move the code, or change the figure):\n{}",
        wrong.join("\n")
    );
}

#[test]
fn every_edge_of_prd_8_2_is_a_dependency_in_the_code() {
    let found: BTreeSet<Edge> = dependencies()
        .into_iter()
        .map(|(from, to, _)| (from, to))
        .collect();
    let unused: Vec<_> = figure_edges("PRD.md")
        .into_iter()
        .filter(|edge| !found.contains(edge))
        .map(|(from, to)| format!("{from} -> {to}"))
        .collect();
    assert!(
        unused.is_empty(),
        "edges of PRD §8.2 that no code uses (remove them from the figure):\n{}",
        unused.join("\n")
    );
}

#[test]
fn the_figure_of_prd_8_2_has_the_same_edges_in_english_and_japanese() {
    assert_eq!(figure_edges("PRD.ja.md"), figure_edges("PRD.md"));
}
