# Generated style guide: what the hand-written guide carried and where it went

`src/design-system/qontinui-runner.styleguide.uibridge.json` is `design-kit
compile` output of `design/language.dtcg.json` (kit `v0.1.0`). It replaced a
hand-written guide. This file records, per key and rule of that hand-written
guide, whether it is carried and why. Authored values cite notification
`3588ba69-6fc1-4b6a-9e9b-9d23729abc6f`.

## Tokens

| Hand-written key | Decision |
|---|---|
| `colors.*` (26) | Carried. Every key resolves from `@qontinui/design-tokens` at the same value. |
| `typography.fontSizes` xs 12, sm 14 | Carried under the package's names: `label` 12, `body` 14 (resolved). |
| `typography.fontSizes` base 16, lg 18, xl 20, 2xl 24, 3xl 30 | Carried as authored `type.scale.{base,lg,xl,2xl,3xl}`: the package scale lacks them and `typography.ts` renders them (`text-2xl` headings, `text-3xl` metrics). |
| `typography.fontFamilies` (Geist) | Not carried. A family is an identity value; it stays `undeclared` and renders as the platform keywords. |
| `typography.fontWeights` | Carried as authored; `normal` is keyed `regular`, the package's name. |
| `typography.lineHeights` | Carried as authored; `normal` is keyed `body`. |
| `typography.letterSpacings` | Carried as authored. The hand-written guide wrote them in `em`; the generated guide renders them as px at a 16px font size, which is exact only at that size (an `em` letter-spacing scales with the element's own font size). No generated rule reads them. |
| `spacing` xs..xxxl | Carried as derived 4px multiples under the same keys. |
| `borderRadius` | Carried as authored, same values in px. |
| `shadows` none..xl | Carried as authored `space.depth`. |
| `shadows` glow-primary / glow-secondary / glow-success | Not carried. Their colour is the package's `color.glow.*` (identity); an authored shadow would restate it as a literal, and kit `v0.1.0` cannot alias a colour inside a shadow. The glow colours are in the palette. |
| `breakpoints` | Carried as authored. |

## Rules

The kit generates rules per role from the source: `palette-color`,
`palette-background-color`, `font-size-on-scale`, `heading-size-on-scale`,
`border-radius-on-scale` and the kit state-matrix rules. It has no way to state
a per-class rule, a height or a cursor.

| Hand-written rule | Decision |
|---|---|
| `text-color-compliance` | Replaced by `palette-color`. |
| `card-background` | Replaced by `palette-background-color` (any palette role, not `surface-raised` only). |
| `h1/h2/h3-typography` (size), `body-text-size`, `badge-typography`, `label-typography` (size), `font-size-range` | Replaced by `font-size-on-scale` and `heading-size-on-scale`, which are looser: any size on the scale, not one per element (`h1-typography` was exactly 24px), and the scale admits 11px (`caption`) and 32px (`display`), outside the old 12-30px `font-size-range`. The weight halves are not carried: the kit emits no weight rule. |
| `btn-border-radius`, `input-border-radius`, `card-border-radius` | Replaced by `border-radius-on-scale` (any declared radius, not one per class). |
| `btn-height`, `input-height` | Not carried: the source has no control-height group and the kit emits no height rule. |
| `btn-cursor-pointer`, `interactive-cursor` | Not carried: the kit emits no cursor rule. |
| `btn-font-weight` | Not carried: the kit emits no weight rule. |

Each "not carried" row is a kit follow-up (a control-height group, cursor and
weight rules), not a decision that the runner no longer wants the constraint.
