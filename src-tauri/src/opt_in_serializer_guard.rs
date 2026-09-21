//! Source invariant: a module-local test serialiser is taken by EVERY test of
//! the module that defines it — or the module is on the allowlist with a
//! reason.
//!
//! `cargo test` runs a binary's tests as threads in one process. When a module
//! keeps process-global state and serialises "the tests that touch it" on a
//! module-local `static … Mutex<()>` — the `series_lock()` shape at
//! `mcp_api.rs` (`fn series_lock() -> MutexGuard<'static, ()>` over a fn-local
//! `static LOCK`), or a bare `static POOL_SERIAL: Mutex<()>` that tests
//! `.lock()` directly — the lock excludes only the tests that opt in. The next
//! test written in that module, by someone who did not read the doc comment
//! above the static, runs in parallel with all of them and reds one of them at
//! random with a panic that names an assertion, not the lock. Measured: the
//! Phase 0 census of plan `2026-09-17-runner-tests-share-in-process-mutable-state`
//! found `mcp_api::memory_search_enrichment_tests::a_skip_lands_in_its_own_series_and_not_in_enriched`
//! red 7/20 in the suite and green 6/6 alone — a module WITH a serialiser, and
//! a test that did not take it. A lock only opt-ins take "excludes nothing"
//! (the same lesson `2026-09-02-coord-unit-tests-share-process-globals-and-a-fixed-tmp-path`
//! recorded for qontinui-coord).
//!
//! This test is the source half of that plan's standing guard (Phase 5, D3):
//! the census (`scripts/test-interleave-census.mjs`, nightly in
//! `flake-escalation.yml`) is the OBSERVATION and finds the next singleton
//! whatever shape it takes; this guard is the one thing a source scan can
//! actually prove about the class — that no `#[cfg(test)]` module defines a
//! serialiser only some of its tests take. It ENUMERATES rather than trusting
//! the plan's prior-art table (which was already stale by eight sites when it
//! was written).
//!
//! # What counts
//!
//! A **test module** is a `mod` carrying a `#[cfg(…)]` attribute that names
//! `test` (`#[cfg(test)]`, `#[cfg(any(test, debug_assertions))]`), any
//! ancestor of which does, or the root of a file whose path is a test file
//! (`tests.rs`, `*_tests.rs`, a `tests/` directory). A **test fn** is a fn
//! carrying an attribute whose path ends in `test`, as in
//! `env_write_lock_guard`.
//!
//! A **serialiser** is one of:
//!
//! * a module-level `static NAME: T` inside a test module whose type is a unit
//!   mutex — `Mutex<()>` under any qualifier or alias ending in `Mutex`
//!   (`std::sync::Mutex<()>`, `tokio::sync::Mutex<()>`, `StdMutex<()>`), bare
//!   or wrapped (`OnceLock<Mutex<()>>`, `Lazy<StdMutex<()>>`), together with
//!   every non-test fn of the same module that mentions it and returns a unit
//!   guard (`MutexGuard<'_, ()>`, `OwnedMutexGuard<()>`) — its **accessors**;
//! * a non-test fn inside a test module that returns a unit guard and declares
//!   such a static in its own body (the `series_lock()` shape — the static is
//!   reachable through the fn alone, so the fn IS the serialiser);
//! * a non-test fn inside a test module that returns a unit guard and declares
//!   no static at all — a **delegating accessor** (`health_lock()` in
//!   `device_jwt_refresher::tenant_slot_refresh_tests` returns
//!   `super::posture_test_lock()`). What it delegates to is someone else's
//!   population; what its own module's tests must do is call it — or take
//!   the lock-shaped callee (`…lock`) it delegates to directly, which is the
//!   same lock.
//!
//! A serialiser's **population** is every test fn in the module that defines
//! it, nested submodules included. A test **takes** a serialiser when its body
//! — closures, nested blocks and macro arguments included — names the static
//! or calls an accessor, or calls a fn defined in the same file that does
//! (closed transitively over same-file calls, keyed exactly as
//! `env_write_lock_guard` keys them: free fns by name, impl fns by
//! `Type::name`). A finding is a serialiser with at least one test in its
//! population that does not take it; the failure names the file, the module,
//! the serialiser and every missing test with its line.
//!
//! # Recognised and excluded by rule (printed, never silent)
//!
//! * A serialiser whose static or accessor is not private (`pub`,
//!   `pub(crate)`, `pub(super)`), or that is declared at file level under its
//!   own `#[cfg(test)]` rather than inside a test module — `posture_test_lock`
//!   (`device_jwt_refresher.rs`), `perf_test_lock` (`settings.rs`),
//!   `restore_forensics_lock` (`coord_mcp.rs`, `pub(super)`), `env_lock`
//!   (`ambient.rs`). Its population spans modules or files by design, so ONE
//!   file's source cannot enumerate it; the guard lists these as
//!   `cross-module` and asserts nothing about them. The module-local accessor
//!   that wraps one (`health_lock()`) is a delegating accessor and IS held to
//!   its own module.
//! * A unit-mutex static declared inside a non-test fn that returns no guard
//!   (`capture_logs_once` in `terminal/session.rs`): the helper holds the lock
//!   for its own duration, so every caller is serialised without opting in —
//!   not this class. The same rule leaves out a fn that returns an RAII
//!   handle WRAPPING the guard together with the state it protects
//!   (`pin_plan_capture_level_for_test` in `mcp/fleet_policy_poller.rs`,
//!   `MarkerOverride::set` in `coord_mcp.rs`): the state cannot be set without
//!   the lock, so there is nothing to forget — that is the per-test-handle
//!   shape the plan prefers, not the opt-in one. (A module-level static such
//!   a handle wraps is still enumerated on its own terms — `MARKER_OVERRIDE_LOCK`
//!   is listed as cross-module — and a test reaching the handle by a path call
//!   is credited with the static.)
//! * A unit-mutex static declared inside a `#[test]` fn's own body
//!   (`health_monitor.rs`, `observe_publishes_the_failure_count_before_it_reports`)
//!   serialises nothing — only that one test can reach it. Listed as
//!   `scoped-to-one-test` and reported as a finding of its own kind, because
//!   the doc comment above it says "serialise" and the lock does not.
//!
//! # Known limits
//!
//! * **Reach through a METHOD is not closed** — the same limit
//!   `env_write_lock_guard` documents, for the same reason: keying impl fns by
//!   bare name would credit any `X::new()` caller with an unrelated `Y::new()`'s
//!   lock. `MarkerOverride::set(..)` (a path call) IS reached; `x.set(..)` is not.
//! * **Taking is by NAME, not by liveness**: `let _ = series_lock();` drops the
//!   guard at once and still counts. The guard proves that a test reaches the
//!   serialiser, not that it holds it across the assertion.
//! * **A serialiser whose population is by design a subset** (the doc comment
//!   says "the two tests that mutate the store") is a finding here all the
//!   same — that IS the opt-in shape — and goes on the allowlist with that
//!   reason, where the reason is visible and the entry goes stale the day the
//!   module changes. The allowlist is a ratchet: an entry that no longer
//!   matches a finding fails this test by name.
//!
//! Registered in `main.rs` only, like `env_write_lock_guard`, whose `rs_files`
//! and `is_test_attr` it reuses. A sibling module rather than an extension of
//! that guard's `scan_source`: that visitor is flat (no module scoping, the
//! property it checks is file-wide), and this one's whole question is "which
//! module defines this and which tests are in it".

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use proc_macro2::{TokenStream, TokenTree};
use syn::visit::{self, Visit};

