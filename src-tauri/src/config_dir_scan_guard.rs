//! Source invariant: no runner code reaches the operator's REAL config dir
//! except through a resolver that a test harness deflects.
//!
//! On 2026-09-23 a runner test binary wrote a defaults document over the
//! operator's live `settings.json` five times in one afternoon, and a second,
//! unfiltered `cargo test` from another worktree did it again after the first
//! route was cut (plan
//! `2026-09-23-runner-unit-tests-overwrite-the-operators-live-settings-json`).
//! Phases 2-3 of that plan made the two `settings.json` resolvers hermetic in a
//! test process. That closes `settings.json`. It does not close the CLASS: any
//! other function that spells `dirs::config_dir()` and joins
//! `com.qontinui.runner` onto it reads and writes the operator's real
//! directory from a test just as the settings load did — and Phase 4 found
//! eleven such functions (the prompt library, prompt snippets, the context
//! library, playwright storage, the config library, the active-instance
//! ledger, the agent command and skill caches, the backup set, and two lib
//! readers of the account roster), every one of which is now routed through
//! `ambient::runner_platform_config_root`.
//!
//! This test is what keeps that true. It enumerates EVERY `dirs::config_dir`
//! (and `dirs::config_local_dir`) reference under `src/` and requires each one
//! to be classified in [`ALLOWLIST`]:
//!
//! * [`Class::Resolver`] — one of the three functions that ARE the guarded
//!   door: the two `settings.json` resolvers and
//!   `ambient::runner_platform_config_root`. Each consults the test-harness
//!   deflection before the platform dir.
//! * [`Class::ForeignApp`] — another application's config dir (Claude
//!   Desktop, VS Code's Cline storage). Not the runner's, so the deflection
//!   has nothing to say about it; the writers there are tested through their
//!   `*_at_path` twins.
//! * [`Class::TestAssertion`] — a test that reads the real path only to
//!   assert it was NOT resolved. Never written.
//! * [`Class::CiSentinelProbe`] — the one deliberate raw write, env-gated and
//!   `#[ignore]`d, that proves CI's sentinel step is live.
//!
//! A runner-config site is never allowlisted: it goes through
//! `qontinui_runner_lib::ambient::runner_platform_config_root(source)` (a root
//! that ignores `QONTINUI_CONFIG_DIR`, as all eleven did) or through
//! `settings::get_config_dir` (one that honours it). An unclassified site
//! fails BY NAME — file, line and enclosing fn — and so does an allowlist row
//! that no longer matches anything, so the list cannot rot into a list of
//! permissions nobody uses.
//!
//! # What counts as a reference
//!
//! Any path whose last two segments are `dirs` and `config_dir` /
//! `config_local_dir`: a call (`dirs::config_dir()`), a fn value
//! (`.or_else(dirs::config_dir)`), or the same tokens inside a macro's
//! arguments (`assert_ne!(x, dirs::config_dir())`). A `use dirs::config_dir`
//! (or a grouped / renamed import of it) is recorded too, at the `use`, since
//! after it a bare `config_dir()` is invisible. Comments and string literals
//! are not code and are not seen — the parse is `syn`, not a text grep, for
//! exactly that reason.
//!
//! # The second invariant: `QONTINUI_CONFIG_DIR` writes restore the var
//!
//! Every test that writes `QONTINUI_CONFIG_DIR` (a `set_var` / `remove_var`
//! whose first argument is that literal, in the test's own body or a
//! same-file helper it calls) must also restore it: an
//! `EnvVarRestore::capture(..)` or the `IsolatedAmbient` fixture (which
//! restores every key it touches on Drop), in its own body or a same-file
//! helper it calls. `env_write_lock_guard` already requires the env LOCK; a
//! locked write that is never undone is still a leak into every later test,
//! and a leaked fixture dir is how the recorded race handed a sibling's empty
//! directory to a `FreshInstall` read.
//!
//! # Known limits
//!
//! * An alias (`use dirs as d; d::config_dir()`) or a re-export under another
//!   name is missed. The crate has none today.
//! * Other platform dirs (`dirs::data_dir`, `dirs::data_local_dir`) are out of
//!   scope: the recorded defect is the config dir, and several data-dir users
//!   (`pair.rs`, `auth.rs`, `embedded_pg.rs`) have their own env overrides.
//! * Restore-credit is by NAME, like the lock guard's: a capture that does not
//!   include `QONTINUI_CONFIG_DIR` still credits the test.
//! * Same-file calls are resolved by the callee's bare name (a method call is
//!   not followed at all), so two same-named fns in one file share their facts.
//!
//! Registered in `main.rs` only, beside `env_write_lock_guard`, whose walker it
//! reuses: it walks all of `src`, lib files included.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;

