# qontinui-spec-ci

Spec-CI executor — walks an `IrPageSpec`'s state graph against a running UI-Bridge-connected app via the runner's HTTP surface and emits a CI-friendly pass/fail report. The behavioural analogue of `spec_audit.py` (which only checks static `assertions[]`).

## What it does

Given a runner URL, app id, and page id:

1. `GET /apps/<app>/spec/get?id=<page>` to fetch the IR.
2. `adaptIRDocumentToWorkflowConfig` (from `@qontinui/shared-types/ui-bridge-ir`) to convert the IR shape into the in-memory `AdaptedWorkflowConfig` that `ui-bridge-auto`'s runtime engine consumes.
3. Snapshot the runner via `GET /ui-bridge/sdk/snapshot`, rebuild a minimal `jsdom` document so `ui-bridge-auto`'s DOM-based matcher (`matchesQuery`) works without a real browser.
4. Construct a `StateMachine` from the adapted config, run `StateDetector.evaluate()` to pin the initial active set.
5. For each `IrTransition` in the doc (skipping `effect: "destructive"` unless `--include-destructive`), call `executeTransition` from `ui-bridge-auto/state/transition-executor`. Actions dispatch through `POST /ui-bridge/sdk/element/<id>/action`; waits use the existing `idle | time | element | vanish | change | stable` machinery.
6. Re-snapshot, re-detect, record whether the `activateStates` actually became active.
7. Emit a deterministic JSON report (results sorted by transition id).

## Why this design

The transition-executor, pathfinder, state-detector, and matcher in `ui-bridge-auto` already exist and are battle-tested (WU-5 complete). The matcher is DOM-bound, but `jsdom` reconstructs enough DOM from the snapshot JSON to keep the matcher working without changing it. Net new code is one file of HTTP adapters + a thin CLI — everything else is plumbing.

## CLI

```
spec-ci-run \
  --runner http://localhost:9876 \
  --app qontinui-web \
  --page operations \
  --output /tmp/spec-ci-operations.json \
  [--include-destructive] \
  [--page-url http://localhost:3001/operations]
```

If `--page-url` is supplied, the executor `POSTs /ui-bridge/sdk/page/navigate` to that URL before evaluating. Useful when the runner's connected SDK isn't already on the target page.

## Output schema

```json
{
  "runnerUrl": "http://localhost:9876",
  "appId": "qontinui-web",
  "pageId": "operations",
  "evaluatedAt": "2026-05-22T...",
  "initialStates": ["..."],
  "transitions": [
    {
      "id": "t1",
      "name": "...",
      "fromStates": ["..."],
      "activateStates": ["..."],
      "effect": "read" | "write" | "destructive" | null,
      "executed": true,
      "passed": true,
      "durationMs": 412,
      "missingActivateStates": [],
      "extraActiveStates": [],
      "error": null
    }
  ],
  "summary": {
    "totalTransitions": 0,
    "executedTransitions": 0,
    "passedTransitions": 0,
    "skippedDestructive": 0,
    "errorTransitions": 0,
    "passRate": 1.0
  }
}
```

## Not in scope for v1

- Pathfinding to a specific target state (use `navigateToState` directly if you need it; the CLI exercises every transition individually).
- Parallel execution — transitions run sequentially since the underlying SDK is single-tenant.
- Counterfactual exploration / regression suite generation. Those are separate ui-bridge-auto subpaths.
