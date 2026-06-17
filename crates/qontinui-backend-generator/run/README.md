# Runtime-verification harness (Fork B) — DEFERRED in Phase 1

This directory is the **run target** the plan's Phase 1 steps 3–5 call for: bring a
generated backend up in a throwaway container (disposable postgres + uvicorn), run
migrations, then have the verify-phase hit the live endpoint and assemble a
`CoverageEvidence` from the running server's behavior.

## What ships in Phase 1 (deterministic, verified)

The crate emits the backend **source** deterministically and the golden test
(`tests/golden_phase1.rs`, 3 passing tests) proves the spec→routes/schema seam:

- the emitted `pairConfirm` route's method+path is byte-equal to the shared
  `endpoint_for(pairConfirm, profile)` == `POST /api/v1/devices/pair-confirm`
  (the #1↔#2 agreement proof), and
- the emitted `Device` model has one column per spec field.

## What is honestly deferred (NOT proven in Phase 1)

The plan's "coverage observed from a running server" premise requires:

1. **A real handler body.** Phase 1 emits a deterministic typed *stub*
   (`return {"deviceToken": ""}`). The real effect ("persists the pairing and returns a
   device token") is authored by the `claude` CLI codegen subprocess
   (`run_prompt_sync`, Fork A) — that is Phase 2/3, not the deterministic core.
2. **Alembic migrations.** Phase 1 emits no `alembic/versions/*`, so there is no
   `alembic upgrade` to create the `devices` table at boot.

Because both are absent, bringing this compose stack up would NOT exercise a real
pairing endpoint — wiring a hand-written migration + handler just to make it boot would
be faking the runtime verification this phase explicitly must not fake. So the harness
is checked in **ready but unexercised**, and the integration test
(`tests/runtime_integration.rs`) is `#[ignore]`d with a body that documents exactly
what it will assert once the Phase-2 bodies + migrations land.

Docker *is* available on this machine (daemon reachable), but availability is not the
blocker — the missing LLM-filled handler and migrations are.

## Bringing it up (after Phase 2 lands)

```sh
# 1. materialize a generated backend into ./generated (see runtime_integration.rs)
# 2. boot it
docker compose -f run/docker-compose.yml up --build
# 3. hit the live endpoint
curl -i -X POST http://localhost:8000/api/v1/devices/pair-confirm
```
