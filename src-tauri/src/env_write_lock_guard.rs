//! Source invariant: every test that WRITES the process environment holds the
//! one process-wide env lock.
//!
//! `std::env` is process-global and `cargo test` runs a binary's tests on a
//! thread pool, so the suite serializes every env-touching test on ONE lock:
//! `ambient::test_support::env_lock()`, re-exported as `crate::test_env::env_lock`
//! by both crate roots. A test that writes env WITHOUT it is not excluded by
//! anything, so it can change a variable underneath a correctly-locked test
//! mid-read — and the locked test is the one that goes red, at random, with a
//! message that names nothing about the writer. (Unlocked READERS can only hurt
//! themselves; a writer is what breaks a sibling.)
//!
//! This class has been fixed three times and regressed three times, because
//! nothing enforced it: #808 put 22 modules on the lock, #915 fixed one more,
//! and plan `2026-08-25-runner-test-suite-env-isolation` found eight more —
//! one of them behind a module-local mutex that excluded nothing holding the
//! shared one. This test is the enforcement.
//!
//! # What counts
//!
//! A **test fn** is any fn carrying an attribute whose path ends in `test`
//! (`#[test]`, `#[tokio::test]`, `#[tokio::test(flavor = …)]`), at any module
//! depth. It **writes env** when its body — closures, nested blocks and macro
//! arguments included — calls `std::env::set_var` / `std::env::remove_var`
//! (any path ending `env::set_var` / `env::remove_var`, or a bare `set_var` /
//! `remove_var` call), or calls a fn defined in the same file that does. It
//! **holds the lock** when its body calls `env_lock()`, names
//! `IsolatedAmbient` (whose constructor takes that same lock for the fixture's
//! life), or calls a fn defined in the same file that does — so a
//! `with_clean_token_env`-style helper that takes the lock covers every test
//! that runs through it. Both properties are closed transitively over
//! same-file calls.
//!
//! Parsed with `syn`, not a brace counter: the vet scan behind this plan
//! counted braces, and a string literal holding a `{` merged two adjacent fns.
//! A file needs parsing if its text mentions `set_var`, `remove_var`, or any
//! [`CROSS_FILE_WRITERS`] name — a file mentioning none of those cannot hold a
//! writer under the rule above — and a file that does and fails to parse fails
//! this test by name rather than being skipped.
//!
//! # Known limits
//!
//! Stated because a guard whose reach is undocumented gets trusted past it.
//! Each bullet has a named KNOWN-MISS fixture in the synthetic self-test, and a
//! loop there asserts every one is still missed — so closing a limit fails by
//! name and points back here, instead of passing silently.
//!
//! * **Release-then-write is missed** (`unlocked_after_a_helper_released_the_lock`).
//!   `takes_lock` carries no ordering and no liveness, so a helper that acquires
//!   the lock and releases it on return still credits its caller;
//!   `let _ = env_lock();`, which drops at once, is the same class. There is no
//!   live instance in this tree today. `with_body_sync_env`
//!   (`plan_workunit_adapter/trigger.rs`) looks like one and is NOT: it holds
//!   its guard across the `f()` call, so it is the shape the rule correctly
//!   credits.
//! * **Cross-file reach is by NAME, not by analysis**
//!   (`unlocked_via_an_unlisted_cross_file_helper`). Only [`LOCK_FN`],
//!   [`LOCK_FIXTURE`] and the [`CROSS_FILE_WRITERS`] names cross a file
//!   boundary — `env_lock` is how every locked test in the tree is credited,
//!   since the single definition lives in `ambient::test_support`. A writer or
//!   locker reached through any OTHER file's helper is invisible; add the name
//!   here when you add such a helper.
//! * **Reach through a METHOD is not closed**
//!   (`unlocked_via_a_same_file_method`). [`FnCollector`] keys an impl fn as
//!   `Type::name` while a method call site names only the method, so a
//!   same-file `impl` method that writes env is unseen at its call sites.
//!   Keying impl fns by bare name as well would credit any `X::new()` caller
//!   with an unrelated `Y::new()`'s lock — a false-NEGATIVE machine — so this
//!   is documented rather than closed.
//! * **Aliased mutators are missed** (`unlocked_via_an_aliased_mutator`).
//!   `use std::env as e; e::set_var(..)` and `let f = std::env::set_var; f(..)`:
//!   the qualifier must read `env`, or the call must be a bare `set_var` /
//!   `remove_var`.
//! * **Drop-only writers** (`unlocked_via_a_local_drop_guard`) are seen only
//!   where the type's name is in [`CROSS_FILE_WRITERS`] (`EnvVarRestore`). A
//!   file-local guard type whose `Drop` writes env — as the deleted
//!   `DbUrlRestore` did — is not.
//!
//! This module is registered in `main.rs` only: it walks all of `src`, so a
//! `lib.rs` twin would only double the work. CI runs it in the ordinary
//! `cargo test` job. Note the self-test's fixture is a raw STRING, which is why
//! the guard does not flag its own deliberately-unlocked writers.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use proc_macro2::{Delimiter, TokenStream, TokenTree};
use syn::visit::{self, Visit};

