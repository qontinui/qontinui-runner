import { beforeEach, describe, expect, it, vi } from "vitest";

// Which route served a read is the point of these tests: the native command
// first, `navigator.clipboard` only when the native command fails.
const native: { value: string; error: Error | null } = { value: "", error: null };
const browser: { value: string; error: Error | null } = { value: "", error: null };
const calls: string[] = [];

vi.mock("@tauri-apps/api/core", () => ({
  invoke: async (cmd: string) => {
    calls.push(`invoke ${cmd}`);
    if (native.error) throw native.error;
    return native.value;
  },
}));
vi.stubGlobal("navigator", {
  clipboard: {
    readText: async () => {
      calls.push("navigator.readText");
      if (browser.error) throw browser.error;
      return browser.value;
    },
  },
});

const { readClipboard } = await import("./clipboard");

beforeEach(() => {
  native.value = "";
  native.error = null;
  browser.value = "";
  browser.error = null;
  calls.length = 0;
});

describe("readClipboard", () => {
  it("reads natively and never touches navigator.clipboard when that succeeds", async () => {
    native.value = "from native";
    browser.value = "from browser";
    await expect(readClipboard()).resolves.toBe("from native");
    expect(calls).toEqual(["invoke clipboard_read_text"]);
  });

  it("falls back to navigator.clipboard when the native read fails", async () => {
    native.error = new Error("command unavailable");
    browser.value = "from browser";
    await expect(readClipboard()).resolves.toBe("from browser");
    expect(calls).toEqual(["invoke clipboard_read_text", "navigator.readText"]);
  });

  it("resolves an empty native clipboard as '' without falling back", async () => {
    browser.value = "stale browser text";
    await expect(readClipboard()).resolves.toBe("");
    expect(calls).toEqual(["invoke clipboard_read_text"]);
  });

  it("rejects, naming both causes, when both reads fail", async () => {
    native.error = new Error("Clipboard read failed: x11 timeout");
    browser.error = new Error("Document is not focused.");
    await expect(readClipboard()).rejects.toThrow(/x11 timeout.*Document is not focused\./);
  });
});
