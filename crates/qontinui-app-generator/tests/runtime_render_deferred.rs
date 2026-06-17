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
//! device/simulator harness that is NOT part of the Rust toolchain. Per the task
//! constraint, it is left UNVERIFIED rather than faked. This `#[ignore]`d test documents
//! exactly what the runtime harness must do and where the seam attaches
//! (`snapshot_element_from` is the deterministic stand-in; the real path replaces it with
//! a live snapshot fetch from the running app).
//!
//! ## Why there is no render path TODAY (verified 2026-06-17)
//!
//! The connected device (`RFCW6041HHN`) runs `io.qontinui.mobile`, a **fixed production
//! Expo build** (`flags=0x0` — NOT debuggable; versionCode 86; signed APK). It cannot
//! attach to Metro, so it cannot load an arbitrary generated `.tsx`. `qontinui-mobile`
//! has **no** generated-screen preview/sandbox route, no watched-dir screen loader, and
//! no Storybook-like harness — its Expo Router screens are the fixed app routes. The
//! runner UI Bridge (`:9876`) snapshots the runner's OWN Tauri webview (confirmed: GET
//! `/ui-bridge/sdk/snapshot?appId=qontinui-mobile` returns the runner's
//! `activeTab:"terminal"` tree, not the mobile's). Snapshotting the existing mobile app
//! would verify the FIXED app, not the generator's emitted screen — so it proves nothing
//! about this crate. **Conclusion: no on-device render of a GENERATED screen is reachable
//! without first building a render harness.**
//!
//! ## Concrete harness proposal (what would close the gap)
//!
//! Smallest viable: a **`development`-profile dev-client build** of `qontinui-mobile`
//! (`eas.json` already defines it: `developmentClient: true`) installed on the device,
//! plus a tiny **generated-screen preview route** added to the app — e.g.
//! `app/__appgen-preview.tsx` that dynamically renders the screen written to a
//! Metro-watched `app/_generated/<id>.tsx`. Then the loop is:
//!   1. `generate_app(spec, profile)` -> write each `screen.tsx` to `app/_generated/<id>.tsx`.
//!   2. `expo start --dev-client`; reload the dev-client on the device (Metro picks up the
//!      watched file).
//!   3. Navigate to `__appgen-preview?screen=<id>` (deep link via the `qontinui://` scheme).
//!   4. Fetch the device's `@qontinui/ui-bridge-native` snapshot (mDNS / cloud-relay /
//!      `adb reverse`) -> deserialize into `UIBridgeSnapshot`.
//!   5. `evaluate(&live_snapshot, &page)` and assert `match_rate == 1.0` — the SAME
//!      assertion the deterministic golden test makes, now against a real render.
//!
//! This is a `qontinui-mobile` + harness change, deliberately out of this crate's scope;
//! it is the single remaining gap between the deterministic proof and a true on-device run.

#[test]
#[ignore = "runtime: requires an Expo/Metro + device|simulator harness (not in the Rust toolchain); the deterministic matcher-correctness proof lives in golden_pairing_confirm.rs"]
fn real_expo_render_snapshot_round_trip() {
    // Intentionally empty: this is a documentation marker for the runtime-deferred leg.
    // It must NOT synthesize a snapshot and claim a render — that is the job of the
    // deterministic golden test. Wiring instructions are in the module docs above.
    panic!("runtime render harness not wired — see module docs; this is deferred, not verified");
}
