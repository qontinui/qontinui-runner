# Runner capabilities — which rung answers for each

Several of the runner's assets are resolved through **rung-ordered resolvers that try a developer checkout before falling back**. On the author's machine the checkout rung answers; on an external operator's machine it does not, and the fallback either silently differs or is absent. This document is the roster of those capabilities; the running binary reports which rung actually answered for each, and comparing a development build's report with a published build's report is the parity check.

<!-- GENERATED — do not edit by hand. Regenerate FROM BASH (Git Bash on Windows), from `src-tauri/`: `cargo run --quiet --bin qontinui-runner -- --capability-manifest-doc > ../docs/runner-capabilities.md`; against an installed binary the same flag works directly — `qontinui-runner --capability-manifest-doc > docs/runner-capabilities.md`. NOT from PowerShell, whose `>` writes UTF-16 or a BOM that `include_str!` cannot read at all. The source of truth is `CAPABILITY_SPECS` in `src-tauri/src/capability_manifest.rs`, and `.github/workflows/capability-manifest-fresh.yml` fails any PR whose checked-in copy differs from a fresh render. -->

Manifest schema version: `1`.

## The rungs

Ordered from *carried by the build* down to *found on the operator's disk*, then the two non-answers. A capability resolving near the top resolves the same way on every machine; one resolving near the bottom resolves only where a checkout happens to exist.

- `embedded` — compiled into the binary (`include_str!` / `include_dir!`) — present wherever the binary is
- `bundle_resource` — unpacked from the installer's `bundle.resources` and located via Tauri's `BaseDirectory::Resource`
- `served` — fetched over the network from qontinui-web or coord at run time
- `disk_cache` — read from a store this device wrote for itself (the on-disk override cache, or the local database) — never carried by the build
- `exe_relative_checkout` — found relative to `current_exe()`, i.e. the checkout this binary was built in — answers on a dev box and nowhere else
- `dev_checkout` — found under `<workspace-root>/qontinui-runner/src-tauri/…` — this repo's source tree, via the workspace root rather than the exe
- `operator_checkout` — found under a SIBLING repo checkout on the operator's disk (e.g. `qontinui-claude-config`) — answers only where that repo exists
- `unresolved` — every rung was tried and none answered — a stated outcome
- `unknown` — nothing observed this capability here — a finding about the OBSERVER, never about the machine

`unresolved` and `unknown` are **not** synonyms. `unresolved` is a finding about the machine: every rung was tried and none answered. `unknown` is a finding about the reporting binary: nothing observed the capability there. A row is never omitted for being unobservable, and an unobservable row is never rendered as the value the code would have fallen back to.

## The capabilities

### 1. `workspace_root`

Where the Qontinui repo checkouts live on this box. This is the root every checkout-bound capability below is resolved relative to, so it is the row that explains most of the others: when it is `unresolved`, every `dev_checkout` and `operator_checkout` row downstream of it is unresolved too, and the manifest should be read from this row outward. A published install on an operator's machine is EXPECTED to have no workspace root at all — that is the normal case for the audience, not a fault.

- Class: `path_resolution`
- Resolved by: `workspace_paths::runner_workspace_root → qontinui_types::paths::qontinui_workspace_root`
- Expected rungs: operator_checkout, exe_relative_checkout, unresolved

### 2. `bundled_resources`

Crate-bundled assets resolved at run time — `resources/code-semantics/ts-language-service.mjs` and everything under `data/` (`runner_state_machine.json`, `htn_methods/`). The resolver tries the installer's unpacked resource dir, then the checkout the exe was built in, then the workspace-root copy of this repo. Its own module doc names the failure this row exists to catch: resolving a developer-checkout file as a shipped one is "a wrong answer that looks right on the author's machine".

- Class: `bundled_asset`
- Resolved by: `bundled_resources::resolve_with_rung over resolve / exe_relative_checkout / dev_checkout`
- Expected rungs: bundle_resource, exe_relative_checkout, dev_checkout, unresolved

### 3. `spec_pages`

The UI Bridge page specs (IR, projection, notes) that back the spec API. Read FILESYSTEM-FIRST from a repo root the caller supplies, with the compile-time `EMBEDDED_PAGES` snapshot as the fallback — and the embedded snapshot covers the `qontinui-runner` app only, so for any other app the filesystem rung is the only rung there is. Filesystem-first means a dev box silently reads a DIFFERENT corpus from the one an operator gets.

