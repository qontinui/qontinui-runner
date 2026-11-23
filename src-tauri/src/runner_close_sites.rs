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
//!   `close_kill_only`, `close_after_graceful_exit`, `close_for_handoff`);
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
//! claim is checked five ways:
//!
//! * Its name is not on the `POST /ui-bridge/invoke/<name>` allowlist, whose
//!   dispatch runs the named fn (NC5-1).
//! * Its name is not in the `POST /ui-bridge/tauri/invoke` proxy's command list
//!   (`ALLOWED_PROXIED_COMMANDS`).
//! * The `POST /ui-bridge/observe/{command}` tier does not run it: its gate,
//!   `observe_projection_for`, answers `None`, and the name is not in
//!   `UI_BRIDGE_OBSERVE_COMMANDS`. That tier runs a command through the same
//!   webview round-trip as an invoke.
//! * It is not a fn of that proxy.
//! * Its name appears in production Rust ONLY in its own declaration and inside
//!   a registration outside the proxy: `tauri::generate_handler![…]` or
//!   `ipc_group!(…)`, the two macros this crate registers commands with. Any
//!   other mention is red, whether it is a call, a fn value handed to
//!   something, an entry in a name-to-handler table, or a registration inside
//!   the proxy, which has no reason to register commands. A check of calls
//!   alone would miss the fn value and the table; a check of the proxy's own
//!   arms alone would be one hop deep. Its only legitimate caller is outside
//!   Rust: the webview, over IPC.
//!
//! ## The close-authority scan
//!
//! `terminal::close_authority::CloseAuthority` is what the graceful-exit
//! primitive requires before it types into a live `claude`. Only the lifecycle
//! Closer may ever obtain one (Phase 93), and from Phase 16 also the relay's
//! operator arm. [`AUTHORIZED_MINTERS`] and [`AUTHORIZED_CONSTRUCTORS`] are
//! empty until then. The scan is an ALLOWLIST. Each rule below is RED on
//! production code:
//!
//! 1. SHAPE. There is one definition, in its own module, with at least one
//!    field and every field private. Its derives are `Debug` and nothing else,
//!    counting a `cfg_attr(…, derive(…))` as a derive.
//! 2. No trait impl for the type, except [`CONSTRUCTION_FREE_TRAITS`] (`Drop`,
//!    `Debug`, `Display`), whose methods only borrow a value that exists.
//! 3. No inherent impl outside its own module.
//! 4. No fn that returns one, unless it is an authorized constructor.
//! 5. No `const` or `static` of the type, an associated one included.
//! 6. No `type` alias of it and no `use … as` rename of it. Every rule here
//!    also reads such an alias, within the file that defines it, as the type.
//! 7. No struct literal of it (`CloseAuthority {`, or `Self {` inside one of
//!    its impls, attributed to the IMPL rather than to an enclosing fn, so an
//!    associated const is caught), unless it is inside an authorized
//!    constructor.
//! 8. No MENTION of the type outside the allowlist. A mention is allowed only
//!    inside `terminal/close_authority.rs` (which rules 1-7 police), on a `use`
//!    line, as the `authority: Option<…CloseAuthority>` parameter of one of
//!    [`AUTHORITY_PARAM_FNS`], or inside an authorized minter. A transmute or a
//!    zeroed value that names the type is therefore red.
//! 9. Every call of the primitive passes `None` as its authority. The only
//!    exceptions are a primitive entry point forwarding its own `authority`
//!    parameter, and an authorized minter. This catches a forged value that
//!    never names the type, such as `Some(unsafe { std::mem::zeroed() })`
//!    passed straight to `TerminalManager::graceful_exit`, because the only
//!    use of an authority is to hand it to the primitive. The authority's
//!    argument is found by the `authority` parameter's place in the entry
//!    point's own signature (one later for a path call of a method), so a
//!    parameter appended after it changes nothing. A fn value of the primitive
//!    (a path `X::graceful_exit` not followed by `(`, or a bare name in an
//!    expression) or a turbofish call hides the argument, so both are red as
//!    well. A field access `.graceful_exit` or a field named `graceful_exit`
//!    is not a fn value and is not read.
//!
//!    9b. Inside an entry point, `authority` is the caller's value. The
//!    parameter is not `mut`, `ref` or a sub-pattern, and every word
//!    `authority` in the body's blanked text is one of four ALLOWED USES: the
//!    argument at the authority's place in a primitive call (rule 9 locates
//!    it); a read, `authority.is_some()` or `.is_none()`; both words of the log
//!    field `authority = authority.is_some()` (or `.is_none()`) followed at once
//!    by `,` or `)`; and, in `drive_graceful_exit` only, the `authority` of
//!    `let Some(_authority) = authority else`. Any other occurrence is red. That
//!    covers every binding form (`let`, `if let`, `for`, `match`, a closure
//!    parameter) and every assignment, because each puts the name somewhere
//!    else, including inside a macro such as `tokio::select!`, which `syn` does
//!    not parse.
//!
//! So a public constructor, an associated const, an alias, a transmute, a
//! zeroed value, a `cfg_attr` derive, or a production call of the test
//! constructor is red in `cargo test` itself. Each route has its own mutation
//! twin below.
//!
//! Rules 7, 8 and 9 read the source with every comment and every string and
//! char literal blanked, so a type named in a log message or a trailing
//! comment is not a mention.
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
//!   not recognised, so the code it guards reads as production. In the roster
//!   scan, a comment that shares a line with code reads as code. All three err
//!   toward REPORTING a site, never toward hiding one.
//! * Rule 9 splits arguments on brackets and braces only, so a comma inside a
//!   turbofish or a closure's parameter list splits an argument. That can only
//!   make the authority's argument read as something other than `None`, which
//!   is red.
//! * An alias is resolved within the file that defines it. It is red there
//!   (rule 6), so a use of it elsewhere is never the only red.
//! * Rule 9b reads the entry points' bodies as text: an `authority` word in a
//!   string or a comment is blanked first, and anything else that is not one
//!   of its four allowed uses is red, so it errs toward red. Its real-tree
//!   test pins the uses it allows today: three forwards, the log field's two
//!   words and `drive_graceful_exit`'s let-else.
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
        "close_for_handoff",
    ),
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
const SESSION_LEVEL_CLOSES: &[&str] = &[
    "close",
    "close_with_deadline",
    "close_kill_only",
    "close_for_handoff",
];

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

/// The only derive a close authority may carry. Anything else may build or
/// duplicate one (`Default`, `Clone`, `Copy`, `Deserialize`, …), so the list is
/// an allowlist and not a list of known-bad derives.
const ALLOWED_DERIVES: &[&str] = &["Debug"];

/// The only attributes the definition may carry besides its derives. An
/// attribute macro (a builder, a constructor generator) writes code that no
/// source scan reads, so anything else is red.
const ALLOWED_STRUCT_ATTRS: &[&str] = &["derive", "doc", "must_use", "allow", "expect"];

/// The primitive's entry points, as `(file, impl self type, fn)`. Each one
/// takes `authority: Option<CloseAuthority>`. That parameter is an allowed
/// mention of the type (rule 8), and forwarding it is the one non-`None`
/// authority a call may pass (rule 9).
const AUTHORITY_PARAM_FNS: &[(&str, Option<&str>, &str)] = &[
    ("terminal/graceful_exit.rs", None, "drive_graceful_exit"),
    (
        "terminal/session.rs",
        Some("TerminalSession"),
        "graceful_exit_then",
    ),
    (
        "terminal/session.rs",
        Some("TerminalSession"),
        "graceful_exit_driven",
    ),
    (
        "terminal/manager.rs",
        Some("TerminalManager"),
        "graceful_exit",
    ),
];

// ---------------------------------------------------------------------------
// Structure, from `syn`
// ---------------------------------------------------------------------------

/// One fn: its name, the lines it spans (1-based, inclusive), the line its body
/// opens on (its signature is `start..=body_start`), the self type of the impl
/// it sits in, whether it is test code, and the identifiers its return type
/// names.
///
/// For a fn NAMED like one of [`AUTHORITY_PARAM_FNS`], rules 9 and 9b also
/// need its `authority` parameter (`None` when it has none) and whether it
/// takes a receiver.
#[derive(Debug, Clone)]
struct FnSpan {
    name: String,
    start: usize,
    end: usize,
    body_start: usize,
    impl_self: Option<String>,
    in_test: bool,
    output_idents: Vec<String>,
    vis: Vis,
    authority_param: Option<AuthorityParam>,
    has_receiver: bool,
}

/// A signature's `authority` parameter: its place among the parameters after
/// any receiver, and whether it could hold anything but the value its caller
/// passed (declared `mut` or `ref`, or with a sub-pattern).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AuthorityParam {
    index: usize,
    rebindable: bool,
}

/// A fn's visibility, as far as the scan needs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Vis {
    Private,
    Super,
    Other,
}

/// One impl block, with the lines it spans.
#[derive(Debug, Clone)]
struct ImplBlock {
    trait_name: Option<String>,
    self_ty: Option<String>,
    in_test: bool,
    start: usize,
    end: usize,
}

/// One `const` or `static`, a free, associated or trait one, with the
/// identifiers its type names, the self type of the impl it sits in, and the
/// lines it spans.
#[derive(Debug, Clone)]
struct TypedItem {
    kind: &'static str,
    name: String,
    ty_idents: Vec<String>,
    impl_self: Option<String>,
    in_test: bool,
    start: usize,
    end: usize,
}

/// One `type Name = …;` (free or associated), with the identifiers its target
/// names.
#[derive(Debug, Clone)]
struct TypeAlias {
    name: String,
    ty_idents: Vec<String>,
    in_test: bool,
    line: usize,
}

/// One `use …::from as to;` rename.
#[derive(Debug, Clone)]
struct UseRename {
    from: String,
    to: String,
    in_test: bool,
    line: usize,
}