use crate::env_write_lock_guard::{is_test_attr, rs_files};

/// Floor for the walk, so a broken path or filter cannot pass vacuously
/// (`env_write_lock_guard` declares the same floor; 1559 files when written).
const MIN_FILES_WALKED: usize = 1000;
/// Floor for the enumerated population: the plan's prior-art table listed 23
/// `static … Mutex<()>` sites by grep, and this guard enumerated 21 test
/// serialisers when it was written (12 module-static, 3 fn-local, 2
/// delegating, 4 cross-module, 1 scoped-to-one-test; the grep's other hits
/// are production locks — `executor::restart_lock`, `coord_register`'s
/// `SPAWN`, `shim_materializer`'s `IDENTITY_MATERIALIZE_LOCK` — or a helper's
/// internal lock, `capture_logs_once`). Below this the detector broke, not
/// the tree. Printed by the guard, so re-measure with `--nocapture`.
const MIN_SERIALIZERS: usize = 15;

/// Sites the guard flags today that stay as they are, each with the reason.
/// Keyed by `(file relative to src/, module path, serialiser label)` exactly
/// as the failure message prints them. An entry that matches no finding fails
/// the guard: a fixed module must leave the list.
///
/// Every reason below is one of two shapes. "Phase 4" is the one site whose
/// remedy is already scheduled: `mcp_api.rs`'s `series_lock` is deleted by
/// Phase 4 of plan `2026-09-17-runner-tests-share-in-process-mutable-state`,
/// which gives each test a private counter instance, so widening the lock now
/// would be work the next PR removes. Every other reason states which tests
/// stand outside the lock and why that is safe TODAY — the resource the lock
/// guards is named, and the tests outside it do not reach that resource. That
/// claim is a source reading by the author of the entry, not a proof; the
/// nightly census is what checks it, and a `SUITE-ONLY` verdict on a module
/// listed here means the reason was wrong and the fix is to take the lock,
/// not to reword the entry.
const ALLOWLIST: &[(&str, &str, &str, &str)] = &[
    (
        "mcp_api.rs",
        "memory_search_enrichment_tests",
        "series_lock",
        "Phase 4 replaces series_lock with a per-test handle (MemoryEnrichCounters), which \
         deletes the lock; the 7/20 SUITE-ONLY red the census measured is that phase's target",
    ),
    (
        "health_monitor.rs",
        "tests",
        "SERIAL",
        "scoped-to-one-test: declared inside observe_publishes_the_failure_count_before_it_reports, \
         so it serialises that test against nothing; the other test that writes \
         BACKEND_WEDGED (stopping_the_monitor_clears_the_wedge_latches) asserts on the \
         latches it sets itself. Hoisting the static and taking it in both is a follow-up \
         recorded here, not silence",
    ),
    (
        "agent_runtime.rs",
        "tests",
        "CONT_GUARD_LOCK",
        "guards the continuation registry + admitted-launch cap (clear_continuation_registry); \
         the 154 tests outside it never call evaluate_continuation_guard* or the registry \
         accessors (grep-verified 2026-09-21) — command builders, payload shapes, env scrubs",
    ),
    (
        "ai_provider/oauth_refresh.rs",
        "tests",
        "REFRESH_LOG_GUARD",
        "guards REFRESH_REQUESTS (the recorded background-refresh log); the 5 tests outside it \
         (has_valid_credentials_*, hard-failure backoff) neither queue nor drain a request",
    ),
    (
        "capability_manifest.rs",
        "tests",
        "store_lock",
        "guards the process-wide provisioning store (reset_provision_store); the 29 tests \
         outside it never touch the store — spec population, unit shapes, rendering",
    ),
    (
        "commands/transcript.rs",
        "tests",
        "CACHE_TESTS_ARE_SERIAL",
        "guards the scan cache, its counters and the process-wide scan dispatcher; the 4 tests \
         outside it are pure (entry_is_servable over arguments) or read this file's source",
    ),
    (
        "embedded_pg.rs",
        "tests",
        "PG_TEST_LOCK",
        "a tokio::sync::Mutex serialising the tests that boot a PostgreSQL cluster (disk, port \
         and archive contention); the 18 tests outside it parse pid files and probe a per-test \
         tempdir and never start a server",
    ),
    (
        "env_agent/enroll.rs",
        "tests",
        "slot_lock",
        "guards ENROLL_IN_FLIGHT; the 10 tests outside it never reach with_enroll_slot or \
         run_enroll — request serialisation, backend resolution, machine.json parsing",
    ),
    (
        "mcp/test_fixtures.rs",
        "tests",
        "TEST_LOCK",
        "guards the registry() singleton; the 48 tests outside it project statuses and \
         parse route tables and never call registry()",
    ),
    (
        "process_helpers.rs",
        "timeout_tests",
        "GAUGE_TESTS",
        "guards assertions on the live_pipe_readers() gauge (two tests, one of which leaves \
         readers alive on purpose); the 12 tests outside it spawn children that bump the \
         gauge but assert nothing about it, as the static's own doc says",
    ),
    (
        "terminal/auto_response.rs",
        "tests",
        "RULES_TEST_LOCK",
        "guards COMPILED_RULES (reload_rules / rules_active / process); the 18 tests outside \
         it score prompts, parse JSON and compute delays over their own values",
    ),
    (
        "terminal/mod.rs",
        "tests",
        "quiet_credential_posture",
        "delegates to the crate-wide posture_test_lock(); the tests outside it that render \
         runner_context() assert on lines the posture does not write (source marker, api \
         port, memory clause) or take posture_test_lock() directly and are credited",
    ),
    (
        "wedge_diagnostics.rs",
        "tests",
        "POOL_SERIAL",
        "guards the process-global LANES slot counts; the 25 tests outside it drive a private \
         LaneTable (fresh_lane_table / spawn_blocking_tracked_in), scan this file's source, \
         or measure a child process — the per-test-handle shape this plan generalises",
    ),
];

