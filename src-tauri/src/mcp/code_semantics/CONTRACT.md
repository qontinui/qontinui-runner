# Code-Semantics Observer — Cross-Repo Contract (Phase 2a/2b)

Twin sub-space `Ξ_Type` (resolved code semantics), implemented as a **pure observer**
(no declared-vs-actual drift pair, no Φ predicate). The TS engine is the TypeScript
**Language Service API** driven by a long-lived Node helper, supervised by a Rust
sidecar module in the runner that hosts the HTTP query surface. coord-native MCP
tools (`phase11()`) proxy to that surface. Plan:
`plans/2026-05-30-twin-code-semantics-lsp-layer.md`.

This file is the single source of truth for the wire contracts the runner side and
the coord side build against independently.

---

## A. Node helper ⇄ Rust sidecar — stdio JSON protocol (runner-internal)

Newline-delimited JSON (one object per line) over the helper child's stdin/stdout.

**Request:** `{"id": <int>, "cmd": "<command>", ...args}`
**Response:** `{"id": <int>, "ok": true, ...result}` | `{"id": <int>, "ok": false, "error": "<msg>"}`

The helper resolves the `typescript` package from (in order): `$QONTINUI_TS_PATH`,
the runner frontend `node_modules/typescript`, then global. If TS cannot be resolved
the helper exits non-zero with a clear stderr message (the sidecar then degrades to
the code_graph fallback).

### Commands

- `init {"project": "<abs dir or tsconfig.json>"}` →
  `{"ready": true, "file_count": <int>, "project": "<resolved tsconfig path>"}`
  Builds the LanguageService + Program. Until this returns, the scope is **cold**.

- `status {}` → `{"indexed": <bool>, "file_count": <int>, "project": "<path|null>"}`

- `symbol_lookup {"name": "<str>", "kind?": "<function|class|interface|type|variable|method|enum>", "file?": "<abs>"}` →
  `{"exists": <bool>, "resolved": [ {"file","name","kind","signature","signature_hash","def_location":{"file","line","col"}} ]}`
  Resolved via the type checker; searches program declarations matching name (+ kind/file filters).

- `signature {"file": "<abs>", "name?": "<str>", "kind?": "<str>"}` **or**
  `{"file": "<abs>", "line": <int>, "col": <int>}` →
  `{"found": <bool>, "signature": "<str>", "signature_hash": "<sha1>", "generics": ["<str>"], "params": [{"name","type"}], "return_type": "<str>", "doc": "<str>"}`
  (`line`/`col` are 1-based.)

- `find_references {"file": "<abs>", "name?": "<str>"}` **or** `{"file","line","col"}` →
  `{"references": [ {"file","line","col","kind": "call|import|impl|reference|definition"} ]}`

- `typecheck {"file": "<abs>", "overlay_patch?": {"file": "<abs>", "new_text": "<str>"}}` →
  `{"ok": <bool>, "errors": [ {"file","line","col","code": <int>, "message"} ], "overlay": <bool>,
    "changed_signatures": [ {"name","before_hash","after_hash"} ],
    "removed_symbols": [ {"name","kind","referenced_by": [ {"file","line"} ]} ] }`
  With `overlay_patch`: apply `new_text` for `overlay_patch.file` **in-memory only**
  (never write disk), recompute diagnostics for `file`, then clear the overlay. This is
  D3 `predict-effect`. `changed_signatures`/`removed_symbols` compare the overlaid
  file's exported symbol set against the on-disk version (2b). `removed_symbols`
  lists exported symbols the overlay deletes that are still referenced elsewhere
  (the Contradiction / active-negation signal).

---

## B. Rust sidecar HTTP surface (runner, axum, port 9876) — `/code-semantics/*`

All POST bodies are JSON; all responses are the **uniform observation envelope** (§C).
`scope` is an optional `(repo,language)` selector; when omitted the sidecar resolves
the scope from the file's nearest `tsconfig.json` (v1 default scope = the runner's own
TS frontend). v1 supports the single default TS scope but the code is structured for
a scope registry (map scope → helper child).

- `GET  /code-semantics/health` →
  `{"status": "ok", "scopes": [ {"scope","language","indexed","file_count","provenance"} ]}`
- `POST /code-semantics/symbol-lookup`  body `{"scope?","file?","name","kind?"}`
- `POST /code-semantics/signature`      body `{"scope?","file","name?","kind?","line?","col?"}`
- `POST /code-semantics/find-references` body `{"scope?","file","name?","line?","col?","cross_repo?": false}`
- `POST /code-semantics/typecheck`      body `{"scope?","file","overlay_patch?": {"file","new_text"}}`

