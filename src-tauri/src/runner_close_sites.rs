//! Runner close-site guard (`runner_close_sites.txt`) and the close-authority
//! mint scan (plan `2026-10-28-one-session-lifecycle-authority`, Phase 14;
//! architecture node R0a, §3.8, round items RV5-1, NC5-1 and NC6-3).
//!
//! Test-only: the whole module is `#[cfg(test)]` in `main.rs`. It is modelled
//! on `runner_spawn_sites.rs` and reuses that module's source walk.
//!
//! ## The roster
//!
//! Every non-test fn that reaches the close of a terminal pane must be listed in
//! `runner_close_sites.txt` with its class. A fn reaches a close when it calls
//! one of these close primitives:
//!
//! * `TerminalManager::close` or `close_all`;
//! * a session-level close (`TerminalSession::close`, `close_with_deadline`,
//!   `close_kill_only`, `close_after_graceful_exit`);
//! * the graceful-exit primitive (`TerminalManager::graceful_exit`);
//! * a relay end: the `remote_terminal_end` frame a source sends.
//!
//! Calling the call token of a `helper` row counts too.
//!
//! A new site is RED until it is classified. A row that names no current site is
//! red as a ghost. The classes are architecture §3.8's, and the roster file's
//! header says what each one means.
//!
//! An `operator` row claims that no automation can borrow that close. The
//! claim is checked two ways (NC5-1):
//!
//! * BY NAME against the `POST /ui-bridge/invoke/<name>` allowlist, whose
//!   dispatch runs the named fn.
//! * PER SITE against the `POST /ui-bridge/tauri/invoke` proxy, whose arms are
//!   their own implementations. No production code in `mcp/tauri_proxy.rs` may
//!   call an `operator` fn, and no fn of the proxy may itself be classed
//!   `operator`.
//!
//! A name check against the proxy's safelist would be wrong: its
//! `terminal_close` arm never calls the Tauri command of that name.
//!
//! ## The close-authority scan
//!
//! `terminal::close_authority::CloseAuthority` is what the graceful-exit
//! primitive requires before it types into a live `claude`. Only the lifecycle
//! Closer may ever obtain one (Phase 93), and from Phase 16 also the relay's
//! operator arm. [`AUTHORIZED_MINTERS`] and [`AUTHORIZED_CONSTRUCTORS`] are
//! empty until then. The scan pins five things:
//!
//! 1. the type's SHAPE: one definition, a private field, no constructing or
//!    duplicating derive;
//! 2. no trait impl for the type anywhere;
//! 3. inherent impls only in its own module;
//! 4. every fn that returns one is `#[cfg(test)]`, unless it is an authorized
//!    constructor;
//! 5. every production MINT SITE is authorized. A mint site is a struct literal,
//!    or a call to an associated fn of the type.
//!
//! So a public constructor, a public field, a `Default`, or a production call
//! of the test constructor is red in `cargo test` itself.
//!
//! ## How sites are found — a source scan, not a type check
//!
//! STRUCTURE comes from `syn`: every fn's line span, the self type of the impl
//! it sits in, and which items are `#[cfg(test)]` / `#[test]`. CALLS are found
//! by regex over the comment-stripped, test-stripped source, so a call inside a
//! macro body (which `syn` does not parse) is still seen. A site is named by
//! the innermost fn whose span holds the match.
//!
//! Known limits, stated so that a green run is read for what it is:
//!
//! * `TerminalManager::close` is recognised by its RECEIVER's shape:
//!   - a binding or field whose name contains `terminal_manager`;
//!   - one whose name is `tm`, `mgr` or `manager`, or one built on those with
//!     `_` (`close_tm`, `tm_clone`);
//!   - `get_terminal_manager(..)`;
//!   - `state::<Arc<TerminalManager>>()`, optionally `.inner()`;
//!   - `self` inside an `impl … TerminalManager`.
//!
//!   A manager bound to an unrelated name is not seen. The call must carry an
//!   argument, which `TerminalManager::close` always does.
//! * A zero-argument `.close()` reads as `TerminalSession::close` only on a
//!   receiver named `session` or `self`, and only under `terminal/`. The
//!   session-level entry points are `pub(super)`, callable only from
//!   `crate::terminal`, so a caller anywhere else does not compile —
//!   `the_session_level_close_entry_points_are_terminal_only` pins that.
//! * A test module declared out of line (`#[cfg(test)] mod tests;`) makes its
//!   file test code as a whole, and every out-of-line child of that file too.
//!   A `#[path = …]` on such a declaration is not followed, so a file it names
//!   reads as production.
//! * A `#[cfg(test)]` on a statement or expression inside a production fn is
//!   not recognised, so the code it guards reads as production. A comment that
//!   shares a line with code reads as code. All three err toward REPORTING a
//!   site, never toward hiding one.
//! * A green scan claims "every close this scan RECOGNISES is classified", not
//!   "every close path is classified".

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use regex::Regex;
use syn::spanned::Spanned;
use syn::visit::Visit;

use crate::runner_spawn_sites::{comment_mask, load_sources, token_re, Sources};

const SITES_REL: &str = "src/runner_close_sites.txt";
const SELF_FILE: &str = "runner_close_sites.rs";

/// Architecture §3.8's door classes.
const DOOR_CLASSES: &[&str] = &[
    "product",
    "operator",
    "webview",
    "agent",
    "relay",
    "runner_exit",
    "exempt",
];

/// The spawn roster's propagation row: a shared close recipe, not a door.
const HELPER: &str = "helper";

/// The close primitives' OWN bodies, as `(file, impl self type, fn)`. Each one
/// IS a close entry point: whoever calls it is the site, and its delegation to
/// a lower entry point is not. `every_primitive_impl_still_exists` keeps the
/// list honest.
const PRIMITIVE_IMPLS: &[(&str, &str, &str)] = &[
    ("terminal/manager.rs", "TerminalManager", "close"),
    ("terminal/manager.rs", "TerminalManager", "close_all"),
    ("terminal/manager.rs", "TerminalManager", "graceful_exit"),
    (
        "terminal/manager.rs",
        "TerminalManager",
        "close_after_graceful_exit",
    ),
    ("terminal/session.rs", "TerminalSession", "close"),
    (
        "terminal/session.rs",
        "TerminalSession",
        "close_with_deadline",
    ),
    ("terminal/session.rs", "TerminalSession", "close_kill_only"),
    (
        "terminal/session.rs",
        "TerminalSession",
        "close_after_graceful_exit",
    ),
    (
        "terminal/session.rs",
        "TerminalSession",
        "graceful_exit_then",
    ),
];

/// The session-level close entry points, which must stay terminal-only.
const SESSION_LEVEL_CLOSES: &[&str] = &["close", "close_with_deadline", "close_kill_only"];

/// The file that defines the close authority, and the type's name.
const AUTHORITY_FILE: &str = "terminal/close_authority.rs";
const AUTHORITY_TYPE: &str = "CloseAuthority";

/// Production fns that may CONSTRUCT a close authority (a struct literal, or a
/// fn returning one), as `(file, fn)`. EMPTY until the lifecycle Closer
/// (Phase 93 / R6) adds its mint here.
const AUTHORIZED_CONSTRUCTORS: &[(&str, &str)] = &[];

