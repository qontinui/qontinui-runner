// @vitest-environment jsdom
/**
 * A tutorial step's text as the user meets it (coord finding 34c8ccd6): its
 * `content` renders as markdown, and its `details` and `tips` reach the
 * screen in both tooltips. Before this, both tooltips printed `content` as
 * plain text and read neither of the other two fields.
 */

import { readFileSync } from "node:fs";
import { join } from "node:path";
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { TutorialStepBody } from "./TutorialStepBody";
import { TutorialTooltip } from "./TutorialTooltip";
import { multimodalVisionTutorial } from "./data/multimodal-vision";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  document.body.innerHTML = "";
});

function render(el: ReturnType<typeof createElement>) {
  act(() => root.render(el));
}

describe("TutorialStepBody", () => {
  it("renders content as markdown, not as raw text", () => {
    render(createElement(TutorialStepBody, { content: "Store **context files** in `dir/`." }));
    const body = container.querySelector('[data-testid="tutorial-step-content"]');
    expect(body?.querySelector("strong")?.textContent).toBe("context files");
    expect(body?.querySelector("code")?.textContent).toBe("dir/");
    expect(body?.textContent).not.toContain("**");
  });

  it("renders a markdown table in details", () => {
    render(
      createElement(TutorialStepBody, {
        content: "c",
        details: "| Prompt | Includes |\n|---|---|\n| generation | every file |",
      }),
    );
    const details = container.querySelector('[data-testid="tutorial-step-details"]');
    expect(details?.querySelectorAll("td")).toHaveLength(2);
    expect(details?.textContent).toContain("every file");
  });

  it("lists each non-blank tip and drops blank ones", () => {
    render(
      createElement(TutorialStepBody, { content: "c", tips: ["Use **one** file", "  ", "Second"] }),
    );
    const items = container.querySelectorAll('[data-testid="tutorial-step-tips"] li');
    expect(Array.from(items, (li) => li.textContent)).toEqual(["Use one file", "Second"]);
  });

  it("renders neither block when details and tips are absent or empty", () => {
    render(createElement(TutorialStepBody, { content: "c", details: " ", tips: [] }));
    expect(container.querySelector('[data-testid="tutorial-step-details"]')).toBeNull();
    expect(container.querySelector('[data-testid="tutorial-step-tips"]')).toBeNull();
  });
});

describe("the tutorial tooltips show a real step's details and tips", () => {
  const step = multimodalVisionTutorial.steps.find((s) => s.id === "project-contexts");

  it("the step the finding observed carries details and tips to show", () => {
    expect(step?.details?.trim()).toBeTruthy();
    expect(step?.tips?.length).toBeGreaterThan(0);
  });

  it("TutorialTooltip renders them", () => {
    const target = document.createElement("div");
    target.id = "tutorial-target";
    document.body.appendChild(target);
    render(
      createElement(TutorialTooltip, {
        targetSelector: "#tutorial-target",
        title: step!.title,
        content: step!.content,
        details: step!.details,
        tips: step!.tips,
      }),
    );
    expect(container.querySelector('[data-testid="tutorial-step-content"]')).not.toBeNull();
    expect(container.querySelector('[data-testid="tutorial-step-details"]')).not.toBeNull();
    expect(container.querySelectorAll('[data-testid="tutorial-step-tips"] li')).toHaveLength(
      step!.tips!.filter((t) => t.trim() !== "").length,
    );
  });

  it("ContextualTutorial hands details and tips to both tooltips", () => {
    // CenteredTooltip is private to ContextualTutorial and needs the tutorial
    // context to mount, so its wiring is pinned at the source: each of the
    // two tooltip call sites passes the step's details and tips through.
    const src = readFileSync(join(__dirname, "ContextualTutorial.tsx"), "utf8");
    expect(src.match(/details=\{currentStep\.details\}/g)).toHaveLength(2);
    expect(src.match(/tips=\{currentStep\.tips\}/g)).toHaveLength(2);
    expect(src).not.toMatch(/\{content\}\s*<\/(div|p)>/);
  });
});
