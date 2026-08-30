/**
 * `TerminalBridgeProxies` — the MOUNT-INDEPENDENT half of a terminal pane's UI
 * Bridge surface.
 *
 * ## Why this file exists (manual-test-loop iter 23, item 1)
 *
 * There was no test file for this component at all, and that absence is
 * precisely why the following shipped:
 *
 * Iteration 21 fixed `sendKeys` on the MOUNTED path (`TerminalInstance.tsx`) by
 * routing every `keys` grammar through `toPtySequence`. The PROXY path in this
 * component — added later, for panes in a virtualized flow-grid layout that
 * mount no `TerminalInstance` at all — kept handing the raw `keys` value to
 * `writePtyById`, whose `TextEncoder.encode` coerces anything non-string via
 * `String()`. Measured on a live runner against a proxy-owned pane:
 *
 *   | payload                        | proxy (wrong)                | mounted (right) |
 *   |--------------------------------|------------------------------|-----------------|
 *   | `{keys:["Enter"]}`             | `{"bytes":5,"success":true}` | `\r`            |
 *   | `{keys:[{"key":"Enter"}]}`     | `{"bytes":15,"success":true}`| `\r`            |
 *   | `{keys:["Enterr"]}`            | `{"bytes":6,"success":true}` | 400 SEND_KEYS_INVALID |
 *
 * The scrollback it left behind on a live PowerShell pane:
 * `PS C:\qontinui-root> EnterEnter[object Object]Enter[object Object]Enterr`.
 * Every one of those answered `success: true` with a byte count, because the
 * write genuinely reached the PTY — silent corruption of someone's real work,
 * reported green.
 *
 * ## How the component is exercised without a DOM
 *
 * The runner's vitest config is `environment: "node"` (no jsdom), so the
 * component cannot be mounted by a real renderer. It CAN be imported, though —
 * unlike `TerminalInstance` it pulls no xterm — so `react` is replaced by a
 * synchronous hook shim (`useEffect` runs inline, `memo` is identity) and the
 * returned element tree is walked by hand to invoke the inner proxy. That runs
 * the component's REAL effect, the REAL `buildDescriptor`, and the REAL
 * `sendKeys` handler; only the registry attachment and the Tauri IPC are
 * mocked. The assertions are therefore on the bytes that would reach the PTY —
 * the exact surface the defect corrupted — not on a source pattern.
 *
 * ## Iteration 24 additions
 *
 * Iteration 23 fixed ONE action (`sendKeys`) on this path. Iteration 24 found
 * that the same divergence had four more instances on the same descriptor —
 * coerced `text` (item 2), a `focus` that stole real focus (item 4), a missing
 * `paste` (item 5) and a hardcoded bracketed-paste `false` (item 6) — all made
 * observable by a proxy that never handed the id back to a remounted pane
 * (item 1). The suites below cover each, and the divergence guard at the bottom
 * is widened from `keys` to `text` so the same class cannot re-diverge a third
 * time.
 */

import { describe, it, expect, vi, beforeEach } from "vitest";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import type { SubordinateBridgeInputOptions } from "./subordinateBridgeRegistration";

// ── react: synchronous hook shim ──────────────────────────────────────────
// Only the four hooks this component uses are overridden; the rest of the real
// module is kept, because `react/jsx-runtime` reads React's internals through
// it and a four-export stub would break the element factory. Each test renders
// once, so a fresh object per `useRef` call is correct — there is no re-render
// to preserve identity across.
vi.mock("react", async (importOriginal) => {
  const actual = await importOriginal<typeof import("react")>();
  return {
    ...actual,
    memo: <T,>(fn: T) => fn,
    useEffect: (fn: () => void | (() => void)) => {
      fn();
    },
    useMemo: <T,>(fn: () => T) => fn(),
    useRef: <T,>(initial: T) => ({ current: initial }),
  };
});

vi.mock("@qontinui/ui-bridge", () => ({
  useUIBridgeOptional: () => ({ registry: { getElement: () => null } }),
}));

// `navigator.clipboard` does not exist under `environment: "node"`; the `paste`
// action (item 5) is defined in terms of it on BOTH paths.
const clipboardText = { value: "" };
vi.stubGlobal("navigator", {
  clipboard: { readText: async () => clipboardText.value },
});

