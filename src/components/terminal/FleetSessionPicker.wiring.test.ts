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
/** One tenant's walk, lifted out of the hook in Phase 4. Its RULES are
 * asserted behaviourally in `fleetWalker.test.ts`; the pins here are only the
 * wiring between it and the hook. */
const WALKER = readFileSync(fileURLToPath(new URL("./fleetWalker.ts", import.meta.url)), "utf8");

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

/**
 * The arguments of the picker's single `fleetEmptyReadMessage(...)` call,
 * comments stripped and whitespace flattened.
 *
 * Anchored to the CALL rather than matched against the whole file, so an
 * argument cannot be deleted while the guard stays green on a mention of the
 * same text somewhere else — and read through `codeOf`, so a comment naming an
 * argument does not satisfy an assertion about passing one.
 *
 * Every failure is LOUD and says which failure it was. A source-grep guard
 * that degrades quietly is worse than none: it reports the absence of an
 * argument when the real fault is that the scan stopped early, and the two
 * arguments this guard exists for are the LAST two, which is exactly what a
 * truncated scan drops.
 *
 *  - More than one call site, or none: thrown, separately worded. The
 *    single-site check is what makes "the first occurrence" a safe thing to
 *    measure — otherwise a helper declared above the real call would be
 *    silently measured in its place.
 *  - Unbalanced, or not four arguments: thrown. Paren counting here does not
 *    understand string, template or regex literals, so a `)` inside an
 *    argument would end the slice early; the arity check turns that from a
 *    confusing green-ish failure into a stated one.
 */
function emptyReadArgs(): string[] {
  const code = codeOf(SOURCE);
  const needle = "fleetEmptyReadMessage(";
  const sites = code.split(needle).length - 1;
  if (sites === 0) throw new Error("FleetSessionPicker no longer calls fleetEmptyReadMessage(");
  if (sites > 1) {
    throw new Error(
      `FleetSessionPicker has ${sites} fleetEmptyReadMessage( call sites; this guard measures ` +
        `the first, so it can no longer speak for the picker's own call`,
    );
  }
  const open = code.indexOf(needle) + needle.length - 1;
  let depth = 0;
  let end = -1;
  for (let i = open; i < code.length; i += 1) {
    if (code[i] === "(") depth += 1;
    else if (code[i] === ")") {
      depth -= 1;
      if (depth === 0) {
        end = i;
        break;
      }
    }
  }
  if (end < 0) throw new Error("unbalanced fleetEmptyReadMessage( call in FleetSessionPicker");

  const inner = code.slice(open + 1, end);
  const args: string[] = [];
  let buf = "";
  let d = 0;
  for (const ch of inner) {
    if (ch === "(" || ch === "[" || ch === "{") d += 1;
    else if (ch === ")" || ch === "]" || ch === "}") d -= 1;
    if (ch === "," && d === 0) {
      args.push(buf);
      buf = "";
    } else buf += ch;
  }
  if (buf.trim()) args.push(buf);
  const cleaned = args.map((a) => a.replace(/\s+/g, " ").trim()).filter((a) => a.length > 0);
  if (cleaned.length !== 4) {
    throw new Error(
      `expected 4 arguments to fleetEmptyReadMessage, parsed ${cleaned.length}: ` +
        JSON.stringify(cleaned),
    );
  }
  return cleaned;
}

