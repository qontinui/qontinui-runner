import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

// ---------------------------------------------------------------------------
// Phase 3 of `2026-08-23-typed-error-boundary-invariant`.
//
// The defect is a catch site that DISCARDS what it caught:
//
//     catch (err) { setError(err instanceof Error ? err.message : "Failed to X"); }
//
// `invoke()` rejects with a plain STRING, not an `Error`, so on every real
// Tauri failure the second arm is the one taken — and the panel renders the
// bare constant while the actual cause ("coord unreachable: connection
// refused") is thrown away.
//
// WHY THIS TEST READS SOURCE. `describeThrown` is already thoroughly tested in
// `coordinatorApi.test.ts` — but those tests prove the HELPER works, which is
// not the property this phase delivers. The property is that this file's catch
// sites USE it, and no assertion about the helper can establish that. Driving
// all fourteen panels through a mocked failing `invoke` would test the same
// thing far more expensively, and would still only cover the paths a test
// happened to exercise; the scan covers every one, including sites added later.
//
// ONE assertion, deliberately. Earlier drafts of this file also pinned the
// COUNT of converted sites and of surviving `String(err)` sites. Those were
// inventory snapshots, not properties: they would have gone red when someone
// added a *correct* fifteenth call, which is precisely the change this test
// should welcome. A count that punishes the improvement it is meant to protect
// is worse than no test.
//
// SCOPE — do not read this as the class being handled. Measured 2026-09-09
// across `src/`: the bare-literal discard shape occurs at ~140 sites in ~74
// files (`useContexts.ts` and `useScheduler.ts` have ten and nine of their
// own). The plan that motivated this phase deferred a `no-restricted-syntax`
// ESLint rule on the grounds that "10 sites in 1 file does not justify" one —
// that reasoning does not survive the measurement, and the rule is now the
// obviously correct instrument. This file-scoped guard closes one file's worth
// and is a placeholder for it, not a substitute.
// ---------------------------------------------------------------------------

const SOURCE = readFileSync(join(__dirname, "CoordinatorDashboard.tsx"), "utf8");

describe("CoordinatorDashboard error handling (Phase 3)", () => {
  it("no catch site hand-rolls error stringification — they all go through describeThrown", () => {
    // Matches the ternary in any form: single or double quotes, a template
    // literal, `String(err)`, or a line Prettier has wrapped. Anything that
    // reaches for `instanceof Error ?` here is re-implementing `describeThrown`
    // less well, whichever arm it falls back to.
    const offenders = SOURCE.match(/\w+\s+instanceof\s+Error\s*\?[^;]*/g) ?? [];
    expect(offenders).toEqual([]);
  });
});