/// How a serialiser was recognised.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// A module-level unit-mutex static (with zero or more accessors).
    ModuleStatic,
    /// A non-test fn returning a unit guard over a static declared in its body.
    FnLocal,
    /// A non-test fn returning a unit guard that declares no static of its own.
    Delegating,
    /// A unit-mutex static declared inside a `#[test]` fn — serialises nothing.
    ScopedToOneTest,
    /// Not private, or declared at file level under `#[cfg(test)]`: its
    /// population is not one module's. Enumerated, asserted about nothing.
    CrossModule,
}

/// One serialiser the scanner recognised.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Serializer {
    /// Module path within the file (`tests`, `tests::nested`), `""` at file level.
    module: String,
    /// The static's name for `ModuleStatic` / `ScopedToOneTest` / `CrossModule`
    /// statics; the fn's name for `FnLocal` / `Delegating`.
    label: String,
    line: usize,
    kind: Kind,
    /// Every name a test may take it by: the static (when module-level) and
    /// each accessor fn.
    handles: BTreeSet<String>,
    /// Why it is `CrossModule`, when it is.
    why: Option<String>,
}

/// A test fn in a serialiser's population that does not take it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct MissingTest {
    line: usize,
    name: String,
}

/// One serialiser with its population verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Finding {
    serializer: Serializer,
    population: usize,
    missing: Vec<MissingTest>,
}

/// The verdict for one source file.
#[derive(Debug, Default)]
struct FileReport {
    /// Every serialiser recognised, of every kind.
    serializers: Vec<Serializer>,
    /// Serialisers with a non-empty population and at least one missing test,
    /// plus every `ScopedToOneTest` (whose finding is its kind).
    findings: Vec<Finding>,
}

// ---------------------------------------------------------------------------
// Type shapes
// ---------------------------------------------------------------------------

fn is_unit_tuple(ty: &syn::Type) -> bool {
    matches!(ty, syn::Type::Tuple(t) if t.elems.is_empty())
}