/// One struct definition: its fields, whether they are named (a braced
/// struct), every derive it can carry, and the path of every other attribute
/// on it, `cfg_attr` flattened.
#[derive(Debug, Clone)]
struct StructDef {
    name: String,
    in_test: bool,
    field_count: usize,
    public_fields: usize,
    named: bool,
    derives: Vec<String>,
    attrs: Vec<String>,
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
    typed_items: Vec<TypedItem>,
    aliases: Vec<TypeAlias>,
    renames: Vec<UseRename>,
    /// The lines of every non-test `use` item.
    use_spans: Vec<(usize, usize)>,
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
        self.typed_items.iter_mut().for_each(|t| t.in_test = true);
        self.aliases.iter_mut().for_each(|a| a.in_test = true);
        self.renames.iter_mut().for_each(|r| r.in_test = true);
        self.use_spans.clear();
    }

    fn in_test_span(&self, line: usize) -> bool {
        self.test_spans.iter().any(|&(s, e)| line >= s && line <= e)
    }

    fn in_use_span(&self, line: usize) -> bool {
        self.use_spans.iter().any(|&(s, e)| line >= s && line <= e)
    }

    /// The innermost fn whose span holds `line`.
    fn innermost_fn(&self, line: usize) -> Option<&FnSpan> {
        self.fns
            .iter()
            .filter(|f| line >= f.start && line <= f.end)
            .min_by_key(|f| f.end - f.start)
    }

    /// The innermost impl block whose span holds `line`.
    fn innermost_impl(&self, line: usize) -> Option<&ImplBlock> {
        self.impls
            .iter()
            .filter(|i| line >= i.start && line <= i.end)
            .min_by_key(|i| i.end - i.start)
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

/// Every derive `attrs` can apply: each plain `#[derive(…)]`, and each
/// `derive(…)` inside a `#[cfg_attr(predicate, …)]` at any depth. A
/// conditional derive applies in some build, so it counts the same.
fn derives_of(attrs: &[syn::Attribute]) -> Vec<String> {
    let mut out = Vec::new();
    for attr in attrs {
        collect_derives(&attr.meta, &mut out);
    }
    out
}

fn collect_derives(meta: &syn::Meta, out: &mut Vec<String>) {
    let syn::Meta::List(list) = meta else {
        return;
    };
    if list.path.is_ident("derive") {
        if let Ok(paths) = list.parse_args_with(
            syn::punctuated::Punctuated::<syn::Path, syn::Token![,]>::parse_terminated,
        ) {
            out.extend(
                paths
                    .iter()
                    .filter_map(|p| p.segments.last().map(|s| s.ident.to_string())),
            );
        }
    } else if list.path.is_ident("cfg_attr") {
        if let Ok(metas) = list.parse_args_with(
            syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
        ) {
            // The first element is the predicate; the rest are attributes.
            for nested in metas.iter().skip(1) {
                collect_derives(nested, out);
            }
        }
    }
}

/// Every identifier `ty` names.
fn type_idents(ty: &syn::Type) -> Vec<String> {
    let mut ids = Idents::default();
    ids.visit_type(ty);
    ids.0
}

/// The `(from, to)` of every `use … as …` rename in `tree`.
fn collect_renames(tree: &syn::UseTree, out: &mut Vec<(String, String)>) {
    match tree {
        syn::UseTree::Path(p) => collect_renames(&p.tree, out),
        syn::UseTree::Rename(r) => out.push((r.ident.to_string(), r.rename.to_string())),
        syn::UseTree::Group(g) => g.items.iter().for_each(|t| collect_renames(t, out)),
        syn::UseTree::Name(_) | syn::UseTree::Glob(_) => {}
    }
}

/// The path of every attribute in `attrs`, as written (`doc`, `must_use`,
/// `serde`, `bon::builder`). A `cfg_attr(predicate, …)` contributes the
/// attributes it can apply, at any depth, rather than itself.
fn attr_paths_of(attrs: &[syn::Attribute]) -> Vec<String> {
    let mut out = Vec::new();
    for attr in attrs {
        collect_attr_paths(&attr.meta, &mut out);
    }
    out
}

fn collect_attr_paths(meta: &syn::Meta, out: &mut Vec<String>) {
    let path = meta.path();
    if path.is_ident("cfg_attr") {
        if let syn::Meta::List(list) = meta {
            if let Ok(metas) = list.parse_args_with(
                syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
            ) {
                for nested in metas.iter().skip(1) {
                    collect_attr_paths(nested, out);
                }
            }
        }
        return;
    }
    out.push(
        path.segments
            .iter()
            .map(|s| s.ident.to_string())
            .collect::<Vec<_>>()
            .join("::"),
    );
}

/// The parameter name rule 9b guards inside a primitive entry point.
const AUTHORITY_PARAM: &str = "authority";

/// `sig`'s `authority` parameter, if it has one.
fn authority_param_of(sig: &syn::Signature) -> Option<AuthorityParam> {
    sig.inputs
        .iter()
        .filter_map(|arg| match arg {
            syn::FnArg::Typed(typed) => Some(&*typed.pat),
            syn::FnArg::Receiver(_) => None,
        })
        .enumerate()
        .find_map(|(index, pat)| match pat {
            syn::Pat::Ident(p) if p.ident == AUTHORITY_PARAM => Some(AuthorityParam {
                index,
                rebindable: p.mutability.is_some() || p.by_ref.is_some() || p.subpat.is_some(),
            }),
            _ => None,
        })
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
        body: Option<&syn::Block>,
        impl_self: Option<String>,
    ) {
        let (start, end) = lines_of(node);
        let body_start = body.map_or(end, |b| b.brace_token.span.open().start().line);
        let mut output = Idents::default();
        output.visit_return_type(&sig.output);
        let entry_point = AUTHORITY_PARAM_FNS
            .iter()
            .any(|&(_, _, name)| sig.ident == name);
        let authority_param = if entry_point {
            authority_param_of(sig)
        } else {
            None
        };
        self.s.fns.push(FnSpan {
            name: sig.ident.to_string(),
            start,
            end,
            body_start,
            impl_self,
            in_test: self.test_depth > 0,
            output_idents: output.0,
            vis,
            authority_param,
            has_receiver: sig.receiver().is_some(),
        });
    }

    fn record_typed<N: Spanned>(
        &mut self,
        kind: &'static str,
        name: &proc_macro2::Ident,
        ty: &syn::Type,
        node: &N,
        impl_self: Option<String>,
    ) {
        let (start, end) = lines_of(node);
        self.s.typed_items.push(TypedItem {
            kind,
            name: name.to_string(),
            ty_idents: type_idents(ty),
            impl_self,
            in_test: self.test_depth > 0,
            start,
            end,
        });
    }

    fn record_alias<N: Spanned>(&mut self, name: &proc_macro2::Ident, ty: &syn::Type, node: &N) {
        self.s.aliases.push(TypeAlias {
            name: name.to_string(),
            ty_idents: type_idents(ty),
            in_test: self.test_depth > 0,
            line: lines_of(node).0,
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
            v.record_fn(&f.sig, vis_of(&f.vis), f, Some(&f.block), None);
            // A fn nested in a method is not in the method's impl.
            v.impl_stack.push(None);
            syn::visit::visit_item_fn(v, f);
            v.impl_stack.pop();
        });
    }
    fn visit_item_impl(&mut self, i: &'ast syn::ItemImpl) {
        self.scoped(&i.attrs, i, |v| {
            let self_ty = last_ident_of_type(&i.self_ty);
            let (start, end) = lines_of(i);
            v.s.impls.push(ImplBlock {
                trait_name: i
                    .trait_
                    .as_ref()
                    .and_then(|(_, p, _)| p.segments.last())
                    .map(|s| s.ident.to_string()),
                self_ty: self_ty.clone(),
                in_test: v.test_depth > 0,
                start,
                end,
            });
            v.impl_stack.push(self_ty);
            syn::visit::visit_item_impl(v, i);
            v.impl_stack.pop();
        });
    }
    fn visit_impl_item_fn(&mut self, f: &'ast syn::ImplItemFn) {
        self.scoped(&f.attrs, f, |v| {
            let impl_self = v.impl_stack.last().cloned().flatten();
            v.record_fn(&f.sig, vis_of(&f.vis), f, Some(&f.block), impl_self);
            syn::visit::visit_impl_item_fn(v, f);
        });
    }
    fn visit_trait_item_fn(&mut self, f: &'ast syn::TraitItemFn) {
        self.scoped(&f.attrs, f, |v| {
            v.record_fn(&f.sig, Vis::Other, f, f.default.as_ref(), None);
            syn::visit::visit_trait_item_fn(v, f);
        });
    }
    fn visit_impl_item_const(&mut self, c: &'ast syn::ImplItemConst) {
        self.scoped(&c.attrs, c, |v| {
            let impl_self = v.impl_stack.last().cloned().flatten();
            v.record_typed("const", &c.ident, &c.ty, c, impl_self);
            syn::visit::visit_impl_item_const(v, c);
        });
    }
    fn visit_trait_item_const(&mut self, c: &'ast syn::TraitItemConst) {
        self.scoped(&c.attrs, c, |v| {
            v.record_typed("const", &c.ident, &c.ty, c, None);
            syn::visit::visit_trait_item_const(v, c);
        });
    }
    fn visit_item_type(&mut self, t: &'ast syn::ItemType) {
        self.scoped(&t.attrs, t, |v| {
            v.record_alias(&t.ident, &t.ty, t);
            syn::visit::visit_item_type(v, t);
        });
    }
    fn visit_impl_item_type(&mut self, t: &'ast syn::ImplItemType) {
        self.scoped(&t.attrs, t, |v| {
            v.record_alias(&t.ident, &t.ty, t);
            syn::visit::visit_impl_item_type(v, t);
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
                named: matches!(s.fields, syn::Fields::Named(_)),
                derives: derives_of(&s.attrs),
                attrs: attr_paths_of(&s.attrs),
            });
            syn::visit::visit_item_struct(v, s);
        });
    }
    fn visit_item_const(&mut self, c: &'ast syn::ItemConst) {
        self.scoped(&c.attrs, c, |v| {
            v.record_typed("const", &c.ident, &c.ty, c, None);
            syn::visit::visit_item_const(v, c);
        });
    }
    fn visit_item_static(&mut self, s: &'ast syn::ItemStatic) {
        self.scoped(&s.attrs, s, |v| {
            v.record_typed("static", &s.ident, &s.ty, s, None);
            syn::visit::visit_item_static(v, s);
        });
    }
    fn visit_item_use(&mut self, u: &'ast syn::ItemUse) {
        self.scoped(&u.attrs, u, |v| {
            let (start, end) = lines_of(u);
            let in_test = v.test_depth > 0;
            if !in_test {
                v.s.use_spans.push((start, end));
            }
            let mut renames = Vec::new();
            collect_renames(&u.tree, &mut renames);
            v.s.renames
                .extend(renames.into_iter().map(|(from, to)| UseRename {
                    from,
                    to,
                    in_test,
                    line: start,
                }));
            syn::visit::visit_item_use(v, u);
        });
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
/// blanked (line numbering kept), plus its structure. `code_only` is the same
/// text with every comment and every string and char literal's contents
/// blanked too, byte for byte, so an offset or a line number means the same in
/// both.
struct Prepared {
    file: String,
    code: String,
    code_only: String,
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
    let keep = |i: usize| comments.get(i) == Some(&false) && !structure.in_test_span(i + 1);
    let mask = |text: &str| {
        text.lines()
            .enumerate()
            .map(|(i, l)| if keep(i) { l } else { "" })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let code = mask(src);
    let code_only = mask(&blank_literals_and_comments(src));
    assert_eq!(
        code_only.len(),
        code.len(),
        "{file}: blanking literals and comments moved a byte offset"
    );
    let mut line_starts = vec![0usize];
    line_starts.extend(code.match_indices('\n').map(|(i, _)| i + 1));
    Prepared {
        file: file.to_string(),
        code,
        code_only,
        line_starts,
        structure,
    }
}

/// `src` with every comment, and the contents of every string and char
/// literal, overwritten with spaces byte for byte. Line breaks stay, so every
/// byte offset and line number is unchanged. Rules 8 and 9 read this copy: a
/// type named in a string, a log message or a trailing comment is not a
/// mention, and a comma inside a string does not split an argument list.
///
/// It lexes line and nested block comments, plain, byte and C strings with
/// their escapes, raw strings with any number of `#`, and char literals as
/// distinct from lifetimes and labels.
fn blank_literals_and_comments(src: &str) -> String {
    let b = src.as_bytes();
    let at = |i: usize| b.get(i).copied().unwrap_or(0);
    let is_ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let mut out = b.to_vec();
    let mut blank = |from: usize, to: usize| {
        if let Some(range) = out.get_mut(from..to.min(b.len())) {
            range
                .iter_mut()
                .filter(|c| **c != b'\n' && **c != b'\r')
                .for_each(|c| *c = b' ');
        }
    };
    let mut i = 0;
    while i < b.len() {
        match at(i) {
            b'/' if at(i + 1) == b'/' => {
                let end = b
                    .get(i..)
                    .unwrap_or_default()
                    .iter()
                    .position(|&c| c == b'\n')
                    .map_or(b.len(), |n| i + n);
                blank(i, end);
                i = end;
            }
            b'/' if at(i + 1) == b'*' => {
                let (mut depth, mut j) = (1usize, i + 2);
                while j < b.len() && depth > 0 {
                    if at(j) == b'/' && at(j + 1) == b'*' {
                        depth += 1;
                        j += 2;
                    } else if at(j) == b'*' && at(j + 1) == b'/' {
                        depth -= 1;
                        j += 2;
                    } else {
                        j += 1;
                    }
                }
                blank(i, j);
                i = j;
            }
            b'"' => {
                let mut j = i + 1;
                while j < b.len() && at(j) != b'"' {
                    j += if at(j) == b'\\' { 2 } else { 1 };
                }
                blank(i + 1, j);
                i = j + 1;
            }
            b'r' | b'b' | b'c' if i == 0 || !is_ident(at(i - 1)) => {
                // A raw string, `r"…"`, `r#"…"#`, `br"…"`, `cr#"…"#`, or the
                // first letter of an identifier or of a byte / C string.
                let r = if at(i) == b'r' { i } else { i + 1 };
                let hashes = b
                    .get(r + 1..)
                    .unwrap_or_default()
                    .iter()
                    .take_while(|&&c| c == b'#')
                    .count();
                let open = r + 1 + hashes;
                if at(r) == b'r' && at(open) == b'"' {
                    let closes =
                        |k: usize| at(k) == b'"' && (1..=hashes).all(|h| at(k + h) == b'#');
                    let mut k = open + 1;
                    while k < b.len() && !closes(k) {
                        k += 1;
                    }
                    blank(open + 1, k);
                    i = k + 1 + hashes;
                } else {
                    i += 1;
                }
            }
            b'\'' => {
                if at(i + 1) == b'\\' {
                    // '\n', '\'', '\\', '\x7f', '\u{…}'.
                    let mut j = i + 3;
                    while j < b.len() && at(j) != b'\'' && at(j) != b'\n' {
                        j += 1;
                    }
                    blank(i + 1, j);
                    i = j + 1;
                } else {
                    let width = src
                        .get(i + 1..)
                        .and_then(|s| s.chars().next())
                        .map_or(1, char::len_utf8);
                    if at(i + 1) != b'\n' && at(i + 1 + width) == b'\'' {
                        blank(i + 1, i + 1 + width);
                        i += 2 + width;
                    } else {
                        // A lifetime or a label.
                        i += 1;
                    }
                }
            }
            _ => i += 1,
        }
    }
    String::from_utf8(out).expect("blanking writes ASCII spaces over whole characters")
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
                pat(r"\.\s*close_for_handoff\s*\(", Scope::Anywhere),
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

/// The `POST /ui-bridge/tauri/invoke` proxy.
const PROXY_FILE: &str = "mcp/tauri_proxy.rs";

/// The macros this crate registers Tauri commands with: `tauri::generate_handler!`
/// (in `main.rs` and in each plugin builder) and `ipc_group!` (the per-module
/// groups `ipc_registry.rs` routes to). Naming a command inside one registers
/// it; it does not call it.
const REGISTRATION_MACROS: &[&str] = &["generate_handler", "ipc_group"];

/// The byte ranges of every registration macro invocation in `code`, from its
/// opening bracket to its closing one.
fn registration_spans(code: &str) -> Vec<(usize, usize)> {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = once(
        &RE,
        &format!(r"\b(?:{})\s*!\s*[\[({{]", REGISTRATION_MACROS.join("|")),
    );
    re.find_iter(code)
        .filter_map(|m| {
            let open = m.end() - 1;
            closing_bracket(code, open).map(|close| (open, close))
        })
        .collect()
}

/// The offset of the bracket that closes the one at `open` in `code`.
fn closing_bracket(code: &str, open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (n, &c) in code.as_bytes().get(open..)?.iter().enumerate() {
        match c {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(open + n);
                }
            }
            _ => {}
        }
    }
    None
}

/// The automation doors besides `/ui-bridge/invoke/<name>` that run a Tauri
/// command by NAME. The operator check reads the real ones ([`REAL_DOORS`]);
/// a twin passes its own.
struct NamedDoors<'a> {
    /// `POST /ui-bridge/tauri/invoke`'s command list (`ALLOWED_PROXIED_COMMANDS`).
    proxied: &'a [&'a str],
    /// Whether `POST /ui-bridge/observe/{command}` runs the command through the
    /// webview round-trip.
    observed: fn(&str) -> bool,
}

