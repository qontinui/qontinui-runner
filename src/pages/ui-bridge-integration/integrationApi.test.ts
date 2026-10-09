/**
 * integrationApi — request shape (URL, method, JSON body, signal) and the
 * "return the envelope, never swallow" contract each call site relies on.
 */

import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

vi.mock("@/lib/runner-api", () => ({ getApiBase: () => "http://runner.test" }));

import { cacheArchitectureSpec, readFile, readPageSource, writeHooks } from "./integrationApi";

const fetchMock = vi.fn();

function respondWith(body: unknown) {
  fetchMock.mockResolvedValueOnce({ json: async () => body });
}

function lastCall(): { url: string; init: RequestInit; body: unknown } {
  const [url, init] = fetchMock.mock.calls[fetchMock.mock.calls.length - 1] as [
    string,
    RequestInit,
  ];
  return { url, init, body: JSON.parse(init.body as string) };
}

beforeEach(() => {
  fetchMock.mockReset();
  vi.stubGlobal("fetch", fetchMock);
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("integrationApi requests", () => {
  it("readFile posts project_path + file_path to read-file", async () => {
    respondWith({ success: true, data: "contents" });
    const ctrl = new AbortController();
    const res = await readFile("/proj", "src/a.ts", ctrl.signal);
    const { url, init, body } = lastCall();
    expect(url).toBe("http://runner.test/ui-bridge/integration/read-file");
    expect(init.method).toBe("POST");
    expect(init.headers).toEqual({ "Content-Type": "application/json" });
    expect(init.signal).toBe(ctrl.signal);
    expect(body).toEqual({ project_path: "/proj", file_path: "src/a.ts" });
    expect(res).toEqual({ success: true, data: "contents" });
  });

  it("readPageSource posts component_path and max_depth (default 2)", async () => {
    respondWith({ success: true, data: { main_source: "x", imported_sources: [] } });
    await readPageSource({ projectPath: "/proj", componentPath: "src/pages/A.tsx" });
    let call = lastCall();
    expect(call.url).toBe("http://runner.test/ui-bridge/integration/read-page-source");
    expect(call.init.method).toBe("POST");
    expect(call.init.signal).toBeUndefined();
    expect(call.body).toEqual({
      project_path: "/proj",
      component_path: "src/pages/A.tsx",
      max_depth: 2,
    });

    respondWith({ success: false, error: "nope" });
    const res = await readPageSource({ projectPath: "/p", componentPath: "c.tsx", maxDepth: 4 });
    call = lastCall();
    expect(call.body).toEqual({ project_path: "/p", component_path: "c.tsx", max_depth: 4 });
    expect(res).toEqual({ success: false, error: "nope" });
  });

  it("writeHooks posts project_path + files to write-hooks", async () => {
    respondWith({ success: true, data: { success: true, files_written: ["a.ts"], warnings: [] } });
    const files = [
      { file_path: "a.ts", modification_type: "create_new" as const, new_content: "x" },
    ];
    const res = await writeHooks("/proj", files);
    const { url, init, body } = lastCall();
    expect(url).toBe("http://runner.test/ui-bridge/integration/write-hooks");
    expect(init.method).toBe("POST");
    expect(body).toEqual({ project_path: "/proj", files });
    expect(res.data?.files_written).toEqual(["a.ts"]);
  });

  it("cacheArchitectureSpec posts project_path + spec_json to cache-architecture-spec", async () => {
    respondWith({ success: true });
    await cacheArchitectureSpec("/proj", '{"a":1}');
    const { url, init, body } = lastCall();
    expect(url).toBe("http://runner.test/ui-bridge/integration/cache-architecture-spec");
    expect(init.method).toBe("POST");
    expect(body).toEqual({ project_path: "/proj", spec_json: '{"a":1}' });
  });
});

describe("integrationApi error contract", () => {
  it("returns a success:false envelope as-is rather than throwing", async () => {
    respondWith({ success: false, error: "File not found" });
    await expect(readFile("/proj", "missing.md")).resolves.toEqual({
      success: false,
      error: "File not found",
    });
  });

  it("rejects on a network failure so the call site's catch decides", async () => {
    fetchMock.mockRejectedValueOnce(new TypeError("Failed to fetch"));
    await expect(readFile("/proj", "a.ts")).rejects.toThrow("Failed to fetch");
  });

  it("rejects on an unparseable body", async () => {
    fetchMock.mockResolvedValueOnce({
      json: async () => {
        throw new SyntaxError("Unexpected token <");
      },
    });
    await expect(writeHooks("/proj", [])).rejects.toThrow(SyntaxError);
  });

  it("propagates an abort of the passed signal", async () => {
    const ctrl = new AbortController();
    fetchMock.mockImplementationOnce(
      (_url: string, init: RequestInit) =>
        new Promise((_resolve, reject) => {
          init.signal?.addEventListener("abort", () =>
            reject(new DOMException("The operation was aborted.", "AbortError")),
          );
        }),
    );
    const pending = readPageSource({ projectPath: "/p", componentPath: "c.tsx" }, ctrl.signal);
    ctrl.abort();
    await expect(pending).rejects.toMatchObject({ name: "AbortError" });
  });
});
