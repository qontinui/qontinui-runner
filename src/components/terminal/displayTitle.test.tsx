import { describe, it, expect } from "vitest";
import { resolveDisplayTitle, isPathShapedTitle } from "./displayTitle";
import { applySpawnNames, renameTabIn, spawnNameOf, type TerminalTab } from "./useTerminalManager";

const base = { title: "Terminal 1", workingDir: "/home/u/proj" };

describe("resolveDisplayTitle precedence", () => {
  it("rung 1: registry name wins over spawnName and title", () => {
    const names = new Map([["sid-1", "my-rename"]]);
    expect(
      resolveDisplayTitle({
        tab: { ...base, claudeSessionId: "sid-1", spawnName: "post-merge-runner#1" },
        registryNames: names,
      }),
    ).toBe("my-rename");
  });

  it("rung 1: /rename flows through the registry map", () => {
    const tab = { ...base, claudeSessionId: "sid-1", spawnName: "impl-x" };
    expect(resolveDisplayTitle({ tab, registryNames: new Map([["sid-1", "a"]]) })).toBe("a");
    expect(resolveDisplayTitle({ tab, registryNames: new Map([["sid-1", "b"]]) })).toBe("b");
    expect(resolveDisplayTitle({ tab, registryNames: new Map() })).toBe("impl-x");
  });

  it("registry miss for other session ids falls to spawnName", () => {
    expect(
      resolveDisplayTitle({
        tab: { ...base, claudeSessionId: "sid-1", spawnName: "impl-x" },
        registryNames: new Map([["other", "zzz"]]),
      }),
    ).toBe("impl-x");
  });

  it("rung 2: spawnName beats a non-path title", () => {
    expect(resolveDisplayTitle({ tab: { ...base, spawnName: "loop-a" } })).toBe("loop-a");
  });

  it("rung 3: plain title is used", () => {
    expect(resolveDisplayTitle({ tab: { ...base, title: "Terminal 3" } })).toBe("Terminal 3");
    expect(resolveDisplayTitle({ tab: { title: "worker:abc12345" } })).toBe("worker:abc12345");
  });

  it("rung 4: fallback", () => {
    expect(resolveDisplayTitle({ tab: { title: "" } })).toBe("Terminal");
    expect(resolveDisplayTitle({ tab: { title: "   " } })).toBe("Terminal");
  });

  it("blank registry / spawn names are skipped", () => {
    expect(
      resolveDisplayTitle({
        tab: { ...base, claudeSessionId: "s", spawnName: "  " },
        registryNames: new Map([["s", " "]]),
      }),
    ).toBe("Terminal 1");
  });
});

describe("path-shaped titles never render", () => {
  const cases: Array<[string, string | undefined]> = [
    ["/home/u/x", undefined],
    ["C:\\work\\x", undefined],
    ["~/proj", undefined],
    ["C:", undefined],
    ["C:\\", undefined],
    ["~", undefined],
    ["myproj", "myproj"], // equals workingDir
  ];
  it.each(cases)("%s", (title, workingDir) => {
    expect(isPathShapedTitle(title, workingDir)).toBe(true);
    expect(resolveDisplayTitle({ tab: { title, workingDir } })).toBe("Terminal");
  });

  it("spawnName survives an OSC title rewrite to the cwd", () => {
    expect(
      resolveDisplayTitle({
        tab: { title: "/home/u/proj", workingDir: "/home/u/proj", spawnName: "impl-foo" },
      }),
    ).toBe("impl-foo");
  });
});

describe("spawnName immutability", () => {
  it("renameTab does not touch spawnName", () => {
    const tabs = [
      { id: "t", title: "a", spawnName: "impl-foo", pid: null, isAlive: true, exitCode: null },
    ] as TerminalTab[];
    const next = renameTabIn(tabs, "t", "/home/u/proj");
    expect(next[0].title).toBe("/home/u/proj");
    expect(next[0].spawnName).toBe("impl-foo");
  });
});

describe("spawnNameOf", () => {
  it("reads spawnName defensively", () => {
    expect(spawnNameOf({ spawnName: " impl-x " })).toBe("impl-x");
    expect(spawnNameOf({})).toBeUndefined();
    expect(spawnNameOf({ spawnName: 5 })).toBeUndefined();
    expect(spawnNameOf(null)).toBeUndefined();
  });
});

describe("isPathShapedTitle: shell prompt titles", () => {
  it("treats the OSC `user@host: <cwd>` form as a path, including the home dir", () => {
    for (const title of [
      "spinak@merytshost: ~",
      "spinak@merytshost:~",
      "spinak@merytshost: /tmp",
      "root@box: ~/proj",
    ]) {
      expect(isPathShapedTitle(title), title).toBe(true);
    }
  });

  it("does not swallow a real session name that merely contains an @", () => {
    for (const title of ["post-merge-runner#1863", "fix a@b bug", "titlebar-demo"]) {
      expect(isPathShapedTitle(title), title).toBe(false);
    }
  });

  it("renders Terminal, not the prompt title, for a shell in the home directory", () => {
    expect(
      resolveDisplayTitle({
        tab: { title: "spinak@merytshost: ~", workingDir: "/home/spinak" },
      }),
    ).toBe("Terminal");
  });
});

describe("applySpawnNames", () => {
  const tab = (id: string, spawnName?: string) =>
    ({ id, title: "t", spawnName }) as unknown as TerminalTab;

  it("fills a missing spawn name from the map", () => {
    const out = applySpawnNames([tab("a")], { a: "post-merge-runner#1863" });
    expect(out[0].spawnName).toBe("post-merge-runner#1863");
  });

  it("never overwrites a name already held (immutable)", () => {
    const tabs = [tab("a", "first")];
    expect(applySpawnNames(tabs, { a: "second" })).toBe(tabs);
  });

  it("returns the same array when nothing changes", () => {
    const tabs = [tab("a"), tab("b")];
    expect(applySpawnNames(tabs, {})).toBe(tabs);
    expect(applySpawnNames(tabs, { z: "x" })).toBe(tabs);
    expect(applySpawnNames(tabs, { a: "   " })).toBe(tabs);
  });
});
