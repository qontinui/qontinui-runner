//! Source invariant: every test-lock constructor is a CHILD of `env_lock`.
//!
//! `cargo test` runs a binary's tests on a thread pool, so the suite serializes
//! shared process-global state with test-only mutexes. `env_lock()`
//! (`ambient::test_support`) is the ROOT of that set: it is reentrant per
//! thread. A test lock that takes `env_lock()` BEFORE its own mutex and holds
//! it for the guard's life is a child. A thread holding a child already holds
//! `env_lock`, so any later `env_lock()` / `isolated_ambient()` on that thread
//! nests. Any thread WAITING on a child also holds `env_lock`, so the child's
//! holder cannot be on another thread. Children therefore never deadlock
//! against the root or against each other, whatever order a test takes them in.
//!
//! A test lock that does NOT take `env_lock` first is a sibling. Two siblings
//! taken in opposite orders on two threads deadlock, and cargo has no per-test
//! timeout, so the run HANGS instead of failing. That happened: the
//! plan-capture pin (`fleet_policy_poller`) was taken pin → env by 25
//! `plan_library` tests and env → pin by four `terminal` / `agent_runtime`
//! tests, each module's comment stating the opposite "crate-wide order", and
//! 4 of 6 `plan_library terminal::` runs hung (plan
//! `2026-10-02-plan-capture-test-pin-and-env-lock-are-taken-in-opposite-orders-so-one-cargo-test-run-can-deadlock`).
//! A per-site order is a convention nothing enforces; this test is the
//! enforcement.
//!
//! # What counts
//!
//! * A **test static** is a `static` of type `Mutex<()>` (or
//!   `OnceLock<Mutex<()>>`, or an alias whose name ends in `Mutex`) declared in
//!   test scope: inside a `#[cfg(test)]`
//!   item (also `cfg(any(test, …))`), a `mod tests` / `mod test_support`, or a
//!   `#[test]` fn — at file level OR inside a fn body. `Mutex<()>` only: a
//!   mutex carrying data is a value, not a serializer.
//! * A **test-lock constructor** is a test-scope fn that acquires a test static
//!   (`X.lock()`, `X.get_or_init(..).lock()`, or `hierarchy_lock(&X)`) and
//!   returns the guard: its return type names `MutexGuard`, `EnvLockGuard`,
//!   `TestLockGuard`, a same-file struct with such a field, or `Self` in such a
//!   struct's impl.
//! * It is **compliant** when every acquisition goes through `hierarchy_lock`
//!   (which takes `env_lock` first by construction), or is preceded in the body
//!   by a call to `env_lock()`, `isolated_ambient()` or `IsolatedAmbient::…`
//!   that is not discarded into `let _ = …` (which releases it at once).
//! * Otherwise it must carry `// test-lock: standalone — <reason>` in the
//!   comment/attribute lines directly above its `fn` or inside its body. A
//!   standalone lock is then AUDITED: no fn in its file may call it and also
//!   reach the hierarchy (a root, a compliant constructor anywhere in the tree,
//!   or a same-file fn that reaches one). A standalone lock that is co-held is
//!   a sibling again, so the marker does not excuse it.
//! * **Inline co-holders**: a test-scope fn that is not a constructor, locks a
//!   test static directly and ALSO directly calls a hierarchy lock must take
//!   the hierarchy lock first.
//!
//! # Known limits
//!
//! * Acquisitions inside macro arguments are not seen (`syn` does not parse
//!   them); no test lock is taken that way today.
//! * The held-for-life check is "not `let _ =`". An env guard bound to a name
//!   and then `drop`ped before the child is released is not detected.
//! * The standalone audit is same-file by name. A standalone constructor is
//!   module-private by convention; if one is made `pub` and called from
//!   another file, convert it instead.
//! * Two STANDALONE locks co-held with each other (neither touching the
//!   hierarchy) are not checked against each other.
//!
//! Registered in `main.rs` only, like `env_write_lock_guard`: it walks all of
//! `src`, so a `lib.rs` twin would only double the work.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use proc_macro2::{TokenStream, TokenTree};
use syn::visit::{self, Visit};