### Cold-index / coverage (D4) — load-bearing

- Scope **cold** (helper not yet `init`-returned):
  - `symbol-lookup`: fall back to the syntactic `code_graph.rs` observer →
    `provenance="code_graph_fallback"`, `coverage≈0.5`, `posterior` high for *declared*
    but `credibility` reflects syntactic-only. If code_graph also finds nothing while
    cold → `coverage=0.0`, `provenance="indexing"`, **result MUST NOT assert
    `exists:false`** (honest "I can't see", not "absent").
  - `signature` / `find-references` / `typecheck` (need resolution): if cold →
    `coverage=0.0`, `provenance="indexing"`, result flagged not-yet-available.
- Scope **warm**: resolved answer → `posterior=1.0`, `coverage=1.0`,
  `provenance="ts-language-service"`, `credibility=(high,high,high)`.
- Node/TS unavailable on the machine → permanent degrade to `code_graph_fallback`
  (symbol-lookup) / `coverage=0` `provenance="engine_unavailable"` (resolution queries).
  Never 500 on a missing engine.

---

## C. Uniform observation envelope (both sidecar HTTP responses and coord MCP output)

```json
{
  "observer": "code_semantics",
  "query": "symbol_lookup|signature|find_references|typecheck",
  "result": { ...query-specific payload (the §A result body, lifted) ... },
  "posterior": 1.0,
  "coverage": 1.0,
  "provenance": "ts-language-service|code_graph_fallback|indexing|engine_unavailable",
  "credibility": { "causal": "high", "authorial": "high", "boundary": "high" },
  "staleness_seconds": null,
  "kernel": false
}
```

- `kernel` is always `false` in v1 (only deterministic gate-worthy queries ship; fuzzy
  completion is Phase 3).
- `credibility` is the triple from the theory (Phase 1): definitional/structural code
  queries are `(high,high,high)` — the LSP crosses the compiler boundary and is
  authorially independent. The three `Ξ_AST`/`Ξ_Type` tiers form a clean 3-rung
  *boundary* ladder: the syntactic `code_graph_fallback` provenance (raw, no import
  resolution) is boundary **`low`** and `coverage<1`; the resolved-import
  `code_graph_resolved` provenance (deterministic file binding, not the compiler's
  resolver) is boundary **`medium`**; the LSP `ts-language-service` is boundary
  **`high`**. Reserving `medium` for the resolved tier keeps the low/medium/high
  triple unambiguously orderable across all three observers.

---

## D. coord MCP tools (`phase11()`) — proxy, credential-light

Tools call the sidecar over HTTP at `$QONTINUI_LSP_SIDECAR_URL` (default
`http://127.0.0.1:9876`). They wrap the sidecar's envelope into the coord
`ObservationEnvelope` and return it. coord hosts **no** language server.

| Tool | Inputs | Maps to |
|---|---|---|
| `coord_symbol_lookup`  | `{scope?, file?, name, kind?}` | POST /code-semantics/symbol-lookup |
| `coord_signature`      | `{scope?, file, name?, kind?, line?, col?}` | POST /code-semantics/signature |
| `coord_find_references`| `{scope?, file, name?, line?, col?, cross_repo?}` | POST /code-semantics/find-references |
| `coord_typecheck_file` | `{scope?, file, overlay_patch?}` | POST /code-semantics/typecheck |

- Sidecar **unreachable** (connection refused / timeout) → `ToolError` with a clear
  message ("code-semantics sidecar unreachable at <url>"). coord stays credential-light.
- Sidecar reachable but scope cold → pass the `coverage<1` / `provenance:"indexing"`
  envelope through (honest, not an error).
- `coord_typecheck_file` with `overlay_patch` = D3 `predict-effect` (2b): never writes a
  file; returns would-typecheck + new errors + changed signatures + removed-referenced
  symbols.

### Five-outcome verification map (2b, coord-side pure classifier)

`classify_edit_outcome(predicted, observed) -> {outcome, rationale}` where
`predicted` = overlay typecheck result, `observed` = post-write typecheck result:

- predicted clean **&** observed clean → **Confirmed**
- predicted clean **&** observed has new errors → **Surprise** (predict missed it)
- predicted errors **&** observed errors (match) → **Confirmed** (knowingly partial)
- overlay removes a symbol still referenced elsewhere → **Contradiction** (active negation)
- index settling / coverage<1 on either side → **Partial**

Unit-tested with a small truth table.