use proc_macro2::{TokenStream, TokenTree};
use syn::visit::{self, Visit};

use crate::env_write_lock_guard::{is_test_attr, rs_files};

/// Why a raw `dirs::config_dir` reference is allowed where it stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Class {
    /// The guarded door itself: it asks the test-harness deflection first.
    Resolver,
    /// Another application's config dir, not the runner's.
    ForeignApp,
    /// A test reading the real path only to assert it was not resolved.
    TestAssertion,
    /// The env-gated, ignored probe that proves the CI sentinel is live.
    CiSentinelProbe,
}

/// Every allowed raw reference: `(file relative to src/, enclosing fn, class,
/// reason)`. The fn is `name` for a free fn and `Type::name` for an impl
/// method; `<use>` for an import. One row covers every reference in that fn.
const ALLOWLIST: &[(&str, &str, Class, &str)] = &[
    (
        "ambient.rs",
        "runner_platform_config_root",
        Class::Resolver,
        "the platform-root door every runner-config path goes through; \
         deflects in a test harness via test_config_root_override",
    ),
    (
        "settings.rs",
        "resolve_config_dir",
        Class::Resolver,
        "the bin's settings.json resolver; consults test_config_dir_override \
         before the platform dir",
    ),
    (
        "profiles.rs",
        "settings_json_path",
        Class::Resolver,
        "the lib's settings.json resolver; consults test_config_dir_override \
         before the platform dir",
    ),
    (
        "wrappers/clients.rs",
        "claude_desktop_path",
        Class::ForeignApp,
        "Claude Desktop's own config dir (<config>/Claude); writers are tested \
         through connect_at_path / disconnect_at_path",
    ),
    (
        "wrappers/clients.rs",
        "cline_path",
        Class::ForeignApp,
        "VS Code's globalStorage for the Cline extension (<config>/Code/User)",
    ),
    (
        "settings.rs",
        "an_unguarded_resolution_with_the_var_unset_is_deflected",
        Class::TestAssertion,
        "reads the real dir only to assert the resolved dir is NOT it",
    ),
    (
        "claude_accounts.rs",
        "canonical_path_ignores_qontinui_config_dir",
        Class::TestAssertion,
        "reads the real dir only to assert the roster path is NOT under it",
    ),
];

/// Floor for the walk, so a broken path or filter cannot pass vacuously. The
/// sibling guards walk the same tree and declare the same floor.
const MIN_FILES_WALKED: usize = 1000;

/// The platform-dir fns this guard polices.
const CONFIG_DIR_FNS: [&str; 2] = ["config_dir", "config_local_dir"];

/// The env var the second invariant is about.
const CONFIG_DIR_VAR: &str = "QONTINUI_CONFIG_DIR";

/// Names that restore `QONTINUI_CONFIG_DIR` when mentioned in a body.
const RESTORERS: [&str; 3] = ["EnvVarRestore", "IsolatedAmbient", "isolated_ambient"];

/// One raw reference, located.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Site {
    line: usize,
    /// Enclosing fn key, or `<use>` / `<item>` outside any fn.
    func: String,
}

/// What one fn body does, for the restore invariant.
#[derive(Debug, Default, Clone)]
struct BodyFacts {
    writes_config_dir_var: bool,
    restores: bool,
    calls: BTreeSet<String>,
}

/// One fn anywhere in the file.
struct FnRecord {
    key: String,
    name: String,
    line: usize,
    is_test: bool,
    facts: BodyFacts,
}

/// Everything the scan learned about one file.
#[derive(Debug, Default)]
struct FileReport {
    sites: Vec<Site>,
    /// Test fns that write `QONTINUI_CONFIG_DIR` without restoring it.
    unrestored: Vec<(usize, String)>,
    /// Test fns that write it at all (restored or not), for non-vacuity.
    config_dir_writing_tests: usize,
}