fn type_args(seg: &syn::PathSegment) -> Vec<&syn::Type> {
    match &seg.arguments {
        syn::PathArguments::AngleBracketed(a) => a
            .args
            .iter()
            .filter_map(|g| match g {
                syn::GenericArgument::Type(t) => Some(t),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// `…Mutex<()>`, bare or wrapped in any generic (`OnceLock<Mutex<()>>`,
/// `Lazy<StdMutex<()>>`).
fn is_unit_mutex_type(ty: &syn::Type) -> bool {
    let syn::Type::Path(tp) = ty else {
        return false;
    };
    let Some(last) = tp.path.segments.last() else {
        return false;
    };
    let args = type_args(last);
    if last.ident.to_string().ends_with("Mutex") && args.len() == 1 && is_unit_tuple(args[0]) {
        return true;
    }
    tp.path
        .segments
        .iter()
        .flat_map(type_args)
        .any(is_unit_mutex_type)
}

/// `…MutexGuard<'_, ()>` / `…OwnedMutexGuard<()>`: a guard over a unit mutex.
fn is_unit_guard_type(ty: &syn::Type) -> bool {
    let syn::Type::Path(tp) = ty else {
        return false;
    };
    let Some(last) = tp.path.segments.last() else {
        return false;
    };
    last.ident.to_string().ends_with("MutexGuard")
        && type_args(last).iter().any(|t| is_unit_tuple(t))
}

fn returns_unit_guard(sig: &syn::Signature) -> bool {
    match &sig.output {
        syn::ReturnType::Type(_, ty) => is_unit_guard_type(ty),
        syn::ReturnType::Default => false,
    }
}

/// `#[cfg(test)]`, `#[cfg(any(test, …))]` — a `cfg` attribute naming `test`.
fn is_cfg_test_attr(attr: &syn::Attribute) -> bool {
    if !attr.path().is_ident("cfg") {
        return false;
    }
    fn names_test(tokens: TokenStream) -> bool {
        tokens.into_iter().any(|t| match t {
            TokenTree::Ident(i) => i == "test",
            TokenTree::Group(g) => names_test(g.stream()),
            _ => false,
        })
    }
    attr.parse_args::<TokenStream>().is_ok_and(names_test)
}

fn is_private(vis: &syn::Visibility) -> bool {
    matches!(vis, syn::Visibility::Inherited)
}

fn vis_label(vis: &syn::Visibility) -> String {
    match vis {
        syn::Visibility::Public(_) => "pub".to_string(),
        syn::Visibility::Restricted(r) => {
            let p: Vec<String> = r
                .path
                .segments
                .iter()
                .map(|s| s.ident.to_string())
                .collect();
            format!("pub({})", p.join("::"))
        }
        syn::Visibility::Inherited => "private".to_string(),
    }
}

/// Is `path` a test file by name: `tests.rs`, `*_tests.rs`, or under `tests/`.
fn is_test_file(rel: &str) -> bool {
    let stem = Path::new(rel)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    stem == "tests"
        || stem.ends_with("_tests")
        || rel.contains("/tests/")
        || rel.starts_with("tests/")
}

// ---------------------------------------------------------------------------
// Body facts
// ---------------------------------------------------------------------------

/// What one fn body does DIRECTLY — before same-file calls are resolved.
#[derive(Debug, Default)]
struct BodyFacts {
    /// Every identifier the body names through a path or a macro's tokens.
    idents: BTreeSet<String>,
    /// Every path call in the body, as `name` and `Qualifier::name` (with
    /// `Self` resolved to the enclosing impl's type) — the same keying as
    /// `env_write_lock_guard`.
    calls: BTreeSet<String>,
    /// Unit-mutex statics declared inside the body.
    local_statics: Vec<(String, usize)>,
}

fn scan_tokens(tokens: TokenStream, facts: &mut BodyFacts, self_ty: Option<&str>) {
    let trees: Vec<TokenTree> = tokens.into_iter().collect();
    for (i, tree) in trees.iter().enumerate() {
        match tree {
            TokenTree::Group(g) => scan_tokens(g.stream(), facts, self_ty),
            TokenTree::Ident(id) => {
                let name = id.to_string();
                facts.idents.insert(name.clone());
                let is_call = matches!(
                    trees.get(i + 1),
                    Some(TokenTree::Group(g)) if g.delimiter() == proc_macro2::Delimiter::Parenthesis
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
                facts.calls.insert(name.clone());
                if let Some(q) = qualifier {
                    let q = if q == "Self" {
                        self_ty.unwrap_or(&q).to_string()
                    } else {
                        q
                    };
                    facts.calls.insert(format!("{q}::{name}"));
                }
            }
            TokenTree::Punct(_) | TokenTree::Literal(_) => {}
        }
    }
}

struct BodyScanner {
    facts: BodyFacts,
    self_ty: Option<String>,
}

impl<'ast> Visit<'ast> for BodyScanner {
    fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
        if let syn::Expr::Path(p) = &*call.func {
            let segs: Vec<String> = p
                .path
                .segments
                .iter()
                .map(|s| s.ident.to_string())
                .collect();
            if let Some(name) = segs.last() {
                self.facts.calls.insert(name.clone());
                if segs.len() >= 2 {
                    let q = &segs[segs.len() - 2];
                    let q = if q == "Self" {
                        self.self_ty.clone().unwrap_or_else(|| q.clone())
                    } else {
                        q.clone()
                    };
                    self.facts.calls.insert(format!("{q}::{name}"));
                }
            }
        }
        visit::visit_expr_call(self, call);
    }

    fn visit_path(&mut self, p: &'ast syn::Path) {
        for s in &p.segments {
            self.facts.idents.insert(s.ident.to_string());
        }
        visit::visit_path(self, p);
    }

    fn visit_macro(&mut self, m: &'ast syn::Macro) {
        scan_tokens(m.tokens.clone(), &mut self.facts, self.self_ty.as_deref());
        visit::visit_macro(self, m);
    }

    fn visit_item_static(&mut self, s: &'ast syn::ItemStatic) {
        if is_unit_mutex_type(&s.ty) {
            self.facts
                .local_statics
                .push((s.ident.to_string(), s.ident.span().start().line));
        }
        visit::visit_item_static(self, s);
    }
}

// ---------------------------------------------------------------------------
// Module-aware collection
// ---------------------------------------------------------------------------

struct FnRecord {
    /// `name` for a free fn, `Type::name` for an impl/trait method.
    key: String,
    name: String,
    line: usize,
    module: String,
    in_test_module: bool,
    is_test: bool,
    /// The item itself carries `#[cfg(test)]` (a file-level test-only fn).
    cfg_test_item: bool,
    private: bool,
    vis: String,
    returns_unit_guard: bool,
    facts: BodyFacts,
}

struct StaticRecord {
    name: String,
    line: usize,
    module: String,
    in_test_module: bool,
    /// The item itself carries `#[cfg(test)]` (a file-level test-only static).
    cfg_test_item: bool,
    private: bool,
    vis: String,
}

struct Collector {
    /// Module path segments, outermost first.
    modules: Vec<String>,
    /// Whether each enclosing module (and the file root) is test-gated.
    test_flags: Vec<bool>,
    /// The enclosing impl's self type, innermost last; `None` inside a fn body.
    scope: Vec<Option<String>>,
    fns: Vec<FnRecord>,
    statics: Vec<StaticRecord>,
}

impl Collector {
    fn new(file_root_is_test: bool) -> Self {
        Self {
            modules: Vec::new(),
            test_flags: vec![file_root_is_test],
            scope: Vec::new(),
            fns: Vec::new(),
            statics: Vec::new(),
        }
    }

    fn module_path(&self) -> String {
        self.modules.join("::")
    }

    fn in_test_module(&self) -> bool {
        self.test_flags.iter().any(|f| *f)
    }

    fn record_fn(
        &mut self,
        ident: &syn::Ident,
        attrs: &[syn::Attribute],
        vis: &syn::Visibility,
        sig: &syn::Signature,
        block: &syn::Block,
    ) {
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
            module: self.module_path(),
            in_test_module: self.in_test_module(),
            is_test: attrs.iter().any(is_test_attr),
            cfg_test_item: attrs.iter().any(is_cfg_test_attr),
            private: is_private(vis),
            vis: vis_label(vis),
            returns_unit_guard: returns_unit_guard(sig),
            name,
            facts: scanner.facts,
        });
    }
}

impl<'ast> Visit<'ast> for Collector {
    fn visit_item_mod(&mut self, m: &'ast syn::ItemMod) {
        self.modules.push(m.ident.to_string());
        self.test_flags.push(m.attrs.iter().any(is_cfg_test_attr));
        visit::visit_item_mod(self, m);
        self.test_flags.pop();
        self.modules.pop();
    }

    fn visit_item_static(&mut self, s: &'ast syn::ItemStatic) {
        // Only ITEM-level statics land here: a fn body's statics are seen by
        // the fn's own `BodyScanner`, and `visit_item_fn` below does not
        // descend into the block through this visitor.
        if is_unit_mutex_type(&s.ty) {
            self.statics.push(StaticRecord {
                name: s.ident.to_string(),
                line: s.ident.span().start().line,
                module: self.module_path(),
                in_test_module: self.in_test_module(),
                cfg_test_item: s.attrs.iter().any(is_cfg_test_attr),
                private: is_private(&s.vis),
                vis: vis_label(&s.vis),
            });
        }
    }

    fn visit_item_fn(&mut self, f: &'ast syn::ItemFn) {
        self.record_fn(&f.sig.ident, &f.attrs, &f.vis, &f.sig, &f.block);
        // Nested fn items inside the body are recorded as free fns of this
        // module (their statics belong to the enclosing fn's facts already).
        self.scope.push(None);
        for stmt in &f.block.stmts {
            if let syn::Stmt::Item(item) = stmt {
                if !matches!(item, syn::Item::Static(_)) {
                    self.visit_item(item);
                }
            }
        }
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
        self.record_fn(&f.sig.ident, &f.attrs, &f.vis, &f.sig, &f.block);
    }

    fn visit_trait_item_fn(&mut self, f: &'ast syn::TraitItemFn) {
        if let Some(block) = &f.default {
            self.record_fn(
                &f.sig.ident,
                &f.attrs,
                &syn::Visibility::Inherited,
                &f.sig,
                block,
            );
        }
    }
}

// ---------------------------------------------------------------------------
// The scan
// ---------------------------------------------------------------------------

fn in_module(candidate: &str, module: &str) -> bool {
    module.is_empty() || candidate == module || candidate.starts_with(&format!("{module}::"))
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
fn scan_source(src: &str, rel: &str) -> syn::Result<FileReport> {
    let file = syn::parse_file(src)?;
    let mut c = Collector::new(is_test_file(rel));
    c.visit_file(&file);
    let Collector { fns, statics, .. } = c;

    let mut serializers: Vec<Serializer> = Vec::new();

    // Module-level statics.
    for s in &statics {
        let accessors: BTreeSet<String> = fns
            .iter()
            .filter(|f| {
                !f.is_test
                    && f.returns_unit_guard
                    && f.module == s.module
                    && f.facts.idents.contains(&s.name)
            })
            .map(|f| f.name.clone())
            .collect();
        let non_private_accessor = fns.iter().find(|f| {
            !f.is_test
                && f.returns_unit_guard
                && f.module == s.module
                && accessors.contains(&f.name)
                && !f.private
        });
        let mut handles = accessors.clone();
        handles.insert(s.name.clone());
        let (kind, why) = if !s.in_test_module {
            if s.cfg_test_item {
                (
                    Kind::CrossModule,
                    Some("declared at file level under its own #[cfg(test)]".to_string()),
                )
            } else {
                // A production static (`IDENTITY_MATERIALIZE_LOCK`): not a test
                // serialiser at all.
                continue;
            }
        } else if !s.private {
            (Kind::CrossModule, Some(format!("static is {}", s.vis)))
        } else if let Some(a) = non_private_accessor {
            (
                Kind::CrossModule,
                Some(format!("accessor {}() is {}", a.name, a.vis)),
            )
        } else {
            (Kind::ModuleStatic, None)
        };
        serializers.push(Serializer {
            module: s.module.clone(),
            label: s.name.clone(),
            line: s.line,
            kind,
            handles,
            why,
        });
    }

    // Fn-local statics and delegating accessors: non-test fns in a test
    // module that return a unit guard.
    for f in &fns {
        if f.is_test || !f.returns_unit_guard {
            continue;
        }
        let over_module_static = statics
            .iter()
            .any(|s| s.module == f.module && f.facts.idents.contains(&s.name));
        if over_module_static {
            continue; // already an accessor of that static
        }
        let has_local = !f.facts.local_statics.is_empty();
        let (kind, why) = if !f.in_test_module {
            if has_local && f.cfg_test_item {
                // `posture_test_lock` / `perf_test_lock`: a file-level fn over its
                // own static, cfg(test)-gated by attribute rather than module.
                (
                    Kind::CrossModule,
                    Some(format!("declared at file level, {}", f.vis)),
                )
            } else {
                // Production code, or a test-only delegate outside any module:
                // not a test serialiser this guard can place.
                continue;
            }
        } else if !f.private {
            (Kind::CrossModule, Some(format!("accessor is {}", f.vis)))
        } else if has_local {
            (Kind::FnLocal, None)
        } else {
            (Kind::Delegating, None)
        };
        let mut handles = BTreeSet::from([f.name.clone()]);
        if kind == Kind::Delegating {
            // What the accessor delegates TO is a handle as well: a test that
            // takes `posture_test_lock()` directly instead of through
            // `health_lock()` holds the same lock. Only lock-shaped call names
            // (`…lock`) — the accessor may also call a reset helper, and a
            // test calling only that is exactly the unlocked shape.
            for c in &f.facts.calls {
                if !c.contains("::") && c.ends_with("lock") {
                    handles.insert(c.clone());
                }
            }
        }
        serializers.push(Serializer {
            module: f.module.clone(),
            label: f.name.clone(),
            line: f.line,
            kind,
            handles,
            why,
        });
    }

    // Statics declared inside a `#[test]` fn's own body.
    for f in fns.iter().filter(|f| f.is_test) {
        for (name, line) in &f.facts.local_statics {
            serializers.push(Serializer {
                module: f.module.clone(),
                label: name.clone(),
                line: *line,
                kind: Kind::ScopedToOneTest,
                handles: BTreeSet::new(),
                why: Some(format!("declared inside test fn {}", f.name)),
            });
        }
    }

    // Population verdicts.
    let mut by_key: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, f) in fns.iter().enumerate() {
        by_key.entry(f.key.as_str()).or_default().push(i);
    }
    let mut findings = Vec::new();
    for s in &serializers {
        match s.kind {
            Kind::CrossModule => continue,
            Kind::ScopedToOneTest => {
                findings.push(Finding {
                    serializer: s.clone(),
                    population: 0,
                    missing: Vec::new(),
                });
                continue;
            }
            Kind::ModuleStatic | Kind::FnLocal | Kind::Delegating => {}
        }
        let mut takes: Vec<bool> = fns
            .iter()
            .map(|f| f.facts.idents.iter().any(|i| s.handles.contains(i)))
            .collect();
        // Close over same-file calls. Monotone, so this settles.
        loop {
            let mut changed = false;
            for (i, f) in fns.iter().enumerate() {
                if !takes[i] && reaches(&f.facts, i, &by_key, &takes) {
                    takes[i] = true;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        let mut population = 0usize;
        let mut missing = Vec::new();
        for (i, f) in fns.iter().enumerate() {
            if !f.is_test || !in_module(&f.module, &s.module) {
                continue;
            }
            population += 1;
            if !takes[i] {
                missing.push(MissingTest {
                    line: f.line,
                    name: f.name.clone(),
                });
            }
        }
        missing.sort();
        if !missing.is_empty() {
            findings.push(Finding {
                serializer: s.clone(),
                population,
                missing,
            });
        }
    }

    Ok(FileReport {
        serializers,
        findings,
    })
}

fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::ModuleStatic => "module-static",
        Kind::FnLocal => "fn-local",
        Kind::Delegating => "delegating",
        Kind::ScopedToOneTest => "scoped-to-one-test",
        Kind::CrossModule => "cross-module",
    }
}

fn render_finding(rel: &str, f: &Finding) -> String {
    let s = &f.serializer;
    let module = if s.module.is_empty() {
        "<file root>"
    } else {
        s.module.as_str()
    };
    match s.kind {
        Kind::ScopedToOneTest => format!(
            "src/{rel}:{} module `{module}` serialiser `{}` ({}): {} — only that test can \
             take it, so it serialises nothing",
            s.line,
            s.label,
            kind_name(s.kind),
            s.why.as_deref().unwrap_or("")
        ),
        _ => {
            let handles: Vec<String> = s.handles.iter().map(|h| format!("`{h}`")).collect();
            let missing: Vec<String> = f
                .missing
                .iter()
                .map(|m| format!("      src/{rel}:{} {}", m.line, m.name))
                .collect();
            format!(
                "src/{rel}:{} module `{module}` serialiser `{}` ({}, taken by {}): {} of {} \
                 test(s) in the module do not take it:\n{}",
                s.line,
                s.label,
                kind_name(s.kind),
                handles.join(" / "),
                f.missing.len(),
                f.population,
                missing.join("\n")
            )
        }
    }
}

// ---------------------------------------------------------------------------
// The guard
// ---------------------------------------------------------------------------

#[test]
fn every_test_in_a_module_with_a_serializer_takes_it_or_the_module_is_allowlisted() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let files = rs_files(&root);
    assert!(
        files.len() > MIN_FILES_WALKED,
        "walked only {} .rs files under {} — the guard scanned nothing",
        files.len(),
        root.display()
    );

    let mut enumerated: Vec<String> = Vec::new();
    let mut serializer_count = 0usize;
    let mut violations: Vec<String> = Vec::new();
    let mut allowlist_hits: BTreeSet<usize> = BTreeSet::new();
    for file in &files {
        let rel = file
            .strip_prefix(&root)
            .unwrap_or(file)
            .display()
            .to_string()
            .replace('\\', "/");
        let src =
            std::fs::read_to_string(file).unwrap_or_else(|e| panic!("reading src/{rel}: {e}"));
        // A serialiser needs a unit mutex or a unit guard somewhere in the text.
        if !(src.contains("Mutex<()>") || src.contains("MutexGuard")) {
            continue;
        }
        let report = scan_source(&src, &rel).unwrap_or_else(|e| {
            panic!(
                "src/{rel}:{}: could not parse with syn ({e}) — the serialiser guard cannot \
                 vouch for a file it cannot read, so it refuses rather than skipping it",
                e.span().start().line
            )
        });
        for s in &report.serializers {
            serializer_count += 1;
            let module = if s.module.is_empty() {
                "<file root>"
            } else {
                s.module.as_str()
            };
            enumerated.push(format!(
                "  {:<19} src/{rel}:{} {module}::{}{}",
                kind_name(s.kind),
                s.line,
                s.label,
                s.why
                    .as_ref()
                    .map(|w| format!("  ({w})"))
                    .unwrap_or_default()
            ));
        }
        for f in &report.findings {
            let key_module = f.serializer.module.as_str();
            let hit = ALLOWLIST.iter().position(|(af, am, al, _)| {
                *af == rel && *am == key_module && *al == f.serializer.label
            });
            match hit {
                Some(ix) => {
                    allowlist_hits.insert(ix);
                }
                None => violations.push(render_finding(&rel, f)),
            }
        }
    }

    // Printed, not just compared, so the doc figure beside MIN_SERIALIZERS
    // stays verifiable: `cargo test -- --nocapture <this test>` re-measures it.
    eprintln!("test serialisers enumerated: {serializer_count} (floor >{MIN_SERIALIZERS})");
    for line in &enumerated {
        eprintln!("{line}");
    }
    assert!(
        serializer_count > MIN_SERIALIZERS,
        "found only {serializer_count} serialiser(s) — the detector has stopped recognising \
         them, so an empty finding list below would prove nothing:\n{}",
        enumerated.join("\n")
    );

    let stale: Vec<String> = ALLOWLIST
        .iter()
        .enumerate()
        .filter(|(ix, _)| !allowlist_hits.contains(ix))
        .map(|(_, (f, m, l, _))| format!("  src/{f} module `{m}` serialiser `{l}`"))
        .collect();
    assert!(
        stale.is_empty(),
        "{} ALLOWLIST entr{} in opt_in_serializer_guard.rs match no finding — the module was \
         fixed, renamed or moved, so the entry (and its reason) is stale. Remove it:\n{}",
        stale.len(),
        if stale.len() == 1 { "y" } else { "ies" },
        stale.join("\n")
    );

    assert!(
        violations.is_empty(),
        "{} module(s) define a test serialiser that only some of their tests take:\n  {}\n\n\
         A module-local `static … Mutex<()>` — or a `fn …_lock()` returning a guard over one — \
         serialises ONLY the tests that take it; the ones that do not run in parallel with all \
         of them, and it is one of the LOCKED tests that goes red, at random, with a panic \
         naming an assertion rather than the lock. Fix: take the serialiser at the top of every \
         test in the module (`let _g = <serialiser>;`), or — better — give the shared state a \
         per-test handle so there is nothing to serialise (the `LaneTable` shape, \
         `wedge_diagnostics.rs`). If the tests outside the lock genuinely never reach the \
         resource it guards, add the module to `ALLOWLIST` in opt_in_serializer_guard.rs with \
         that reason, where the nightly interleave census can check it. \
         Plan `2026-09-17-runner-tests-share-in-process-mutable-state`, Phase 5; dossier \
         `runner-tests-share-in-process-mutable-state`.",
        violations.len(),
        violations.join("\n  ")
    );
}

/// The guard must be SEEN to fail: a module with a fn-local serialiser two
/// tests take and one does not is named, with the module, the serialiser and
/// the missing test; a module-level static works the same; every excluded
/// shape is enumerated with its kind and asserted about nothing.
#[test]
fn the_guard_names_the_module_the_serializer_and_the_tests_that_do_not_take_it() {
    const SRC: &str = r#"
static PRODUCTION_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
pub(crate) fn crate_wide_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
static FILE_LEVEL_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    fn series_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(())).lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn takes_it_directly() {
        let _serialised = series_lock();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn takes_it_through_a_helper() {
        with_series(|| {});
    }

    fn with_series(f: impl FnOnce()) {
        let _g = series_lock();
        f();
    }

    #[test]
    fn takes_it_inside_a_macro() {
        assert!({
            let _g = series_lock();
            true
        });
    }

    #[test]
    fn forgets_it() {
        assert_eq!(1, 1);
    }

    mod nested {
        #[test]
        fn forgets_it_in_a_nested_module() {}
    }
}

#[cfg(test)]
mod static_tests {
    use once_cell::sync::Lazy;
    use std::sync::Mutex as StdMutex;

    static SERIAL: Lazy<StdMutex<()>> = Lazy::new(|| StdMutex::new(()));

    #[test]
    fn locks_the_static() {
        let _g = SERIAL.lock().unwrap();
    }
    #[test]
    fn forgets_the_static() {}
}

#[cfg(test)]
mod tokio_tests {
    static PG_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    #[tokio::test]
    async fn awaits_the_lock() {
        let _g = PG_TEST_LOCK.lock().await;
    }
    #[tokio::test]
    async fn forgets_the_tokio_lock() {}
}

#[cfg(test)]
mod delegating_tests {
    fn health_lock() -> std::sync::MutexGuard<'static, ()> {
        super::crate_wide_lock()
    }
    #[test]
    fn takes_the_delegate() {
        let _g = health_lock();
    }
    #[test]
    fn takes_the_delegates_target_directly() {
        let _g = super::crate_wide_lock();
    }
    #[test]
    fn forgets_the_delegate() {}
}

#[cfg(test)]
mod accessor_tests {
    static STORE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    fn store_lock() -> std::sync::MutexGuard<'static, ()> {
        STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }
    struct Handle(std::sync::MutexGuard<'static, ()>);
    impl Handle {
        fn set() -> Self {
            Handle(STORE_LOCK.lock().unwrap())
        }
    }
    #[test]
    fn takes_the_accessor() {
        let _g = store_lock();
    }
    #[test]
    fn takes_the_static() {
        let _g = STORE_LOCK.lock().unwrap();
    }
    #[test]
    fn takes_it_through_a_handle() {
        let _h = Handle::set();
    }
    #[test]
    fn forgets_the_accessor() {}
}

#[cfg(test)]
mod complete_tests {
    fn lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap()
    }
    #[test]
    fn one() { let _g = lock(); }
    #[test]
    fn two() { let _g = lock(); }
}

