# Top-10 Daily Actions Audit — Terminal Page

> **Phase 1 deliverable** for the 2026-05-28 redesign plan (`plans/2026-05-28-terminal-page-redesign-plan.md` §0 success criterion (b), §4 Phase 1).
> The redesign is only justified if the new affordances (CommandBar, suggestion chips, hover controls, palette) are **at least as fast** as the current chrome for the operator's top-10 daily actions. This document fixes the top-10 list and per-action keystroke budget so Phase 2 (CommandBar v1) can be measured against it.

## Methodology

No production telemetry exists for chrome-button click frequency. The ranking instead triangulates four evidence sources, weighted by signal strength:

1. **Hotkey binding (strongest signal).** An operator-bound `Ctrl+Shift+*` chord means *"I do this multiple times per work session."* Source: `src/components/terminal/useKeyboardShortcuts.ts:91-296` (24 distinct bindings).
2. **Page-purpose alignment (strong).** The page is a coordination surface for *multiple concurrent AI sessions*. Actions that directly operate on that purpose (jump-to-needs-input, approve-all, spawn-AI) rank higher than generic terminal ops.
3. **Persistent-chrome real estate (weak).** A button that ships in `TerminalTabBar` or `ZoneStatusBar` was deemed visible-worthy by past authors. Many of those are now-rare-but-historically-promoted.
4. **UI Bridge `useUIComponent` exposure (orthogonal).** Actions exposed to external agents reflect *agent* needs, not operator frequency — used as a tie-breaker only.

A keystroke-budget rule supplements ranking: **for each top-10 action, the new CommandBar path must be ≤ the existing keystroke count.** Hotkey'd actions don't need to be replaced by CommandBar paths — they just need a slash equivalent for discoverability and AI/external-agent invocation.

## The top 10

Ranked by estimated frequency in a typical multi-AI session work day.

| # | Action | Current path(s) | Current keystrokes | Slash equivalent | New keystrokes (best case) |
|---|--------|-----------------|--------------------|------------------|----------------------------|
| 1 | **Focus a session (switch active zone)** | `Ctrl+Tab` / `Ctrl+1..9` / click the window | 1 chord or 1 click | `/focus <n>` | 1 click (unchanged, zones already clickable per the 2026-05-28 grid-first decision) |
| 2 | **Spawn a session** (default = plain PTY) | `Ctrl+Shift+T` / tab-bar `+` button | 1 chord OR 1 click | `/spawn` | 1 chord (`Ctrl+/` + `Enter` if `/spawn` is the recent top) |
| 3 | **Jump to next needs-input session** | `Ctrl+Shift+N` / status-bar `Next Action` button (only renders when count>0) | 1 chord | `/focus needs-input` (chip alternative for first-time learners) | 1 chord (unchanged) — slash form for discoverability only |
| 4 | **Approve all needs-input sessions** | `Ctrl+Shift+Enter` (sends `y\r` to every needs-input PTY) | 1 chord | `/approve-all` | 1 chord (unchanged) — slash form for discoverability only |
| 5 | **Maximize / restore current zone** | `Ctrl+Shift+F` / per-zone double-click (future: hover control) | 1 chord or 1 dblclick | `/maximize` (active zone) or `/maximize <n>` | 1 chord (unchanged) |
| 6 | **Close a session** | `Ctrl+Shift+W` / tab-bar per-tab close `X` (today) / hover-control close (Phase 5) | 1 chord OR 1 click | `/close` (active) or `/close <n>` | 1 chord (unchanged) |
| 7 | **Change layout preset** | `Ctrl+Shift+1..8` / `Ctrl+Shift+L` (cycle) / layout-picker dropdown | 1 chord OR multi-click | `/layout <preset>` | 1 chord (unchanged) — slash form replaces the dropdown's multi-click |
| 8 | **Spawn an AI session** (Claude under specific account, optional context prompt) | Tab-bar chevron → LaunchMenu → account select → count → context → submit | **5-8 clicks/keystrokes** (worst case in current chrome) | `/spawn-ai <count> <account>` and `/spawn-best <count>` | **5-6 keystrokes via CommandBar autocomplete**; **the marquee CommandBar win** |
| 9 | **Restart a completed/errored session in its zone** | `Ctrl+Shift+R` / chip on errored zones (Phase 4) | 1 chord | `/restart` (active zone) or `/restart-zone <n>` | 1 chord (unchanged) — gated on state ∈ {`completed`, `error`} per `TransitionEffectsContext.tsx:39-69` |
| 10 | **Swap two zones' tab assignments** | `Ctrl+Shift+X` (mark source, focus dest, repeat to swap) | 2 chords + zone-focus interactions | `/swap <a> <b>` | 1 CommandBar invocation (~6 keystrokes) — **fewer keystrokes than today's two-step chord workflow** |