/// Whether the observe tier runs `name`: its gate, `observe_projection_for`,
/// answers `Some`, or the name is in `UI_BRIDGE_OBSERVE_COMMANDS`.
fn observed_by_the_real_tier(name: &str) -> bool {
    crate::ui_bridge_invoke::observe_projection_for(name).is_some()
        || crate::ui_bridge_invoke::UI_BRIDGE_OBSERVE_COMMANDS
            .iter()
            .any(|c| c.name == name)
}

/// The crate's own named doors.
const REAL_DOORS: NamedDoors<'static> = NamedDoors {
    proxied: crate::mcp::tauri_proxy::ALLOWED_PROXIED_COMMANDS,
    observed: observed_by_the_real_tier,
};

/// `operator` rows an automation can borrow (NC5-1), each with the route.
///
/// An operator fn is borrowable when:
///
/// 1. its name is on the `/ui-bridge/invoke/<name>` allowlist;
/// 2. its name is in the `/ui-bridge/tauri/invoke` proxy's command list;
/// 3. the `/ui-bridge/observe/{command}` tier runs it;
/// 4. it is a fn of that proxy; or
/// 5. production Rust names it anywhere but its own declaration and a
///    [`REGISTRATION_MACROS`] invocation outside the proxy. This is a MENTION
///    rule, not a call rule: a fn value handed to a proxy arm, or an entry in a
///    name-to-handler table, names the fn without calling it and is red too. A
///    registration inside the proxy counts as a mention, since the proxy has no
///    reason to register commands.
fn borrowable_operator_rows(prepared: &[Prepared], rows: &Rows, doors: &NamedDoors) -> Vec<String> {
    assert!(
        prepared.iter().any(|p| p.file == PROXY_FILE),
        "the UI Bridge Tauri proxy ({PROXY_FILE}) is part of the scan"
    );
    let spans: Vec<Vec<(usize, usize)>> = prepared
        .iter()
        .map(|p| {
            if p.file == PROXY_FILE {
                Vec::new()
            } else {
                registration_spans(&p.code_only)
            }
        })
        .collect();
    let mut out = Vec::new();
    for ((file, func), _) in rows.iter().filter(|(_, r)| r.class == "operator") {
        if crate::ui_bridge_invoke::is_allowlisted(func) {
            out.push(format!(
                "{file}:{func} is on the POST /ui-bridge/invoke/<name> allowlist"
            ));
        }
        if doors.proxied.contains(&func.as_str()) {
            out.push(format!(
                "{file}:{func} is a command name of the POST /ui-bridge/tauri/invoke proxy \
                 (ALLOWED_PROXIED_COMMANDS)"
            ));
        }
        if (doors.observed)(func) {
            out.push(format!(
                "{file}:{func} is run by POST /ui-bridge/observe/{{command}} \
                 (observe_projection_for / UI_BRIDGE_OBSERVE_COMMANDS)"
            ));
        }
        if file == PROXY_FILE {
            out.push(format!(
                "{file}:{func} is a fn of the POST /ui-bridge/tauri/invoke proxy itself"
            ));
        }
        let name = Regex::new(&format!(r"\b{}\b", regex::escape(func))).expect("name regex");
        let mut named = BTreeSet::new();
        for (p, spans) in prepared.iter().zip(&spans) {
            for m in name.find_iter(&p.code_only) {
                let own_declaration = p.file == *file && is_declaration(&p.code_only, m.start());
                let registered = spans.iter().any(|&(s, e)| m.start() > s && m.start() < e);
                if !own_declaration && !registered {
                    let line = p.line_of(m.start());
                    named.insert(format!("{}:{line} ({})", p.file, owner_of(p, line)));
                }
            }
        }
        out.extend(named.into_iter().map(|at| {
            format!(
                "{file}:{func} is named at {at} — an operator fn may be named only in its own \
                 declaration and in a `generate_handler!` / `ipc_group!` registration outside \
                 the proxy, so only the webview's IPC reaches it"
            )
        }));
    }
    out
}

// ---------------------------------------------------------------------------
// The close-authority scan
// ---------------------------------------------------------------------------

/// What the close-authority scan found. The lists after `violations` are what
/// it checked and let pass, so a test can prove the scan was not vacuous.
#[derive(Debug, Default)]
struct AuthorityScan {
    violations: Vec<String>,
    /// Rule 8's allowed `authority` parameter mentions, as `file:fn`.
    param_mentions: Vec<String>,
    /// Rule 9's primitive calls that pass `None`, as `file:fn`.
    none_calls: Vec<String>,
    /// Rule 9's primitive calls through which an entry point forwards its own
    /// `authority`, as `file:fn` of that entry point.
    forwarded_calls: Vec<String>,
    /// Rule 9b's allowed uses of `authority` inside the entry points' bodies,
    /// as `file:fn kind`, one per word.
    entry_uses: Vec<String>,
}

/// Everything wrong with how a close authority can be obtained: the type's
/// shape (rule 1), then rules 2-9b over every file.
fn close_authority_scan(prepared: &[Prepared]) -> AuthorityScan {
    let mut scan = file_scan(prepared);
    let mut violations = shape_violations(prepared);
    violations.append(&mut scan.violations);
    scan.violations = violations;
    scan
}

fn close_authority_violations(prepared: &[Prepared]) -> Vec<String> {
    close_authority_scan(prepared).violations
}

/// Rules 2-9b over every file, without rule 1's whole-tree shape check.
fn file_scan(prepared: &[Prepared]) -> AuthorityScan {
    let mut scan = AuthorityScan::default();
    let signatures = entry_signatures(prepared);
    for p in prepared {
        let names = authority_names(p);
        scan.violations.extend(impl_violations(p, &names));
        scan.violations.extend(constructor_violations(p, &names));
        scan.violations.extend(typed_item_violations(p, &names));
        scan.violations.extend(alias_violations(p, &names));
        scan.violations.extend(literal_violations(p, &names));
        mention_violations(p, &names, &mut scan);
        let forwards = primitive_call_violations(p, &signatures, &mut scan);
        entry_point_violations(p, &forwards, &mut scan);
    }
    scan
}

/// Each primitive entry point's name, with its `authority` parameter's place
/// and whether it takes a receiver, as its own signature declares them. Rule 9
/// reads a call's authority at that place.
fn entry_signatures(prepared: &[Prepared]) -> BTreeMap<String, (usize, bool)> {
    prepared
        .iter()
        .flat_map(|p| {
            p.structure
                .fns
                .iter()
                .filter(move |f| !f.in_test && is_entry_point(&p.file, f))
                .filter_map(|f| {
                    f.authority_param
                        .map(|a| (f.name.clone(), (a.index, f.has_receiver)))
                })
        })
        .collect()
}

/// A regex compiled once per test process.
fn once(cell: &'static std::sync::OnceLock<Regex>, re: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(re).unwrap_or_else(|e| panic!("regex {re:?}: {e}")))
}

/// A regex matching any of `names` as a whole word.
fn any_name_re(names: &BTreeSet<String>, suffix: &str) -> Regex {
    let alternation = names
        .iter()
        .map(|n| regex::escape(n))
        .collect::<Vec<_>>()
        .join("|");
    Regex::new(&format!(r"\b(?:{alternation})\b{suffix}")).expect("name regex")
}

/// Rule 1: exactly one definition, in its own module, a braced struct with at
/// least one field and every field private, deriving `Debug` and nothing else,
/// and carrying no attribute outside [`ALLOWED_STRUCT_ATTRS`].
fn shape_violations(prepared: &[Prepared]) -> Vec<String> {
    let mut out = Vec::new();
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
            } else if !def.named {
                out.push(format!(
                    "{AUTHORITY_TYPE} is a tuple struct, whose constructor is a call that the \
                     struct-literal rule does not read — keep its fields named"
                ));
            }
            if def.public_fields > 0 {
                out.push(format!(
                    "{AUTHORITY_TYPE} has a non-private field, so another module can build one"
                ));
            }
            for d in def
                .derives
                .iter()
                .filter(|d| !ALLOWED_DERIVES.contains(&d.as_str()))
            {
                out.push(format!(
                    "{AUTHORITY_TYPE} derives {d}, which may build or duplicate an authority — \
                     it may derive only {ALLOWED_DERIVES:?}"
                ));
            }
            for a in def
                .attrs
                .iter()
                .filter(|a| !ALLOWED_STRUCT_ATTRS.contains(&a.as_str()))
            {
                out.push(format!(
                    "{AUTHORITY_TYPE} carries #[{a}], which may generate code no source scan \
                     reads — it may carry only {ALLOWED_STRUCT_ATTRS:?}"
                ));
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
    out
}

/// The names that denote the close authority in `p`: the type, plus every
/// non-test `type` alias of it and `use … as` rename of it in this file, to a
/// fixed point (an alias of an alias). An alias is red where it is defined
/// (rule 6), so resolving it in its own file is enough for every other rule to
/// read it as the type there.
fn authority_names(p: &Prepared) -> BTreeSet<String> {
    let mut names = BTreeSet::from([AUTHORITY_TYPE.to_string()]);
    loop {
        let found: Vec<String> = p
            .structure
            .aliases
            .iter()
            .filter(|a| !a.in_test && a.ty_idents.iter().any(|i| names.contains(i)))
            .map(|a| a.name.clone())
            .chain(
                p.structure
                    .renames
                    .iter()
                    .filter(|r| !r.in_test && r.to != "_" && names.contains(&r.from))
                    .map(|r| r.to.clone()),
            )
            .collect();
        let before = names.len();
        names.extend(found);
        if names.len() == before {
            return names;
        }
    }
}

/// `CloseAuthority`, or `T (a second name for CloseAuthority)`.
fn described(name: &str) -> String {
    if name == AUTHORITY_TYPE {
        name.to_string()
    } else {
        format!("{name} (a second name for {AUTHORITY_TYPE})")
    }
}