const { attached, invoke } = vi.hoisted(() => ({
  attached: [] as SubordinateBridgeInputOptions[],
  // The IPC boundary. `writePtyById` resolves its default `invoker` from
  // `@tauri-apps/api/core` at call time, so mocking it captures exactly the
  // base64 payload the Rust `terminal_write` command would receive.
  invoke: vi.fn(async (_cmd: string, _args: Record<string, unknown>) => ({})),
}));

vi.mock("./subordinateBridgeRegistration", () => ({
  attachSubordinateBridgeInput: (opts: SubordinateBridgeInputOptions) => {
    attached.push(opts);
    return () => {};
  },
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args: Record<string, unknown>) => invoke(cmd, args),
}));

const { TerminalBridgeProxies } = await import("./TerminalBridgeProxies");

/**
 * Decode the UTF-8 bytes handed to the Nth `terminal_write`.
 *
 * Selects by COMMAND rather than by call index: since iteration 24 the paste
 * handlers first probe `terminal_get_bracketed_paste` (item 6), so the write is
 * no longer always `invoke.mock.calls[0]`.
 */
function writtenText(writeIndex = 0): string {
  const writes = invoke.mock.calls.filter(
    (c) => (c as unknown as [string])[0] === "terminal_write",
  ) as unknown as Array<[string, { data: string }]>;
  expect(writes.length).toBeGreaterThan(writeIndex);
  const binary = atob(writes[writeIndex][1].data);
  return new TextDecoder().decode(Uint8Array.from(binary, (c) => c.charCodeAt(0)));
}

/** Did anything at all reach the PTY? The load-bearing negative assertion. */
function ptyWriteCount(): number {
  return invoke.mock.calls.filter((c) => (c as unknown as [string])[0] === "terminal_write").length;
}

interface ElementLike {
  type: unknown;
  props: Record<string, unknown> & { children?: unknown };
}

interface ProxyDescriptor {
  label: string;
  actions: string[];
  customActions: Record<string, { handler: (params?: unknown) => Promise<unknown> }>;
}

/**
 * Render the host and run the inner proxy for `terminalId`, returning its
 * REAL descriptor.
 *
 * Walks the element tree by hand because there is no renderer: the host's
 * children are `{ type: TerminalBridgeProxy, props }` pairs, and calling the
 * type with its props runs the child's body and (through the shim) its effects.
 */
function descriptorFor(
  tabs: Parameters<typeof TerminalBridgeProxies>[0]["tabs"],
  terminalId: string,
): ProxyDescriptor {
  const host = (TerminalBridgeProxies as unknown as (p: { tabs: typeof tabs }) => ElementLike)({
    tabs,
  });
  const children = (host.props.children as ElementLike[]) ?? [];
  const child = children.find((c) => c.props.terminalId === terminalId);
  if (!child) throw new Error(`no proxy rendered for ${terminalId}`);
  (child.type as (p: unknown) => unknown)(child.props);
  const opts = attached.find((a) => a.elementId === `terminal-input-${terminalId}`);
  if (!opts) throw new Error(`proxy for ${terminalId} never attached`);
  return opts.buildDescriptor() as unknown as ProxyDescriptor;
}

/** The proxy's handler for one action name. */
function handlerFor(
  tabs: Parameters<typeof TerminalBridgeProxies>[0]["tabs"],
  terminalId: string,
  action: string,
): (params?: unknown) => Promise<unknown> {
  const descriptor = descriptorFor(tabs, terminalId);
  const entry = descriptor.customActions[action];
  if (!entry) throw new Error(`proxy for ${terminalId} advertises no '${action}'`);
  return entry.handler;
}

function sendKeysHandlerFor(
  tabs: Parameters<typeof TerminalBridgeProxies>[0]["tabs"],
  terminalId: string,
): (params?: unknown) => Promise<unknown> {
  return handlerFor(tabs, terminalId, "sendKeys");
}

const LIVE_TABS = [{ id: "term-live", title: "PowerShell", isAlive: true, exitCode: null }];