fn is_config_dir_path(path: &syn::Path) -> bool {
    let n = path.segments.len();
    n >= 2
        && path.segments[n - 2].ident == "dirs"
        && CONFIG_DIR_FNS
            .iter()
            .any(|f| path.segments[n - 1].ident == f)
}

/// `dirs :: config_dir` as a raw token sequence (macro arguments are not
/// parsed as expressions). Returns the line of every match.
fn token_sites(tokens: TokenStream, out: &mut Vec<usize>) {
    let toks: Vec<TokenTree> = tokens.into_iter().collect();
    for (i, t) in toks.iter().enumerate() {
        match t {
            TokenTree::Group(g) => token_sites(g.stream(), out),
            TokenTree::Ident(id) if id == "dirs" => {
                let colons = matches!(toks.get(i + 1), Some(TokenTree::Punct(p)) if p.as_char() == ':')
                    && matches!(toks.get(i + 2), Some(TokenTree::Punct(p)) if p.as_char() == ':');
                let target = matches!(toks.get(i + 3),
                    Some(TokenTree::Ident(f)) if CONFIG_DIR_FNS.iter().any(|n| f == n));
                if colons && target {
                    out.push(id.span().start().line);
                }
            }
            _ => {}
        }
    }
}

/// Facts from raw macro tokens: a `set_var`/`remove_var` followed by a group
/// whose first token is the literal, and any restorer name.
fn token_facts(tokens: TokenStream, facts: &mut BodyFacts) {
    let toks: Vec<TokenTree> = tokens.into_iter().collect();
    for (i, t) in toks.iter().enumerate() {
        match t {
            TokenTree::Group(g) => token_facts(g.stream(), facts),
            TokenTree::Ident(id) => {
                let s = id.to_string();
                if RESTORERS.contains(&s.as_str()) {
                    facts.restores = true;
                }
                if s == "set_var" || s == "remove_var" {
                    if let Some(TokenTree::Group(g)) = toks.get(i + 1) {
                        if first_token_is_var_literal(g.stream()) {
                            facts.writes_config_dir_var = true;
                        }
                    }
                } else if let Some(TokenTree::Group(g)) = toks.get(i + 1) {
                    if g.delimiter() == proc_macro2::Delimiter::Parenthesis {
                        facts.calls.insert(s);
                    }
                }
            }
            _ => {}
        }
    }
}

fn first_token_is_var_literal(tokens: TokenStream) -> bool {
    matches!(tokens.into_iter().next(),
        Some(TokenTree::Literal(l)) if l.to_string() == format!("\"{CONFIG_DIR_VAR}\""))
}

/// Scans one fn body for the restore invariant's facts.
#[derive(Default)]
struct BodyScanner {
    facts: BodyFacts,
}

impl<'ast> Visit<'ast> for BodyScanner {
    fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
        if let syn::Expr::Path(p) = &*call.func {
            if let Some(last) = p.path.segments.last() {
                let name = last.ident.to_string();
                if name == "set_var" || name == "remove_var" {
                    if let Some(syn::Expr::Lit(syn::ExprLit {
                        lit: syn::Lit::Str(s),
                        ..
                    })) = call.args.first()
                    {
                        if s.value() == CONFIG_DIR_VAR {
                            self.facts.writes_config_dir_var = true;
                        }
                    }
                } else {
                    self.facts.calls.insert(name);
                }
            }
        }
        visit::visit_expr_call(self, call);
    }

    fn visit_path(&mut self, p: &'ast syn::Path) {
        if p.segments
            .iter()
            .any(|s| RESTORERS.iter().any(|r| s.ident == r))
        {
            self.facts.restores = true;
        }
        visit::visit_path(self, p);
    }

    fn visit_macro(&mut self, m: &'ast syn::Macro) {
        token_facts(m.tokens.clone(), &mut self.facts);
        visit::visit_macro(self, m);
    }
}

/// Walks a file: records every raw reference under its enclosing fn, and
/// every fn's restore facts.
#[derive(Default)]
struct Collector {
    /// Enclosing impl self type (innermost last); `None` inside a fn body.
    impl_scope: Vec<Option<String>>,
    /// Enclosing fn keys, innermost last.
    fn_stack: Vec<String>,
    sites: Vec<Site>,
    fns: Vec<FnRecord>,
}