/// The two process-env mutators.
const WRITE_FNS: [&str; 2] = ["set_var", "remove_var"];
/// The shared lock's accessor, however it is imported.
const LOCK_FN: &str = "env_lock";
/// The fixture that holds [`LOCK_FN`]'s lock for its whole life.
const LOCK_FIXTURE: &str = "IsolatedAmbient";

/// Helpers that write the process env from ANOTHER FILE, named here because
/// the same-file call closure cannot reach them.
///
/// `crate::test_env::isolate_coord_env` writes all seven
/// `profiles::COORD_BASE_ENV_KEYS` and clears the runtime tier override;
/// `capture_coord_env` and `EnvVarRestore` write on Drop. Without these, a
/// test whose only mutation is a call to one of them is invisible to this
/// guard — and its file need not contain `set_var` at all, which is why they
/// also widen the parse prefilter below. `ci_node/subscription.rs` is exactly
/// that shape: zero occurrences of either mutator, seven env writes per test
/// through the helper, and the fixture family the 2026-08-25 flake came from.
const CROSS_FILE_WRITERS: [&str; 3] = ["isolate_coord_env", "capture_coord_env", "EnvVarRestore"];

/// Floor for the walk, so a broken path or filter cannot pass vacuously.
/// Sibling ratchet `row_get_ratchet.rs` declares its own floor the same way;
/// 1559 files were walked when this was written.
const MIN_FILES_WALKED: usize = 1000;
/// Floor for the detected population, for the same reason: an empty offender
/// list proves nothing if the detector stopped recognising writers. 169
/// env-writing test fns were found when this was written (measured by running
/// this detector standalone over the tree, not estimated).
const MIN_ENV_WRITING_TESTS: usize = 100;

/// A test fn that writes the process env without holding the shared lock.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Offender {
    line: usize,
    name: String,
}

/// The verdict for one source file.
#[derive(Debug, Default)]
struct FileReport {
    /// Test fns that write env, locked or not. Counted so the real-tree test
    /// can prove it saw the population it polices.
    env_writing_tests: usize,
    offenders: Vec<Offender>,
}

/// What one fn body does DIRECTLY — before same-file calls are resolved.
#[derive(Debug, Default)]
struct BodyFacts {
    writes_env: bool,
    takes_lock: bool,
    /// Every path call in the body, as `name` and, when qualified,
    /// `Qualifier::name` (with `Self` resolved to the enclosing impl's type).
    calls: BTreeSet<String>,
}

impl BodyFacts {
    fn record_call(&mut self, qualifier: Option<&str>, name: &str, self_ty: Option<&str>) {
        if WRITE_FNS.contains(&name) && matches!(qualifier, None | Some("env")) {
            self.writes_env = true;
        }
        // A named cross-file helper, as the callee (`isolate_coord_env(..)`) or
        // as the qualifier (`EnvVarRestore::capture(..)`). See
        // [`CROSS_FILE_WRITERS`]: the same-file closure cannot see these, and
        // the file need not mention either mutator.
        if CROSS_FILE_WRITERS.contains(&name)
            || qualifier.is_some_and(|q| CROSS_FILE_WRITERS.contains(&q))
        {
            self.writes_env = true;
        }
        if name == LOCK_FN {
            self.takes_lock = true;
        }
        self.calls.insert(name.to_string());
        if let Some(q) = qualifier {
            let q = if q == "Self" { self_ty.unwrap_or(q) } else { q };
            self.calls.insert(format!("{q}::{name}"));
        }
    }