#[cfg(test)]
mod shared_tests {
    pub(super) fn shared_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap()
    }
    #[test]
    fn forgets_the_shared_lock() {}
}

#[cfg(test)]
mod internal_tests {
    fn capture_once(f: &impl Fn()) -> String {
        static CAPTURE_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _g = CAPTURE_SERIAL.lock().unwrap();
        f();
        String::new()
    }
    #[test]
    fn uses_the_helper() { capture_once(&|| {}); }
    #[test]
    fn does_not() {}
}

#[cfg(test)]
mod one_test_tests {
    #[test]
    fn serialises_against_nothing() {
        static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _g = SERIAL.lock().unwrap();
    }
    #[test]
    fn a_sibling() {}
}

#[cfg(test)]
mod data_lock_tests {
    static TABLE: std::sync::Mutex<Vec<u8>> = std::sync::Mutex::new(Vec::new());
    fn table() -> std::sync::MutexGuard<'static, Vec<u8>> { TABLE.lock().unwrap() }
    #[test]
    fn not_a_serialiser() { table().push(1); }
}
"#;
    let report = scan_source(SRC, "fixture.rs").expect("synthetic source parses");

    let kinds: BTreeMap<String, (Kind, String)> = report
        .serializers
        .iter()
        .map(|s| {
            (
                format!("{}::{}", s.module, s.label),
                (s.kind, s.why.clone().unwrap_or_default()),
            )
        })
        .collect();
    let expect = [
        ("::crate_wide_lock", Kind::CrossModule),
        ("::FILE_LEVEL_LOCK", Kind::CrossModule),
        ("tests::series_lock", Kind::FnLocal),
        ("static_tests::SERIAL", Kind::ModuleStatic),
        ("tokio_tests::PG_TEST_LOCK", Kind::ModuleStatic),
        ("delegating_tests::health_lock", Kind::Delegating),
        ("accessor_tests::STORE_LOCK", Kind::ModuleStatic),
        ("complete_tests::lock", Kind::FnLocal),
        ("shared_tests::shared_lock", Kind::CrossModule),
        ("one_test_tests::SERIAL", Kind::ScopedToOneTest),
    ];
    for (label, kind) in expect {
        let got = kinds
            .get(label)
            .unwrap_or_else(|| panic!("serialiser `{label}` was not enumerated; got {kinds:?}"));
        assert_eq!(
            got.0, kind,
            "serialiser `{label}` has the wrong kind ({})",
            got.1
        );
    }
    assert_eq!(
        report.serializers.len(),
        expect.len(),
        "exactly these serialisers and no others — `PRODUCTION_LOCK` (not test-gated), \
         `CAPTURE_SERIAL` (internal to a helper that returns no guard), `TABLE` (a data \
         lock) and `accessor_tests::store_lock` (an accessor of STORE_LOCK, not a second \
         serialiser) must not appear: {:?}",
        report
            .serializers
            .iter()
            .map(|s| format!("{}::{}", s.module, s.label))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        kinds["shared_tests::shared_lock"].1, "accessor is pub(super)",
        "the cross-module reason names the visibility"
    );

    let by_label: BTreeMap<String, &Finding> = report
        .findings
        .iter()
        .map(|f| {
            (
                format!("{}::{}", f.serializer.module, f.serializer.label),
                f,
            )
        })
        .collect();
    let missing_of = |label: &str| -> Vec<&str> {
        by_label
            .get(label)
            .unwrap_or_else(|| panic!("no finding for `{label}`; findings: {:?}", by_label.keys()))
            .missing
            .iter()
            .map(|m| m.name.as_str())
            .collect()
    };
    assert_eq!(
        missing_of("tests::series_lock"),
        ["forgets_it", "forgets_it_in_a_nested_module"],
        "direct, helper and macro takers are credited; the two forgetters (one in a nested \
         module) are named"
    );
    assert_eq!(by_label["tests::series_lock"].population, 5);
    assert_eq!(missing_of("static_tests::SERIAL"), ["forgets_the_static"]);
    assert_eq!(
        missing_of("tokio_tests::PG_TEST_LOCK"),
        ["forgets_the_tokio_lock"]
    );
    assert_eq!(
        missing_of("delegating_tests::health_lock"),
        ["forgets_the_delegate"],
        "a test taking the lock the accessor delegates to (crate_wide_lock) directly is credited"
    );
    assert_eq!(by_label["delegating_tests::health_lock"].population, 3);
    assert_eq!(
        missing_of("accessor_tests::STORE_LOCK"),
        ["forgets_the_accessor"],
        "the static, its accessor fn and an RAII handle's path call all credit a test"
    );
    assert_eq!(
        by_label["one_test_tests::SERIAL"].population, 0,
        "a scoped-to-one-test finding carries no population — its kind is the finding"
    );
    assert!(
        !by_label.contains_key("complete_tests::lock"),
        "a module whose every test takes its serialiser is not a finding"
    );
    assert!(
        !by_label.contains_key("shared_tests::shared_lock")
            && !by_label.contains_key("::crate_wide_lock"),
        "cross-module serialisers are enumerated, never findings"
    );
    assert_eq!(
        report.findings.len(),
        6,
        "exactly the six findings above: {:?}",
        by_label.keys().collect::<Vec<_>>()
    );

    // The message is actionable: file, line, module, serialiser, the missing
    // tests with their lines — and the line is the fn's own.
    let text = render_finding("fixture.rs", by_label["tests::series_lock"]);
    let forgets_line = SRC
        .lines()
        .position(|l| l.contains("fn forgets_it()"))
        .expect("fixture line")
        + 1;
    for needle in [
        "src/fixture.rs:",
        "module `tests`",
        "serialiser `series_lock`",
        "fn-local",
        "2 of 5 test(s)",
        &format!("src/fixture.rs:{forgets_line} forgets_it"),
        "forgets_it_in_a_nested_module",
    ] {
        assert!(
            text.contains(needle),
            "finding text lacks {needle:?}:\n{text}"
        );
    }
    let scoped = render_finding("fixture.rs", by_label["one_test_tests::SERIAL"]);
    assert!(
        scoped.contains("scoped-to-one-test") && scoped.contains("serialises_against_nothing"),
        "{scoped}"
    );
}

