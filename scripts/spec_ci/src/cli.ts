#!/usr/bin/env node
/**
 * spec-ci-run — walk an IrPageSpec's transitions against a running app and
 * emit a CI-friendly pass/fail report.
 *
 * Pipeline:
 *   1. Fetch the IR via runner Spec API.
 *   2. Adapt IR → AdaptedWorkflowConfig (uses qontinui-schemas/ts adapter).
 *   3. Build StateMachine + StateDetector from the adapted shape.
 *   4. Snapshot the connected app, detect the initial active state set.
 *   5. For each transition (skipping `effect: "destructive"` unless opted in):
 *      a. executeTransition() — drives actions via HttpActionExecutor.
 *      b. Re-snapshot, re-detect.
 *      c. Record whether activateStates became active and exitStates left.
 *   6. Emit report.
 */

import { writeFileSync } from "node:fs";
import { adaptIRDocumentToWorkflowConfig } from "@qontinui/shared-types/ui-bridge-ir";
import type { IRDocument } from "@qontinui/shared-types/ui-bridge-ir";
import {
  StateMachine,
  StateDetector,
  executeTransition,
} from "@qontinui/ui-bridge-auto/runtime";
import { HttpRegistry, type BridgeKind } from "./http-registry.js";
import { HttpActionExecutor, navigateRunner } from "./http-action-executor.js";

// ---------------------------------------------------------------------------
// CLI args
// ---------------------------------------------------------------------------

interface Args {
  runner: string;
  app: string;
  page: string;
  output?: string;
  pageUrl?: string;
  bridge: BridgeKind;
  includeDestructive: boolean;
}

function parseArgs(argv: string[]): Args {
  const args: Partial<Args> = { includeDestructive: false, bridge: "sdk" };
  for (let i = 0; i < argv.length; i++) {
    const flag = argv[i];
    const value = argv[i + 1];
    switch (flag) {
      case "--runner":
        args.runner = value;
        i++;
        break;
      case "--app":
        args.app = value;
        i++;
        break;
      case "--page":
        args.page = value;
        i++;
        break;
      case "--output":
        args.output = value;
        i++;
        break;
      case "--page-url":
        args.pageUrl = value;
        i++;
        break;
      case "--bridge":
        if (value !== "sdk" && value !== "control") {
          throw new Error(`--bridge must be 'sdk' or 'control' (got ${value})`);
        }
        args.bridge = value;
        i++;
        break;
      case "--include-destructive":
        args.includeDestructive = true;
        break;
      case "-h":
      case "--help":
        printHelp();
        process.exit(0);
      default:
        if (flag.startsWith("--")) {
          throw new Error(`Unknown flag: ${flag}`);
        }
    }
  }
  if (!args.runner || !args.app || !args.page) {
    printHelp();
    throw new Error("--runner, --app, and --page are required");
  }
  return args as Args;
}

function printHelp(): void {
  process.stderr.write(
    [
      "spec-ci-run — walk an IrPageSpec's transitions and emit a CI report",
      "",
      "Usage:",
      "  spec-ci-run --runner <url> --app <id> --page <pageId> [--output <path>]",
      "             [--page-url <url>] [--include-destructive]",
      "",
      "Flags:",
      "  --runner <url>             Runner base URL (e.g. http://localhost:9876)",
      "  --app <id>                 App id registered on the runner",
      "  --page <pageId>            Spec page id (the slug, not the URL path)",
      "  --output <path>            Write report JSON here (default stdout)",
      "  --page-url <url>           Navigate the SDK to this URL before evaluating",
      "  --bridge <sdk|control>     Bridge surface to target (default: sdk).",
      "                             Use 'control' to drive the runner's own webview",
      "                             (always connected); use 'sdk' for external apps.",
      "  --include-destructive      Run transitions tagged effect=destructive",
      "",
    ].join("\n"),
  );
}

// ---------------------------------------------------------------------------
// IR fetch
// ---------------------------------------------------------------------------

/**
 * Fetch the raw IR document for a spec page. The Spec API exposes the file
 * via `GET /apps/<app>/spec/get?path=pages/<id>/state-machine.derived.json`
 * (raw file contents, path-traversal protected). The sibling
 * `/spec/page/<id>` endpoint returns a different projection shape with
 * `groups` instead of `states` — we want raw IR so the adapter sees the
 * shape it expects.
 *
 * Error envelopes from `spec/get` are flat: `{ ok: false, reason: "...",
 * detail?: ... }`. Surface the reason in the thrown error so CI logs are
 * actionable.
 */