/// What a line belongs to, for a message: the innermost fn, impl block or
/// `const` / `static` that holds it.
fn owner_of(p: &Prepared, line: usize) -> String {
    let s = &p.structure;
    let holds = |start: usize, end: usize| line >= start && line <= end;
    s.fns
        .iter()
        .filter(|f| holds(f.start, f.end))
        .map(|f| (f.end - f.start, f.name.clone()))
        .chain(s.impls.iter().filter(|i| holds(i.start, i.end)).map(|i| {
            (
                i.end - i.start,
                format!("impl {}", i.self_ty.as_deref().unwrap_or("_")),
            )
        }))
        .chain(
            s.typed_items
                .iter()
                .filter(|t| holds(t.start, t.end))
                .map(|t| (t.end - t.start, format!("{} {}", t.kind, t.name))),
        )
        .min_by_key(|(len, _)| *len)
        .map_or_else(|| "<file scope>".to_string(), |(_, name)| name)
}

/// The fn a construction at `line` is attributed to: the innermost fn that
/// holds it, unless an impl block or a `const` / `static` within that fn's
/// lines holds it too. Then the construction belongs to that item, so an
/// associated const can never borrow an enclosing fn's authorization.
fn constructing_fn(p: &Prepared, line: usize) -> Option<&FnSpan> {
    let s = &p.structure;
    let f = s.innermost_fn(line)?;
    let nested = s
        .impls
        .iter()
        .map(|i| (i.start, i.end))
        .chain(s.typed_items.iter().map(|t| (t.start, t.end)))
        .any(|(start, end)| line >= start && line <= end && start >= f.start && end <= f.end);
    (!nested).then_some(f)
}

/// The text of `offset`'s line in `code_only`, up to `offset`.
fn prefix_of(p: &Prepared, offset: usize) -> &str {
    let start = p
        .line_starts
        .get(p.line_of(offset).saturating_sub(1))
        .copied()
        .unwrap_or(0);
    p.code_only.get(start..offset).unwrap_or("")
}

fn is_entry_point(file: &str, f: &FnSpan) -> bool {
    AUTHORITY_PARAM_FNS
        .iter()
        .any(|&(pf, ps, pn)| pf == file && pn == f.name && f.impl_self.as_deref() == ps)
}

fn is_authorized(list: &[(&str, &str)], file: &str, f: Option<&FnSpan>) -> bool {
    f.is_some_and(|f| list.iter().any(|&(lf, lfn)| lf == file && lfn == f.name))
}

/// The traits a close authority may implement: each method only borrows a
/// value that already exists, so none can build, duplicate or convert into one.
/// Every other trait (`Clone`, `Copy`, `Default`, `From`, `Into`, `TryFrom`,
/// `Deserialize`, …) is red.
const CONSTRUCTION_FREE_TRAITS: &[&str] = &["Drop", "Debug", "Display"];

/// Rules 2 and 3: no trait impl for the type outside
/// [`CONSTRUCTION_FREE_TRAITS`], and no inherent impl outside its own module.
fn impl_violations(p: &Prepared, names: &BTreeSet<String>) -> Vec<String> {
    p.structure
        .impls
        .iter()
        .filter(|i| !i.in_test)
        .filter_map(|i| {
            let ty = i.self_ty.as_deref().filter(|t| names.contains(*t))?;
            match &i.trait_name {
                Some(t) if CONSTRUCTION_FREE_TRAITS.contains(&t.as_str()) => None,
                Some(t) => Some(format!(
                    "{}:{} impl {t} for {} — no trait may build, duplicate or convert into one",
                    p.file,
                    i.start,
                    described(ty)
                )),
                None if p.file != AUTHORITY_FILE => Some(format!(
                    "{}:{} an inherent impl of {} outside {AUTHORITY_FILE}",
                    p.file,
                    i.start,
                    described(ty)
                )),
                None => None,
            }
        })
        .collect()
}

/// Rule 4: every production fn that returns one is an authorized constructor.
fn constructor_violations(p: &Prepared, names: &BTreeSet<String>) -> Vec<String> {
    p.structure
        .fns
        .iter()
        .filter(|f| !f.in_test)
        .filter(|f| {
            f.output_idents.iter().any(|i| names.contains(i))
                || (f.impl_self.as_ref().is_some_and(|s| names.contains(s))
                    && f.output_idents.iter().any(|i| i == "Self"))
        })
        .filter(|f| !is_authorized(AUTHORIZED_CONSTRUCTORS, &p.file, Some(*f)))
        .map(|f| {
            format!(
                "{}:{} returns a {AUTHORITY_TYPE} outside #[cfg(test)] — only the lifecycle \
                 Closer may mint one",
                p.file, f.name
            )
        })
        .collect()
}

/// Rule 5: no `const` or `static` holds one, an associated one included.
fn typed_item_violations(p: &Prepared, names: &BTreeSet<String>) -> Vec<String> {
    p.structure
        .typed_items
        .iter()
        .filter(|t| !t.in_test)
        .filter(|t| {
            t.ty_idents.iter().any(|i| names.contains(i))
                || (t.impl_self.as_ref().is_some_and(|s| names.contains(s))
                    && t.ty_idents.iter().any(|i| i == "Self"))
        })
        .map(|t| {
            format!(
                "{}:{} `{} {}` holds a {AUTHORITY_TYPE} — a stored authority outlives the one \
                 close it was minted for",
                p.file, t.start, t.kind, t.name
            )
        })
        .collect()
}

/// Rule 6: no `type` alias of it and no `use … as` rename of it.
fn alias_violations(p: &Prepared, names: &BTreeSet<String>) -> Vec<String> {
    let aliases = p
        .structure
        .aliases
        .iter()
        .filter(|a| !a.in_test && a.ty_idents.iter().any(|i| names.contains(i)))
        .map(|a| (a.line, format!("`type {}` aliases", a.name)));
    let renames = p
        .structure
        .renames
        .iter()
        .filter(|r| !r.in_test && names.contains(&r.from))
        .map(|r| (r.line, format!("`use … as {}` renames", r.to)));
    aliases
        .chain(renames)
        .map(|(line, what)| {
            format!(
                "{}:{line} {what} {AUTHORITY_TYPE} — a second name for the authority, which \
                 every rule here would have to follow",
                p.file
            )
        })
        .collect()
}

/// A prefix that makes the name after it a declaration or a signature, not an
/// expression: `struct`, `enum`, `union`, `trait`, `type`, `mod`, `impl` (with
/// its generics), `for` in an impl header, or `->`, each optionally followed
/// by a path.
fn declaration_prefix_re() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    once(
        &RE,
        r"(?:\b(?:struct|enum|union|trait|type|mod|impl|for)(?:\s*<[^>]*>)?|->)\s*(?:[A-Za-z_][A-Za-z0-9_]*\s*::\s*)*$",
    )
}

/// Rule 7: no struct literal of it outside an authorized constructor. A
/// `Self { .. }` inside one of its impls counts, attributed to what holds it:
/// the impl, an associated `const`, or the fn (see [`constructing_fn`]).
fn literal_violations(p: &Prepared, names: &BTreeSet<String>) -> Vec<String> {
    static SELF_LITERAL: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let decl = declaration_prefix_re();
    let mut sites: Vec<(usize, &str)> = any_name_re(names, r"\s*\{")
        .find_iter(&p.code_only)
        .filter(|m| !decl.is_match(prefix_of(p, m.start())))
        .map(|m| (m.start(), "builds one with a struct literal"))
        .collect();
    sites.extend(
        once(&SELF_LITERAL, r"\bSelf\s*\{")
            .find_iter(&p.code_only)
            .filter(|m| {
                let line = p.line_of(m.start());
                p.structure.innermost_impl(line).is_some_and(|i| {
                    !i.in_test && i.self_ty.as_ref().is_some_and(|t| names.contains(t))
                }) && !decl.is_match(prefix_of(p, m.start()))
            })
            .map(|m| (m.start(), "builds one with `Self { .. }`")),
    );
    sites
        .into_iter()
        .filter_map(|(offset, what)| {
            let line = p.line_of(offset);
            let f = constructing_fn(p, line);
            (!is_authorized(AUTHORIZED_CONSTRUCTORS, &p.file, f)).then(|| {
                format!(
                    "{}:{line} ({}) {what} — only the lifecycle Closer may mint a \
                     {AUTHORITY_TYPE}",
                    p.file,
                    owner_of(p, line)
                )
            })
        })
        .collect()
}

/// Rule 8: a mention of the type is allowed only inside its own module, on a
/// `use` line, as the `authority: Option<…>` parameter of a primitive entry
/// point, or inside an authorized minter. Everything else is red, so a new way
/// of naming the type is red until someone decides it is safe.
fn mention_violations(p: &Prepared, names: &BTreeSet<String>, scan: &mut AuthorityScan) {
    static PARAM: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static ASSOC_CALL: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    if p.file == AUTHORITY_FILE {
        return;
    }
    let param = once(
        &PARAM,
        r"\bauthority\s*:\s*Option\s*<\s*(?:[A-Za-z_][A-Za-z0-9_]*\s*::\s*)*$",
    );
    let assoc_call = once(&ASSOC_CALL, r"^\s*::\s*([A-Za-z_][A-Za-z0-9_]*)\s*\(");
    for m in any_name_re(names, "").find_iter(&p.code_only) {
        let line = p.line_of(m.start());
        if p.structure.in_use_span(line) {
            continue;
        }
        let f = p.structure.innermost_fn(line);
        if is_authorized(AUTHORIZED_MINTERS, &p.file, f) {
            continue;
        }
        let prefix = prefix_of(p, m.start());
        if let Some(f) = f.filter(|f| is_entry_point(&p.file, f)) {
            let in_signature =
                line < f.body_start || (line == f.body_start && !prefix.contains('{'));
            if in_signature && m.as_str() == AUTHORITY_TYPE && param.is_match(prefix) {
                scan.param_mentions.push(format!("{}:{}", p.file, f.name));
                continue;
            }
        }
        let what = match assoc_call
            .captures(p.code_only.get(m.end()..).unwrap_or(""))
            .and_then(|c| c.get(1))
        {
            Some(assoc) => format!("calls `{}::{}`", m.as_str(), assoc.as_str()),
            None => format!("names `{}`", m.as_str()),
        };
        scan.violations.push(format!(
            "{}:{line} ({}) {what} — a mention is allowed only in {AUTHORITY_FILE}, on a `use` \
             line, as the `authority` parameter of a primitive entry point, or in an authorized \
             minter; only the lifecycle Closer may obtain a {AUTHORITY_TYPE}",
            p.file,
            owner_of(p, line)
        ));
    }
}

/// The top-level arguments of the call whose `(` is at `open` in `code`, each
/// trimmed and paired with the offset it starts at, with a trailing comma's
/// empty one dropped. `None` when the parentheses never balance. Brackets and
/// braces nest; `<…>` and closure bars do not, so a comma inside a turbofish or
/// a closure's parameter list splits an argument. That can only make the
/// authority's argument read as something other than `None`, which is red.
fn call_args(code: &str, open: usize) -> Option<Vec<(usize, &str)>> {
    let trimmed = |from: usize, to: usize| {
        let raw = code.get(from..to)?;
        Some((from + (raw.len() - raw.trim_start().len()), raw.trim()))
    };
    let mut depth = 0usize;
    let mut start = open + 1;
    let mut args = Vec::new();
    for (n, &c) in code.as_bytes().get(open..)?.iter().enumerate() {
        let k = open + n;
        match c {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    args.push(trimmed(start, k)?);
                    if args.last().is_some_and(|&(_, a)| a.is_empty()) {
                        args.pop();
                    }
                    return Some(args);
                }
            }
            b',' if depth == 1 => {
                args.push(trimmed(start, k)?);
                start = k + 1;
            }
            _ => {}
        }
    }
    None
}

/// The primitive's entry points, by the name a call uses. Each name is used by
/// no other fn in the crate, so a call is read in every file, a macro body
/// included.
const PRIMITIVE_CALLS: &[&str] = &[
    "graceful_exit",
    "graceful_exit_then",
    "graceful_exit_driven",
    "drive_graceful_exit",
];