/// Production fns that may OBTAIN a close authority by calling a constructor,
/// as `(file, fn)`. EMPTY until the lifecycle Closer (Phase 93 / R6). From
/// Phase 16 the relay's operator arm may join it, for an end that carries the
/// relay's `origin: user_session` stamp.
const AUTHORIZED_MINTERS: &[(&str, &str)] = &[];

/// Derives that would construct or duplicate a close authority.
const FORBIDDEN_DERIVES: &[&str] = &["Default", "Clone", "Copy", "Deserialize"];

// ---------------------------------------------------------------------------
// Structure, from `syn`
// ---------------------------------------------------------------------------

/// One fn: its name, the lines it spans (1-based, inclusive), the self type of
/// the impl it sits in, whether it is test code, and the identifiers its return
/// type names.
#[derive(Debug, Clone)]
struct FnSpan {
    name: String,
    start: usize,
    end: usize,
    impl_self: Option<String>,
    in_test: bool,
    output_idents: Vec<String>,
    vis: Vis,
}

/// A fn's visibility, as far as the scan needs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Vis {
    Private,
    Super,
    Other,
}

/// One impl block.
#[derive(Debug, Clone)]
struct ImplBlock {
    trait_name: Option<String>,
    self_ty: Option<String>,
    in_test: bool,
}

/// One struct definition.
#[derive(Debug, Clone)]
struct StructDef {
    name: String,
    in_test: bool,
    field_count: usize,
    public_fields: usize,
    derives: Vec<String>,
}

/// One out-of-line `mod name;` declaration, and whether it is test code (the
/// declaration is `#[cfg(test)]`, or sits in test code).
#[derive(Debug, Clone)]
struct ChildMod {
    name: String,
    test: bool,
}

#[derive(Debug, Default)]
struct Structure {
    fns: Vec<FnSpan>,
    test_spans: Vec<(usize, usize)>,
    impls: Vec<ImplBlock>,
    structs: Vec<StructDef>,
    child_mods: Vec<ChildMod>,
}

impl Structure {
    /// Mark the whole file as test code: it is the out-of-line body of a
    /// `#[cfg(test)]` module (see [`test_only_files`]).
    fn mark_all_test(&mut self, line_count: usize) {
        self.test_spans = vec![(1, line_count.max(1))];
        self.fns.iter_mut().for_each(|f| f.in_test = true);
        self.impls.iter_mut().for_each(|i| i.in_test = true);
        self.structs.iter_mut().for_each(|s| s.in_test = true);
        self.child_mods.iter_mut().for_each(|m| m.test = true);
    }

    fn in_test_span(&self, line: usize) -> bool {
        self.test_spans.iter().any(|&(s, e)| line >= s && line <= e)
    }

    /// The innermost fn whose span holds `line`.
    fn innermost_fn(&self, line: usize) -> Option<&FnSpan> {
        self.fns
            .iter()
            .filter(|f| line >= f.start && line <= f.end)
            .min_by_key(|f| f.end - f.start)
    }
}

fn lines_of<T: Spanned>(node: &T) -> (usize, usize) {
    let span = node.span();
    (span.start().line, span.end().line)
}

/// `#[test]`, `#[tokio::test(…)]`, or exactly `#[cfg(test)]`. A compound cfg
/// (`any(test, …)`, `not(test)`) is NOT test scope: the code it guards exists in
/// some non-test build.
fn is_test_attr(attr: &syn::Attribute) -> bool {
    let path = attr.path();
    if path.is_ident("test") {
        return true;
    }
    if path.segments.len() == 2 && path.segments.last().is_some_and(|s| s.ident == "test") {
        return true;
    }
    if path.is_ident("cfg") {
        if let syn::Meta::List(list) = &attr.meta {
            return list.tokens.to_string().trim() == "test";
        }
    }
    false
}

fn last_ident_of_type(ty: &syn::Type) -> Option<String> {
    match ty {
        syn::Type::Path(p) => p.path.segments.last().map(|s| s.ident.to_string()),
        syn::Type::Reference(r) => last_ident_of_type(&r.elem),
        _ => None,
    }
}

/// Every identifier a syntax node names.
#[derive(Default)]
struct Idents(Vec<String>);

impl<'ast> Visit<'ast> for Idents {
    fn visit_ident(&mut self, ident: &'ast proc_macro2::Ident) {
        self.0.push(ident.to_string());
    }
}

fn vis_of(vis: &syn::Visibility) -> Vis {
    match vis {
        syn::Visibility::Inherited => Vis::Private,
        syn::Visibility::Restricted(r) if r.path.is_ident("super") => Vis::Super,
        _ => Vis::Other,
    }
}

fn derives_of(attrs: &[syn::Attribute]) -> Vec<String> {
    let mut out = Vec::new();
    for attr in attrs.iter().filter(|a| a.path().is_ident("derive")) {
        if let Ok(paths) = attr.parse_args_with(
            syn::punctuated::Punctuated::<syn::Path, syn::Token![,]>::parse_terminated,
        ) {
            out.extend(
                paths
                    .iter()
                    .filter_map(|p| p.segments.last().map(|s| s.ident.to_string())),
            );
        }
    }
    out
}

#[derive(Default)]
struct StructureVisitor {
    s: Structure,
    test_depth: usize,
    impl_stack: Vec<Option<String>>,
}

impl StructureVisitor {
    fn scoped<N: Spanned>(
        &mut self,
        attrs: &[syn::Attribute],
        node: &N,
        body: impl FnOnce(&mut Self),
    ) {
        let test = attrs.iter().any(is_test_attr);
        if test {
            self.s.test_spans.push(lines_of(node));
            self.test_depth += 1;
        }
        body(self);
        if test {
            self.test_depth -= 1;
        }
    }

    fn record_fn<N: Spanned>(
        &mut self,
        sig: &syn::Signature,
        vis: Vis,
        node: &N,
        impl_self: Option<String>,
    ) {
        let (start, end) = lines_of(node);
        let mut output = Idents::default();
        output.visit_return_type(&sig.output);
        self.s.fns.push(FnSpan {
            name: sig.ident.to_string(),
            start,
            end,
            impl_self,
            in_test: self.test_depth > 0,
            output_idents: output.0,
            vis,
        });
    }
}