    fn record_path_call(&mut self, path: &syn::Path, self_ty: Option<&str>) {
        let segs: Vec<String> = path.segments.iter().map(|s| s.ident.to_string()).collect();
        let Some(name) = segs.last() else {
            return;
        };
        let qualifier = segs.len().checked_sub(2).map(|i| segs[i].as_str());
        self.record_call(qualifier, name, self_ty);
    }
}

/// Is `path` a process-env mutator named as a value, e.g. the argument of
/// `keys.iter().for_each(std::env::remove_var)`?
fn is_env_write_path(path: &syn::Path) -> bool {
    let n = path.segments.len();
    n >= 2
        && path.segments[n - 2].ident == "env"
        && WRITE_FNS.iter().any(|w| path.segments[n - 1].ident == w)
}

/// `syn` does not parse macro arguments (`assert!(…)`, `tokio::select! {…}`),
/// so scan their tokens for the same three signals: an `ident(` call (with an
/// optional `Qualifier::` before it, and not a `.method(` call), and any
/// mention of [`LOCK_FIXTURE`].
fn scan_tokens(tokens: TokenStream, facts: &mut BodyFacts, self_ty: Option<&str>) {
    let trees: Vec<TokenTree> = tokens.into_iter().collect();
    for (i, tree) in trees.iter().enumerate() {
        match tree {
            TokenTree::Group(g) => scan_tokens(g.stream(), facts, self_ty),
            TokenTree::Ident(id) => {
                let name = id.to_string();
                if name == LOCK_FIXTURE {
                    facts.takes_lock = true;
                }
                let is_call = matches!(
                    trees.get(i + 1),
                    Some(TokenTree::Group(g)) if g.delimiter() == Delimiter::Parenthesis
                );
                let is_method =
                    i >= 1 && matches!(&trees[i - 1], TokenTree::Punct(p) if p.as_char() == '.');
                if !is_call || is_method {
                    continue;
                }
                let qualifier = match i
                    .checked_sub(3)
                    .map(|q| (&trees[q], &trees[q + 1], &trees[q + 2]))
                {
                    Some((TokenTree::Ident(q), TokenTree::Punct(a), TokenTree::Punct(b)))
                        if a.as_char() == ':' && b.as_char() == ':' =>
                    {
                        Some(q.to_string())
                    }
                    _ => None,
                };
                facts.record_call(qualifier.as_deref(), &name, self_ty);
            }
            TokenTree::Punct(_) | TokenTree::Literal(_) => {}
        }
    }
}

/// Collects [`BodyFacts`] for one fn body.
struct BodyScanner {
    facts: BodyFacts,
    self_ty: Option<String>,
}

impl<'ast> Visit<'ast> for BodyScanner {
    fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
        if let syn::Expr::Path(p) = &*call.func {
            self.facts
                .record_path_call(&p.path, self.self_ty.as_deref());
        }
        visit::visit_expr_call(self, call);
    }

    fn visit_expr_path(&mut self, p: &'ast syn::ExprPath) {
        if is_env_write_path(&p.path) {
            self.facts.writes_env = true;
        }
        visit::visit_expr_path(self, p);
    }

    fn visit_path(&mut self, p: &'ast syn::Path) {
        if p.segments.iter().any(|s| s.ident == LOCK_FIXTURE) {
            self.facts.takes_lock = true;
        }
        visit::visit_path(self, p);
    }

    fn visit_macro(&mut self, m: &'ast syn::Macro) {
        scan_tokens(m.tokens.clone(), &mut self.facts, self.self_ty.as_deref());
        visit::visit_macro(self, m);
    }
}