/// Rule 9: every call of the primitive passes `None` as its authority. An
/// entry point may forward its own `authority`, and an authorized minter may
/// pass what it minted. Every file is read the same way. A call written in a
/// macro body is read where the macro is DEFINED, so a `#[macro_export]`
/// macro that expands to a call inside `crate::terminal` is still read. A
/// `use … as` rename of an entry point is red, because a call through the new
/// name would not be read.
///
/// The authority's argument is the one at the `authority` parameter's place in
/// the entry point's own signature (`signatures`), one later for a path call
/// of a method (`TerminalManager::graceful_exit(&tm, …)`), so a parameter
/// appended after it changes nothing. A name with no signature in the scanned
/// tree falls back to the last argument.
///
/// A fn value of the primitive hides the argument, so it is red: a path
/// `X::graceful_exit` not followed by `(`, or a bare name in an expression. A
/// turbofish call hides it too. A field access `.graceful_exit` (Rust has no
/// `obj.method` fn values) and a field or binding NAMED `graceful_exit` (a
/// `name:` declaration, a `let`) are not read.
fn primitive_call_violations(
    p: &Prepared,
    signatures: &BTreeMap<String, (usize, bool)>,
    scan: &mut AuthorityScan,
) -> BTreeSet<usize> {
    let mut forwards = BTreeSet::new();
    static PRIMITIVE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static NOT_AN_EXPRESSION: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = once(
        &PRIMITIVE,
        &format!(r"\b(?:{})\b", PRIMITIVE_CALLS.join("|")),
    );
    for r in p
        .structure
        .renames
        .iter()
        .filter(|r| !r.in_test && PRIMITIVE_CALLS.contains(&r.from.as_str()))
    {
        scan.violations.push(format!(
            "{}:{} `use … as {}` renames `{}` — a call through the new name is not read; \
             every call passes `None`",
            p.file, r.line, r.to, r.from
        ));
    }
    // A `mod` declaration, or a `let` / `let mut` binding of the name.
    let not_an_expression = once(&NOT_AN_EXPRESSION, r"(?:\bmod|\blet(?:\s+mut)?)\s*$");
    let code = &p.code_only;
    for m in re.find_iter(code) {
        let line = p.line_of(m.start());
        let prefix = prefix_of(p, m.start());
        let before = prefix.trim_end();
        let name = m.as_str();
        // `.graceful_exit(` is the commonest call; `.graceful_exit` with no call
        // is a field access.
        let method_or_field = before.ends_with('.');
        if is_declaration(code, m.start())
            || p.structure.in_use_span(line)
            || not_an_expression.is_match(prefix)
        {
            continue;
        }
        let f = p.structure.innermost_fn(line);
        let minter = is_authorized(AUTHORIZED_MINTERS, &p.file, f);
        let owner = owner_of(p, line);
        let rest = code.get(m.end()..).unwrap_or("");
        let after = rest.trim_start();
        let refused = |why: String| {
            format!(
                "{}:{line} ({owner}) {why} — every call passes `None`; only a primitive entry \
                 point may forward its own `authority`, and only an authorized minter may pass \
                 one",
                p.file
            )
        };
        if let Some(path) = after.strip_prefix("::") {
            // `graceful_exit::GracefulExitOutcome` is a module path; a
            // turbofish is a call the argument parse does not read.
            if path.trim_start().starts_with('<') && !minter {
                scan.violations.push(refused(format!(
                    "calls `{name}` with a turbofish, whose arguments the scan does not read"
                )));
            }
            continue;
        }
        if !after.starts_with('(') {
            // Not a call. A field access, or a field / parameter / literal field
            // NAMED like the primitive (`graceful_exit: bool`), is no fn value.
            let field = method_or_field || after.starts_with(':');
            if !field && !minter {
                scan.violations.push(refused(format!(
                    "names `{name}` as a value, so the authority it is handed cannot be read"
                )));
            }
            continue;
        }
        let open = m.end() + (rest.len() - after.len());
        let Some(args) = call_args(code, open) else {
            scan.violations.push(refused(format!(
                "calls `{name}(` with arguments the scan cannot parse"
            )));
            continue;
        };
        let authority = match signatures.get(name) {
            Some(&(index, has_receiver)) => {
                let path_call = before.ends_with("::");
                args.get(if has_receiver && path_call {
                    index + 1
                } else {
                    index
                })
            }
            None => args.last(),
        };
        let Some(&(at, authority)) = authority else {
            scan.violations.push(refused(format!(
                "calls `{name}(` with no argument at the authority's place"
            )));
            continue;
        };
        if authority == "None" {
            scan.none_calls.push(format!("{}:{owner}", p.file));
        } else if authority == AUTHORITY_PARAM && f.is_some_and(|f| is_entry_point(&p.file, f)) {
            scan.forwarded_calls.push(format!("{}:{owner}", p.file));
            forwards.insert(at);
        } else if !minter {
            let shown: String = authority
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .chars()
                .take(80)
                .collect();
            scan.violations.push(refused(format!(
                "passes `{shown}` as the authority of `{name}(`"
            )));
        }
    }
    forwards
}

/// Rule 9b: inside a primitive entry point, `authority` IS the caller's value.
/// The parameter is not `mut`, `ref` or a sub-pattern. And every word
/// `authority` in the body's blanked text is one of four ALLOWED USES:
///
/// * the argument at the authority's place in a primitive call, which rule 9
///   located (`forwards`);
/// * a read, `authority.is_some()` or `authority.is_none()`;
/// * both words of the log field `authority = authority.is_some()` (or
///   `.is_none()`), followed at once by `,` or `)`;
/// * in `drive_graceful_exit` only, the `authority` of `let Some(_authority) = authority else`.
///
/// Any other occurrence is red. Every binding form (`let`, `if let`, `for`,
/// `match`, a closure parameter, a macro's `$i`) and every assignment puts the
/// name somewhere else, so none needs listing. The text is read rather than
/// the tree, because `syn` does not parse a macro body such as a
/// `tokio::select!` arm.
fn entry_point_violations(p: &Prepared, forwards: &BTreeSet<usize>, scan: &mut AuthorityScan) {
    static WORD: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static READ: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static LOG_FIELD: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static LET_ELSE_HEAD: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static LET_ELSE_TAIL: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let word = once(&WORD, r"\bauthority\b");
    let read = once(&READ, r"^authority\s*\.\s*is_(?:some|none)\s*\(\s*\)");
    let log_field = once(
        &LOG_FIELD,
        r"^authority\s*=\s*(authority)\s*\.\s*is_(?:some|none)\s*\(\s*\)\s*[,)]",
    );
    let let_else_head = once(
        &LET_ELSE_HEAD,
        r"\blet\s+Some\s*\(\s*_authority\s*\)\s*=\s*$",
    );
    let let_else_tail = once(&LET_ELSE_TAIL, r"^authority\s+else\b");
    let unchanged = "a primitive entry point forwards the authority its caller passed, unchanged";
    for f in p
        .structure
        .fns
        .iter()
        .filter(|f| !f.in_test && is_entry_point(&p.file, f))
    {
        if f.authority_param.is_some_and(|a| a.rebindable) {
            scan.violations.push(format!(
                "{}:{} declares its `authority` parameter `mut`, `ref` or with a sub-pattern — \
                 {unchanged}",
                p.file, f.name
            ));
        }
        let body = entry_body(p, f);
        let mut log_field_second_words = BTreeSet::new();
        let mut red_lines = BTreeSet::new();
        for m in word.find_iter(p.code_only.get(body.clone()).unwrap_or("")) {
            let at = body.start + m.start();
            let tail = p.code_only.get(at..).unwrap_or("");
            let kind = if log_field_second_words.remove(&at) {
                // Counted with the field's first word.
                continue;
            } else if forwards.contains(&at) {
                "forward"
            } else if let Some(second) = log_field.captures(tail).and_then(|c| c.get(1)) {
                // The field's second word is recorded now, and skipped when
                // the scan reaches it.
                log_field_second_words.insert(at + second.start());
                scan.entry_uses
                    .push(format!("{}:{} log field", p.file, f.name));
                "log field"
            } else if read.is_match(tail) {
                "read"
            } else if f.name == "drive_graceful_exit"
                && let_else_head.is_match(prefix_of(p, at))
                && let_else_tail.is_match(tail)
            {
                "let-else"
            } else {
                red_lines.insert(p.line_of(at));
                continue;
            };
            scan.entry_uses
                .push(format!("{}:{} {kind}", p.file, f.name));
        }
        scan.violations.extend(red_lines.into_iter().map(|line| {
            format!(
                "{}:{line} ({}) uses `authority` other than as the forwarded argument, an \
                 `is_some()` / `is_none()` read, the log field or drive_graceful_exit's let-else — \
                 {unchanged}",
                p.file, f.name
            )
        }));
    }
}

/// The byte range of `f`'s body in `p.code_only`: from the first `{` on the
/// line its body opens on, to the end of its last line.
fn entry_body(p: &Prepared, f: &FnSpan) -> std::ops::Range<usize> {
    let line_start = |line: usize| {
        p.line_starts
            .get(line.saturating_sub(1))
            .copied()
            .unwrap_or(p.code_only.len())
    };
    let open_line = line_start(f.body_start);
    let start = p
        .code_only
        .get(open_line..)
        .and_then(|rest| rest.find('{'))
        .map_or(open_line, |n| open_line + n);
    let end = p
        .line_starts
        .get(f.end)
        .map_or(p.code_only.len(), |&next| next.saturating_sub(1));
    start..end.max(start)
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
    let violations = borrowable_operator_rows(real_tree(), &load_rows(), &REAL_DOORS);
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

/// The check above finds nothing; this proves it LOOKED. It found the test
/// constructor, and that it is test-only. It allowed exactly the four entry
/// points' `authority` parameters. It saw the three forwards between them, and
/// it saw the production callers pass `None`. A scan gone blind to the type
/// or to the calls fails here instead of passing vacuously.
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
    let scan = close_authority_scan(prepared);
    let sorted = |v: &[String]| {
        let mut v = v.to_vec();
        v.sort();
        v
    };
    assert_eq!(
        sorted(&scan.param_mentions),
        vec![
            "terminal/graceful_exit.rs:drive_graceful_exit",
            "terminal/manager.rs:graceful_exit",
            "terminal/session.rs:graceful_exit_driven",
            "terminal/session.rs:graceful_exit_then",
        ],
        "each primitive entry point takes `authority: Option<CloseAuthority>` exactly once — \
         update AUTHORITY_PARAM_FNS if an entry point moved"
    );
    assert_eq!(
        sorted(&scan.forwarded_calls),
        vec![
            "terminal/manager.rs:graceful_exit",
            "terminal/session.rs:graceful_exit_driven",
            "terminal/session.rs:graceful_exit_then",
        ],
        "the entry points forward the caller's authority down to `drive_graceful_exit`"
    );
    assert_eq!(
        sorted(&scan.entry_uses),
        vec![
            "terminal/graceful_exit.rs:drive_graceful_exit let-else",
            "terminal/manager.rs:graceful_exit forward",
            "terminal/session.rs:graceful_exit_driven forward",
            "terminal/session.rs:graceful_exit_driven log field",
            "terminal/session.rs:graceful_exit_driven log field",
            "terminal/session.rs:graceful_exit_then forward",
        ],
        "the four entry bodies use `authority` exactly as rule 9b allows: three forwards, \
         the log field's two words, and drive_graceful_exit's let-else"
    );
    for caller in [
        "mcp/backend_relay.rs:end_admitted_terminal",
        "mcp/debug_graceful_exit.rs:graceful_exit_door",
        "mcp/steward.rs:stop_at_boundary",
        "session/wind_down_executor.rs:wind_down_exit",
    ] {
        assert!(
            scan.none_calls.iter().any(|c| c == caller),
            "rule 9 never saw {caller} call the primitive with `None` — the call scan has gone \
             blind ({:?})",
            scan.none_calls
        );
    }
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
        file_scan(&prepared).violations,
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

/// A twin's named doors: none.
const NO_DOORS: NamedDoors<'static> = NamedDoors {
    proxied: &[],
    observed: no_command,
};

fn no_command(_: &str) -> bool {
    false
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
        borrowable_operator_rows(&prepared, &rows, &NO_DOORS),
        vec![
            "commands/terminal.rs:terminal_set_title is on the POST /ui-bridge/invoke/<name> \
             allowlist"
                .to_string()
        ]
    );
}

