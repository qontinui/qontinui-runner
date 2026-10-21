import { describe, it, expect, vi } from "vitest";

import {
  closeEmptyTabsAction,
  reorganizeTabsAction,
  type TabBarCommandsContext,
} from "./useTabBarCommands";
import type { CommandResult, ResolverContext } from "./types";

const rctx = {} as ResolverContext;

function ctxWith(over: Partial<TabBarCommandsContext> = {}): TabBarCommandsContext {
  return {
    pageCount: 3,
    closeEmptyPages: vi.fn(async () => 2),
    openReorganize: vi.fn(),
    reorganizeOpen: false,
    ...over,
  };
}

const run = (a: { handler: (args: never, c: ResolverContext) => Promise<CommandResult> }) =>
  a.handler({} as never, rctx);

describe("/close-empty-tabs", () => {
  it("reports how many tabs were closed", async () => {
    const ctx = ctxWith();
    const res = await run(closeEmptyTabsAction(ctx));
    expect(res).toMatchObject({
      ok: true,
      value: { verb: "closed", noun: "empty tab", affected: 2 },
    });
    expect(ctx.closeEmptyPages).toHaveBeenCalledOnce();
  });

  it("reports zero (not success-with-nothing) when no tab was empty", async () => {
    const res = await run(closeEmptyTabsAction(ctxWith({ closeEmptyPages: async () => 0 })));
    expect(res).toMatchObject({ ok: true, value: { affected: 0 } });
  });

  it("fails without closing anything when there is only one tab", async () => {
    const ctx = ctxWith({ pageCount: 1 });
    const res = await run(closeEmptyTabsAction(ctx));
    expect(res).toMatchObject({ ok: false, code: "no-tabs" });
    expect(ctx.closeEmptyPages).not.toHaveBeenCalled();
  });

  it("surfaces a thrown close as a failure", async () => {
    const res = await run(
      closeEmptyTabsAction(
        ctxWith({
          closeEmptyPages: async () => {
            throw new Error("boom");
          },
        }),
      ),
    );
    expect(res).toMatchObject({ ok: false, code: "close-empty-failed" });
  });

  it("is declared destructive so a Tier-2 phrasing can't reroute into it", () => {
    expect(closeEmptyTabsAction(ctxWith()).destructive).toBe(true);
  });
});

describe("/reorganize-tabs", () => {
  it("opens the dialog", async () => {
    const ctx = ctxWith();
    const res = await run(reorganizeTabsAction(ctx));
    expect(ctx.openReorganize).toHaveBeenCalledOnce();
    expect(res).toMatchObject({ ok: true, value: { verb: "opened", affected: 1 } });
  });

  it("says so when it was already open", async () => {
    const res = await run(reorganizeTabsAction(ctxWith({ reorganizeOpen: true })));
    expect(res).toMatchObject({ ok: true, value: { affected: 0 } });
  });

  it("needs two tabs", async () => {
    const ctx = ctxWith({ pageCount: 1 });
    expect(await run(reorganizeTabsAction(ctx))).toMatchObject({ ok: false, code: "no-tabs" });
    expect(ctx.openReorganize).not.toHaveBeenCalled();
  });
});