/// One fn anywhere in the file, keyed the way a call site names it.
struct FnRecord {
    /// `name` for a free fn, `Type::name` for an inherent/trait impl method.
    key: String,
    name: String,
    line: usize,
    is_test: bool,
    facts: BodyFacts,
}

fn is_test_attr(attr: &syn::Attribute) -> bool {
    attr.path()
        .segments
        .last()
        .is_some_and(|s| s.ident == "test")
}

/// Walks a file and records every fn with a body.
#[derive(Default)]
struct FnCollector {
    /// The enclosing impl's self type, innermost last. `None` inside a fn
    /// body, where a nested `fn` is a free fn again.
    scope: Vec<Option<String>>,
    fns: Vec<FnRecord>,
}

impl FnCollector {
    fn record(&mut self, ident: &syn::Ident, attrs: &[syn::Attribute], block: &syn::Block) {
        let self_ty = self.scope.last().cloned().flatten();
        let mut scanner = BodyScanner {
            facts: BodyFacts::default(),
            self_ty: self_ty.clone(),
        };
        scanner.visit_block(block);
        let name = ident.to_string();
        self.fns.push(FnRecord {
            key: match &self_ty {
                Some(t) => format!("{t}::{name}"),
                None => name.clone(),
            },
            line: ident.span().start().line,
            is_test: attrs.iter().any(is_test_attr),
            name,
            facts: scanner.facts,
        });
    }
}

impl<'ast> Visit<'ast> for FnCollector {
    fn visit_item_fn(&mut self, f: &'ast syn::ItemFn) {
        self.record(&f.sig.ident, &f.attrs, &f.block);
        self.scope.push(None);
        visit::visit_item_fn(self, f);
        self.scope.pop();
    }

    fn visit_item_impl(&mut self, i: &'ast syn::ItemImpl) {
        let ty = match &*i.self_ty {
            syn::Type::Path(tp) => tp.path.segments.last().map(|s| s.ident.to_string()),
            _ => None,
        };
        self.scope.push(ty);
        visit::visit_item_impl(self, i);
        self.scope.pop();
    }

    fn visit_impl_item_fn(&mut self, f: &'ast syn::ImplItemFn) {
        self.record(&f.sig.ident, &f.attrs, &f.block);
        self.scope.push(None);
        visit::visit_impl_item_fn(self, f);
        self.scope.pop();
    }

    fn visit_trait_item_fn(&mut self, f: &'ast syn::TraitItemFn) {
        if let Some(block) = &f.default {
            self.record(&f.sig.ident, &f.attrs, block);
        }
        self.scope.push(None);
        visit::visit_trait_item_fn(self, f);
        self.scope.pop();
    }
}

/// Does any call in `facts` reach a fn (other than `me`) whose flag is set?
fn reaches(
    facts: &BodyFacts,
    me: usize,
    by_key: &HashMap<&str, Vec<usize>>,
    flags: &[bool],
) -> bool {
    facts.calls.iter().any(|k| {
        by_key
            .get(k.as_str())
            .is_some_and(|ix| ix.iter().any(|&j| j != me && flags[j]))
    })
}

