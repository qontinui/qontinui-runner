/**
 * Builders for termhost-bench results documents, used to generate the committed
 * fixtures (`fixtures/results-*.json`) and as the base the rejection tests mutate.
 * Values are produced through stats.mjs, so a fixture is shaped exactly as the
 * orchestrator will shape real results; degradation ratios are computed from the
 * matching N = 1 rows, never written as constants.
 *
 * Regenerate the committed fixtures (a test fails when they drift):
 *   node scripts/termhost-bench/__tests__/helpers/results.mjs --write
 */

import { writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { KPI_CATALOG, getKpi } from "../../kpi-catalog.mjs";
import { degradationKpi, measureKpi, notApplicableKpi, slope, unknownKpi } from "../../stats.mjs";
import { subjectKey } from "../../validate.mjs";

export const FIXTURES_DIR = join(dirname(fileURLToPath(import.meta.url)), "..", "fixtures");
const DEGRADATION_IDS = KPI_CATALOG.filter((k) => k.degradation_of).map((k) => k.id);

/** n deterministic samples spread over [lo, hi]. */
export function spread(n, lo, hi) {
  return Array.from(
    { length: n },
    (_, i) => lo + ((hi - lo) * ((i * 37) % n)) / Math.max(1, n - 1),
  );
}

const SAMPLES = {
  output_latency_ms: () => spread(600, 8, 70),
  input_latency_ms: () => spread(200, 12, 90),
  frame_interval_ms: () => spread(1800, 15, 40),
  switch_latency_ms: () => spread(30, 40, 160),
  spawn_latency_ms: () => spread(25, 900, 2600),
  cpu_pct: () => spread(120, 20, 95),
  gpu_pct: () => spread(120, 5, 30),
  producer_stall_pct: () => spread(15, 0, 4),
  workload_cpu_pct: () => spread(120, 60, 140),
  compositor_cpu_pct: () => spread(120, 3, 12),
  wsl_vm_cpu_pct: () => spread(120, 10, 25),
};

const VALUES = {
  jank_share: { value: 1.5, n: 1800 },
  cpu_ms_per_mb: { value: 41.2, n: 120 },
  memory_mb: { value: 812.4, n: 120 },
  memory_growth_mb_per_min: { value: 0.8, n: 120 },
  delivered_ratio: { value: 0.998, n: 15 },
  hangs: { value: 0, n: 116 },
  crashes: { value: 0, n: 6 },
  marker_loss: { value: 0.2, n: 9000 },
  marker_skipped: { value: 3.1, n: 9000 },
  workload_memory_mb: { value: 1450, n: 120 },
  compositor_memory_mb: { value: 210, n: 120 },
  wsl_vm_memory_mb: { value: 1900, n: 120 },
};

/**
 * Every KPI measured, except those `overrides` replaces. A degradation_* defaults to
 * unknown(no_baseline_level): only `withDegradations` and a real N = 1 row measure one.
 * A slope over the N ladder defaults to unknown(too_few_levels): only `withLadderSlopes`,
 * which reads the whole file, knows how many N levels there are.
 *
 * @param {Record<string, object>} [overrides] id -> KPI value
 */
export function measuredKpis(overrides = {}) {
  const out = {};
  for (const row of KPI_CATALOG) {
    if (overrides[row.id]) out[row.id] = overrides[row.id];
    else if (row.degradation_of) out[row.id] = unknownKpi(row.id, "no_baseline_level");
    else if (row.needs.includes("n_ladder")) out[row.id] = unknownKpi(row.id, "too_few_levels");
    else if (row.shape === "stats") out[row.id] = measureKpi(row.id, SAMPLES[row.id]());
    else out[row.id] = measureKpi(row.id, VALUES[row.id]);
  }
  return out;
}

/** The row's degradation_* values computed against the matching N = 1 row's kpis. */
export function withDegradations(kpis, kpisAtOne) {
  const out = { ...kpis };
  for (const id of DEGRADATION_IDS) out[id] = degradationKpi(id, kpis, kpisAtOne);
  return out;
}

/**
 * Fill every row's slopes over the N ladder (memory_mb_per_session) from the document's
 * own rows: the least-squares slope of the base KPI over the distinct N of the rows for
 * the same subject and arm (validate.mjs `subjectKey`), measured from `min_n` levels up,
 * else unknown(too_few_levels) with n = the levels the file holds. Mutates and returns doc.
 */
export function withLadderSlopes(doc) {
  for (const kpi of KPI_CATALOG.filter((k) => k.needs.includes("n_ladder"))) {
    const base = kpi.derived_from[0];
    const ladders = new Map();
    for (const row of doc.rows) {
      if (row.kpis[base]?.state !== "measured") continue;
      const key = subjectKey(row);
      if (!ladders.has(key)) ladders.set(key, new Map());
      ladders.get(key).set(row.scenario.n, row.kpis[base].value);
    }
    for (const row of doc.rows) {
      const ladder = ladders.get(subjectKey(row)) ?? new Map();
      const xs = [...ladder.keys()];
      row.kpis[kpi.id] =
        ladder.size >= getKpi(kpi.id).min_n
          ? measureKpi(
              kpi.id,
              slope(
                xs,
                xs.map((x) => ladder.get(x)),
              ),
            )
          : unknownKpi(kpi.id, "too_few_levels", { n: ladder.size });
    }
  }
  return doc;
}

/** An N = 1 row's degradation_* values: it is the baseline itself. */
export function baselineDegradations() {
  return Object.fromEntries(
    DEGRADATION_IDS.map((id) => [id, notApplicableKpi(id, "baseline_level")]),
  );
}

export const RUN = {
  id: "2026-10-11T12-00-00Z-fixture",
  started_at: "2026-10-11T12:00:00Z",
  finished_at: "2026-10-11T12:42:10.500Z",
  harness_commit: "94d141a980a349c97b5f679e1f4745a2a9b9d919",
  node_version: "v22.20.0",
  command: ["node", "scripts/termhost-bench/cli.mjs", "run", "--env", "windows-desktop"],
};

export function setup(id, overrides = {}) {
  const base = {
    runner: {
      id: "runner",
      variant: "default",
      app_version: "0.1.0",
      build: {
        commit: "94d141a980a349c97b5f679e1f4745a2a9b9d919",
        source: "worktree",
        role: "single",
      },
      settings_digest: { state: "unknown", reason: "not_instrumented" },
      pty_host_impl: "system_conhost",
      render_backend: "xterm",
      pty_holder: false,
      launch_mode: "supervisor_spawn_test",
      session_shell: "powershell.exe -NoLogo -NoExit",
      shell_integration: "injected",
    },
    vscode: {
      id: "vscode",
      variant: "default",
      app_version: "1.105.0",
      build: { state: "not_applicable", reason: "not_applicable_to_setup" },
      settings_digest: `sha256:${"ab".repeat(32)}`,
      pty_host_impl: "bundled_openconsole",
      render_backend: "webgl",
      pty_holder: { state: "not_applicable", reason: "not_applicable_to_setup" },
      launch_mode: "portable_pinned",
      session_shell: "powershell.exe -NoLogo -NoExit",
      shell_integration: "not_injected",
    },
    tmux: {
      id: "tmux",
      variant: "default",
      app_version: "3.2a",
      build: { state: "not_applicable", reason: "not_applicable_to_setup" },
      settings_digest: `sha256:${"cd".repeat(32)}`,
      pty_host_impl: "unix_pty",
      render_backend: { state: "not_applicable", reason: "not_applicable_to_setup" },
      pty_holder: { state: "not_applicable", reason: "not_applicable_to_setup" },
      launch_mode: "private_server",
      session_shell: "bash",
      shell_integration: { state: "unknown", reason: "not_instrumented" },
    },
  }[id];
  return { ...base, ...overrides };
}

export function scenario(overrides = {}) {
  return {
    profile: "claude-tui",
    profile_calibrated: false,
    profile_provenance: "assumption",
    profile_digest: `sha256:${"12".repeat(32)}`,
    n: 15,
    arm: "tiled",
    v_target: 9,
    v_actual: 9,
    warmup_ms: 10000,
    soak_ms: 60000,
    seed: 1,
    run_index: 0,
    run_count: 3,
    ...overrides,
  };
}

export function row({
  setupId = "runner",
  setupOverrides = {},
  scenarioOverrides = {},
  kpis = measuredKpis(),
  labels = [],
  diagnostics = {},
  incidents = [],
} = {}) {
  return {
    setup: setup(setupId, setupOverrides),
    scenario: scenario(scenarioOverrides),
    kpis,
    diagnostics,
    labels,
    incidents,
  };
}

/**
 * A complete windows-desktop results document: rows 0-2 are N = 1 focus (runner),
 * N = 15 tiled (runner) and N = 15 tiled (vscode); rows 3-4 are the N = 1 tiled
 * baselines the two N = 15 rows' degradations are computed against.
 */
export function windowsDesktopResults() {
  const n1Focus = { n: 1, v_target: 1, v_actual: 1, arm: "focus" };
  const n1Tiled = { n: 1, v_target: 1, v_actual: 1, arm: "tiled" };
  const naWsl = {
    wsl_vm_cpu_pct: notApplicableKpi("wsl_vm_cpu_pct", "not_applicable_to_setup"),
    wsl_vm_memory_mb: notApplicableKpi("wsl_vm_memory_mb", "not_applicable_to_setup"),
  };
  const atN15 = {
    output_latency_ms: measureKpi("output_latency_ms", spread(600, 12, 140)),
    input_latency_ms: measureKpi("input_latency_ms", spread(200, 18, 170)),
    cpu_pct: measureKpi("cpu_pct", spread(120, 90, 380)),
  };
  const runnerBase = measuredKpis({ ...baselineDegradations(), ...naWsl });
  const vscodeBase = measuredKpis({ ...baselineDegradations(), ...naWsl });
  return withLadderSlopes({
    schema: "termhost-bench/v1",
    run: RUN,
    machine: {
      name: "fixture-workstation",
      environment: "windows-desktop",
      os: { platform: "win32", release: "10.0.26300", build: "26300" },
      cpu: { model: "Fixture CPU 16-core", logical_cores: 32 },
      ram_mb: 65536,
      gpu: [{ name: "Fixture GPU", driver: "32.0.15", wddm: "3.2" }],
      display: { width_px: 2560, height_px: 1440, refresh_hz: 144, scale_pct: 125 },
      background_load: { cpu_pct: 3.5, memory_used_mb: 18000, sampled_ms: 10000 },
    },
    capabilities: {
      "capture.wgc": {
        state: "OPERATIVE",
        reason: "per-window capture frames arrive",
        checked_at: "2026-10-11T11:59:00Z",
      },
      "capture.dda": { state: "OPERATIVE", reason: "output duplication available" },
      "input.safe": { state: "DEGRADED", reason: "raw input listener up; foreground check slow" },
      "counters.gpu": { state: "OPERATIVE", reason: "GPU Engine counters enumerated" },
      "session.interactive": { state: "OPERATIVE", reason: "unlocked interactive desktop" },
    },
    rows: [
      row({
        scenarioOverrides: n1Focus,
        kpis: measuredKpis({ ...baselineDegradations(), ...naWsl }),
      }),
      row({
        kpis: withDegradations(measuredKpis({ ...naWsl, ...atN15 }), runnerBase),
        diagnostics: {
          "health.ui_thread_wedged": {
            state: "measured",
            comparable: false,
            source: "/health uiThread.wedged",
            data: { wedged: false },
          },
          webgl_panes: {
            state: "measured",
            comparable: false,
            source: "renderer census",
            unit: "panes",
            value: 8,
            n: 1,
          },
          backend_echo_latency_ms: {
            state: "measured",
            comparable: false,
            source: "GET /terminals/{id}/ws round trip",
            unit: "ms",
            n: 200,
            stats: { p50: 1.9, p95: 4.4 },
          },
          transport_counters: {
            state: "unknown",
            comparable: false,
            source: "GET /terminals/transport-stats",
            reason: "not_instrumented",
          },
        },
      }),
      row({
        setupId: "vscode",
        kpis: withDegradations(
          measuredKpis({
            ...naWsl,
            ...atN15,
            producer_stall_pct: measureKpi("producer_stall_pct", spread(15, 0, 12), {
              coverage: { observed: 15, expected: 15 },
            }),
          }),
          vscodeBase,
        ),
      }),
      row({ scenarioOverrides: n1Tiled, kpis: runnerBase }),
      row({ setupId: "vscode", scenarioOverrides: n1Tiled, kpis: vscodeBase }),
    ],
    incidents: [],
  });
}

/** A complete linux-xvfb results document with honest unknowns. */
export function linuxXvfbResults() {
  const unknownCapture = (id) =>
    unknownKpi(id, "occluded", {
      n: 12,
      detail: "the window lost the foreground for most of the soak",
    });
  return withLadderSlopes({
    schema: "termhost-bench/v1",
    run: {
      ...RUN,
      id: "2026-10-11T13-00-00Z-xvfb",
      command: ["node", "scripts/termhost-bench/cli.mjs", "run", "--env", "linux-xvfb"],
    },
    machine: {
      name: "github-hosted-ubuntu-22.04",
      environment: "linux-xvfb",
      os: {
        platform: "linux",
        release: "6.5.0-1025-azure",
        build: { state: "not_applicable", reason: "not_applicable_to_environment" },
      },
      cpu: { model: "AMD EPYC 7763 64-Core Processor", logical_cores: 4 },
      ram_mb: 16384,
      gpu: { state: "not_applicable", reason: "not_applicable_to_environment" },
      display: { width_px: 1920, height_px: 1080, refresh_hz: 60, scale_pct: 100 },
      background_load: { state: "unknown", reason: "not_instrumented" },
    },
    capabilities: {
      "capture.x11": { state: "OPERATIVE", reason: "damage + shm on :99" },
      "input.safe": { state: "OPERATIVE", reason: "private Xvfb display" },
      "counters.gpu": { state: "INOPERATIVE-ON-THIS-MACHINE", reason: "no GPU (llvmpipe)" },
    },
    rows: [
      row({
        setupOverrides: {
          session_shell: "bash",
          pty_host_impl: "unix_pty",
          launch_mode: "subject_instance_root",
          build: {
            commit: "94d141a980a349c97b5f679e1f4745a2a9b9d919",
            source: "artifact",
            role: "head",
          },
        },
        scenarioOverrides: {
          n: 5,
          arm: "focus",
          v_target: 1,
          v_actual: { state: "unknown", reason: "sessions_not_rendered" },
          run_count: 2,
        },
        labels: ["software_rendered"],
        // No N = 1 row in this file: every degradation is honestly unknown.
        kpis: withDegradations(
          measuredKpis({
            gpu_pct: notApplicableKpi("gpu_pct", "gpu_counters_unavailable"),
            hangs: unknownCapture("hangs"),
            marker_loss: unknownCapture("marker_loss"),
            marker_skipped: unknownCapture("marker_skipped"),
            input_latency_ms: unknownKpi("input_latency_ms", "insufficient_samples", { n: 40 }),
            wsl_vm_cpu_pct: notApplicableKpi("wsl_vm_cpu_pct", "not_applicable_to_setup"),
            wsl_vm_memory_mb: notApplicableKpi("wsl_vm_memory_mb", "not_applicable_to_setup"),
          }),
          undefined,
        ),
      }),
      row({
        setupId: "tmux",
        scenarioOverrides: { n: 5, arm: "focus", v_target: 1, v_actual: 1, run_count: 2 },
        labels: ["software_rendered", "assert_only_smoke"],
        kpis: withDegradations(
          measuredKpis({
            gpu_pct: notApplicableKpi("gpu_pct", "gpu_counters_unavailable"),
            switch_latency_ms: unknownKpi("switch_latency_ms", "harness_error", {
              detail: "select-window timed out",
            }),
            wsl_vm_cpu_pct: notApplicableKpi("wsl_vm_cpu_pct", "not_applicable_to_setup"),
            wsl_vm_memory_mb: notApplicableKpi("wsl_vm_memory_mb", "not_applicable_to_setup"),
          }),
          undefined,
        ),
        incidents: [
          {
            kind: "harness_error",
            at: "2026-10-11T13:05:00Z",
            detail: "select-window timed out once",
            stops_run: false,
          },
        ],
      }),
    ],
    incidents: [],
  });
}

/** The committed fixtures: file name -> exact bytes. */
export function renderFixtures() {
  return {
    "results-windows-desktop.json": `${JSON.stringify(windowsDesktopResults(), null, 2)}\n`,
    "results-linux-xvfb.json": `${JSON.stringify(linuxXvfbResults(), null, 2)}\n`,
  };
}

const invokedDirectly =
  process.argv[1] && resolve(process.argv[1]) === resolve(fileURLToPath(import.meta.url));
if (invokedDirectly && process.argv.includes("--write")) {
  for (const [name, text] of Object.entries(renderFixtures())) {
    writeFileSync(join(FIXTURES_DIR, name), text);
    process.stdout.write(`wrote ${join(FIXTURES_DIR, name)}\n`);
  }
}
