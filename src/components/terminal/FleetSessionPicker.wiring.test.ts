/**
 * Wiring invariants for `FleetSessionPicker` and the walk behind it that no
 * pure-function test can reach.
 *
 * Two classes of defect live here.
 *
 * The first is a MISPAIRING. The component holds two filter sets at once — the
 * one it is currently requesting and the one the rows on screen were served
 * for — and saying anything about the rows with the pending set makes the
 * on-screen claim false ("coord truncated this read at 250" when it truncated
 * at 100). A failed refetch makes that permanent: `useFleetSessions`
 * deliberately keeps the old response and returns `loading` to false.
 *
 * The second is the CONTRACT BREAK Phase 5a repaired. coord retired `truncated`
 * for a keyset cursor; the component and the hook must mention neither the
 * field nor the retired page ladder, and the walk must send the cursor rather
 * than a bigger `limit`.
 *
 * Asserted by reading the source, following `SessionManagerPanel.test.ts`: the
 * runner's vitest config is `environment: "node"` — no jsdom, no
 * `@testing-library/react` — so there is no render to inspect.
 */

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { describe, it, expect } from "vitest";

const SOURCE = readFileSync(
  fileURLToPath(new URL("./FleetSessionPicker.tsx", import.meta.url)),
  "utf8",
);
const HOOK = readFileSync(fileURLToPath(new URL("./useFleetSessions.ts", import.meta.url)), "utf8");
const DISCOVERY = readFileSync(
  fileURLToPath(new URL("./fleetDiscovery.ts", import.meta.url)),
  "utf8",
);

/**
 * The same source with its comments removed.
 *
 * The "is it gone?" assertions below have to scan CODE: these files document
 * the retired `truncated` contract and the retired page ladder at length, on
 * purpose — a reader who meets `nextCursor` with no record of what it replaced
 * is one refactor away from reinventing the bug. Scanning raw text would make
 * that documentation fail the guard and pressure someone into deleting the
 * explanation instead of the defect.
 *
 * `//` is only treated as a line comment when it is not part of a `://`, so a
 * url inside a string survives.
 */
