/**
 * The Recent Cost Events table shows per-event cost figures; served policy
 * production-and-cost `per-task-cost-is-a-labelled-equivalent` requires them
 * to be labelled a list-price equivalent. The runner's vitest environment is
 * `node`, so this renders through `react-dom/server` (see
 * `StreamingMessageView.test.tsx`).
 */

import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";

import { CostFeedTable } from "./CostFeedTable";
import { LIST_PRICE_EQUIVALENT } from "./costProvenance";
import type { CostUpdateEvent } from "./types";

const event: CostUpdateEvent = {
  task_run_id: "run-1",
  phase: "agentic",
  iteration: 1,
  input_tokens: 1200,
  output_tokens: 300,
  cache_creation_tokens: 0,
  cache_read_tokens: 800,
  cost_usd: 0.0042,
  cumulative_cost_usd: 0.0042,
  cache_hit_rate: 0.4,
  timestamp: 0,
};

describe("CostFeedTable", () => {
  it("labels the cost columns a list-price equivalent", () => {
    const html = renderToStaticMarkup(<CostFeedTable events={[event]} />);
    expect(html).toContain("<caption");
    expect(html).toContain(`Cost and Cumulative: ${LIST_PRICE_EQUIVALENT}`);
    expect(html).toContain(`title="${LIST_PRICE_EQUIVALENT}"`);
    expect(html).toContain("$0.0042");
  });

  it("shows no cost figure, and so needs no label, when there are no events", () => {
    const html = renderToStaticMarkup(<CostFeedTable events={[]} />);
    expect(html).toContain("No cost events recorded yet.");
    expect(html).not.toContain("$");
  });
});
