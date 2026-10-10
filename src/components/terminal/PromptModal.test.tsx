// @vitest-environment jsdom
/**
 * `PromptModal` — the fan-out mode, end to end from the modal the Terminal page
 * renders (plan
 * `2026-09-20-terminal-page-review-notes-become-prompts-and-prompt-matrix-fan-out`
 * Phase 7).
 *
 * The REAL modal is mounted (React 19 `createRoot` + `act`, no Testing Library —
 * the pattern `useSessionReview.test.ts` uses), and only the network is stubbed,
 * at `fetch`: the runner's `POST /fanout` and the per-member collision probe.
 * Everything between the operator's keystrokes and the request body — matrix
 * parsing, expansion, the per-member preview, the Create gate, the request the
 * runner receives and the verdict on its answer — is the production path.
 */

import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";

import wire from "./fixtures/fanout-runs.json";
import { PromptModal } from "./PromptModal";
import type { PromptTemplate } from "./promptLibraryApi";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const SHIP_NOTES: PromptTemplate = {
  name: "ship-notes",
  title: "Ship notes",
  description: "Release notes for one platform",
  category: "release",
  default_action: "spawn",
  version: 2,
  parameters: [
    {
      name: "platform",
      type: "string",
      label: "Platform",
      description: "Which platform",
      required: true,
    },
  ],
  body: "Write release notes for {{platform}}",
};

