# Vendored `tao` 0.35.0 — qontinui patch

This directory is the crates.io `tao` 0.35.0 source
(`checksum 1cf65722394c2ac443e80120064987f8914ee1d4e4e36e63cdf10f2990f01159`),
**byte-identical except for the three changes described below**, wired in via
`[patch.crates-io]` in the workspace `Cargo.toml`.

## The change

`EventLoopRunnerShared<T>` becomes an `Arc` instead of an `Rc`.

```diff
--- src/platform_impl/windows/event_loop/runner.rs
-  rc::Rc,
+  sync::Arc,

-pub(crate) type EventLoopRunnerShared<T> = Rc<EventLoopRunner<T>>;
+pub(crate) type EventLoopRunnerShared<T> = Arc<EventLoopRunner<T>>;

--- src/platform_impl/windows/event_loop.rs
-  rc::Rc,
-    let runner_shared = Rc::new(EventLoopRunner::new(thread_msg_target, wait_thread_id));
+    let runner_shared = Arc::new(EventLoopRunner::new(thread_msg_target, wait_thread_id));
```

### Linux (added 2026-10-01)

`EventLoopWindowTarget::windows` becomes an `Arc` instead of an `Rc`.

```diff
--- src/platform_impl/linux/event_loop.rs
-  sync::atomic::{AtomicBool, Ordering},
+  sync::{
+    atomic::{AtomicBool, Ordering},
+    Arc,
+  },

-  pub(crate) windows: Rc<RefCell<HashSet<WindowId>>>,
+  pub(crate) windows: Arc<RefCell<HashSet<WindowId>>>,

-      windows: Rc::new(RefCell::new(HashSet::new())),
+      windows: Arc::new(RefCell::new(HashSet::new())),
```

Same bug, Linux field: the Linux `EventLoopWindowTarget` is `#[derive(Clone)]`
and that `Rc` is its only non-atomic refcount (`gdk::Display` /
`gtk::Application` clones are atomic `g_object_ref`s, `glib::Sender` and
`crossbeam_channel::Sender` are `Arc`-backed). Every off-main-thread
`AppHandle` / `Webview` clone or drop reaches it through the same
`DispatcherMainThreadContext` `unsafe impl Send + Sync`. A lost update frees
the `RcBox` while it is still referenced, and glibc later aborts the process
on the corrupted free lists: `malloc(): unaligned tcache chunk detected`,
`corrupted double-linked list`, `malloc(): unsorted double linked list
corrupted`. That killed the Linux primary 12 times between 2026-09-13 and
2026-10-01 (counted in qontinui-dev-notes `investigation-results/2026-10-01-runner-heap-corruption-aborts.md`) while only the Windows half
was patched. As on Windows, only the REFCOUNT becomes atomic; the `RefCell` is
still only borrowed on the main thread (`platform_impl/linux/window.rs`, in
window construction).

### Windows `EventLoopThreadExecutor::execute_in_thread` (qontinui-runner `d7e1ede77`, 2026-08-30)

Not a refcount change, and not recorded here until 2026-10-06 — so a re-vendor
that re-applied only the two `Rc` -> `Arc` deltas above would silently drop it.
In `src/platform_impl/windows/event_loop.rs`, `execute_in_thread`:

- reclaims the double-boxed closure (`drop(Box::from_raw(raw))`) on EVERY
  `PostMessageW` failure instead of leaking it;
- treats `ERROR_INVALID_WINDOW_HANDLE` (`E_INVALID_WINDOW_HANDLE`) as a benign
  race with window teardown — the target is remembered in
  `DEAD_TARGET_WINDOWS` and logged once per window — instead of the upstream
  `assert!(res.is_ok(), "PostMessage failed ; is the messages queue full?")`
  panic; any other failure still panics.

Read `git show d7e1ede77 -- vendor/tao-0.35.0/src/platform_impl/windows/event_loop.rs`
for the exact hunk before re-vendoring.

That is the entire source delta. Verify the `src/` tree is identical to the
crates.io release except for those three files:

```sh
diff -rq ~/.cargo/registry/src/index.crates.io-*/tao-0.35.0/src vendor/tao-0.35.0/src
# expected: exactly
#   .../event_loop/runner.rs  and  vendor/.../event_loop/runner.rs differ
#   .../event_loop.rs         and  vendor/.../event_loop.rs        differ
#   .../linux/event_loop.rs   and  vendor/.../linux/event_loop.rs  differ
```

Only cargo-vendor bookkeeping files differ outside `src/`: this copy drops the
`.cargo-ok` / `Cargo.toml.orig` markers cargo writes locally, and the registry
copy carries a `.cargo_vcs_info.json` this one does not. None are crate source.

## Why

`EventLoopRunnerShared` lives inside `EventLoopWindowTarget`, which is held by
`tauri_runtime_wry::DispatcherMainThreadContext` — and that type carries:

```rust
// SAFETY: we ensure this type is only used on the main thread.
unsafe impl<T: UserEvent> Send for DispatcherMainThreadContext<T> {}
unsafe impl<T: UserEvent> Sync for DispatcherMainThreadContext<T> {}
```

The safety comment holds for *dereferencing* the pointee, but not for the
refcount. Cloning a `tauri::AppHandle` or `Webview` on a background thread
transitively clones this `Rc`, so its **non-atomic** count is incremented
concurrently with the main thread. That is a data race, and it corrupts.

One bug, three different Windows exceptions:

| Exception | Mechanism |
|---|---|
| `0xc000001d` ILLEGAL_INSTRUCTION | count wrapped past `usize::MAX`; `Rc::inc_strong`'s overflow `abort()` compiles to `ud2` |
| `0xc0000409` STATUS_STACK_BUFFER_OVERRUN | count read as 0; `alloc/src/rc.rs` `hint::assert_unchecked` UB check → non-unwinding `__fastfail` |
| `0xc0000374` STATUS_HEAP_CORRUPTION | premature free of a still-referenced runner |

This killed the primary runner ~14 times between 2026-07-07 and 2026-07-21,
always on a `tokio-rt-worker` thread, at one fault offset.

## How it was diagnosed

Note for anyone re-reading old crash reports: `[profile.dev] debug = 0` in the
workspace `Cargo.toml` strips line tables, so the `backtrace` crate falls back
to the PE **export table** and resolves every Rust frame to the nearest
exported C symbol. That is where the phantom
`aws_lc_..._jent_entropy_switch_notime_impl` frame in 100% of crash reports
comes from — it is an artifact, not a culprit, and it caused a ten-day
misdiagnosis. The evidence that actually identified this bug:

1. Disassembly at the faulting RIP decoded to
   `add rax,1 / mov [rcx],rax / cmp rax,0 / jne +2 / ud2` — a **non-atomic**
   increment, i.e. `Rc::inc_strong`, not `Arc`.
2. The call chain was walked inside the crashed image itself, reaching
   `Rc::clone` → `inc_strong`, with `ud2` at `+0x48`.
3. The caller was identified by a byte signature unique across the whole
   image as `tao::EventLoopWindowTarget::clone`.

## Upstream

<https://github.com/tauri-apps/tauri/issues/15408> — open, and unfixed in
every released version. tauri PR #14805 (unreleased 2.12.0) moves the
`unsafe impl` up to `Context` but does **not** remove this `Rc` clone race.

`src-tauri/tests/tao_arc_patch_active.rs` fails the build if this patch ever
stops applying. On a tauri upgrade, re-vendor the new tao version with the same
change rather than deleting that test.
