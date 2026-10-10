/**
 * Pure helpers for the Privacy section's "Outbound data flows" list
 * (`EgressPrivacyPanel`). Plan `2026-10-10-spec-front-end-phase-9-generic-boundary`,
 * Phase 8 item 2.
 *
 * The runner reports every flow's switch on `GET /health` → `data.egress`
 * (`src-tauri/src/egress.rs` `health_json`). This module turns that block into
 * rows, and owns the honesty rules: a flow the runner did not report is
 * UNKNOWN — never rendered as "on" — and every state names the rung that
 * decided it.
 */

/** One flow as `/health` `data.egress.<key>` reports it. */
export interface EgressFlowReport {
  allowed: boolean;
  /** `coord` | `persisted` | `profile` | `product_default`. */
  source: string;
  domain: string;
  applies_at_next_start: boolean;
  refused: number;
  /** Telemetry only: what this process's boot actually installed. */
  in_effect?: boolean | null;
}

/** The six flows, in the runner's order, with their plain-language copy. */
export const EGRESS_FLOWS = [
  {
    key: "transcript_sync",
    label: "Transcript sync",
    description:
      "AI session transcripts, tenant memory records and memory queries, sent to your coord tenant. Also needs your own AI content sync toggle.",
  },
  {
    key: "code_mirror",
    label: "Code mirror",
    description:
      "Agent branches pushed to coord's git origin every few minutes. While it is off, that copy is not made, so work on this machine is not protected against loss.",
  },
  {
    key: "terminal_stream",
    label: "Terminal streaming",
    description:
      "Raw terminal output sent to coord and through the web relay, and remote terminal attach in either direction.",
  },
  {
    key: "telemetry",
    label: "Telemetry",
    description:
      "Crash reports, OpenTelemetry traces, and UI-error / crash notices forwarded to the web console.",
  },
  {
    key: "update_check",
    label: "Update check",
    description: "The request for the latest released version of this app.",
  },
  {
    key: "skill_mirror",
    label: "Skill mirror",
    description:
      "The fetch of the shared skills and commands. While it is off, sessions get the copy bundled with this build.",
  },
] as const;

export type EgressFlowKey = (typeof EGRESS_FLOWS)[number]["key"];

/** Where the tenant switches are written — the runner never writes them. */
export const TENANT_POLICY_PATH = "/admin/coord/tenant-policy";

/** Human wording for the rung that decided a flow's state. */
export function describeEgressSource(source: string): string {
  switch (source) {
    case "coord":
      return "set for this project in the web console";
    case "persisted":
      return "last answer from coord (it has not answered since this start)";
    case "profile":
      return "this machine's profile default";
    case "product_default":
      return "product default";
    default:
      return `unrecognised source "${source}"`;
  }
}

/** One rendered row. `state` is "unknown" when the runner reported nothing. */
export interface EgressRow {
  key: EgressFlowKey;
  label: string;
  description: string;
  state: "on" | "off" | "unknown";
  sourceText: string;
  /** Shown beside the state when a flip needs a restart to take effect. */
  nextStartNote: string | null;
  refused: number;
}

/**
 * Build the six rows from `/health`'s `data.egress` (or `undefined` when the
 * read failed or the runner predates the block). A missing or malformed entry
 * is UNKNOWN, never "on".
 */
export function buildEgressRows(egress: unknown): EgressRow[] {
  const block =
    egress && typeof egress === "object" ? (egress as Record<string, unknown>) : undefined;
  return EGRESS_FLOWS.map((flow) => {
    const raw = block?.[flow.key];
    const report = isFlowReport(raw) ? raw : null;
    if (!report) {
      return {
        key: flow.key,
        label: flow.label,
        description: flow.description,
        state: "unknown",
        sourceText: "the runner did not report this flow",
        nextStartNote: null,
        refused: 0,
      };
    }
    let nextStartNote: string | null = null;
    if (report.applies_at_next_start) {
      const pending = typeof report.in_effect === "boolean" && report.in_effect !== report.allowed;
      nextStartNote = pending
        ? `applies at next start (currently ${report.in_effect ? "on" : "off"})`
        : "applies at next start";
    }
    return {
      key: flow.key,
      label: flow.label,
      description: flow.description,
      state: report.allowed ? "on" : "off",
      sourceText: describeEgressSource(report.source),
      nextStartNote,
      refused: report.refused,
    };
  });
}

function isFlowReport(raw: unknown): raw is EgressFlowReport {
  if (!raw || typeof raw !== "object") return false;
  const r = raw as Record<string, unknown>;
  return (
    typeof r.allowed === "boolean" &&
    typeof r.source === "string" &&
    typeof r.applies_at_next_start === "boolean" &&
    typeof r.refused === "number"
  );
}

/**
 * The deep link to the web panel for one flow, or null when no web origin is
 * known (render plain text rather than a broken link).
 */
export function tenantPolicyLink(webAppUrl: string | null, key: EgressFlowKey): string | null {
  if (!webAppUrl) return null;
  return `${webAppUrl.replace(/\/+$/, "")}${TENANT_POLICY_PATH}#egress-${key}`;
}

/** Is a `check_for_updates` / `install_update` answer the egress refusal? */
export function isUpdateEgressOff(data: unknown): boolean {
  return (
    !!data && typeof data === "object" && (data as Record<string, unknown>).status === "egress_off"
  );
}

/** What both update surfaces render in place of an error. */
export const UPDATE_EGRESS_OFF_MESSAGE = "Update checks are off for this project";
