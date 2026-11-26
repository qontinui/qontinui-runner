/**
 * The UI Bridge SDK's named-type family rule, for the runner frontend.
 *
 * A copy of `NAMED_TYPE_FAMILIES`, `namedTypeFamily` and `answersToNamedType`
 * from `@qontinui/ui-bridge` `packages/ui-bridge/src/core/element-kind.ts`
 * (0.46.0, ui-bridge `9f373a12`). It is restated rather than imported because
 * the SDK's package entry does not export it: `core/index.ts` re-exports none
 * of `element-kind`, so it is reachable only inside the SDK. The runner's Rust
 * copy is `crates/spec-check/src/named_type.rs`; keep all three in step.
 *
 * The SDK keeps `input` / `textbox` and `select` / `combobox` / `listbox` apart
 * as element KINDS (their verbs differ), but a person names them
 * interchangeably, and before 0.46 the registry reported every
 * `role="textbox"` as `input` and every `role="combobox"` / `"listbox"` as
 * `select`. So a reader that MATCHES a type a caller named (a query, a filter,
 * a typed-in keyword) matches the name's whole family; a reader that
 * classifies an element by its own type reads the precise kind.
 */

/** Kinds a caller names interchangeably. Lower-case. */
const NAMED_TYPE_FAMILIES: readonly (readonly string[])[] = [
  ["input", "textbox"],
  ["select", "combobox", "listbox"],
];

/** The kinds a caller-named type answers to, lower-cased: its family, else itself. */
export function namedTypeFamily(named: string): readonly string[] {
  const lower = named.toLowerCase();
  return NAMED_TYPE_FAMILIES.find((family) => family.includes(lower)) ?? [lower];
}

/** Whether an element of kind `type` answers to the caller-named type `named`. */
export function answersToNamedType(type: string | undefined, named: string): boolean {
  return type !== undefined && namedTypeFamily(named).includes(type.toLowerCase());
}
