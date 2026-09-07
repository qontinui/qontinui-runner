---
name: visual-audit
description: Run a declarative visual audit on a UI Bridge-connected page. Combines five vision-core analyzers (layout, typography, color, dynamic, elements) with the 12-assertion DSL (no_overlap, element_above, contains_text, text_fits_container, aligned_*, color_within, typography_consistent, no_layout_shift_since, no_clipping, animation_settled, contrast_meets_wcag). Returns structured findings + pass/fail per assertion — never pixels. Use as the canonical "audit this page for visual regressions / a11y violations / layout bugs" entrypoint.
user-invocable: true
---

# Visual Audit

Phase 6 of the UI Bridge Vision Pipeline. The runner exposes four endpoints
that turn declarative visual questions into structured answers:

| Endpoint | What it does |
|---|---|
| `POST /ui-bridge/vision/analyze` | Run one of the five analyzers (layout/typography/color/dynamic/elements). Returns `findings: [{kind, severity, region?, detail, elements?}]`. |
| `POST /ui-bridge/vision/assert` | Evaluate a list of declarative assertions over the captured frame + caller-supplied snapshot. Returns per-assertion pass/fail + reason. |
| `POST /ui-bridge/vision/baseline` | Capture a baseline image + register the snapshot's element bboxes under `name`. |
| `GET  /ui-bridge/vision/baselines` | List registered baselines. |

`/control/visibility`, discussed later, is **not** among them — it lives on the
web UI Bridge SDK, a different surface, and the runner 404s it.

Both `analyze` and `assert` require a **caller-supplied ElementSnapshot**.
> ### ⚠️ A raw `discover` payload is NOT a snapshot. Project it first.
>
> This skill used to document `curl .../discover | jq '.data'` piped straight
> into `analyze`. **That recipe silently returns a clean bill of health on a
> broken page**, and it is why the occlusion bug on the runner's Terminal page
> went unreported by every audit run against it.
>
> `DiscoveredElement` and vision-core's `Element` share exactly ONE field —
> `id`. Geometry lives at `state.rect` as `{x,y,width,height}` floats, not at
> `bbox` as `{x,y,w,h}` ints; text lives at `state.textContent`;
> `interactable` is derived, not carried. Rust's `Element` has no
> `deny_unknown_fields`, so the mismatched payload **parses** — into a
> snapshot where every element is `bbox: None, text: None,
> interactable: false`. The layout analyzer then filters its pairwise checks
> on exactly those fields, finds an empty set, and returns **zero findings**,
> byte-identical to a genuine pass.
>
> `rust-vision-core` pins this with a test named
> `a_raw_discover_payload_is_not_a_supported_input`. Project properly, or
> don't ask.
>
> **The server now refuses this, and that is the part you can rely on.** Every
> analyzer computes a `SnapshotCoverage` from the snapshot itself and returns an
> explicit `verdict`. A snapshot with no measurable geometry makes `layout`
> answer **`{"state":"blocked", "reason":…}`** — *the preconditions were not
> met*, which is a distinct answer from `"checked"` with an empty finding list,
> and it is not green. You no longer have to read a stderr line to find out
> whether a clean verdict meant anything: **read `verdict.state`.**

Get the snapshot from the runner's own `discover` endpoint and run it through
the projection script:

```bash
# 1. Discover -> project into a real ElementSnapshot.
#    --stats goes to stderr; READ IT (see below).
curl -fsS -X POST http://127.0.0.1:9876/ui-bridge/control/discover \
     -H 'Content-Type: application/json' -d '{"interactive_only":false}' \
  | python3 "$ROOT/qontinui-claude-config/scripts/uibridge-to-elementsnapshot.py" \
      --stats > /tmp/snapshot.json

# 2. Run the layout analyzer (occlusion, overlap, zero-area, alignment).
python3 -c 'import json,sys;print(json.dumps({"analyzer":"layout","snapshot":json.load(open("/tmp/snapshot.json"))}))' \
  | curl -fsS -X POST http://127.0.0.1:9876/ui-bridge/vision/analyze \
      -H "Content-Type: application/json" -d @- \
  | python3 -c 'import json,sys;[print("[%s] %s: %s" % (f["severity"], f["kind"], f["detail"])) for f in json.load(sys.stdin)["data"]["findings"]]'
```