- Class: `bundled_asset`
- Resolved by: `spec_api::storage::{read_ir, read_projection, read_notes, list_pages} over EMBEDDED_PAGES`
- Expected rungs: operator_checkout, embedded, unresolved

### 4. `fleet_commands`

The agent command procedures written into a spawned session's `<cwd>/.claude/commands/*.md`, so `/vet-plan` and friends resolve in a session whose cwd is a fresh worktree. Embedded via `include_str!`, so it should answer on every machine — and every failure path here degrades one step and `warn!`s rather than aborting the spawn, which is correct behaviour and completely invisible. This row is what makes that degradation a value.

- Class: `session_provisioning`
- Resolved by: `fleet_commands::provision_fleet_commands_into over FLEET_COMMANDS`
- Expected rungs: embedded, unresolved

### 5. `fleet_skills`

The agent SKILLS written into a spawned session's `<cwd>/.claude/skills/<name>/SKILL.md`. Embedded via `include_dir!` — a whole directory tree per skill, helper scripts included. Embedded-only today: the served-override half of plan 2026-08-20-fleet-served-agent-skills is qontinui-web#1071, which has not landed, so a `served` reading on this row would itself be a finding.

- Class: `session_provisioning`
- Resolved by: `fleet_skills::provision_fleet_skills_into over FLEET_SKILLS / embedded_skill_count`
- Expected rungs: embedded, unresolved

### 6. `fleet_agents`

The named-subagent definitions written into a spawned session's `<cwd>/.claude/agents/*.md` — the definitions `claude` reads to resolve `code-reviewer`, `merge-specialist` and the rest. Embedded via `include_dir!` as a FLOOR beneath the checkout copy (`agent_definitions` below), which still wins where it exists. Without either, a spawned agent silently has no subagents: the named subagent does not resolve, the review never runs, and coord eventually ages the PR out as `specialist_timeout` — a failure with no error at the point of cause.

- Class: `session_provisioning`
- Resolved by: `fleet_agents (FLEET_AGENTS, include_dir!)`
- Expected rungs: embedded, unresolved

### 7. `agent_definitions`

The CHECKOUT copy of the same subagent definitions, read from `<workspace-root>/qontinui-claude-config/.claude/agents/*.md` off the operator's disk. It outranks the embedded floor, so on a box that has that sibling repo this row — not `fleet_agents` — decides what a session actually gets. Two sources for one asset, with nothing asserting they agree; when the root does not resolve the copy is a no-op that logs "no qontinui-root resolved; skipping .claude/agents" and continues.

- Class: `session_provisioning`
- Resolved by: `agent_runtime::provision_agent_definitions_from_root`
- Expected rungs: operator_checkout, unresolved

### 8. `agent_commands_registry`

The account-versioned override registry for the agent command procedures: fetch `GET {base}/api/v1/agent-commands`, else the on-disk `agent-commands-cache.json`, else the embedded default. The one surface here that is genuinely operator-safe by design — and, until this manifest, the one where NOTHING reported which of the three arms won. The shape precedent for fixing that is `/health`'s `database.arm`, which already publishes exactly this kind of verdict.

- Class: `served_registry`
- Resolved by: `agent_commands::resolve_registry (fetch_overrides_blocking / read_cache_at / builtin)`
- Expected rungs: served, disk_cache, embedded

### 9. `slash_commands`

Import of `<workspace-root>/qontinui-claude-config/.claude/commands/*.md` as runner workflows. Purely a sibling-checkout scan with no embedded or bundled fallback of any kind, so on any device without that repo it returns `Err` and the workflows simply do not exist. The clearest instance of the class in the roster: it is not that this degrades on a published install, it is that it cannot run at all there.

- Class: `session_provisioning`
- Resolved by: `slash_commands::{find_commands_directory_reported, sync_slash_commands}`
- Expected rungs: operator_checkout, unresolved

---

This roster is incomplete by construction: a manifest reports only what someone thought to list, so it cannot catch a capability nobody enumerated. That is why the parity check has a second, behavioural axis, and why a disagreement between the two is reported as a finding **about this roster** rather than reconciled toward either side. Adding a capability is adding a row to `CAPABILITY_SPECS`.
