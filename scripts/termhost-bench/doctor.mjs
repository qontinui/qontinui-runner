/**
 * The termhost-bench doctor: which mechanisms a run needs work on THIS machine, in
 * the vocabulary of qontinui-claude-config `scripts/capability-doctor.sh` —
 * OPERATIVE / DEGRADED / INOPERATIVE-ON-THIS-MACHINE / UNKNOWN, each with a reason.
 * An absence is INOPERATIVE, an unanswered probe is UNKNOWN, and neither is ever
 * rendered as "fine".
 *
 * Plan `2026-10-11-terminal-page-performance-kpis-across-setups`, Phase 3 ("doctor ->
 * launch"). It joins the probe's own doctor (`termhost-probe doctor --json`: the
 * capture sources, the input safeguard, the clock, the GPU counters, the session)
 * with the harness's: the environment's platform and tools, the probe binary, Node,
 * and each setup adapter's `check()`. The mapping from gathered facts to verdicts is
 * pure (`mapDoctor`) and tested; gathering (`gatherDoctorFacts`) is the impure half.
 *
 * Every verdict is also written into each results file's `capabilities`, so a row
 * that reads `unknown(capture_unavailable)` sits next to the reason why.
 */

import os from "node:os";

import { drainMechanism, readDeviceDrain } from "./drain.mjs";
import { DOCTOR_STATES } from "./kpi-catalog.mjs";
import { probeDoctor } from "./probe.mjs";
import { findOnPath } from "./envs/linux-xvfb.mjs";

export const OPERATIVE = "OPERATIVE";
export const DEGRADED = "DEGRADED";
export const INOPERATIVE = "INOPERATIVE-ON-THIS-MACHINE";
export const UNKNOWN = "UNKNOWN";

/** The probe's mechanisms (`crates/termhost-probe/src/doctor.rs` `MECHANISMS`). */
export const PROBE_MECHANISMS = Object.freeze([
  "session.interactive",
  "capture.wgc",
  "capture.dda",
  "capture.x11",
  "input.safe",
  "clock.aligned",
  "counters.gpu",
]);

const v = (state, reason) => ({ state, reason });

/**
 * Facts -> verdicts. Pure.
 *
 * @param {{
 *   environment: string, envPlatform: string, envNotBuilt: string | null, platform: string,
 *   nodeVersion: string, xvfbPath?: string | null, windowManagerPath?: string | null,
 *   display?: {started: boolean, detail: string} | null,
 *   probePath: string | null, probeDoctor?: {record: object | null, error: string | null} | null,
 *   setupMechanisms?: Record<string, {state: string, reason: string}>,
 * }} f
 * @returns {Record<string, {state: string, reason: string}>}
 */