impl<'ast> Visit<'ast> for StructureVisitor {
    fn visit_item_mod(&mut self, m: &'ast syn::ItemMod) {
        self.scoped(&m.attrs, m, |v| {
            if m.content.is_none() {
                v.s.child_mods.push(ChildMod {
                    name: m.ident.to_string(),
                    test: v.test_depth > 0,
                });
            }
            syn::visit::visit_item_mod(v, m);
        });
    }
    fn visit_item_fn(&mut self, f: &'ast syn::ItemFn) {
        self.scoped(&f.attrs, f, |v| {
            v.record_fn(&f.sig, vis_of(&f.vis), f, None);
            // A fn nested in a method is not in the method's impl.
            v.impl_stack.push(None);
            syn::visit::visit_item_fn(v, f);
            v.impl_stack.pop();
        });
    }
    fn visit_item_impl(&mut self, i: &'ast syn::ItemImpl) {
        self.scoped(&i.attrs, i, |v| {
            let self_ty = last_ident_of_type(&i.self_ty);
            v.s.impls.push(ImplBlock {
                trait_name: i
                    .trait_
                    .as_ref()
                    .and_then(|(_, p, _)| p.segments.last())
                    .map(|s| s.ident.to_string()),
                self_ty: self_ty.clone(),
                in_test: v.test_depth > 0,
            });
            v.impl_stack.push(self_ty);
            syn::visit::visit_item_impl(v, i);
            v.impl_stack.pop();
        });
    }
    fn visit_impl_item_fn(&mut self, f: &'ast syn::ImplItemFn) {
        self.scoped(&f.attrs, f, |v| {
            let impl_self = v.impl_stack.last().cloned().flatten();
            v.record_fn(&f.sig, vis_of(&f.vis), f, impl_self);
            syn::visit::visit_impl_item_fn(v, f);
        });
    }
    fn visit_trait_item_fn(&mut self, f: &'ast syn::TraitItemFn) {
        self.scoped(&f.attrs, f, |v| {
            v.record_fn(&f.sig, Vis::Other, f, None);
            syn::visit::visit_trait_item_fn(v, f);
        });
    }
    fn visit_item_struct(&mut self, s: &'ast syn::ItemStruct) {
        self.scoped(&s.attrs, s, |v| {
            let (field_count, public_fields) = match &s.fields {
                syn::Fields::Named(n) => (
                    n.named.len(),
                    n.named
                        .iter()
                        .filter(|f| !matches!(f.vis, syn::Visibility::Inherited))
                        .count(),
                ),
                syn::Fields::Unnamed(u) => (
                    u.unnamed.len(),
                    u.unnamed
                        .iter()
                        .filter(|f| !matches!(f.vis, syn::Visibility::Inherited))
                        .count(),
                ),
                syn::Fields::Unit => (0, 0),
            };
            v.s.structs.push(StructDef {
                name: s.ident.to_string(),
                in_test: v.test_depth > 0,
                field_count,
                public_fields,
                derives: derives_of(&s.attrs),
            });
            syn::visit::visit_item_struct(v, s);
        });
    }
    fn visit_item_const(&mut self, c: &'ast syn::ItemConst) {
        self.scoped(&c.attrs, c, |v| syn::visit::visit_item_const(v, c));
    }
    fn visit_item_static(&mut self, s: &'ast syn::ItemStatic) {
        self.scoped(&s.attrs, s, |v| syn::visit::visit_item_static(v, s));
    }
    fn visit_item_use(&mut self, u: &'ast syn::ItemUse) {
        self.scoped(&u.attrs, u, |v| syn::visit::visit_item_use(v, u));
    }
    fn visit_item_enum(&mut self, e: &'ast syn::ItemEnum) {
        self.scoped(&e.attrs, e, |v| syn::visit::visit_item_enum(v, e));
    }
    fn visit_item_trait(&mut self, t: &'ast syn::ItemTrait) {
        self.scoped(&t.attrs, t, |v| syn::visit::visit_item_trait(v, t));
    }
    fn visit_item_macro(&mut self, m: &'ast syn::ItemMacro) {
        self.scoped(&m.attrs, m, |v| syn::visit::visit_item_macro(v, m));
    }
}

fn structure_of(file: &str, src: &str) -> Structure {
    let ast = syn::parse_file(src)
        .unwrap_or_else(|e| panic!("{file} does not parse, so it cannot be scanned: {e}"));
    let mut v = StructureVisitor::default();
    v.visit_file(&ast);
    v.s
}

// ---------------------------------------------------------------------------
// Prepared sources
// ---------------------------------------------------------------------------

/// One file, ready to scan: the source with comment lines and test spans
/// blanked (line numbering kept), plus its structure.
struct Prepared {
    file: String,
    code: String,
    line_starts: Vec<usize>,
    structure: Structure,
}

impl Prepared {
    /// The 1-based line holding byte `offset` of `code`.
    fn line_of(&self, offset: usize) -> usize {
        self.line_starts.partition_point(|&s| s <= offset)
    }
}

/// One file, prepared on its own. Whether the whole file is a test module's
/// out-of-line body is a fact about OTHER files, which only [`prepare_all`]
/// sees.
fn prepare(file: &str, src: &str) -> Prepared {
    masked(file, src, structure_of(file, src))
}

fn masked(file: &str, src: &str, structure: Structure) -> Prepared {
    let lines: Vec<&str> = src.lines().collect();
    let comments = comment_mask(&lines);
    let masked: Vec<&str> = lines
        .iter()
        .enumerate()
        .map(|(i, l)| {
            if comments[i] || structure.in_test_span(i + 1) {
                ""
            } else {
                *l
            }
        })
        .collect();
    let code = masked.join("\n");
    let mut line_starts = vec![0usize];
    line_starts.extend(code.match_indices('\n').map(|(i, _)| i + 1));
    Prepared {
        file: file.to_string(),
        code,
        line_starts,
        structure,
    }
}

fn prepare_all(sources: &Sources) -> Vec<Prepared> {
    let mut parsed: Vec<(&str, &str, Structure)> = sources
        .iter()
        .filter(|(file, _)| file.as_str() != SELF_FILE)
        .map(|(file, src)| (file.as_str(), src.as_str(), structure_of(file, src)))
        .collect();
    let test_files = test_only_files(&parsed);
    for (file, src, structure) in &mut parsed {
        if test_files.contains(*file) {
            structure.mark_all_test(src.lines().count());
        }
    }
    parsed
        .into_iter()
        .map(|(file, src, structure)| masked(file, src, structure))
        .collect()
}

/// The files `file`'s out-of-line child module `name` can live in.
fn child_module_files(file: &str, name: &str) -> [String; 2] {
    let (dir, leaf) = file.rsplit_once('/').unwrap_or(("", file));
    let base = if matches!(leaf, "mod.rs" | "lib.rs" | "main.rs") {
        dir.to_string()
    } else {
        let stem = leaf.strip_suffix(".rs").unwrap_or(leaf);
        if dir.is_empty() {
            stem.to_string()
        } else {
            format!("{dir}/{stem}")
        }
    };
    let under = |rest: String| {
        if base.is_empty() {
            rest
        } else {
            format!("{base}/{rest}")
        }
    };
    [under(format!("{name}.rs")), under(format!("{name}/mod.rs"))]
}

/// Files that are test code as a whole: the out-of-line body of a
/// `#[cfg(test)] mod name;`, and, transitively, every out-of-line child of such
/// a file. Scanned line by line, they would read as production: `syn` sees
/// their items, never the `#[cfg(test)]` on the declaration in the parent.
fn test_only_files(parsed: &[(&str, &str, Structure)]) -> BTreeSet<String> {
    let by_file: BTreeMap<&str, &Structure> = parsed.iter().map(|(f, _, s)| (*f, s)).collect();
    let mut work: Vec<String> = parsed
        .iter()
        .flat_map(|(file, _, s)| {
            s.child_mods
                .iter()
                .filter(|m| m.test)
                .flat_map(move |m| child_module_files(file, &m.name))
        })
        .collect();
    let mut test = BTreeSet::new();
    while let Some(file) = work.pop() {
        let Some(structure) = by_file.get(file.as_str()) else {
            continue;
        };
        if !test.insert(file.clone()) {
            continue;
        }
        work.extend(
            structure
                .child_mods
                .iter()
                .flat_map(|m| child_module_files(&file, &m.name)),
        );
    }
    test
}

