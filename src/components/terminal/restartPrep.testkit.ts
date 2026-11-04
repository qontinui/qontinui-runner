/**
 * Wire-shaped restart-prep fixtures for the store and panel specs: a cohort
 * row, a verdict, a record and its `restart-prep-changed` payload, each with
 * overrides.
 */
import type { CohortRow, PrepChanged, PrepareRecord, PrepVerdict } from "./restartPrep";

export function row(id: string, over: Partial<CohortRow> = {}): CohortRow {
  return {
    session: {
      claudeSessionId: id,
      terminalId: `term-${id}`,
      pageId: "page-a",
      zoneIndex: 0,
      title: "claude",
      sessionName: `name-${id}`,
      accountLabel: "gmail",
      configDir: "/cfg/gmail",
    },
    effort: { level: "high", source: "screen" },
    baseline: { baseline: "high", default_source: "cli_default" },
    model: null,
    state_at_prepare: "working",
    held: false,
    notified_at: null,
    limit_reset_at: null,
    finished: false,
    pause_state: "notified",
    ...over,
  };
}

export function verdict(over: Partial<PrepVerdict> = {}): PrepVerdict {
  return {
    prepare_id: "prep-1",
    state: "preparing",
    hold: "confirmed",
    ready_to_restart: false,
    blockers: [],
    cohort_captured_at_ms: 1,
    evaluated_at_ms: 2,
    evaluated_at: "",
    ...over,
  };
}

export function record(over: Partial<PrepareRecord> = {}): PrepareRecord {
  return {
    prepare_id: "prep-1",
    started_at_ms: 1_000,
    started_at: "",
    prepared_build: "b1",
    state: "preparing",
    hold: { status: "confirmed" },
    cohort: { captured_at_ms: 1, captured_at: "", reason: "restart-prep", rows: [row("s1")] },
    reason: "",
    hold_until_ms: null,
    hold_until: null,
    verdict: null,
    ...over,
  };
}

export function changed(rec: PrepareRecord): PrepChanged {
  return {
    record: rec,
    view: {
      hold: "confirmed",
      active: true,
      accounts_frozen: true,
      local_hold: false,
      same_build_hold: false,
      hold_expired_at: null,
      release_failed: null,
      verdict_current: true,
    },
  };
}
