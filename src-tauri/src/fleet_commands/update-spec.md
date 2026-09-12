# Update UI Bridge Page Spec

Author or update an IrPageSpec for a page, validate it against the live UI via spec-check, and iterate until the match rate target is hit.

**Input**: `$ARGUMENTS` — one of:

| Form | Example | What happens |
|------|---------|-------------|
| Page URL | `https://qontinui.io/operations` | Single-page mode |
| Page slug or name | `operations`, `settings` | Single-page mode (infers project) |
| File path | `qontinui-runner/src/pages/specs/SpecsPage.tsx` | Single-page mode |
| Project name | `web`, `qontinui-web`, `runner`, `qontinui-runner` | **Project mode** — discover all pages, diff against existing specs, author missing ones |

## Project mode

When `$ARGUMENTS` matches a project name (`web`, `qontinui-web`, `runner`, `qontinui-runner`):

### 1. Discover all routable pages

**qontinui-web** — glob `<workspace-root>/qontinui-web/frontend/src/app/(app)/*/page.tsx`. Each directory name is a route slug. Base URL = `https://qontinui.io/<slug>`. If `<workspace-root>/qontinui-web` is not checked out, report that project mode needs the web repo and stop.

**qontinui-runner** — read `VALID_TAB_IDS` from `<workspace-root>/qontinui-runner/src/components/app/tab-types.ts`. Each ID is a page slug. If `<workspace-root>/qontinui-runner` is not checked out, report that project mode needs the runner repo and stop.

### 2. Diff against existing specs

```bash
# List specs the runner already knows about for this app
curl -s http://localhost:9876/apps/<APP_ID>/spec/list | jq '.specs[].specId'
```

Also check on disk: `ls <repo_root>/specs/pages/` for directories containing `state-machine.derived.json`.

### 3. Present the gap

Show the user a table: all discovered routes, which have specs, which don't, and which have Playwright tests (check `<repo_root>/tests/e2e/**/*.spec.ts`). Ask the user which pages to author — "all missing", a specific subset, or "all including re-authoring existing ones".

### 4. Fan out

Launch one subagent per page using the single-page workflow below (Phases 0–4). Each subagent gets its own worktree branched from `origin/main`.

**Allocate the worktree THROUGH coord — never a bare `git worktree add`**
*(plan `2026-08-18-undeclared-worktree-exposure-and-classification`, Phase 3)*.
A hand-rolled worktree has no `coord.agent_worktrees` row, which means it cannot
be attributed to a session, cannot hold a retention pin, and cannot be counted or
drained by policy — it just accumulates. (It is no longer *deleted* underneath
you: Phase 2 of that plan withdrew removal authority from the `undeclared`
trigger. The cost of skipping this is now a permanent leak rather than data loss,
which is a trade made deliberately — not a reason to skip it.)

```
POST $COORD_HTTP_URL/agents/allocate
{
  "device_id":  "<this machine's device_id>",
  "repos":      [{"repo": "<repo>"}],
  "intent":     "<what this worktree is for>",
  "work_unit_id": "<the plan's work_unit_id UUID, WHEN THERE IS ONE>"
}
```

`work_unit_id` is **optional** — a declaration without a plan is still a
declaration, and that is exactly why the field is nullable. Omit it rather than
inventing one.

Three response shapes you must handle, or the call is worse than useless:

1. **`worktrees[].worktree_path` is RELATIVE** (`agent-worktrees/<agent_id>/<repo>`)
   — re-root it under the workspace root and create the checkout with the branch
   coord reserved:
   `git -C <repo> worktree add -b <worktrees[].branch> <absolute-path> <worktrees[].parent_sha>`.
2. **`isolation.mode` may be `wait` or `shared_branch`.** On `wait`, coord is out
   of disk/build-slot budget — report `reason` / `blocking` and retry or
   serialize; do not force a worktree. On `shared_branch`, the canonical checkout
   can carry the branch.
3. **HTTP 409 `repo_not_registered`** — the repo is not in
   `coord.canonical_repos`, so coord cannot decide a parent SHA. Supply
   `parent_sha` explicitly, or fall back to a plain `git worktree add` and say in
   your report that the worktree is undeclared and why. An unregistered repo is
   the one legitimate reason to skip the declaration.

Only when coord answers 409 `repo_not_registered` (or is unreachable) does the
old raw form apply, and it produces an undeclared worktree you must say so about:

```
git worktree add -b spec/<SPEC_SLUG> <WORKTREE> origin/main
```

