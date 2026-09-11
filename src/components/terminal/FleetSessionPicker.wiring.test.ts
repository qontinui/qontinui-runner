/**
 * Wiring invariants for `FleetSessionPicker` that no pure-function test can
 * reach.
 *
 * `fleetTruncation` takes `limit` as a free parameter, so it is structurally
 * incapable of noticing that a caller handed it the WRONG one — and the wrong
 * one is the easy mistake here, because the component holds two limits at once:
 * the filter it is currently requesting, and the one the rows on screen were
 * actually served for. Pairing the pending limit with the previous response
 * makes the banner state a falsehood ("coord truncated this read at 250" when
 * coord truncated it at 100) and skips a rung of the page ladder. A failed
 * refetch makes it permanent: `useFleetSessions` deliberately keeps the old
 * response and returns `loading` to false.
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

describe("everything said ABOUT the loaded rows is said with the query they came from", () => {
  it("classifies truncation with the APPLIED limit, never the pending one", () => {
    expect(SOURCE).toContain("fleetTruncation(response, appliedQuery?.limit");
    expect(SOURCE).not.toContain("fleetTruncation(response, server.limit)");
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

  it("routes the page control through loadMore, which retries an already-requested limit", () => {
    // A failed larger read leaves the banner describing the older, smaller
    // response while `server.limit` already holds the bigger number. A bare
    // `setServer` would then change nothing and the click would be inert.
    expect(SOURCE).toContain("loadMore(truncation.nextLimit)");
    expect(SOURCE).toContain("if (server.limit >= nextLimit) void refresh();");
  });
});
