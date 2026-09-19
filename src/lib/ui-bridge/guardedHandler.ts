/**
 * guardedHandler — the ONE way a UI Bridge action handler reads caller-supplied
 * params.
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
 *     defaults and the action runs BARE. `create-plain(5)` answered
 *     `success: true` and spawned a PTY; `create-best-account(5)` spawned one
 *     AND wrote a `claude --session-id … --config-dir …` line into it.
 *   - an UNDECLARED key is silently dropped, so
 *     `create-with-command({command: "echo pwn", zzz: "x"})` reported success
 *     over a key the action does not have.
 *   - a NON-SCALAR value is truthy, so `{name: {}}` sails past `if (!name)`
 *     and becomes the computed key `"[object Object]"` in a persisted map, or
 *     `[object Object]` typed into a live PTY.
 *
 * A previous round fixed four such surfaces by hand and left three siblings
 * open IN THE SAME `actions: [...]` array. Fixing the instances you can see is
 * what failed, so the fix is a shape that CANNOT be written unguarded, plus a
 * mechanical enumeration that goes red when a new one is
 * (`actionSurfaces.ts` + `actionSurfaces.enforcement.test.ts`).
 *
 * ## Why the guard wraps the HANDLER, not the action
 *
 * An action registration stays a plain object literal carrying its `id`,
 * `paramSchema` and `effect`:
 *
 *     {
 *       id: "save-profile",
 *       paramSchema: PROFILE_NAME_SCHEMA,
 *       effect: "destructive",
 *       handler: guardedHandler("save-profile", PROFILE_NAME_SCHEMA, (args) => …),
 *     }
 *
 * That is deliberate. The component-action `effect` framework
 * (plan `2026-09-04-effect-calculus-joins-the-component-action-registry`)
 * reads registrations out of the AST — `action-effect-coverage.test.ts`
 * requires every action to carry an `effect`, and
 * `scripts/capture-component-effect-fixture.cjs` projects real registrations
 * into the SDK → runner → Rust boundary fixture. A wrapper around the whole
 * def (`guardedAction({…})`, as this guard was first written) hides the
 * literal from both. Guarding at the handler keeps both properties: the
 * registration is still a literal the effect walk reads, and the handler is
 * still incapable of seeing an unbound bag.
 *
 * `actionSurfaces.ts` checks the two halves agree: a guarded handler whose
 * schema argument is not the same expression as its literal's `paramSchema` is
 * a violation, because a guard validating against a schema nobody publishes is
 * the same drift one level down.
 *
 * ## What the guard does
 *
 * {@link bindSchemaBag} — the same binder `callRegistry` → `bindDirect` gives
 * every registry command — has already
 *
 *   1. refused a non-object bag (`5`, `"zz"`, `[]`),
 *   2. refused any value that is not text or a finite number (unless the field
 *      is exempted, below),
 *   3. refused any key the `paramSchema` does not declare — including
 *      `__proto__`, which a plain assignment would lose to the prototype
 *      accessor,
 *
 * by the time `run` is entered. A refusal throws BEFORE `run`, which is the
 * only point at which an argument the handler cannot read is still cheap to
 * reject: no PTY, no `terminal_write`, no `setting_set`, no React re-render.
 * The thrown error carries `.code = ACTION_PARAMS_INVALID`, the same
 * `message` + `.code` shape the terminal payload validators use, which is what
 * the SDK hoists onto the response.
 *
 * Coercion: a clean numeric token becomes a number, so `{count: "2"}` arrives
 * as `2` — the same reading Tier 1 applies to typed text. `run` therefore
 * reads text fields through `parse.ts`'s `textArg`.
 *
 * ## Exemptions from per-VALUE checking (never from the bag or key checks)
 *
 *   - `structuredParams: [field]` — a declared field whose contract genuinely
 *     admits a list or an object (`sendKeys`'s `keys`: a raw string, an array
 *     of names, or an array of `{key, modifiers}` descriptors). Taking it is a
 *     promise the field has a validator of its own (`toPtySequence`). A field
 *     named here that the schema does not declare is ignored and then refused
 *     as undeclared — an action cannot widen itself past its own schema.
 *   - `valuesCheckedBy: "handler"` — EVERY declared field passes through
 *     un-coerced and un-dropped (an explicit `null` included), for a handler whose per-field validation is already a typed
 *     validator with its own machine-readable code (`requireTextPayload`,
 *     `requireMaxLines`). Coercing `{text: "5"}` to `5` ahead of such a
 *     validator would turn a valid write into `WRITE_TEXT_INVALID`. The bag
 *     and key-set refusals still apply, and they are the part those handlers
 *     never had.
 *
 * ## What this does NOT do
 *
 * It does not check that a REQUIRED field is present, that a number is in
 * range, or that a name exists in a map. Those are the action's own semantics
 * and stay in `run`, where the operator-facing sentence for them lives.
 *
 * An action whose handler declares no parameter cannot be INFLUENCED by a bag
 * and is not required to route through here — but it still answers `✓` for a
 * key it does not have. That residual is measured and listed in
 * `actionSurfaces.ts`; it is not closed by this module.
 */

import { bindSchemaBag } from "@/components/terminal/commands/bind";

/** The `paramSchema` map form: field name → human-readable type sentence. */
export type ParamSchema = Readonly<Record<string, string>>;

/** Machine-readable code on every refusal this guard throws. */
export const ACTION_PARAMS_INVALID = "ACTION_PARAMS_INVALID";

export interface GuardOptions {
  /** Declared fields passed through un-coerced; see the module header. */
  structuredParams?: readonly string[];
  /**
   * `"handler"` passes EVERY declared field through un-coerced, because the
   * handler validates each value itself with a typed validator. The default,
   * `"guard"`, coerces and refuses non-scalar values here.
   */
  valuesCheckedBy?: "guard" | "handler";
}

/**
 * Build a handler that binds the caller's bag against `paramSchema` before
 * `run` is entered.
 *
 * `id` names the surface in the refusal sentence — `<id>: takes no argument
 * named "zzz"` — matching the sentence the slash route has always painted.
 */
export function guardedHandler<TResult>(
  id: string,
  paramSchema: ParamSchema,
  run: (args: Record<string, unknown>) => TResult,
  options: GuardOptions = {},
): (params?: unknown) => TResult {
  const passthrough =
    options.valuesCheckedBy === "handler"
      ? Object.keys(paramSchema)
      : (options.structuredParams ?? []);
  return (params?: unknown) => {
    // `params ?? {}` maps ONLY nullish to the empty bag. `5`, `"zz"` and `[]`
    // go through to `bindSchemaBag` and are refused there — collapsing them
    // to `{}` here would reintroduce the exact laundering this exists to stop.
    const bound = bindSchemaBag(id, paramSchema, params ?? {}, passthrough);
    if (bound.refusal) {
      const err = new Error(`${ACTION_PARAMS_INVALID}: ${bound.refusal}`) as Error & {
        code?: string;
      };
      err.code = ACTION_PARAMS_INVALID;
      throw err;
    }
    if (options.valuesCheckedBy === "handler") {
      // Hand the caller's values over EXACTLY as sent — including an explicit
      // `null`, which the binder's passthrough treats as absent. A typed
      // validator that refuses `null` with its own code must still see it.
      // Binding has proven the bag is an object whose keys are all declared.
      return run(Object.fromEntries(Object.entries((params ?? {}) as object)));
    }
    return run(bound.args);
  };
}
