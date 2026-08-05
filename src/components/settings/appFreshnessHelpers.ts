/**
 * Pure helpers for `AppFreshnessSettings` — the App Registry's per-app
 * auto-fresh configuration.
 *
 * Extracted from the component so the wire-body and save-gating decisions are
 * testable without a DOM (the runner's vitest config is `environment: "node"`;
 * see `LockYieldPolicySettings.test.tsx` for the precedent). Those decisions
 * carry the only real hazards in this panel — see {@link buildPatchBody} and
 * {@link effectiveAfterSave}.
 */

/** Mirrors `qontinui_types::apps::validate_update_strategy`. */
export type UpdateStrategy = "pull_only" | "pull_build";

export const UPDATE_STRATEGIES: readonly UpdateStrategy[] = ["pull_only", "pull_build"];

export const DEFAULT_UPDATE_STRATEGY: UpdateStrategy = "pull_only";

export function isUpdateStrategy(value: string): value is UpdateStrategy {
  return (UPDATE_STRATEGIES as readonly string[]).includes(value);
}

/**
 * The `App` wire shape. Mirrors `qontinui-schemas/rust/src/apps.rs`
 * (`rename_all = "camelCase"`); the runner frontend hand-mirrors Rust types by
 * convention rather than depending on the schemas package — see
 * `src/types/geometry.ts`. Only the fields this panel reads are modelled.
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
 * The `PATCH /apps/:app_id` body. Mirrors the subset of
 * `qontinui_types::apps::UpdateAppRequest` this panel sets.
 *
 * Typed explicitly rather than as `Record<string, string>`: `UpdateAppRequest`
 * has no `deny_unknown_fields`, so serde silently ignores a key it doesn't
 * recognise. A mistyped field would PATCH, return 200, and change nothing.
 */
export interface AppFreshnessPatch {
  updateStrategy: UpdateStrategy;
  buildCommand?: string;
  startCommand?: string;
}

/**
 * Project an app onto its editable form. An unrecognised `updateStrategy`
 * degrades to `pull_only` — the safe direction, since `pull_only` never runs a
 * command. {@link isKnownStrategy} lets the UI say so instead of silently
 * misrepresenting the stored value.
 */
export function formOf(app: RegisteredApp): AppFreshnessForm {
  const strategy = app.updateStrategy ?? DEFAULT_UPDATE_STRATEGY;
  return {
    updateStrategy: isUpdateStrategy(strategy) ? strategy : DEFAULT_UPDATE_STRATEGY,
    buildCommand: app.buildCommand ?? "",
    startCommand: app.startCommand ?? "",
  };
}

/**
 * Does this build recognise the app's stored strategy? `false` means
 * {@link formOf} degraded it, so the select is showing something other than
 * what is stored.
 *
 * Note the row is then NOT dirty — both sides of the comparison degrade
 * identically — so this build cannot change such a value at all. That is the
 * safe outcome (silently downgrading a newer runner's strategy would be
 * worse), and the UI must say so rather than warn about an overwrite that
 * cannot happen.
 */
export function isKnownStrategy(app: RegisteredApp): boolean {
  return app.updateStrategy === undefined || isUpdateStrategy(app.updateStrategy);
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
 * `UpdateAppRequest` is applied server-side with `COALESCE($n, column)`
 * (`database/pg/apps.rs`), so an omitted field leaves the column untouched.
 * Two rules follow, both API facts rather than choices made here:
 *
 * 1. **An empty command is never sent.** The auto-fresh engine runs
 *    `if let Some(cmd) = app.build_command` and checks only the exit status
 *    (`fleet.rs::execute_build_and_restart`); an empty shell command exits 0
 *    on every platform. Persisting `""` would mark an app freshly built
 *    without having built anything, and the `fresh_only` dispatcher would then
 *    route tests to a host serving stale code. Omitting the field is the only
 *    safe encoding of "I left this blank" — and it means a command **cannot be
 *    cleared** through this endpoint, which the panel states rather than
 *    pretending otherwise.
 * 2. **Commands are only sent under `pull_build`.** Otherwise a command typed
 *    while `pull_build` was selected, then abandoned by switching back to
 *    `pull_only`, would be persisted invisibly — the inputs are hidden under
 *    `pull_only` — and executed the moment anyone flipped the app back.
 *
 * `updateStrategy` is always sent: it has a value in every state, and it is
 * what decides whether the commands run at all.
 */
export function buildPatchBody(form: AppFreshnessForm): AppFreshnessPatch {
  const body: AppFreshnessPatch = { updateStrategy: form.updateStrategy };
  if (form.updateStrategy !== "pull_build") return body;
  const build = form.buildCommand.trim();
  const start = form.startCommand.trim();
  if (build) body.buildCommand = build;
  if (start) body.startCommand = start;
  return body;
}

/**
 * The row as it will exist AFTER saving this form — `formOf(app)` with
 * {@link buildPatchBody} applied on top, mirroring the server's COALESCE.
 *
 * Every gate in the panel must be derived from this, not from the raw form,
 * because the two diverge exactly where a blank is omitted. Deriving `dirty`
 * from the form instead lets an operator clear a stored command, see Save
 * enabled, get a success log, and watch the field repopulate — an edit that
 * silently undoes itself. Deriving the "no effective command" warning from the
 * form instead makes it fire for a row whose commands are still stored.
 */
export function effectiveAfterSave(app: RegisteredApp, form: AppFreshnessForm): AppFreshnessForm {
  const stored = formOf(app);
  const patch = buildPatchBody(form);
  return {
    updateStrategy: patch.updateStrategy,
    buildCommand: patch.buildCommand ?? stored.buildCommand,
    startCommand: patch.startCommand ?? stored.startCommand,
  };
}

/**
 * `pull_build` with no command that will actually run.
 *
 * The engine takes neither `if let Some` arm, returns `Ok(())`, and marks the
 * app `fresh` at the new SHA with nothing built — the exact false-fresh state
 * rule 1 of {@link buildPatchBody} exists to prevent, reached via NULL/NULL
 * instead of `""`. Declaring `pull_build` is the operator asserting "pulling
 * is not enough", so the panel refuses to save it rather than annotating it.
 *
 * Takes the EFFECTIVE row, not the form: clearing both inputs on an app whose
 * commands are stored is not this state, because the omitted keys leave them
 * in place.
 */
export function isFalselyFresh(effective: AppFreshnessForm): boolean {
  return (
    effective.updateStrategy === "pull_build" &&
    !effective.buildCommand.trim() &&
    !effective.startCommand.trim()
  );
}