`python3`, not `jq` — jq is absent on the Windows operator box, and a missing
binary there reads identically to an empty result.

**Read the response's `verdict`. The server decides this now — you are not the
gate.** Every `analyze` response carries a `coverage` object (the same four
counters `--stats` prints) and an explicit `verdict`:

| `verdict.state` | Means | Green? |
|---|---|---|
| `"checked"` | Preconditions met. An empty finding list here genuinely means clean. | yes |
| `"degraded"` | Ran, but a named dimension was unmeasurable — e.g. no stacking order, so occlusion is UNKNOWN. Findings are real but incomplete. Carries a `reason`. | yes |
| `"blocked"` | Preconditions **not** met. The input was too impoverished to check anything, so the finding list answers nothing. Carries a `reason`. | **no** |

The verdict is an internally-tagged object, so read `verdict.state` (and
`verdict.reason` on the two non-`checked` states), not a bare string.

A snapshot whose elements carry no geometry makes `layout` answer `Blocked`
rather than returning `[]`. That is the whole point: a `layout` result that
used to be byte-identical to a clean page now carries a verdict that is not
green. Note the scope — this is a statement about each analyzer's own
preconditions, not a guarantee that some other analyzer cannot still return an
honestly-empty `Checked`.

The `--stats` line still prints, and is still useful for seeing *why* a verdict
came out the way it did before you send anything — but it is **no longer the
defence**. It reports the same counters the server now computes for itself:

```
projected 118/118 elements: 118 with geometry, 96 with stacking order, ...
```

> **`Degraded` is green on purpose, and this is worth understanding.** Almost no
> real snapshot carries stacking order — the projector emits `z_index` only when
> the computed `zIndex` parses as an integer, and `auto` deliberately does not —
> so a `Degraded`-fails-the-gate rule would fire on essentially every page and
> teach everyone to ignore it. `Degraded` is a statement about *coverage*, not a
> defect. Only `Blocked` is non-green.
>
> **And `coverage.withStacking` counts POPULATED ranks, not correct ones.** The
> layout analyzer compares `z_index` across stacking contexts, while a CSS
> `zIndex` read is per-context; where a producer emits the latter, occlusion
> verdicts can invert while coverage reads 100 %. A high `withStacking` is not a
> trust signal.

### No runner window? The checks still run.

`analyze` and `assert` capture a frame **best-effort**. Layout, typography and
elements are pure geometry over the snapshot, and no assertion in the DSL
reads the frame at all, so a runner reporting `frontendState: "window_missing"`
still answers every geometric question. A failed capture comes back as
`frameError` in the response — check for it rather than assuming the pixels
agreed. Only `color` and `dynamic` degrade, and they say so with a `skipped`
finding.

The same holds for the `vision-audit` binary, where `--frame` is optional —
that is the path qontinui-web's CI style gate uses, with no runner in the loop
at all.

### Targeting a remote device (phone / registered app)

By default `analyze` / `assert` / `baseline` capture the **runner's own desktop
window**. To audit a *different* surface, pass a `target` field with the
device/app id — the runner sources the frame from that target. **Crucially, the
`snapshot` must also come from the same target**, because the assertion logic
matches element bboxes against the captured frame; a runner-sourced snapshot
over a device-sourced frame would be incoherent. Fetch `discover` from the
device, not the runner:

