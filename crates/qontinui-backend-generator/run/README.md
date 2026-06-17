# Runtime-verification harness (Fork B) — REAL in Phase 2

This directory is the **run target** the plan's Phase 1–2 steps call for: bring a
generated backend up in a throwaway container (disposable postgres + uvicorn), create the
schema, then have the verify-phase hit the live endpoint and assemble a
`CoverageEvidence` from the running server's behavior.

## What ships now (Phase 2 — runnable + observed)

`generate_runnable(spec, profile, BreakMode::None)` emits a backend that **really
persists and behaves**:

- a synchronous SQLAlchemy 2.0 stack over **psycopg** whose `Base.metadata.create_all`
  runs on startup (FastAPI lifespan), so the `devices` table truly exists;
- a `pairConfirm` handler that **inserts a `Device` row** and returns a token-shaped body
  with `201` — route method/path still come **only** from the shared `endpoint_for`
  (`POST /api/v1/devices/pair-confirm`), never re-derived;
- a `/healthz` probe used as the compose healthcheck.

The integration test `tests/runtime_integration.rs` (gated `#[ignore]`, needs Docker)
materializes this tree, boots the stack via the compose file below, POSTs a valid pairing,
reads the **real** HTTP status + body, **introspects the live `devices` table**, and
builds a `CoverageEvidence` from those observations — then asserts via the real
`evaluate_completeness` that the `entities.Device` refs + `operations.pairConfirm` are
covered and `operations.pairConfirm.effect` is filled. A deliberately-broken variant
(`BreakMode::DropColumn`) drops the live `callback` column and the verdict surfaces
`entities.Device.fields.callback` as a gap — proving the evidence reflects the running
server, not the generator's claims.

## Bringing it up

```sh
# Run the gated integration test (it materializes + boots + probes + tears down):
cargo test -p qontinui-backend-generator -- --ignored runtime_pair_confirm_observed_from_live_server

# Or by hand (port 8000 is taken on the dev host, so publish on 8010):
#   1. materialize a generated backend into ./generated (BreakMode::None)
#   2. boot it
APP_HOST_PORT=8010 docker compose -f run/docker-compose.yml up --build -d
#   3. hit the live endpoint
curl -i -X POST http://localhost:8010/api/v1/devices/pair-confirm \
  -H 'Content-Type: application/json' \
  -d '{"device_name":"dev","device_id":"d1","state":"s","callback":"https://x/y"}'
#   4. tear down
docker compose -f run/docker-compose.yml down -v
```

## Still deferred (Phase 3)

- **LLM-authored handler bodies** (Fork A, `run_prompt_sync`). Phase 2 uses a correct
  deterministic persisting handler — preferable to an unverified LLM call for the
  runtime-verification milestone.
- **Alembic migrations.** Phase 2 creates the schema with `create_all` on startup (the
  simplest thing that really creates the table); alembic `upgrade head` is a Phase-3
  refinement.
- The full multi-entity/all-operations walk, generated pytest tiers + coverage gate, and
  the enforcement (`code-reviewer`/`security-scan`) loop.
