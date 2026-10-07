import { describe, expect, it } from "vitest";

import {
  fanoutPreviewRowId,
  fanoutStripMemberId,
  fanoutStripRunId,
  shortRunKeys,
  type PreviewRowPart,
  type StripMemberPart,
  type StripRunPart,
} from "./fanoutUiIds";

const ROW_PARTS: PreviewRowPart[] = ["", "tick", "expand", "prompt", "collisions"];
const RUN_PARTS: StripRunPart[] = [
  "run",
  "toggle",
  "age",
  "panel",
  "cap-decrease",
  "cap-value",
  "cap-increase",
  "cancel-queued",
  "note",
];
const MEMBER_PARTS: StripMemberPart[] = ["member", "release"];

describe("fan-out UI Bridge ids", () => {
  it("gives every preview row part an id of its own", () => {
    const ids = [0, 1, 2, 10].flatMap((i) => ROW_PARTS.map((p) => fanoutPreviewRowId(p, i)));
    expect(new Set(ids).size).toBe(ids.length);
    expect(fanoutPreviewRowId("", 3)).toBe("terminal.fanout-preview-row.3");
    expect(fanoutPreviewRowId("tick", 3)).toBe("terminal.fanout-preview-row-tick.3");
  });

  it("gives every run and every member of every run ids of their own", () => {
    const runIds = ["0a1b2c3d-1111-4000-8000-000000000001", "9f8e7d6c-2222-4000-8000-000000000002"];
    const keys = shortRunKeys(runIds);
    const ids = runIds.flatMap((id) => {
      const k = keys.get(id)!;
      return [
        ...RUN_PARTS.map((p) => fanoutStripRunId(p, k)),
        ...[0, 1, 11].flatMap((m) => MEMBER_PARTS.map((p) => fanoutStripMemberId(p, k, m))),
      ];
    });
    expect(new Set(ids).size).toBe(ids.length);
    expect(fanoutStripRunId("toggle", keys.get(runIds[0])!)).toBe(
      "terminal.fanout-strip-toggle.0a1b2c3d",
    );
    expect(fanoutStripMemberId("release", "0a1b2c3d", 4)).toBe(
      "terminal.fanout-strip-release.0a1b2c3d.4",
    );
  });

  it("never lets a part suffix collide with a bare container id", () => {
    // `terminal.fanout-strip` (the container) must not equal any run part id.
    expect(RUN_PARTS.map((p) => fanoutStripRunId(p, "abcd1234"))).not.toContain(
      "terminal.fanout-strip",
    );
  });

  it("grows a short run key past 8 characters only where two runs share a prefix", () => {
    const a = "abcdef12-0000-4000-8000-00000000000a";
    const b = "abcdef12-9999-4000-8000-00000000000b";
    const c = "12345678-0000-4000-8000-00000000000c";
    const keys = shortRunKeys([a, b, c]);
    expect(keys.get(c)).toBe("12345678");
    expect(keys.get(a)).not.toBe(keys.get(b));
    expect(keys.get(a)).toBe("abcdef12-0");
    expect(keys.get(b)).toBe("abcdef12-9");
    // A repeated id is one run, one key.
    expect(shortRunKeys([a, a]).get(a)).toBe("abcdef12");
  });
});
