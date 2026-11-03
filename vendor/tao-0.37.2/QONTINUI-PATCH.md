# Vendored `tao` 0.37.2 — qontinui patch

This directory is the crates.io `tao` 0.37.2 source
(`checksum d5777b06fe6435126ae8d816eba269e050e1e10d302cf5461e9c49eccabc3548`),
**byte-identical except for the three changes described below**, wired in via
`[patch.crates-io]` in the workspace `Cargo.toml`.

It replaces the earlier `vendor/tao-0.35.0` (tauri 2.11 era). The tauri 2.12
upgrade (`tauri-runtime-wry ~2.12.3` requires `tao ^0.37.0`) made the old patch
stop applying, so the same three changes were re-derived against 0.37.2's code
rather than re-applied as a diff — upstream moved the surrounding code (see
change 3).

## Change 1 — Windows: `EventLoopRunnerShared<T>` becomes an `Arc`

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

Upstream 0.37.2 still has the `Rc` (`runner.rs:27`, `Rc::new` at
`event_loop.rs:196`).

## Change 2 — Linux: `EventLoopWindowTarget::windows` becomes an `Arc` (added 2026-10-01)

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

Upstream 0.37.2 still has the `Rc` (`linux/event_loop.rs:60` and `:252`). The
file still imports `rc::Rc` because 0.37 added two unrelated `Rc<Cell<u32>>`
event-time guards that live only on the main thread.

Same bug, Linux field: the Linux `EventLoopWindowTarget` is `#[derive(Clone)]`
and that `Rc` is its only non-atomic refcount (`gdk::Display` /
`gtk::Application` clones are atomic `g_object_ref`s, `glib::Sender` and
`crossbeam_channel::Sender` are `Arc`-backed). Every off-main-thread
`AppHandle` / `Webview` clone or drop reaches it through the same
`unsafe impl Send + Sync` described under "Why". A lost update frees the
`RcBox` while it is still referenced, and glibc later aborts the process on
the corrupted free lists: `malloc(): unaligned tcache chunk detected`,
`corrupted double-linked list`, `malloc(): unsorted double linked list
corrupted`. That killed the Linux primary 12 times between 2026-09-13 and
2026-10-01 (counted in qontinui-dev-notes
`investigation-results/2026-10-01-runner-heap-corruption-aborts.md`) while
only the Windows half was patched. As on Windows, only the REFCOUNT becomes
atomic; the `RefCell` is still only borrowed on the main thread
(`platform_impl/linux/window.rs`, in window construction).

## Change 3 — Windows: `EventLoopThreadExecutor::execute_in_thread` (qontinui-runner `d7e1ede77`, 2026-08-30)

Not a refcount change — so a re-vendor that re-applied only the two
`Rc` -> `Arc` deltas above would silently drop it. `tao_arc_patch_active`
now asserts it too.

What upstream did between 0.35 and 0.37: the old
`assert!(res.is_ok(), "PostMessage failed ; is the messages queue full?")`
panic became a `log::error!("PostMessage failed ; is the messages queue full?
Error code {} - {}", ...)` on every failure — but the double-boxed closure is
**still leaked** on every failed post, and the error is logged once per call
(442 calls/second were measured while a pop-out was closing under load).

What the patched `execute_in_thread` in
`src/platform_impl/windows/event_loop.rs` does instead:

- returns early, before boxing, for a target window already known dead
  (`DEAD_TARGET_WINDOWS`, a `LazyLock<parking_lot::Mutex<HashSet<isize>>>`
  keyed by raw `HWND`);
- reclaims the double-boxed closure (`drop(Box::from_raw(raw))`) on EVERY
  `PostMessageW` failure instead of leaking it;
- treats `ERROR_INVALID_WINDOW_HANDLE` (`E_INVALID_WINDOW_HANDLE`, HRESULT
  `0x80070578`) as benign event-loop teardown — the target is remembered in
  `DEAD_TARGET_WINDOWS` and a `warn!` is logged once per target;
- logs any other failure with `error!` on every occurrence, naming the window
  and saying the closure was dropped without running.

