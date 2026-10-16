/**
 * termhost-bench `report`: results rendered as markdown, the CI gate's
 * assertions, and the redacted copy public CI may upload (the runner's own rows
 * only, D7).
 *
 * Plan `2026-10-11-terminal-page-performance-kpis-across-setups`, Phases 3 and 6.
 * The report takes one results file or several, and renders ONE table per
 * environment: it refuses files from two environments outright (D6), because a
 * linux-xvfb number beside a Windows one implies a comparison its provenance
 * cannot carry [policy: verification-and-evidence/unknown-must-not-render-as-a-default].
 * In order:
 *
 *   - for `windows-desktop`, the headline line — exactly
 *     `headline: windows-desktop n=15 setups=runner,vscode,windows-terminal,tmux-wsl arms=focus,tiled state=measured`
 *     — emitted only when all four setups' headline KPIs read `measured` at
 *     N = 15 in both arms (a KPI with no referent in an arm, `not_applicable_to_arm`,
 *     is not a gap). The committed report's content anchor
 *     (`rtp-windows-desktop-comparison-recorded`) reads that line; otherwise the
 *     report says what is missing, in words that cannot match it;
 *   - the headline table: N = 15, each setup and arm, every headline KPI and its
 *     degradation as the median over runs with the run spread (no composite, D10);
 *   - every row in one table, with `profile_calibrated` and `pty_host_impl` beside
 *     each (the schema at this phase has no `crosscheck_profile` field);
 *   - each setup's facts, every unknown with its reason, the runner's diagnostics
 *     (labelled non-comparable), incidents and mechanisms not OPERATIVE.
 *
 * A number appears only for a measured value; anything else is `state(reason)`.
 */

import { HEADLINE, KPI_CATALOG, SETUP_IDS, getKpi } from "./kpi-catalog.mjs";
import { CAPTURE_KPIS, isCaptureHealthUnknown } from "./kpis.mjs";
import { redactRunnerOnly } from "./results.mjs";
import { validateResults } from "./validate.mjs";
import { HEADLINE_KPIS, VERDICT_EXCLUDED_LABELS, comparableBuild, compareStatistic, spreadOf, statesOf, valueOf } from "./compare.mjs";
import { summarizeDiagnostic } from "./diagnostics.mjs";

/** The KPIs the per-row table shows (the operator's headline set plus frame cadence, D10 — no composite). */
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

/** The degradations the headline table shows beside the headline KPIs (compare.mjs `HEADLINE_KPIS`). */
export const HEADLINE_DEGRADATIONS = Object.freeze(KPI_CATALOG.filter((k) => k.degradation_of).map((k) => k.id));

/** The four setups the operator's question names (D6, windows-desktop). */
export const HEADLINE_SETUPS = Object.freeze(["runner", "vscode", "windows-terminal", "tmux-wsl"]);

/** The one line the committed windows-desktop report carries once the comparison is recorded. */
export const HEADLINE_LINE = `headline: ${HEADLINE.environment} n=${HEADLINE.n} setups=${HEADLINE_SETUPS.join(",")} arms=${HEADLINE.arms.join(",")} state=measured`;

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
  return `${num(v)}${kpi.shape === "value" ? "" : ` ${compareStatistic(kpi)}`}`;
}

const num = (v) => (Math.abs(v) >= 100 ? v.toFixed(0) : Math.abs(v) >= 10 ? v.toFixed(1) : v.toFixed(2));

const absentText = (v) => (v && typeof v === "object" && "state" in v ? `${v.state}(${v.reason})` : String(v));

/**
 * The one environment a set of results documents shares.
 *
 * @throws when they name two, or none
 */
export function oneEnvironment(docs) {
  const envs = [...new Set(docs.map((d) => d?.machine?.environment))];
  if (envs.length !== 1 || !envs[0]) {
    throw new Error(`refusing to put ${envs.map(String).join(" and ")} in one table: numbers compare only within one environment (D6)`);
  }
  return envs[0];
}

/** A row's build, as the report names it. */
const buildText = (s) => (s.build?.role ? `${s.build.role} ${s.build.state ? "unproven build" : (s.build.commit?.slice(0, 12) ?? "")}`.trim() : "-");

