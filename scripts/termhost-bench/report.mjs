/**
 * termhost-bench `report`: a results file rendered as markdown (one table per
 * setup, variant and build role, every unknown with its reason, the profile's
 * calibration flag and the PTY host beside every row), the CI gate's assertions,
 * and the redacted copy public CI may upload (the runner's own rows only, D7).
 *
 * Plan `2026-10-11-terminal-page-performance-kpis-across-setups`, Phase 3. Phase 6
 * grows this into the committed-report generator (one table per environment, the
 * headline line for `windows-desktop`); the rendering rules here are the ones it
 * keeps: a number only for a measured value, `unknown(<reason>)` otherwise.
 */

import { getKpi } from "./kpi-catalog.mjs";
import { redactRunnerOnly } from "./results.mjs";
import { validateResults } from "./validate.mjs";
import { compareStatistic, valueOf } from "./compare.mjs";

/** The KPIs the per-row table shows (the operator's headline set, D10 — no composite). */
export const REPORT_KPIS = Object.freeze([
  "output_latency_ms",
  "input_latency_ms",
  "frame_interval_ms",
  "cpu_pct",
  "memory_mb",
  "producer_stall_pct",
  "hangs",
  "switch_latency_ms",
  "spawn_latency_ms",
]);

/**
 * One KPI cell: a measured value's number (a stats-shape KPI's compared statistic,
 * named), else state(reason).
 */
export function cell(kpiValue, id) {
  if (!kpiValue) return "missing";
  if (kpiValue.state !== "measured") return `${kpiValue.state}(${kpiValue.reason ?? "?"})`;
  const kpi = getKpi(id);
  const v = valueOf(kpiValue, kpi);
  if (v === null) return "measured(?)";
  const r = Math.abs(v) >= 100 ? v.toFixed(0) : Math.abs(v) >= 10 ? v.toFixed(1) : v.toFixed(2);
  return `${r}${kpi.shape === "value" ? "" : ` ${compareStatistic(kpi)}`}`;
}

const absentText = (v) => (v && typeof v === "object" && "state" in v ? `${v.state}(${v.reason})` : String(v));

/** The whole report as markdown. */
export function renderReport(doc) {
  const m = doc.machine;
  const lines = [
    `## termhost-bench ${doc.run.id} — ${m.environment}`,
    "",
    `Machine: ${m.name}, ${m.os.platform} ${m.os.release}, ${m.cpu.logical_cores} logical cores, ${m.ram_mb} MB RAM. ` +
      `Started ${doc.run.started_at}, finished ${doc.run.finished_at}. Numbers compare only within one environment (D6).`,
    "",
  ];
  const groups = new Map();
  for (const r of doc.rows) {
    const key = `${r.setup.id}|${r.setup.variant}|${r.setup.build?.role ?? "-"}`;
    if (!groups.has(key)) groups.set(key, []);
    groups.get(key).push(r);
  }
  for (const rows of groups.values()) {
    const s = rows[0].setup;
    lines.push(
      `### ${s.id}${s.variant !== "default" ? `/${s.variant}` : ""}${s.build?.role ? ` (${s.build.role} ${s.build.state ? "unproven build" : (s.build.commit?.slice(0, 12) ?? "")})` : ""}`,
      "",
      `app ${absentText(s.app_version)}; launch ${s.launch_mode}; pty host ${absentText(s.pty_host_impl)}; render ${absentText(s.render_backend)}; ` +
        `holder ${absentText(s.pty_holder)}; shell ${s.session_shell} (${absentText(s.shell_integration)})`,
      "",
      `| N | arm | run | V (target/actual) | profile (calibrated) | ${REPORT_KPIS.join(" | ")} | labels |`,
      `|---|---|---|---|---|${REPORT_KPIS.map(() => "---").join("|")}|---|`,
    );
    for (const r of rows.sort((a, b) => a.scenario.n - b.scenario.n || a.scenario.arm.localeCompare(b.scenario.arm) || a.scenario.run_index - b.scenario.run_index)) {
      const sc = r.scenario;
      lines.push(
        `| ${sc.n} | ${sc.arm} | ${sc.run_index + 1}/${sc.run_count} | ${sc.v_target}/${typeof sc.v_actual === "number" ? sc.v_actual : absentText(sc.v_actual)} | ` +
          `${sc.profile} (${sc.profile_calibrated}) | ${REPORT_KPIS.map((id) => cell(r.kpis[id], id)).join(" | ")} | ${r.labels.join(", ") || "-"} |`,
      );
    }
    lines.push("");
  }
  const unknowns = [];
  for (const r of doc.rows) {
    for (const [id, v] of Object.entries(r.kpis)) {
      if (v.state === "unknown" && v.reason !== "no_baseline_level" && v.reason !== "too_few_levels") {
        unknowns.push(`- ${r.setup.id}/${r.setup.variant} N=${r.scenario.n} ${r.scenario.arm} run ${r.scenario.run_index}: \`${id}\` unknown(${v.reason})${v.detail ? ` — ${v.detail}` : ""}`);
      }
    }
  }
  if (unknowns.length > 0) lines.push("### Unknown values and why", "", ...unknowns.slice(0, 400), unknowns.length > 400 ? `- … ${unknowns.length - 400} more` : "", "");
  // The run's own incidents, then each row's (the probe's, attributed to the arm
  // that recorded them).
  const incidentLine = (i, where) => `- **${i.kind}**${where}${i.stops_run ? " (stopped the run)" : ""} at ${i.at}: ${i.detail}`;
  const incidents = [
    ...doc.incidents.map((i) => incidentLine(i, "")),
    ...doc.rows.flatMap((r) =>
      (r.incidents ?? []).map((i) => incidentLine(i, ` (${r.setup.id}/${r.setup.variant} N=${r.scenario.n} ${r.scenario.arm} run ${r.scenario.run_index})`)),
    ),
  ];
  if (incidents.length > 0) lines.push("### Incidents", "", ...incidents, "");
  const caps = Object.entries(doc.capabilities).filter(([, c]) => c.state !== "OPERATIVE");
  if (caps.length > 0) lines.push("### Mechanisms not OPERATIVE", "", ...caps.map(([id, c]) => `- \`${id}\` ${c.state}: ${c.reason}`), "");
  return `${lines.join("\n")}\n`;
}

