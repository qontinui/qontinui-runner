# Spec storage root (Section 2 of UI Bridge redesign)

This directory is the storage root for the new IR-based spec system. It is
managed by the Spec API exposed at `/spec/...` on the runner's HTTP server
(port 9876). Every IR document, projection, and human-authored note for a
page lives under `pages/<page-id>/`.

## Layout

```
specs/
  pages/<id>/
    state-machine.derived.json   IR document (camelCase, authoring-time)
    spec.uibridge.json           Bundled-page legacy projection (generated)
    notes.md                     Human notes (carried into projection)
  architecture/                  Section 8/11 — initially empty
  design-system/                 Section 8/11 — initially empty
  contracts/                     Section 8/11 — initially empty
  scripts/
    generate-bundled.mjs         Node helper to regenerate projections
                                 from the IR via `@qontinui/shared-types`.
```

## Conventions

- IR documents are the source of truth. Projections are derived artifacts and
  never hand-edited.
- The bundled projection (`spec.uibridge.json`) preserves the legacy spec
  shape so existing tooling (`/update-spec`, `error_monitor::curator`,
  `spec_drift`, etc.) keeps working through the migration. Section 3 is the
  point at which legacy consumers repoint at the IR.
- The Spec API server validates path traversal — `?path=` queries must
  resolve inside this root or the request is rejected.

## Override

Set `QONTINUI_SPECS_ROOT=<absolute path>` to point the runner at a different
storage root (useful for tests). When unset, the API resolves to this
directory relative to the runner's repo.
