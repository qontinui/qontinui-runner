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
 * Build the `PATCH /apps/:app_id` body for a form, given the app it edits.
 *
 * The server normalizes each command with `normalize_command`
 * (`database/pg/apps.rs`) onto three states, and this function is what selects
 * between them:
 *
 * | Emitted | Server does |
 * |---|---|
 * | key omitted | leaves the column exactly as it is |
 * | `""` | **clears** the column |
 * | `"npm run build"` | stores it, trimmed |
 *
 * Two rules, both about not persisting something the operator cannot see:
 *
 * 1. **A blank is sent only as a deliberate clear** — when the input is blank
 *    AND the stored value is not. Blanking an already-empty command emits
 *    nothing, so a no-op edit never produces a write.
 * 2. **Under `pull_only`, a *set* is omitted but a *clear* is sent.** The
 *    command inputs are hidden under `pull_only`, so persisting a value typed
 *    under `pull_build` and then abandoned would store a command the operator
 *    cannot see — which the engine would run the moment anyone flipped the app
 *    back. Clearing has the opposite risk profile: it removes a hidden value
 *    rather than creating one, and it is the whole point of the "switch this
 *    app's shape" flow, so it is allowed through.
 *
 * `updateStrategy` is always sent: it has a value in every state, and it is
 * what decides whether the commands run at all.
 *
 * Takes `app` because rule 1 cannot be evaluated from the form alone — "is
 * there anything to clear?" is a question about the stored row.
 */
export function buildPatchBody(app: RegisteredApp, form: AppFreshnessForm): AppFreshnessPatch {
  const body: AppFreshnessPatch = { updateStrategy: form.updateStrategy };
  const stored = formOf(app);
  const isBuild = form.updateStrategy === "pull_build";

  const resolve = (input: string, storedValue: string): string | undefined => {
    const trimmed = input.trim();
    if (trimmed) return isBuild ? trimmed : undefined;
    // Blank input: a clear, but only if there is something stored to clear.
    return storedValue.trim() ? "" : undefined;
  };

  const build = resolve(form.buildCommand, stored.buildCommand);
  const start = resolve(form.startCommand, stored.startCommand);
  if (build !== undefined) body.buildCommand = build;
  if (start !== undefined) body.startCommand = start;
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
  const patch = buildPatchBody(app, form);
  return {
    updateStrategy: patch.updateStrategy,
    // `??` is correct for all three states: an omitted key is `undefined` and
    // falls through to the stored value, while an emitted `""` is NOT nullish
    // and therefore yields the cleared value.
    buildCommand: patch.buildCommand ?? stored.buildCommand,
    startCommand: patch.startCommand ?? stored.startCommand,
  };
}

/**
 * `pull_build` with no command that will actually run.
 *
 * The engine now REFUSES this configuration outright
 * (`fleet.rs::execute_build_and_restart` returns `Err`, so the app records
 * `failed` rather than `fresh`), and the panel refuses to create it — declaring
 * `pull_build` is the operator asserting "pulling is not enough", so an
 * apparently-successful save that guarantees a failed refresh is worse than a
 * blocked button.
 *
 * Takes the EFFECTIVE row, not the form: what matters is what will be stored
 * after the PATCH, not what the inputs currently show.
 */
export function isFalselyFresh(effective: AppFreshnessForm): boolean {
  return (
    effective.updateStrategy === "pull_build" &&
    !effective.buildCommand.trim() &&
    !effective.startCommand.trim()
  );
}