beforeEach(() => {
  attached.length = 0;
  invoke.mockClear();
  clipboardText.value = "";
  // Default the id-addressed bracketed-paste probe (item 6) to "off" so the
  // suites that do not care about it read like the pre-iteration-24 behaviour;
  // the item-6 suite overrides it per test.
  invoke.mockImplementation(async (cmd: string) =>
    cmd === "terminal_get_bracketed_paste" ? { data: { bracketedPaste: false } } : {},
  );
});

describe("TerminalBridgeProxies sendKeys — translates on the PROXY path", () => {
  it("registers the proxy under the mount-independent element id", () => {
    sendKeysHandlerFor(LIVE_TABS, "term-live");
    expect(attached.map((a) => a.elementId)).toEqual(["terminal-input-term-live"]);
    // The label is how an operator tells a proxy registration from a mounted
    // one in `getAllElements()`; the whole defect was that the two diverged.
    const label = (attached[0].buildDescriptor() as unknown as ProxyDescriptor).label;
    expect(label).toContain("[no mounted view");
  });

  // ── grammar 1: raw string, written verbatim ──────────────────────────────
  it("writes a raw string verbatim", async () => {
    const sendKeys = sendKeysHandlerFor(LIVE_TABS, "term-live");
    await sendKeys({ keys: "ls -la\r" });
    expect(writtenText()).toBe("ls -la\r");
  });

  // ── grammar 2: bare key names ────────────────────────────────────────────
  it('translates ["Enter"] to CR — not the literal text "Enter"', async () => {
    const sendKeys = sendKeysHandlerFor(LIVE_TABS, "term-live");
    await sendKeys({ keys: ["Enter"] });
    // The measured defect wrote 5 bytes of "Enter". One byte, and it is CR.
    expect(writtenText()).toBe("\r");
    expect(writtenText()).not.toBe("Enter");
  });

  it("translates a multi-key bare-name array", async () => {
    const sendKeys = sendKeysHandlerFor(LIVE_TABS, "term-live");
    await sendKeys({ keys: ["h", "i", "Enter"] });
    expect(writtenText()).toBe("hi\r");
  });

  // ── grammar 3: SDK descriptor array ──────────────────────────────────────
  it('translates [{key:"Enter"}] to CR — not "[object Object]"', async () => {
    const sendKeys = sendKeysHandlerFor(LIVE_TABS, "term-live");
    await sendKeys({ keys: [{ key: "Enter" }] });
    // The measured defect wrote 15 bytes of "[object Object]".
    expect(writtenText()).toBe("\r");
    expect(writtenText()).not.toContain("object Object");
  });

  // ── modified key ─────────────────────────────────────────────────────────
  it("translates Ctrl+C to the 0x03 interrupt byte", async () => {
    const sendKeys = sendKeysHandlerFor(LIVE_TABS, "term-live");
    await sendKeys({ keys: [{ key: "c", modifiers: { ctrl: true } }] });
    expect(writtenText()).toBe("\x03");
  });

  it("translates ArrowUp to the normal-cursor-mode CSI sequence", async () => {
    const sendKeys = sendKeysHandlerFor(LIVE_TABS, "term-live");
    await sendKeys({ keys: ["ArrowUp"] });
    expect(writtenText()).toBe("\x1b[A");
  });

  // ── negative controls: these MUST fail, and must reach no PTY ────────────
  it("throws SEND_KEYS_INVALID for an untranslatable key instead of typing its name", async () => {
    const sendKeys = sendKeysHandlerFor(LIVE_TABS, "term-live");
    await expect(sendKeys({ keys: ["Enterr"] })).rejects.toThrow("SEND_KEYS_INVALID");
    // The load-bearing half: nothing reached the PTY. The defect wrote the
    // 6 bytes "Enterr" into a live shell and reported success.
    expect(invoke).not.toHaveBeenCalled();
  });

  it("throws SEND_KEYS_INVALID for a missing keys param", async () => {
    const sendKeys = sendKeysHandlerFor(LIVE_TABS, "term-live");
    await expect(sendKeys({})).rejects.toThrow("SEND_KEYS_INVALID");
    expect(invoke).not.toHaveBeenCalled();
  });

  it("throws SEND_KEYS_INVALID for an empty array", async () => {
    const sendKeys = sendKeysHandlerFor(LIVE_TABS, "term-live");
    await expect(sendKeys({ keys: [] })).rejects.toThrow("SEND_KEYS_INVALID");
    expect(invoke).not.toHaveBeenCalled();
  });

  it("emits the CSI-modified form for Ctrl+ArrowUp and appends the next key", async () => {
    const sendKeys = sendKeysHandlerFor(LIVE_TABS, "term-live");
    await expect(
      sendKeys({ keys: [{ key: "ArrowUp", modifiers: { ctrl: true } }, { key: "1" }] }),
    ).resolves.toBeTruthy();
    // Ctrl+ArrowUp is the CSI-modified form; the "1" follows it verbatim.
    expect(writtenText()).toBe("\x1b[1;5A1");
  });

  // ── the exited-pane envelope still wins over translation ─────────────────
  it("refuses before the IPC when the pane's process is gone", async () => {
    const sendKeys = sendKeysHandlerFor(
      [{ id: "term-dead", title: "gone", isAlive: false, exitCode: 1 }],
      "term-dead",
    );
    await expect(sendKeys({ keys: ["Enter"] })).rejects.toThrow("TERMINAL_EXITED");
    expect(invoke).not.toHaveBeenCalled();
  });
});

