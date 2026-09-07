//! The ONE seam through which this crate reads **ambient machine state** —
//! `~/.qontinui/` and the process environment that decides where `~/.qontinui/`
//! is.
//!
//! # The problem this exists for
//!
//! Plan `2026-09-03-runner-tests-read-ambient-machine-state`. A handful of code
//! paths called `dirs::home_dir()` directly and read a real file under it. A
//! test exercising such a path passes on a clean CI runner and fails on a
//! configured developer box — and it fails as an ordinary assertion mismatch,
//! so it reads as *"my diff broke this"* rather than *"this test reached
//! outside its fixture"*.
//!
//! CI on qontinui/qontinui-runner#1325 measured that precisely: the full suite
//! run under a deliberately poisoned `$HOME` on ubuntu-22.04 and
//! windows-latest reddened exactly ONE test on each leg —
//! `session::tests::tenant_scope_of_distinguishes_owned_from_both_unknowns`,
//! with `left: Owned(6c0a78b7-…) right: Unresolved`. It had read the poisoned
//! `machine.json::active_tenant_id`.
//!
//! # What this module provides
//!
//! 1. **One reader.** [`qontinui_dir`], [`machine_json_path`] and
//!    [`read_machine_json`] are the supported way to reach that directory, so
//!    there is exactly one place to point at a fixture.
//! 2. **One env surface.** [`AMBIENT_ENV_KEYS`] names every process variable
//!    that can change where an ambient read lands, so a fixture can capture and
//!    restore the whole surface instead of a hand-maintained subset that drifts.
//! 3. **A canary.** In a test process, an ambient read taken with no live
//!    [`test_support::IsolatedAmbient`] guard **panics, naming the concrete
//!    ambient source it was about to read**. That is the deliverable: the
//!    failure mode stops presenting as an assertion mismatch and starts saying
//!    what it actually was.
//!
//! # The canary's reach — a runtime property, not a `cfg`
//!
//! It has to cover BOTH crate roots. `cfg(test)` cannot: it is set only while
//! compiling a crate's own test binary, and the runner *binary* crate
//! (`main.rs`) links this rlib compiled WITHOUT it — yet `main.rs`'s module
//! tree is exactly where the one test this plan exists for lives. Widening to
//! `debug_assertions` alone is worse: that is on in an ordinary dev build of
//! the runner, where no fixture is ever live, so the canary would panic on the
//! first real read and brick dev runs.
//!
//! So the *code* is compiled under `any(test, debug_assertions)` and the
//! *decision to evaluate* is made at runtime by
//! [`test_support::canary_armed`] — `cfg!(test)`, an explicit
//! [`test_support::arm_canary`], or "this executable is a cargo test binary in
//! `<target>/<profile>/deps/`". Read that function's docs for why the
//! heuristic's failure modes are asymmetric on purpose.
//!
//! # Release builds carry none of this
//!
//! Both [`test_support`] and the canary body are gated on
//! `any(test, debug_assertions)`, so a release build compiles neither — same
//! discipline as the `mcp::test_fixtures` seam the `seam-gate` CI job guards.

use std::path::PathBuf;

/// The file under [`qontinui_dir`] that records this machine's identity and its
/// operator-stated tenant. Named once so the two readers cannot drift.
pub const MACHINE_JSON: &str = "machine.json";