Run subagents in parallel batches (3–5 concurrent is safe — they share one SDK connection but the snapshot API serializes). Each subagent follows the full skill from Phase 0 onward with PAGE_URL and SPEC_SLUG pre-filled.

### 5. Collect results

After all subagents complete, report a summary table: page slug, matchRate, state count, assertion count, pass/fail. Commit all specs in one PR per project or one PR per page (user's choice).

---

## Single-page mode

When `$ARGUMENTS` is a page URL, slug, or file path, compute these derived constants:

- **RUNNER** = `http://localhost:9876` (primary runner)
- **APP_ID** = infer from the URL or file path:
  - URLs containing `:3001` or files under `qontinui-web/` → `qontinui-web`
  - URLs containing `:9876` or files under `qontinui-runner/` → `qontinui-runner`
  - Otherwise ask the user
- **PAGE_URL** = the full URL to the page (e.g., `https://qontinui.io/operations`)
- **SPEC_SLUG** = the last non-empty path segment of PAGE_URL, lowercased, `/` → `-`. Examples: `/qa-dashboard` → `qa-dashboard`, `/snapshot/test-generator` → `snapshot-test-generator`
- **SPEC_PATH** = `<repo_root>/specs/pages/<SPEC_SLUG>/state-machine.derived.json` (where `<repo_root>` is the app's repo root from the app registry)

---

## Phase 0: Precondition checks

Run all four checks before authoring. Abort if any fails.

### 0.1 — SDK connected

```bash
curl -s $RUNNER/ui-bridge/sdk/status | jq -e '.connected == true and (.allConnections | length > 0)' >/dev/null
```

**FAIL** → stop: "No SDK connected; the user must sign in to the app in a real browser and confirm the mDNS handshake."

### 0.2 — App registered and local

```bash
curl -s $RUNNER/apps/$APP_ID | jq -e '.uiBridgeUrl | startswith("http://localhost")'
```

**FAIL** → stop: "uiBridgeUrl still points at staging; PATCH it to the local URL first."

### 0.3 — Navigation works

```bash
curl -s -X POST $RUNNER/ui-bridge/sdk/page/navigate \
  -H 'Content-Type: application/json' \
  -d '{"url": "<PAGE_URL>"}'
```

Wait ~2s for hydration, then take a snapshot:

```bash
curl -s $RUNNER/ui-bridge/sdk/snapshot > /tmp/$SPEC_SLUG.snap.json
```

Assert: `jq '.data.elements | length' < /tmp/$SPEC_SLUG.snap.json` > 20 AND `jq '.data.page.url'` contains the PAGE_URL pathname.

(Read the snapshot via `<` — bash then opens it. Handing the POSIX path to the
NATIVE jq as an argument fails under an inherited `MSYS_NO_PATHCONV=1`: jq exits
2 "Could not open file", the assert reads empty, and the **FAIL** below fires on
a perfectly good page.)

**FAIL** → stop: "Navigate succeeded but snapshot is from the wrong page or has too few elements. Check if the route loaded."

### 0.4 — Not auth-walled

Assert: no element with `id: "email"` + `label: "Email Address"` + `tagName: "input"` in the snapshot. (This is the qontinui-web login page footprint.)

**FAIL** → stop: "App session expired; re-sign in at PAGE_URL in the browser."

---

## Phase 1: Survey the live page

This is the primary input for spec authoring. Read the actual rendered page, not guesses from code.

1. **Survey elements:** `jq '.data.elements | map({id, tagName, type, label, textContent: .state.textContent}) | .[0:80]' < /tmp/$SPEC_SLUG.snap.json`
2. **Identify stable IDs vs auto-generated:** IDs like `heading-1-operations`, `button-sign-in` are stable. IDs like `content-paragraph-...-0` are auto-generated (still usable but less stable across refactors).
3. **Cluster into states:** A state is a coherent UI slice — "page header", "filter bar", "results table", "empty state", "error state". Target 3–8 states, 3–8 assertions each.
4. **(Optional) Read source code** for additional context: component structure, conditional rendering, hooks, data flow. This enriches the spec but is NOT a prerequisite — the snapshot is the ground truth.

---

## Phase 2: Write the spec

### IR document shape

```json
{
  "version": "1.0",
  "id": "<SPEC_SLUG>",
  "name": "<PageNameInPascalCase>",
  "description": "<one paragraph: what the page does, who uses it>",
  "provenance": { "source": "ai-generated", "appId": "<APP_ID>" },
  "metadata": { "tags": ["<area>", "<feature>"] },
  "states": [
    {
      "id": "<SPEC_SLUG>-<state-slug>",
      "name": "<Human-readable state name>",
      "description": "<what this state represents>",
      "assertions": [
        {
          "id": "<state-id>-elem-0",
          "description": "<what this assertion proves>",
          "category": "element-presence",
          "severity": "critical",
          "assertionType": "exists",
          "target": {
            "type": "search",
            "criteria": { "role": "heading", "text": "Active" },
            "label": "Active page heading"
          },
          "source": "ai-generated",
          "reviewed": false,
          "enabled": true
        }
      ],
      "provenance": { "source": "ai-generated" }
    }
  ]
}
```

**Severity:** `critical` for must-haves (page broken without this), `warning` for nice-to-haves.

**Category:** `element-presence` | `element-state` | `text-content` | `accessibility` | `navigation` | `interaction` | `data-display` | `layout` | `design` | `state-consistency`

### Criterion resolution chain

The spec-check matcher resolves criterion fields against live elements via fallback chains. Understand these before writing any criterion:

| Criterion field | Resolution chain |
|-----------------|-----------------|
| `role` | `el.role` → implicit ARIA role from `tag_name` (h1–h6 → heading, button → button, a → link, select → combobox, input derived from `element_type`: email/text/password → textbox, checkbox/radio → same, search → searchbox, submit → button) → `element_type` keyword (paragraph, heading) when tag is generic |
| `accessible_name` | `el.accessibleName` → `el.label` → `el.state.textContent`. **Footgun:** `el.label` comes from React's `useUIElement({ label })` hint, which often differs from displayed text |
| `text` | `el.text` → `el.state.textContent` → `el.label` |
| `aria_label` | `el.ariaLabel` → `el.label` |
| `id` | exact match against `el.id`, `el.identifier.htmlId`, `el.identifier.testId`, `el.identifier.awasId`, `el.identifier.uiId`. **Most reliable** |
| `tag_name` | exact, no fallback |
| `text_contains` | substring of derived text (asymmetric) |

Phase 1 (binary): every `Some(_)` field must score 1.0 against the same element. Phase 2 (partial): scores all candidates, surfaces top 5 with per-field diffs — those diffs are your debugging signal.

### Criterion selection strategy (ID-first)

1. **Stable ID available?** Use `{ "id": "<id>" }`. Survives copy edits, immune to label≠textContent ambiguity. This is the most reliable.
2. **Headings + body text:** `{ "role": "heading", "text": "..." }` — combine role + text for selectivity.
3. **Badges / status pills** where the visible text differs from the React label: use `id`, not `accessible_name` — the matcher's fallback prefers `label` over `textContent`, so `accessible_name: "Healthy"` resolves against the label and fails with `text_mismatch`.
4. **Don't use `tag_name` alone** — too unselective.
5. **`selector` is last resort** — CSS selectors are fragile across refactors.

### Assert structure, not data

The backend serves live data that drifts across runs. Specs must hold across that drift.

**OK to assert:**
- "There's a heading 'Operations'."
- "There's a button labeled 'Refresh'."
- "The fleet overview section renders." (assert the section container exists)
- "The table has a header row." (assert `<thead>` / column-header roles)

**NOT OK to assert:**
- "There are 5 runners in the table." (varies per run)
- "Workflow 'TestFlow' appears in the list." (depends on the test account)
- "Tier badge shows 'Tier 2'." (varies per user)
- "Email is josh@qontinui.io." (PII, varies per session)

For pages whose markup differs meaningfully at zero rows vs ≥1, write **two states**:
- `<slug>-empty` — assert the empty-state copy ("No runners yet")
- `<slug>-populated` — assert the table header / row container shape, not specific row content

Spec-check picks whichever state matches the live page via `recommend_state`.

### State machines (for multi-configuration pages)

Include state machine transitions when the page has tabs, modals, or mode switches. Each state owns its transitions:

```json
{
  "id": "settings-general",
  "name": "General Settings Tab",
  "elements": [
    { "dataAttributes": { "page-id": "settings-general" } },
    { "role": "heading", "textContent": "General Settings" }
  ],
  "isInitial": true,
  "transitions": [
    {
      "id": "general-to-ai",
      "name": "Switch to AI Settings tab",
      "activateStates": ["settings-ai"],
      "deactivateStates": ["settings-general"],
      "staysVisible": false,
      "process": [
        {
          "action": "click",
          "target": { "role": "tab", "textContent": "AI" },
          "waitAfter": { "type": "idle", "timeout": 3000 }
        }
      ]
    }
  ]
}
```

Rules:
- Elements must not overlap between states — each state identifiable by unique elements
- Set `staysVisible: true` for transitions that open modals/overlays where the background remains
- One state should be `isInitial: true`
- All state IDs in `activateStates`/`deactivateStates` must exist in the spec

### Transitions (behavioral flows)

Transitions encode interactive flows — the actions a user performs and the state changes that result. The runtime (`executeTransition` + `StateDetector`) executes them via the in-browser `window.__qontinuiSpecCi__` executor: asserts `fromStates` active → runs `actions` via the SDK action surface → applies `waitAfter` → re-snapshots → asserts `activateStates` became active.

#### IrTransition shape

```json
{
  "id": "login-submit",
  "name": "Submit valid credentials",
  "fromStates": ["login-form-empty"],
  "activateStates": ["dashboard-loaded"],
  "exitStates": ["login-form-empty"],
  "effect": "read",
  "actions": [
    { "type": "type",  "target": { "id": "login-username" }, "params": { "text": "ci-bot@qontinui.io" } },
    { "type": "type",  "target": { "id": "login-password" }, "params": { "text": "..." } },
    { "type": "click", "target": { "id": "button-sign-in" },
      "waitAfter": { "type": "vanish", "query": { "id": "button-sign-in" } } }
  ]
}
```

| Field | Required | Description |
|-------|----------|-------------|
| `id` | yes | Unique kebab-case identifier |
| `name` | yes | Human-readable description of the flow |
| `fromStates` | yes | States that must be active before the transition fires |
| `activateStates` | yes | States that must become active after the transition completes |
| `exitStates` | no | States that must leave active set after completion |
| `effect` | yes | `read` (safe, default CI), `write` (mutates state), `destructive` (deletes data — skipped unless `--include-destructive`) |
| `actions` | yes | Ordered action sequence (see below) |

#### Action types

Each action in the `actions` array:

```json
{
  "type": "click" | "type" | "scroll" | "sendKeys" | "select" | "clear",
  "target": { "id": "..." } | { "role": "...", "text": "..." },
  "params": { "text": "..." },
  "waitAfter": { "type": "idle" | "vanish" | "element" | "time" | "change" | "stable", ... }
}
```

- **`target`** uses the same criterion format as assertion targets. Prefer `id` for reliability.
- **`waitAfter`** keys on the observable change:
  - `"vanish"` + `query` — wait for an element to disappear (e.g., login button after submit)
  - `"element"` + `query` — wait for an element to appear (e.g., dashboard heading)
  - `"idle"` + `timeout` — wait for the page to settle (default choice for tab switches)
  - `"time"` + `ms` — fixed delay (last resort)
  - `"change"` — wait for the target element's content to change
  - `"stable"` — wait for no DOM changes over a settling window

#### Authoring pattern

1. **State A** = reuse a structural state already authored in the spec. Don't re-derive states that exist.
2. **State B** = the post-action state. If it doesn't exist yet, add it with assertions on the changed UI (element appears/disappears, text changes, visibility toggles).
3. **Transition A→B** with `actions[]` and a `waitAfter` keyed to the observable change.
4. **Tag the `effect`**: `read` for navigations and views, `write` for creates/updates/toggles, `destructive` for deletes. CI skips `destructive` by default.

#### What to cover with transitions

Map each interactive flow that does more than "page renders X" into an A→B transition. The behavioral-critical subset:
- **Auth flows** — login, logout, session refresh
- **CRUD paths** — create/edit/delete workflows, form submissions
- **Key state mutations** — toggle settings, enable/disable features
- **Navigation with side effects** — publish flows, marketplace actions

Flows that are purely "page renders correctly" are already covered by structural state assertions — only interactive flows need transitions.

#### Verification

Author transitions against a known test account (e.g., ci-bot credentials via AWS SSM). Run each through Spec CI (`POST /spec-check`) before counting it green. Transitions with `effect: "destructive"` need a teardown strategy or idempotent setup.

### Advanced assertion types

Beyond `exists` (the workhorse), these types are available when needed:

| Type | Use for | Required fields |
|------|---------|-----------------|
| `exists` / `notExists` | Element presence or absence | — |
| `visible` / `hidden` | Visibility state | — |
| `hasText` / `containsText` | Exact or partial text match | `expected` |
| `count` | Number of matching elements | `expected` (number) |
| `attribute` | Element attribute value | `attributeName` + `expected` |
| `cssProperty` | Computed CSS property | `propertyName` + `expected` |
| `behavior` | User interaction produces expected result (requires AI evaluation) | — |
| `semantic` | Data correctness, algorithm output (requires AI evaluation) | — |

**Decision rule:** "Can this be verified from a single UI snapshot without performing any action?" Yes → deterministic type. No → `behavior` or `semantic`.

### Merge with existing spec

If an IR document already exists for this page:
1. Compare each existing state against the live page — keep states that still match, remove states for removed functionality
2. Preserve assertion IDs that haven't changed — this maintains continuity
3. Never reduce detail — if an existing assertion has a detailed description, keep or enhance it
4. Add new states and assertions for uncovered functionality

---

## Phase 3: Validate via spec-check

This is the core feedback loop. Do not ship a spec without passing validation.

### 3.1 — Write the spec to disk

Either write the file directly at SPEC_PATH, or POST to the Spec API:

```bash
curl -s -X POST $RUNNER/apps/$APP_ID/spec/author \
  -H 'Content-Type: application/json' \
  -d @spec.json
```

### 3.2 — Run spec-check

```bash
curl -s -X POST $RUNNER/spec-check \
  -H 'Content-Type: application/json' \
  -d '{"app_id":"<APP_ID>","spec_id":"<SPEC_SLUG>"}' \
  | jq '.summary'
```

### 3.3 — Check acceptance criteria

All three must hold before shipping:

1. `summary.overallMatchRate ≥ 0.8` and `summary.matchOutcome ∈ {partial_match, full_match}`
2. Every failing assertion's `outcome.miss.reason` is specific (`text_mismatch`, `role_mismatch`, `attribute_mismatch`, etc.) — **never `no_candidates`**. `no_candidates` means the criterion targets an element that doesn't exist on the rendered page; that's an authoring bug.
3. The spec has **≥ 3 states**. A 1-state "page structure" spec is a stub, not coverage.

### 3.4 — Debug failures

For each failing assertion, read `stateResults[].assertions[].outcome.miss`:

```bash
curl -s -X POST $RUNNER/spec-check \
  -H 'Content-Type: application/json' \
  -d '{"app_id":"<APP_ID>","spec_id":"<SPEC_SLUG>"}' \
  | jq '.stateResults[].assertions[] | select(.outcome.match == false) | {id: .assertion.id, reason: .outcome.miss.reason, diffs: .outcome.miss.field_diffs}'
```

The `field_diffs` array tells you which field disagreed and what the live value is.

**Fix strategies:**
- `field_diffs[].field == "text"` with a close `actual` → match the criterion to what's rendered
- `field_diffs[].field == "role"` with a sensible alternative → switch role (often heading ↔ paragraph for `<p>`)
- `no_candidates` → element doesn't exist; delete the assertion or target a real element nearby
- Persistent `text_mismatch` after two tries → fall back to `id`

### 3.5 — Iterate

Don't ship at first 0.8 — push toward 1.0. Repeat 3.2–3.4 until match rate stabilizes.

---

## Phase 4: Commit and report

### Writing the file

If you haven't already written via `/spec/author`, write the file to SPEC_PATH directly. The next `GET /apps/$APP_ID/spec/get` regenerates the projection.

**PowerShell BOM trap:** Never use `Set-Content -Encoding UTF8` to write IR files on Windows PowerShell 5.1 — it emits a UTF-8 BOM that `serde_json` rejects. Use `[System.IO.File]::WriteAllText($path, $jsonText, [System.Text.UTF8Encoding]::new($false))`.

### Commit format

```
feat(specs): <SPEC_SLUG> — <state count> states, matchRate <rate>
```

No AI/Claude attribution in commit messages.

### Report back (≤ 200 words)

- Final matchRate, matchOutcome, state count, assertion count
- PR URL + commit SHA (if committed)
- Criterion style that dominated (id / role+text / text-only)
- Page features without UI Bridge instrumentation (= frontend follow-up work)
- Any states that were deliberately omitted with a one-line reason

---

## What NOT to do

- Don't author criteria you haven't validated against a real snapshot.
- Don't pad assertion counts — 5 high-quality ≫ 20 brittle.
- Don't use `accessible_name` for visible text when `el.label` differs from `el.state.textContent` — known footgun.
- Don't ignore `no_candidates` by lowering thresholds. Fix or delete the assertion.
- Don't guess at page structure without taking a snapshot first.
- Don't write assertions about live data (row counts, specific names, user emails).
- Don't use the legacy `requiredElements` / `groups` shape — use `states[].assertions[]`.
