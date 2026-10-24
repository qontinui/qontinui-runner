import { describe, it, expect } from "vitest";
import { aiOutputOf } from "./aiOutput";

describe("aiOutputOf", () => {
  it("reads a successful reply's output log", () => {
    expect(aiOutputOf({ success: true, data: { output_log: "hello" } })).toEqual({
      log: "hello",
      historyUnavailable: false,
    });
  });

  it("keeps the live output a database outage still returns", () => {
    expect(
      aiOutputOf({
        success: false,
        message: "DATABASE_UNAVAILABLE: breaker open",
        data: { output_log: "live text", db_output_unavailable: true },
      }),
    ).toEqual({ log: "live text", historyUnavailable: true });
  });

  it("reports an outage with no live output as unavailable, not as an empty log", () => {
    expect(
      aiOutputOf({ success: false, data: { output_log: "", db_output_unavailable: true } }),
    ).toEqual({ log: null, historyUnavailable: true });
  });

  it("drops the log of any other failure", () => {
    expect(aiOutputOf({ success: false, data: { output_log: "stale" } })).toEqual({
      log: null,
      historyUnavailable: false,
    });
  });
});