impl Collector {
    fn current_fn(&self) -> String {
        self.fn_stack
            .last()
            .cloned()
            .unwrap_or_else(|| "<item>".to_string())
    }

    fn enter(&mut self, ident: &syn::Ident, attrs: &[syn::Attribute], block: &syn::Block) {
        let self_ty = self.impl_scope.last().cloned().flatten();
        let name = ident.to_string();
        let key = match &self_ty {
            Some(t) => format!("{t}::{name}"),
            None => name.clone(),
        };
        let mut scanner = BodyScanner::default();
        scanner.visit_block(block);
        self.fns.push(FnRecord {
            key: key.clone(),
            name,
            line: ident.span().start().line,
            is_test: attrs.iter().any(is_test_attr),
            facts: scanner.facts,
        });
        self.fn_stack.push(key);
        self.impl_scope.push(None);
    }

    fn leave(&mut self) {
        self.fn_stack.pop();
        self.impl_scope.pop();
    }

    fn use_tree_imports_config_dir(tree: &syn::UseTree, under_dirs: bool) -> bool {
        match tree {
            syn::UseTree::Path(p) => {
                Self::use_tree_imports_config_dir(&p.tree, under_dirs || p.ident == "dirs")
            }
            syn::UseTree::Name(n) => under_dirs && CONFIG_DIR_FNS.iter().any(|f| n.ident == f),
            syn::UseTree::Rename(r) => under_dirs && CONFIG_DIR_FNS.iter().any(|f| r.ident == f),
            syn::UseTree::Glob(_) => under_dirs,
            syn::UseTree::Group(g) => g
                .items
                .iter()
                .any(|t| Self::use_tree_imports_config_dir(t, under_dirs)),
        }
    }
}

impl<'ast> Visit<'ast> for Collector {
    fn visit_item_fn(&mut self, f: &'ast syn::ItemFn) {
        self.enter(&f.sig.ident, &f.attrs, &f.block);
        visit::visit_item_fn(self, f);
        self.leave();
    }

    fn visit_item_impl(&mut self, i: &'ast syn::ItemImpl) {
        let ty = match &*i.self_ty {
            syn::Type::Path(tp) => tp.path.segments.last().map(|s| s.ident.to_string()),
            _ => None,
        };
        self.impl_scope.push(ty);
        visit::visit_item_impl(self, i);
        self.impl_scope.pop();
    }

    fn visit_impl_item_fn(&mut self, f: &'ast syn::ImplItemFn) {
        self.enter(&f.sig.ident, &f.attrs, &f.block);
        visit::visit_impl_item_fn(self, f);
        self.leave();
    }

    fn visit_trait_item_fn(&mut self, f: &'ast syn::TraitItemFn) {
        if let Some(block) = &f.default {
            self.enter(&f.sig.ident, &f.attrs, block);
            visit::visit_trait_item_fn(self, f);
            self.leave();
        } else {
            visit::visit_trait_item_fn(self, f);
        }
    }

    fn visit_item_use(&mut self, u: &'ast syn::ItemUse) {
        if Self::use_tree_imports_config_dir(&u.tree, false) {
            self.sites.push(Site {
                line: u.use_token.span.start().line,
                func: "<use>".to_string(),
            });
        }
        visit::visit_item_use(self, u);
    }

    fn visit_path(&mut self, p: &'ast syn::Path) {
        if is_config_dir_path(p) {
            let line = p
                .segments
                .first()
                .map(|s| s.ident.span().start().line)
                .unwrap_or(0);
            self.sites.push(Site {
                line,
                func: self.current_fn(),
            });
        }
        visit::visit_path(self, p);
    }

    fn visit_macro(&mut self, m: &'ast syn::Macro) {
        let mut lines = Vec::new();
        token_sites(m.tokens.clone(), &mut lines);
        let func = self.current_fn();
        self.sites.extend(lines.into_iter().map(|line| Site {
            line,
            func: func.clone(),
        }));
        visit::visit_macro(self, m);
    }
}