/// A test file's root is a test module: a `tests.rs` split out of its parent
/// (`spec_api/tests.rs`) defines its serialisers at file level, and they are
/// module-local, not cross-module.
#[test]
fn a_test_files_root_counts_as_a_test_module() {
    const SRC: &str = r#"
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
#[test]
fn takes() { let _g = SERIAL.lock().unwrap(); }
#[test]
fn forgets() {}
"#;
    let report = scan_source(SRC, "spec_api/tests.rs").expect("parses");
    assert_eq!(report.serializers.len(), 1);
    assert_eq!(report.serializers[0].kind, Kind::ModuleStatic);
    assert_eq!(report.findings.len(), 1);
    assert_eq!(report.findings[0].missing[0].name, "forgets");
    assert_eq!(report.findings[0].population, 2);

    let as_ordinary = scan_source(SRC, "spec_api/mod.rs").expect("parses");
    assert!(
        as_ordinary.serializers.is_empty(),
        "the same static in a non-test file is production, not a serialiser"
    );
    assert!(
        is_test_file("foo/bar_tests.rs")
            && is_test_file("tests/x.rs")
            && !is_test_file("foo/tests_helper.rs")
    );
}

/// A file that does not parse is an error the real-tree test turns into a
/// named failure — never a silent skip.
#[test]
fn the_guard_refuses_source_it_cannot_parse() {
    assert!(scan_source(
        "#[cfg(test)] mod tests { static LOCK: Mutex<()> = ; }",
        "x.rs"
    )
    .is_err());
}
