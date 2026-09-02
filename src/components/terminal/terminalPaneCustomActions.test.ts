/**
 * The four terminal-pane custom actions, driven with the PTY spied.
 *
 * These four are the sharpest surfaces in the app: their effect is bytes typed
 * into a live Claude / PowerShell session. Iteration 12 measured
 *
 *   - `writeToTerminal({text: {}})` typing the literal characters
 *     `[object Object]` into a live PTY and answering `{success: true,
 *     bytes: 15}`;
 *   - `pasteText({text: {}})` failing with `Er.replace is not a function` — a
 *     minified variable name shown to an operator;
 *
 * in BOTH copies of the block, because there were two hand-written copies. So
 * the assertions here are on the WRITE COUNT, not on the throw: a handler that
 * threw after writing is exactly the failure being fixed, and it threw too.
 */

import { describe, it, expect, vi } from "vitest";
import {
  buildTerminalPaneCustomActions,
  type TerminalPaneEffects,
} from "./terminalPaneCustomActions";
import { TERMINAL_EXITED, type TerminalWriteResult } from "./terminalWriteResult";

const DESCRIPTIONS = {
  sendKeys: "send keys",
  writeToTerminal: "write",
  pasteText: "paste",
  getScrollback: "scrollback",
};

function harness(overrides: Partial<TerminalPaneEffects> = {}) {
  const writePty = vi.fn(
    async (data: string): Promise<TerminalWriteResult> => ({
      success: true,
      bytes: data.length,
    }),
  );
  const readScrollback = vi.fn((maxLines: number) => `lines:${maxLines}`);
  const effects: TerminalPaneEffects = {
    writePty,
    bracketedPasteMode: () => false,
    readScrollback,
    ...overrides,
  };
  return {
    writePty,
    readScrollback,
    actions: buildTerminalPaneCustomActions(effects, DESCRIPTIONS),
  };
}

/** Bags no pane action can accept, whatever field it declares. */
const MALFORMED_BAGS: Array<[string, unknown]> = [
  ["a number", 5],
  ["a string", "zz"],
  ["an empty list", []],
  ["true", true],
  ["an undeclared key", { zzz: "x" }],
];

describe("terminal pane custom actions — every one refuses with an EMPTY wire", () => {
  for (const name of ["sendKeys", "writeToTerminal", "pasteText", "getScrollback"] as const) {
    it(`${name} writes nothing for a malformed bag`, async () => {
      for (const [label, bag] of MALFORMED_BAGS) {
        const { writePty, actions } = harness();
        await expect(
          Promise.resolve().then(() => actions[name].handler(bag)),
          `${name} / ${label}`,
        ).rejects.toThrow();
        expect(writePty, `${name} / ${label} reached the PTY`).not.toHaveBeenCalled();
      }
    });

    it(`${name} writes nothing for a non-scalar value on its own field`, async () => {
      const field = name === "sendKeys" ? "keys" : name === "getScrollback" ? "maxLines" : "text";
      // `keys` is the ONE declared structured field, so an object there is
      // refused by `toPtySequence` rather than by coercion — still before any
      // write, which is the property under test.
      for (const value of [{}, [], true]) {
        const { writePty, actions } = harness();
        await expect(
          Promise.resolve().then(() => actions[name].handler({ [field]: value })),
        ).rejects.toThrow();
        expect(writePty).not.toHaveBeenCalled();
      }
    });
  }
});

describe("writeToTerminal", () => {
  it("types `[object Object]` into nothing (the iteration-12 defect)", async () => {
    const { writePty, actions } = harness();
    await expect(
      Promise.resolve().then(() => actions.writeToTerminal.handler({ text: {} })),
    ).rejects.toThrow('writeToTerminal: "text" must be text or a number (got an object)');
    expect(writePty).not.toHaveBeenCalled();
  });

  it("still writes real text, and reads a numeric-looking string back as text", async () => {
    const { writePty, actions } = harness();
    await actions.writeToTerminal.handler({ text: "echo hi\r" });
    expect(writePty).toHaveBeenCalledWith("echo hi\r");
    // Binding coerces `"5"` to the number 5; `textArg` restores the five the
    // caller actually sent, rather than refusing a legal command.
    await actions.writeToTerminal.handler({ text: "5" });
    expect(writePty).toHaveBeenLastCalledWith("5");
  });

  it("still refuses a missing text with its own sentence", async () => {
    const { writePty, actions } = harness();
    await expect(Promise.resolve().then(() => actions.writeToTerminal.handler({}))).rejects.toThrow(
      "writeToTerminal: 'text' is required",
    );
    expect(writePty).not.toHaveBeenCalled();
  });

  it("does NOT answer success for a write that reached no process", async () => {
    const { actions } = harness({
      writePty: async () => ({
        success: false,
        code: TERMINAL_EXITED,
        error: "gone",
        hint: "restart it",
        terminalId: "t1",
        exitCode: 1,
      }),
    });
    await expect(
      Promise.resolve().then(() => actions.writeToTerminal.handler({ text: "x" })),
    ).rejects.toThrow("gone restart it");
  });
});