describe("TerminalBridgeProxies — plan tabs claim no terminal id", () => {
  it("renders no proxy for a plan tab", () => {
    const host = (
      TerminalBridgeProxies as unknown as (p: { tabs: unknown }) => {
        props: { children?: ElementLike[] };
      }
    )({
      tabs: [
        { id: "plan-1", title: "a plan", type: "plan" },
        { id: "term-1", title: "a shell" },
      ],
    });
    expect((host.props.children ?? []).map((c) => c.props.terminalId)).toEqual(["term-1"]);
  });
});

describe("TerminalBridgeProxies writeToTerminal — typed, not coerced (iter 24, item 2)", () => {
  it("writes an ordinary string verbatim", async () => {
    const write = handlerFor(LIVE_TABS, "term-live", "writeToTerminal");
    await write({ text: "echo hi\r" });
    expect(writtenText()).toBe("echo hi\r");
  });

  // THE regression the item is about: `"0"` is falsy AND valid.
  it('accepts the falsy-but-valid string "0"', async () => {
    const write = handlerFor(LIVE_TABS, "term-live", "writeToTerminal");
    await expect(write({ text: "0" })).resolves.toBeTruthy();
    expect(writtenText()).toBe("0");
  });

  it.each([
    ["a number", 42],
    ["an object", { a: 1 }],
    ["an array", ["a", "b"]],
    ["a boolean", true],
    ["null", null],
  ])("throws WRITE_TEXT_INVALID for %s and writes NOTHING", async (_label, value) => {
    const write = handlerFor(LIVE_TABS, "term-live", "writeToTerminal");
    await expect(write({ text: value })).rejects.toThrow("WRITE_TEXT_INVALID");
    // The load-bearing half. The defect wrote `42` / `[object Object]` / `a,b`
    // into a LIVE shell and answered HTTP 200 with a byte count.
    expect(ptyWriteCount()).toBe(0);
  });

  it("throws WRITE_TEXT_INVALID for a missing text param", async () => {
    const write = handlerFor(LIVE_TABS, "term-live", "writeToTerminal");
    await expect(write({})).rejects.toThrow("WRITE_TEXT_INVALID");
    expect(ptyWriteCount()).toBe(0);
  });

  it("carries a machine-readable .code, which is what the SDK hoists", async () => {
    const write = handlerFor(LIVE_TABS, "term-live", "writeToTerminal");
    await expect(write({ text: 42 })).rejects.toMatchObject({ code: "WRITE_TEXT_INVALID" });
  });

  it("never leaks a minified internal identifier (item 3)", async () => {
    const write = handlerFor(LIVE_TABS, "term-live", "writeToTerminal");
    await expect(write({ text: 42 })).rejects.not.toThrow(/\.replace is not a function/);
  });
});

