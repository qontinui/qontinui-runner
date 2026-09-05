/**
 * Unit tests for the one module that names `terminal_get_scrollback`: the
 * local ring reader every `ITerminalBackend.readScrollbackRing` delegates to,
 * and the two mount-independent consumers call directly.
 *
 * The invoker is injected, so no Tauri IPC is exercised (same precedent as
 * `resumeVerification.test.ts`'s `readTail`).
 */

import { describe, it, expect, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import {
  LOCAL_SCROLLBACK_RING_COMMAND,
  decodeRingBytes,
  readLocalScrollbackRing,
} from "./localScrollbackRing";

/** Base64 of the given latin-1 string — what the runner puts in `data.data`. */
function b64(s: string): string {
  return Buffer.from(s, "latin1").toString("base64");
}

/** A scripted runner answer: the exact `CommandResponse` shape the command emits. */
function ringResponse(payload: string, startOffset: number) {
  return {
    success: true,
    message: null,
    data: { data: b64(payload), startOffset, endOffset: startOffset + payload.length },
  };
}

describe("decodeRingBytes", () => {
  it("round-trips arbitrary bytes, including ESC and high bytes", () => {
    const original = Uint8Array.from([0x1b, 0x5b, 0x33, 0x31, 0x6d, 0xc3, 0xa9, 0x00, 0xff]);
    const encoded = Buffer.from(original).toString("base64");
    expect(Array.from(decodeRingBytes(encoded))).toEqual(Array.from(original));
  });

  it("decodes the empty payload to an empty array", () => {
    expect(decodeRingBytes("").length).toBe(0);
  });
});

describe("readLocalScrollbackRing", () => {
  it("issues exactly the local command, addressed by terminalId", async () => {
    const invoker = vi.fn(async () => ringResponse("hello", 0));
    await readLocalScrollbackRing("term-7", invoker);
    expect(invoker).toHaveBeenCalledTimes(1);
    expect(invoker).toHaveBeenCalledWith(LOCAL_SCROLLBACK_RING_COMMAND, { terminalId: "term-7" });
    expect(LOCAL_SCROLLBACK_RING_COMMAND).toBe("terminal_get_scrollback");
  });

  it("returns the decoded bytes with the stream offsets the runner reported", async () => {
    const payload = "\x1b[31mred\x1b[0m\r\n";
    const invoker = vi.fn(async () => ringResponse(payload, 1000));
    const ring = await readLocalScrollbackRing("t", invoker);
    expect(ring).not.toBeNull();
    expect(new TextDecoder().decode(ring!.bytes)).toBe(payload);
    expect(ring!.startOffset).toBe(1000);
    expect(ring!.endOffset).toBe(1000 + payload.length);
    // The invariant scrollbackReplay's offset math leans on.
    expect(ring!.endOffset - ring!.startOffset).toBe(ring!.bytes.length);
  });

  it("returns an empty window (not null) for an empty ring", async () => {
    const invoker = vi.fn(async () => ringResponse("", 42));
    const ring = await readLocalScrollbackRing("t", invoker);
    expect(ring).toEqual({ bytes: new Uint8Array(0), startOffset: 42, endOffset: 42 });
  });

  it("returns null when the runner answers success:false", async () => {
    const invoker = vi.fn(async () => ({ success: false, message: "gone", data: null }));
    expect(await readLocalScrollbackRing("t", invoker)).toBeNull();
  });

  it("returns null when the runner answers with no ring data", async () => {
    const invoker = vi.fn(async () => ({ success: true, message: null, data: null }));
    expect(await readLocalScrollbackRing("t", invoker)).toBeNull();
  });

  it("returns null when the payload is not a string", async () => {
    const invoker = vi.fn(async () => ({
      success: true,
      message: null,
      data: { data: 17, startOffset: 0, endOffset: 0 },
    }));
    expect(await readLocalScrollbackRing("t", invoker)).toBeNull();
  });

  it("returns null when the IPC resolves to nothing at all (a bare mock)", async () => {
    const invoker = vi.fn(async () => undefined);
    expect(await readLocalScrollbackRing("t", invoker)).toBeNull();
  });

  it("propagates a transport rejection so callers can tell 'no answer' from 'no ring'", async () => {
    const invoker = vi.fn(async () => {
      throw new Error("Terminal not found: t");
    });
    await expect(readLocalScrollbackRing("t", invoker)).rejects.toThrow("Terminal not found");
  });
});