/// Every process environment variable that can change what an ambient read
/// answers — the capture list a fixture must restore to be hermetic.
///
/// It is a SUPERSET of [`crate::profiles::COORD_BASE_ENV_KEYS`] (the lib's own
/// declaration of the coord-base surface); `ambient_env_keys_cover_coord_base`
/// below asserts that containment so the two cannot drift apart silently.
///
/// The additions beyond that set:
///
/// - `QONTINUI_HOME` — the explicit override honored by [`qontinui_dir`].
/// - `HOME` / `USERPROFILE` — what `dirs::home_dir()` reads when the override
///   is absent, on unix and Windows respectively.
/// - `QONTINUI_ROOT` / `QONTINUI_WORKSPACE_ROOT` — workspace discovery.
/// - `QONTINUI_PLANS_DIR` — the plan corpus authoring directory.
/// - `QONTINUI_DISABLE_KEYCHAIN` — flips the credential store to a file
///   backend; a headless Linux box exports it and a test that inherits it
///   exercises a different code path than CI does.
/// - `DATABASE_URL` — set on a developer box and in DB-gated CI, unset
///   elsewhere.
pub const AMBIENT_ENV_KEYS: &[&str] = &[
    // --- profiles::COORD_BASE_ENV_KEYS, mirrored (see the assertion test) ---
    "COORD_HTTP_URL",
    "QONTINUI_ENV",
    "QONTINUI_CONFIG_DIR",
    "QONTINUI_SECURE_STORAGE_DIR",
    "QONTINUI_SERVER_MODE",
    "QONTINUI_RUNNER_TOKEN",
    "QONTINUI_RUNNER_TIER",
    // --- the home / workspace surface ---
    "QONTINUI_HOME",
    "HOME",
    "USERPROFILE",
    "QONTINUI_ROOT",
    "QONTINUI_WORKSPACE_ROOT",
    "QONTINUI_PLANS_DIR",
    "QONTINUI_DISABLE_KEYCHAIN",
    "DATABASE_URL",
];

/// `~/.qontinui/machine.json` as this codebase reads it — ONE serde struct, so
/// the several parsers that grew independently have a single type to converge
/// on.
///
/// `active_tenant_id` is deliberately left as a raw [`serde_json::Value`]:
/// [`crate::tenant_pin`] draws a three-way distinction from it that a
/// `Option<Uuid>` would erase — an **absent** field is a legitimate
/// single-tenant install (`Unpinned`), while a **present but malformed** value
/// is a machine that tried to state its tenant and produced garbage
/// (`Unresolvable`).
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
pub struct MachineJson {
    /// This machine's coord device id, when the document carries one.
    pub device_id: Option<String>,
    /// Raw and UNVALIDATED. `None` = the key is absent; `Some(Value::Null)` =
    /// present and explicitly null; anything else is the stated value.
    pub active_tenant_id: Option<serde_json::Value>,
    /// Every other key, so a caller needing a field this struct does not name
    /// can reach it without introducing a second parser.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
    /// NOT from the document: `false` when the file could not be read at all,
    /// or was not valid JSON.
    ///
    /// This is the distinction an all-`None` struct cannot carry, and
    /// [`crate::tenant_pin`] needs it: an unreadable file is `Unresolvable`
    /// while a readable one missing the field is `Unpinned`.
    #[serde(skip)]
    pub readable: bool,
}

/// The `~/.qontinui` directory: `$QONTINUI_HOME` when set and non-empty,
/// otherwise `dirs::home_dir()?/.qontinui`.
///
/// `None` when there is no home directory to derive one from — the same
/// "cannot state anything" outcome callers already handled.
pub fn qontinui_dir() -> Option<PathBuf> {
    canary("~/.qontinui");
    qontinui_dir_unchecked()
}

/// The path of [`MACHINE_JSON`] under [`qontinui_dir`].
pub fn machine_json_path() -> Option<PathBuf> {
    canary("~/.qontinui/machine.json");
    qontinui_dir_unchecked().map(|d| d.join(MACHINE_JSON))
}

/// Read and parse [`MACHINE_JSON`].
///
/// Total: every failure (no home dir, missing file, unreadable file,
/// unparseable JSON) yields a [`MachineJson::default`] with `readable: false`
/// rather than an error — the callers all had to fold those cases anyway, and
/// folding them once here is the point of the seam.
pub fn read_machine_json() -> MachineJson {
    canary("~/.qontinui/machine.json");
    let Some(path) = qontinui_dir_unchecked().map(|d| d.join(MACHINE_JSON)) else {
        return MachineJson::default();
    };
    let Ok(bytes) = std::fs::read(path) else {
        return MachineJson::default();
    };
    parse_machine_json(&bytes)
}

/// The parse half of [`read_machine_json`], split out so every outcome is
/// reachable from a test without touching the filesystem or `$HOME`.
pub fn parse_machine_json(bytes: &[u8]) -> MachineJson {
    match serde_json::from_slice::<MachineJson>(bytes) {
        Ok(mut doc) => {
            doc.readable = true;
            doc
        }
        Err(_) => MachineJson::default(),
    }
}