```bash
# Snapshot AND frame from the same target (the device serves both).
#
# ⚠️ DO NOT run this one through the projection script — this is the ONE place
# in this skill where `| jq '.data'` is correct. The React Native SDK on :8087
# emits the runner-native vision-core shape ALREADY: `bbox {x,y,w,h}` at top
# level, `fg_color` / `font_size_px`, and a truthful `interactable`. It is an
# ElementSnapshot on arrival, by design — see ui-bridge
# `packages/ui-bridge-native/src/core/vision-fields.ts`, whose header says the
# fields are emitted "so a visual-audit caller can post the snapshot through
# with no transform", and `rust-vision-core/src/element_snapshot.rs`, whose
# `bbox` doc-comment names a mobile `discover` snapshot as a supported source.
# The projector reads the WEB shape (`state.rect`, `state.computedStyles`), so
# projecting a native payload DROPS every one of those fields — measured: 0
# with geometry, and every element marked interactable by `inferActions`.
SNAP=$(curl -sS "http://<device-ip>:8087/ui-bridge/control/discover" | jq '.data')

# -sS, not -fsS: an unknown `target` 500s with `unknown vision target '<id>'`
# in the RESPONSE BODY, which the paragraph below tells you to read. `-f`
# discards the body and prints only curl's own exit-22 line, which cannot
# distinguish a typo'd id from an offline device.
curl -sS -X POST http://127.0.0.1:9876/ui-bridge/vision/analyze \
  -H "Content-Type: application/json" \
  -d "$(jq -nc --argjson s "$SNAP" '{analyzer:"layout", snapshot:$s, target:"<device-id>"}')" \
  | jq '.data.findings[]'

# assert + baseline take `target` the same way
curl -sS -X POST http://127.0.0.1:9876/ui-bridge/vision/assert \
  -H "Content-Type: application/json" \
  -d "$(jq -nc --argjson s "$SNAP" '{snapshot:$s, target:"<device-id>", assertions:[{"type":"no_clipping"}]}')"
```

`target` resolves against a registered physical device → registered app → adb
serial, in that order; an unknown id 500s with `unknown vision target '<id>'`,
and a target serving no screenshot fails loudly rather than silently capturing
the runner desktop. Omit `target` for the default runner-desktop behavior.

## The Five Analyzers

| Analyzer | What it checks |
|---|---|
| **layout** | **Directed occlusion** (which element is on top and what it hides — see below), pairwise overlap among interactive elements (ignoring nested layouts), zero-area elements, alignment jitter on near-y-baseline groups (3+ elements within 3px). |
| **typography** | Font-family + font-size cluster counts. Flags >3 distinct families or >8 distinct sizes (warning + info — usual sign of design-system drift). |
| **color** | WCAG contrast for every text element with both `fg_color` and `bg_color` populated. Flags individual sub-AA (4.5:1) elements + a "contrast density" warning when ≥25% of text fails. |
| **dynamic** | Two-frame pixel diff. Currently requires the caller to wire up a `prior_frame` source; this maps to `vision/diff` for now. |
| **elements** | Empty-snapshot detection, no-interactive / no-text smell, sub-24×24 interactive targets (WCAG 2.5.8). |

### `occlusion` vs `overlap` — they answer different questions

`overlap` asks *"do two clickable targets collide?"*: interactive elements
only, and full nesting is exempted as intentional layout (a button inside its
container).

`occlusion` asks *"is anything hidden from the reader?"*, and inverts all three
of those choices on purpose:

| | `overlap` | `occlusion` |
|---|---|---|
| Participants | `interactable` only | every element with a bbox |
| Full containment | exempt (intentional nesting) | the WORST case, always reported |
| Direction | none — symmetric pair | `z_index` names occluder → occluded |
| Severity | Warning | **Critical** when the covered element has text |

The participant rule is the one that matters most in practice: what a floating
widget hides is nearly always a *label* — a name, a status, a count — and
labels are never interactive, so `overlap` structurally cannot see this bug
class.

A finding reads:

```
[critical] occlusion: terminal-zone-minimap (z=30) covers 32% of
           terminal-zone-header-8 (z=10) (text: "Zone 8: qontinui-web (a3f2c1d0)")
```