/// Close `flag` transitively over same-file calls.
fn close_over_calls(fns: &[FnRecord], mut flag: Vec<bool>) -> Vec<bool> {
    let mut by_name: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, f) in fns.iter().enumerate() {
        by_name.entry(f.name.as_str()).or_default().push(i);
        if f.key != f.name {
            by_name.entry(f.key.as_str()).or_default().push(i);
        }
    }
    loop {
        let mut changed = false;
        for (i, f) in fns.iter().enumerate() {
            if flag[i] {
                continue;
            }
            let reached = f.facts.calls.iter().any(|c| {
                by_name
                    .get(c.as_str())
                    .is_some_and(|ix| ix.iter().any(|&j| j != i && flag[j]))
            });
            if reached {
                flag[i] = true;
                changed = true;
            }
        }
        if !changed {
            return flag;
        }
    }
}

/// Scan one file's source. `Err` only when it does not parse.
fn scan_source(src: &str) -> syn::Result<FileReport> {
    let file = syn::parse_file(src)?;
    let mut c = Collector::default();
    c.visit_file(&file);

    let writes = close_over_calls(
        &c.fns,
        c.fns
            .iter()
            .map(|f| f.facts.writes_config_dir_var)
            .collect(),
    );
    let restores = close_over_calls(&c.fns, c.fns.iter().map(|f| f.facts.restores).collect());

    let mut report = FileReport {
        sites: c.sites,
        ..FileReport::default()
    };
    report.sites.sort();
    report.sites.dedup();
    for (i, f) in c.fns.iter().enumerate() {
        if f.is_test && writes[i] {
            report.config_dir_writing_tests += 1;
            if !restores[i] {
                report.unrestored.push((f.line, f.key.clone()));
            }
        }
    }
    report.unrestored.sort();
    Ok(report)
}

/// Classify `(file, site)` pairs against `allowlist`. Returns the
/// unclassified sites (`src/<file>:<line> in <fn>`) and the allowlist rows
/// that matched nothing (`<file> :: <fn>`).
fn classify(
    sites: &[(String, Site)],
    allowlist: &[(&str, &str, Class, &str)],
) -> (Vec<String>, Vec<String>) {
    let mut used = vec![false; allowlist.len()];
    let mut unclassified = Vec::new();
    for (file, site) in sites {
        match allowlist
            .iter()
            .position(|(f, func, _, _)| f == file && *func == site.func)
        {
            Some(ix) => used[ix] = true,
            None => unclassified.push(format!("src/{file}:{} in `{}`", site.line, site.func)),
        }
    }
    let stale = allowlist
        .iter()
        .zip(&used)
        .filter(|(_, u)| !**u)
        .map(|((f, func, _, _), _)| format!("{f} :: {func}"))
        .collect();
    (unclassified, stale)
}

/// Is `file` a candidate at all? A file mentioning neither `config_dir` nor
/// the env var cannot hold a site or a write under the rules above.
fn needs_parse(src: &str) -> bool {
    CONFIG_DIR_FNS.iter().any(|f| src.contains(f)) || src.contains(CONFIG_DIR_VAR)
}