/// Check one file's source. `Err` only when it does not parse.
fn scan_source(src: &str) -> syn::Result<FileReport> {
    let file = syn::parse_file(src)?;
    let mut collector = FnCollector::default();
    collector.visit_file(&file);
    let fns = collector.fns;

    let mut by_key: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, f) in fns.iter().enumerate() {
        by_key.entry(f.key.as_str()).or_default().push(i);
    }
    let mut locks: Vec<bool> = fns.iter().map(|f| f.facts.takes_lock).collect();
    let mut writes: Vec<bool> = fns.iter().map(|f| f.facts.writes_env).collect();
    // Close both properties over same-file calls. Monotone, so this settles.
    loop {
        let mut changed = false;
        for (i, f) in fns.iter().enumerate() {
            if !locks[i] && reaches(&f.facts, i, &by_key, &locks) {
                locks[i] = true;
                changed = true;
            }
            if !writes[i] && reaches(&f.facts, i, &by_key, &writes) {
                writes[i] = true;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    let mut report = FileReport::default();
    for (i, f) in fns.iter().enumerate() {
        if !(f.is_test && writes[i]) {
            continue;
        }
        report.env_writing_tests += 1;
        if !locks[i] {
            report.offenders.push(Offender {
                line: f.line,
                name: f.name.clone(),
            });
        }
    }
    report.offenders.sort();
    Ok(report)
}

/// Every `.rs` file under `root`, in a stable order.
fn rs_files(root: &Path) -> Vec<PathBuf> {
    walkdir::WalkDir::new(root)
        .sort_by_file_name()
        .into_iter()
        .map(|e| e.unwrap_or_else(|err| panic!("walking {}: {err}", root.display())))
        .filter(|e| e.file_type().is_file() && e.path().extension().is_some_and(|x| x == "rs"))
        .map(walkdir::DirEntry::into_path)
        .collect()
}

#[test]
fn every_env_writing_test_holds_the_shared_env_lock() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let files = rs_files(&root);
    assert!(
        files.len() > MIN_FILES_WALKED,
        "walked only {} .rs files under {} — the guard scanned nothing",
        files.len(),
        root.display()
    );

    let mut env_writing_tests = 0usize;
    let mut violations: Vec<String> = Vec::new();
    for file in &files {
        let rel = file
            .strip_prefix(&root)
            .unwrap_or(file)
            .display()
            .to_string();
        let src =
            std::fs::read_to_string(file).unwrap_or_else(|e| panic!("reading src/{rel}: {e}"));
        // Parse a file if it mentions either mutator OR any named cross-file
        // writer. `ci_node/subscription.rs` contains neither `set_var` nor
        // `remove_var`, and still writes seven env vars per test through
        // `crate::test_env::isolate_coord_env` — prefiltering on the mutators
        // alone never parsed it. See [`CROSS_FILE_WRITERS`].
        let mentions_writer = WRITE_FNS.iter().any(|w| src.contains(w))
            || CROSS_FILE_WRITERS.iter().any(|w| src.contains(w));
        if !mentions_writer {
            continue;
        }
        let report = scan_source(&src).unwrap_or_else(|e| {
            panic!(
                "src/{rel}:{}: could not parse with syn ({e}) — the env-write guard cannot \
                 vouch for a file it cannot read, so it refuses rather than skipping it",
                e.span().start().line
            )
        });
        env_writing_tests += report.env_writing_tests;
        violations.extend(
            report
                .offenders
                .into_iter()
                .map(|o| format!("src/{rel}:{} {}", o.line, o.name)),
        );
    }

    // Non-vacuity: the detector must actually be finding the (locked)
    // env-writing tests this binary is known to have. If it drops to a
    // handful, the detector broke — not the tree.
    assert!(
        env_writing_tests > MIN_ENV_WRITING_TESTS,
        "found only {env_writing_tests} env-writing test fns — the detector has stopped \
         recognising them, so an empty offender list below would prove nothing"
    );
    assert!(
        violations.is_empty(),
        "{} test fn(s) write the process environment without holding the shared env lock:\n  \
         {}\n\n\
         `std::env` is process-global and tests run in parallel, so there is ONE lock for every \
         env-touching test — `crate::test_env::env_lock()` (the single definition is \
         `ambient::test_support::env_lock`, re-exported by both crate roots). An unlocked writer \
         is excluded by nothing: it changes a variable underneath a correctly-locked test \
         mid-read, and the LOCKED test is the one that fails, at random. A module-local mutex \
         does not count — it excludes nothing holding the shared one.\n\
         Fix: hold `let _g = crate::test_env::env_lock();` for the whole body, declared BEFORE \
         any `EnvVarRestore::capture(..)` so the restore runs while the lock is still held — or \
         construct an `IsolatedAmbient`, or route through a same-file helper that takes the \
         lock. Plan `2026-08-25-runner-test-suite-env-isolation`.",
        violations.len(),
        violations.join("\n  ")
    );
}

/// The guard must be SEEN to fail: run the same checker over synthetic source
/// covering every shape it claims to handle.
#[test]
fn the_guard_flags_unlocked_writers_and_passes_locked_ones() {
    const SRC: &str = r#"
fn lock_taking_helper(body: impl FnOnce()) {
    let _g = crate::test_env::env_lock();
    body();
}
fn transitively_locks() {
    lock_taking_helper(|| {});
}
fn writes_without_lock() {
    std::env::set_var("K", "v");
}
fn not_a_test_writes_freely() {
    std::env::remove_var("K");
}
struct Fixture;
impl Fixture {
    fn new() -> Self {
        let _g = crate::test_env::env_lock();
        Fixture
    }
    fn fresh() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unlocked_direct() {
        std::env::set_var("K", "v");
    }
    #[tokio::test]
    async fn unlocked_in_a_closure() {
        let clear = || std::env::remove_var("K");
        clear();
    }
    #[tokio::test(flavor = "multi_thread")]
    async fn unlocked_via_use_env() {
        use std::env;
        env::set_var("K", "v");
    }
    #[test]
    fn unlocked_bare_call() {
        use std::env::remove_var;
        remove_var("K");
    }
    #[test]
    fn unlocked_inside_a_macro() {
        assert!({
            std::env::set_var("K", "v");
            true
        });
    }
    #[test]
    fn unlocked_as_a_fn_value() {
        ["K"].into_iter().for_each(std::env::remove_var);
    }
    #[test]
    fn unlocked_via_a_same_file_writer() {
        super::writes_without_lock();
    }

    // ---- KNOWN MISSES ----------------------------------------------------
    // Each pins one bullet of the module doc's `# Known limits`, so closing a
    // limit fails the loop in the self-test rather than passing silently. All
    // four write the process env for real and none is flagged.
    #[test]
    fn unlocked_via_an_aliased_mutator() {
        use std::env as e;
        e::set_var("K", "v");
    }
    #[test]
    fn unlocked_via_an_unlisted_cross_file_helper() {
        crate::some_other_file::a_helper_that_writes_env();
    }
    struct LocalDropGuard;
    impl Drop for LocalDropGuard {
        fn drop(&mut self) {
            std::env::remove_var("K");
        }
    }
    #[test]
    fn unlocked_via_a_local_drop_guard() {
        let _g = LocalDropGuard;
    }
    struct MethodWriter;
    impl MethodWriter {
        fn writes_env_from_a_method(&self) {
            std::env::set_var("K", "v");
        }
    }
    #[test]
    fn unlocked_via_a_same_file_method() {
        MethodWriter.writes_env_from_a_method();
    }
    mod nested {
        #[test]
        fn unlocked_in_a_nested_module() {
            unsafe { std::env::set_var("K", "v") };
        }
    }

    #[test]
    fn locked_directly() {
        let _g = crate::test_env::env_lock();
        let _restore = crate::test_env::EnvVarRestore::capture(&["K"]);
        std::env::set_var("K", "v");
    }
    #[test]
    fn locked_by_the_ambient_fixture() {
        let _a = IsolatedAmbient::new();
        std::env::set_var("K", "v");
    }
    #[test]
    fn locked_through_a_helper() {
        lock_taking_helper(|| std::env::set_var("K", "v"));
    }
    #[test]
    fn locked_through_a_helper_of_a_helper() {
        lock_taking_helper(|| lock_taking_helper(|| std::env::set_var("K", "v")));
    }
    #[test]
    fn locked_via_a_cross_file_helper() {
        let _g = crate::test_env::env_lock();
        crate::test_env::isolate_coord_env(dir.path(), "{}");
    }
    #[test]
    fn unlocked_via_a_cross_file_helper() {
        crate::test_env::isolate_coord_env(dir.path(), "{}");
    }
    #[test]
    fn unlocked_via_env_var_restore() {
        let _restore = crate::test_env::EnvVarRestore::capture(&["K"]);
    }
    /// KNOWN MISS, asserted as one below rather than left to be discovered.
    ///
    /// `takes_lock` is "this body reaches a locking fn", with no ordering and no
    /// liveness, so a helper that acquires the lock and RELEASES it on return
    /// still credits its caller — and the write below is genuinely unprotected.
    /// Deciding otherwise needs the write to be lexically inside the helper's
    /// closure, which this coarse rule does not model. The live shape is
    /// `plan_workunit_adapter/trigger.rs`'s `with_body_sync_env`, and
    /// `let _ = env_lock();` (dropped at once) is the same class.
    #[test]
    fn unlocked_after_a_helper_released_the_lock() {
        super::transitively_locks();
        std::env::set_var("K", "v");
    }
    #[test]
    fn locked_by_a_fixture_constructor() {
        let _f = Fixture::fresh();
        std::env::set_var("K", "v");
    }
    #[test]
    fn locked_inside_a_macro() {
        let _g = env_lock();
        assert!({
            std::env::remove_var("K");
            true
        });
    }
    #[test]
    fn only_reads() {
        let _ = std::env::var("K");
    }
    #[test]
    fn a_method_named_set_var_is_not_the_process_env() {
        let mut cmd = Builder::default();
        cmd.set_var("K", "v");
        let _ = "std::env::set_var(\"K\", \"v\")";
    }
}
"#;
    let report = scan_source(SRC).expect("synthetic source parses");
    let mut names: Vec<&str> = report.offenders.iter().map(|o| o.name.as_str()).collect();
    names.sort_unstable();
    // EVERY known limit in the module doc, pinned as a miss. This runs BEFORE
    // the exact-list assertion below on purpose: that one would also fail if a
    // limit closed, but it would fail saying "no more, no fewer" and name
    // neither the limit nor this doc — so the diagnostic would be dead text.
    // Closing any of these is welcome; update the doc's Known limits and delete
    // the line here when you do.
    for (miss, limit) in [
        (
            "unlocked_after_a_helper_released_the_lock",
            "release-then-write: `takes_lock` carries no ordering or liveness",
        ),
        (
            "unlocked_via_an_aliased_mutator",
            "aliased mutators: the qualifier must read `env`",
        ),
        (
            "unlocked_via_an_unlisted_cross_file_helper",
            "cross-file reach is by NAME: only CROSS_FILE_WRITERS/LOCK_FIXTURE/LOCK_FN cross a file",
        ),
        (
            "unlocked_via_a_local_drop_guard",
            "a file-local guard type whose Drop writes env is not a writer at its call sites",
        ),
        (
            "unlocked_via_a_same_file_method",
            "reach through a METHOD is not closed: impl fns are keyed `Type::name`",
        ),
    ] {
        assert!(
            !names.contains(&miss),
            "`{miss}` is now FLAGGED, so the guard has closed a documented limit \
             ({limit}). Tighten this test and update the module doc's Known limits."
        );
    }
    assert_eq!(
        names,
        [
            "unlocked_as_a_fn_value",
            "unlocked_bare_call",
            "unlocked_direct",
            "unlocked_in_a_closure",
            "unlocked_in_a_nested_module",
            "unlocked_inside_a_macro",
            "unlocked_via_a_cross_file_helper",
            "unlocked_via_a_same_file_writer",
            "unlocked_via_env_var_restore",
            "unlocked_via_use_env",
        ],
        "the guard must flag exactly the unlocked writers — no more, no fewer"
    );
    // 10 unlocked + 1 known miss + 7 locked writers. `only_reads` and
    // `a_method_named_set_var_is_not_the_process_env` are not writers: a method
    // call is an edge, never a `std::env` write.
    assert_eq!(report.env_writing_tests, 18);

    // The line it reports is the fn's own line (proc-macro2 `span-locations`),
    // so a failure message is clickable rather than approximate.
    let expected_line = SRC
        .lines()
        .position(|l| l.contains("fn unlocked_direct"))
        .expect("fixture line")
        + 1;
    let direct = report
        .offenders
        .iter()
        .find(|o| o.name == "unlocked_direct")
        .expect("unlocked_direct is flagged");
    assert_eq!(direct.line, expected_line);
}

/// A file that does not parse is an error the real-tree test turns into a
/// named failure — never a silent skip.
#[test]
fn the_guard_refuses_source_it_cannot_parse() {
    assert!(scan_source("#[test] fn broken( { std::env::set_var(\"K\", \"v\"); }").is_err());
}