/// Calls that take (or nest on) the env lock — the hierarchy root.
const HIERARCHY_ROOTS: [&str; 4] = [
    "env_lock",
    "isolated_ambient",
    "IsolatedAmbient",
    "hierarchy_lock",
];
/// The root's accessor and static, exempt from the constructor rule.
const ROOT_FN: &str = "env_lock";
const ROOT_STATIC: &str = "ENV_LOCK";
/// Return-type names that carry a held lock guard.
const GUARD_TYPES: [&str; 3] = ["MutexGuard", "EnvLockGuard", "TestLockGuard"];
/// The opt-out marker. Must be followed by `—` (or `-`) and a reason.
const STANDALONE_MARKER: &str = "test-lock: standalone";
/// Floor for the walk, so a broken path or filter cannot pass vacuously.
const MIN_FILES_WALKED: usize = 1000;
/// Constructors the scan MUST see, so an empty or broken detector cannot pass.
const REQUIRED_CONSTRUCTORS: [&str; 2] = ["pin_plan_capture_level_for_test", "posture_test_lock"];
/// Floor for the detected constructor population. 14 were found when this was
/// written: 5 children (the pin, `posture_test_lock`, `perf_test_lock`,
/// `restore_forensics_lock`, `MarkerOverride::set`) and 9 standalone.
const MIN_CONSTRUCTORS: usize = 10;

type Pos = (usize, usize);

fn pos_of(span: proc_macro2::Span) -> Pos {
    let lc = span.start();
    (lc.line, lc.column)
}

fn collect_idents(ts: TokenStream, out: &mut Vec<String>) {
    for tt in ts {
        match tt {
            TokenTree::Group(g) => collect_idents(g.stream(), out),
            TokenTree::Ident(i) => out.push(i.to_string()),
            TokenTree::Punct(_) | TokenTree::Literal(_) => {}
        }
    }
}

/// `#[cfg(test)]`, `#[cfg(any(test, debug_assertions))]` — not `cfg(not(test))`.
fn is_cfg_test(attr: &syn::Attribute) -> bool {
    if !attr.path().is_ident("cfg") {
        return false;
    }
    let syn::Meta::List(list) = &attr.meta else {
        return false;
    };
    let mut idents = Vec::new();
    collect_idents(list.tokens.clone(), &mut idents);
    idents.iter().any(|i| i == "test") && !idents.iter().any(|i| i == "not")
}

fn is_test_attr(attr: &syn::Attribute) -> bool {
    attr.path()
        .segments
        .last()
        .is_some_and(|s| s.ident == "test")
}

fn attrs_are_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| is_cfg_test(a) || is_test_attr(a))
}

/// Every path-segment name in a type, and whether it contains `Mutex<()>`.
#[derive(Default)]
struct TypeFacts {
    idents: BTreeSet<String>,
    unit_mutex: bool,
}

impl<'ast> Visit<'ast> for TypeFacts {
    fn visit_path_segment(&mut self, s: &'ast syn::PathSegment) {
        // `ends_with`, not `==`: a `use std::sync::Mutex as StdMutex` alias
        // (`commands/transcript.rs`) is the same serializer.
        if s.ident.to_string().ends_with("Mutex") {
            if let syn::PathArguments::AngleBracketed(a) = &s.arguments {
                if a.args.iter().any(|g| {
                    matches!(g, syn::GenericArgument::Type(syn::Type::Tuple(t)) if t.elems.is_empty())
                }) {
                    self.unit_mutex = true;
                }
            }
        }
        self.idents.insert(s.ident.to_string());
        visit::visit_path_segment(self, s);
    }
}

fn type_facts(ty: &syn::Type) -> TypeFacts {
    let mut f = TypeFacts::default();
    f.visit_type(ty);
    f
}

/// Pass 1: test statics and guard-carrying structs.
#[derive(Default)]
struct DeclCollector {
    test_scope: Vec<bool>,
    test_statics: HashSet<String>,
    guard_structs: HashSet<String>,
}

impl DeclCollector {
    fn in_test(&self) -> bool {
        self.test_scope.iter().any(|t| *t)
    }
}