/// [`qontinui_dir`] without the canary — the internal spelling the canaried
/// entry points and the fixture itself use, so a read that has ALREADY reported
/// its concrete source does not re-report a vaguer one.
fn qontinui_dir_unchecked() -> Option<PathBuf> {
    match std::env::var_os("QONTINUI_HOME") {
        Some(v) if !v.is_empty() => Some(PathBuf::from(v)),
        _ => dirs::home_dir().map(|home| home.join(".qontinui")),
    }
}

// ============================================================================
// The canary
// ============================================================================

/// Fail an unguarded ambient read, naming the source it was about to take.
///
/// Three allow arms, in order:
///
/// 1. this process is not a test harness at all — see
///    [`test_support::canary_armed`]. A shipped or dev runner reads ambient
///    state because that is its job;
/// 2. a live [`test_support::IsolatedAmbient`] on THIS thread — the precise
///    signal;
/// 3. else any live guard anywhere in the process. Deliberately soft: a test
///    that hands work to a helper thread or a tokio worker still reads through
///    its own fixture, and this arm guarantees the canary produces no false
///    positives at the cost of missing a genuinely unguarded read that happens
///    to overlap a guarded test. A canary that cries wolf gets deleted.
#[cfg(any(test, debug_assertions))]
fn canary(source: &str) {
    if !test_support::canary_armed() {
        return;
    }
    if test_support::thread_is_guarded() || test_support::live_guard_count() > 0 {
        return;
    }
    let home = match std::env::var_os("QONTINUI_HOME") {
        Some(v) if !v.is_empty() => format!("QONTINUI_HOME={}", v.to_string_lossy()),
        _ => "QONTINUI_HOME unset".to_string(),
    };
    panic!(
        "ambient read of {source} ({home}) from a test with no isolated_ambient() guard \
         — see plan 2026-09-03-runner-tests-read-ambient-machine-state"
    );
}

/// Release builds carry no canary at all — `debug_assertions` is off there, and
/// `test_support` (which holds the state this reads) is not compiled either.
#[cfg(not(any(test, debug_assertions)))]
#[inline(always)]
fn canary(_source: &str) {}

// ============================================================================
// The fixture
// ============================================================================

