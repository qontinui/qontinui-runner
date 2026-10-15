import { beforeEach, describe, expect, it } from "vitest";

import {
  noteDecode,
  noteOutputEvent,
  noteRingReplay,
  noteWriteRendered,
  transportClockStart,
  transportElapsedNs,
  transportStats,
} from "./transportStats";

beforeEach(() => {
  transportStats.enabled = true;
  transportStats.rawIpc = null;
  transportStats.reset();
});

describe("transportStats", () => {
  it("accumulates decode time, calls and bytes per site", () => {
    noteDecode("pane", transportClockStart(), 100);
    noteDecode("tap", transportClockStart(), 100);
    noteDecode("tap", transportClockStart(), 50);
    expect(transportStats.decodeCalls).toEqual({ pane: 1, tap: 2 });
    expect(transportStats.decodeBytes).toEqual({ pane: 100, tap: 150 });
    expect(transportStats.decodeNs.pane).toBeGreaterThanOrEqual(0);
  });

  it("accumulates write->render intervals, events and ring replays", () => {
    noteWriteRendered(transportClockStart(), 4096);
    noteOutputEvent(false);
    noteOutputEvent(true);
    noteRingReplay(1_048_576, 2048);
    expect(transportStats.writeToRenderCount).toBe(1);
    expect(transportStats.writeToRenderBytes).toBe(4096);
    expect(transportStats.eventsDelivered).toBe(2);
    expect(transportStats.eventsForeign).toBe(1);
    expect(transportStats.ringReplay).toEqual({
      fetches: 1,
      bytesFetched: 1_048_576,
      bytesWritten: 2048,
    });
  });

  it("skips the clock entirely while disabled, but still counts", () => {
    transportStats.enabled = false;
    const start = transportClockStart();
    expect(start).toBe(0);
    expect(transportElapsedNs(start)).toBe(0);
    noteDecode("pane", start, 10);
    expect(transportStats.decodeNs.pane).toBe(0);
    expect(transportStats.decodeCalls.pane).toBe(1);
  });

  it("measures elapsed time in nanoseconds", async () => {
    const start = transportClockStart();
    await new Promise((r) => setTimeout(r, 5));
    expect(transportElapsedNs(start)).toBeGreaterThan(1_000_000);
  });

  it("reset zeroes every counter but keeps enabled and rawIpc", () => {
    noteDecode("pane", transportClockStart(), 1);
    noteOutputEvent(true);
    noteRingReplay(1, 1);
    transportStats.rawIpc = true;
    transportStats.enabled = false;
    transportStats.reset();
    expect(transportStats.decodeCalls.pane).toBe(0);
    expect(transportStats.eventsForeign).toBe(0);
    expect(transportStats.ringReplay.fetches).toBe(0);
    expect(transportStats.rawIpc).toBe(true);
    expect(transportStats.enabled).toBe(false);
  });

  it("starts with rawIpc null — Phase 4 fills it", () => {
    expect(transportStats.rawIpc).toBeNull();
  });
});
