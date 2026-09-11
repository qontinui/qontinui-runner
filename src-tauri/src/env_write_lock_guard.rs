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
//! A file only needs parsing if its text mentions `set_var` or `remove_var` —
//! no other file can contain a writer under the rule above — and a file that
//! does and fails to parse fails this test by name rather than being skipped.

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
        files.len() > 1000,
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
        if !WRITE_FNS.iter().any(|w| src.contains(w)) {
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
        env_writing_tests > 100,
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
    assert_eq!(
        names,
        [
            "unlocked_as_a_fn_value",
            "unlocked_bare_call",
            "unlocked_direct",
            "unlocked_in_a_closure",
            "unlocked_in_a_nested_module",
            "unlocked_inside_a_macro",
            "unlocked_via_a_same_file_writer",
            "unlocked_via_use_env",
        ],
        "the guard must flag exactly the unlocked writers — no more, no fewer"
    );
    // 8 unlocked + 6 locked writers; `only_reads` and the method call are not writers.
    assert_eq!(report.env_writing_tests, 14);

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