**`occlusion_unknown` is not a pass.** When intersecting elements carry no
usable stacking order the analyzer reports that it could not determine a
direction. Treat it as UNKNOWN and fix the projection, never as "nothing is
covered".

Ancestor/descendant pairs are exempt — a child painting over its own parent's
box is what nesting *is*.

### `/control/visibility` — the focused question

`analyze` sweeps the page. When you want just "what is covering what", the
**web UI Bridge SDK** (`@qontinui/ui-bridge`) exposes a dedicated endpoint that
reports the directed relation — occluder → occluded — sourced from the
registry's own `elementFromPoint` hit-test (`state.occludedBy` /
`occludedPct`). The hit-test observes what the compositor actually painted, so
it sees `clip-path`, transformed ancestors and scroll clipping that a
bounding-box model cannot derive.

**Which surfaces serve it — check before you read a 404 as a pass:**

| Surface | `/control/visibility` |
|---|---|
| Web SDK in-page (`@qontinui/ui-bridge` server handlers) | **served** |
| qontinui-web relay (`:3001/api/ui-bridge`, `https://qontinui.io/api/ui-bridge`) | **served** — relay twin, hit-test half only, so findings are a SUBSET of the in-page endpoint's |
| Runner `:9876` (Tauri WebView) | **404 — not served.** The runner has its own **Rust** route table (`qontinui-runner/src-tauri/src/mcp/ui_bridge/routing.rs`); the frontend's `@qontinui/ui-bridge` npm dependency does not put routes on that port |
| React Native (`@qontinui/ui-bridge-native`) | **404 — not served**, pinned by `packages/ui-bridge-native/src/server/__tests__/http-status-mapping.test.ts` |

```bash
# Web SDK / relay surface — a DIFFERENT APPLICATION from the runner's :9876
# that every other example in this skill targets. Only run this when the page
# you are auditing is itself qontinui-web (see the applicability note below).
# -f so the 404 below is a non-zero exit rather than a 0 with an error body,
# -S so it still says why under -s. A 404 that exits 0 reads as a pass.
curl -fsS -X POST http://localhost:3001/api/ui-bridge/control/visibility \
  -H 'Content-Type: application/json' -d '{"minRatio":0.02}'
```

**Only ask this of the surface you are actually auditing.** The endpoint
answers about the surface it is sent to, and `:3001` is qontinui-web — not the
runner. This skill's own worked example is a *runner* page (the Terminal-page
occlusion bug above), and for that page the command has no correct target:
running it anyway returns a healthy-looking 200 about qontinui-web, which is
worse than the 404 in the table, because nothing in the response marks it as
off-target. **On the runner and on React Native there is no visibility endpoint
to reach** — record occlusion as UNVERIFIED and use `analyze`'s `occlusion`
finding, which reaches every surface that can produce a snapshot.

`verdict` is `clear`, `occlusions_found`, or `unknown_empty_registry` — the
last of which is why an empty `occlusions` list is not automatically good
news.

**A 404 is UNKNOWN, never a pass.** On the runner and on React Native the route
does not exist, and a web surface running an `@qontinui/ui-bridge` build older
than 2026-08-27 (`4284cd29`) will 404 it too. Never delete the check on a 404:
record the page's occlusion state as UNVERIFIED and fall back to `analyze`'s
`occlusion` finding (above), which reaches every surface that can produce a
snapshot.

⚠️ **`includeExpected` is accepted and echoed back but NEVER APPLIED.** Commit
`ccb77d2` deleted the `if (occ.isExpectedOverlay && !includeExpected) continue;`
filter along with the `computeVisibility` import (a genuine package cycle —
`ui-bridge-auto` depends on `ui-bridge`), and both the handler and its relay
twin now hardcode `isExpectedOverlay: false`. So **an open modal or dropdown
reports `occlusions_found` with `hidesText: true`** — expected overlays are not
suppressed. Triage those by hand against what you know is open; do not read them
as a regression. Restoring the filter needs a design decision about where
`isExpectedOverlay` is computed, tracked in plan
`2026-08-27-mobile-relay-followups-observability-and-sdk-contracts`.

