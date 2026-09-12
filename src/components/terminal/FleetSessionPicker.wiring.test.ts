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
    expect(SOURCE).toContain("fleetTruncation(response, sessions.length, hasMore)");
    expect(SOURCE).not.toContain("fleetTruncation(response, server.limit");
    expect(SOURCE).not.toContain("fleetTruncation(response, appliedQuery");
    // And never on the ENVELOPE's cursor, which is the pair that diverges the
    // moment the walk drops one — see the suite below. Scanned through
    // `codeOf` for the reason its own jsdoc gives: these files DOCUMENT the
    // defect at length on purpose, and a raw-text guard would fail on the
    // explanation and pressure someone into deleting that instead.
    expect(codeOf(SOURCE)).not.toContain("response.nextCursor");
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

  it("never calls a stalled WALK a failed READ — in the EMPTY state too", () => {
    // Reachable: coord serves an empty page carrying a cursor, then stalls on
    // the `more`. The inline banner drew this distinction from the start; the
    // empty-state branch printed "This is a failed read, not an empty fleet."
    // over a read coord had answered.
    // The sentence must be the FALSE arm of a `walkStalled` ternary, not the
    // unconditional text it used to be.
    const code = codeOf(SOURCE);
    expect(code).toContain("This is a failed read, not an empty fleet.");
    const at = code.indexOf("This is a failed read");
    const guarded = code.slice(Math.max(0, at - 400), at);
    expect(guarded).toContain("{walkStalled");
    // Both empty-state arms are conditional on it, so neither can be reinstated
    // unconditionally without this failing.
    expect(code.split(/\bwalkStalled\b/).length - 1).toBeGreaterThanOrEqual(3);
  });

  it("never calls a stalled WALK a failed READ", () => {
    // coord answered: the rows below are current and only the next page is out
    // of reach. "Last refresh failed — showing the previous read" over that is
    // a false claim in the other direction, so the prefix is conditional.
    expect(HOOK).toContain("walkStalled,");
    expect(SOURCE).toMatch(/\{walkStalled\s*\?\s*error\s*:/);
  });
});

/**
 * The walk's own cursor and the last envelope's `nextCursor` are DIFFERENT
 * facts, and the picker must classify on the first.
 *
 * `useFleetSessions` drops the walk's cursor on `cursor_malformed`,
 * `cursor_version_unsupported` and on a cursor coord returns unchanged, while
 * deliberately keeping the previous successful response — whose `nextCursor` is
 * still set. Reading completeness off that envelope puts a "Load more" control
 * on screen in exactly the state where `fetchPage("more")` returns immediately:
 * a button that does nothing at all when clicked, which is the failure the
 * whole phase exists to remove rather than a cosmetic one.
 */
describe("a dropped cursor removes the control, and does not claim completeness", () => {
  it("reads the WALK's answer, which the hook exports for this", () => {
    expect(HOOK).toContain("hasMore: walk.nextCursor !== null,");
    expect(SOURCE).toContain("hasMore,");
    expect(SOURCE).toContain("fleetTruncation(response, sessions.length, hasMore)");
  });

  it("invokes `loadMore` from exactly ONE place, inside the `more-available` guard", () => {
    // The property is "no arm but `more-available` offers a page control", and
    // two earlier attempts at it guarded a SPELLING instead.
    //
    // The first sliced between two `indexOf` anchors where the end anchor
    // matched EARLIER in the file than the start; `String.slice` with
    // `end < start` returns "", so both assertions were vacuously true and a
    // live "Load more" button planted in the unreachable block passed.
    //
    // The second counted the shared id constant and the exact text
    // `void loadMore()` — so a freshly written button with its own id and
    // `onClick={() => { loadMore(); }}` spelled neither and passed too, while
    // being exactly the dead control this phase exists to remove.
    //
    // What cannot be spelled around: a page control has to CALL `loadMore`.
    // Count the references and pin where the single call site sits.
    const code = codeOf(SOURCE);

    // One destructure from the hook, one call. A third reference is a second
    // way to advance the walk, wherever and however it is written.
    expect(code.split(/\bloadMore\b/).length - 1).toBe(2);

    // The call site is inside the `more-available` JSX guard. Anchored on the
    // guard's OPENING BRACE, which occurs once — the bare predicate also
    // appears as an argument to `fleetFilteredOutMessage` far earlier in the
    // component, and anchoring on that spans most of the file.
    const jsxGuard = '{truncation.kind === "more-available" && (';
    expect(code.split(jsxGuard).length - 1).toBe(1);
    const guardAt = code.indexOf(jsxGuard);
    const callAt = code.search(/\bloadMore\(\)/);
    expect(guardAt).toBeGreaterThan(-1);
    expect(callAt).toBeGreaterThan(guardAt);
    // …and no other truncation arm opens between the guard and the call, so the
    // control cannot have been re-parented without this failing.
    const between = code.slice(guardAt + jsxGuard.length, callAt);
    expect(between.length).toBeGreaterThan(0);
    expect(between).not.toContain("truncation.kind ===");
  });

  it("does not stack two incompleteness warnings on the stalled path", () => {
    // A stalled cursor sets BOTH `walkStalled` (whose banner says, in the same
    // words, that the walk cannot advance — with a Retry) and `unreachable`.
    // Rendering both puts the same fact on screen twice, one styled as an error.
    expect(SOURCE).toContain('truncation.kind === "unreachable" && !walkStalled');
  });

  it("gives the two strips DISTINCT bridge ids", () => {
    // They make opposite claims about whether the next page can be fetched. A
    // driver that could not tell them apart would read "there is more" as
    // "there is a control for it".
    expect(SOURCE).toContain('FLEET_PICKER_UNREACHABLE_ID = "terminal.fleet-picker-unreachable"');
    expect(SOURCE).toContain('FLEET_PICKER_TRUNCATION_ID = "terminal.fleet-picker-truncation"');
  });

  it("projects coord's machine error code, not only the prose banner", () => {
    // The CODE is coord's contract and the detail prose explicitly is not, so a
    // driver that had to match on the sentence would break on a reword.
    expect(HOOK).toContain("errorCode,");
    expect(SOURCE).toContain("errorCode,");
    expect(SOURCE).toContain('data-fleet-error-code={errorCode ?? ""}');
  });
});