/**
 * Whether the headline comparison is established: for each of the four setups
 * and both arms, at least one N = 15 row (default variant, not a baseline build,
 * a proven build where the setup has one, not excluded from verdicts), and every
 * headline KPI in every such row `measured` — or `not_applicable` because the KPI
 * has no referent in that arm. Every row behind the line must come from ONE
 * machine (`machine.name`, the schema's only machine identity): four setups
 * measured on two machines are not one comparison.
 *
 * @param {object[]} docs results documents of ONE environment
 * @returns {{established: boolean, missing: string[]}}
 */
export function headlineStatus(docs) {
  const env = oneEnvironment(docs);
  if (env !== HEADLINE.environment) return { established: false, missing: [`the environment is ${env}, not ${HEADLINE.environment}`] };
  const rows = docs.flatMap((d) => d.rows.map((r) => ({ ...r, machineName: d.machine?.name })));
  const missing = [];
  const machines = new Set();
  for (const setup of HEADLINE_SETUPS) {
    for (const arm of HEADLINE.arms) {
      const at = rows.filter(
        (r) =>
          r.setup.id === setup &&
          r.setup.variant === "default" &&
          r.scenario.n === HEADLINE.n &&
          r.scenario.arm === arm &&
          r.setup.build?.role !== "baseline" &&
          comparableBuild(r) &&
          !(r.labels ?? []).some((l) => VERDICT_EXCLUDED_LABELS.includes(l)),
      );
      if (at.length === 0) {
        missing.push(`${setup} ${arm}: no N = ${HEADLINE.n} row`);
        continue;
      }
      for (const r of at) machines.add(r.machineName);
      for (const id of HEADLINE_KPIS) {
        const bad = at.filter((r) => {
          const v = r.kpis[id];
          return !(v?.state === "measured" || (v?.state === "not_applicable" && v.reason === "not_applicable_to_arm"));
        });
        if (bad.length > 0) missing.push(`${setup} ${arm}: ${id} ${statesOf(bad.map((r) => r.kpis[id]))}`);
      }
    }
  }
  if (machines.size > 1) missing.push(`the rows come from ${machines.size} machines (${[...machines].sort().join(", ")}): one comparison is one machine's`);
  return { established: missing.length === 0, missing };
}

function spreadCell(rows, id) {
  const kpi = getKpi(id);
  const raw = rows.map((r) => r.kpis[id]);
  const s = spreadOf(raw.map((v) => valueOf(v, kpi)).filter((v) => v !== null));
  if (!s) return statesOf(raw);
  const stat = kpi.shape === "value" ? "" : ` ${compareStatistic(kpi)}`;
  return s.runs > 1 ? `${num(s.median)}${stat} [${num(s.min)}–${num(s.max)}] (${s.runs} runs)` : `${num(s.median)}${stat}`;
}

const setupOrder = (id) => {
  const i = SETUP_IDS.indexOf(id);
  return i < 0 ? SETUP_IDS.length : i;
};
const rowOrder = (a, b) =>
  setupOrder(a.setup.id) - setupOrder(b.setup.id) ||
  a.setup.variant.localeCompare(b.setup.variant) ||
  String(a.setup.build?.role ?? "").localeCompare(String(b.setup.build?.role ?? "")) ||
  a.scenario.n - b.scenario.n ||
  a.scenario.arm.localeCompare(b.scenario.arm) ||
  a.scenario.run_index - b.scenario.run_index;

/**
 * The report as markdown: one results document, or several from ONE environment.
 *
 * @param {object | object[]} input
 */