/// The operator check on the REAL tree, against the real named doors.
///
/// * An `operator` row for `end_remote_session` is caught: the proxy's
///   `remote_session_end` arm names it, in `dispatch`.
/// * An `operator` row for the Tauri command `terminal_close` is caught ONLY
///   by name, because `terminal_close` is in `ALLOWED_PROXIED_COMMANDS`. Its
///   other appearances are its registrations, `generate_handler!` in its
///   plugin builder and in `main.rs`, which name it without calling it. So this
///   is also the real tree's control for the registration exemption.
/// * An `operator` row for `github_list_repos` is caught by the observe tier,
///   whose real gate runs it.
/// * The proxy's own `dispatch` can never be an operator door.
#[test]
fn the_operator_check_on_the_real_doors() {
    assert!(
        REAL_DOORS.proxied.contains(&"terminal_close"),
        "fixture: terminal_close must stay a proxied command name for this to mean anything"
    );
    assert!(
        (REAL_DOORS.observed)("github_list_repos") && !(REAL_DOORS.observed)("terminal_close"),
        "fixture: github_list_repos must stay observe-allowed, and terminal_close not"
    );
    let prepared = real_tree();
    let caught = rows_of(&[(
        "commands/remote_end.rs",
        "end_remote_session",
        "operator",
        "x",
    )]);
    let violations = borrowable_operator_rows(prepared, &caught, &REAL_DOORS);
    assert!(
        violations.iter().any(|v| v.starts_with(
            "commands/remote_end.rs:end_remote_session is named at mcp/tauri_proxy.rs:"
        ) && v.contains(" (dispatch) — ")),
        "{violations:#?}"
    );
    let by_name = rows_of(&[("commands/terminal.rs", "terminal_close", "operator", "x")]);
    assert_eq!(
        borrowable_operator_rows(prepared, &by_name, &REAL_DOORS),
        vec![
            "commands/terminal.rs:terminal_close is a command name of the POST \
             /ui-bridge/tauri/invoke proxy (ALLOWED_PROXIED_COMMANDS)"
                .to_string()
        ],
        "terminal_close is caught by the proxy's name list and by nothing else: its \
         registrations are not mentions"
    );
    let observed = rows_of(&[(
        "commands/setup_wizard.rs",
        "github_list_repos",
        "operator",
        "x",
    )]);
    assert!(
        borrowable_operator_rows(prepared, &observed, &REAL_DOORS)
            .iter()
            .any(|v| v
                == "commands/setup_wizard.rs:github_list_repos is run by POST \
                    /ui-bridge/observe/{command} (observe_projection_for / \
                    UI_BRIDGE_OBSERVE_COMMANDS)"),
        "the real observe tier's command is caught"
    );
    let the_proxy_itself = rows_of(&[("mcp/tauri_proxy.rs", "dispatch", "operator", "x")]);
    assert!(
        borrowable_operator_rows(prepared, &the_proxy_itself, &REAL_DOORS)
            .iter()
            .any(|v| v.ends_with("is a fn of the POST /ui-bridge/tauri/invoke proxy itself")),
        "the proxy's own dispatch can never be an operator door"
    );
}

/// The fixture's operator command module: the command and its `ipc_group`
/// registration. The macro's name is split across `concat!` on purpose:
/// `tests/tauri_commands_are_registered.rs` reads every source file for an
/// invocation of that macro, string literals included, and would take this
/// fixture for a real command group with no `GROUPS` entry.
const OPERATOR_MODULE: &str = concat!(
    "#[tauri::command]\npub async fn operator_close(id: &str) {\n    tm.close(id);\n}\n",
    "crate::ipc_",
    "group!(operator_close, other_command);\n",
);

/// The synthetic operator tree: a proxy, an operator command, and its two
/// registrations (a plugin's `generate_handler!` and an `ipc_group!`).
fn operator_tree(proxy_body: &str, shared_body: &str) -> Vec<Prepared> {
    let proxy = format!("pub(crate) async fn dispatch(cmd: &str, args: A) {{\n{proxy_body}}}\n");
    let shared = format!("pub(crate) async fn shared_helper() {{\n{shared_body}}}\n");
    synthetic(&[
        ("mcp/tauri_proxy.rs", proxy.as_str()),
        ("commands/shared.rs", shared.as_str()),
        ("commands/operator.rs", OPERATOR_MODULE),
        (
            "main.rs",
            "fn run() {\n    let _h = tauri::generate_handler![\n        \
             commands::operator::operator_close,\n        other,\n    ];\n}\n",
        ),
    ])
}

fn operator_row() -> Rows {
    rows_of(&[("commands/operator.rs", "operator_close", "operator", "x")])
}

/// The message the mention rule gives for a mention at `at`.
fn named_at(at: &str) -> String {
    format!(
        "commands/operator.rs:operator_close is named at {at} — an operator fn may be named only \
         in its own declaration and in a `generate_handler!` / `ipc_group!` registration outside \
         the proxy, so only the webview's IPC reaches it"
    )
}

/// The control: an operator fn named only in its own declaration and in its
/// `generate_handler!` / `ipc_group!` registrations is not borrowable.
#[test]
fn an_operator_fn_named_only_in_its_registrations_is_green() {
    let tree = operator_tree("    let _ = cmd;\n", "");
    let doors = NamedDoors {
        proxied: &["other_command"],
        observed: |name| name == "other_command",
    };
    assert_eq!(
        borrowable_operator_rows(&tree, &operator_row(), &doors),
        Vec::<String>::new()
    );
}

/// The depth half of NC5-1 (round-2 finding 4). A proxy arm calls a shared
/// helper, and the helper calls the operator fn.
#[test]
fn an_operator_fn_with_any_production_caller_is_red() {
    let tree = operator_tree(
        "    if cmd == \"x\" {\n        crate::commands::shared::shared_helper().await;\n    }\n",
        "    crate::commands::operator::operator_close(\"t\").await;\n",
    );
    assert_eq!(
        borrowable_operator_rows(&tree, &operator_row(), &NO_DOORS),
        vec![named_at("commands/shared.rs:2 (shared_helper)")]
    );
}

/// Round-3 R1: a proxy arm that hands the fn over as a VALUE calls nothing,
/// and neither does a name-to-handler table. Both name it, so both are red.
#[test]
fn an_operator_fn_passed_as_a_value_is_red() {
    let tree = operator_tree(
        "    run_command(args, crate::commands::operator::operator_close).await;\n",
        "    static HANDLERS: &[(&str, Handler)] =\n        \
         &[(\"close\", crate::commands::operator::operator_close)];\n",
    );
    assert_eq!(
        borrowable_operator_rows(&tree, &operator_row(), &NO_DOORS),
        vec![
            named_at("commands/shared.rs:3 (static HANDLERS)"),
            named_at("mcp/tauri_proxy.rs:2 (dispatch)"),
        ]
    );
}

/// Round-3 R1: an operator fn's name in the proxy's command list is red, even
/// with no Rust mention at all: the proxy would offer a command of that name.
#[test]
fn an_operator_fn_named_in_the_proxy_list_is_red() {
    let tree = operator_tree("    let _ = cmd;\n", "");
    let doors = NamedDoors {
        proxied: &["setting_get", "operator_close"],
        observed: no_command,
    };
    assert_eq!(
        borrowable_operator_rows(&tree, &operator_row(), &doors),
        vec![
            "commands/operator.rs:operator_close is a command name of the POST \
             /ui-bridge/tauri/invoke proxy (ALLOWED_PROXIED_COMMANDS)"
                .to_string()
        ]
    );
}

/// Round-4 F2: the observe tier runs a command through the same webview
/// round-trip as an invoke, so an operator fn it runs is red.
#[test]
fn an_operator_fn_the_observe_tier_runs_is_red() {
    let tree = operator_tree("    let _ = cmd;\n", "");
    let doors = NamedDoors {
        proxied: &[],
        observed: |name| name == "operator_close",
    };
    assert_eq!(
        borrowable_operator_rows(&tree, &operator_row(), &doors),
        vec![
            "commands/operator.rs:operator_close is run by POST /ui-bridge/observe/{command} \
             (observe_projection_for / UI_BRIDGE_OBSERVE_COMMANDS)"
                .to_string()
        ]
    );
}

/// Round-4 F2: a registration INSIDE the proxy is a mention, since the proxy
/// has no reason to register commands.
#[test]
fn a_registration_inside_the_proxy_is_red() {
    let tree = operator_tree(
        "    let _h = tauri::generate_handler![crate::commands::operator::operator_close];\n",
        "",
    );
    assert_eq!(
        borrowable_operator_rows(&tree, &operator_row(), &NO_DOORS),
        vec![named_at("mcp/tauri_proxy.rs:2 (dispatch)")]
    );
}

const SEALED: &str = "#[derive(Debug)]\npub struct CloseAuthority {\n    _sealed: (),\n}\n\
    impl CloseAuthority {\n    #[cfg(test)]\n    pub(crate) fn for_test() -> Self {\n        Self { _sealed: () }\n    }\n}\n";

fn authority_tree(authority_src: &str, others: &[(&str, &str)]) -> Vec<Prepared> {
    let mut files = vec![(AUTHORITY_FILE, authority_src)];
    files.extend_from_slice(others);
    synthetic(&files)
}

/// The scan's violations over `prepared`, each `needle` asserted present.
fn assert_red(prepared: &[Prepared], needles: &[&str]) -> Vec<String> {
    let violations = close_authority_violations(prepared);
    for needle in needles {
        assert!(
            violations.iter().any(|v| v.contains(needle)),
            "expected a violation containing {needle:?}, got {violations:#?}"
        );
    }
    violations
}