// ---------------------------------------------------------------------------
// The close primitives
// ---------------------------------------------------------------------------

/// Where a pattern applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    Anywhere,
    /// Only under `terminal/`, where the session-level entry points live.
    TerminalModule,
    /// Only inside an `impl … TerminalManager`, where `self` is the manager.
    ImplOfTerminalManager,
}

struct Pattern {
    re: Regex,
    scope: Scope,
}

fn pat(re: &str, scope: Scope) -> Pattern {
    Pattern {
        re: Regex::new(re).expect("close-site regex"),
        scope,
    }
}

/// The close primitives, grouped by the FAMILY each pattern detects, so the
/// vacuity floor can say which family went blind (see the spawn roster's
/// `primitive_families`).
fn close_families() -> Vec<(&'static str, Vec<Pattern>)> {
    // A manager-shaped receiver: `tm`, `mgr`, `manager`, and names built on
    // them with `_` (`close_tm`, `tm_clone`, `graceful_tm`).
    const MGR: &str = r"\b(?:[A-Za-z0-9]+_)*(?:tm|mgr|manager)(?:_[A-Za-z0-9]+)*";
    vec![
        (
            "TerminalManager::close",
            vec![
                pat(
                    r"\b[A-Za-z_]*terminal_manager[A-Za-z0-9_]*\s*\.\s*close\s*\(\s*[^)\s]",
                    Scope::Anywhere,
                ),
                pat(
                    &format!(r"{MGR}\s*\.\s*close\s*\(\s*[^)\s]"),
                    Scope::Anywhere,
                ),
                pat(
                    &format!(r"{MGR}\s*\.\s*inner\s*\(\s*\)\s*\.\s*close\s*\(\s*[^)\s]"),
                    Scope::Anywhere,
                ),
                pat(
                    r"\bget_terminal_manager\s*\([^)]*\)\s*\.\s*close\s*\(",
                    Scope::Anywhere,
                ),
                pat(
                    r"TerminalManager\s*>+\s*\(\s*\)\s*(?:\.\s*inner\s*\(\s*\)\s*)?\.\s*close\s*\(",
                    Scope::Anywhere,
                ),
                pat(
                    r"\bself\s*\.\s*close\s*\(\s*[^)\s]",
                    Scope::ImplOfTerminalManager,
                ),
            ],
        ),
        (
            "TerminalManager::close_all",
            vec![pat(r"\.\s*close_all\s*\(", Scope::Anywhere)],
        ),
        (
            "TerminalManager::graceful_exit",
            vec![
                pat(r"\.\s*graceful_exit\s*\(", Scope::Anywhere),
                pat(r"\.\s*graceful_exit_then\s*\(", Scope::Anywhere),
                pat(r"\.\s*graceful_exit_driven\s*\(", Scope::Anywhere),
            ],
        ),
        (
            "session-level close",
            vec![
                pat(r"\.\s*close_with_deadline\s*\(", Scope::Anywhere),
                pat(r"\.\s*close_kill_only\s*\(", Scope::Anywhere),
                pat(r"\.\s*close_after_graceful_exit\s*\(", Scope::Anywhere),
                pat(
                    r"\b(?:session|self)\s*\.\s*close\s*\(\s*\)",
                    Scope::TerminalModule,
                ),
            ],
        ),
        (
            "relay end",
            vec![pat(
                r#""type"\s*:\s*"remote_terminal_end""#,
                Scope::Anywhere,
            )],
        ),
    ]
}

/// Is the match at `offset` the NAME in a fn declaration (`fn foo(`)?
fn is_declaration(code: &str, offset: usize) -> bool {
    code.get(..offset)
        .unwrap_or("")
        .trim_end()
        .strip_suffix("fn")
        .is_some_and(|rest| {
            rest.chars()
                .next_back()
                .is_none_or(|c| !(c.is_alphanumeric() || c == '_'))
        })
}

fn in_scope(scope: Scope, file: &str, f: Option<&FnSpan>) -> bool {
    match scope {
        Scope::Anywhere => true,
        Scope::TerminalModule => file.starts_with("terminal/"),
        Scope::ImplOfTerminalManager => {
            f.and_then(|f| f.impl_self.as_deref()) == Some("TerminalManager")
        }
    }
}

fn is_primitive_impl(file: &str, f: &FnSpan) -> bool {
    PRIMITIVE_IMPLS
        .iter()
        .any(|&(pf, ps, pn)| pf == file && pn == f.name && f.impl_self.as_deref() == Some(ps))
}

/// `(file, fn)` for every production match of any pattern.
fn scan_calls(prepared: &[Prepared], patterns: &[Pattern]) -> BTreeSet<(String, String)> {
    let mut out = BTreeSet::new();
    for p in prepared {
        for pattern in patterns {
            for m in pattern.re.find_iter(&p.code) {
                if is_declaration(&p.code, m.start()) {
                    continue;
                }
                let line = p.line_of(m.start());
                let f = p.structure.innermost_fn(line);
                if !in_scope(pattern.scope, &p.file, f) {
                    continue;
                }
                if f.is_some_and(|f| is_primitive_impl(&p.file, f)) {
                    continue;
                }
                let name = f.map_or_else(|| "<file scope>".to_string(), |f| f.name.clone());
                out.insert((p.file.clone(), name));
            }
        }
    }
    out
}

/// A helper row's call token as a regex that tolerates whitespace and line
/// breaks around its punctuation. A call that rustfmt has split across lines
/// (`client()` then `.end(` on the next) is therefore still seen.
fn helper_token_re(token: &str) -> Regex {
    let mut re = String::new();
    if token.starts_with(|c: char| c.is_alphanumeric() || c == '_') {
        re.push_str(r"\b");
    }
    for c in token.chars() {
        if c.is_alphanumeric() || c == '_' {
            re.push(c);
        } else {
            re.push_str(r"\s*");
            re.push_str(&regex::escape(&c.to_string()));
            re.push_str(r"\s*");
        }
    }
    Regex::new(&re).expect("helper token regex")
}

/// Every site: callers of the primitives, then callers of each `helper` row's
/// token.
fn scan_sites(prepared: &[Prepared], rows: &Rows) -> BTreeSet<(String, String)> {
    let mut patterns: Vec<Pattern> = close_families()
        .into_iter()
        .flat_map(|(_, pats)| pats)
        .collect();
    patterns.extend(
        rows.values()
            .filter(|r| r.class == HELPER)
            .map(|r| Pattern {
                re: helper_token_re(&r.detail),
                scope: Scope::Anywhere,
            }),
    );
    scan_calls(prepared, &patterns)
}

// ---------------------------------------------------------------------------
// The roster file
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
struct Row {
    class: String,
    detail: String,
}

type Rows = BTreeMap<(String, String), Row>;