export function mapDoctor(f) {
  const m = {};
  m["env.platform"] =
    f.platform === f.envPlatform
      ? v(OPERATIVE, `${f.platform} hosts the ${f.environment} environment`)
      : v(INOPERATIVE, `${f.environment} runs on ${f.envPlatform}; this machine is ${f.platform}`);
  m["env.harness"] = f.envNotBuilt ? v(INOPERATIVE, f.envNotBuilt) : v(OPERATIVE, `the ${f.environment} environment is built`);
  const major = Number(/^v(\d+)/.exec(f.nodeVersion ?? "")?.[1] ?? 0);
  m["node.version"] = major >= 20 ? v(OPERATIVE, `node ${f.nodeVersion}`) : v(INOPERATIVE, `node ${f.nodeVersion} is older than v20`);
  if (f.environment === "linux-xvfb" && f.platform !== "linux") {
    m["env.xvfb"] = v(INOPERATIVE, `Xvfb runs on Linux; this machine is ${f.platform}`);
    m["env.window_manager"] = v(INOPERATIVE, `the linux-xvfb window manager runs on Linux; this machine is ${f.platform}`);
  } else if (f.environment === "linux-xvfb") {
    m["env.xvfb"] = f.xvfbPath ? v(OPERATIVE, `Xvfb at ${f.xvfbPath}`) : v(INOPERATIVE, "Xvfb is not on PATH (apt-get install xvfb)");
    m["env.window_manager"] = f.windowManagerPath
      ? v(OPERATIVE, `matchbox-window-manager at ${f.windowManagerPath}: windows are maximised and focused`)
      : v(DEGRADED, "no matchbox-window-manager: windows are neither maximised nor focused by policy; the harness activates the measured window itself");
  }
  if (!f.probePath) {
    m["probe.binary"] = v(INOPERATIVE, "termhost-probe is not built (cargo build -p termhost-probe) and no --probe was given");
  } else if (!f.probeDoctor?.record) {
    m["probe.binary"] = v(UNKNOWN, f.probeDoctor?.error ?? `termhost-probe at ${f.probePath} did not answer doctor`);
  } else {
    m["probe.binary"] = v(OPERATIVE, `termhost-probe at ${f.probePath}`);
  }
  const record = f.probeDoctor?.record ?? null;
  for (const id of PROBE_MECHANISMS) {
    const found = record?.mechanisms?.find?.((x) => x?.id === id);
    if (found && DOCTOR_STATES.includes(found.state)) {
      m[id] = v(found.state, String(found.reason || "(no reason given)"));
    } else if (f.display && !f.display.started && f.environment === "linux-xvfb") {
      m[id] = v(UNKNOWN, `not probed: no X display to probe on (${f.display.detail})`);
    } else {
      m[id] = v(UNKNOWN, record ? `the probe's doctor did not report ${id}` : `not probed: ${m["probe.binary"].reason}`);
    }
  }
  // D9: the doctor's own probes must never move the foreground. A probe doctor that
  // saw the foreground change around its probe window records it in `violations`
  // (crates/termhost-probe/src/doctor.rs); that is a broken safety rule on THIS
  // desktop, so it blocks the run rather than becoming a note beside it.
  if (record) {
    const violations = Array.isArray(record.violations) ? record.violations.map(String) : [];
    m["probe.foreground_safety"] =
      violations.length > 0
        ? v(INOPERATIVE, `refused: the probe's doctor moved the foreground (${violations.length} D9 violation(s): ${violations.join("; ").slice(0, 300)})`)
        : v(OPERATIVE, "the probe's doctor recorded no D9 violation");
  } else {
    m["probe.foreground_safety"] = v(UNKNOWN, `not probed: ${m["probe.binary"].reason}`);
  }
  for (const [id, verdict] of Object.entries(f.setupMechanisms ?? {})) {
    m[id] = DOCTOR_STATES.includes(verdict?.state) ? v(verdict.state, String(verdict.reason || "(no reason given)")) : v(UNKNOWN, `malformed verdict for ${id}`);
  }
  // Read before any unit, so a drained (or unknown-drain) box spawns nothing.
  m["device.drain"] = drainMechanism(f.drain);
  return m;
}

/**
 * The verdicts a run cannot start without (anything else degrades rows to unknown
 * with its reason instead of stopping the run).
 */
export function blockingMechanisms(mechanisms) {
  return ["env.platform", "env.harness", "env.xvfb", "env.hosted_runner", "probe.binary", "probe.foreground_safety", "node.version", "device.drain"]
    .filter((id) => mechanisms[id] && mechanisms[id].state !== OPERATIVE)
    .map((id) => ({ id, ...mechanisms[id] }));
}

/**
 * The probe doctor's `notes`: observations that are NOT violations — a foreground
 * change the probe did not cause (the operator switched windows while the doctor's
 * probe was up). Informational: shown in the doctor report, never a verdict, and
 * never a reason to block a run (unlike `violations`, probe.foreground_safety).
 *
 * @returns {string[]}
 */
export function doctorNotes(facts) {
  const notes = facts?.probeDoctor?.record?.notes;
  return Array.isArray(notes) ? notes.map(String) : [];
}

/**
 * The doctor report (`--json`): capability-doctor's shape, plus the probe
 * doctor's informational `notes`.
 *
 * @param {Record<string, {state: string, reason: string}>} mechanisms
 * @param {{environment: string, checkedAt?: string, notes?: string[]}} o
 */