#[test]
fn every_raw_config_dir_reference_is_classified_and_every_config_dir_write_restores() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let files = rs_files(&root);
    assert!(
        files.len() > MIN_FILES_WALKED,
        "walked only {} .rs files under {} — the guard scanned nothing",
        files.len(),
        root.display()
    );

    let mut sites: Vec<(String, Site)> = Vec::new();
    let mut unrestored: Vec<String> = Vec::new();
    let mut config_dir_writing_tests = 0usize;
    for file in &files {
        let rel = file
            .strip_prefix(&root)
            .unwrap_or(file)
            .to_string_lossy()
            .replace('\\', "/");
        let src =
            std::fs::read_to_string(file).unwrap_or_else(|e| panic!("reading src/{rel}: {e}"));
        if !needs_parse(&src) {
            continue;
        }
        let report = scan_source(&src).unwrap_or_else(|e| {
            panic!(
                "src/{rel}:{}: could not parse with syn ({e}) — the config-dir guard cannot \
                 vouch for a file it cannot read, so it refuses rather than skipping it",
                e.span().start().line
            )
        });
        config_dir_writing_tests += report.config_dir_writing_tests;
        unrestored.extend(
            report
                .unrestored
                .into_iter()
                .map(|(line, name)| format!("src/{rel}:{line} {name}")),
        );
        sites.extend(report.sites.into_iter().map(|s| (rel.clone(), s)));
    }

    // Non-vacuity: the three resolvers must be SEEN, or the detector broke
    // (the stale-row check below says so by name too), and the writers of
    // QONTINUI_CONFIG_DIR this tree is known to have must be found.
    eprintln!(
        "raw dirs::config_dir references: {}; QONTINUI_CONFIG_DIR-writing tests: {}",
        sites.len(),
        config_dir_writing_tests
    );
    assert!(
        config_dir_writing_tests >= 5,
        "found only {config_dir_writing_tests} tests writing QONTINUI_CONFIG_DIR — the \
         detector has stopped recognising them, so an empty offender list proves nothing"
    );

    let (unclassified, stale) = classify(&sites, ALLOWLIST);
    assert!(
        unclassified.is_empty(),
        "{} unclassified raw `dirs::config_dir` reference(s):\n  {}\n\n\
         In a test process `dirs::config_dir()` is the operator's REAL config dir — on \
         Windows no env override can redirect it — and a runner writer rooted there is \
         how a test binary overwrote the live settings.json (plan \
         2026-09-23-runner-unit-tests-overwrite-the-operators-live-settings-json).\n\
         Fix: a runner path (anything under `com.qontinui.runner`) takes \
         `qontinui_runner_lib::ambient::runner_platform_config_root(\"<module>::<fn>\")` \
         (`crate::ambient::…` in the lib) — or `settings::get_config_dir()` if it should \
         honour QONTINUI_CONFIG_DIR. Only ANOTHER app's dir may stay raw: add an ALLOWLIST \
         row in src/config_dir_scan_guard.rs classifying it, with the reason.",
        unclassified.len(),
        unclassified.join("\n  ")
    );
    assert!(
        stale.is_empty(),
        "ALLOWLIST row(s) in src/config_dir_scan_guard.rs match no reference any more:\n  {}\n\
         Delete the row (or fix its file/fn if the site moved) — an unused permission is \
         how an allowlist rots.",
        stale.join("\n  ")
    );
    assert!(
        unrestored.is_empty(),
        "{} test fn(s) write QONTINUI_CONFIG_DIR and never restore it:\n  {}\n\
         Hold `let _restore = crate::test_env::EnvVarRestore::capture(&[\"QONTINUI_CONFIG_DIR\"]);` \
         (after the env lock, so it drops first) or use the `IsolatedAmbient` fixture. A \
         leaked fixture dir is how the recorded race handed a sibling's empty directory to \
         a FreshInstall read.",
        unrestored.len(),
        unrestored.join("\n  ")
    );
}