describe("TerminalBridgeProxies pasteText — typed, not coerced (iter 24, items 2 & 3)", () => {
  it.each([
    ["a number", 42],
    ["an object", { a: 1 }],
  ])("throws PASTE_TEXT_INVALID for %s and writes NOTHING", async (_label, value) => {
    const paste = handlerFor(LIVE_TABS, "term-live", "pasteText");
    await expect(paste({ text: value })).rejects.toThrow("PASTE_TEXT_INVALID");
    expect(ptyWriteCount()).toBe(0);
  });

  it("does not surface `Er.replace is not a function` (the minified leak)", async () => {
    const paste = handlerFor(LIVE_TABS, "term-live", "pasteText");
    const err = await paste({ text: 42 }).then(
      () => new Error("should have thrown"),
      (e: unknown) => e as Error,
    );
    expect(err.message).not.toMatch(/replace is not a function/);
    expect(err.message).toContain("PASTE_TEXT_INVALID");
    expect((err as Error & { code?: string }).code).toBe("PASTE_TEXT_INVALID");
  });

  it('accepts "0"', async () => {
    const paste = handlerFor(LIVE_TABS, "term-live", "pasteText");
    await expect(paste({ text: "0" })).resolves.toBeTruthy();
    expect(writtenText()).toBe("0");
  });
});

describe("TerminalBridgeProxies focus/blur — refuse, never steal (iter 24, item 4)", () => {
  it("advertises NO standard actions, so nothing can call element.focus()", () => {
    // `["focus", "blur"]` used to be here. The runner's Rust gate takes an
    // advertised action at its word and the SDK's `performFocus` is
    // unconditional, so a `focus` request moved REAL keyboard focus onto the
    // hidden 1×1 textarea and reported success.
    expect(descriptorFor(LIVE_TABS, "term-live").actions).toEqual([]);
  });

  it.each([["focus"], ["blur"]])("%s throws TERMINAL_NO_MOUNTED_VIEW", async (action) => {
    const handler = handlerFor(LIVE_TABS, "term-live", action);
    await expect(handler({})).rejects.toThrow("TERMINAL_NO_MOUNTED_VIEW");
  });

  it("the refusal carries a .code and touches no PTY", async () => {
    const handler = handlerFor(LIVE_TABS, "term-live", "focus");
    await expect(handler({})).rejects.toMatchObject({ code: "TERMINAL_NO_MOUNTED_VIEW" });
    expect(invoke).not.toHaveBeenCalled();
  });

  it("registers focus as a customAction, which SHADOWS the SDK built-in", () => {
    // Omitting it from `actions` alone would leave the outcome to two gates,
    // one of which (`isElementActionAllowed`) reads an EMPTY list as
    // permissive. A same-named customAction wins over the built-in outright.
    const custom = descriptorFor(LIVE_TABS, "term-live").customActions;
    expect(Object.keys(custom)).toEqual(expect.arrayContaining(["focus", "blur"]));
  });
});

describe("TerminalBridgeProxies paste — exists on this path too (iter 24, item 5)", () => {
  it("reads the clipboard and writes it to the PTY by id", async () => {
    clipboardText.value = "from-clipboard";
    const paste = handlerFor(LIVE_TABS, "term-live", "paste");
    await expect(paste()).resolves.toBeTruthy();
    expect(writtenText()).toBe("from-clipboard");
  });

  it("answers a zero-byte success on an empty clipboard, exactly as mounted does", async () => {
    clipboardText.value = "";
    const paste = handlerFor(LIVE_TABS, "term-live", "paste");
    await expect(paste()).resolves.toEqual({ success: true, bytes: 0 });
    expect(ptyWriteCount()).toBe(0);
  });

  it("refuses before the IPC when the pane's process is gone", async () => {
    clipboardText.value = "x";
    const paste = handlerFor(
      [{ id: "term-dead", title: "gone", isAlive: false, exitCode: 1 }],
      "term-dead",
      "paste",
    );
    await expect(paste()).rejects.toThrow("TERMINAL_EXITED");
    expect(ptyWriteCount()).toBe(0);
  });
});

