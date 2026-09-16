/**
 * `WorkerSessionCell` — the honesty contract of the Conductor worker cell.
 *
 * The runner's vitest config is `environment: "node"` with no React Testing
 * Library, so (as `StreamingMessageView.test.tsx` does) the pure presentational
 * pieces are rendered with `renderToStaticMarkup` and the state logic is
 * exercised through its exported pure helpers. The container's wiring into
 * `ZoneGrid` is pinned at source level.
 */

import { describe, it, expect } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import {
  ConversationView,
  FileChangesPanel,
  SteeringLedger,
  WorkerStateBadge,
  describeWorkerState,
  deliveryLabel,
  settleQueuedOnTransition,
  type SteeringEntry,
} from "./WorkerSessionCell";
import type { FileChangesRead, SessionFileChangesResponse } from "./workerFileChanges";

describe("describeWorkerState", () => {
  it("is UNKNOWN until a read settles, and UNKNOWN when it failed — whatever the state value", () => {
    expect(describeWorkerState("ready", "pending")).toEqual({ label: "attaching…", tone: "unknown" });
    expect(describeWorkerState("closed", "failed")).toEqual({ label: "UNKNOWN", tone: "unknown" });
    expect(describeWorkerState("processing", "failed").tone).toBe("unknown");
  });

  it("maps the SessionManager states once read", () => {
    expect(describeWorkerState("processing", "ok").tone).toBe("busy");
    expect(describeWorkerState("ready", "ok").tone).toBe("idle");
    expect(describeWorkerState("closed", "ok")).toEqual({ label: "finished", tone: "done" });
    expect(describeWorkerState("error", "ok").tone).toBe("error");
    expect(describeWorkerState("connecting", "ok").tone).toBe("starting");
  });
});

describe("settleQueuedOnTransition", () => {
  const queued: SteeringEntry = { id: "q", text: "do x", atMs: 1, delivery: "queued" };
  const sent: SteeringEntry = { id: "s", text: "do y", atMs: 2, delivery: "sent" };

  it("flips queued entries to delivered on the ready → processing edge only", () => {
    const after = settleQueuedOnTransition([queued, sent], "ready", "processing");
    expect(after.map((e) => e.delivery)).toEqual(["delivered", "sent"]);
  });

  it("leaves every entry alone on any other edge", () => {
    for (const [prev, next] of [
      ["processing", "ready"],
      ["processing", "processing"],
      ["ready", "ready"],
      ["ready", "closed"],
      ["connecting", "processing"],
    ] as const) {
      expect(settleQueuedOnTransition([queued, sent], prev, next).map((e) => e.delivery)).toEqual([
        "queued",
        "sent",
      ]);
    }
  });

  it("labels each delivery state distinctly, naming the failure", () => {
    expect(deliveryLabel(queued)).toContain("queued");
    expect(deliveryLabel(sent)).toBe("sent");
    expect(deliveryLabel({ ...sent, delivery: "delivered" })).toBe("delivered");
    expect(deliveryLabel({ ...sent, delivery: "failed", error: "queue full" })).toBe(
      "failed: queue full",
    );
  });
});

describe("WorkerStateBadge", () => {
  it("renders UNKNOWN (with the error as title) when the read failed", () => {
    const html = renderToStaticMarkup(
      <WorkerStateBadge state="closed" readStatus="failed" lastReadError="state read failed: boom" />,
    );
    expect(html).toContain("UNKNOWN");
    expect(html).toContain('data-worker-state="unknown"');
    expect(html).toContain("state read failed: boom");
    expect(html).not.toContain("finished");
  });

  it("renders the live state once read", () => {
    const html = renderToStaticMarkup(
      <WorkerStateBadge state="processing" readStatus="ok" lastReadError={null} />,
    );
    expect(html).toContain("working");
    expect(html).toContain('data-worker-state="processing"');
  });
});

describe("SteeringLedger", () => {
  it("shows queued and sent entries with their delivery state", () => {
    const html = renderToStaticMarkup(
      <SteeringLedger
        entries={[
          { id: "1", text: "first", atMs: 0, delivery: "sent" },
          { id: "2", text: "second", atMs: 0, delivery: "queued" },
        ]}
      />,
    );
    expect(html).toContain('data-delivery="sent"');
    expect(html).toContain('data-delivery="queued"');
    expect(html).toContain("delivered when the current turn ends");
  });

  it("renders nothing with no entries", () => {
    expect(renderToStaticMarkup(<SteeringLedger entries={[]} />)).toBe("");
  });
});

const okResponse = (files: SessionFileChangesResponse["files"]): SessionFileChangesResponse => ({
  sessionId: "w1",
  files,
  readAtMs: 0,
});

