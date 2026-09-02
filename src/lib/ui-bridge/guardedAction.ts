/**
 * guardedAction — the ONE way a UI Bridge action reads caller-supplied params.
 *
 * ## The class this closes
 *
 * A UI Bridge action's `handler` receives `params?: unknown` straight off the
 * wire. Written the natural way,
 *
 *     handler: async (params?: unknown) => {
 *       const { count = 1 } = (params ?? {}) as { count?: number };
 *       …effect…
 *     }
 *
 * the destructuring IS the validation, and it is not validation at all:
 *
 *   - `Object.entries(5)` is `[]`, so a NON-OBJECT bag destructures to the
 *     defaults and the action runs BARE. `create-plain(5)` therefore answered
 *     `success: true` and spawned a PTY for input its own schema calls a
 *     number; `create-best-account(5)` spawned one AND wrote a
 *     `claude --session-id … --config-dir …` line into it — a costly effect
 *     from a bag nobody checked.
 *   - an UNDECLARED key is silently dropped, so
 *     `create-with-command({command: "echo pwn", zzz: "x"})` reported success
 *     over a key the action does not have.
 *   - a NON-SCALAR value is truthy, so `{name: {}}` sails past `if (!name)`
 *     and becomes the computed key `"[object Object]"` in a persisted map, or
 *     `[object Object]` typed into a live PTY, or `Er.replace is not a
 *     function` shown to an operator.
 *
 * PR #1301 fixed four such surfaces by hand and left three siblings open IN
 * THE SAME `actions: [...]` ARRAY, while asserting in its own docstring that
 * they were closed. That is the eleventh recurrence of one failure: *a fix
 * passed review because the defective shape lived somewhere the fix did not
 * reach*. Fixing the instances you can see is what has failed eleven times.
 *
 * So the fix here is not another sweep. It is a shape that CANNOT be written
 * unguarded, plus a mechanical enumeration that goes red when a new one is
 * (`actionSurfaces.ts` + `actionSurfaces.enforcement.test.ts`).
 *
 * ## The shape
 *
 * A guarded action declares a `paramSchema` and a `run(args)`. It never sees
 * `params`. {@link bindSchemaBag} — the same binder `callRegistry` →
 * `bindDirect` gives every registry command — has already
 *
 *   1. refused a non-object bag (`5`, `"zz"`, `[]`, `null`),
 *   2. refused any value that is not text or a finite number,
 *   3. refused any key the `paramSchema` does not declare,
 *
 * by the time `run` is entered. A refusal throws BEFORE `run`, which is the
 * only point at which an argument the handler cannot read is still cheap to
 * reject: no PTY, no `terminal_write`, no `setting_set`, no React re-render.
 *
 * ## Why `run` takes a bag rather than typed fields
 *
 * `bindSchemaBag` coerces a clean numeric token, so `{count: "2"}` arrives as
 * the number `2` on every route — the same reading Tier 1 applies to typed
 * text. That means `run` must read text fields through `parse.ts`'s `textArg`
 * (which turns `2` back into `"2"`) rather than trusting a `string` type
 * annotation. Handing `run` a pre-typed struct would have to pick one reading
 * and would be wrong for the other; handing it the bag keeps the choice at the
 * one place that knows which field is which.
 *
 * ## What this does NOT do
 *
 * It does not check that a REQUIRED field is present, or that a number is in
 * range, or that a name exists in a map. Those are the action's own semantics
 * and stay in `run`, where the operator-facing sentence for them already
 * lives. This owns exactly the three refusals above — the ones that are the
 * same on every surface and were therefore re-derived, differently, on each.
 *
 * And it only governs surfaces that are WRITTEN through it. An action whose
 * handler declares no parameter cannot be INFLUENCED by a bag and so is not
 * required to route through here — but it does still answer `✓` for a key it
 * does not have, which is weaker than the "enforced rather than merely
 * documented" standard argued above, and on eight measured surfaces the call
 * is not even inert: `terminal-page.create-terminal({zzz: "x"})` answers `✓`
 * over a key it does not have AND spawns a PTY. That residual is measured,
 * scoped and listed in `actionSurfaces.ts`; it is not closed by this module,
 * and this comment must not be read as though it were.
 */

import { bindSchemaBag } from "@/components/terminal/commands/bind";

/** The `paramSchema` map form: field name → human-readable type sentence. */
export type ParamSchema = Record<string, string>;