/// The guard must be SEEN to fail: run the same scanner over synthetic source
/// covering every shape it claims to handle. The source is a raw STRING,
/// which is why the real-tree scan does not flag these deliberate offenders.
#[test]
fn the_guard_flags_unclassified_references_and_unrestored_writes() {
    const SRC: &str = r#"
use dirs::config_dir;
use dirs::{home_dir, config_local_dir as cld};

// dirs::config_dir() in a comment is not code.
const DOC: &str = "dirs::config_dir()";

fn unguarded_runner_path() -> Option<std::path::PathBuf> {
    dirs::config_dir().map(|d| d.join("com.qontinui.runner").join("prompts.json"))
}

fn as_a_fn_value() -> Option<std::path::PathBuf> {
    None.or_else(dirs::config_dir)
}

fn inside_a_macro() {
    assert!(dirs::config_dir().is_some(), "x");
}

struct Store;
impl Store {
    fn new() -> Option<std::path::PathBuf> {
        dirs::config_local_dir()
    }
}

fn allowed_foreign() -> Option<std::path::PathBuf> {
    dirs::config_dir().map(|d| d.join("Claude"))
}

fn helper_writes() {
    std::env::set_var("QONTINUI_CONFIG_DIR", "/tmp/x");
}

#[cfg(test)]
mod tests {
    #[test]
    fn writes_without_restore() {
        let _g = env_lock();
        std::env::set_var("QONTINUI_CONFIG_DIR", "/tmp/x");
    }

    #[test]
    fn writes_through_a_helper_without_restore() {
        let _g = env_lock();
        super::helper_writes();
    }

    #[test]
    fn removes_without_restore() {
        let _g = env_lock();
        std::env::remove_var("QONTINUI_CONFIG_DIR");
    }

    #[test]
    fn writes_with_restore() {
        let _g = env_lock();
        let _r = crate::test_env::EnvVarRestore::capture(&["QONTINUI_CONFIG_DIR"]);
        std::env::set_var("QONTINUI_CONFIG_DIR", "/tmp/x");
    }

    #[test]
    fn writes_under_the_fixture() {
        let _amb = crate::test_env::isolated_ambient();
        std::env::remove_var("QONTINUI_CONFIG_DIR");
    }

    #[test]
    fn writes_another_var() {
        let _g = env_lock();
        std::env::set_var("XDG_CONFIG_HOME", "/tmp/x");
    }
}
"#;
    let report = scan_source(SRC).expect("synthetic source parses");
    let sites: Vec<(String, Site)> = report
        .sites
        .iter()
        .cloned()
        .map(|s| ("synthetic.rs".to_string(), s))
        .collect();
    let allow: &[(&str, &str, Class, &str)] = &[
        (
            "synthetic.rs",
            "allowed_foreign",
            Class::ForeignApp,
            "fixture",
        ),
        ("synthetic.rs", "no_such_fn", Class::Resolver, "fixture"),
    ];
    let (unclassified, stale) = classify(&sites, allow);

    let flagged: BTreeSet<String> = sites
        .iter()
        .filter(|(_, s)| {
            unclassified
                .iter()
                .any(|u| u.ends_with(&format!("`{}`", s.func)))
        })
        .map(|(_, s)| s.func.clone())
        .collect();
    let expected: BTreeSet<String> = [
        "<use>",
        "unguarded_runner_path",
        "as_a_fn_value",
        "inside_a_macro",
        "Store::new",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    assert_eq!(flagged, expected, "unclassified: {unclassified:?}");
    assert_eq!(
        sites.iter().filter(|(_, s)| s.func == "<use>").count(),
        2,
        "both the plain and the grouped/renamed import are recorded: {sites:?}"
    );
    assert!(
        unclassified
            .iter()
            .any(|u| u.starts_with("src/synthetic.rs:9 in `unguarded_runner_path`")),
        "an unguarded site is named by file, line and fn: {unclassified:?}"
    );
    assert!(
        !sites.iter().any(|(_, s)| s.line == 5 || s.line == 6),
        "a comment or a string literal is not a reference: {sites:?}"
    );
    assert!(
        !unclassified.iter().any(|u| u.contains("allowed_foreign")),
        "an allowlisted site passes"
    );
    assert_eq!(stale, vec!["synthetic.rs :: no_such_fn".to_string()]);

    let unrestored: BTreeSet<&str> = report.unrestored.iter().map(|(_, n)| n.as_str()).collect();
    assert_eq!(
        unrestored,
        [
            "removes_without_restore",
            "writes_through_a_helper_without_restore",
            "writes_without_restore",
        ]
        .into_iter()
        .collect(),
    );
    assert_eq!(
        report.config_dir_writing_tests, 5,
        "five tests write the var (restored or not); writing another var does not count"
    );
}

/// Every row in the real allowlist carries a non-empty reason, no row is
/// duplicated, and exactly the three known resolvers are classed
/// [`Class::Resolver`] — a fourth "resolver" is a runner-config site wearing
/// the one class that lets it stay raw.
#[test]
fn every_allowlist_row_carries_a_reason_and_only_the_three_resolvers_are_resolvers() {
    let mut seen = BTreeSet::new();
    let mut per_class: std::collections::BTreeMap<Class, usize> = Default::default();
    for (file, func, class, reason) in ALLOWLIST {
        assert!(!reason.trim().is_empty(), "{file} :: {func} has no reason");
        assert!(
            seen.insert((*file, *func)),
            "{file} :: {func} is listed twice"
        );
        *per_class.entry(*class).or_default() += 1;
    }
    eprintln!("config-dir allowlist by class: {per_class:?}");
    let resolvers: BTreeSet<(&str, &str)> = ALLOWLIST
        .iter()
        .filter(|(_, _, c, _)| *c == Class::Resolver)
        .map(|(f, func, _, _)| (*f, *func))
        .collect();
    assert_eq!(
        resolvers,
        [
            ("ambient.rs", "runner_platform_config_root"),
            ("profiles.rs", "settings_json_path"),
            ("settings.rs", "resolve_config_dir"),
        ]
        .into_iter()
        .collect(),
        "only the guarded doors may be classed Resolver"
    );
}

/// Does an integration-test file's CODE (not its comments) link the lib, and
/// does it assert the canary is armed? `(links_lib, asserts_canary)`.
fn integration_file_facts(src: &str) -> syn::Result<(bool, bool)> {
    #[derive(Default)]
    struct V {
        links_lib: bool,
        asserts_canary: bool,
    }
    impl<'ast> Visit<'ast> for V {
        fn visit_path(&mut self, p: &'ast syn::Path) {
            if p.segments
                .first()
                .is_some_and(|s| s.ident == "qontinui_runner_lib")
            {
                self.links_lib = true;
            }
            if p.segments.last().is_some_and(|s| s.ident == "canary_armed") {
                self.asserts_canary = true;
            }
            visit::visit_path(self, p);
        }
        fn visit_use_path(&mut self, u: &'ast syn::UsePath) {
            if u.ident == "qontinui_runner_lib" {
                self.links_lib = true;
            }
            visit::visit_use_path(self, u);
        }
        fn visit_item_extern_crate(&mut self, e: &'ast syn::ItemExternCrate) {
            if e.ident == "qontinui_runner_lib" {
                self.links_lib = true;
            }
            visit::visit_item_extern_crate(self, e);
        }
        fn visit_macro(&mut self, m: &'ast syn::Macro) {
            let text = m.tokens.to_string();
            if text.contains("qontinui_runner_lib") {
                self.links_lib = true;
            }
            if text.contains("canary_armed") {
                self.asserts_canary = true;
            }
            visit::visit_macro(self, m);
        }
    }
    let file = syn::parse_file(src)?;
    let mut v = V::default();
    v.visit_file(&file);
    Ok((v.links_lib, v.asserts_canary))
}

/// Integration tests (`src-tauri/tests/*.rs`) are separate crates that link the
/// lib WITHOUT `cfg(test)`, so the only thing making the config-dir resolvers
/// hermetic inside them is `canary_armed()`'s `deps/` detection. Phase 4's
/// audit found that none of them links the lib in code today (they read
/// source, parse YAML, or spawn a binary). This keeps the audit true: a file
/// that starts calling into the lib must also pin, in the runner bin's
/// `test_env` live-canary style, that the canary armed itself there.
#[test]
fn an_integration_test_that_links_the_lib_asserts_the_canary_is_armed() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests");
    let files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("reading {}: {e}", dir.display()))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "rs"))
        .collect();
    assert!(
        files.len() >= 10,
        "found only {} integration test files under {} — the scan read nothing",
        files.len(),
        dir.display()
    );
    let mut offenders = Vec::new();
    for file in &files {
        let src = std::fs::read_to_string(file)
            .unwrap_or_else(|e| panic!("reading {}: {e}", file.display()));
        let (links, asserts) = integration_file_facts(&src)
            .unwrap_or_else(|e| panic!("{}: could not parse ({e})", file.display()));
        if links && !asserts {
            offenders.push(file.display().to_string());
        }
    }
    assert!(
        offenders.is_empty(),
        "integration test file(s) call into qontinui_runner_lib without asserting the \
         ambient canary armed itself there:\n  {}\n\
         Add `assert!(qontinui_runner_lib::ambient::test_support::canary_armed(), \"…\");` \
         (see main.rs `ambient_canary_arms_itself_in_this_test_binary`). Without it, a \
         `deps/` detection that stopped recognising the binary would let every settings \
         or config-dir writer it reaches hit the operator's real files.",
        offenders.join("\n  ")
    );

    // The detector itself, on synthetic sources.
    assert_eq!(
        integration_file_facts("//! qontinui_runner_lib in a comment\nfn f() {}").unwrap(),
        (false, false)
    );
    assert_eq!(
        integration_file_facts("use qontinui_runner_lib::profiles;\nfn f() {}").unwrap(),
        (true, false)
    );
    assert_eq!(
        integration_file_facts(
            "#[test] fn t() { assert!(qontinui_runner_lib::ambient::test_support::canary_armed()); }"
        )
        .unwrap(),
        (true, true)
    );
}
