/**
 * Pure helpers for `AppFreshnessSettings` — the App Registry's per-app
 * auto-fresh configuration.
 *
 * Extracted from the component so the wire-body decision is testable without a
 * DOM (the runner's vitest config is `environment: "node"`; see
 * `LockYieldPolicySettings.test.tsx` for the precedent). That decision carries
 * the only real hazard in this panel — see {@link buildPatchBody}.
 */

/** Mirrors `qontinui_types::apps::validate_update_strategy`. */
export type UpdateStrategy = "pull_only" | "pull_build";

export const UPDATE_STRATEGIES: readonly UpdateStrategy[] = ["pull_only", "pull_build"];

export const DEFAULT_UPDATE_STRATEGY: UpdateStrategy = "pull_only";

export function isUpdateStrategy(value: string): value is UpdateStrategy {
  return (UPDATE_STRATEGIES as readonly string[]).includes(value);
}

/**
 * The `App` wire shape (`qontinui-schemas` `apps.rs`, `rename_all =
 * "camelCase"`). Only the fields this panel reads are modelled.
 *
 * `updateStrategy` is a plain string with a serde default, not an enum — a
 * value from a newer runner must render, not crash.
 */
export interface RegisteredApp {
  appId: string;
  repoRoot: string;
  displayName: string;
  updateStrategy?: string;
  buildCommand?: string | null;
  startCommand?: string | null;
}

export interface AppListResponse {
  ok: boolean;
  apps: RegisteredApp[];
}

/** The editable subset, so "dirty" is a real comparison rather than a flag. */
export interface AppFreshnessForm {
  updateStrategy: UpdateStrategy;
  buildCommand: string;
  startCommand: string;
}

/**
 * Project an app onto its editable form. An unrecognised `updateStrategy`
 * degrades to `pull_only` — the safe direction, since `pull_only` never runs a
 * command.
 */
export function formOf(app: RegisteredApp): AppFreshnessForm {
  const strategy = app.updateStrategy ?? DEFAULT_UPDATE_STRATEGY;
  return {
    updateStrategy: isUpdateStrategy(strategy) ? strategy : DEFAULT_UPDATE_STRATEGY,
    buildCommand: app.buildCommand ?? "",
    startCommand: app.startCommand ?? "",
  };
}

export function sameForm(a: AppFreshnessForm, b: AppFreshnessForm): boolean {
  return (
    a.updateStrategy === b.updateStrategy &&
    a.buildCommand === b.buildCommand &&
    a.startCommand === b.startCommand
  );
}

/**
 * Build the `PATCH /apps/:app_id` body for a form.
 *
 * `UpdateAppRequest` is applied server-side with `COALESCE($n, column)`, so an
 * omitted field leaves the column untouched. Two consequences, both API facts
 * rather than choices made here:
 *
 * 1. **An empty command is never sent.** The auto-fresh engine runs
 *    `if let Some(cmd) = app.build_command`, and an empty shell command exits
 *    0 on every platform — so persisting `""` would mark an app freshly built
 *    without having built anything, and the `fresh_only` dispatcher would then
 *    route tests to a host serving stale code. Omitting the field is the only
 *    safe encoding of "I left this blank".
 * 2. **A command cannot be cleared** through this endpoint; there is no value
 *    meaning "set back to NULL". The panel says so rather than pretending.
 *
 * `updateStrategy` is always sent: it is the one field with a non-null value
 * in every state, and it is what decides whether the commands run at all.
 */
export function buildPatchBody(form: AppFreshnessForm): Record<string, string> {
  const body: Record<string, string> = { updateStrategy: form.updateStrategy };
  const build = form.buildCommand.trim();
  const start = form.startCommand.trim();
  if (build) body.buildCommand = build;
  if (start) body.startCommand = start;
  return body;
}