impl<'ast> Visit<'ast> for DeclCollector {
    fn visit_item_mod(&mut self, m: &'ast syn::ItemMod) {
        let t = attrs_are_test(&m.attrs) || m.ident == "tests" || m.ident == "test_support";
        self.test_scope.push(t);
        visit::visit_item_mod(self, m);
        self.test_scope.pop();
    }
    fn visit_item_fn(&mut self, f: &'ast syn::ItemFn) {
        self.test_scope.push(attrs_are_test(&f.attrs));
        visit::visit_item_fn(self, f);
        self.test_scope.pop();
    }
    fn visit_item_impl(&mut self, i: &'ast syn::ItemImpl) {
        self.test_scope.push(attrs_are_test(&i.attrs));
        visit::visit_item_impl(self, i);
        self.test_scope.pop();
    }
    fn visit_impl_item_fn(&mut self, f: &'ast syn::ImplItemFn) {
        self.test_scope.push(attrs_are_test(&f.attrs));
        visit::visit_impl_item_fn(self, f);
        self.test_scope.pop();
    }
    fn visit_item_static(&mut self, s: &'ast syn::ItemStatic) {
        if (self.in_test() || attrs_are_test(&s.attrs)) && type_facts(&s.ty).unit_mutex {
            self.test_statics.insert(s.ident.to_string());
        }
        visit::visit_item_static(self, s);
    }
    fn visit_item_struct(&mut self, s: &'ast syn::ItemStruct) {
        if s.fields.iter().any(|f| {
            type_facts(&f.ty)
                .idents
                .iter()
                .any(|i| GUARD_TYPES.contains(&i.as_str()))
        }) {
            self.guard_structs.insert(s.ident.to_string());
        }
        visit::visit_item_struct(self, s);
    }
}

/// One call in a fn body, by its last path segment.
#[derive(Debug, Clone)]
struct Call {
    name: String,
    pos: Pos,
    /// `let _ = name(..)`: the guard is dropped at once.
    discarded: bool,
}

/// One acquisition of a test static in a fn body.
#[derive(Debug, Clone)]
struct StaticLock {
    name: String,
    pos: Pos,
    /// Through `hierarchy_lock(&X)`, which takes `env_lock` first itself.
    via_hierarchy: bool,
}

#[derive(Default)]
struct BodyScan<'s> {
    statics: Option<&'s HashSet<String>>,
    calls: Vec<Call>,
    locks: Vec<StaticLock>,
    discarded: HashSet<Pos>,
}

fn call_name(path: &syn::Path) -> Option<String> {
    if path.segments.iter().any(|s| s.ident == "IsolatedAmbient") {
        return Some("IsolatedAmbient".to_string());
    }
    path.segments.last().map(|s| s.ident.to_string())
}

/// The static a `.lock()` receiver chain is rooted at, if it is a bare path.
fn receiver_root(mut e: &syn::Expr) -> Option<String> {
    loop {
        match e {
            syn::Expr::MethodCall(m) => e = &m.receiver,
            syn::Expr::Reference(r) => e = &r.expr,
            syn::Expr::Paren(p) => e = &p.expr,
            syn::Expr::Path(p) => return p.path.segments.last().map(|s| s.ident.to_string()),
            _ => return None,
        }
    }
}

impl<'ast> Visit<'ast> for BodyScan<'_> {
    fn visit_expr_call(&mut self, c: &'ast syn::ExprCall) {
        if let syn::Expr::Path(p) = &*c.func {
            if let Some(name) = call_name(&p.path) {
                let pos = pos_of(
                    p.path
                        .segments
                        .last()
                        .map_or_else(proc_macro2::Span::call_site, |s| s.ident.span()),
                );
                if name == "hierarchy_lock" {
                    if let Some(root) = c.args.first().and_then(receiver_root) {
                        if self.statics.is_some_and(|s| s.contains(&root)) {
                            self.locks.push(StaticLock {
                                name: root,
                                pos,
                                via_hierarchy: true,
                            });
                        }
                    }
                }
                self.calls.push(Call {
                    name,
                    pos,
                    discarded: false,
                });
            }
        }
        visit::visit_expr_call(self, c);
    }

    fn visit_expr_method_call(&mut self, m: &'ast syn::ExprMethodCall) {
        if m.method == "lock" && m.args.is_empty() {
            if let Some(root) = receiver_root(&m.receiver) {
                if self.statics.is_some_and(|s| s.contains(&root)) {
                    self.locks.push(StaticLock {
                        name: root,
                        pos: pos_of(m.method.span()),
                        via_hierarchy: false,
                    });
                }
            }
        }
        visit::visit_expr_method_call(self, m);
    }

    fn visit_local(&mut self, l: &'ast syn::Local) {
        if matches!(l.pat, syn::Pat::Wild(_)) {
            if let Some(init) = &l.init {
                if let syn::Expr::Call(c) = &*init.expr {
                    if let syn::Expr::Path(p) = &*c.func {
                        if let Some(s) = p.path.segments.last() {
                            self.discarded.insert(pos_of(s.ident.span()));
                        }
                    }
                }
            }
        }
        visit::visit_local(self, l);
    }

    // A nested fn is recorded on its own by `FnCollector`.
    fn visit_item_fn(&mut self, _f: &'ast syn::ItemFn) {}
}