/**
 * The CI gate's assertions, checked on a valid document.
 *
 * A failure line names the KPI's state and reason code. Its free-text `detail` is
 * added verbatim for the runner's own rows only: public CI logs carry these lines,
 * a detail can quote a reading, and a third-party setup's readings never leave the
 * job (D7). A third party's line carries the detail with every number masked
 * ({@link readingFreeDetail}): the CAUSE leaves the job, the reading does not. Without
 * it, CI run 38362046020's `vscode N = 1 tiled run 0: output_latency_ms is
 * unknown(harness_error)` named no cause at all — several unrelated failures
 * share that reason code, and the results file stays in the VM by design.
 *
 * `requireMeasuredIfOperative` adds KPIs to the measured requirement only where
 * the file's own capabilities report a mechanism OPERATIVE — e.g. the capture KPIs
 * on a hosted Windows VM whose capture the image decides. Where the mechanism is
 * not OPERATIVE those KPIs may read unknown (with the reason the validator
 * already requires); they are never missing.
 *
 * `abBaseline`: the run measured a head and a baseline build (`--ab-baseline`).
 * Then every runner row at a gate N must carry a PROVEN build: a row labelled
 * `unknown_build` fails the gate, measured or not, because an A/B number from a
 * build nobody proved is no comparison at all (D12).
 *
 * @param {object} doc
 * @param {{requireMeasured?: string[], atN?: number[], setups?: string[], arms?: string[],
 *   requireRowsN?: number[], roles?: string[], abBaseline?: boolean,
 *   requireMeasuredIfOperative?: {mechanism: string, kpis: string[]}[]}} a
 * @returns {string[]} failures (empty = the gate holds)
 */