/**
 * A page RESIZE must re-use the walk, not restart it.
 *
 * coord fingerprints `device_id` / `state` / `include_closed` into the cursor
 * and deliberately leaves `limit` out, so a cursor survives a changed page size
 * — which is why `fleetScopeKey` omits it. Closing `fetchPage` over `limit` and
 * then keying the restart effect on `fetchPage` undoes all of that silently:
 * every accumulated page is discarded and re-fetched.
 */
describe("the restart trigger is the SCOPE, not the callback's identity", () => {
  it("names the scope explicitly in the restart trigger", () => {
    // Not because `fetchPage`'s identity is wrong — the two move together — but
    // so the trigger SAYS what it is. Keying only on a callback's identity makes
    // the trigger an implicit consequence of that callback's dependency list,
    // which is exactly how `limit` got in.
    expect(HOOK).toContain("const scopeKey = fleetScopeKey({ deviceId, state, includeClosed });");
    expect(HOOK).toContain("}, [fetchPage, scopeKey, restartToken]);");
    // And it needs no suppression: both are real dependencies of the effect.
    expect(HOOK).not.toContain("eslint-disable-next-line react-hooks/exhaustive-deps");
  });

  it("syncs the page-size ref BEFORE the restart effect, in declaration order", () => {
    // React runs effects in declaration order. Reversed, a commit that changes
    // the page size and the scope together fetches page one at the OLD size —
    // silently, and only in that one case.
    expect(HOOK.indexOf("limitRef.current = limit;")).toBeLessThan(
      HOOK.indexOf('void fetchPage("restart");'),
    );
  });

  it("keeps `limit` out of the dependency list that restarts the walk", () => {
    expect(HOOK).toContain("[deviceId, state, includeClosed],");
    expect(HOOK).not.toContain("[deviceId, state, includeClosed, limit],");
  });

  it("reads the page size through a ref so the next page uses the new one", () => {
    // Out of the dep list, but still current at the moment of the call — the
    // same reason `cursorRef` exists.
    expect(HOOK).toContain("const limitRef = useRef(limit);");
    expect(HOOK).toContain("const limit = limitRef.current;");
  });

  it("leaves `limit` out of the scope fingerprint itself", () => {
    expect(DISCOVERY).toContain(
      "return JSON.stringify([scope.deviceId, scope.state, scope.includeClosed]);",
    );
  });
});

/**
 * Two timestamps coord serves and the picker used to read neither.
 *
 * On a list capped at one page that was survivable; under a cursor walk the
 * list runs to every session on the tenant, and `coord.sessions.state` is a
 * stored column a watcher advances — it can read `active` over a session whose
 * last heartbeat was days ago.
 */
describe("a walked list shows when each row was last observed", () => {
  it("renders the row's most recent instant through the shared formatter", () => {
    expect(SOURCE).toContain("fleetSessionActivity(s)");
    expect(SOURCE).toContain("formatRelativeTime(activity.iso)");
  });

  it("renders nothing at all when coord served no parseable timestamp", () => {
    // Null is UNKNOWN. A placeholder would look like an answer coord never gave.
    expect(SOURCE).toContain("if (free.length === 0 && !activity) return null;");
  });

  it("projects the exact instant for a driver, not only the locale prose", () => {
    // Same rule as `data-fleet-error-code`: the rendered form is locale- and
    // clock-dependent and the span is `truncate`d, so it is not a contract.
    expect(SOURCE).toContain("data-session-activity={activity.iso}");
    expect(SOURCE).toContain("data-session-activity-kind={activity.verb}");
  });

  it("keeps the relative label moving while the panel sits open", () => {
    // Computed during render, and nothing else re-renders the picker between
    // fetches — without a tick a row reads "heartbeat 2m ago" an hour later,
    // which is a stale liveness claim in the one place this list must be honest.
    expect(SOURCE).toContain("setClockTick");
    expect(SOURCE).toContain("setInterval(() => setClockTick((t) => t + 1), 30_000)");
    expect(SOURCE).toContain("clearInterval(id)");
  });
});