describe("FileChangesPanel", () => {
  const noop = () => {};

  it("renders a failed read as UNKNOWN with the error, never as an empty list", () => {
    const read: FileChangesRead = { status: "error", error: "HTTP 500", atMs: 0, previous: null };
    const html = renderToStaticMarkup(<FileChangesPanel read={read} onRefresh={noop} />);
    expect(html).toContain('data-file-changes-status="error"');
    expect(html).toContain("UNKNOWN");
    expect(html).toContain("HTTP 500");
    expect(html).not.toContain("No snapshotted edits");
  });

  it("labels a stale previous read as such while the fresh one failed", () => {
    const previous = okResponse([
      {
        filePath: "/repo/a.ts",
        status: "modified",
        before: "a\n",
        after: "b\n",
        beforeBytes: 2,
        afterBytes: 2,
        beforeSha256: "x",
        afterSha256: "y",
        truncated: false,
        takenAt: null,
        detail: null,
      },
    ]);
    const read: FileChangesRead = { status: "error", error: "boom", atMs: 0, previous };
    const html = renderToStaticMarkup(<FileChangesPanel read={read} onRefresh={noop} />);
    expect(html).toContain("may be stale");
    expect(html).toContain("a.ts");
    expect(html).toContain("UNKNOWN");
  });

  it("says a successful empty read is genuinely empty", () => {
    const read: FileChangesRead = { status: "ok", response: okResponse([]) };
    const html = renderToStaticMarkup(<FileChangesPanel read={read} onRefresh={noop} />);
    expect(html).toContain("No snapshotted edits");
    expect(html).not.toContain("UNKNOWN");
  });

  it("shows an unreadable file as UNKNOWN with the backend detail, beside real changes", () => {
    const read: FileChangesRead = {
      status: "ok",
      response: okResponse([
        {
          filePath: "/repo/b.ts",
          status: "unreadable",
          before: null,
          after: null,
          beforeBytes: null,
          afterBytes: null,
          beforeSha256: null,
          afterSha256: null,
          truncated: false,
          takenAt: null,
          detail: "pre-edit snapshot blob is missing on disk",
        },
        {
          filePath: "/repo/c.ts",
          status: "created",
          before: null,
          after: "new\n",
          beforeBytes: null,
          afterBytes: 4,
          beforeSha256: null,
          afterSha256: "z",
          truncated: false,
          takenAt: null,
          detail: null,
        },
      ]),
    };
    const html = renderToStaticMarkup(<FileChangesPanel read={read} onRefresh={noop} />);
    expect(html).toContain("blob is missing on disk");
    expect(html).toContain('data-file-change="unreadable"');
    expect(html).toContain('data-file-change="created"');
    expect(html).toContain("+1");
    expect(html).toContain("2 changed");
  });
});

describe("ConversationView", () => {
  const base = {
    streamingContent: "",
    streamingDroppedChars: 0,
    isProcessing: false,
    toolActivity: null,
  };

  it("renders a failed read as UNKNOWN, not as an empty transcript", () => {
    const html = renderToStaticMarkup(
      <ConversationView
        {...base}
        messages={[]}
        readStatus="failed"
        lastReadError="history read failed: pg down"
      />,
    );
    expect(html).toContain("UNKNOWN");
    expect(html).toContain("pg down");
    expect(html).not.toContain("No transcript yet");
  });

  it("only calls an empty transcript empty after a successful read", () => {
    const pending = renderToStaticMarkup(
      <ConversationView {...base} messages={[]} readStatus="pending" lastReadError={null} />,
    );
    expect(pending).toContain("attaching");
    expect(pending).not.toContain("No transcript yet");
    const ok = renderToStaticMarkup(
      <ConversationView {...base} messages={[]} readStatus="ok" lastReadError={null} />,
    );
    expect(ok).toContain("No transcript yet");
  });

  it("renders the in-flight tail through StreamingMessageView while processing", () => {
    const html = renderToStaticMarkup(
      <ConversationView
        {...base}
        messages={[{ role: "user", content: "hello" }]}
        streamingContent="working on it"
        isProcessing
        readStatus="ok"
        lastReadError={null}
      />,
    );
    expect(html).toContain("hello");
    expect(html).toContain("working on it");
    expect(html).toContain("animate-pulse"); // the caret
  });
});

describe("ZoneGrid wiring", () => {
  const source = readFileSync(resolve(__dirname, "./ZoneGrid.tsx"), "utf8");

  it("mounts WorkerSessionCell at both TerminalInstance sites (maximized + zoned)", () => {
    expect(source.match(/<WorkerSessionCell\s/g)?.length).toBe(2);
  });

  it("never gives a worker tab the hidden TerminalInstance mount", () => {
    expect(source).toContain("!t.sessionBacked");
  });
});