How a call gets here, and what "target window" means: `execute_in_thread`
is reached through the `EventLoopThreadExecutor` that tao's Windows `Window`
holds (`window.rs:98`, made by `create_thread_executor` at `window.rs:1132`),
whenever a `Window` setter/getter runs off the event-loop thread. It is NOT
reached via `EventLoopProxy` — `EventLoopProxy::send_event` calls
`PostMessageW` directly. The executor's `target_window` is always the event
loop's hidden message window `thread_msg_target` (`event_loop.rs`
`create_thread_executor`), never a pop-out's HWND. So
`ERROR_INVALID_WINDOW_HANDLE` means that message window is gone — event-loop
teardown — not that a pop-out closed, and `DEAD_TARGET_WINDOWS` holds at most
about one entry per event loop. Windows does reuse HWND values; the set is
sound only because a message window is destroyed once, at teardown.

Re-derived difference from the 0.35.0 patch: the old patch kept upstream's
`panic!` for a non-`ERROR_INVALID_WINDOW_HANDLE` failure. Upstream itself has
since stopped panicking there, and a panic on a background thread is strictly
worse than a loud log for a closure that is already dropped, so the 0.37.2
patch logs instead. The upstream message text is gone from the file, which is
what the test checks.

## Verifying the delta

That is the entire source delta. Verify the `src/` tree is identical to the
crates.io release except for those three files:

```sh
diff -rq ~/.cargo/registry/src/index.crates.io-*/tao-0.37.2/src vendor/tao-0.37.2/src
# expected: exactly
#   .../event_loop/runner.rs  and  vendor/.../event_loop/runner.rs differ
#   .../event_loop.rs         and  vendor/.../event_loop.rs        differ
#   .../linux/event_loop.rs   and  vendor/.../linux/event_loop.rs  differ
```

(The registry copy exists only if some build fetched registry tao; otherwise
download `https://static.crates.io/crates/tao/tao-0.37.2.crate` and check its
sha256 against the checksum above.)

Only cargo bookkeeping files differ outside `src/`: this copy drops the
`Cargo.toml.orig` and `.cargo_vcs_info.json` files the published crate
carries, and adds this file. `Cargo.toml` is the normalized one from the
published crate. None are crate source.

## Why

`EventLoopRunnerShared` (Windows) and `windows` (Linux) live inside tao's
`EventLoopWindowTarget`, which is held by
`tauri_runtime_wry::DispatcherMainThreadContext::window_target`. Since
tauri-runtime-wry 2.12 that context is the `main_thread` field of
`tauri_runtime_wry::Context<T>` (`tauri-runtime-wry-2.12.3/src/lib.rs:235-249`),
and it is `Context<T>` that carries the blanket thread-safety claim:

```rust
unsafe impl<T: UserEvent> Send for Context<T> {}
unsafe impl<T: UserEvent> Sync for Context<T> {}
```

(In tauri-runtime-wry 2.11 the same `unsafe impl Send/Sync` sat directly on
`DispatcherMainThreadContext<T>`, with a `SAFETY: we ensure this type is only
used on the main thread` comment. tauri PR #14805 moved it up to `Context`;
the reachable `Rc` is unchanged.)

That claim holds for *dereferencing* the window target — `Context::run_threaded`
only hands out `&DispatcherMainThreadContext` on the main thread — but not for
the refcount. `Context<T>` is `#[derive(Clone)]`, so cloning a
`tauri::AppHandle` or `Webview` on a background thread clones `Context`, which
clones `main_thread`, which clones the `EventLoopWindowTarget` and with it this
`Rc`. Its **non-atomic** count is incremented concurrently with the main
thread. That is a data race, and it corrupts.

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
every released version through tao 0.37.2 / tauri 2.12.5. tauri PR #14805
(shipped in 2.12) moved the `unsafe impl` up to `Context` but does **not**
remove this `Rc` clone race.

`src-tauri/tests/tao_arc_patch_active.rs` fails the build if this patch ever
stops applying, if tao resolves to any version other than this one, or if the
vendored source loses any of the three changes. On a tauri upgrade, re-vendor
the new tao version with the same changes rather than deleting that test.