describe("TerminalBridgeProxies pasteText — bracketed-paste read by id (item 6)", () => {
  it("wraps the envelope when the PTY has DEC 2004 enabled", async () => {
    invoke.mockImplementation(async (cmd: string) =>
      cmd === "terminal_get_bracketed_paste" ? { data: { bracketedPaste: true } } : {},
    );
    const paste = handlerFor(LIVE_TABS, "term-live", "pasteText");
    await paste({ text: "a\nb" });
    // Identical to what the MOUNTED path produces for the same input and the
    // same DEC 2004 state — `preparePasteData("a\nb", true)`.
    expect(writtenText()).toBe("\x1b[200~a\rb\x1b[201~");
  });

  it("sends it bare when the PTY has DEC 2004 disabled", async () => {
    invoke.mockImplementation(async (cmd: string) =>
      cmd === "terminal_get_bracketed_paste" ? { data: { bracketedPaste: false } } : {},
    );
    const paste = handlerFor(LIVE_TABS, "term-live", "pasteText");
    await paste({ text: "a\nb" });
    expect(writtenText()).toBe("a\rb");
  });

  it("asks the RUNNER for the state rather than assuming it", async () => {
    const paste = handlerFor(LIVE_TABS, "term-live", "pasteText");
    await paste({ text: "x" });
    expect(invoke).toHaveBeenCalledWith("terminal_get_bracketed_paste", {
      terminalId: "term-live",
    });
  });

  it("throws BRACKETED_PASTE_UNKNOWN rather than guessing, and writes nothing", async () => {
    invoke.mockImplementation(async (cmd: string) => {
      if (cmd === "terminal_get_bracketed_paste") throw new Error("Terminal not found");
      return {};
    });
    const paste = handlerFor(LIVE_TABS, "term-live", "pasteText");
    await expect(paste({ text: "x" })).rejects.toThrow("BRACKETED_PASTE_UNKNOWN");
    // Defaulting to `false` here is what the fix removed: it would silently
    // reinstate the divergence and report it green.
    expect(ptyWriteCount()).toBe(0);
  });

  it("throws rather than guessing when the probe answers no boolean", async () => {
    invoke.mockImplementation(async (cmd: string) =>
      cmd === "terminal_get_bracketed_paste" ? { data: {} } : {},
    );
    const paste = handlerFor(LIVE_TABS, "term-live", "pasteText");
    await expect(paste({ text: "x" })).rejects.toThrow("BRACKETED_PASTE_UNKNOWN");
    expect(ptyWriteCount()).toBe(0);
  });

  it("skips the probe for a dead pane so TERMINAL_EXITED stays the diagnosis", async () => {
    const paste = handlerFor(
      [{ id: "term-dead", title: "gone", isAlive: false, exitCode: 1 }],
      "term-dead",
      "pasteText",
    );
    await expect(paste({ text: "x" })).rejects.toThrow("TERMINAL_EXITED");
    expect(invoke).not.toHaveBeenCalled();
  });
});

// ── divergence guards ───────────────────────────────────────────
// The behavioral tests above prove THIS path is right. These prove the two
// paths cannot silently drift apart again: iteration 21 fixed one of them and
// the other was written afterwards without it, which is the whole defect — and
// iteration 24 found the same shape four more times on the SAME descriptor,
// because the iteration-23 guard was written for `keys` alone.
const PATHS = ["TerminalInstance.tsx", "TerminalBridgeProxies.tsx"] as const;

function sourceOf(file: string): string {
  return readFileSync(resolve(__dirname, file), "utf8");
}

describe("both sendKeys paths route through the same translator", () => {
  it.each(PATHS.map((f) => [f]))("%s calls toPtySequence on the keys payload", (file) => {
    const source = sourceOf(file);
    expect(source).toContain('from "./terminalKeySequence"');
    expect(source).toMatch(/toPtySequence\(keys\)/);
  });

  it("neither path hands a raw keys value to a PTY write", () => {
    for (const file of PATHS) {
      // `writePtyById(id, keys, …)` / `writePty(keys)` — the untranslated form.
      expect(sourceOf(file)).not.toMatch(/writePty(ById)?\((?:[^)]*,\s*)?keys\s*[,)]/);
    }
  });
});

