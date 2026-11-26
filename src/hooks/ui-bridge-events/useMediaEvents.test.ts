/**
 * useMediaEvents tests: `image_diff` is the one live media case.
 *
 * runner#2242 deleted every other case in this hook and left `image_diff` as
 * the frontend half of the live `POST /ui-bridge/ai/image-diff`
 * (`mcp/ui_bridge/ai_analyze.rs`). Nothing pinned that split: without the
 * surviving case the route answers `Unknown request type: image_diff` with no
 * failing check, and a retired name restored here would read as a working
 * action.
 *
 * The vitest config runs under `environment: "node"` with no renderer, so
 * `useCallback` is stubbed to hand back the handler it was given.
 */

import { beforeEach, describe, expect, it, vi } from "vitest";

const compareVisualRegression = vi.hoisted(() => vi.fn());

vi.mock("react", async (importOriginal) => ({
  ...(await importOriginal<typeof import("react")>()),
  useCallback: <T>(fn: T) => fn,
}));
vi.mock("@qontinui/ui-bridge/ai", () => ({ compareVisualRegression }));

import type { UIBridgeRequestPayload, UIBridgeResponsePayload } from "./types";
import { useMediaEvents } from "./useMediaEvents";

function setup() {
  const responses: UIBridgeResponsePayload[] = [];
  // eslint-disable-next-line react-hooks/rules-of-hooks -- no renderer here; useCallback is stubbed above
  const handle = useMediaEvents({
    sendResponse: async (response) => {
      responses.push(response);
    },
  });
  return { handle, responses };
}

function imageDiff(fields: Partial<UIBridgeRequestPayload>): UIBridgeRequestPayload {
  return { requestId: "req-1", type: "image_diff", ...fields } as UIBridgeRequestPayload;
}

describe("useMediaEvents", () => {
  beforeEach(() => {
    compareVisualRegression.mockReset();
  });

  describe("image_diff", () => {
    it("strips a data-URL prefix so the SDK does not prefix it twice", async () => {
      compareVisualRegression.mockResolvedValue({ match: true, diffPercentage: 0 });
      const { handle, responses } = setup();

      const handled = await handle(
        imageDiff({
          params: { a: "data:image/png;base64,AAAA", b: "BBBB", options: { blur: 2 } },
        }),
      );

      expect(handled).toBe(true);
      expect(compareVisualRegression).toHaveBeenCalledTimes(1);
      const [a, b, options] = compareVisualRegression.mock.calls[0];
      expect(a.data).toBe("AAAA");
      expect(b.data).toBe("BBBB");
      expect(options).toEqual({ blur: 2 });
      expect(responses).toHaveLength(1);
      expect(responses[0]).toMatchObject({
        requestId: "req-1",
        type: "image_diff",
        success: true,
        data: { match: true, diffPercentage: 0 },
      });
    });

    it("prefers params, the only field the live route sends, over a body", async () => {
      compareVisualRegression.mockResolvedValue({ match: true });
      const { handle } = setup();

      await handle(imageDiff({ params: { a: "PPPP", b: "PPPP" }, body: { a: "BODY", b: "BODY" } }));

      expect(compareVisualRegression.mock.calls[0][0].data).toBe("PPPP");
    });

    it("reads the images from the body when no params were sent", async () => {
      compareVisualRegression.mockResolvedValue({ match: false });
      const { handle, responses } = setup();

      await handle(imageDiff({ body: { a: "AAAA", b: "BBBB" } }));

      expect(compareVisualRegression.mock.calls[0][0].data).toBe("AAAA");
      expect(compareVisualRegression.mock.calls[0][2]).toEqual({});
      expect(responses[0]).toMatchObject({ success: true, data: { match: false } });
    });

    it.each([
      ["a", { b: "BBBB" }],
      ["b", { a: "AAAA" }],
    ])("refuses a request with no `%s` image without running the diff", async (_name, params) => {
      const { handle, responses } = setup();

      expect(await handle(imageDiff({ params }))).toBe(true);

      expect(compareVisualRegression).not.toHaveBeenCalled();
      expect(responses).toHaveLength(1);
      // Rust pairs a reply with its request by id, so a wrong id here would
      // leave the route waiting out its IPC timeout.
      expect(responses[0]).toMatchObject({
        requestId: "req-1",
        success: false,
        error: "image_diff requires both `a` and `b` base64 image strings",
      });
    });

    it("answers a failed diff itself, with the failure's message", async () => {
      compareVisualRegression.mockRejectedValue(new Error("decode failed"));
      const { handle, responses } = setup();

      expect(await handle(imageDiff({ params: { a: "AAAA", b: "BBBB" } }))).toBe(true);

      expect(responses).toHaveLength(1);
      expect(responses[0]).toMatchObject({
        requestId: "req-1",
        success: false,
        error: "decode failed",
      });
    });
  });

  // Returning false hands the request on down the chain in
  // `useUIBridgeEventHandler`, which answers `Unknown request type`; these
  // names have no route left to send them.
  it.each([
    "find_media",
    "media_audit",
    "capture_media_snapshot",
    "analyze_media",
    "capture_element_images",
    "get_element_images",
    "capture_single_element",
    "media_compare",
  ])("does not answer the retired %s", async (type) => {
    const { handle, responses } = setup();

    const handled = await handle({
      requestId: "req-1",
      type,
      params: { a: "AAAA", b: "BBBB" },
    } as unknown as UIBridgeRequestPayload);

    expect(handled).toBe(false);
    expect(responses).toEqual([]);
    expect(compareVisualRegression).not.toHaveBeenCalled();
  });
});