export function renderReport(input) {
  const docs = Array.isArray(input) ? input : [input];
  if (docs.length === 0) throw new Error("no results to report");
  const env = oneEnvironment(docs);
  const rows = docs.flatMap((d) => d.rows).sort(rowOrder);
  const lines = [`## termhost-bench ${docs.map((d) => d.run.id).join(", ")} — ${env}`, ""];
  for (const doc of docs) {
    const m = doc.machine;
    lines.push(
      `Machine: ${m.name}, ${m.os.platform} ${m.os.release}, ${m.cpu.logical_cores} logical cores, ${m.ram_mb} MB RAM. ` +
        `Run ${doc.run.id}: started ${doc.run.started_at}, finished ${doc.run.finished_at}. Numbers compare only within one environment (D6).`,
    );
  }
  lines.push("");

  // The headline line, and the headline table.
  if (env === HEADLINE.environment) {
    const st = headlineStatus(docs);
    if (st.established) lines.push(HEADLINE_LINE, "");
    else lines.push(`The headline comparison is NOT established (${st.missing.length} gap(s)):`, "", ...st.missing.map((m) => `- ${m}`), "");
  }
  const atHeadlineN = rows.filter((r) => r.scenario.n === HEADLINE.n);
  if (atHeadlineN.length > 0) {
    const cols = [...HEADLINE_KPIS, ...HEADLINE_DEGRADATIONS];
    lines.push(
      `### Headline: N = ${HEADLINE.n} (median over runs, run spread in brackets)`,
      "",
      `| setup | build | arm | runs | profile (calibrated) | pty_host_impl | ${cols.join(" | ")} | labels |`,
      `|---|---|---|---|---|---|${cols.map(() => "---").join("|")}|---|`,
    );
    const groups = new Map();
    for (const r of atHeadlineN) {
      const key = `${r.setup.id}|${r.setup.variant}|${buildText(r.setup)}|${r.scenario.arm}`;
      if (!groups.has(key)) groups.set(key, []);
      groups.get(key).push(r);
    }
    for (const g of groups.values()) {
      const r0 = g[0];
      const cal = [...new Set(g.map((r) => `${r.scenario.profile} (${r.scenario.profile_calibrated})`))].join(", ");
      const pty = [...new Set(g.map((r) => absentText(r.setup.pty_host_impl)))].join(", ");
      // Labels such as dom_renderer_fallback say a row describes a configuration
      // no operator runs (D6): it stays in the table, named, and out of any verdict.
      const labels = [...new Set(g.flatMap((r) => r.labels ?? []))].sort().join(", ") || "-";
      lines.push(
        `| ${r0.setup.id}${r0.setup.variant !== "default" ? `/${r0.setup.variant}` : ""} | ${buildText(r0.setup)} | ${r0.scenario.arm} | ${g.length} | ${cal} | ${pty} | ${cols.map((id) => spreadCell(g, id)).join(" | ")} | ${labels} |`,
      );
    }
    lines.push("");
  }

  // Every row, one table for the environment.
  lines.push(
    `### Every row (${env})`,
    "",
    `| setup | build | N | arm | run | V (target/actual) | profile (calibrated) | pty_host_impl | cross-check | ${REPORT_KPIS.join(" | ")} | labels |`,
    `|---|---|---|---|---|---|---|---|---|${REPORT_KPIS.map(() => "---").join("|")}|---|`,
  );
  for (const r of rows) {
    const s = r.setup;
    const sc = r.scenario;
    lines.push(
      `| ${s.id}${s.variant !== "default" ? `/${s.variant}` : ""} | ${buildText(s)} | ${sc.n} | ${sc.arm} | ${sc.run_index + 1}/${sc.run_count} | ${sc.v_target}/${typeof sc.v_actual === "number" ? sc.v_actual : absentText(sc.v_actual)} | ` +
        `${sc.profile} (${sc.profile_calibrated}) | ${absentText(s.pty_host_impl)} | ${sc.crosscheck_profile} | ${REPORT_KPIS.map((id) => cell(r.kpis[id], id)).join(" | ")} | ${r.labels.join(", ") || "-"} |`,
    );
  }
  lines.push("");

  // Each setup's facts.
  const facts = new Map();
  for (const r of rows) {
    const s = r.setup;
    const key = `${s.id}|${s.variant}|${buildText(s)}`;
    if (facts.has(key)) continue;
    facts.set(
      key,
      `- **${s.id}${s.variant !== "default" ? `/${s.variant}` : ""}** (${buildText(s)}): app ${absentText(s.app_version)}; launch ${s.launch_mode}; pty host ${absentText(s.pty_host_impl)}; ` +
        `render ${absentText(s.render_backend)}; holder ${absentText(s.pty_holder)}; shell ${s.session_shell} (${absentText(s.shell_integration)})`,
    );
  }
  lines.push("### Setups", "", ...facts.values(), "");

  const where = (r) => `${r.setup.id}/${r.setup.variant}${r.setup.build?.role ? ` (${r.setup.build.role})` : ""} N=${r.scenario.n} ${r.scenario.arm} run ${r.scenario.run_index}`;
  const unknowns = [];
  for (const r of rows) {
    for (const [id, v] of Object.entries(r.kpis)) {
      if (v.state === "unknown" && v.reason !== "no_baseline_level" && v.reason !== "too_few_levels") {
        unknowns.push(`- ${where(r)}: \`${id}\` unknown(${v.reason})${v.detail ? ` — ${v.detail}` : ""}`);
      }
    }
  }
  if (unknowns.length > 0) lines.push("### Unknown values and why", "", ...unknowns.slice(0, 400), unknowns.length > 400 ? `- … ${unknowns.length - 400} more` : "", "");

  // The runner's own diagnostics: never compared, never in a verdict (D1).
  const diags = rows
    .filter((r) => r.setup.id === "runner" && r.diagnostics && Object.keys(r.diagnostics).length > 0)
    .map((r) => [`- ${where(r)}:`, ...Object.keys(r.diagnostics).sort().map((k) => `  - \`${k}\`: ${summarizeDiagnostic(r.diagnostics[k])}`)].join("\n"));
  if (diags.length > 0) lines.push("### Runner diagnostics (non-comparable)", "", "Runner internals with no counterpart in another host: context for a runner reading, never compared and never in a verdict (D1).", "", ...diags, "");

  // The runs' own incidents, then each row's (the probe's, attributed to the arm
  // that recorded them).
  const incidentLine = (i, w) => `- **${i.kind}**${w}${i.stops_run ? " (stopped the run)" : ""} at ${i.at}: ${i.detail}`;
  const incidents = [
    ...docs.flatMap((d) => d.incidents.map((i) => incidentLine(i, docs.length > 1 ? ` (run ${d.run.id})` : ""))),
    ...rows.flatMap((r) => (r.incidents ?? []).map((i) => incidentLine(i, ` (${r.setup.id}/${r.setup.variant} N=${r.scenario.n} ${r.scenario.arm} run ${r.scenario.run_index})`))),
  ];
  if (incidents.length > 0) lines.push("### Incidents", "", ...incidents, "");
  const caps = docs.flatMap((d) =>
    Object.entries(d.capabilities)
      .filter(([, c]) => c.state !== "OPERATIVE")
      .map(([id, c]) => `- \`${id}\` ${c.state}: ${c.reason}${docs.length > 1 ? ` (run ${d.run.id})` : ""}`),
  );
  if (caps.length > 0) lines.push("### Mechanisms not OPERATIVE", "", ...caps, "");
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
 * `acceptCaptureHealthUnknown` (`--accept-capture-health-unknown`, the
 * third-party smokes on windows-hosted only): on a windows-hosted file, a
 * THIRD-PARTY row's capture-dependent KPI (catalog `needs: capture`) may read
 * unknown for capture health — capture_disagreement, or the probe-fell-behind
 * verdict (`kpis.mjs` isCaptureHealthUnknown). A software-composited hosted VM's
 * capture says nothing about a third-party host, and its smoke there checks
 * plumbing (D6). Every other unknown still fails (pane_stalled,
 * sessions_not_rendered, a launch failure, …), the plumbing KPIs still have to
 * be measured, and runner rows are never excused: the runner's own gate does
 * not pass this flag, and it would not apply to them if it did.
 *
 * `abBaseline`: the run measured a head and a baseline build (`--ab-baseline`).
 * Then every runner row at a gate N must carry a PROVEN build: a row labelled
 * `unknown_build` fails the gate, measured or not, because an A/B number from a
 * build nobody proved is no comparison at all (D12).
 *
 * @param {object} doc
 * @param {{requireMeasured?: string[], atN?: number[], setups?: string[], arms?: string[],
 *   requireRowsN?: number[], roles?: string[], abBaseline?: boolean,
 *   requireMeasuredIfOperative?: {mechanism: string, kpis: string[]}[], acceptCaptureHealthUnknown?: boolean}} a
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
            if (captureHealthExcused(doc, a, setup, id, v)) continue;
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

/** Whether `--accept-capture-health-unknown` excuses this KPI value (see checkAssertions). */
export function captureHealthExcused(doc, a, setup, id, v) {
  return (
    a.acceptCaptureHealthUnknown === true &&
    setup !== "runner" &&
    doc.machine?.environment === "windows-hosted" &&
    CAPTURE_KPIS.includes(id) &&
    isCaptureHealthUnknown(v)
  );
}

/** The longest third-party detail a gate line carries, in code points. */
export const READING_FREE_DETAIL_MAX = 240;

/**
 * A process exit code the detail labels as one — an ALLOWLIST, so anything it
 * does not name is masked. The forms: `exit N`, `exit code N`, `exited with
 * code N`, where N is an ASCII-decimal u32 (at most 4294967295, no sign) or
 * `0x` with 1-8 hex digits, optionally followed by a parenthesised hex status
 * (`(0xC0000005)`). What follows N — or the status, when there is one — must be
 * the end of the text, one of `; , : )`, or whitespace and one of the words
 * after / from / while / on / during / when. Anything else — a unit (`ms`,
 * `seconds`, `MiB`, `lines`), a word, a decimal or exponent part, another
 * digit, a second status — makes it a reading, masked (`exit 5 (0x5)ms` is).
 * Kept: an exit code is the process's verdict on why it stopped, a CAUSE (an
 * access violation, a refusal), not a measurement of the setup.
 *
 * A known limit: the shape is matched, not the meaning, so a reading worded
 * exactly like an exit code — "waited until exit 4500; gave up" — is kept.
 * No producer in this harness words a reading that way; a new one must not.
 */
const EXIT_END = String.raw`(?=$|[;,:)]|\s+(?:after|from|while|on|during|when)(?![\p{L}\p{N}-]))`;
const EXIT_CODE = String.raw`\b(?:exit|exit code|exited with code) (?<code>[0-9]{1,10}|0[xX][0-9a-fA-F]{1,8})(?: \(0[xX][0-9a-fA-F]{1,8}\))?${EXIT_END}`;
const U32_MAX = 4294967295;
/**
 * A number: optionally signed (a sign only where no letter or digit precedes it,
 * so `win32-x64` keeps its hyphen), decimal in any script (`\p{Nd}`), with digit
 * groups, a fraction or a leading `.`, and an exponent; or hex (`0x…`, whole); or
 * `Infinity` / `NaN`.
 */
const NUMBER = String.raw`(?:(?<![\p{L}\p{N}])[+-])?(?:\bInfinity\b|\bNaN\b|0[xX][0-9a-fA-F]+|(?:\p{Nd}+(?:[.,]\p{Nd}+)*|\.\p{Nd}+)(?:[eE][+-]?\p{Nd}+)?)`;
const MASK = new RegExp(`(${EXIT_CODE})|${NUMBER}`, "giu");
const NUMBERS = new RegExp(NUMBER, "giu");

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
  const cps = Array.from(maskReadings(detail));
  return cps.length > READING_FREE_DETAIL_MAX ? `${cps.slice(0, READING_FREE_DETAIL_MAX - 1).join("")}…` : cps.join("");
}

/** {@link readingFreeDetail}'s masking without its length cut (e.g. for a dedupe key). */
export function maskReadings(detail) {
  return String(detail).replace(MASK, (m, exit, ...rest) => {
    if (exit === undefined) return "#";
    const code = rest.at(-1)?.code ?? "";
    // A decimal code past u32 is not an exit code: mask it like any number.
    if (/^[0-9]+$/.test(code) && Number(code) > U32_MAX) return exit.replace(NUMBERS, "#");
    return exit;
  });
}

/** Validate, then return `{valid, errors}` plus the redacted copy (or null). */
export function prepareReport(doc) {
  const res = validateResults(doc);
  return { ...res, redacted: res.valid ? redactRunnerOnly(doc) : null };
}