describe("both text paths route through the same type guard (iter 24, item 2)", () => {
  it.each(PATHS.map((f) => [f]))("%s validates `text` with requireTextPayload", (file) => {
    const source = sourceOf(file);
    expect(source).toContain('from "./terminalTextPayload"');
    expect(source).toMatch(/requireTextPayload\(\s*text,\s*WRITE_TEXT_INVALID/);
    expect(source).toMatch(/requireTextPayload\(\s*text,\s*PASTE_TEXT_INVALID/);
  });

  it("neither path still uses the truthiness check that coerced non-strings", () => {
    for (const file of PATHS) {
      const source = sourceOf(file);
      // The exact shape of the defect: `if (!text) throw … 'text' is required`.
      // It let `{text: 42}` through to `String()` coercion AND rejected `"0"`.
      expect(source).not.toMatch(/if \(!text\) throw/);
      expect(source).not.toContain("'text' is required");
    }
  });

  it("neither path ASSERTS the automation payload is a string", () => {
    for (const file of PATHS) {
      const source = sourceOf(file);
      // `(params || {}) as { text?: string }` was the whole bug in one line: a
      // cast, not a check, over a value that arrives from an HTTP request.
      // Every such destructure must read `text?: unknown` and be narrowed by
      // `requireTextPayload` — which is what the assertions above pin.
      expect(source).not.toMatch(/params \|\| \{\}\) as \{ text\?: string \}/);
      expect(source).toMatch(/params \|\| \{\}\) as \{ text\?: unknown \}/);
    }
  });
});

describe("neither paste path hardcodes bracketed-paste state (iter 24, item 6)", () => {
  it("the proxy reads DEC 2004 by id instead of passing a literal false", () => {
    const source = sourceOf("TerminalBridgeProxies.tsx");
    expect(source).toContain('from "./bracketedPasteById"');
    // The literal that made one pasteText call produce two different byte
    // streams depending on whether the pane was scrolled into view.
    expect(source).not.toMatch(/preparePasteData\([^)]*,\s*false\s*\)/);
  });

  it("the mounted path reads it off the live backend", () => {
    expect(sourceOf("TerminalInstance.tsx")).toMatch(/bracketedPasteMode\s*\?\?\s*false/);
  });
});

describe("the id maps to the mounted node whenever one exists (iter 24, item 1)", () => {
  it("the mounted path announces its live view", () => {
    const source = sourceOf("TerminalInstance.tsx");
    expect(source).toContain('from "./mountedTerminalViews"');
    expect(source).toMatch(/registerMountedTerminalView\(/);
    // LIVENESS, not mere mounting: a pane mounts ~200ms before its backend
    // builds, and yielding during that window would answer ELEMENT_NOT_FOUND.
    expect(source).toMatch(/backendRef\.current\?\.getInputElement\(\)/);
  });

  it("the mounted path keeps a reclaim watchdog on the shared id", () => {
    // `attachBridgeInputRegistration` owns the watchdog; the guard here is that
    // the mounted path still routes through it rather than registering direct.
    expect(sourceOf("TerminalInstance.tsx")).toContain("attachBridgeInputRegistration({");
  });

  it("the proxy consults that record before claiming", () => {
    const source = sourceOf("TerminalBridgeProxies.tsx");
    expect(source).toContain('from "./mountedTerminalViews"');
    expect(source).toMatch(/shouldYield: \(\) => hasMountedTerminalView\(terminalId\)/);
  });
});

describe("the proxy advertises the same action surface as a mounted pane (item 5)", () => {
  // `getScrollback` is served from the PTY ring here and from the rendered
  // xterm buffer there — different SOURCE, same action, which is the point.
  const SHARED = ["sendKeys", "writeToTerminal", "paste", "pasteText", "getScrollback"] as const;

  it.each(SHARED.map((a) => [a]))("proxy advertises %s", (action) => {
    expect(Object.keys(descriptorFor(LIVE_TABS, "term-live").customActions)).toContain(action);
  });

  it.each(SHARED.map((a) => [a]))("the mounted path advertises %s too", (action) => {
    // `TerminalInstance` cannot be imported here (it pulls @xterm/addon-canvas,
    // which touches `self` at module init and crashes under `environment:
    // "node"`), so its descriptor is read from source. A capability that exists
    // on only one path is the defect — `paste` was mounted-only, so it appeared
    // and vanished as a pane scrolled through a virtualized flow grid.
    expect(sourceOf("TerminalInstance.tsx")).toMatch(
      new RegExp(`\\n\\s+${action}: \\{\\n\\s+id: "${action}"`),
    );
  });
});
