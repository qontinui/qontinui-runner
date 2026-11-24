import { describe, it, expect, vi, beforeEach } from "vitest";

const readFileMock = vi.fn();
vi.mock("../../integrationApi", () => ({ readFile: (...a: unknown[]) => readFileMock(...a) }));

import { pageRegistrationsStep } from "./pageRegistrations";
import {
  NO_PAGE_OPTIONS,
  aiMessage,
  appliedFiles,
  appliedStatuses,
  contentOf,
  currentPage,
  flushAsync,
  page,
  stubContext,
  userMessage,
} from "./stubContext.testutil";

const REG_TURN = aiMessage(
  [
    "```tsx",
    "// FILE: src/lib/ui-bridge/pages/settings-registrations.tsx",
    "export const regs = [];",
    "```",
  ].join("\n"),
);

beforeEach(() => readFileMock.mockReset());

describe("pageRegistrationsStep", () => {
  it("stores the files and registration output, then chains to the tutorial on the LIVE refs", () => {
    const ctx = stubContext();
    // Set after ctx is built: the handler must read them at call time.
    ctx.pageOptionsRef.current = { ...NO_PAGE_OPTIONS, generateTutorials: true };
    ctx.currentPageRef.current = currentPage();
    const messages = [userMessage("go"), aiMessage("earlier"), REG_TURN];

    pageRegistrationsStep(ctx, contentOf(messages));

    expect(ctx.processedMessageCountRef.current).toBe(2);
    expect(appliedFiles(ctx, []).map((f) => f.filePath)).toEqual([
      "src/lib/ui-bridge/pages/settings-registrations.tsx",
    ]);
    expect(ctx.currentPageRef.current!.registrationOutput).toContain("export const regs = [];");
    expect(ctx.pendingStepRef.current).toBe("page-tutorial");
    expect(ctx.setPhase).toHaveBeenCalledWith("generating-page-tutorial");
    expect(appliedStatuses(ctx, [{ state: "active", label: "Registrations: /x" }])).toEqual([
      { state: "done", label: "Registrations: /x" },
      { state: "active", label: "Tutorial: /dashboard/settings" },
    ]);
    expect(ctx.sendMessage.mock.calls[0][0]).toMatch(/^Now generate a tutorial for this page\./);
    expect(ctx.flow.advanceToNextPage).not.toHaveBeenCalled();
  });

  it("chains to page-spec, merging an existing spec read through readFile", async () => {
    readFileMock.mockResolvedValueOnce({ success: true, data: '{"existing":true}' });
    const ctx = stubContext({ projectPath: "/proj-a" });
    ctx.pageOptionsRef.current = { ...NO_PAGE_OPTIONS, generateSpecs: true };
    ctx.currentPageRef.current = currentPage({ page: page({ has_spec: true }) });

    pageRegistrationsStep(ctx, contentOf([REG_TURN]));
    expect(ctx.pendingStepRef.current).toBe("page-spec");
    expect(ctx.setPhase).toHaveBeenCalledWith("generating-page-spec");
    await flushAsync();

    expect(readFileMock).toHaveBeenCalledWith(
      "/proj-a",
      "src/specs/dashboard-settings.spec.uibridge.json",
      ctx.controller.signal,
    );
    expect(ctx.sendMessage).toHaveBeenCalledTimes(1);
    expect(ctx.sendMessage.mock.calls[0][0]).toMatch(/^Now generate a page spec/);
    expect(ctx.sendMessage.mock.calls[0][0]).toContain('{"existing":true}');
  });

  it("chains to the architecture diagram with the current page", () => {
    const ctx = stubContext();
    const cur = currentPage();
    ctx.pageOptionsRef.current = { ...NO_PAGE_OPTIONS, generateArchitectureDiagrams: true };
    ctx.currentPageRef.current = cur;

    pageRegistrationsStep(ctx, contentOf([REG_TURN]));

    expect(ctx.flow.chainToArchitectureDiagram).toHaveBeenCalledWith(cur);
  });

  it("chains to demo/tour planning with the page route", () => {
    const ctx = stubContext();
    ctx.pageOptionsRef.current = { ...NO_PAGE_OPTIONS, generateProductTours: true };
    ctx.currentPageRef.current = currentPage();

    pageRegistrationsStep(ctx, contentOf([REG_TURN]));

    expect(ctx.flow.chainToDemoScript).toHaveBeenCalledWith(
      "/dashboard/settings",
      ctx.controller.signal,
    );
  });

  it("with nothing else enabled, advances to the next page", () => {
    const ctx = stubContext();
    ctx.currentPageRef.current = currentPage();

    pageRegistrationsStep(ctx, contentOf([aiMessage("no files")]));

    expect(ctx.setGeneratedFiles).not.toHaveBeenCalled();
    expect(ctx.flow.advanceToNextPage).toHaveBeenCalledWith(ctx.controller.signal);
  });
});