async function fetchIR(runner: string, app: string, page: string): Promise<IRDocument> {
  const path = `pages/${page}/state-machine.derived.json`;
  const url = `${runner.replace(/\/$/, "")}/apps/${encodeURIComponent(app)}/spec/get?path=${encodeURIComponent(path)}`;
  const resp = await fetch(url);
  if (!resp.ok) {
    const text = await resp.text();
    throw new Error(`spec/get HTTP ${resp.status}: ${text.slice(0, 200)}`);
  }
  const body = (await resp.json()) as IRDocument & { ok?: boolean; reason?: string };
  if (body.ok === false || !body.states || !body.transitions) {
    throw new Error(
      `spec/get returned no IR for app=${app} page=${page} (reason: ${body.reason ?? "missing states/transitions"})`,
    );
  }
  return body as IRDocument;
}

// ---------------------------------------------------------------------------
// Report shape
// ---------------------------------------------------------------------------

interface TransitionReport {
  id: string;
  name: string;
  fromStates: string[];
  activateStates: string[];
  exitStates: string[];
  effect: string | null;
  executed: boolean;
  passed: boolean;
  durationMs: number;
  missingActivateStates: string[];
  unexpectedExitStatesStillActive: string[];
  error: string | null;
}

interface Report {
  runnerUrl: string;
  appId: string;
  pageId: string;
  evaluatedAt: string;
  initialStates: string[];
  finalStates: string[];
  transitions: TransitionReport[];
  summary: {
    totalTransitions: number;
    executedTransitions: number;
    passedTransitions: number;
    skippedDestructive: number;
    errorTransitions: number;
    passRate: number;
  };
}

// ---------------------------------------------------------------------------
// Driver
// ---------------------------------------------------------------------------