## The Assertion DSL

Each assertion is a tagged-JSON object:

| Assertion | Body shape |
|---|---|
| `no_overlap` | `{"type":"no_overlap","elements":["btn-a","btn-b"],"tolerance_px":0}` |
| `element_above` | `{"type":"element_above","elements":["dropdown","panel"],"require_overlap":true}` — asserts `elements[0]` paints **on top of** `elements[1]`. Reads the snapshot's resolved `paint_order`, **never** a raw `z-index`: a `z-50` element nested inside a `z-10` stacking context genuinely loses to a `z-20` sibling of that context, so a raw-z comparison reports the opposite of the truth. `require_overlap` defaults **true** — "which is on top" is not a question two disjoint elements answer. When the snapshot source did not resolve stacking, `paint_order` is absent and the assertion answers **"cannot answer"** rather than passing vacuously. |
| `contains_text` | `{"type":"contains_text","target":{"element":"h1"},"text":"Hello","kind":"contains"}` |
| `text_fits_container` | `{"type":"text_fits_container","element":"label-7"}` — checks **both** axes. Horizontal truncation (`scroll_width_px > bbox.w`, i.e. a `truncate` / `text-overflow: ellipsis`) is the common failure and was undetectable before; an element with no `scroll_width_px` reports the horizontal axis as UNKNOWN rather than passing silently. |
| `aligned_horizontally` | `{"type":"aligned_horizontally","elements":["a","b","c"],"axis_tolerance_px":2}` |
| `aligned_vertically` | `{"type":"aligned_vertically","elements":["nav-1","nav-2"]}` |
| `color_within` | `{"type":"color_within","element":"logo","expected":{"r":255,"g":51,"b":51},"delta_e_max":5}` |
| `typography_consistent` | `{"type":"typography_consistent","elements":["h1","h2"],"dimensions":["font_family"]}` |
| `no_layout_shift_since` | `{"type":"no_layout_shift_since","baseline":"v1.0-home","tolerance_px":2}` |
| `no_clipping` | `{"type":"no_clipping","region":{"x":0,"y":0,"w":1280,"h":720}}` |
| `animation_settled` | `{"type":"animation_settled","region":null,"settle_frames":3}` |
| `contrast_meets_wcag` | `{"type":"contrast_meets_wcag","element":"btn-save","level":"aa"}` |

### Example: audit the page for the kinds of bugs Phase 6 was designed to catch

```bash
# PROJECT FIRST. `discover | jq '.data'` is not a snapshot — see the warning at
# the top of this file; it returns zero findings on a broken page.
curl -fsS -X POST http://127.0.0.1:9876/ui-bridge/control/discover \
     -H 'Content-Type: application/json' -d '{"interactive_only":false}' \
  | python3 "$ROOT/qontinui-claude-config/scripts/uibridge-to-elementsnapshot.py" \
      --stats > /tmp/snapshot.json
SNAP=$(cat /tmp/snapshot.json)

curl -fsS -X POST http://127.0.0.1:9876/ui-bridge/vision/assert \
  -H "Content-Type: application/json" \
  -d "$(jq -nc --argjson snapshot "$SNAP" '{
    snapshot: $snapshot,
    assertions: [
      { "type": "no_overlap", "elements": ["button-terminal-1", "button-terminal-2"] },
      { "type": "no_clipping" },
      { "type": "aligned_horizontally", "elements": ["nav-link-1","nav-link-2","nav-link-3"], "axis_tolerance_px": 2 }
    ]
  }')" | jq '.data'
```

Response:

```json
{
  "results": [
    { "passed": false, "detail": "button-terminal-1 and button-terminal-2 overlap by 1632 px²",
      "assertion": { "type": "no_overlap", "elements": ["button-terminal-1","button-terminal-2"] } },
    { "passed": true, "assertion": { "type": "no_clipping" } },
    { "passed": true, "assertion": { "type": "aligned_horizontally", "elements": [...] } }
  ],
  "allPassed": false
}
```