#[test]
fn the_sealed_shape_passes_and_a_test_module_may_mint() {
    let prepared = authority_tree(
        SEALED,
        &[(
            "terminal/graceful_exit.rs",
            "#[cfg(test)]\nmod tests {\n    fn t() { drive_graceful_exit(Some(CloseAuthority::for_test())); }\n}\n",
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
    assert!(violations[1].contains("(new) builds one with `Self { .. }`"));
}

#[test]
fn a_public_field_a_unit_or_tuple_type_or_a_duplicating_derive_is_red() {
    let public = SEALED.replace("    _sealed: (),", "    pub sealed: (),");
    assert_red(&authority_tree(&public, &[]), &["non-private field"]);
    assert_red(
        &authority_tree("pub struct CloseAuthority;\n", &[]),
        &["has no field"],
    );
    assert_red(
        &authority_tree("pub struct CloseAuthority(());\n", &[]),
        &["is a tuple struct"],
    );
    for derive in ["Clone", "Default", "Copy", "serde::Deserialize"] {
        let derived = SEALED.replace("#[derive(Debug)]", &format!("#[derive(Debug, {derive})]"));
        let name = derive.rsplit("::").next().unwrap_or(derive);
        assert_red(
            &authority_tree(&derived, &[]),
            &[&format!("derives {name}")],
        );
    }
}

/// Finding 1, route 4: a derive behind `cfg_attr`, at any depth, is a derive,
/// and an attribute macro on the definition is red.
#[test]
fn a_cfg_attr_derive_or_an_attribute_macro_is_red() {
    for (attr, needle) in [
        ("#[cfg_attr(not(test), derive(Clone))]", "derives Clone"),
        (
            "#[cfg_attr(unix, cfg_attr(not(test), derive(Default)))]",
            "derives Default",
        ),
        ("#[bon::builder]", "carries #[bon::builder]"),
        ("#[cfg_attr(not(test), serde(default))]", "carries #[serde]"),
    ] {
        let src = SEALED.replace("#[derive(Debug)]", &format!("#[derive(Debug)]\n{attr}"));
        assert_red(&authority_tree(&src, &[]), &[needle]);
    }
}

#[test]
fn a_trait_impl_or_a_foreign_inherent_impl_is_red() {
    assert_red(
        &authority_tree(
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
        ),
        &[
            "impl Default for CloseAuthority",
            "an inherent impl of CloseAuthority outside",
        ],
    );
}

/// Finding 1, route 1: an associated const builds one with `Self { .. }`
/// inside no fn at all. The literal is attributed to what holds it (the
/// const), never to an enclosing fn, and the const is red as a stored
/// authority.
#[test]
fn an_associated_const_is_red() {
    let src = SEALED.replace(
        "impl CloseAuthority {",
        "impl CloseAuthority {\n    pub const UNCHECKED: Self = Self { _sealed: () };",
    );
    let violations = close_authority_violations(&authority_tree(&src, &[]));
    assert_eq!(violations.len(), 2, "{violations:#?}");
    assert!(violations[0].contains(":6 `const UNCHECKED` holds a CloseAuthority"));
    assert!(violations[1].contains(":6 (const UNCHECKED) builds one with `Self { .. }`"));
}

/// Rule 5: a free `const` in the type's own module and a `static` anywhere
/// are red, however they are initialized.
#[test]
fn a_const_or_a_static_of_the_type_is_red() {
    let src = format!(
        "{SEALED}pub(crate) const FORGED: CloseAuthority = CloseAuthority {{ _sealed: () }};\n"
    );
    assert_red(
        &authority_tree(
            &src,
            &[(
                "mcp/stash.rs",
                "use crate::terminal::close_authority::CloseAuthority;\n\
                 static STASH: std::sync::Mutex<Option<CloseAuthority>> = std::sync::Mutex::new(None);\n",
            )],
        ),
        &[
            ":11 `const FORGED` holds a CloseAuthority",
            ":11 (const FORGED) builds one with a struct literal",
            "mcp/stash.rs:2 `static STASH` holds a CloseAuthority",
            "mcp/stash.rs:2 (static STASH) names `CloseAuthority`",
        ],
    );
}

/// Finding 1, route 2a: a `type` alias is red where it is defined, and rule 7
/// reads it as the type, so its struct literal is red too.
#[test]
fn an_alias_with_a_struct_literal_is_red() {
    let literal = format!(
        "{SEALED}type T = CloseAuthority;\npub(crate) fn forge() {{\n    let _a = T {{ _sealed: () }};\n}}\n"
    );
    assert_red(
        &authority_tree(&literal, &[]),
        &[
            ":11 `type T` aliases CloseAuthority",
            ":13 (forge) builds one with a struct literal",
        ],
    );
}

/// Finding 1, route 2b: `impl From<()>` for an alias is a trait impl for the
/// type (rule 2), its `from` returns one (rule 4), and the alias itself is red.
#[test]
fn an_alias_with_a_from_impl_is_red() {
    assert_red(
        &authority_tree(
            SEALED,
            &[(
                "mcp/convert.rs",
                "use crate::terminal::close_authority::CloseAuthority;\ntype T = CloseAuthority;\n\
                 impl From<()> for T {\n    fn from(_: ()) -> Self {\n        todo!()\n    }\n}\n",
            )],
        ),
        &[
            "mcp/convert.rs:2 `type T` aliases CloseAuthority",
            "mcp/convert.rs:3 impl From for T (a second name for CloseAuthority)",
            "mcp/convert.rs:from returns a CloseAuthority",
            "mcp/convert.rs:2 (<file scope>) names `CloseAuthority`",
        ],
    );
}

/// Rule 6's other half: a `use … as` rename is red, and the new name is read
/// as the type.
#[test]
fn a_use_rename_is_red_and_is_read_as_the_type() {
    assert_red(
        &authority_tree(
            SEALED,
            &[(
                "mcp/renamed.rs",
                "use crate::terminal::close_authority::CloseAuthority as Token;\n\
                 pub(crate) fn hold(_t: Option<Token>) {}\n",
            )],
        ),
        &[
            "mcp/renamed.rs:1 `use … as Token` renames CloseAuthority",
            "mcp/renamed.rs:2 (hold) names `Token`",
        ],
    );
}

/// Finding 1, route 3a: a transmute names the type, so it is a mention outside
/// the allowlist (rule 8).
#[test]
fn a_transmute_is_red() {
    let violations = assert_red(
        &authority_tree(
            SEALED,
            &[(
                "mcp/forge.rs",
                "use crate::terminal::close_authority::CloseAuthority;\n\
                 pub(crate) fn transmuted() {\n    let _a = unsafe { std::mem::transmute::<(), CloseAuthority>(()) };\n}\n",
            )],
        ),
        &["mcp/forge.rs:3 (transmuted) names `CloseAuthority`"],
    );
    assert_eq!(violations.len(), 1, "{violations:#?}");
}

/// Finding 1, route 3b: a zeroed value. One that names the type is a mention
/// (rule 8). One that never names it, passed straight to the primitive, is a
/// non-`None` authority (rule 9).
#[test]
fn a_zeroed_value_is_red() {
    let violations = assert_red(
        &authority_tree(
            SEALED,
            &[(
                "mcp/forge.rs",
                "use crate::terminal::close_authority::CloseAuthority;\n\
                 pub(crate) fn zeroed() {\n    let _a: CloseAuthority = unsafe { std::mem::zeroed() };\n}\n\
                 pub(crate) async fn inferred(tm: &T) {\n    let _ = tm.graceful_exit(id, d, Some(unsafe { std::mem::zeroed() })).await;\n}\n",
            )],
        ),
        &[
            "mcp/forge.rs:3 (zeroed) names `CloseAuthority`",
            "mcp/forge.rs:6 (inferred) passes `Some(unsafe { std::mem::zeroed() })` as the \
             authority of `graceful_exit(`",
        ],
    );
    assert_eq!(violations.len(), 2, "{violations:#?}");
}

/// A production call of the TEST constructor compiles under `cargo test`
/// (where `cfg(test)` holds crate-wide), so the scan, not the compiler, must
/// turn it red in the test run. That holds inside a macro body too. Handing
/// one to the primitive is red twice: as a mention and as a non-`None`
/// authority.
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
    assert_eq!(violations.len(), 3, "{violations:#?}");
    assert!(violations[0].contains("(wind_down_exit) calls `CloseAuthority::for_test`"));
    assert!(violations[1].contains("(in_a_macro) calls `CloseAuthority::for_test`"));
    assert!(violations[2].contains(
        "(wind_down_exit) passes `Some(CloseAuthority::for_test())` as the authority of \
         `graceful_exit(`"
    ));
}

/// Rule 8's one parameter position is `authority: Option<…CloseAuthority>`
/// on one of the four entry points, in its own file and impl. There it is
/// allowed, and the forwards between the entry points pass (positive
/// control). The same parameter on any other fn, on a same-named fn of
/// another impl, or a second mention anywhere in an entry point, is red.
#[test]
fn the_authority_parameter_is_allowed_only_on_the_four_entry_points() {
    let scan = close_authority_scan(&authority_tree(
        SEALED,
        &[
            (
                "terminal/graceful_exit.rs",
                "use crate::terminal::close_authority::CloseAuthority;\n\
                 pub(super) async fn drive_graceful_exit<W>(\n    write: W,\n    screen: S,\n    probe: P,\n    close_tab: C,\n    \
                 timing: T,\n    authority: Option<CloseAuthority>,\n) -> O {\n    \
                 let Some(_authority) = authority else {\n        return O::Refused;\n    };\n    todo!()\n}\n",
            ),
            (
                "terminal/session.rs",
                "impl TerminalSession {\n    pub(super) async fn graceful_exit_then(&self, d: D, c: C, authority: Option<crate::terminal::close_authority::CloseAuthority>) -> O {\n        \
                 self.graceful_exit_driven(t, p, c, authority).await\n    }\n    \
                 async fn graceful_exit_driven(&self, t: T, p: P, c: C, authority: Option<crate::terminal::close_authority::CloseAuthority>) -> O {\n        \
                 drive_graceful_exit(|b| w(b, x), || s(), p, c, t, authority).await\n    }\n}\n",
            ),
            (
                "terminal/manager.rs",
                "impl TerminalManager {\n    pub async fn graceful_exit(\n        self: &Arc<Self>,\n        id: &str,\n        \
                 deadline: Duration,\n        authority: Option<crate::terminal::close_authority::CloseAuthority>,\n    ) -> R {\n        \
                 Ok(s.graceful_exit_then(d, move || async move { close(id, 1) }, authority).await)\n    }\n}\n",
            ),
            (
                "mcp/door.rs",
                "pub(crate) async fn door(tm: &T) {\n    let _ = tm\n        .graceful_exit(\n            id,\n            \
                 crate::terminal::graceful_exit::DEFAULT_DEADLINE,\n            None,\n        )\n        .await;\n}\n",
            ),
        ],
    ));
    let sorted = |v: &[String]| {
        let mut v = v.to_vec();
        v.sort();
        v
    };
    assert_eq!(scan.violations, Vec::<String>::new());
    assert_eq!(
        sorted(&scan.param_mentions),
        vec![
            "terminal/graceful_exit.rs:drive_graceful_exit",
            "terminal/manager.rs:graceful_exit",
            "terminal/session.rs:graceful_exit_driven",
            "terminal/session.rs:graceful_exit_then",
        ]
    );
    assert_eq!(
        sorted(&scan.forwarded_calls),
        vec![
            "terminal/manager.rs:graceful_exit",
            "terminal/session.rs:graceful_exit_driven",
            "terminal/session.rs:graceful_exit_then",
        ]
    );
    assert_eq!(scan.none_calls, vec!["mcp/door.rs:door"]);
    assert_eq!(
        sorted(&scan.entry_uses),
        vec![
            "terminal/graceful_exit.rs:drive_graceful_exit let-else",
            "terminal/manager.rs:graceful_exit forward",
            "terminal/session.rs:graceful_exit_driven forward",
            "terminal/session.rs:graceful_exit_then forward",
        ]
    );

    assert_red(
        &authority_tree(
            SEALED,
            &[
                (
                    "mcp/helper.rs",
                    "use crate::terminal::close_authority::CloseAuthority;\n\
                     pub(crate) fn pass(authority: Option<CloseAuthority>) {}\n",
                ),
                (
                    "terminal/manager.rs",
                    "impl Other {\n    pub async fn graceful_exit(&self, authority: Option<crate::terminal::close_authority::CloseAuthority>) {}\n}\n",
                ),
                (
                    "terminal/graceful_exit.rs",
                    "use crate::terminal::close_authority::CloseAuthority;\n\
                     pub(super) async fn drive_graceful_exit(authority: Option<CloseAuthority>) -> CloseAuthority {\n    \
                     let _held: Option<CloseAuthority> = None;\n    todo!()\n}\n",
                ),
            ],
        ),
        &[
            "mcp/helper.rs:2 (pass) names `CloseAuthority`",
            "terminal/manager.rs:2 (graceful_exit) names `CloseAuthority`",
            "terminal/graceful_exit.rs:2 (drive_graceful_exit) names `CloseAuthority`",
            "terminal/graceful_exit.rs:3 (drive_graceful_exit) names `CloseAuthority`",
        ],
    );
}

/// Rule 9's other arms. A fn value or a turbofish hides the argument, so both
/// are red. Only a primitive entry point may forward an `authority`, so a
/// helper that takes one and passes it on is red even when it never names the
/// type.
#[test]
fn hiding_or_relaying_the_authority_argument_is_red() {
    assert_red(
        &authority_tree(
            SEALED,
            &[
                (
                    "mcp/value.rs",
                    "pub(crate) fn as_value() {\n    let _f = crate::terminal::TerminalManager::graceful_exit;\n}\n",
                ),
                (
                    "mcp/relay.rs",
                    "pub(crate) async fn relay<A>(tm: &T, authority: Option<A>) {\n    \
                     let _ = tm.graceful_exit(id, d, authority).await;\n}\n",
                ),
                (
                    "terminal/session.rs",
                    "impl TerminalSession {\n    pub(super) async fn graceful_exit_then(&self, d: D, c: C, authority: Option<crate::terminal::close_authority::CloseAuthority>) -> O {\n        \
                     self.graceful_exit_driven::<P, C>(t, p, c, authority).await\n    }\n}\n",
                ),
            ],
        ),
        &[
            "mcp/value.rs:2 (as_value) names `graceful_exit` as a value",
            "mcp/relay.rs:2 (relay) passes `authority` as the authority of `graceful_exit(`",
            "terminal/session.rs:3 (graceful_exit_then) calls `graceful_exit_driven` with a \
             turbofish",
        ],
    );
}

/// Rule 9b: inside an entry point, `authority` must be the caller's value. A
/// `mut` parameter is red, so is the reassignment it allows, and so is a `let`
/// that shadows it, while the forward that follows each still passes rule 9.
#[test]
fn an_entry_point_that_rebinds_its_authority_is_red() {
    let violations = assert_red(
        &authority_tree(
            SEALED,
            &[
                (
                    "terminal/manager.rs",
                    "impl TerminalManager {\n    pub async fn graceful_exit(&self, id: &str, d: D, mut authority: Option<crate::terminal::close_authority::CloseAuthority>) -> R {\n        \
                     authority = forge();\n        s.graceful_exit_then(d, c, authority).await\n    }\n}\n",
                ),
                (
                    "terminal/session.rs",
                    "impl TerminalSession {\n    async fn graceful_exit_driven(&self, t: T, p: P, c: C, authority: Option<crate::terminal::close_authority::CloseAuthority>) -> O {\n        \
                     let authority = forge();\n        drive_graceful_exit(w, s, p, c, t, authority).await\n    }\n}\n",
                ),
            ],
        ),
        &[
            "terminal/manager.rs:graceful_exit declares its `authority` parameter `mut`",
            "terminal/manager.rs:3 (graceful_exit) uses `authority` other than as the forwarded argument",
            "terminal/session.rs:3 (graceful_exit_driven) uses `authority` other than as the forwarded argument",
        ],
    );
    assert_eq!(violations.len(), 3, "{violations:#?}");
}

/// Round-3 R3: `syn` does not parse a macro body, so a rebinding inside a
/// `tokio::select!` arm forwards a forged value the tree never shows. Rule 9b
/// reads the body's blanked text too. The control, the log field
/// `authority = authority.is_some()` in a tracing macro, stays green.
#[test]
fn a_rebinding_inside_a_macro_is_red() {
    let violations = assert_red(
        &authority_tree(
            SEALED,
            &[
                (
                    "terminal/manager.rs",
                    "impl TerminalManager {\n    pub async fn graceful_exit(&self, id: &str, d: D, authority: Option<crate::terminal::close_authority::CloseAuthority>) -> R {\n        \
                     tokio::select! {\n            _ = tick() => {\n                \
                     let authority = Some(unsafe { std::mem::zeroed() });\n                \
                     session.graceful_exit_then(d, close, authority).await\n            }\n        }\n    }\n}\n",
                ),
                (
                    "terminal/session.rs",
                    "impl TerminalSession {\n    async fn graceful_exit_driven(&self, t: T, p: P, c: C, authority: Option<crate::terminal::close_authority::CloseAuthority>) -> O {\n        \
                     info!(terminal_id = %self.id, authority = authority.is_some(), \"starting\");\n        \
                     drive_graceful_exit(w, s, p, c, t, authority).await\n    }\n}\n",
                ),
            ],
        ),
        &["terminal/manager.rs:5 (graceful_exit) uses `authority` other than as the forwarded argument"],
    );
    assert_eq!(violations.len(), 1, "{violations:#?}");
}

/// An entry point whose body runs `arm` inside a `tokio::select!` arm, which
/// `syn` does not parse. The arm sits alone on line 5.
fn select_arm_tree(arm: &str) -> Vec<Prepared> {
    let manager = format!(
        "impl TerminalManager {{\n    pub async fn graceful_exit(&self, id: &str, d: D, authority: Option<crate::terminal::close_authority::CloseAuthority>) -> R {{\n        \
         tokio::select! {{\n            _ = tick() => {{\n                {arm}\n            }}\n        }}\n    }}\n}}\n"
    );
    authority_tree(SEALED, &[("terminal/manager.rs", manager.as_str())])
}

/// Each round-4 F1 shape forges an authority inside a macro and forwards it.
/// The forward itself is an allowed use, so exactly the forging line is red.
fn assert_forged_in_a_macro_is_red(arm: &str) {
    let violations = close_authority_violations(&select_arm_tree(arm));
    assert_eq!(
        violations.len(),
        1,
        "exactly the forging line is red for {arm:?}: {violations:#?}"
    );
    assert!(
        violations[0].starts_with(
            "terminal/manager.rs:5 (graceful_exit) uses `authority` other than as the forwarded \
             argument"
        ),
        "{violations:#?}"
    );
}

#[test]
fn an_if_let_rebinding_inside_a_macro_is_red() {
    assert_forged_in_a_macro_is_red(
        "if let Some(authority) = Some(Some(unsafe { std::mem::zeroed() })) { session.graceful_exit_then(d, close, authority).await }",
    );
}

#[test]
fn a_for_rebinding_inside_a_macro_is_red() {
    assert_forged_in_a_macro_is_red(
        "for authority in [Some(unsafe { std::mem::zeroed() })] { session.graceful_exit_then(d, close, authority).await; }",
    );
}

#[test]
fn a_tuple_let_rebinding_inside_a_macro_is_red() {
    assert_forged_in_a_macro_is_red(
        "let (authority, _) = (Some(unsafe { std::mem::zeroed() }), 0); session.graceful_exit_then(d, close, authority).await",
    );
}

#[test]
fn a_match_arm_rebinding_inside_a_macro_is_red() {
    assert_forged_in_a_macro_is_red(
        "match Some(Some(unsafe { std::mem::zeroed() })) { Some(authority) => session.graceful_exit_then(d, close, authority).await, None => todo!() }",
    );
}

#[test]
fn a_closure_parameter_inside_a_macro_is_red() {
    assert_forged_in_a_macro_is_red(
        "[|authority| session.graceful_exit_then(d, close, authority)][0](Some(unsafe { std::mem::zeroed() })).await",
    );
}

/// The round-3 exemption had no end anchor, so this forge matched it. The log
/// field is now allowed only when `,` or `)` follows the read at once.
#[test]
fn an_unanchored_log_field_forge_is_red() {
    assert_forged_in_a_macro_is_red(
        "authority = authority.is_none().then(|| unsafe { std::mem::zeroed() }); session.graceful_exit_then(d, close, authority).await",
    );
}

/// Round 5: a `#[macro_export]` macro defined OUTSIDE `terminal/` can expand
/// to a call of the driver inside `crate::terminal`, where its privacy holds.
/// The expansion site names nothing, so the call is read where it is written,
/// in the macro's body. Forging `Some(…)` there is red, read at the
/// `authority` parameter's place in the driver's own signature.
#[test]
fn a_macro_defined_outside_terminal_that_calls_the_driver_is_red() {
    let violations = assert_red(
        &authority_tree(
            SEALED,
            &[
                (
                    "terminal/graceful_exit.rs",
                    "use crate::terminal::close_authority::CloseAuthority;\n\
                     pub(super) async fn drive_graceful_exit<W>(\n    write: W,\n    screen: S,\n    \
                     probe: P,\n    close_tab: C,\n    timing: T,\n    \
                     authority: Option<CloseAuthority>,\n) -> O {\n    todo!()\n}\n",
                ),
                (
                    "ipc_forge.rs",
                    "#[macro_export]\nmacro_rules! forge_close {\n    ($w:expr) => {\n        \
                     $crate::terminal::graceful_exit::drive_graceful_exit($w, s, p, c, t, Some(unsafe { ::std::mem::zeroed() }))\n    \
                     };\n}\n",
                ),
                (
                    "terminal/caller.rs",
                    "pub(super) async fn close_it(w: W) {\n    let _ = crate::forge_close!(w).await;\n}\n",
                ),
            ],
        ),
        &["ipc_forge.rs:4 (<file scope>) passes `Some(unsafe { ::std::mem::zeroed() })` as the \
           authority of `drive_graceful_exit(`"],
    );
    assert_eq!(
        violations.len(),
        1,
        "the macro's body is the only red; its expansion site names nothing: {violations:#?}"
    );
}

/// Round 5: a `use … as` rename of the driver, or a fn value of it, outside
/// `terminal/` is red. A call through the new name, or through the value,
/// would never be read.
#[test]
fn a_rename_or_a_fn_value_of_the_driver_is_red() {
    assert_red(
        &authority_tree(
            SEALED,
            &[
                (
                    "mcp/alias.rs",
                    "use crate::terminal::graceful_exit::drive_graceful_exit as d;\n\
                     pub(crate) async fn go(w: W) {\n    let _ = d(w, s, p, c, t, Some(forge())).await;\n}\n",
                ),
                (
                    "mcp/value.rs",
                    "pub(crate) fn hold() {\n    let _d = crate::terminal::graceful_exit::drive_graceful_exit;\n}\n",
                ),
            ],
        ),
        &[
            "mcp/alias.rs:1 `use … as d` renames `drive_graceful_exit` — a call through the new \
             name is not read",
            "mcp/value.rs:2 (hold) names `drive_graceful_exit` as a value",
        ],
    );
}

/// Round-3 R4(a): rule 2 refuses a trait that can build or duplicate an
/// authority, and only that. `Drop` (like `Debug` and `Display`) only
/// borrows a value that exists, so `impl Drop` is green and `impl Default` is
/// red.
#[test]
fn a_drop_impl_is_green_and_a_default_impl_is_red() {
    let drop =
        format!("{SEALED}impl Drop for CloseAuthority {{\n    fn drop(&mut self) {{}}\n}}\n");
    assert_eq!(
        close_authority_violations(&authority_tree(&drop, &[])),
        Vec::<String>::new()
    );
    let default = format!(
        "{SEALED}impl Default for CloseAuthority {{\n    fn default() -> Self {{\n        todo!()\n    }}\n}}\n"
    );
    assert_red(
        &authority_tree(&default, &[]),
        &["terminal/close_authority.rs:11 impl Default for CloseAuthority"],
    );
}

/// Round-3 R4(b): rule 9 reads the authority at the `authority` parameter's
/// place in the entry point's own signature. With a parameter appended after
/// it (R0b-1's `door`), a call passing `None` there stays green, a path call
/// counts its receiver, and a forged value there is still red.
#[test]
fn the_authority_is_read_at_its_place_in_the_signature() {
    let tree = authority_tree(
        SEALED,
        &[
            (
                "terminal/manager.rs",
                "impl TerminalManager {\n    pub async fn graceful_exit(\n        &self,\n        id: &str,\n        \
                 authority: Option<crate::terminal::close_authority::CloseAuthority>,\n        door: Door,\n    ) -> R {\n        \
                 todo!()\n    }\n}\n",
            ),
            (
                "mcp/door.rs",
                "pub(crate) async fn by_method(tm: &T) {\n    let _ = tm.graceful_exit(id, None, Door::Agent).await;\n}\n\
                 pub(crate) async fn by_path(tm: &T) {\n    let _ = TerminalManager::graceful_exit(tm, id, None, Door::Agent).await;\n}\n\
                 pub(crate) async fn forged(tm: &T) {\n    let _ = tm.graceful_exit(id, Some(forge()), Door::Agent).await;\n}\n",
            ),
        ],
    );
    let scan = close_authority_scan(&tree);
    assert_eq!(
        scan.none_calls,
        vec!["mcp/door.rs:by_method", "mcp/door.rs:by_path"]
    );
    assert_eq!(
        scan.violations.len(),
        1,
        "only the forged call is red: {:#?}",
        scan.violations
    );
    assert!(scan.violations[0].starts_with(
        "mcp/door.rs:8 (forged) passes `Some(forge())` as the authority of `graceful_exit(`"
    ));
}

/// Round-3 R4(c): only what can be a fn value is read as one. A path
/// `TerminalManager::graceful_exit` handed to `map` is red. A struct field
/// named `graceful_exit`, its access `s.graceful_exit`, and a literal setting
/// it are not fn values, so they are green.
#[test]
fn a_path_fn_value_is_red_and_a_field_of_that_name_is_green() {
    assert_red(
        &authority_tree(
            SEALED,
            &[(
                "mcp/value.rs",
                "pub(crate) fn all(v: Vec<T>) {\n    let _ = v.into_iter().map(TerminalManager::graceful_exit);\n}\n",
            )],
        ),
        &["mcp/value.rs:2 (all) names `graceful_exit` as a value"],
    );
    assert_eq!(
        close_authority_violations(&authority_tree(
            SEALED,
            &[(
                "mcp/flags.rs",
                "pub struct Flags {\n    pub graceful_exit: bool,\n}\n\
                 pub(crate) fn read(s: &Flags) -> bool {\n    s.graceful_exit\n}\n\
                 pub(crate) fn make() -> Flags {\n    Flags { graceful_exit: true }\n}\n",
            )],
        )),
        Vec::<String>::new()
    );
}

/// Rules 8 and 9 read code, not text. The type named in a string, a log
/// message, a raw string or a trailing or block comment is no mention, and a
/// comma inside a string does not split rule 9's arguments.
#[test]
fn a_mention_in_a_string_or_a_comment_is_not_a_mention() {
    let violations = close_authority_violations(&authority_tree(
        SEALED,
        &[(
            "mcp/talk.rs",
            "pub(crate) async fn talk<'a>(tm: &'a T) {\n    \
             warn!(\"no CloseAuthority can be presented, so nothing was typed\");\n    \
             let _ = tm.graceful_exit(id, \"a, Some(x)\", None).await; // CloseAuthority\n    \
             let _r = r#\"CloseAuthority { _sealed: () }\"#;\n    \
             let _c = '\"'; /* CloseAuthority { _sealed: () } */\n}\n",
        )],
    ));
    assert_eq!(violations, Vec::<String>::new());
}

/// The blanking every rule 8 and 9 read rests on: offsets unchanged, literals
/// and comments (nested ones included) gone, lifetimes and code kept.
#[test]
fn blanking_keeps_every_offset_and_reads_lifetimes_as_code() {
    let src = "fn f<'a>(x: &'a str) -> char { let s = \"Close\\\"Authority\"; \
               let r = r#\"a \"b\"#; let c = '\\''; let d = 'é'; x.len(); 'q' } // tail\n\
               /* outer /* inner */ still */ g();\n";
    let blanked = blank_literals_and_comments(src);
    assert_eq!(blanked.len(), src.len());
    assert_eq!(blanked.lines().count(), src.lines().count());
    assert!(
        blanked.contains("fn f<'a>(x: &'a str) -> char"),
        "{blanked}"
    );
    assert!(
        blanked.contains("x.len();") && blanked.contains("g();"),
        "{blanked}"
    );
    for gone in ["Authority", "\"b", "tail", "inner", "still", "é"] {
        assert!(!blanked.contains(gone), "{gone:?} survived: {blanked}");
    }
}
