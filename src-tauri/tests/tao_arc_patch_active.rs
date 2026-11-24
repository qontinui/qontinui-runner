//! Guards the `[patch.crates-io] tao` override in the workspace `Cargo.toml`.
//!
//! `vendor/tao-0.35.0` is a copy of the crates.io tao 0.35.0 source whose
//! refcount changes these tests pin (the full delta, including a non-refcount
//! Windows fix, is in `vendor/tao-0.35.0/QONTINUI-PATCH.md`). On Windows,
//! `EventLoopRunnerShared<T>` is an `Arc`, not an `Rc`; on Linux, so is
//! `EventLoopWindowTarget::windows`. The Windows
//! refcount is mutated from background threads (every off-main-thread
//! `AppHandle::clone()` reaches it through `tauri_runtime_wry`'s
//! `unsafe impl Send + Sync` on `DispatcherMainThreadContext`), so a
//! non-atomic count races and corrupts — killing the runner with
//! `0xc000001d` / `0xc0000409` / `0xc0000374`. See the comment on the
//! `[patch.crates-io]` section for the full forensics.
//!
//! Why this test exists: a `[patch]` that does not apply is **not an error**.
//! Cargo emits at most a warning and silently uses the registry crate. So if a
//! tauri bump moves the tree to a tao version this patch does not match, the
//! crash returns with no build failure and no signal — exactly the kind of
//! silent regression that cost ten days of misdiagnosis the first time.
//! These assertions turn that into a red test.

use std::path::{Path, PathBuf};

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is <workspace>/src-tauri for this package.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("src-tauri always has a parent (the workspace root)")
        .to_path_buf()
}

/// The lockfile must resolve `tao` to our local path patch, not to the
/// registry. A patched path dependency has no `source` key in `Cargo.lock`.
#[test]
fn tao_resolves_to_the_vendored_patch() {
    let lock_path = workspace_root().join("Cargo.lock");
    let lock = std::fs::read_to_string(&lock_path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", lock_path.display()));

    let entry = lock
        .split("[[package]]")
        .find(|block| block.contains("\nname = \"tao\"\n"))
        .unwrap_or_else(|| {
            panic!(
                "no `tao` package in {} — did the dependency go away?",
                lock_path.display()
            )
        });

    assert!(
        !entry.contains("source = \"registry+"),
        "`tao` resolved to the crates.io registry, so the [patch.crates-io] override in the \
         workspace Cargo.toml is NOT applying. The non-atomic `Rc` refcount race is back and \
         will crash the runner intermittently with 0xc000001d / 0xc0000409 / 0xc0000374. \
         Most likely cause: a tauri upgrade moved the tree to a tao version other than the \
         vendored 0.35.0. Fix by re-vendoring the new tao version with the same Rc->Arc change \
         (see vendor/tao-0.35.0/QONTINUI-PATCH.md), not by deleting this test.\n\
         Lockfile entry was:\n{entry}"
    );

    assert!(
        entry.contains("version = \"0.35.0\""),
        "`tao` is no longer at 0.35.0; the vendored patch is pinned to that version. \
         Re-vendor against the new version before updating this assertion.\n\
         Lockfile entry was:\n{entry}"
    );
}

/// The vendored source must still carry the change. Guards against someone
/// refreshing `vendor/` from upstream and silently dropping the patch.
#[test]
fn vendored_tao_still_uses_an_atomic_refcount() {
    let runner_rs =
        workspace_root().join("vendor/tao-0.35.0/src/platform_impl/windows/event_loop/runner.rs");
    let src = std::fs::read_to_string(&runner_rs)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", runner_rs.display()));

    assert!(
        src.contains("pub(crate) type EventLoopRunnerShared<T> = Arc<EventLoopRunner<T>>;"),
        "vendor/tao-0.35.0 no longer declares `EventLoopRunnerShared` as an `Arc`. The vendored \
         copy has been reverted or re-synced from upstream, which reintroduces the data race. \
         Re-apply the Rc->Arc change (see vendor/tao-0.35.0/QONTINUI-PATCH.md)."
    );
}

/// The LINUX half of the same race. On Linux the shared refcount that every
/// off-main-thread `AppHandle` / `Webview` clone (and drop) reaches is
/// `EventLoopWindowTarget::windows` — `#[derive(Clone)]` on a struct that
/// `tauri_runtime_wry::DispatcherMainThreadContext` declares `Send + Sync`.
/// Upstream it is an `Rc`, so a background-thread clone/drop races the main
/// thread's, the count drifts, and the `RcBox` is freed while still
/// referenced. On the Linux primary that surfaced as glibc heap-corruption
/// aborts (`malloc(): unaligned tcache chunk detected`, `corrupted
/// double-linked list`) — 12 SIGABRTs between 2026-09-13 and 2026-10-01.
/// Plan `2026-10-01-linux-runner-heap-corruption-tao-windows-rc-race`.
#[test]
fn vendored_tao_linux_window_set_uses_an_atomic_refcount() {
    let event_loop_rs =
        workspace_root().join("vendor/tao-0.35.0/src/platform_impl/linux/event_loop.rs");
    let src = std::fs::read_to_string(&event_loop_rs)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", event_loop_rs.display()));

    assert!(
        src.contains("pub(crate) windows: Arc<RefCell<HashSet<WindowId>>>,")
            && src.contains("windows: Arc::new(RefCell::new(HashSet::new())),"),
        "vendor/tao-0.35.0's Linux `EventLoopWindowTarget::windows` is no longer an `Arc`. \
         It is cloned and dropped from tokio threads through tauri's `unsafe impl Send + Sync` \
         on `DispatcherMainThreadContext`, so an `Rc` there is a non-atomic refcount race that \
         corrupts the glibc heap. Re-apply the Rc->Arc change \
         (see vendor/tao-0.35.0/QONTINUI-PATCH.md)."
    );
    assert!(
        !src.contains("Rc<RefCell<HashSet<WindowId>>>"),
        "an `Rc<RefCell<HashSet<WindowId>>>` is back in the Linux tao event loop — that is the \
         heap-corrupting refcount race (see vendor/tao-0.35.0/QONTINUI-PATCH.md)."
    );
}
