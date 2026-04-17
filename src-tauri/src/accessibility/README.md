# Accessibility — Adapter Architecture

The `accessibility` module is qontinui's native accessibility layer. It exposes
a unified node/state/pattern model to the rest of the runner and delegates the
platform-specific work to per-OS adapters that all implement
[`PlatformAdapter`](./traits.rs).

## Module layout

```
accessibility/
  mod.rs                  - AccessibilityManager facade, TreeCache, RefManager,
                            FusionEngine, event broadcast.
  traits.rs               - PlatformAdapter trait. The public contract every
                            adapter (including any future JAB adapter) meets.
  model.rs                - UnifiedNode, UnifiedRole, InteractionPattern, etc.
  query/                  - Fluent QueryBuilder and typed-control accessors.
  cache.rs                - Generation-scoped tree cache.
  ref_manager.rs          - Persistent ref IDs at ~/.qontinui/a11y_refs.
  events.rs               - A11yEvent channel types.
  fusion.rs               - Native-tree + DOM-tree fusion engine (reserved).
  interaction.rs          - Shared interaction plumbing.
  integration_test.rs     - Smoke test against a live Notepad window (#[ignore]).
  flaui_conformance_test.rs - Conformance suite cribbed from FlaUI.Core.UITests.
  adapters/
    mod.rs                - Platform-gated adapter dispatch (create_platform_adapter).
    uia/                  - Windows UIA (see below).
    atspi_adapter.rs      - Linux AT-SPI2 via atspi + zbus.
    ax.rs                 - macOS AX via core-foundation FFI.
```

A single `create_platform_adapter()` in `adapters/mod.rs` picks the adapter
for the current target via `cfg`. No runtime OS branching.

## Windows UIA — split into a directory

The Windows adapter used to be a single 822-line `adapters/uia.rs`. It now
lives as `adapters/uia/` with three files:

```
adapters/uia/
  mod.rs    - Public UiaAdapter. PlatformAdapter impl. UiaBackend trait.
              BackendChoice enum and select_backend selector.
  common.rs - Backend-agnostic helpers: pattern-ID constants, control-type
              mapping, UiaHandleTable, tree-walk (build_node), pattern
              dispatch (interact_with_element), FocusChangedHandler, UiaState.
  uia3.rs   - UIA3 backend: picks CUIAutomation class ID. init_state and
              find_root helpers specific to this backend.
```

This mirrors the proxy-builder pattern already used by `atspi_adapter.rs`:
backend code produces the low-level COM handles, shared code drives them.

## `UiaBackend` trait — what it abstracts

```rust
pub trait UiaBackend: Send + Sync {
    fn name(&self) -> &'static str;
    fn initialize(&self) -> anyhow::Result<(IUIAutomation, IUIAutomationTreeWalker)>;
}
```

The trait is intentionally narrow. UIA2 and UIA3 expose the same Rust
interface types (`IUIAutomation`, `IUIAutomationElement`, `IUIAutomationTreeWalker`)
through the `windows` crate — the only real difference is *which* COM class ID
you pass to `CoCreateInstance` and which `windows` crate feature exposes it.
Moving that one choice behind `initialize()` lets all the real logic (tree
walking, handle bookkeeping, pattern dispatch, event handling) stay in
`common.rs`, unchanged, regardless of backend.

Properties of the trait chosen on purpose:

- `Send + Sync` because adapter code holds the backend across
  `tokio::task::spawn_blocking` boundaries.
- `initialize` is synchronous because COM init must happen on a blocking
  thread anyway; the adapter wraps the call in `spawn_blocking`.
- Find-root, tree-walk, and pattern dispatch are *not* trait methods — they're
  free functions / free impls in `common.rs` / `uia3.rs` that take the
  `IUIAutomation` + `IUIAutomationTreeWalker` produced by `initialize`. That
  keeps the trait surface from ballooning and avoids forcing every backend to
  re-implement identical logic.

## Adding a new UIA backend

1. Add a module under `adapters/uia/`, e.g. `uia2.rs`.
2. Define a struct (e.g. `Uia2Backend`) and impl `UiaBackend` for it. The
   `initialize` body should `CoCreateInstance` whichever COM class ID you
   target (for UIA2 this means a different class plus likely an extra
   `windows` crate feature flag — confirm before implementing).
3. Extend the [`BackendChoice`] enum in `mod.rs` with a variant for the new
   backend.
4. Extend the `match` in `select_backend` to return a boxed instance.
5. Extend the `match` in `UiaAdapter::init_uia` that maps
   `self.backend.name()` back to a `BackendChoice` for the `spawn_blocking`
   hop. (This round-trip exists because `Box<dyn UiaBackend>` is not `Clone`
   but must cross the blocking boundary.)
6. If the backend shares the UIA3 `find_root` call shapes, reuse
   `uia3::find_root`. If not, add a `find_root` next to your backend struct.
7. When the selector grows a non-trivial fall-through strategy (e.g. `Auto`
   mode that retries UIA2 on empty trees), put that logic in
   `adapters/uia/auto.rs` — a file that is intentionally absent today.

## Why UIA2 support is deferred

FlaUI ships dual UIA2 / UIA3 bindings because UIA3 has known WinForms bugs
while UIA2 lacks coverage for some modern apps. Qontinui's scout memo flagged
this as a gap but no concrete WinForms/legacy blocker has hit yet, so landing
a second backend is deferred until one does.

The refactor documented above is the *prerequisite* for that eventual UIA2
addition — it has standalone value even without UIA2 because it replaces the
822-line monolith with focused modules. All the scaffolding (`UiaBackend`
trait, `BackendChoice` enum, `select_backend` selector, `adapters/uia/`
directory layout) is in place so the UIA2 backend can slot in without another
round of restructuring.

No `adapters/uia/uia2.rs` or `adapters/uia/auto.rs` exists yet — that is the
deferred work. `adapters/uia/mod.rs` currently always selects UIA3.