/**
 * A component-level action as `useUIComponent({ actions: [...] })` takes it.
 *
 * Declared structurally rather than imported from the SDK so this module has
 * no runtime dependency on `@qontinui/ui-bridge` — the enforcement test and
 * the unit tests import it under vitest's `environment: "node"`, where the
 * SDK's DOM-touching entry points cannot load. Same reasoning as
 * `terminalKeySequence.ts`'s leaf-module note.
 */
export interface GuardedComponentAction {
  id: string;
  label?: string;
  description?: string;
  paramSchema?: Record<string, unknown>;
  handler: (params?: unknown) => unknown;
}

/** An element-level custom action as a `customActions: {...}` value takes it. */
export interface GuardedCustomAction {
  id: string;
  description?: string;
  paramSchema?: Record<string, unknown>;
  handler: (params?: unknown) => unknown;
}

/** What an author writes instead of a `handler`. */
export interface GuardedActionSpec<TResult> {
  id: string;
  label?: string;
  description?: string;
  /**
   * Every argument this action accepts. REQUIRED, and `{}` is a meaningful
   * value: an action that declares `{}` refuses every supplied key, which is
   * what "takes no arguments" has to mean if it is to be enforced rather than
   * merely documented. Omitting it is not an option the type offers, because
   * an omitted schema is how "this one is different" starts.
   */
  paramSchema: ParamSchema;
  /**
   * Declared fields whose VALUE is passed to `run` un-coerced, because their
   * contract genuinely admits a list or an object.
   *
   * There is exactly one today: `sendKeys`'s `keys`, whose SDK contract is a
   * raw string OR an array of key names OR an array of `{key, modifiers}`
   * descriptors. Refusing the two array forms would break the SDK's canonical
   * spelling, so the field is exempted from per-value coercion — and ONLY
   * from that. The bag must still be an object and every key must still be
   * declared, which is what `sendKeys` never had.
   *
   * Taking this exemption is a promise that the field has a validator of its
   * own. `keys` has `terminalKeySequence.ts::toPtySequence`, which throws
   * rather than typing an untranslatable key's own name into a live PTY. A
   * field listed here that is NOT in `paramSchema` is ignored and then refused
   * as undeclared — an action cannot widen itself past its own schema.
   */
  structuredParams?: readonly string[];
  /**
   * The effect. Entered ONLY with a bag that survived binding: an object, no
   * undeclared keys, every value text or a finite number (except a
   * {@link GuardedActionSpec.structuredParams} field, which arrives as sent).
   */
  run: (args: Record<string, unknown>) => TResult;
}

/**
 * Build the handler. Shared by both wrappers so there is one refusal point,
 * not two that can drift.
 *
 * Named `label` rather than `id` in the sentence because that is what the
 * operator typed at: `bindSchemaBag` paints `<label>: takes no argument named
 * "zzz"`, matching the sentence the slash route has always painted.
 */
function guardHandler<TResult>(spec: GuardedActionSpec<TResult>): (params?: unknown) => TResult {
  return (params?: unknown) => {
    // `params ?? {}` maps ONLY nullish to the empty bag. `5`, `"zz"` and `[]`
    // go through to `bindSchemaBag` and are refused there — collapsing them
    // to `{}` here would reintroduce the exact laundering this exists to stop.
    const bound = bindSchemaBag(
      spec.id,
      spec.paramSchema,
      params ?? {},
      spec.structuredParams ?? [],
    );
    if (bound.refusal) throw new Error(bound.refusal);
    return spec.run(bound.args);
  };
}

/**
 * A component action (`useUIComponent({ actions: [...] })`) that cannot read
 * an unvalidated bag.
 */
export function guardedAction<TResult>(spec: GuardedActionSpec<TResult>): GuardedComponentAction {
  const handler = guardHandler(spec);
  return {
    id: spec.id,
    label: spec.label,
    description: spec.description,
    paramSchema: spec.paramSchema,
    handler,
  };
}

/**
 * An element custom action (`customActions: { … }`) that cannot read an
 * unvalidated bag.
 *
 * The SDK's `CustomAction` has no `paramSchema` field, so the schema is not
 * published on the element descriptor — but it is still the thing that
 * decides which keys are refused, and it is still read by the enforcement
 * scan. Carrying it on the object anyway costs nothing and means one shape
 * answers both wrappers.
 */
export function guardedCustomAction<TResult>(
  spec: Omit<GuardedActionSpec<TResult>, "label">,
): GuardedCustomAction {
  const handler = guardHandler(spec as GuardedActionSpec<TResult>);
  return {
    id: spec.id,
    description: spec.description,
    paramSchema: spec.paramSchema,
    handler,
  };
}