function codeOf(text: string): string {
  return text.replace(/\/\*[\s\S]*?\*\//g, "").replace(/(^|[^:])\/\/.*$/gm, "$1");
}

describe("everything said ABOUT the loaded rows is said with the query they came from", () => {
  it("classifies completeness from the response and the ACCUMULATED count", () => {
    // Stronger than passing the applied limit in: the page size now comes off
    // the response itself, so there is no parameter a caller could hand the
    // wrong value to. `sessions` is the hook's accumulation for that same
    // response — both move in one tick.
    expect(SOURCE).toContain("fleetTruncation(response, sessions.length)");
    expect(SOURCE).not.toContain("fleetTruncation(response, server.limit)");
    expect(SOURCE).not.toContain("fleetTruncation(response, appliedQuery");
  });

  it("explains an empty READ with the applied query, falling back to the pending one", () => {
    // The fallback only bites before any read completed, where the two are
    // equal anyway — and that branch renders "no successful read yet", not an
    // emptiness claim.
    expect(SOURCE).toContain("fleetEmptyReadMessage(appliedQuery ?? server");
  });

  it("projects the applied query under data-fleet-*, the pending one under data-fleet-pending-*", () => {
    // A UI Bridge driver reads these in one pass, so the filter set and the row
    // counts beside it have to describe the same query.
    for (const applied of [
      "data-fleet-limit={appliedQuery?.limit",
      "data-fleet-device-filter={appliedQuery?.deviceId",
      "data-fleet-state-filter={appliedQuery?.state",
    ]) {
      expect(SOURCE, applied).toContain(applied);
    }
    for (const pending of [
      "data-fleet-pending-limit={server.limit}",
      "data-fleet-pending-device-filter={server.deviceId",
      "data-fleet-pending-state-filter={server.state",
    ]) {
      expect(SOURCE, pending).toContain(pending);
    }
    // The trap this guards: the row counts come from the response, so pairing
    // them with the CONTROLS' state publishes a filter set that did not produce
    // them.
    expect(SOURCE).not.toContain("data-fleet-limit={server.limit}");
    expect(SOURCE).not.toContain("data-fleet-device-filter={server.deviceId");
    expect(SOURCE).not.toContain("data-fleet-state-filter={server.state");
  });

  it("publishes the applied query in the SAME tick as the response it belongs to", () => {
    // The pairing the sibling review established. `setAppliedQuery` sits in the
    // success branch beside `setResponse`, never in an effect over `response`
    // — an effect would publish the PREVIOUS response's rows under the NEW
    // query for one render, which is exactly the false claim it exists to stop.
    const success = HOOK.slice(HOOK.indexOf("setResponse(result)"));
    expect(success.indexOf("setAppliedQuery({")).toBeGreaterThan(-1);
    expect(success.indexOf("setAppliedQuery({")).toBeLessThan(success.indexOf("} catch"));
    expect(HOOK).not.toMatch(/useEffect\([^)]*setAppliedQuery/s);
  });
});

describe("the retired `truncated` contract is gone from every layer", () => {
  it("no fleet module reads, declares or fakes the retired flag", () => {
    // The break this phase repaired: `if (!response.truncated)` reads
    // `!undefined` as "complete", so the banner silently stopped rendering and
    // the picker claimed a full list again. Leaving the identifier anywhere is
    // how that comes back.
    for (const [name, text] of [
      ["FleetSessionPicker.tsx", SOURCE],
      ["useFleetSessions.ts", HOOK],
      ["fleetDiscovery.ts", DISCOVERY],
    ] as const) {
      const code = codeOf(text);
      expect(code, name).not.toMatch(/\.truncated\b/);
      expect(code, name).not.toMatch(/^\s*truncated[?]?:/m);
    }
    // And the doc comments MUST still say what was retired and why, so the
    // next reader does not rediscover `truncated` as a good idea.
    expect(HOOK).toMatch(/`truncated` is GONE/);
  });

  it("the page ladder is deleted, not merely unused", () => {
    for (const gone of ["FLEET_LIMIT_LADDER", "nextFleetLimit", "nextLimit", "at-ceiling"]) {
      expect(codeOf(DISCOVERY), gone).not.toContain(gone);
      expect(codeOf(SOURCE), gone).not.toContain(gone);
      expect(codeOf(HOOK), gone).not.toContain(gone);
    }
  });

  it("the page control fetches the next CURSOR page, never a bigger limit", () => {
    // A ladder click used to raise `server.limit`. A walk click must not touch
    // the filter at all — it re-sends coord's cursor with the identical scope.
    expect(SOURCE).toContain("onClick={() => void loadMore()}");
    expect(SOURCE).not.toMatch(/setServer\([^)]*limit:/);
  });
});

describe("the walk sends the cursor, and only within its own scope", () => {
  it("puts the cursor on the wire beside the three scope parameters", () => {
    expect(HOOK).toContain('invoke<FleetSessionsResponse>("fleet_sessions_list"');
    expect(HOOK).toContain("args: { deviceId, state, includeClosed, limit, cursor }");
  });

  it("sends a cursor ONLY when advancing, never on a restart", () => {
    // An empty `cursor` is page one to coord rather than an error, so a restart
    // that sent a stale one would silently walk the previous scope.
    expect(HOOK).toContain('const cursor = mode === "more" ? cursorRef.current : null;');
    expect(HOOK).toContain('if (mode === "restart") cursorRef.current = null;');
  });

  it("refuses to 'advance' with no cursor instead of re-reading page one", () => {
    expect(HOOK).toContain('if (mode === "more" && cursor === null) return;');
  });

  it("restarts on a scope mismatch and shows the operator nothing", () => {
    // Changing a filter mid-walk is ordinary use. The restart goes through a
    // token the single effect watches, so no second effect is added and the
    // cursor is dropped before the new page one is asked for.
    expect(HOOK).toContain("if (fleetErrorIsRestart(code)) {");
    expect(HOOK).toContain("setRestartToken((t) => t + 1);");
    const branch = HOOK.slice(
      HOOK.indexOf("if (fleetErrorIsRestart(code)) {"),
      HOOK.indexOf("if (fleetErrorInvalidatesCursor(code)) {"),
    );
    expect(branch).toContain("cursorRef.current = null;");
    // No error is published on this path — `return` precedes the setError below.
    expect(branch).not.toContain("setError(");
  });

  it("discards a response whose scope has been superseded rather than merging it", () => {
    // Without this a page from the previous scope lands in the new scope's
    // accumulation, and the count beside the filters describes neither query.
    expect(HOOK).toContain("if (generationRef.current !== generation) return;");
  });

  it("keeps `loadMore` off a stale closure by reading the cursor from a ref", () => {
    expect(HOOK).toContain("const cursorRef = useRef<string | null>(null);");
    expect(HOOK).toContain('const loadMore = useCallback(() => fetchPage("more"), [fetchPage]);');
  });
});

describe("a page in flight never makes a true list read as a stale one", () => {
  it("shows the previous-read banner for a RESTART only", () => {
    // `loadingMore` appends: the rows below are the same walk's earlier pages
    // and stay valid, so borrowing the "these are from the previous read"
    // banner would be a false claim in the other direction.
    const banner = SOURCE.slice(SOURCE.indexOf("FLEET_PICKER_REREADING_ID") - 400);
    expect(SOURCE).toContain("{loading && (");
    expect(banner.slice(0, 600)).not.toContain("loadingMore &&");
  });

  it("disables both page controls while either read is in flight", () => {
    expect(SOURCE).toContain("disabled={loading || loadingMore}");
  });

  it("never calls a stalled WALK a failed READ", () => {
    // coord answered: the rows below are current and only the next page is out
    // of reach. "Last refresh failed — showing the previous read" over that is
    // a false claim in the other direction, so the prefix is conditional.
    expect(HOOK).toContain("walkStalled,");
    expect(SOURCE).toMatch(/\{walkStalled\s*\?\s*error\s*:/);
  });
});