fn parse_rows(text: &str) -> Rows {
    let mut out = BTreeMap::new();
    for (n, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let cells: Vec<&str> = line.splitn(4, '|').map(str::trim).collect();
        assert_eq!(
            cells.len(),
            4,
            "runner_close_sites.txt:{}: expected `file | fn | class | detail`, got {line:?}",
            n + 1
        );
        assert!(
            DOOR_CLASSES.contains(&cells[2]) || cells[2] == HELPER,
            "runner_close_sites.txt:{}: class {:?} is none of {DOOR_CLASSES:?} or `{HELPER}`",
            n + 1,
            cells[2]
        );
        assert!(
            !cells[3].is_empty() && cells[3] != "-",
            "runner_close_sites.txt:{}: a row must say {}",
            n + 1,
            if cells[2] == HELPER {
                "the call token its callers use"
            } else {
                "what the close is, and for `exempt` why it ends no session's work"
            }
        );
        let prev = out.insert(
            (cells[0].to_string(), cells[1].to_string()),
            Row {
                class: cells[2].to_string(),
                detail: cells[3].to_string(),
            },
        );
        assert!(
            prev.is_none(),
            "runner_close_sites.txt:{}: duplicate row",
            n + 1
        );
    }
    out
}

fn load_rows() -> Rows {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(SITES_REL);
    parse_rows(&std::fs::read_to_string(path).expect("read runner_close_sites.txt"))
}

/// What the roster check found wrong.
#[derive(Debug, Default, PartialEq, Eq)]
struct RosterVerdict {
    unlisted: Vec<(String, String)>,
    ghosts: Vec<(String, String)>,
}

fn roster_verdict(prepared: &[Prepared], rows: &Rows) -> RosterVerdict {
    let found = scan_sites(prepared, rows);
    let listed: BTreeSet<(String, String)> = rows.keys().cloned().collect();
    RosterVerdict {
        unlisted: found.difference(&listed).cloned().collect(),
        ghosts: listed.difference(&found).cloned().collect(),
    }
}

