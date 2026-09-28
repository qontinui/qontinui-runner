/**
 * The observation read is bounded client-side: a stuck hop aborts instead of
 * holding the shared poller's in-flight slot forever.
 */

import { describe, it, expect, vi } from "vitest";

const tracedFetch = vi.fn();
vi.mock("@/lib/runner-api", () => ({
  getApiBase: () => "http://127.0.0.1:1",
  tracedFetch: (...args: unknown[]) => tracedFetch(...args),
}));

import { fetchSupervisorObservation } from "./useSupervisorObservation";

describe("fetchSupervisorObservation", () => {
  it("aborts a read that does not answer within the timeout", async () => {
    tracedFetch.mockImplementation(
      (_url: string, init?: RequestInit) =>
        new Promise((_resolve, reject) => {
          init?.signal?.addEventListener("abort", () => reject(new Error("aborted")));
        }),
    );
    await expect(fetchSupervisorObservation(20)).rejects.toThrow("aborted");
  });

  it("returns the envelope's data on success", async () => {
    const data = {
      observed: true,
      probed_at: "t",
      port: 7,
      base_url: "http://127.0.0.1:7",
      reason: null,
    };
    tracedFetch.mockResolvedValue(
      new Response(JSON.stringify({ success: true, data }), { status: 200 }),
    );
    await expect(fetchSupervisorObservation()).resolves.toEqual(data);
  });
});
