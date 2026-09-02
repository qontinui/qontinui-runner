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

/** Decode the UTF-8 bytes handed to `terminal_write` on the Nth invoke. */
function writtenText(callIndex = 0): string {
  const call = invoke.mock.calls[callIndex] as unknown as [string, { data: string }];
  expect(call[0]).toBe("terminal_write");
  const binary = atob(call[1].data);
  return new TextDecoder().decode(Uint8Array.from(binary, (c) => c.charCodeAt(0)));
}

interface ElementLike {
  type: unknown;
  props: Record<string, unknown> & { children?: unknown };
}

/**
 * Render the host and run the inner proxy for `terminalId`, returning its
 * `sendKeys` handler.
 *
 * Walks the element tree by hand because there is no renderer: the host's
 * children are `{ type: TerminalBridgeProxy, props }` pairs, and calling the
 * type with its props runs the child's body and (through the shim) its effects.
 */
function sendKeysHandlerFor(
  tabs: Parameters<typeof TerminalBridgeProxies>[0]["tabs"],
  terminalId: string,
): (params?: unknown) => Promise<unknown> {
  const host = (TerminalBridgeProxies as unknown as (p: { tabs: typeof tabs }) => ElementLike)({
    tabs,
  });
  const children = (host.props.children as ElementLike[]) ?? [];
  const child = children.find((c) => c.props.terminalId === terminalId);
  if (!child) throw new Error(`no proxy rendered for ${terminalId}`);
  (child.type as (p: unknown) => unknown)(child.props);
  const opts = attached.find((a) => a.elementId === `terminal-input-${terminalId}`);
  if (!opts) throw new Error(`proxy for ${terminalId} never attached`);
  const descriptor = opts.buildDescriptor() as {
    customActions: Record<string, { handler: (params?: unknown) => Promise<unknown> }>;
  };
  return descriptor.customActions.sendKeys.handler;
}

const LIVE_TABS = [{ id: "term-live", title: "PowerShell", isAlive: true, exitCode: null }];

beforeEach(() => {
  attached.length = 0;
  invoke.mockClear();
});

describe("TerminalBridgeProxies sendKeys — translates on the PROXY path", () => {
  it("registers the proxy under the mount-independent element id", () => {
    sendKeysHandlerFor(LIVE_TABS, "term-live");
    expect(attached.map((a) => a.elementId)).toEqual(["terminal-input-term-live"]);
    // The label is how an operator tells a proxy registration from a mounted
    // one in `getAllElements()`; the whole defect was that the two diverged.
    const label = (attached[0].buildDescriptor() as { label: string }).label;
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

// ── divergence guard ──────────────────────────────────────────────────────
//
// The behavioral tests above prove THIS path translates. This one used to
// prove the two paths could not drift apart, by asserting that each file's
// source text mentions `toPtySequence(keys)`. That guard was true and
// insufficient, and iteration 12 measured exactly how: `writeToTerminal` and
// `pasteText` sit in the same two blocks, were never translated in EITHER
// copy, and a per-handler text assertion for one handler says nothing about
// its neighbours. Two copies policed handler-by-handler is a guard whose
// coverage has to be extended by hand every time a handler is added — the
// shape that has now failed eleven times in this loop.
//
// So the divergence is closed structurally instead: there is ONE definition
// (`terminalPaneCustomActions.ts`), both components consume it, and neither
// may build a pane custom action of its own. A new handler is automatically
// covered because there is nowhere else to write one.
describe("both pane paths consume ONE definition of the custom actions", () => {
  it.each([["TerminalInstance.tsx"], ["TerminalBridgeProxies.tsx"]])(
    "%s builds its customActions from the shared factory",
    (file) => {
      const source = readFileSync(resolve(__dirname, file), "utf8");
      expect(source).toContain('from "./terminalPaneCustomActions"');
      expect(source).toMatch(/customActions:[\s\S]{0,200}buildTerminalPaneCustomActions\(/);
    },
  );

  it("neither path declares a pane custom action of its own", () => {
    // `paste` is the one exception and it is named here rather than pattern-
    // matched away: it reads `navigator.clipboard`, which only a mounted pane
    // may do, and it takes no parameters at all.
    const MOUNTED_ONLY = new Set(["paste"]);
    for (const file of ["TerminalInstance.tsx", "TerminalBridgeProxies.tsx"]) {
      const source = readFileSync(resolve(__dirname, file), "utf8");
      const declared = [...source.matchAll(/^\s{6,}(\w+):\s*\{\s*$\n\s+id:\s*"(\w+)"/gm)].map(
        (m) => m[2],
      );
      expect(declared.filter((id) => !MOUNTED_ONLY.has(id))).toEqual([]);
    }
  });

  it("the shared factory is the only place a keys payload meets a PTY write", () => {
    const shared = readFileSync(resolve(__dirname, "terminalPaneCustomActions.ts"), "utf8");
    expect(shared).toContain('from "./terminalKeySequence"');
    expect(shared).toMatch(/toPtySequence\(args\.keys\)/);
    for (const file of ["TerminalInstance.tsx", "TerminalBridgeProxies.tsx"]) {
      const source = readFileSync(resolve(__dirname, file), "utf8");
      // `writePtyById(id, keys, …)` / `writePty(keys)` — the untranslated form.
      expect(source).not.toMatch(/writePty(ById)?\((?:[^)]*,\s*)?keys\s*[,)]/);
      expect(source).not.toContain("toPtySequence");
    }
  });
});
