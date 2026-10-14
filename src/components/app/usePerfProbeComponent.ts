/**
 * Registers the `perf-probe` UI Bridge component — perf-harness's eval-free
 * frontend probe (`src/lib/perfProbe.ts`; plan
 * `2026-10-14-perf-harness-frontend-probe-evaluates-strings-the-csp-refuses`).
 *
 * Called from `AppContent` beside the `setup-wizard` registration, before any
 * early return, so the component is registered whenever the main window runs —
 * whatever tab is showing and whatever the auth state. `useUIComponent` only
 * registers while its owner is mounted, which is why it is not on the Terminal
 * page.
 */

import { useEffect } from "react";
import { useUIComponent } from "@qontinui/ui-bridge";

import { buildPerfProbeActions, perfProbe } from "@/lib/perfProbe";

export function usePerfProbeComponent(): void {
  useUIComponent({
    // Literal, so the effect walk and a grep both read the id perf-harness addresses.
    id: "perf-probe",
    name: "Perf Probe",
    description:
      "perf-harness's frontend probe (scripts/perf-harness.mjs): rAF frame gaps, long tasks, " +
      "terminal-output delivery counts and window.__qontinuiTransportStats, read without " +
      "page/evaluate (the CSP refuses evaluated strings). Idle until 'install'.",
    scope: "global",
    // A list factory so the handlers are testable under vitest's node env
    // (`perfProbe.test.ts`). The lint rule cannot enumerate a call; the AST walk
    // in `action-effect-coverage.test.ts` follows it to the literal, which is
    // the same arrangement `TerminalPage`'s `buildTerminalLaunchMenuActions` uses.
    // eslint-disable-next-line @qontinui/ui-bridge/require-action-effect -- see the comment above
    actions: buildPerfProbeActions(perfProbe),
  });

  // An unmounted owner must not leave observers running in the webview.
  useEffect(() => () => void perfProbe.stop(), []);
}