async function run(args: Args): Promise<Report> {
  // 1. Fetch the raw IR (we need it for `effect` which the adapter strips).
  const ir = await fetchIR(args.runner, args.app, args.page);
  const adapted = adaptIRDocumentToWorkflowConfig(ir);

  // 2. Build the in-memory machine.
  const machine = new StateMachine();
  machine.defineStates(adapted.states);
  machine.defineTransitions(adapted.transitions);

  // 3. Open the registry + executor. The registry's first refresh doubles as
  //    a precondition check: if the SDK isn't connected we bail with a
  //    structured error rather than producing nonsense pass/fail data.
  const registry = new HttpRegistry(args.runner, args.bridge);

  if (args.pageUrl) {
    await navigateRunner(args.runner, args.pageUrl, args.bridge);
  }

  const initialSnapshot = await registry.refresh();
  if (!initialSnapshot.sdkConnected) {
    throw new Error(
      `SDK not connected to ${args.app} — refusing to author results. Hint: ${initialSnapshot.fallbackNote ?? "no detail from runner"}`,
    );
  }

  const detector = new StateDetector(machine, registry);
  detector.evaluate();
  const initialStates = Array.from(machine.getActiveStates()).sort();

  const executor = new HttpActionExecutor(args.runner, registry, args.bridge);

  // 4. Walk every transition in declaration order. We don't pathfind here —
  //    the CLI exercises each transition individually so a single failure
  //    doesn't cascade. State activation is checked *after* the transition's
  //    actions run against whatever state was active going in.
  const transitionReports: TransitionReport[] = [];
  let skippedDestructive = 0;

  for (const t of ir.transitions) {
    const effect = t.effect ?? null;
    const adaptedT = adapted.transitions.find((x) => x.id === t.id);
    if (!adaptedT) {
      transitionReports.push(emptyTransitionReport(t.id, t.name, effect, "transition missing from adapter output"));
      continue;
    }

    if (effect === "destructive" && !args.includeDestructive) {
      skippedDestructive++;
      transitionReports.push({
        id: t.id,
        name: t.name,
        fromStates: adaptedT.fromStates,
        activateStates: adaptedT.activateStates,
        exitStates: adaptedT.exitStates,
        effect,
        executed: false,
        passed: false,
        durationMs: 0,
        missingActivateStates: [],
        unexpectedExitStatesStillActive: [],
        error: "skipped: effect=destructive (pass --include-destructive to run)",
      });
      continue;
    }

    const started = Date.now();
    let error: string | null = null;
    let executed = false;

    try {
      await executeTransition(adaptedT, executor);
      executed = true;
    } catch (err) {
      error = err instanceof Error ? err.message : String(err);
    }

    // Force a final refresh + re-evaluate even on error; an aborted
    // transition may have completed half its actions, leaving the UI in a
    // partial-success state we want to record.
    try {
      await registry.refresh();
      detector.evaluate();
    } catch (err) {
      const detail = err instanceof Error ? err.message : String(err);
      error = error ? `${error} | post-detect: ${detail}` : `post-detect: ${detail}`;
    }

    const active = machine.getActiveStates();
    const missingActivateStates = adaptedT.activateStates.filter(
      (s) => !active.has(s),
    );
    const unexpectedExitStatesStillActive = adaptedT.exitStates.filter((s) =>
      active.has(s),
    );

    const passed =
      executed &&
      error === null &&
      missingActivateStates.length === 0 &&
      unexpectedExitStatesStillActive.length === 0;

    transitionReports.push({
      id: t.id,
      name: t.name,
      fromStates: adaptedT.fromStates,
      activateStates: adaptedT.activateStates,
      exitStates: adaptedT.exitStates,
      effect,
      executed,
      passed,
      durationMs: Date.now() - started,
      missingActivateStates,
      unexpectedExitStatesStillActive,
      error,
    });
  }

  detector.dispose();

  // 5. Build the report.
  const finalStates = Array.from(machine.getActiveStates()).sort();
  const executedCount = transitionReports.filter((r) => r.executed).length;
  const passedCount = transitionReports.filter((r) => r.passed).length;
  const errorCount = transitionReports.filter((r) => r.error !== null && !r.error.startsWith("skipped:")).length;

  return {
    runnerUrl: args.runner,
    appId: args.app,
    pageId: args.page,
    evaluatedAt: new Date().toISOString(),
    initialStates,
    finalStates,
    transitions: transitionReports.sort((a, b) => a.id.localeCompare(b.id)),
    summary: {
      totalTransitions: ir.transitions.length,
      executedTransitions: executedCount,
      passedTransitions: passedCount,
      skippedDestructive,
      errorTransitions: errorCount,
      passRate:
        executedCount === 0 ? 0 : passedCount / executedCount,
    },
  };
}

function emptyTransitionReport(
  id: string,
  name: string,
  effect: string | null,
  error: string,
): TransitionReport {
  return {
    id,
    name,
    fromStates: [],
    activateStates: [],
    exitStates: [],
    effect,
    executed: false,
    passed: false,
    durationMs: 0,
    missingActivateStates: [],
    unexpectedExitStatesStillActive: [],
    error,
  };
}

// ---------------------------------------------------------------------------
// Entrypoint
// ---------------------------------------------------------------------------

async function main(): Promise<number> {
  let args: Args;
  try {
    args = parseArgs(process.argv.slice(2));
  } catch (err) {
    process.stderr.write(`${err instanceof Error ? err.message : String(err)}\n`);
    return 2;
  }

  try {
    const report = await run(args);
    const out = JSON.stringify(report, null, 2);
    if (args.output) {
      writeFileSync(args.output, out, "utf-8");
      process.stderr.write(
        `[spec-ci] wrote ${args.output} (${report.summary.passedTransitions}/${report.summary.executedTransitions} passed, ${report.summary.skippedDestructive} destructive-skipped, ${report.summary.errorTransitions} errored)\n`,
      );
    } else {
      process.stdout.write(out + "\n");
    }
    // CI gate: exit non-zero iff any executed transition failed.
    return report.summary.errorTransitions > 0 ||
      (report.summary.executedTransitions > 0 &&
        report.summary.passedTransitions < report.summary.executedTransitions)
      ? 1
      : 0;
  } catch (err) {
    process.stderr.write(
      `[spec-ci] fatal: ${err instanceof Error ? err.message : String(err)}\n`,
    );
    return 1;
  }
}

main().then((code) => process.exit(code));
