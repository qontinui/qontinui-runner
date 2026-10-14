/**
 * Every USD figure on the Cost Control panel is labelled a list-price
 * equivalent (served policy production-and-cost
 * `per-task-cost-is-a-labelled-equivalent`). Rendered through
 * `react-dom/server` because the runner's vitest environment is `node` (see
 * `StreamingMessageView.test.tsx`); `CostFeedTable.test.tsx` covers the feed.
 */

import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";

import { AnomalyFeed } from "./AnomalyFeed";
import { BudgetGauge } from "./BudgetGauge";
import { CircuitBreakerStatus } from "./CircuitBreakerStatus";
import { LIST_PRICE_EQUIVALENT } from "./costProvenance";
import type { ActiveBudgetStatus, CostAnomalyEvent } from "./types";

const budget: ActiveBudgetStatus = {
  execution_id: "exec-1",
  total_cost_usd: 1.25,
  max_cost_usd: 5,
  remaining_fraction: 0.75,
  total_tokens: 1000,
  max_tokens: 10000,
  circuit_breaker_tripped: false,
  anomaly_detector_sample_count: 4,
  anomaly_detector_mean: 0.0123,
};

const anomaly: CostAnomalyEvent = {
  task_run_id: "run-1",
  cost_usd: 0.5,
  mean_cost_usd: 0.05,
  std_dev: 0.01,
  z_score: 4.5,
  message: "cost spike",
  timestamp: 0,
};

describe("cost panel labels", () => {
  it("BudgetGauge labels spent / max as a list-price equivalent", () => {
    const html = renderToStaticMarkup(<BudgetGauge budget={budget} />);
    expect(html).toContain("$1.25 / $5.00");
    expect(html).toContain(LIST_PRICE_EQUIVALENT);
  });

  it("AnomalyFeed labels its cost figures when it shows any", () => {
    const html = renderToStaticMarkup(<AnomalyFeed anomalies={[anomaly]} />);
    expect(html).toContain("$0.5000");
    expect(html).toContain(`Costs: ${LIST_PRICE_EQUIVALENT}`);
    expect(renderToStaticMarkup(<AnomalyFeed anomalies={[]} />)).not.toContain("$");
  });

  it("CircuitBreakerStatus labels the mean cost", () => {
    const html = renderToStaticMarkup(<CircuitBreakerStatus budget={budget} />);
    expect(html).toContain(`$0.0123 (${LIST_PRICE_EQUIVALENT})`);
  });
});
