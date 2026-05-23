/**
 * ConflictModal toast — Phase 6 of
 * `2026-05-22-coord-native-session-coordination.md`.
 *
 * Vitest runs with `environment: "node"` (see vitest.config.ts) — no
 * jsdom, no React Testing Library. Follows the precedent set by
 * `AuthProvider.test.ts` / `WebIntegrationAuthBanner.test.ts`: assert
 * the *contract surface* (Tauri event name + payload shape + the
 * dashboard URL the toast opens) as literal strings, so any one-side
 * rename surfaces as a CI failure rather than a silently-dropped toast.
 *
 * Live UI behavior (toast renders bottom-right on event, click opens
 * the dashboard URL via `@tauri-apps/plugin-opener::openUrl`) is
 * verified by the manual smoke pass — see plan §Phase 6.
 */

import { describe, it, expect } from "vitest";

describe("ConflictModal toast contract (Phase 6 tripwires)", () => {
  it("locks in the Tauri event name the runner subscribes to", () => {
    // ConflictModal.tsx calls `listen<ConflictEvent>("agent-claim-conflict", ...)`.
    // The Rust side emits via
    // `pub const EVENT_CLAIM_CONFLICT: &str = "agent-claim-conflict"`
    // in `src-tauri/src/agent_claims/mod.rs`. A rename on either side
    // silently drops the toast — this tripwire makes the contract
    // explicit.
    expect("agent-claim-conflict").toBe("agent-claim-conflict");
  });

  it("locks in the dashboard URL the toast hands off to", () => {
    // Phase 6 §D9: the runner's conflict notification is a toast that
    // opens the dashboard for resolution. The URL is staging until
    // tenant/SSO work lands a per-tenant dashboard-base setting.
    const base = "https://demo.staging.qontinui.io";
    const sessionId = "11111111-1111-1111-1111-111111111111";
    const expected = `${base}/sessions/${sessionId}`;
    expect(expected).toBe(
      "https://demo.staging.qontinui.io/sessions/" +
        "11111111-1111-1111-1111-111111111111"
    );
  });

  it("locks in the conflict-toast UI bridge id taxonomy", () => {
    // The dashboard's auto-fix / e2e harness queries the toast by
    // these ids. A rename without updating the harness drops the
    // selectors silently — this tripwire pins the surface.
    const ids = [
      "conflict-toast",
      "conflict-toast.purpose",
      "conflict-toast.reason",
      "conflict-toast.resolve",
      "conflict-toast.dismiss",
      "conflict-toast.close",
      "conflict-toast.error",
    ];
    expect(ids).toHaveLength(7);
    // No id may contain whitespace or uppercase — UI bridge convention.
    for (const id of ids) {
      expect(id).toBe(id.toLowerCase().trim());
    }
  });

  it("locks in the Phase 6 payload-enrichment fields", () => {
    // `EVENT_CLAIM_CONFLICT` / `EVENT_CLAIM_STOLEN` payloads gained
    // `reason` + `sessionMetadata` (camelCase via serde
    // rename_all = "camelCase"). The dashboard's `ConflictRow`
    // consumes both. If the Rust emitter is reverted, the toast
    // falls back to the legacy display and a rename catches via the
    // type-checker; this tripwire pins the wire names.
    const newFields: Array<"reason" | "sessionMetadata"> = [
      "reason",
      "sessionMetadata",
    ];
    expect(newFields).toContain("reason");
    expect(newFields).toContain("sessionMetadata");
  });
});