## Baselines

Register a "what good looks like" snapshot for a route:

```bash
curl -fsS -X POST http://127.0.0.1:9876/ui-bridge/vision/baseline \
  -H "Content-Type: application/json" \
  -d "$(jq -nc --argjson snapshot "$SNAP" '{
    name: "home-v1.2",
    snapshot: $snapshot
  }')"
```

Later, assert nothing has shifted:

```json
{ "type": "no_layout_shift_since", "baseline": "home-v1.2", "tolerance_px": 2 }
```

Baselines live in-process; they do **not** persist across runner
restarts. Use them for in-session regression checks, not durable
artifacts. List with `GET /ui-bridge/vision/baselines`.

## Canonical Audit Recipe (the original motivating use case)

After landing UI changes, run the full audit:

```bash
# PROJECT FIRST — same reason as above. Read the --stats line on stderr:
# `0 with geometry` / `0 with stacking order` are UNKNOWN, not a clean page.
curl -fsS -X POST http://127.0.0.1:9876/ui-bridge/control/discover \
     -H 'Content-Type: application/json' -d '{"interactive_only":false}' \
  | python3 "$ROOT/qontinui-claude-config/scripts/uibridge-to-elementsnapshot.py" \
      --stats > /tmp/snapshot.json
SNAP=$(cat /tmp/snapshot.json)

# 1. Layout analyzer — catches the overlap bugs the plan was designed for
curl -fsS -X POST http://127.0.0.1:9876/ui-bridge/vision/analyze \
  -H "Content-Type: application/json" \
  -d "$(jq -nc --argjson s "$SNAP" '{analyzer:"layout",snapshot:$s}')" \
  | jq '.data.findings[] | select(.severity != "info")'

# 2. Elements analyzer — sub-24×24 targets + empty-snapshot smells
curl -fsS -X POST http://127.0.0.1:9876/ui-bridge/vision/analyze \
  -H "Content-Type: application/json" \
  -d "$(jq -nc --argjson s "$SNAP" '{analyzer:"elements",snapshot:$s}')" \
  | jq '.data.findings[]'

# 3. Contrast — only meaningful when snapshot supplies fg_color + bg_color
curl -fsS -X POST http://127.0.0.1:9876/ui-bridge/vision/analyze \
  -H "Content-Type: application/json" \
  -d "$(jq -nc --argjson s "$SNAP" '{analyzer:"color",snapshot:$s}')" \
  | jq '.data.findings[]'

# 4. A default assertion bundle for every interactive page
curl -fsS -X POST http://127.0.0.1:9876/ui-bridge/vision/assert \
  -H "Content-Type: application/json" \
  -d "$(jq -nc --argjson s "$SNAP" '{
        snapshot: $s,
        assertions: [
          { "type": "no_clipping" }
        ]
      }')" | jq '.data.allPassed'
```

The terminal-tab overlap bug that triggered the whole Phase 6 design
becomes a one-line `no_overlap` invocation that reports it.

## When NOT to use

- **Pixel-level comparison vs a baseline image** — use `vision/diff` instead.
- **Generating image output for a downstream model** — use `vision/capture`.
- **OCR-only / "what text is visible"** — use `vision/extract` (the `/visual-check` skill).
- **Single-query "describe what's on screen"** — use `vision/describe`.

The audit endpoints are for declarative assertions: "I expect X to be
true; tell me whether it is." That's the right shape for regression
checks and a11y gates, not for ad-hoc exploration.

## Configuration

No env-var configuration — the endpoints route through the same
in-process state (cache, semaphore, baselines map) as the rest of the
vision pipeline. The OCR + VLM client envs (`QONTINUI_VISION_OCR_*`,
`QONTINUI_VISION_VLM_*` from Phase 4) apply if a `contains_text`
assertion needs to fall back to OCR.