/// Everything the evaluation needs about one fn.
#[derive(Debug, Clone)]
struct FnFacts {
    name: String,
    line: usize,
    in_test: bool,
    returns_guard: bool,
    marker: bool,
    calls: Vec<Call>,
    locks: Vec<StaticLock>,
}

impl FnFacts {
    fn is_constructor(&self) -> bool {
        // The root itself (`env_lock` over `ENV_LOCK`) is exempt: it is what
        // every child takes first, not a child.
        self.in_test
            && self.returns_guard
            && !self.locks.is_empty()
            && self.name != ROOT_FN
            && !self.locks.iter().any(|l| l.name == ROOT_STATIC)
    }
}

/// Pass 2: every fn with a body.
struct FnCollector<'a> {
    lines: Vec<&'a str>,
    decls: &'a DeclCollector,
    test_scope: Vec<bool>,
    impl_ty: Vec<Option<String>>,
    fns: Vec<FnFacts>,
}

/// Does a `// test-lock: standalone — <reason>` sit in the comment/attribute
/// lines directly above the `fn`, or inside its body?
fn has_marker(lines: &[&str], fn_line: usize, end_line: usize) -> bool {
    let valid = |l: &str| {
        l.split_once(STANDALONE_MARKER).is_some_and(|(_, rest)| {
            let rest = rest.trim_start();
            let rest = rest
                .strip_prefix('—')
                .or_else(|| rest.strip_prefix('-'))
                .unwrap_or("");
            !rest.trim().is_empty()
        })
    };
    // 1-based `fn_line`: the line above is index fn_line - 2.
    let mut i = fn_line.saturating_sub(1);
    while i > 0 {
        let l = lines[i - 1].trim_start();
        if !(l.starts_with("//") || l.starts_with("#[")) {
            break;
        }
        if valid(l) {
            return true;
        }
        i -= 1;
    }
    (fn_line..=end_line.min(lines.len())).any(|n| valid(lines[n - 1]))
}

impl FnCollector<'_> {
    fn in_test(&self) -> bool {
        self.test_scope.iter().any(|t| *t)
    }

    fn record(&mut self, sig: &syn::Signature, block: &syn::Block, test: bool) {
        let mut scan = BodyScan {
            statics: Some(&self.decls.test_statics),
            ..BodyScan::default()
        };
        scan.visit_block(block);
        let discarded = std::mem::take(&mut scan.discarded);
        let calls = scan
            .calls
            .into_iter()
            .map(|mut c| {
                c.discarded = discarded.contains(&c.pos);
                c
            })
            .collect();
        let returns_guard = match &sig.output {
            syn::ReturnType::Default => false,
            syn::ReturnType::Type(_, ty) => {
                let tf = type_facts(ty);
                tf.idents.iter().any(|i| {
                    GUARD_TYPES.contains(&i.as_str())
                        || self.decls.guard_structs.contains(i)
                        || (i == "Self"
                            && self
                                .impl_ty
                                .last()
                                .cloned()
                                .flatten()
                                .is_some_and(|t| self.decls.guard_structs.contains(&t)))
                })
            }
        };
        let line = sig.fn_token.span.start().line;
        let end = block.brace_token.span.close().start().line;
        self.fns.push(FnFacts {
            name: sig.ident.to_string(),
            line,
            in_test: test || self.in_test(),
            returns_guard,
            marker: has_marker(&self.lines, line, end),
            calls,
            locks: scan.locks,
        });
    }
}