describe("everything said ABOUT the loaded rows is said with the query they came from", () => {
  it("classifies completeness from the response and the ACCUMULATED count", () => {
    // Stronger than passing the applied limit in: the page size now comes off
    // the response itself, so there is no parameter a caller could hand the
    // wrong value to. `sessions` is the hook's accumulation for that same
    // response — both move in one tick.
    //
    // Phase 4: classified per WALK inside the hook and folded over the union —
    // each walk by its own envelope, its own accumulation, and its own cursor —
    // so the picker can no longer pair one walk's envelope with the union.
    expect(HOOK).toContain("truncation: fleetUnionTruncation(");
    expect(HOOK).toContain("loaded: w.walk.sessions.length,");
    expect(SOURCE).toContain("    truncation,\n");
    expect(codeOf(SOURCE)).not.toContain("fleetTruncation(");
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
    expect(emptyReadArgs()[0]).toBe("appliedQuery ?? server");
  });

  it("hands the empty message the live text box, not a literal", () => {
    // `text` is a `string`, so replacing it with `""` is not a tsc error — and
    // it silently deletes the whole text-filter branch, the one sentence that
    // stops an operator blaming the search box for an empty list. Same class
    // as the tenant substitution below, and the last unpinned argument.
    expect(emptyReadArgs()[1]).toBe("text");
  });

  it("does not hand the empty message a tenant it invented", () => {
    // The tenant argument is REQUIRED (`tenantId: string | null`), so omitting
    // it is a tsc failure and needs no guard here. What tsc cannot catch is
    // substituting something for the envelope's own value — a remembered
    // tenant, a default, the device's binding — which is exactly the read that
    // produced the 2026-09-28 false report: a correct, empty read of a
    // DIFFERENT tenant than the one holding 200 sessions.
    //
    // Phase 4: the argument is the hook's `readTenants` — the tenant(s) whose
    // walks ANSWERED, each by the tenant its rows were served under, plus the
    // ones whose read FAILED — so a merged empty page cannot claim a tenant it
    // never read.
    expect(emptyReadArgs()[2]).toBe("readTenants");
    expect(HOOK).toContain(".map((w) => servedTenantOf(w.response, w.requestedTenant)),");
    expect(HOOK).toContain("failed: walks.filter(walkFailed).map((w) => w.requestedTenant),");
  });

  it("takes completeness from coord's POSITIVE signal, not from the walk's silence", () => {
    // `readIsComplete` defaults to false, so unlike the tenant the compiler
    // does not hold this one and the guard has to.
    //
    // It must be `kind === "none"` — coord said this was the last page.
    // `!== "unknown"` or `=== "more-available"` would both treat `unreachable`
    // (coord served a cursor the walk can no longer use) as a finished walk,
    // printing "No open sessions in tenant X" under a strip saying coord has
    // more.
    //
    // That state is unreachable through this caller, so this is defence in
    // depth and is labelled so wherever it appears: an empty accumulation
    // means every page was empty, and an empty page carries no cursor because
    // coord's `finish_page` truncates to `limit >= 1` rows before minting one.
    // An earlier draft credited that to "every path producing `unreachable`
    // also sets `error` in the same tick" — which does hold, but is not the
    // reason and is not the simplest one.
    expect(emptyReadArgs()[3]).toBe('truncation.kind === "none"');
  });

  it("projects the tenant as an attribute on the element whose dataset is captured", () => {
    // This file's own rule, stated above with the other data-fleet-* pins: the
    // sentence is not the contract, because a driver matching on it breaks on
    // any rewording. The tenant scope is the claim this change turns on, so it
    // gets a machine-readable projection like every other fleet fact.
    //
    // PLACEMENT is the assertion, not mere presence — and the element that
    // matters is the REGISTERED one. `data-page-element` does not put a node
    // in the control snapshot (the scanner takes interactive elements plus
    // anything carrying `data-ui-bridge-id`), so both are pinned: the root
    // carries the control id, and the tenant sits in its attribute block.
    const code = codeOf(SOURCE);
    const start = code.indexOf("data-page-element={FLEET_SESSION_PICKER_ELEMENT}");
    const end = code.indexOf("data-fleet-pending-include-closed=");
    // Both markers asserted before slicing. `indexOf` returning -1 would make
    // `slice(start, -1)` run to the end of the file, degrading this into a
    // whole-file presence check — the quiet failure `emptyReadArgs` above
    // refuses to have, and it was reachable here by rewording a className.
    expect(start).toBeGreaterThanOrEqual(0);
    expect(end).toBeGreaterThan(start);
    const block = code.slice(start, end);
    expect(block).toContain("data-ui-bridge-id={FLEET_PICKER_ROOT_ID}");
    expect(block).toContain('data-fleet-tenant={servedTenants.join(",")}');
    expect(block).toContain("data-fleet-tenant-failed=");
  });

  it("does not print an UNSCOPED count beside the scoped message", () => {
    // "0 sessions on 0 devices" was the FIRST line of the 2026-09-28 false
    // report. `fleetCountSummary` takes no tenant and emits that under a panel
    // titled Fleet, above the message — so fixing only the sentence would have
    // left the same unscoped claim on the line read first.
    //
    // `sessions.length`, not `visible.length`: the latter would also suppress
    // "0 of 47 loaded on 0 of 3 devices", which is this line at its most
    // informative and is exactly when `fleetFilteredOutMessage` renders.
    expect(codeOf(SOURCE)).toMatch(/sessions\.length === 0\s*\?\s*null\s*:\s*fleetCountSummary/);
  });

  it("licenses the empty message with observed-empty, never with a failed or absent read", () => {
    // Every sentence `fleetEmptyReadMessage` emits presupposes a SUCCESSFUL
    // read that returned nothing. This gate is what makes that true, and it is
    // invisible to that function's own tests: `emptyReasonFor` returns
    // "not-loaded" whenever `response` is null, so the null-tenant arm can
    // never actually render — the reachable unknown-tenant case is coord's
    // untyped envelope, not a first paint.
    //
    // Phase 4 adds `partial` (some tenants answered empty, others failed),
    // which licenses the message only because `readTenants` makes it name the
    // failed tenants — see `fleetEmptyReadMessage`.
    expect(codeOf(SOURCE)).toContain(
      'emptyReason !== "observed-empty" && emptyReason !== "partial"',
    );
  });

  // NOT COVERED HERE, and stated rather than implied: nothing asserts that the
  // empty message and the truncation strip cannot both be on screen making
  // opposite claims, which is the whole point of the completeness axis. It is
  // not merely untested but untestable in this suite — the runner's vitest is
  // `environment: "node"` (see this file's header), so there is no render to
  // inspect. The guards above pin the INPUTS that make the pair consistent;
  // they cannot pin the pair.

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
    // The pairing the sibling review established: the applied query is written
    // in the SAME snapshot update as the response, never by an effect over it —
    // an effect would publish the PREVIOUS response's rows under the NEW query
    // for one render. Behaviourally asserted in `fleetWalker.test.ts`; pinned
    // here so the update cannot be split without this failing.
    const success = WALKER.slice(WALKER.indexOf("response: result,"));
    expect(success.indexOf("appliedQuery: {")).toBeGreaterThan(-1);
    expect(success.indexOf("appliedQuery: {")).toBeLessThan(success.indexOf("} catch"));
    expect(HOOK).not.toMatch(/useEffect\([^)]*appliedQuery/s);
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
      ["fleetWalker.ts", WALKER],
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
      expect(codeOf(WALKER), gone).not.toContain(gone);
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
  // The walk's RULES — no cursor on a restart, none invented on an advance, a
  // superseded page discarded, a scope mismatch restarted silently — are
  // behaviour of `FleetTenantWalker` and are asserted against a fake coord in
  // `fleetWalker.test.ts`. What only a source read can pin is the wiring to the
  // real command.
  it("puts the cursor on the wire beside the scope parameters and the tenant", () => {
    expect(HOOK).toContain('invoke<FleetSessionsResponse>("fleet_sessions_list"');
    // `tenant` is the Rust arg name (`FleetSessionsArgs.tenant`); a misspelled
    // key is dropped by serde in silence and the read runs under the default.
    for (const arg of [
      "deviceId: req.deviceId,",
      "state: req.state,",
      "includeClosed: req.includeClosed,",
      "limit: req.limit,",
      "cursor: req.cursor,",
      "tenant: req.tenant,",
    ]) {
      expect(HOOK, arg).toContain(arg);
    }
    expect(HOOK).toContain("fetchPage: invokeFleetPage,");
  });

  it("builds a NEW walker per scope and disposes the old ones", () => {
    // A response still in flight for the previous scope must be discarded,
    // not merged; disposal is what makes it so.
    expect(HOOK).toContain("new FleetTenantWalker({");
    expect(HOOK).toContain("for (const w of walkers) w.dispose();");
  });

  it("advances only walks that hold a cursor", () => {
    expect(HOOK).toContain(
      "await Promise.all(walkersRef.current.filter((w) => w.canAdvance).map((w) => w.loadMore()));",
    );
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
  it("reads each WALK's answer, never the envelope's cursor", () => {
    expect(HOOK).toContain("canAdvance: w.walk.nextCursor !== null,");
    expect(HOOK).toContain("hasMore: walks.some((w) => w.walk.nextCursor !== null),");
    expect(codeOf(HOOK)).not.toContain("response.nextCursor");
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
  it("names the scope explicitly in the restart trigger — every tenant's cursor scope", () => {
    // The trigger SAYS what it is: one cursor scope per walked tenant. Keying
    // on a callback's identity made the trigger an implicit consequence of that
    // callback's dependency list, which is exactly how `limit` once got in.
    expect(HOOK).toContain(
      "tenants.map((tenantId) => fleetScopeKey({ deviceId, state, includeClosed, tenantId })),",
    );
    expect(HOOK).toContain(
      "}, [scopeKey, tenantsKey, deviceId, state, includeClosed, publish, acceptRows]);",
    );
    // And it needs no suppression: every entry is a real dependency.
    expect(HOOK).not.toContain("eslint-disable-next-line react-hooks/exhaustive-deps");
  });

  it("syncs the page-size ref BEFORE the restart effect, in declaration order", () => {
    // React runs effects in declaration order. Reversed, a commit that changes
    // the page size and the scope together fetches page one at the OLD size —
    // silently, and only in that one case.
    expect(HOOK.indexOf("limitRef.current = limit;")).toBeGreaterThan(-1);
    expect(HOOK.indexOf("limitRef.current = limit;")).toBeLessThan(
      HOOK.indexOf("for (const w of walkers) void w.restart();"),
    );
  });

  it("keeps `limit` out of the dependency list that restarts the walk", () => {
    expect(HOOK).not.toMatch(/\[scopeKey,[^\]]*\blimit\b[^\]]*\]\);/);
  });

  it("reads the page size through a ref so the next page uses the new one", () => {
    // Out of the dep list, but still current at the moment of the call.
    expect(HOOK).toContain("const limitRef = useRef(limit);");
    expect(HOOK).toContain("pageSize: () => limitRef.current,");
    expect(WALKER).toContain("const limit = this.opts.pageSize();");
  });

  it("leaves `limit` out of the scope fingerprint itself", () => {
    expect(DISCOVERY).toContain(
      "return JSON.stringify([scope.deviceId, scope.state, scope.includeClosed, scope.tenantId]);",
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

describe("the 'tab open on this page' notices follow the live tab list", () => {
  // The opened id is recorded once, on success. Gating the notice on it alone
  // kept "Attached — tab open on this page." on screen after the tab closed.
  it("gates both notices on openedTabStillOpen, never on the bare id", () => {
    const code = codeOf(SOURCE);
    expect(code).toMatch(/openedTabStillOpen\(row\?\.openedId, tabs\)/);
    expect(code).toMatch(/openedTabStillOpen\(state\.openedId, tabs\)/);
    expect(code).not.toMatch(/\{\s*row\?\.openedId\s*&&/);
    expect(code).not.toMatch(/if \(state\.openedId\)/);
  });
});

/**
 * The Fleet view's tenant (plan
 * `2026-09-29-fleet-view-reads-one-unchosen-tenant-so-a-multi-bound-device-sees-a-fraction-of-its-fleet`,
 * Phase 3). coord scopes the read to the PRESENTED credential's tenant, so the
 * tenant is chosen here, stays view-local, and follows a row into its attach.
 */
describe("the tenant selector reads, and attaches, the tenant it names", () => {
  it("restarts the walk on a tenant change — the tenant is in every walk's scope", () => {
    expect(HOOK).toContain("fleetScopeKey({ deviceId, state, includeClosed, tenantId })");
  });

  it("hands the selection to the hook and never to set_active_tenant", () => {
    expect(SOURCE).toContain("tenants: walkTenants,");
    expect(SOURCE).toContain("fleetWalkTenants(fleetTenant, tenantCandidates)");
    // Moving the device default for new sessions is not this view's to do.
    expect(codeOf(SOURCE)).not.toContain("set_active_tenant");
    expect(codeOf(SOURCE)).not.toContain("setDefaultTenantForNewSessions");
  });

  it("opens on the policy default until the operator picks — derived, not written by an effect", () => {
    expect(SOURCE).toContain("defaultFleetTenantChoice(tenantPin, tenantCandidates)");
    expect(codeOf(SOURCE)).not.toMatch(/useEffect\([^]*?setFleetTenantPick/);
  });

  it("projects the requested tenant beside the one(s) that answered", () => {
    expect(SOURCE).toContain('data-fleet-tenant={servedTenants.join(",")}');
    expect(SOURCE).toContain(
      'data-fleet-tenant-requested={allTenants ? FLEET_TENANT_ALL_VALUE : (fleetTenant ?? "")}',
    );
    expect(SOURCE).toContain('data-fleet-row-tenant={s.servedTenantId ?? ""}');
  });

  it("mints attach and create grants under the tenant the ROW or GROUP was served for", () => {
    // Never the selection — in a merged view that is every tenant, and between
    // a switch and its answer the rows still belong to the previous one.
    expect(SOURCE).toContain("tenant: s.servedTenantId ?? null,");
    expect(SOURCE).toContain("createRemote(groupKey, g.deviceId, g.label, g.tenantId)");
    expect(codeOf(SOURCE)).not.toMatch(/tenant: fleetTenant/);
    expect(codeOf(SOURCE)).not.toContain("rowsTenant");
  });
});

/**
 * The merged multi-tenant view (Phase 4). The union's honesty rules are pure
 * and asserted in `fleetWalker.test.ts`; these pin the picker's use of them.
 */
describe("the merged view shows every tenant's rows and every tenant's failure", () => {
  it("renders the per-tenant error strip in the merged view only", () => {
    expect(SOURCE).toContain("{merged && failures.length > 0 && (");
    expect(SOURCE).toContain("data-ui-bridge-id={fleetTenantErrorId(f.requestedTenant)}");
    expect(SOURCE).toContain("onClick={() => void refreshTenant(f.requestedTenant)}");
  });

  it("groups a merged read by device AND tenant", () => {
    expect(SOURCE).toContain("groupByDevice(visible, { byTenant: merged })");
  });

  it("reads a row's degraded flags off its OWN tenant's envelope", () => {
    expect(SOURCE).toContain(
      "envelopes.get(s.servedTenantId ?? null)?.deviceIdentityColumnsPresent",
    );
  });

  it("counts devices, not groups, and names the tenant count of a merged read", () => {
    expect(SOURCE).toContain("devices: devicesShown,");
    expect(SOURCE).toContain("tenants: merged ? servedTenants.length : undefined,");
  });
});