### Why these and not the others

**Action candidates rejected from the top 10**, with one-line justification:

| Action | Why not top-10 |
|--------|----------------|
| Toggle auto-focus on needs-input (`Ctrl+Shift+A`) | Set-and-forget; toggled <1×/day in steady use. Hotkey survives, slash exists, no CommandBar pressure. |
| Toggle sound notification (`Ctrl+Shift+S`) | Set once at start of session; same logic. |
| Toggle focus mode (`Ctrl+Shift+D`) | Same — preference-set, not per-action. |
| Output search (`Ctrl+Shift+/`) | Frequent for power users, but bounded to "I lost something" — not per-session-action. Belongs to top-15. |
| Pin zone (`Ctrl+Shift+O`) | Layout-organization action, set-and-forget. |
| Filter by tag (`Ctrl+Shift+G`) | Only matters in heavily-labeled workspaces; opt-in feature. |
| Save / load profile | Rare (~1×/week for someone who profiles workflows) but high-value when used. Top-15. |
| Generate workflow from session | Workflow-builder integration; the operator running it is in a *different mode* than "watching sessions." Top-20. |
| Analyze dropdown (6 analysis types) | Same — investigative mode, not coordination mode. Top-20. |
| Plan: Implement / Verify / Refresh | Plan-builder integration; rare. |
| Findings / File-ownership / Sessions-sidebar toggles | All set-once-and-keep-open in their respective workflows. |
| Auto-approve / auto-restart configuration | Preference-set. |
| Reorganize pages (AI) | Page-level, weekly cadence at most. |
| Resume frozen session (`Ctrl+Shift+J`) | Rare unless an operator routinely freezes/resumes — not the common path. |
| View-mode cycle (`Ctrl+Shift+M`) | Three-state visual density toggle; rarely re-cycled. |
| Resume / Sessions sidebar toggle (`Ctrl+Shift+B`) | Mode switch, not per-action. |
| Export / Sort zones / Doc finder / Metrics / History / Shortcuts help | All discoverable-on-demand from palette; no need to ship them in default chrome. |

The cut-off between #10 and #11 is somewhat arbitrary. **#10 Swap zones** was kept above #11 (Output search) because today's swap-zone UX is unusually awful (two-step chord + zone focus) — the redesign delivers measurable improvement here whereas Output search already works. If telemetry from Phase 2 contradicts this, demote swap and promote search.

## Per-action success criterion checks

For each top-10, the new CommandBar invocation path must satisfy:

```
new_keystrokes(action)  <=  current_keystrokes(action)    [hard requirement]
new_latency(action)     <=  current_latency(action) + 50ms [soft requirement]
```

### Hotkey-dominant actions (#1, #3, #4, #5, #6, #7, #9)

These are already 1-chord. The CommandBar is **not** intended to beat the hotkey — it provides the slash equivalent for:
- Operator discoverability (autocomplete teaches the hotkey via its display in the suggestion row)
- AI tool-call invocation (Tier 3 maps free text → action)
- External-agent invocation via the UI Bridge adapter

Hard requirement here = "exists in the registry with the correct paramSchema." No latency or keystroke regression possible.

### Marquee CommandBar wins (#8 Spawn AI, #10 Swap zones)

The two top-10 actions where today's chrome is genuinely worse than a typed command:

**#8 Spawn AI — current path:**
1. Click chevron at tab bar (1 click) → 2. wait for `LaunchMenu` to render → 3. click "Create AI Session" tile (1 click) → 4. click account row (1 click) → 5. type count, e.g. `3` (1 keystroke) → 6. *(optional)* paste context prompt → 7. click submit (1 click). **Minimum 4 clicks + 1 keystroke + dropdown render wait ≈ 5-8 actions.**

**#8 via CommandBar:**
1. `Ctrl+/` (1 chord) → 2. type `sp` → 3. autocomplete shows `/spawn-ai 1 <account>` at top → 4. Tab to fill in best-account placeholder → 5. edit count to `3` → 6. Enter. **5-6 keystrokes**, zero clicks, no dropdown render wait.

**#10 Swap zones — current path:**
1. Focus zone A (`Ctrl+<n>`) → 2. `Ctrl+Shift+X` (marks source) → 3. focus zone B (`Ctrl+<m>`) → 4. `Ctrl+Shift+X` (swaps). **4 chords**, with a mental model gotcha (must remember which is marked).

**#10 via CommandBar:** `Ctrl+/`, `swap 2 5`, Enter. **~8 keystrokes total**, no mode tracking. Fewer chords, less mental load.