impl<'ast> Visit<'ast> for FnCollector<'_> {
    fn visit_item_mod(&mut self, m: &'ast syn::ItemMod) {
        let t = attrs_are_test(&m.attrs) || m.ident == "tests" || m.ident == "test_support";
        self.test_scope.push(t);
        visit::visit_item_mod(self, m);
        self.test_scope.pop();
    }
    fn visit_item_fn(&mut self, f: &'ast syn::ItemFn) {
        let t = attrs_are_test(&f.attrs);
        self.record(&f.sig, &f.block, t);
        self.test_scope.push(t);
        self.impl_ty.push(None);
        visit::visit_item_fn(self, f);
        self.impl_ty.pop();
        self.test_scope.pop();
    }
    fn visit_item_impl(&mut self, i: &'ast syn::ItemImpl) {
        let ty = match &*i.self_ty {
            syn::Type::Path(tp) => tp.path.segments.last().map(|s| s.ident.to_string()),
            _ => None,
        };
        self.test_scope.push(attrs_are_test(&i.attrs));
        self.impl_ty.push(ty);
        visit::visit_item_impl(self, i);
        self.impl_ty.pop();
        self.test_scope.pop();
    }
    fn visit_impl_item_fn(&mut self, f: &'ast syn::ImplItemFn) {
        let t = attrs_are_test(&f.attrs);
        self.record(&f.sig, &f.block, t);
        self.test_scope.push(t);
        self.impl_ty.push(None);
        visit::visit_impl_item_fn(self, f);
        self.impl_ty.pop();
        self.test_scope.pop();
    }
}

/// Parse one file's facts. `Err` only when it does not parse.
fn scan_source(src: &str) -> syn::Result<Vec<FnFacts>> {
    let file = syn::parse_file(src)?;
    let mut decls = DeclCollector::default();
    decls.visit_file(&file);
    let mut fns = FnCollector {
        lines: src.lines().collect(),
        decls: &decls,
        test_scope: Vec::new(),
        impl_ty: Vec::new(),
        fns: Vec::new(),
    };
    fns.visit_file(&file);
    Ok(fns.fns)
}

/// How a constructor was classified.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Verdict {
    Child,
    Standalone,
}

/// A violation, named by file, line and fn.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Finding {
    file: String,
    line: usize,
    name: String,
    problem: String,
}

/// The tree-wide result.
#[derive(Debug, Default)]
struct Report {
    /// `(file, fn name, verdict)` for every constructor found.
    constructors: Vec<(String, String, Verdict)>,
    findings: Vec<Finding>,
}

/// Is this constructor a compliant child? `Err` names why not.
fn check_constructor(f: &FnFacts) -> Result<(), String> {
    for lock in &f.locks {
        if lock.via_hierarchy {
            continue;
        }
        let before: Vec<&Call> = f
            .calls
            .iter()
            .filter(|c| HIERARCHY_ROOTS.contains(&c.name.as_str()) && c.pos < lock.pos)
            .collect();
        if before.iter().any(|c| !c.discarded) {
            continue;
        }
        if !before.is_empty() {
            return Err(format!(
                "takes `env_lock()` only into `let _ = …`, which releases it at once, before \
                 locking test static `{}`",
                lock.name
            ));
        }
        return Err(format!(
            "locks test static `{}` without first taking `env_lock()`",
            lock.name
        ));
    }
    Ok(())
}