function jsonReply(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

let liveUnmount: (() => void) | null = null;

afterEach(() => {
  liveUnmount?.();
  liveUnmount = null;
  vi.unstubAllGlobals();
});

/** Let pending fetches, timers at 0 and the state they commit land. */
async function settle() {
  for (let i = 0; i < 3; i++) {
    await act(async () => {
      await new Promise((r) => setTimeout(r, 0));
    });
  }
}

/** Type into a React-controlled input the way a keystroke does. */
async function typeInto(el: Element | null, value: string) {
  expect(el).not.toBeNull();
  const input = el as HTMLInputElement;
  const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!;
  await act(async () => {
    setter.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await settle();
}

async function click(el: Element | null) {
  expect(el).not.toBeNull();
  await act(async () => {
    (el as HTMLElement).click();
  });
  await settle();
}

describe("PromptModal fan-out mode", () => {
  it("previews every member of a matrix and creates exactly the ticked rows on the runner", async () => {
    const requests: Array<{ url: string; method: string; body: string | null }> = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = String(input);
        const method = init?.method ?? "GET";
        requests.push({ url, method, body: typeof init?.body === "string" ? init.body : null });
        if (url.endsWith("/file-registry/probe-conflicts")) {
          return jsonReply({
            predicted_collisions: [{ file_path: "docs/RELEASE.md" }],
            ai_status: "Ok",
          });
        }
        if (url.endsWith("/fanout") && method === "POST") return jsonReply(wire.createdTwo);
        return jsonReply({ success: false, error: "not stubbed" }, 404);
      }),
    );
    const created: string[] = [];

    const host = document.createElement("div");
    document.body.appendChild(host);
    const root = createRoot(host);
    const props = {
      prompts: [SHIP_NOTES],
      auth: { state: "ok" as const },
      loading: false,
      onRefresh: () => {},
      initialSlug: "ship-notes",
      sessions: [],
      onSpawn: () => {},
      onInsert: () => {},
      onClose: () => {},
      fanout: {
        accounts: [],
        defaultWorkingDir: "/work/repo",
        onCreated: (runId: string) => created.push(runId),
      },
      initialMode: "fanout" as const,
    };
    await act(async () => {
      root.render(
        createElement(function Page() {
          return PromptModal(props);
        }),
      );
    });
    await settle();
    liveUnmount = () => {
      act(() => root.unmount());
      host.remove();
    };

    // Opened straight into fan-out mode, with nothing to preview yet.
    const modal = host.querySelector('[data-ui-bridge-id="terminal.prompt-modal"]');
    expect(modal?.getAttribute("data-mode")).toBe("fanout");
    expect(modal?.textContent).toContain("Fan out a prompt");
    expect(
      host.querySelector('[data-ui-bridge-id="terminal.fanout-matrix-error"]')?.textContent,
    ).toContain("Enter a matrix");

    // One axis, two values → two members; the matrix supplies the required parameter.
    await typeInto(
      host.querySelector('[data-ui-bridge-id="terminal.fanout-matrix"]'),
      "platform: iOS, Android",
    );
    expect(host.querySelector('[data-ui-bridge-id="terminal.fanout-matrix-error"]')).toBeNull();
    const row0 = host.querySelector('[data-ui-bridge-id="terminal.fanout-preview-row.0"]');
    const row1 = host.querySelector('[data-ui-bridge-id="terminal.fanout-preview-row.1"]');
    expect(row0?.textContent).toContain("ship-notes — iOS");
    expect(row1?.textContent).toContain("ship-notes — Android");
    expect(host.querySelector('[data-ui-bridge-id="terminal.fanout-preview-row.2"]')).toBeNull();
    expect(
      host.querySelector('[data-ui-bridge-id="terminal.fanout-isolation-warning"]')?.textContent,
    ).toContain("all 2 members share that directory");

    // The Create gate is open (absolute dir, all rows complete) and says what it will do.
    const createButton = host.querySelector(
      '[data-ui-bridge-id="terminal.fanout-create"]',
    ) as HTMLButtonElement;
    expect(createButton.disabled).toBe(false);
    expect(createButton.textContent).toContain("Create 2 sessions");
    expect(host.querySelector('[data-ui-bridge-id="terminal.fanout-create-blocked"]')).toBeNull();

    // The per-member collision probe answers after its debounce.
    await act(async () => {
      await new Promise((r) => setTimeout(r, 700));
    });
    await settle();
    expect(
      host.querySelector('[data-ui-bridge-id="terminal.fanout-preview-row-collisions.0"]')
        ?.textContent,
    ).toContain("1 collision");
    expect(
      requests
        .filter((r) => r.url.endsWith("/file-registry/probe-conflicts"))
        .map((r) => r.body)
        .sort(),
    ).toEqual([
      JSON.stringify({ prompt: "Write release notes for Android", cwd: "/work/repo" }),
      JSON.stringify({ prompt: "Write release notes for iOS", cwd: "/work/repo" }),
    ]);

    await click(createButton);

    // Exactly the previewed rows were posted, each carrying its preview number.
    const post = requests.find((r) => r.url.endsWith("/fanout") && r.method === "POST");
    // Byte-for-byte: the body the runner receives, in the order it is built.
    expect(post?.body).toBe(
      JSON.stringify({
        templateSlug: "ship-notes",
        templateVersion: 2,
        maxConcurrent: 3,
        configDirPolicy: { kind: "bestHeadroom" },
        workingDir: "/work/repo",
        members: [
          { title: "ship-notes — iOS", prompt: "Write release notes for iOS", previewIndex: 0 },
          {
            title: "ship-notes — Android",
            prompt: "Write release notes for Android",
            previewIndex: 1,
          },
        ],
      }),
    );

    // Judged against what the runner holds, then the same body cannot be created twice.
    const result = host.querySelector('[data-ui-bridge-id="terminal.fanout-create-result"]');
    expect(result?.getAttribute("data-result")).toBe("ok");
    expect(result?.textContent).toContain("Queued 2 members — up to 3 run at once");
    expect(created).toEqual([wire.createdTwo.data.run.id]);
    expect(createButton.disabled).toBe(true);
    expect(createButton.textContent).toContain("Created");
    expect(
      host.querySelector('[data-ui-bridge-id="terminal.fanout-create-blocked"]')?.textContent,
    ).toBeTruthy();
  });

  it("offers Fan out… from the single-session form only when the page supplies a fan-out context", async () => {
    const host = document.createElement("div");
    document.body.appendChild(host);
    const root = createRoot(host);
    const base = {
      prompts: [SHIP_NOTES],
      auth: { state: "ok" as const },
      loading: false,
      onRefresh: () => {},
      initialSlug: "ship-notes",
      sessions: [],
      onSpawn: () => {},
      onInsert: () => {},
      onClose: () => {},
    };
    await act(async () => {
      root.render(
        createElement(function Page() {
          return PromptModal({
            ...base,
            fanout: { accounts: [], defaultWorkingDir: "/work/repo" },
          });
        }),
      );
    });
    liveUnmount = () => {
      act(() => root.unmount());
      host.remove();
    };
    const modal = host.querySelector('[data-ui-bridge-id="terminal.prompt-modal"]');
    expect(modal?.getAttribute("data-mode")).toBe("prompt");
    const action = host.querySelector('[data-ui-bridge-id="terminal.prompt-modal-fanout"]');
    expect(action?.textContent).toContain("Fan out…");

    await click(action);
    expect(modal?.getAttribute("data-mode")).toBe("fanout");
    expect(host.querySelector('[data-ui-bridge-id="terminal.fanout-preview"]')).not.toBeNull();

    // Without the context, the action is not offered at all.
    await act(async () => {
      root.render(
        createElement(function Page() {
          return PromptModal(base);
        }),
      );
    });
    expect(host.querySelector('[data-ui-bridge-id="terminal.prompt-modal-fanout"]')).toBeNull();
  });
});