export function doctorReport(mechanisms, { environment, checkedAt = new Date().toISOString(), notes = [] }) {
  const list = Object.entries(mechanisms).map(([id, x]) => ({ id, state: x.state, reason: x.reason }));
  const count = (s) => list.filter((x) => x.state === s).length;
  return {
    schema: "termhost-bench/doctor/v1",
    host: os.hostname(),
    os: `${process.platform} ${os.release()}`,
    environment,
    checked_at: checkedAt,
    mechanisms: list,
    notes: [...notes],
    summary: { operative: count(OPERATIVE), degraded: count(DEGRADED), inoperative: count(INOPERATIVE), unknown: count(UNKNOWN) },
  };
}

/** The results file's `capabilities` block. */
export function toCapabilities(mechanisms, checkedAt = new Date().toISOString()) {
  return Object.fromEntries(Object.entries(mechanisms).map(([id, x]) => [id, { state: x.state, reason: x.reason, checked_at: checkedAt }]));
}

/** The human rendering. */
export function renderDoctor(report) {
  const lines = [`termhost-bench doctor: ${report.host} (${report.os}), environment ${report.environment}, at ${report.checked_at}`, "", "MECHANISMS:"];
  for (const m of report.mechanisms) lines.push(`  ${m.id.padEnd(22)} ${m.state.padEnd(28)} ${m.reason}`);
  if ((report.notes ?? []).length > 0) {
    lines.push("", "NOTES (informational: not the probe's doing, and never a reason to block a run):");
    for (const n of report.notes) lines.push(`  ${n}`);
  }
  const s = report.summary;
  lines.push("", `SUMMARY: ${s.inoperative} INOPERATIVE-ON-THIS-MACHINE, ${s.degraded} DEGRADED, ${s.operative} OPERATIVE, ${s.unknown} UNKNOWN (of ${report.mechanisms.length} mechanisms)`);
  return `${lines.join("\n")}\n`;
}

/**
 * Gather the facts (impure): tools on PATH, the probe's doctor (run with the
 * environment's display), each adapter's check().
 *
 * @param {{environment: string, spec: object, probePath: string | null, adapters?: object[],
 *   envVars?: object | null, display?: {started: boolean, detail: string} | null,
 *   which?: (name: string) => string | null, doctorFn?: typeof probeDoctor}} o
 */
export async function gatherDoctorFacts({
  environment,
  spec,
  probePath,
  adapters = [],
  envVars = null,
  display = null,
  which = (name) => findOnPath(name),
  doctorFn = probeDoctor,
  drainFn = readDeviceDrain,
}) {
  const facts = {
    environment,
    envPlatform: spec.platform,
    envNotBuilt: spec.notBuilt,
    platform: process.platform,
    nodeVersion: process.version,
    probePath,
    probeDoctor: null,
    display,
    setupMechanisms: {},
    drain: await drainFn(),
  };
  if (environment === "linux-xvfb") {
    facts.xvfbPath = process.platform === "linux" ? which("Xvfb") : null;
    facts.windowManagerPath = process.platform === "linux" ? which("matchbox-window-manager") : null;
  }
  const canProbe = probePath && !(environment === "linux-xvfb" && (!display || !display.started));
  if (canProbe) {
    facts.probeDoctor = doctorFn({ probe: probePath, env: { ...process.env, ...(envVars ?? {}) } });
  }
  // Verdicts the environment decides itself (e.g. windows-hosted: a GitHub-hosted runner).
  Object.assign(facts.setupMechanisms, spec.doctorFacts?.(process.env) ?? {});
  for (const a of adapters) {
    try {
      const r = await a.check();
      Object.assign(facts.setupMechanisms, r?.mechanisms ?? {});
    } catch (e) {
      facts.setupMechanisms[`setup.${a.id}`] = { state: UNKNOWN, reason: `check() failed: ${e.message}` };
    }
  }
  return facts;
}