/// Evaluate every file's facts together: constructors are checked per fn, the
/// standalone audit and inline rule need the tree-wide set of child names.
fn evaluate(files: &[(String, Vec<FnFacts>)]) -> Report {
    let mut report = Report::default();
    let mut child_names: HashSet<String> = HashSet::new();
    let mut standalone: Vec<(usize, String)> = Vec::new(); // (file index, name)

    for (fi, (file, fns)) in files.iter().enumerate() {
        for f in fns.iter().filter(|f| f.is_constructor()) {
            match check_constructor(f) {
                Ok(()) => {
                    child_names.insert(f.name.clone());
                    report
                        .constructors
                        .push((file.clone(), f.name.clone(), Verdict::Child));
                }
                Err(_) if f.marker => {
                    standalone.push((fi, f.name.clone()));
                    report
                        .constructors
                        .push((file.clone(), f.name.clone(), Verdict::Standalone));
                }
                Err(problem) => report.findings.push(Finding {
                    file: file.clone(),
                    line: f.line,
                    name: f.name.clone(),
                    problem,
                }),
            }
        }
    }

    let is_hierarchy = |name: &str| HIERARCHY_ROOTS.contains(&name) || child_names.contains(name);

    for (fi, (file, fns)) in files.iter().enumerate() {
        // Same-file closure: which fns reach the hierarchy at all?
        let mut by_name: HashMap<&str, Vec<usize>> = HashMap::new();
        for (i, f) in fns.iter().enumerate() {
            by_name.entry(f.name.as_str()).or_default().push(i);
        }
        let mut reaches: Vec<bool> = fns
            .iter()
            .map(|f| {
                f.calls
                    .iter()
                    .any(|c| !c.discarded && is_hierarchy(&c.name))
            })
            .collect();
        loop {
            let mut changed = false;
            for (i, f) in fns.iter().enumerate() {
                if reaches[i] {
                    continue;
                }
                if f.calls.iter().any(|c| {
                    by_name
                        .get(c.name.as_str())
                        .is_some_and(|ix| ix.iter().any(|&j| j != i && reaches[j]))
                }) {
                    reaches[i] = true;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }

        // Standalone audit.
        for (_, s) in standalone.iter().filter(|(sfi, _)| *sfi == fi) {
            for (i, f) in fns.iter().enumerate() {
                if &f.name == s || !reaches[i] {
                    continue;
                }
                if f.calls.iter().any(|c| &c.name == s) {
                    report.findings.push(Finding {
                        file: file.clone(),
                        line: f.line,
                        name: f.name.clone(),
                        problem: format!(
                            "holds `{s}` (marked `{STANDALONE_MARKER}`) together with the env-lock \
                             hierarchy — a co-held lock is not standalone; make `{s}` a child"
                        ),
                    });
                }
            }
        }

        // Inline co-holders.
        for f in fns.iter().filter(|f| f.in_test && !f.is_constructor()) {
            let Some(first_lock) = f
                .locks
                .iter()
                .filter(|l| !l.via_hierarchy)
                .min_by_key(|l| l.pos)
            else {
                continue;
            };
            let first_h = f
                .calls
                .iter()
                .filter(|c| !c.discarded && is_hierarchy(&c.name))
                .map(|c| c.pos)
                .min();
            if first_h.is_some_and(|h| h > first_lock.pos) {
                report.findings.push(Finding {
                    file: file.clone(),
                    line: f.line,
                    name: f.name.clone(),
                    problem: format!(
                        "locks test static `{}` BEFORE taking the env-lock hierarchy — take \
                         `env_lock()` first",
                        first_lock.name
                    ),
                });
            }
        }
    }
    report.findings.sort();
    report
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
fn every_test_lock_constructor_is_a_child_of_env_lock() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let paths = rs_files(&root);
    assert!(
        paths.len() > MIN_FILES_WALKED,
        "walked only {} .rs files under {} — the guard scanned nothing",
        paths.len(),
        root.display()
    );
    let mut files = Vec::new();
    for path in &paths {
        let rel = path
            .strip_prefix(&root)
            .unwrap_or(path)
            .display()
            .to_string();
        let src =
            std::fs::read_to_string(path).unwrap_or_else(|e| panic!("reading src/{rel}: {e}"));
        // No `Mutex` in the text ⇒ no test static and no lock to check.
        if !src.contains("Mutex") {
            continue;
        }
        let fns = scan_source(&src).unwrap_or_else(|e| {
            panic!(
                "src/{rel}:{}: could not parse with syn ({e}) — the test-lock hierarchy guard \
                 cannot vouch for a file it cannot read, so it refuses rather than skipping it",
                e.span().start().line
            )
        });
        files.push((format!("src/{rel}"), fns));
    }
    let report = evaluate(&files);

    eprintln!(
        "test-lock constructors detected: {} (floor >={MIN_CONSTRUCTORS}): {:?}",
        report.constructors.len(),
        report.constructors
    );
    let lines: Vec<String> = report
        .findings
        .iter()
        .map(|f| format!("{}:{} {} — {}", f.file, f.line, f.name, f.problem))
        .collect();
    assert!(
        lines.is_empty(),
        "{} test-lock hierarchy violation(s):\n  {}\n\n\
         `env_lock()` is the ROOT of the test-lock hierarchy. Every test lock that can be held \
         together with it (or with another child) must take it FIRST and hold it for the guard's \
         life — `crate::test_env::hierarchy_lock(&YOUR_STATIC)` does both — so two tests can take \
         them in either order without an AB/BA deadlock that hangs `cargo test`. A lock that is \
         genuinely only ever held alone may instead carry `// test-lock: standalone — <reason>` \
         above its constructor. Plan \
         `2026-10-02-plan-capture-test-pin-and-env-lock-are-taken-in-opposite-orders-so-one-cargo-test-run-can-deadlock`.",
        lines.len(),
        lines.join("\n  ")
    );
    for required in REQUIRED_CONSTRUCTORS {
        assert!(
            report
                .constructors
                .iter()
                .any(|(_, n, v)| n == required && *v == Verdict::Child),
            "the scan did not see `{required}` as a compliant child of env_lock — either the \
             detector broke (and the empty finding list above proved nothing) or it was deleted"
        );
    }
    assert!(
        report.constructors.len() >= MIN_CONSTRUCTORS,
        "found only {} test-lock constructors — the detector has stopped recognising them",
        report.constructors.len()
    );
}

/// The guard must be SEEN to fail: run it over synthetic source covering every
/// shape it claims to handle.
#[test]
fn the_guard_flags_inverted_test_locks_and_passes_children() {
    const SRC: &str = r#"
#[cfg(test)]
static ITEM_LEVEL: std::sync::Mutex<()> = std::sync::Mutex::new(());
static PRODUCTION: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
pub fn item_level_child() -> crate::test_env::TestLockGuard {
    crate::test_env::hierarchy_lock(&ITEM_LEVEL)
}

#[cfg(test)]
pub fn item_level_inverted() -> std::sync::MutexGuard<'static, ()> {
    ITEM_LEVEL.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn production_lock_is_not_a_test_lock() -> std::sync::MutexGuard<'static, ()> {
    PRODUCTION.lock().unwrap()
}

#[cfg(test)]
pub struct Pin(std::sync::MutexGuard<'static, ()>, crate::test_env::EnvLockGuard);

#[cfg(test)]
pub fn once_lock_child() -> Pin {
    static LOCK: OnceLock<std::sync::Mutex<()>> = OnceLock::new();
    let env = crate::test_env::env_lock();
    let g = LOCK.get_or_init(|| std::sync::Mutex::new(())).lock().unwrap();
    Pin(g, env)
}

#[cfg(test)]
pub fn once_lock_inverted() -> Pin {
    static LOCK2: OnceLock<std::sync::Mutex<()>> = OnceLock::new();
    let g = LOCK2.get_or_init(|| std::sync::Mutex::new(())).lock().unwrap();
    let env = crate::test_env::env_lock();
    Pin(g, env)
}

#[cfg(test)]
struct Marker(std::sync::MutexGuard<'static, ()>);
#[cfg(test)]
impl Marker {
    fn set_inverted() -> Self {
        Marker(ITEM_LEVEL.lock().unwrap())
    }
}

#[cfg(test)]
mod tests {
    static DATA: std::sync::Mutex<Vec<u8>> = std::sync::Mutex::new(Vec::new());

    fn in_fn_static_child() -> crate::test_env::TestLockGuard {
        static INNER: std::sync::Mutex<()> = std::sync::Mutex::new(());
        crate::test_env::hierarchy_lock(&INNER)
    }

    fn discarded_env_is_not_held() -> std::sync::MutexGuard<'static, ()> {
        static D: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _ = crate::test_env::env_lock();
        D.lock().unwrap()
    }

    fn ambient_first_child() -> (crate::test_env::IsolatedAmbient, std::sync::MutexGuard<'static, ()>) {
        static A: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let amb = crate::test_env::isolated_ambient();
        (amb, A.lock().unwrap())
    }

    // test-lock: standalone — module-private, its holders take no other lock
    fn quiet_standalone() -> std::sync::MutexGuard<'static, ()> {
        static Q: std::sync::Mutex<()> = std::sync::Mutex::new(());
        Q.lock().unwrap()
    }

    // test-lock: standalone — claimed alone, but `co_holds_standalone` disagrees
    fn lying_standalone() -> std::sync::MutexGuard<'static, ()> {
        static L: std::sync::Mutex<()> = std::sync::Mutex::new(());
        L.lock().unwrap()
    }

    // test-lock: standalone
    fn marker_without_a_reason() -> std::sync::MutexGuard<'static, ()> {
        static R: std::sync::Mutex<()> = std::sync::Mutex::new(());
        R.lock().unwrap()
    }

    static ALIASED: StdMutex<()> = StdMutex::new(());
    fn aliased_inverted() -> std::sync::MutexGuard<'static, ()> {
        ALIASED.lock().unwrap()
    }

    fn data_mutex_is_not_a_serializer() -> std::sync::MutexGuard<'static, Vec<u8>> {
        DATA.lock().unwrap()
    }

    fn reaches_the_hierarchy() {
        let _g = crate::test_env::env_lock();
    }

    #[test]
    fn uses_the_quiet_one() {
        let _q = quiet_standalone();
    }

    #[test]
    fn co_holds_standalone() {
        let _l = lying_standalone();
        reaches_the_hierarchy();
    }

    #[test]
    fn co_holds_a_child_in_either_order() {
        let _c = in_fn_static_child();
        let _e = crate::test_env::env_lock();
        let _o = once_lock_child();
    }

    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn inline_env_first() {
        let _e = crate::test_env::env_lock();
        let _s = SERIAL.lock().unwrap();
    }

    #[test]
    fn inline_inverted() {
        let _s = SERIAL.lock().unwrap();
        let _p = once_lock_child();
    }

    #[test]
    fn inline_alone() {
        let _s = SERIAL.lock().unwrap();
    }
}
"#;
    let fns = scan_source(SRC).expect("synthetic source parses");
    let report = evaluate(&[("synthetic.rs".to_string(), fns)]);

    let mut children: Vec<&str> = report
        .constructors
        .iter()
        .filter(|(_, _, v)| *v == Verdict::Child)
        .map(|(_, n, _)| n.as_str())
        .collect();
    children.sort_unstable();
    assert_eq!(
        children,
        [
            "ambient_first_child",
            "in_fn_static_child",
            "item_level_child",
            "once_lock_child"
        ],
        "every compliant shape must be recognised as a child"
    );
    let mut standalone: Vec<&str> = report
        .constructors
        .iter()
        .filter(|(_, _, v)| *v == Verdict::Standalone)
        .map(|(_, n, _)| n.as_str())
        .collect();
    standalone.sort_unstable();
    assert_eq!(standalone, ["lying_standalone", "quiet_standalone"]);

    let mut flagged: Vec<&str> = report.findings.iter().map(|f| f.name.as_str()).collect();
    flagged.sort_unstable();
    assert_eq!(
        flagged,
        [
            "aliased_inverted",
            "co_holds_standalone",
            "discarded_env_is_not_held",
            "inline_inverted",
            "item_level_inverted",
            "marker_without_a_reason",
            "once_lock_inverted",
            "set_inverted",
        ],
        "the guard must flag exactly the inverted constructors, the co-held standalone, and the \
         inverted inline co-holder — no more, no fewer"
    );
    let discarded = report
        .findings
        .iter()
        .find(|f| f.name == "discarded_env_is_not_held")
        .expect("flagged");
    assert!(
        discarded.problem.contains("let _ ="),
        "{}",
        discarded.problem
    );

    // The line it reports is the fn's own line, so a failure is clickable.
    let expected_line = SRC
        .lines()
        .position(|l| l.contains("fn item_level_inverted"))
        .expect("fixture line")
        + 1;
    let f = report
        .findings
        .iter()
        .find(|f| f.name == "item_level_inverted")
        .expect("flagged");
    assert_eq!(f.line, expected_line);
}

/// A file that does not parse is an error the real-tree test turns into a
/// named failure — never a silent skip.
#[test]
fn the_guard_refuses_source_it_cannot_parse() {
    assert!(scan_source("static L: Mutex<()> = Mutex::new(()); fn broken( {").is_err());
}