describe("pasteText", () => {
  it("refuses a non-scalar with a TYPED message, not a minified variable name", async () => {
    const { writePty, actions } = harness();
    const err = await Promise.resolve()
      .then(() => actions.pasteText.handler({ text: {} }))
      .catch((e: unknown) => e as Error);
    expect(err).toBeInstanceOf(Error);
    expect((err as Error).message).toBe(
      'pasteText: "text" must be text or a number (got an object)',
    );
    // The literal failure iteration 12 recorded.
    expect((err as Error).message).not.toMatch(/\.replace is not a function/);
    expect(writePty).not.toHaveBeenCalled();
  });

  it("reads bracketed-paste mode at INVOCATION, not at registration", async () => {
    let bracketed = false;
    const { writePty, actions } = harness({ bracketedPasteMode: () => bracketed });
    await actions.pasteText.handler({ text: "hi" });
    const unbracketed = writePty.mock.calls[0][0];
    bracketed = true;
    await actions.pasteText.handler({ text: "hi" });
    const wrapped = writePty.mock.calls[1][0];
    // The flag flips whenever the foreground program changes; capturing it at
    // registration would paste in the wrong mode for the rest of the session.
    expect(wrapped).not.toBe(unbracketed);
    expect(wrapped).toContain("hi");
  });
});

describe("sendKeys", () => {
  it("still serves all three grammars", async () => {
    const { writePty, actions } = harness();
    await actions.sendKeys.handler({ keys: "ls\r" });
    await actions.sendKeys.handler({ keys: ["Enter"] });
    await actions.sendKeys.handler({ keys: [{ key: "c", modifiers: { ctrl: true } }] });
    expect(writePty.mock.calls.map((c) => c[0])).toEqual(["ls\r", "\r", "\x03"]);
  });

  it("refuses an untranslatable key rather than typing its name", async () => {
    const { writePty, actions } = harness();
    await expect(
      Promise.resolve().then(() => actions.sendKeys.handler({ keys: ["Enterr"] })),
    ).rejects.toThrow(/SEND_KEYS_INVALID/);
    expect(writePty).not.toHaveBeenCalled();
  });

  it("refuses an undeclared sibling key even when `keys` itself is fine", async () => {
    // The half `toPtySequence` could never have caught: it validates a VALUE,
    // and an undeclared key is a property of the BAG.
    const { writePty, actions } = harness();
    await expect(
      Promise.resolve().then(() => actions.sendKeys.handler({ keys: "ls\r", zzz: "x" })),
    ).rejects.toThrow('sendKeys: takes no argument named "zzz"');
    expect(writePty).not.toHaveBeenCalled();
  });
});

describe("getScrollback", () => {
  it("defaults to 500 and honours a supplied bound", async () => {
    const { readScrollback, actions } = harness();
    expect(await actions.getScrollback.handler({})).toBe("lines:500");
    expect(await actions.getScrollback.handler({ maxLines: 12 })).toBe("lines:12");
    expect(readScrollback.mock.calls.map((c) => c[0])).toEqual([500, 12]);
  });

  it("refuses an unusable bound rather than silently reading 500", async () => {
    // A caller who asked for `maxLines: "lots"` and silently got 500 cannot
    // tell that their bound was ignored.
    const { readScrollback, actions } = harness();
    for (const bad of ["lots", 0, -3, 1.5]) {
      await expect(
        Promise.resolve().then(() => actions.getScrollback.handler({ maxLines: bad })),
      ).rejects.toThrow(/must be a positive whole number|must be text or a number/);
    }
    expect(readScrollback).not.toHaveBeenCalled();
  });
});