/// The isolation fixture, plus the ONE process-wide env lock this rlib owns.
///
/// Gated on `any(test, debug_assertions)` rather than `cfg(test)` so the runner
/// *binary* crate's tests can use it too: `cargo test` builds the bin's
/// dependencies (this rlib included) without `cfg(test)` but WITH
/// `debug_assertions`. A release build has neither and compiles none of this.
#[cfg(any(test, debug_assertions))]
pub mod test_support {
    use std::cell::Cell;
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Mutex, MutexGuard};

    /// A single process-wide lock that serializes every test which reads or
    /// mutates a `std::env` variable.
    ///
    /// `std::env` is process-global, so two tests touching the same var in
    /// parallel race — one clobbers the value mid-read, the code-under-test
    /// sees the wrong value, and CI reddens non-deterministically (the flake
    /// class fixed 2026-07-11; cf. `qontinui_shim::resolve_real_in`).
    ///
    /// It lives HERE, in the rlib, rather than once per crate root, because the
    /// runner-bin test binary links this rlib: a lock defined in `main.rs` and
    /// a lock defined in `lib.rs` are two different statics in that one
    /// process, so a bin test holding one would not exclude an
    /// [`IsolatedAmbient`] holding the other. `lib.rs::test_env` and
    /// `main.rs::test_env` both re-export this one.
    ///
    /// Poison-recovering so a panicking test can't cascade-fail the rest.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    thread_local! {
        /// How many [`EnvLockGuard`]s this thread currently holds. See
        /// [`env_lock`].
        static ENV_LOCK_DEPTH: Cell<usize> = const { Cell::new(0) };
    }

    /// The guard [`env_lock`] returns. Opaque on purpose — see that function
    /// for why it is not a bare `MutexGuard`.
    pub struct EnvLockGuard {
        /// `Some` only for the OUTERMOST acquisition on this thread; a nested
        /// one holds nothing and so releases nothing when it drops.
        _inner: Option<MutexGuard<'static, ()>>,
    }

    impl Drop for EnvLockGuard {
        fn drop(&mut self) {
            ENV_LOCK_DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
            // `_inner` drops after this, releasing the mutex at depth 0.
        }
    }

    /// Acquire the shared env lock. Hold the returned guard for the whole body
    /// of any test that touches `std::env`.
    ///
    /// **Reentrant per thread.** A plain `std::sync::Mutex` is not, and that
    /// mattered the moment [`IsolatedAmbient`] started taking this same lock:
    /// ~115 runner-bin tests reach ambient machine state, a dozen of them from
    /// bodies that already hold `env_lock()`, and a non-reentrant lock would
    /// have turned each of those into a silent hang rather than a failure.
    /// Nesting is counted per thread and the mutex is released only when the
    /// outermost guard drops; RAII gives the LIFO drop order that requires.
    ///
    /// Cross-thread exclusion is unchanged — that is the property the lock
    /// exists for.
    pub fn env_lock() -> EnvLockGuard {
        let depth = ENV_LOCK_DEPTH.with(|d| d.get());
        let inner = if depth == 0 {
            Some(ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner()))
        } else {
            None
        };
        ENV_LOCK_DEPTH.with(|d| d.set(depth + 1));
        EnvLockGuard { _inner: inner }
    }

    /// RAII guard that restores the captured env vars to their pre-capture
    /// values on drop (including the panic path). Use for tests that mutate a
    /// process-global var which may already be set in the environment (e.g.
    /// `DATABASE_URL` in dev / DB-gated CI) so the test can't leak its value —
    /// or its removal — to sibling tests in the same binary.
    pub struct EnvVarRestore {
        saved: Vec<(&'static str, Option<OsString>)>,
    }

    impl EnvVarRestore {
        pub fn capture(keys: &[&'static str]) -> Self {
            let saved = keys.iter().map(|&k| (k, std::env::var_os(k))).collect();
            Self { saved }
        }
    }

    impl Drop for EnvVarRestore {
        fn drop(&mut self) {
            for (k, v) in &self.saved {
                match v {
                    Some(val) => std::env::set_var(k, val),
                    None => std::env::remove_var(k),
                }
            }
        }
    }

    /// How many [`IsolatedAmbient`] guards are alive in this process.
    static LIVE_GUARDS: AtomicUsize = AtomicUsize::new(0);

    /// Explicit override for [`canary_armed`], set by [`arm_canary`].
    static EXPLICIT_ARM: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    /// Force the canary on for this process, regardless of what
    /// [`canary_armed`]'s heuristic concludes.
    ///
    /// Exists so a test binary that the heuristic cannot recognise (a custom
    /// harness, a binary copied out of `deps/`) can still arm it, and so the
    /// arming does not depend on some other test having constructed a fixture
    /// first. There is no disarm: a process that has ever declared itself a
    /// test harness stays one.
    pub fn arm_canary() {
        EXPLICIT_ARM.store(true, Ordering::SeqCst);
    }

    /// Whether the canary should evaluate at all in THIS process.
    ///
    /// # Why this is not simply the `cfg`
    ///
    /// `cfg(test)` is set only while compiling a crate's OWN test binary. When
    /// `cargo test` builds the runner *binary* crate, it links this rlib as an
    /// ordinary dependency compiled WITHOUT `cfg(test)` — so a `cfg(test)`
    /// canary is a no-op for every test defined in `main.rs`'s module tree,
    /// which is precisely where the one test this plan exists for lives
    /// (`session::tests::tenant_scope_of_distinguishes_owned_from_both_unknowns`).
    ///
    /// Widening the gate to `debug_assertions` alone would be worse: that is ON
    /// in an ordinary dev build of the runner, where no fixture is ever live,
    /// so the canary would panic on the first real `read_machine_json()` and
    /// brick dev runs. The gate has to be a *runtime* property of the process,
    /// not a compile-time property of the build.
    ///
    /// Three signals, cheapest first:
    ///
    /// 1. `cfg!(test)` — definitive for this rlib's own test binary.
    /// 2. [`arm_canary`] — an explicit declaration.
    /// 3. Otherwise: is this executable a cargo-built test binary? Cargo emits
    ///    every unit-test, integration-test and bench binary into
    ///    `<target>/<profile>/deps/`, and the runner binary is never RUN from
    ///    there — `cargo run`, `tauri dev`, the published build and the
    ///    installed build all live one directory up or somewhere else
    ///    entirely.
    ///
    /// The heuristic's failure modes are asymmetric on purpose. A false
    /// negative (a test binary the heuristic does not recognise) turns the
    /// canary off, which is exactly the pre-plan status quo and costs nothing.
    /// A false positive would panic a real runner — which requires launching
    /// the shipped binary from a directory literally named `deps`, and is
    /// additionally impossible in a release build where this whole module is
    /// `cfg`-ed out.
    ///
    /// Memoised: `current_exe()` is a syscall and `read_machine_json` is not
    /// hot, but it is called on paths that run per session.
    pub fn canary_armed() -> bool {
        if cfg!(test) || EXPLICIT_ARM.load(Ordering::SeqCst) {
            return true;
        }
        static DETECTED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *DETECTED.get_or_init(|| {
            std::env::current_exe()
                .ok()
                .and_then(|exe| exe.parent().map(|d| d.file_name() == Some("deps".as_ref())))
                .unwrap_or(false)
        })
    }

    thread_local! {
        /// Whether THIS thread currently holds an [`IsolatedAmbient`].
        static THREAD_ARMED: Cell<bool> = const { Cell::new(false) };
    }

    /// See [`LIVE_GUARDS`]. Read by the canary.
    pub fn live_guard_count() -> usize {
        LIVE_GUARDS.load(Ordering::SeqCst)
    }

    /// See [`THREAD_ARMED`]. Read by the canary.
    pub fn thread_is_guarded() -> bool {
        THREAD_ARMED.with(|c| c.get())
    }

    /// An RAII fixture that makes every ambient read in its scope land inside a
    /// throwaway directory, and arms the canary for reads taken outside one.
    ///
    /// On construction it:
    ///
    /// - takes the process-wide [`env_lock`], so no sibling test can observe or
    ///   race the environment it is about to rewrite;
    /// - captures every [`super::AMBIENT_ENV_KEYS`] value for restoration on
    ///   drop, **including the panic path**;
    /// - creates a `tempfile::tempdir()` and points `QONTINUI_HOME`, `HOME` and
    ///   `USERPROFILE` at it — so both [`super::qontinui_dir`] and
    ///   `dirs::home_dir()` resolve inside the fixture;
    /// - removes `COORD_HTTP_URL`, `QONTINUI_ROOT`, `QONTINUI_WORKSPACE_ROOT`
    ///   and `DATABASE_URL`, which a configured developer box exports and a
    ///   clean CI runner does not;
    /// - clears the process-global runtime tier override
    ///   ([`crate::profiles::set_runtime_tier_override`]), which is not an env
    ///   var and so is outside [`EnvVarRestore`]'s reach.
    ///
    /// The directory starts EMPTY: there is no `machine.json` until a test
    /// writes one with [`IsolatedAmbient::write_machine_json`]. That is the
    /// exit criterion made testable — the file becomes a fixture INPUT rather
    /// than something the box happens to have.
    pub struct IsolatedAmbient {
        // Field order is drop order, and drop order matters: restore the
        // environment and release the arming BEFORE the temp dir is deleted,
        // so nothing can resolve `QONTINUI_HOME` to a path that no longer
        // exists. `_lock` is last so it outlives every restore.
        _restore: EnvVarRestore,
        dir: tempfile::TempDir,
        prev_thread_armed: bool,
        _lock: MutexGuard<'static, ()>,
    }

    impl IsolatedAmbient {
        /// Construct the fixture. See the type docs for everything it does.
        #[allow(clippy::new_without_default)]
        pub fn new() -> Self {
            let lock = env_lock();
            let restore = EnvVarRestore::capture(super::AMBIENT_ENV_KEYS);
            let dir = tempfile::tempdir().expect("isolated ambient fixture needs a temp dir");

            let prev_thread_armed = THREAD_ARMED.with(|c| c.replace(true));
            LIVE_GUARDS.fetch_add(1, Ordering::SeqCst);

            for key in ["QONTINUI_HOME", "HOME", "USERPROFILE"] {
                std::env::set_var(key, dir.path());
            }
            for key in [
                "COORD_HTTP_URL",
                "QONTINUI_ROOT",
                "QONTINUI_WORKSPACE_ROOT",
                "DATABASE_URL",
            ] {
                std::env::remove_var(key);
            }
            crate::profiles::set_runtime_tier_override(None);

            Self {
                _restore: restore,
                dir,
                prev_thread_armed,
                _lock: lock,
            }
        }

        /// The fixture's root — the directory `QONTINUI_HOME` points at, and so
        /// the directory [`super::qontinui_dir`] answers.
        pub fn dir(&self) -> &Path {
            self.dir.path()
        }

        /// Where [`super::read_machine_json`] will look inside this fixture.
        pub fn machine_json_path(&self) -> PathBuf {
            self.dir.path().join(super::MACHINE_JSON)
        }

        /// Write a `machine.json` into the fixture, verbatim.
        pub fn write_machine_json(&self, contents: &str) -> PathBuf {
            let path = self.machine_json_path();
            std::fs::write(&path, contents).expect("fixture machine.json must be writable");
            path
        }

        /// Write a `machine.json` stating `active_tenant_id`.
        pub fn write_active_tenant_id(&self, tenant: uuid::Uuid) -> PathBuf {
            self.write_machine_json(&format!(
                "{{\"device_id\":\"fixture-device\",\"active_tenant_id\":\"{tenant}\"}}"
            ))
        }

        /// Write a `settings.json` into the fixture and point
        /// `QONTINUI_CONFIG_DIR` / `QONTINUI_SECURE_STORAGE_DIR` at it, so the
        /// tier a `profiles` read infers comes from the fixture rather than
        /// from the box.
        pub fn write_settings_json(&self, contents: &str) -> PathBuf {
            let path = self.dir.path().join("settings.json");
            std::fs::write(&path, contents).expect("fixture settings.json must be writable");
            std::env::set_var("QONTINUI_CONFIG_DIR", self.dir.path());
            std::env::set_var("QONTINUI_SECURE_STORAGE_DIR", self.dir.path());
            path
        }
    }

    impl Drop for IsolatedAmbient {
        fn drop(&mut self) {
            LIVE_GUARDS.fetch_sub(1, Ordering::SeqCst);
            let prev = self.prev_thread_armed;
            THREAD_ARMED.with(|c| c.set(prev));
            crate::profiles::set_runtime_tier_override(None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The containment that keeps [`AMBIENT_ENV_KEYS`] from drifting away from
    /// the lib's own declaration of the coord-base surface. The hand-maintained
    /// version of this list already drifted once — `QONTINUI_SERVER_MODE`
    /// became a tier signal and two of three copies never learned about it.
    #[test]
    fn ambient_env_keys_cover_coord_base() {
        for key in crate::profiles::COORD_BASE_ENV_KEYS {
            assert!(
                AMBIENT_ENV_KEYS.contains(key),
                "AMBIENT_ENV_KEYS is missing coord-base key {key}"
            );
        }
    }

    #[test]
    fn ambient_env_keys_have_no_duplicates() {
        let mut seen: Vec<&str> = AMBIENT_ENV_KEYS.to_vec();
        seen.sort_unstable();
        let before = seen.len();
        seen.dedup();
        assert_eq!(before, seen.len(), "AMBIENT_ENV_KEYS has a duplicate");
    }

    /// THE deliverable, negatively: an ambient read with no fixture does not
    /// return a value the box happens to have — it fails, naming what it read.
    ///
    /// Holds [`test_support::env_lock`] so the canary's soft process-global arm
    /// cannot be satisfied by a sibling test's guard running concurrently: an
    /// `IsolatedAmbient` holds that same lock for its whole life.
    #[test]
    #[should_panic(expected = "ambient read of")]
    fn unguarded_qontinui_dir_read_names_its_ambient_source() {
        let _lock = test_support::env_lock();
        let _ = qontinui_dir();
    }

    #[test]
    #[should_panic(expected = "ambient read of ~/.qontinui/machine.json")]
    fn unguarded_machine_json_read_names_the_file() {
        let _lock = test_support::env_lock();
        let _ = read_machine_json();
    }

    #[test]
    fn guarded_reads_land_inside_the_fixture() {
        let amb = test_support::IsolatedAmbient::new();
        assert_eq!(qontinui_dir().as_deref(), Some(amb.dir()));
        assert_eq!(machine_json_path(), Some(amb.machine_json_path()));
    }

    /// The bare case the poisoned-home CI run reddened: with a fixture, the
    /// absence of a `machine.json` is a property of the FIXTURE, not of the
    /// machine the test happens to run on.
    #[test]
    fn fixture_starts_with_no_machine_json() {
        let _amb = test_support::IsolatedAmbient::new();
        let doc = read_machine_json();
        assert!(!doc.readable, "a fresh fixture must carry no machine.json");
        assert!(doc.active_tenant_id.is_none());
        assert!(doc.device_id.is_none());
    }

    #[test]
    fn fixture_machine_json_is_an_input() {
        let amb = test_support::IsolatedAmbient::new();
        let tenant = uuid::Uuid::from_u128(0xA11CE);
        amb.write_active_tenant_id(tenant);

        let doc = read_machine_json();
        assert!(doc.readable);
        assert_eq!(doc.device_id.as_deref(), Some("fixture-device"));
        assert_eq!(
            doc.active_tenant_id.as_ref().and_then(|v| v.as_str()),
            Some(tenant.to_string().as_str())
        );
    }

    #[test]
    fn qontinui_home_override_beats_the_home_dir() {
        let amb = test_support::IsolatedAmbient::new();
        let elsewhere = amb.dir().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::env::set_var("QONTINUI_HOME", &elsewhere);
        assert_eq!(qontinui_dir().as_deref(), Some(elsewhere.as_path()));
    }

    /// An EMPTY `QONTINUI_HOME` is not an override — it is the shape a
    /// `systemd` unit's `Environment=FOO=` produces, and treating it as a path
    /// would resolve every ambient read to the process cwd.
    #[test]
    fn empty_qontinui_home_falls_through_to_the_home_dir() {
        let _amb = test_support::IsolatedAmbient::new();
        std::env::set_var("QONTINUI_HOME", "");
        // Only the LEAF is asserted, not the whole path: `dirs::home_dir()`
        // reads `$HOME` on unix but the FOLDERID_Profile known folder on
        // Windows, so the fixture's `HOME` does not steer it there. What is
        // under test is that an empty value is not treated as a path — which
        // would otherwise resolve every ambient read to the process cwd (the
        // `systemd Environment=FOO=` shape).
        let dir = qontinui_dir().expect("a home directory must resolve");
        assert_eq!(dir.file_name().and_then(|s| s.to_str()), Some(".qontinui"));
        assert!(dir.parent().is_some_and(|p| !p.as_os_str().is_empty()));
    }

    // ---- the pure parser, reachable without touching `$HOME` ----

    #[test]
    fn unparseable_machine_json_is_not_readable() {
        let doc = parse_machine_json(b"{ this is not json");
        assert!(!doc.readable);
    }

    #[test]
    fn readable_document_missing_the_field_is_distinguishable_from_an_unreadable_one() {
        let present = parse_machine_json(br#"{"device_id":"d","hostname":"msi"}"#);
        assert!(present.readable);
        assert!(present.active_tenant_id.is_none());
        assert_eq!(present.device_id.as_deref(), Some("d"));
        // The key the struct does not name is still reachable.
        assert_eq!(
            present.extra.get("hostname").and_then(|v| v.as_str()),
            Some("msi")
        );

        let absent = parse_machine_json(b"");
        assert!(!absent.readable);
    }

    #[test]
    fn explicit_null_tenant_is_present_and_null_not_absent() {
        let doc = parse_machine_json(br#"{"active_tenant_id":null}"#);
        assert!(doc.readable);
        assert_eq!(doc.active_tenant_id, Some(serde_json::Value::Null));
    }
}
