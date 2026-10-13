/**
 * `pageHealth` IPC — what the runner webview answers the Rust
 * `/control/page-health` forward with.
 *
 * The Rust side parses this reply strictly as the canonical
 * `Observation<PageHealthValue>`, so the contract pinned here is: the producer
 * is the SDK's (`sdk/page-health`), the wire is the canonical flat envelope
 * (JSON round-trip, as the IPC carries it), and "could not look" is a typed
 * `unknown` — never a thrown IPC and never a CRITICAL report over nothing.
 */

import { describe, it, expect } from "vitest";
import { SDK_PAGE_HEALTH_PRODUCER_ID } from "@qontinui/ui-bridge";

import { readPageHealth } from "./pageHealth";

/** What the IPC actually carries: the JSON text, not the in-memory object. */
function overTheWire(takeSnapshot: () => unknown): Record<string, unknown> {
  return JSON.parse(JSON.stringify(readPageHealth(takeSnapshot))) as Record<string, unknown>;
}

/**
 * Every provenance key the Rust canon (`qontinui_vision_core::Provenance`)
 * requires present — `null` is a statement, an absent key fails its parse.
 */
const PROVENANCE_KEYS = [
  "cache",
  "confidence",
  "coverage",
  "evaluatedAt",
  "observedAt",
  "producer",
  "source",
];

function expectCanonicalProvenance(obs: Record<string, unknown>): void {
  expect(Object.keys(obs.provenance as object).sort()).toEqual(PROVENANCE_KEYS);
}

function producerId(obs: Record<string, unknown>): unknown {
  return (obs.provenance as { producer: { id: string } }).producer.id;
}

function element(id: string, rect: { x: number; y: number; width: number; height: number }) {
  return {
    id,
    type: "text",
    category: "content",
    state: { visible: true, textContent: id, normalizedRect: rect },
  };
}

describe("readPageHealth", () => {
  it("a snapshot that throws is unknown{producer_failed}, not a thrown IPC", () => {
    const obs = overTheWire(() => {
      throw new Error("registry gone");
    });
    expect(obs.status).toBe("unknown");
    expect((obs.unknown as { code: string }).code).toBe("producer_failed");
    expect(String((obs.unknown as { detail: string }).detail)).toContain("registry gone");
    expect(producerId(obs)).toBe(SDK_PAGE_HEALTH_PRODUCER_ID);
    expect(obs).not.toHaveProperty("value");
    expectCanonicalProvenance(obs);
  });

  it("a snapshot with no `elements` key stays unknown — never a verdict over []", () => {
    const obs = overTheWire(() => ({ components: [], timestamp: 1 }));
    expect(obs.status).toBe("unknown");
    expect(JSON.stringify(obs)).not.toContain("CRITICAL");
    expectCanonicalProvenance(obs);
  });

  it("an empty registry is unknown, never a CRITICAL empty page", () => {
    const obs = overTheWire(() => ({ elements: [], components: [], timestamp: 1 }));
    expect(obs.status).toBe("unknown");
    expect(JSON.stringify(obs)).not.toContain("CRITICAL");
  });

  it("a laid-out snapshot is measured by the SDK's producer, on the canonical wire", () => {
    const obs = overTheWire(() => ({
      elements: [
        element("title", { x: 0, y: 0, width: 1, height: 0.1 }),
        element("body", { x: 0.1, y: 0.2, width: 0.8, height: 0.6 }),
      ],
      components: [],
      timestamp: Date.parse("2026-10-13T12:00:00Z"),
      snapshotId: "snap-1",
    }));
    expect(obs.status).toBe("measured");
    expect(producerId(obs)).toBe(SDK_PAGE_HEALTH_PRODUCER_ID);
    expectCanonicalProvenance(obs);
    const value = obs.value as Record<string, unknown>;
    expect(Object.keys(value).sort()).toEqual([
      "element_count",
      "findings",
      "heatmap",
      "summary",
      "visible_count",
    ]);
    expect(value.element_count).toBe(2);
    const provenance = obs.provenance as Record<string, unknown>;
    expect(provenance.observedAt).toBe("2026-10-13T12:00:00.000Z");
    expect(provenance.source).toEqual({ snapshotId: "snap-1" });
  });
});
