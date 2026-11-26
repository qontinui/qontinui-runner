//! The design-bearing `react-ui-bridge` template: the file set a New Project
//! scaffold writes.
//!
//! Every constant is a real file under `new_project_templates/react-ui-bridge/`,
//! embedded at compile time (`include_str!`), so scaffolding never touches the
//! network. The files carry a `.tmpl` suffix so no tool treats a half-templated
//! `package.json` or workflow as a live one; `{{name}}` / `{{app_id}}` are
//! substituted by `new_project_templates::substitute`.
//!
//! Two groups of files live there:
//!
//! - **Authored** — `package.json`, `vite.config.ts`, `src/App.tsx`, `CLAUDE.md`,
//!   `design/language.dtcg.json`, and the rest. Edit these by hand.
//! - **Compile outputs** — `src/design/*`, `specimen.html`, and the component
//!   index between the two markers of `CLAUDE.md`. They are the verbatim output
//!   of `design-kit compile`, `design-kit specimen` and `design-kit index` (the
//!   pinned `@qontinui/design-kit` tag) run on `design/language.dtcg.json`; never
//!   edit them by hand. To refresh them after changing the source or moving the
//!   kit tag: scaffold with this template into a scratch directory, run `npm
//!   install && npm run design:compile`, then copy the files back. The
//!   scaffolded project's `npm run build` fails on any drift, so a stale copy
//!   here cannot ship green.
//!
//! The source's identity groups (colour, typeface, imagery style) are
//! `undeclared`: they are the product owner's to declare, and the template's
//! tests refuse an agent-authored value there. Its structural groups (type scale,
//! spacing, radius, layout, motion, budgets, thresholds) are authored defaults,
//! each recorded as `authored(<notification_ref>)`; the reference is the coord
//! finding that covers the template's authored defaults and their licence, and
//! a product's own session replaces them with its own authored values and
//! finding.

/// `@qontinui/ui-bridge` is pinned exactly to the version the runner itself
/// ships with (the floor of the repo-root `package.json` range; it must also be
/// the exact `@qontinui/ui-bridge` peer of the design-kit tag below), so a
/// scaffold built without a lockfile
/// cannot drift onto a newer contract; the template's tests assert it is not
/// below the runner's pin. `@qontinui/design-kit` is installed by git tag (no
/// registry credential; `prepare` builds `dist/`).
pub(super) const PACKAGE_JSON: &str = include_str!("react-ui-bridge/package.json.tmpl");
pub(super) const VITE_CONFIG: &str = include_str!("react-ui-bridge/vite.config.ts.tmpl");
pub(super) const TSCONFIG: &str = include_str!("react-ui-bridge/tsconfig.json.tmpl");
pub(super) const INDEX_HTML: &str = include_str!("react-ui-bridge/index.html.tmpl");
pub(super) const MAIN_TSX: &str = include_str!("react-ui-bridge/src/main.tsx.tmpl");
pub(super) const APP_TSX: &str = include_str!("react-ui-bridge/src/App.tsx.tmpl");
pub(super) const INDEX_CSS: &str = include_str!("react-ui-bridge/src/index.css.tmpl");
pub(super) const README: &str = include_str!("react-ui-bridge/README.md.tmpl");
pub(super) const CLAUDE_MD: &str = include_str!("react-ui-bridge/CLAUDE.md.tmpl");

/// The visual system's source. Identity groups `undeclared`; structural groups
/// `authored(...)`.
pub(super) const LANGUAGE_SOURCE: &str =
    include_str!("react-ui-bridge/design/language.dtcg.json.tmpl");

/// `design-kit compile` outputs, committed as source.
pub(super) const TOKENS_CSS: &str = include_str!("react-ui-bridge/src/design/tokens.css.tmpl");
pub(super) const TOKENS_TS: &str = include_str!("react-ui-bridge/src/design/tokens.ts.tmpl");
pub(super) const THEME_CSS: &str = include_str!("react-ui-bridge/src/design/theme.css.tmpl");
pub(super) const COMPILE_REPORT: &str =
    include_str!("react-ui-bridge/src/design/compile-report.json.tmpl");
pub(super) const DESIGN_BUDGETS: &str =
    include_str!("react-ui-bridge/src/design/design-budgets.json.tmpl");
pub(super) const THRESHOLDS: &str =
    include_str!("react-ui-bridge/src/design/visual-system.thresholds.json.tmpl");
pub(super) const STYLE_GUIDE: &str =
    include_str!("react-ui-bridge/src/design/app.styleguide.uibridge.json.tmpl");

/// `design-kit specimen` output: every kit component in every state, plus the
/// design-system board. The second Vite page (no router dependency).
pub(super) const SPECIMEN_HTML: &str = include_str!("react-ui-bridge/specimen.html.tmpl");

/// A real CI check for the scaffolded repo.
///
/// A repo with no workflow files at all is a repo where no check can ever be
/// red: coord's merge predicate has an arm that admits a PR carrying zero
/// check rows when the default branch is positively established to hold no
/// workflows, so a freshly scaffolded owner repo would auto-merge every PR
/// against nothing. Shipping this file closes that composition — once `main`
/// carries a workflow, that arm declines and zero-check PRs block.
///
/// Four measured constraints, do not "improve" past them:
/// - `npm install`, never `npm ci` — the template ships no `package-lock.json`
///   and `npm ci` fails `EUSAGE` on a bare scaffold. For the same reason
///   `setup-node` carries no `cache: npm`, which keys on a lockfile.
/// - `npm run build` is the whole check: the template's `build` script runs
///   `design-kit compile --check`, the specimen and component-index drift
///   checks, `design-kit lint-classes src`, `tsc --noEmit` and `vite build`,
///   so it covers design drift, literals, typecheck and build.
/// - No tree-cleanliness step: `vite build` writes
///   `src/state-machine.derived.json`, so one would fail by construction. (The
///   committed `src/design/*` are source, not build outputs; drift in them is
///   `design-kit compile --check`'s job, inside `npm run build`.)
/// - No warnings-as-errors: the build legitimately warns (Node built-in
///   modules externalized for the browser, a direct-`eval` advisory inside
///   ui-bridge's dist, a chunk over vite's 500 kB hint) and still exits 0.
///
/// Deliberately contains no `${{ ... }}` GitHub Actions expressions, keeping
/// this file's `{{name}}`/`{{app_id}}` templating and Actions' own out of the
/// same document.
///
/// `node-version: '24'` is the version the green run was actually measured on,
/// and it is not arbitrary: `vite@8` and `@vitejs/plugin-react@6` both declare
/// `engines.node = "^20.19.0 || >=22.12.0"`, so a bare `'20'` would put a
/// scaffolded repo's CI on an end-of-life major sitting exactly on that floor —
/// green today, red on the next drift. Raise this with the template's
/// dependency floors; never lower it below `22.12`.
pub(super) const CI_WORKFLOW: &str = include_str!("react-ui-bridge/.github/workflows/ci.yml.tmpl");