export function checkAssertions(doc, a) {
  const failures = [];
  const setups = a.setups ?? [...new Set(doc.rows.map((r) => r.setup.id))];
  const arms = a.arms ?? ["focus", "tiled"];
  const roleOk = (r) => !a.roles || a.roles.includes(r.setup.build?.role ?? "-");
  const required = [...(a.requireMeasured ?? [])];
  for (const cond of a.requireMeasuredIfOperative ?? []) {
    if (doc.capabilities?.[cond.mechanism]?.state === "OPERATIVE") {
      for (const k of cond.kpis) if (!required.includes(k)) required.push(k);
    }
  }
  for (const n of a.requireRowsN ?? []) {
    for (const setup of setups) {
      for (const arm of arms) {
        if (!doc.rows.some((r) => r.setup.id === setup && r.scenario.n === n && r.scenario.arm === arm && roleOk(r))) {
          failures.push(`no ${setup} row for N = ${n}, arm ${arm}`);
        }
      }
    }
  }
  for (const n of a.atN ?? []) {
    for (const setup of setups) {
      for (const arm of arms) {
        const rows = doc.rows.filter((r) => r.setup.id === setup && r.scenario.n === n && r.scenario.arm === arm && roleOk(r));
        if (rows.length === 0) {
          failures.push(`no ${setup} row for N = ${n}, arm ${arm} to check`);
          continue;
        }
        if (a.abBaseline && setup === "runner") {
          for (const r of rows) {
            if ((r.labels ?? []).includes("unknown_build") || r.setup.build?.state) {
              failures.push(
                `runner${r.setup.build?.role ? ` (${r.setup.build.role})` : ""} N = ${n} ${arm} run ${r.scenario.run_index}: the build is unproven (unknown_build); an A/B gate needs both builds proven (D12)`,
              );
            }
          }
        }
        for (const id of required) {
          for (const r of rows) {
            const v = r.kpis[id];
            if (v?.state !== "measured") {
              const detail = !v?.detail ? "" : setup === "runner" ? ` — ${v.detail}` : ` — ${readingFreeDetail(v.detail)}`;
              failures.push(
                `${setup}${r.setup.build?.role ? ` (${r.setup.build.role})` : ""} N = ${n} ${arm} run ${r.scenario.run_index}: ${id} is ${v?.state ?? "missing"}(${v?.reason ?? "?"})${detail}`,
              );
            }
          }
        }
      }
    }
  }
  return failures;
}

/** The longest third-party detail a gate line carries, in code points. */
export const READING_FREE_DETAIL_MAX = 240;

/**
 * A process exit code the detail labels as one (`exit 3`, `exit code 3221225477`,
 * `exit status: -1`), with the NTSTATUS that may follow it in parentheses
 * (`(0xC0000005)`). Kept: an exit code is the process's verdict on why it stopped
 * — a CAUSE (an access violation, a refusal) — not a measurement of the setup.
 */
const EXIT_CODE = String.raw`\bexit(?: code| status)?[ :=]+[+-]?\p{Nd}+(?: \(0[xX][cC][0-9a-fA-F]{7}\))?`;
/**
 * A number: optionally signed (a sign only where no letter or digit precedes it,
 * so `win32-x64` keeps its hyphen), decimal in any script (`\p{Nd}`), with digit
 * groups, a fraction or a leading `.`, and an exponent; or hex (`0x…`, whole); or
 * `Infinity` / `NaN`.
 */
const NUMBER = String.raw`(?:(?<![\p{L}\p{N}])[+-])?(?:\bInfinity\b|\bNaN\b|0[xX][0-9a-fA-F]+|(?:\p{Nd}+(?:[.,]\p{Nd}+)*|\.\p{Nd}+)(?:[eE][+-]?\p{Nd}+)?)`;
const MASK = new RegExp(`(${EXIT_CODE})|${NUMBER}`, "gu");

/**
 * A third-party KPI detail with no reading left in it (D7): every number — a
 * latency, a lag, a count, a percentage, a pid, a handle — becomes `#`, sign and
 * all; hex, non-ASCII digits, exponents, `Infinity` and `NaN` included. A labelled
 * process exit code is kept (see EXIT_CODE: a cause, not a reading). The text is
 * cut at {@link READING_FREE_DETAIL_MAX} code points, never inside a surrogate
 * pair. What remains is the sentence that names the cause ("the probe fell behind
 * its capture source (frame lag # ms ...").
 */
export function readingFreeDetail(detail) {
  const masked = String(detail).replace(MASK, (m, exit) => (exit !== undefined ? exit : "#"));
  const cps = Array.from(masked);
  return cps.length > READING_FREE_DETAIL_MAX ? `${cps.slice(0, READING_FREE_DETAIL_MAX - 1).join("")}…` : masked;
}

/** Validate, then return `{valid, errors}` plus the redacted copy (or null). */
export function prepareReport(doc) {
  const res = validateResults(doc);
  return { ...res, redacted: res.valid ? redactRunnerOnly(doc) : null };
}