/// `operator` rows an automation can borrow (NC5-1), each with the route.
fn borrowable_operator_rows(prepared: &[Prepared], rows: &Rows) -> Vec<String> {
    let proxy = prepared
        .iter()
        .find(|p| p.file == "mcp/tauri_proxy.rs")
        .expect("the UI Bridge Tauri proxy (mcp/tauri_proxy.rs) is part of the scan");
    let mut out = Vec::new();
    for ((file, func), _) in rows.iter().filter(|(_, r)| r.class == "operator") {
        if crate::ui_bridge_invoke::is_allowlisted(func) {
            out.push(format!(
                "{file}:{func} is on the POST /ui-bridge/invoke/<name> allowlist"
            ));
        }
        if file == "mcp/tauri_proxy.rs" {
            out.push(format!(
                "{file}:{func} is a fn of the POST /ui-bridge/tauri/invoke proxy itself"
            ));
        } else if token_re(&format!("{func}("))
            .find_iter(&proxy.code)
            .any(|m| !is_declaration(&proxy.code, m.start()))
        {
            out.push(format!(
                "{file}:{func} is called by the POST /ui-bridge/tauri/invoke proxy"
            ));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// The close-authority scan
// ---------------------------------------------------------------------------

/// Everything wrong with how a close authority can be obtained.
fn close_authority_violations(prepared: &[Prepared]) -> Vec<String> {
    let mut out = Vec::new();

    // (1) Shape: exactly one definition, in its own module, sealed.
    let defs: Vec<(&str, &StructDef)> = prepared
        .iter()
        .flat_map(|p| {
            p.structure
                .structs
                .iter()
                .filter(|s| s.name == AUTHORITY_TYPE && !s.in_test)
                .map(move |s| (p.file.as_str(), s))
        })
        .collect();
    match defs.as_slice() {
        [(file, def)] => {
            if *file != AUTHORITY_FILE {
                out.push(format!(
                    "{AUTHORITY_TYPE} is defined in {file}, not {AUTHORITY_FILE}"
                ));
            }
            if def.field_count == 0 {
                out.push(format!(
                    "{AUTHORITY_TYPE} has no field, so any module can build one by naming it"
                ));
            }
            if def.public_fields > 0 {
                out.push(format!(
                    "{AUTHORITY_TYPE} has a non-private field, so another module can build one"
                ));
            }
            for d in &def.derives {
                if FORBIDDEN_DERIVES.contains(&d.as_str()) {
                    out.push(format!(
                        "{AUTHORITY_TYPE} derives {d}, which builds or duplicates an authority"
                    ));
                }
            }
        }
        [] => out.push(format!(
            "no {AUTHORITY_TYPE} definition found — the scan is vacuous"
        )),
        many => out.push(format!(
            "{} {AUTHORITY_TYPE} definitions: {:?}",
            many.len(),
            many.iter().map(|(f, _)| *f).collect::<Vec<_>>()
        )),
    }

    for p in prepared {
        // (2) No trait impl, and (3) inherent impls only in its own module.
        for i in p.structure.impls.iter().filter(|i| !i.in_test) {
            if i.self_ty.as_deref() != Some(AUTHORITY_TYPE) {
                continue;
            }
            match &i.trait_name {
                Some(t) => out.push(format!(
                    "{}: impl {t} for {AUTHORITY_TYPE} — no trait may build, duplicate or convert into one",
                    p.file
                )),
                None if p.file != AUTHORITY_FILE => out.push(format!(
                    "{}: an inherent impl of {AUTHORITY_TYPE} outside {AUTHORITY_FILE}",
                    p.file
                )),
                None => {}
            }
        }

        // (4) Every production fn that returns one is an authorized constructor.
        for f in p.structure.fns.iter().filter(|f| !f.in_test) {
            let returns_one = f.output_idents.iter().any(|i| i == AUTHORITY_TYPE)
                || (f.impl_self.as_deref() == Some(AUTHORITY_TYPE)
                    && f.output_idents.iter().any(|i| i == "Self"));
            let authorized = AUTHORIZED_CONSTRUCTORS
                .iter()
                .any(|&(file, func)| file == p.file && func == f.name);
            if returns_one && !authorized {
                out.push(format!(
                    "{}:{} returns a {AUTHORITY_TYPE} outside #[cfg(test)] — only the lifecycle Closer may mint one",
                    p.file, f.name
                ));
            }
        }

        // (5) Every production mint site is authorized.
        out.extend(unauthorized_mint_sites(p));
    }
    out
}

/// Production struct literals of the authority, and calls of its associated
/// fns, that no authorized fn makes.
fn unauthorized_mint_sites(p: &Prepared) -> Vec<String> {
    let literal = Regex::new(&format!(r"\b{AUTHORITY_TYPE}\s*\{{")).expect("literal regex");
    let self_literal = Regex::new(r"\bSelf\s*\{").expect("Self literal regex");
    let call = Regex::new(&format!(
        r"\b{AUTHORITY_TYPE}\s*::\s*[A-Za-z_][A-Za-z0-9_]*\s*\("
    ))
    .expect("call regex");
    // `struct X {`, `impl X {`, `impl T for X {` and their path-qualified forms
    // are declarations, not literals.
    let declaration = Regex::new(&format!(
        r"\b(?:struct|impl|for)\s+(?:[A-Za-z_][A-Za-z0-9_]*::)*{AUTHORITY_TYPE}\b"
    ))
    .expect("declaration regex");
    type Allowed = &'static [(&'static str, &'static str)];
    let mut sites: Vec<(usize, &'static str, Allowed)> = Vec::new();
    // `-> CloseAuthority {` / `-> Self {` is a fn signature meeting its body,
    // not a literal; check (4) judges such a fn as a constructor already.
    let after_arrow = |offset: usize| {
        p.code
            .get(..offset)
            .unwrap_or("")
            .trim_end()
            .ends_with("->")
    };
    for m in literal.find_iter(&p.code) {
        let line_start = p.line_starts[p.line_of(m.start()) - 1];
        let line_text = p.code.get(line_start..m.end()).unwrap_or("");
        if !declaration.is_match(line_text) && !after_arrow(m.start()) {
            sites.push((
                m.start(),
                "builds one with a struct literal",
                AUTHORIZED_CONSTRUCTORS,
            ));
        }
    }
    for m in self_literal.find_iter(&p.code) {
        let f = p.structure.innermost_fn(p.line_of(m.start()));
        if f.and_then(|f| f.impl_self.as_deref()) == Some(AUTHORITY_TYPE) && !after_arrow(m.start())
        {
            sites.push((
                m.start(),
                "builds one with `Self { .. }`",
                AUTHORIZED_CONSTRUCTORS,
            ));
        }
    }
    for m in call.find_iter(&p.code) {
        sites.push((
            m.start(),
            "calls one of its associated fns",
            AUTHORIZED_MINTERS,
        ));
    }
    sites
        .into_iter()
        .filter_map(|(offset, what, allowed)| {
            let line = p.line_of(offset);
            let name = p
                .structure
                .innermost_fn(line)
                .map_or("<file scope>", |f| f.name.as_str());
            let authorized = allowed
                .iter()
                .any(|&(file, func)| file == p.file && func == name);
            (!authorized).then(|| {
                format!(
                    "{}:{line} ({name}) {what} — only the lifecycle Closer may obtain a \
                     {AUTHORITY_TYPE}",
                    p.file
                )
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Tests over the real tree
// ---------------------------------------------------------------------------

/// The real source tree, parsed once per test process.
fn real_tree() -> &'static [Prepared] {
    static TREE: std::sync::OnceLock<Vec<Prepared>> = std::sync::OnceLock::new();
    TREE.get_or_init(|| prepare_all(&load_sources()))
}

#[test]
fn every_close_site_is_listed_and_no_row_is_a_ghost() {
    let prepared = real_tree();
    let rows = load_rows();

    // Per-FAMILY coverage, not one lump total: a family whose patterns stop
    // matching would otherwise hide behind the others' count.
    let blind: Vec<&str> = close_families()
        .into_iter()
        .filter(|(_, pats)| scan_calls(prepared, pats).is_empty())
        .map(|(name, _)| name)
        .collect();
    assert!(
        blind.is_empty(),
        "these close primitives matched NOTHING — the scan has gone blind to a whole \
         family: {blind:?}"
    );
    let found = scan_sites(prepared, &rows);
    assert!(
        found.len() >= 25,
        "found only {} close sites — scan broken?",
        found.len()
    );

    let verdict = roster_verdict(prepared, &rows);
    assert!(
        verdict.unlisted.is_empty(),
        "close sites with no runner_close_sites.txt row — classify each `file | fn | class | \
         detail` (architecture §3.8's classes; the file's header says which is which): {:#?}",
        verdict.unlisted
    );
    assert!(
        verdict.ghosts.is_empty(),
        "rows naming no current close site — delete them: {:#?}",
        verdict.ghosts
    );
}

#[test]
fn no_operator_close_is_borrowable_through_either_ui_bridge_route() {
    let violations = borrowable_operator_rows(real_tree(), &load_rows());
    assert!(
        violations.is_empty(),
        "an `operator` row claims no automation can borrow the close, and these can: \
         {violations:#?}"
    );
}

#[test]
fn every_primitive_impl_still_exists() {
    let prepared = real_tree();
    for &(file, self_ty, name) in PRIMITIVE_IMPLS {
        let p = prepared
            .iter()
            .find(|p| p.file == file)
            .unwrap_or_else(|| panic!("{file} is gone"));
        assert!(
            p.structure
                .fns
                .iter()
                .any(|f| f.name == name && f.impl_self.as_deref() == Some(self_ty) && !f.in_test),
            "PRIMITIVE_IMPLS names {self_ty}::{name} in {file}, which no longer exists — \
             update the list"
        );
    }
}

/// NC6-3's premise. The scan reads `session.close()` / `self.close()` as
/// `TerminalSession::close` only under `terminal/`, which is sound only while
/// no other module CAN call a session-level close. These entry points stay
/// `pub(super)` (or private).
#[test]
fn the_session_level_close_entry_points_are_terminal_only() {
    let prepared = real_tree();
    let session = prepared
        .iter()
        .find(|p| p.file == "terminal/session.rs")
        .expect("terminal/session.rs");
    for name in SESSION_LEVEL_CLOSES {
        let f = session
            .structure
            .fns
            .iter()
            .find(|f| {
                f.name == *name && f.impl_self.as_deref() == Some("TerminalSession") && !f.in_test
            })
            .unwrap_or_else(|| panic!("TerminalSession::{name} is gone — update the list"));
        assert!(
            matches!(f.vis, Vis::Private | Vis::Super),
            "TerminalSession::{name} is callable outside crate::terminal ({:?}), where the \
             close-site scan cannot see its callers — keep it `pub(super)`",
            f.vis
        );
    }
}

/// The structure comes from a real parse with real spans, so a multi-line fn
/// spans many lines. If span joining ever broke, every fn would span one line
/// and every site would be attributed to `<file scope>`.
#[test]
fn fn_spans_come_from_the_parse() {
    let prepared = real_tree();
    let manager = prepared
        .iter()
        .find(|p| p.file == "terminal/manager.rs")
        .expect("terminal/manager.rs");
    let graceful = manager
        .structure
        .fns
        .iter()
        .find(|f| f.name == "graceful_exit" && f.impl_self.as_deref() == Some("TerminalManager"))
        .expect("TerminalManager::graceful_exit");
    assert!(
        graceful.end - graceful.start > 40,
        "TerminalManager::graceful_exit spans {}..{} — span joining is broken",
        graceful.start,
        graceful.end
    );
}

#[test]
fn close_authority_has_no_constructor_outside_test_code() {
    let violations = close_authority_violations(real_tree());
    assert!(
        violations.is_empty(),
        "a CloseAuthority can be obtained outside the lifecycle Closer: {violations:#?}"
    );
}

/// The roster names the source scan's own subject. The mint scan above checks
/// every module; this checks that it actually SAW the type and its test
/// constructor, so an empty result is a real "nothing mints", not a scan that
/// found nothing to look at.
#[test]
fn no_module_but_the_closer_can_obtain_a_close_authority() {
    let prepared = real_tree();
    let authority = prepared
        .iter()
        .find(|p| p.file == AUTHORITY_FILE)
        .expect("terminal/close_authority.rs");
    let test_ctor = authority
        .structure
        .fns
        .iter()
        .find(|f| f.name == "for_test")
        .expect("the test constructor");
    assert!(
        test_ctor.in_test,
        "CloseAuthority::for_test is not #[cfg(test)]"
    );
    let mints: Vec<String> = prepared.iter().flat_map(unauthorized_mint_sites).collect();
    assert!(
        mints.is_empty(),
        "production code obtains a CloseAuthority: {mints:#?}"
    );
}

// ---------------------------------------------------------------------------
// Mutation twins over synthetic sources — each check must be able to fail
// ---------------------------------------------------------------------------

fn synthetic(files: &[(&str, &str)]) -> Vec<Prepared> {
    files.iter().map(|(f, s)| prepare(f, s)).collect()
}

fn rows_of(entries: &[(&str, &str, &str, &str)]) -> Rows {
    entries
        .iter()
        .map(|(f, func, class, detail)| {
            (
                (f.to_string(), func.to_string()),
                Row {
                    class: class.to_string(),
                    detail: detail.to_string(),
                },
            )
        })
        .collect()
}

#[test]
fn a_new_close_site_is_red_until_classified_and_a_stale_row_is_a_ghost() {
    let prepared = synthetic(&[(
        "mcp/new_door.rs",
        "pub async fn close_door(tm: &T) {\n    let _ = tm\n        .close(\"x\");\n}\n\
         pub async fn graceful_door(m: &M) {\n    m.graceful_exit(\"x\", d, None).await;\n}\n",
    )]);
    let rows = rows_of(&[(
        "mcp/new_door.rs",
        "gone_door",
        "agent",
        "a door deleted since",
    )]);
    let verdict = roster_verdict(&prepared, &rows);
    assert_eq!(
        verdict.unlisted,
        vec![
            ("mcp/new_door.rs".to_string(), "close_door".to_string()),
            ("mcp/new_door.rs".to_string(), "graceful_door".to_string()),
        ]
    );
    assert_eq!(
        verdict.ghosts,
        vec![("mcp/new_door.rs".to_string(), "gone_door".to_string())]
    );
}

#[test]
fn test_code_comments_and_declarations_are_not_sites() {
    let prepared = synthetic(&[(
        "mcp/quiet.rs",
        "/// Never call tm.close(id) from here.\n\
         pub fn documented() {\n    // tm.close(id)\n}\n\
         pub fn close(tm: &T, id: &str) {}\n\
         #[cfg(test)]\nfn helper() { tm.close(\"t\"); }\n\
         #[cfg(test)]\nmod tests {\n    fn t() { tm.close(\"t\"); x.graceful_exit(a, b, None); }\n}\n\
         #[test]\nfn direct() { mgr.close(\"t\"); }\n",
    )]);
    assert!(scan_sites(&prepared, &Rows::new()).is_empty());
}

/// A `#[cfg(test)] mod name;` declared out of line makes its file test code
/// as a whole, transitively. A non-test out-of-line child stays production.
#[test]
fn an_out_of_line_test_module_is_test_code() {
    let sources: Sources = [
        ("mcp/door.rs", "#[cfg(test)]\nmod tests;\nmod real;\n"),
        (
            "mcp/door/tests.rs",
            "fn fixture(tm: T) { tm.close(\"t\"); }\nmod helpers;\n",
        ),
        (
            "mcp/door/tests/helpers.rs",
            "fn deeper(tm: T) { tm.close(\"t\"); }\n",
        ),
        (
            "mcp/door/real.rs",
            "pub fn real_close(tm: T) { tm.close(\"t\"); }\n",
        ),
        ("terminal/mod.rs", "#[cfg(test)]\nmod probe_tests;\n"),
        (
            "terminal/probe_tests/mod.rs",
            "fn t() { let a = CloseAuthority::for_test(); }\n",
        ),
    ]
    .iter()
    .map(|(f, s)| (f.to_string(), s.to_string()))
    .collect();
    let prepared = prepare_all(&sources);
    let found: Vec<_> = scan_sites(&prepared, &Rows::new()).into_iter().collect();
    assert_eq!(
        found,
        vec![("mcp/door/real.rs".to_string(), "real_close".to_string())]
    );
    assert_eq!(
        prepared
            .iter()
            .flat_map(unauthorized_mint_sites)
            .collect::<Vec<_>>(),
        Vec::<String>::new(),
        "a test module's out-of-line body may mint"
    );
}

#[test]
fn every_receiver_shape_and_scope_is_found() {
    let prepared = synthetic(&[
        (
            "mcp/shapes.rs",
            "impl Holder { fn by_field(&self) { self.terminal_manager.close(id); } }\n\
             fn by_suffix(close_tm: T) { close_tm.close(&id); }\n\
             fn by_inner(tm: S) { tm.inner().close(tid); }\n\
             fn by_getter(s: &S) { get_terminal_manager(&s).close(id); }\n\
             fn by_state(app: &A) { app.state::<Arc<TerminalManager>>().inner().close(id); }\n\
             fn by_drain(tm: T) { tm.close_all(deadline); }\n\
             fn by_session(s: S) { s.close_kill_only(); }\n\
             fn zero_arg_outside_terminal(session: S) { session.close(); }\n\
             fn relay_frame() { let f = json!({ \"type\": \"remote_terminal_end\" }); }\n\
             impl ReapTarget for crate::terminal::TerminalManager {\n    fn reap(&self) { self.close(id); }\n}\n\
             impl Other { fn not_the_manager(&self) { self.close(id); } }\n",
        ),
        (
            "terminal/inner.rs",
            "impl Drop for Pane { fn drop(&mut self) { self.close(); } }\n",
        ),
    ]);
    let found: Vec<String> = scan_sites(&prepared, &Rows::new())
        .into_iter()
        .map(|(f, func)| format!("{f}:{func}"))
        .collect();
    assert_eq!(
        found,
        vec![
            "mcp/shapes.rs:by_drain",
            "mcp/shapes.rs:by_field",
            "mcp/shapes.rs:by_getter",
            "mcp/shapes.rs:by_inner",
            "mcp/shapes.rs:by_session",
            "mcp/shapes.rs:by_state",
            "mcp/shapes.rs:by_suffix",
            "mcp/shapes.rs:reap",
            "mcp/shapes.rs:relay_frame",
            "terminal/inner.rs:drop",
        ],
        "a zero-arg `.close()` outside terminal/ and `self.close` outside the manager \
         are not TerminalManager closes"
    );
}

#[test]
fn a_helper_rows_callers_are_sites_and_a_primitive_body_is_not() {
    let prepared = synthetic(&[
        (
            "mcp/recipe.rs",
            "pub(crate) async fn end_it(tm: T) { tm.close(&id); }\n\
             pub async fn door() { end_it(tm).await; }\n\
             pub async fn split_door() { client()\n        .end(a).await; }\n\
             pub async fn end_it_with() { end_it_with_more(tm).await; }\n",
        ),
        (
            "terminal/manager.rs",
            "impl TerminalManager {\n    pub fn close(&self, id: &str) { session.close(); }\n}\n",
        ),
    ]);
    let rows = rows_of(&[
        ("mcp/recipe.rs", "end_it", "helper", "end_it("),
        ("mcp/remote.rs", "end", "helper", "client().end("),
    ]);
    let found: Vec<_> = scan_sites(&prepared, &rows).into_iter().collect();
    assert_eq!(
        found,
        vec![
            ("mcp/recipe.rs".to_string(), "door".to_string()),
            ("mcp/recipe.rs".to_string(), "end_it".to_string()),
            ("mcp/recipe.rs".to_string(), "split_door".to_string()),
        ],
        "a helper's callers are sites, a call split across lines included, and a longer \
         name that merely starts with the token is not; TerminalManager::close's own body \
         is not a site"
    );
}

/// The by-name half of NC5-1. `terminal_set_title` is on the
/// `/ui-bridge/invoke` allowlist today; a synthetic `operator` row naming it
/// must be reported.
#[test]
fn an_allowlisted_operator_close_row_is_caught() {
    assert!(
        crate::ui_bridge_invoke::is_allowlisted("terminal_set_title"),
        "fixture: terminal_set_title must stay allowlisted — pick another allowlisted \
         command if it is ever removed, or this check goes vacuous"
    );
    let prepared = synthetic(&[("mcp/tauri_proxy.rs", "async fn dispatch() {}\n")]);
    let rows = rows_of(&[(
        "commands/terminal.rs",
        "terminal_set_title",
        "operator",
        "x",
    )]);
    assert_eq!(
        borrowable_operator_rows(&prepared, &rows),
        vec![
            "commands/terminal.rs:terminal_set_title is on the POST /ui-bridge/invoke/<name> \
             allowlist"
                .to_string()
        ]
    );
}

/// The per-site half of NC5-1, on the REAL proxy source. An `operator` row
/// for a fn a proxy arm calls is caught (`end_remote_session`, which the
/// `remote_session_end` arm calls). An `operator` row for the Tauri command
/// `terminal_close` is NOT caught, although `terminal_close` is a safelisted
/// proxy command NAME: the proxy's arm of that name calls `tm.close` and never
/// the command. A name check would be red there, which is why the check is per
/// site.
#[test]
fn the_proxy_check_is_per_site_not_by_name() {
    assert!(
        crate::mcp::tauri_proxy::ALLOWED_PROXIED_COMMANDS.contains(&"terminal_close"),
        "fixture: terminal_close must stay a proxied command name for this to mean anything"
    );
    let prepared = real_tree();
    let caught = rows_of(&[(
        "commands/remote_end.rs",
        "end_remote_session",
        "operator",
        "x",
    )]);
    assert_eq!(
        borrowable_operator_rows(prepared, &caught),
        vec![
            "commands/remote_end.rs:end_remote_session is called by the POST \
             /ui-bridge/tauri/invoke proxy"
                .to_string()
        ]
    );
    let by_name_only = rows_of(&[("commands/terminal.rs", "terminal_close", "operator", "x")]);
    assert!(
        borrowable_operator_rows(prepared, &by_name_only).is_empty(),
        "the proxy's terminal_close arm never calls the Tauri command of that name"
    );
    let the_proxy_itself = rows_of(&[("mcp/tauri_proxy.rs", "dispatch", "operator", "x")]);
    assert_eq!(
        borrowable_operator_rows(prepared, &the_proxy_itself).len(),
        1,
        "the proxy's own dispatch can never be an operator door"
    );
}

const SEALED: &str = "#[derive(Debug)]\npub struct CloseAuthority {\n    _sealed: (),\n}\n\
    impl CloseAuthority {\n    #[cfg(test)]\n    pub(crate) fn for_test() -> Self {\n        Self { _sealed: () }\n    }\n}\n";

fn authority_tree(authority_src: &str, others: &[(&str, &str)]) -> Vec<Prepared> {
    let mut files = vec![(AUTHORITY_FILE, authority_src)];
    files.extend_from_slice(others);
    synthetic(&files)
}

#[test]
fn the_sealed_shape_passes_and_a_test_module_may_mint() {
    let prepared = authority_tree(
        SEALED,
        &[(
            "terminal/graceful_exit.rs",
            "#[cfg(test)]\nmod tests {\n    fn t() { drive(Some(CloseAuthority::for_test())); }\n}\n",
        )],
    );
    assert_eq!(close_authority_violations(&prepared), Vec::<String>::new());
}

/// Mutation (i), in miniature: a public constructor is red twice, as a
/// production fn returning one and as a production struct literal.
#[test]
fn a_public_constructor_is_red() {
    let src = SEALED.replace(
        "impl CloseAuthority {",
        "impl CloseAuthority {\n    pub fn new() -> Self {\n        Self { _sealed: () }\n    }",
    );
    let violations = close_authority_violations(&authority_tree(&src, &[]));
    assert_eq!(violations.len(), 2, "{violations:#?}");
    assert!(violations[0].contains("terminal/close_authority.rs:new returns a CloseAuthority"));
    assert!(violations[1].contains("builds one with `Self { .. }`"));
}

#[test]
fn a_public_field_a_unit_type_or_a_duplicating_derive_is_red() {
    let public = SEALED.replace("    _sealed: (),", "    pub sealed: (),");
    assert!(close_authority_violations(&authority_tree(&public, &[]))
        .iter()
        .any(|v| v.contains("non-private field")));

    let unit = "pub struct CloseAuthority;\n";
    assert!(close_authority_violations(&authority_tree(unit, &[]))
        .iter()
        .any(|v| v.contains("has no field")));

    for derive in ["Clone", "Default", "Copy", "serde::Deserialize"] {
        let derived = SEALED.replace("#[derive(Debug)]", &format!("#[derive(Debug, {derive})]"));
        assert!(
            close_authority_violations(&authority_tree(&derived, &[]))
                .iter()
                .any(|v| v.contains("derives")),
            "derive({derive}) must be red"
        );
    }
}

#[test]
fn a_trait_impl_or_a_foreign_inherent_impl_is_red() {
    let violations = close_authority_violations(&authority_tree(
        SEALED,
        &[
            (
                "mcp/sneaky.rs",
                "impl Default for CloseAuthority { fn default() -> Self { todo!() } }\n",
            ),
            (
                "mcp/other.rs",
                "impl crate::terminal::close_authority::CloseAuthority {\n    pub fn peek(&self) {}\n}\n",
            ),
        ],
    ));
    assert!(
        violations
            .iter()
            .any(|v| v.contains("impl Default for CloseAuthority")),
        "{violations:#?}"
    );
    assert!(
        violations
            .iter()
            .any(|v| v.contains("an inherent impl of CloseAuthority outside")),
        "{violations:#?}"
    );
}

/// A production caller of the TEST constructor compiles under `cargo test`
/// (where `cfg(test)` holds crate-wide), so the scan, not the compiler, must
/// be what turns it red in the test run. This also covers a call made inside
/// a macro body.
#[test]
fn a_production_mint_outside_the_closer_is_red() {
    let violations = close_authority_violations(&authority_tree(
        SEALED,
        &[(
            "session/wind_down_executor.rs",
            "async fn wind_down_exit(m: &M) {\n    m.graceful_exit(id, d, Some(CloseAuthority::for_test())).await;\n}\n\
             fn in_a_macro() { let v = vec![CloseAuthority::for_test()]; }\n",
        )],
    ));
    assert_eq!(violations.len(), 2, "{violations:#?}");
    assert!(violations[0].contains("(wind_down_exit) calls one of its associated fns"));
    assert!(violations[1].contains("(in_a_macro) calls one of its associated fns"));
}
