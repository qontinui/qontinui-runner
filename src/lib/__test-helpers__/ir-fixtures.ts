/**
 * Shared IR-document fixture helpers for tests.
 *
 * The IR's `IRState` carries assertions (`IRAssertion[]`), not raw element
 * criteria. Tests want to think in criteria — "this state requires a Submit
 * button" — so we expose `makeTestAssertion(stateId, idx, criteria)` which
 * wraps a single `IRElementCriteria` in the deterministic `IRAssertion`
 * envelope used throughout the IR.
 *
 * Keeping this in `src/lib/__test-helpers__/` means both
 * `src/lib/__tests__/*` and `src/pages/regression/__tests__/*` can import it
 * via `@/lib/__test-helpers__/ir-fixtures`.
 */

import type { IRAssertion, IRElementCriteria } from "@qontinui/shared-types/ui-bridge-ir";

/**
 * Build a stable `IRAssertion` from a single element criterion, suitable for
 * test-fixture `IRState.assertions[]` lists. The id is deterministic so
 * `assertionIdOf`-style derivations in regression suites stay reproducible.
 */
export function makeTestAssertion(
  stateId: string,
  idx: number,
  criteria: IRElementCriteria,
): IRAssertion {
  return {
    id: `${stateId}-elem-${idx}`,
    description: `Required element ${idx}`,
    category: "element-presence",
    severity: "critical",
    assertionType: "exists",
    target: {
      type: "search",
      criteria: criteria as Record<string, unknown>,
      label: `Required element ${idx}`,
    },
    source: "test-fixture",
    reviewed: false,
    enabled: true,
  };
}