### Per-zone-control actions (#2, #5 alt, #6 alt, #9 alt)

Phase 5 adds hover controls in each zone for spawn/maximize/close/restart. These add a third path alongside hotkey + slash. The hard requirement is that none of the three paths regresses; the hover path is "discoverable for the operator who doesn't know the hotkey yet."

## Open questions surfaced by this audit

These do not block Phase 1, but are worth flagging:

1. **#3 (Jump to needs-input) and #4 (Approve-all) are AI-session-specific.** A user running mostly plain PTYs would re-rank these down. Should the audit be re-run with a usage-mode tag (AI-heavy vs. plain-shell-heavy)? Recommendation: keep the AI-heavy rank as the default since the redesign motivation was multi-AI-session work specifically.
2. **Compound actions are not in the top-10** (e.g., *"close all completed"*, *"restart every errored zone"*, *"spawn 3 in this repo and load profile X"*). These are the natural Tier-3 AI use cases. Phase 8 may justify itself entirely on the strength of these — track Tier-2 vs. Tier-3 acceptance rate per phrasing during dogfooding.
3. **#8 Spawn AI's account selection** is currently order-dependent (LaunchMenu sorts by `utilization`). The slash form `/spawn-ai 3 <account>` should accept either the explicit account name or the literal `best`/`@best` to map to `create-best-account`. Phase 1's `paramSchema` must accommodate this.
4. **Where do per-zone hover controls (Phase 5) win the keystroke race?** Mostly for mouse-driven operators who don't memorize chords. The audit assumes a keyboard-driven operator; a mouse-driven operator's top-10 would put Phase-5 hover actions higher (close, restart, maximize). Both populations are served; the keystroke budgets above are for the keyboard-driven case.

## Action-registry entries implied by this audit

Phase 1 `registry.ts` MUST register at least these (slash → handler). IDs chosen to align with existing UI Bridge `useUIComponent` action names where they overlap (preservation per §5(4) of the plan):

| Slash | Registry id | Existing UI Bridge id | Handler source |
|-------|-------------|------------------------|----------------|
| `/focus <n>` | `terminal.focus` | — | `zoneLayout.setFocusedZone` |
| `/focus needs-input` | `terminal.focus.needs-input` | — | `zoneLayout.focusNextNeedsInput` |
| `/focus next` / `/focus prev` | `terminal.focus.next` / `.prev` | — | `zoneLayout.focusNextZone` / `focusPrevZone` |
| `/spawn` | `terminal.spawn` | `create-plain` | `onQuickLaunch` (TerminalPage.tsx:539) |
| `/spawn-ai <count> <account>` | `terminal.spawn-ai` | `create-ai-session` | `onLaunchAiSession` (TerminalPage.tsx:559) |
| `/spawn-best <count>` | `terminal.spawn-best` | `create-best-account` | (uses `onLaunchAiSession` with `sortedAccountsForBridge[0]`) |
| `/spawn-with <count> <command>` | `terminal.spawn-with` | `create-with-command` | `onQuickLaunch` with autoCommand |
| `/approve-all` | `terminal.approve-all` | — | inline writer in `useKeyboardShortcuts.ts:142-151` (extract to a helper) |
| `/maximize` / `/maximize <n>` | `terminal.maximize` | — | `zoneLayout.toggleMaximize` (`useZoneLayout.ts:303-308`) |
| `/close` / `/close <n>` | `terminal.close` | — | `closeTerminal` |
| `/layout <preset>` | `terminal.layout` | `zone-layout-picker` actions (4th `useUIComponent` site) | `zoneLayout.setLayoutId` |
| `/restart` / `/restart-zone <n>` | `terminal.restart` | — | `transitionEffects.handleRestartInZone` (gated; surface gate in result) |
| `/swap <a> <b>` | `terminal.swap` | — | `zoneLayout.assignTabToZone` (twice, as in `useKeyboardShortcuts.ts:171-184`) |

That is 12 action entries to cover the top-10 (some actions have multiple arg shapes). The Phase-1 stretch set of ~25 expands this with profile/export/search/etc.

## Done definition for this audit

- ✅ Top-10 named, ranked, with evidence sources cited.
- ✅ Each entry has a current-keystroke count and a new-keystroke budget.
- ✅ Rejected candidates listed with one-line justification.
- ✅ Implied registry entries enumerated with existing handler sources.
- ✅ Open questions flagged for operator review.

When the operator signs off on this list (or amends it), Phase 1 plumbing (`registry.ts`, `useCommandAction.ts`, `types.ts`) can begin against this exact target.
