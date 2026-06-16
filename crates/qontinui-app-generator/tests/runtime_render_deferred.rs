//! Runtime-deferred leg of Phase 1 (HONEST stub — NOT a fake verification).
//!
//! The plan's full observability loop is:
//!
//! ```text
//!   generate_screen(IrState) -> app/<id>.tsx
//!     -> run under the UI Bridge native server (Expo/Metro + device|simulator)
//!     -> capture a real NativeBridgeSnapshot
//!     -> qontinui_spec_check::evaluate(snapshot, spec) -> SpecCheckResult
//! ```
//!
//! The deterministic legs (generate the screen; prove the instrumentation manifest is
//! matcher-correct by projecting it into a UIBridgeSnapshot and running the REAL
//! matcher) are verified in `golden_pairing_confirm.rs`.
//!
//! The ON-DEVICE leg — actually rendering the emitted `.tsx` in Expo and reading back a
//! NativeBridgeSnapshot from the running app — requires a Metro bundler + an Expo
//! device/simulator harness that is NOT part of the Rust toolchain and is not trivially
//! present in this environment. Per the task constraint, it is left UNVERIFIED rather
//! than faked. This `#[ignore]`d test documents exactly what the runtime harness must
//! do and where the seam attaches (`snapshot_element_from` is the deterministic
//! stand-in; the real path replaces it with a live snapshot fetch from the running app).
//!
//! To run the real verification once an Expo harness exists:
//!   1. Write `artifact.tsx` to the generated Expo app's `app/<id>.tsx`.
//!   2. Boot the app under `@qontinui/ui-bridge-native` with the native HTTP server.
//!   3. GET the bridge snapshot endpoint -> deserialize into `UIBridgeSnapshot`.
//!   4. `evaluate(&live_snapshot, &page)` and assert `match_rate == 1.0`.

#[test]
#[ignore = "runtime: requires an Expo/Metro + device|simulator harness (not in the Rust toolchain); the deterministic matcher-correctness proof lives in golden_pairing_confirm.rs"]
fn real_expo_render_snapshot_round_trip() {
    // Intentionally empty: this is a documentation marker for the runtime-deferred leg.
    // It must NOT synthesize a snapshot and claim a render — that is the job of the
    // deterministic golden test. Wiring instructions are in the module docs above.
    panic!("runtime render harness not wired — see module docs; this is deferred, not verified");
}
