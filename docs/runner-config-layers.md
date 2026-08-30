# Runner configuration layers

Every layer below independently contributes to the effective configuration of a runner session. There is no merge function anywhere — each one is resolved by its own hand-rolled resolver — which is why `config report` aggregates them instead of trying to unify them.

<!-- GENERATED — do not edit by hand. Regenerate FROM BASH (Git Bash on Windows): `cargo run --bin config_report -- --layer-doc > docs/runner-config-layers.md`. NOT from PowerShell, whose `>` writes UTF-16 or a BOM that `include_str!` cannot read at all. The source of truth is `LAYER_SPECS` in `src-tauri/src/config_report.rs`, and `config_report_checked_in_layer_doc_is_the_generators_output` fails if the checked-in file drifts from it. -->

## 1. Settings struct (~90 fields) — WHOLE-FILE provenance (`settings_struct`)

whether the ~90-field settings document this runner is acting on is the user's real persisted state (`loaded`), the compiled-in defaults of a genuine first run (`fresh_install`), or a DEFAULT PLACEHOLDER standing in for a file that exists and could not be read (`unreadable` — the variant where `tier`, `web_integration.runner_token`, `setup_completed`, `qontinui_user_id` and the sync toggles are not the user's values at all). PER-FIELD attribution — "did `app_mode` come from disk or from `default_app_mode()`?" — was CONSIDERED AND DELIBERATELY NOT BUILT. It would take a hand-written `serde::Deserialize` over ~90 fields, each recording whether its `#[serde(default = "…")]` fired: a large new correctness surface on the one code path every identity, tier and credential decision in the runner already depends on, in exchange for a question no reported confusion case turns on. The three that motivated this report are answered elsewhere — env-var staleness by the env-generation section, the shared `.mcp.json` write refusal by layer 14, and the endpoint/config-dir precedence by layers 2 and 5. This row is therefore resolved at a deliberately coarser grain than the plan first proposed, and says so in its own output

- Resolved by: `settings::read_settings_from_disk → settings::SettingsProvenance`
- Owned by: bin
- Status: reported

## 2. Config-dir resolution (`config_dir`)

which directory settings.json is read from and written to, overridable by the `QONTINUI_CONFIG_DIR` env var; RESOLVED and statted, never created

- Resolved by: `settings::resolve_config_dir`
- Owned by: bin
- Status: reported

## 3. Second independent reader of settings.json (`settings_json_second_reader`)

the lib-side reader of the SAME settings.json the bin reads — a second path to the same file with its own error handling, so the two can disagree about whether it was readable

- Resolved by: `profiles::settings_json_path`
- Owned by: lib
- Status: reported

## 4. profiles.json active profile → coord base (`profiles_coord_base`)

the effective coordinator base URL and which of the five resolution arms produced it (env `COORD_HTTP_URL`, the `QONTINUI_ENV`-selected profile's `coord_url`, the tier default, the unknown-tier production default, or the dev-localhost guess)

- Resolved by: `profiles::coord_base_policy`
- Owned by: lib
- Status: reported

## 5. Endpoint registry — qontinui-web backend base URL (`api_endpoint_registry`)

the single source of truth for the web-backend base across every runner subsystem, resolved over a documented four-rung order (env `QONTINUI_WEB_BACKEND_URL`, env `QONTINUI_API_URL`, the persisted paired backend, the build default)

- Resolved by: `api_config::resolve_api_base_url`
- Owned by: bin
- Status: reported

## 6. Launch-env snapshot (`launch_env_snapshot`)

the runner's own environment, read ONCE in `main` — the first of the generations that makes an env var three restarts deep

- Resolved by: `launch_env::RunnerLaunchEnv::read`
- Owned by: bin
- Status: reported

## 7. Ad-hoc runtime env reads (`adhoc_env_reads`)

the `std::env::var` calls scattered through the runtime, which see the CURRENT process env rather than the launch snapshot — so two values sourced from "the environment" can disagree

- Resolved by: `std::env::var call sites (runner binary)`
- Owned by: bin
- Status: reported

## 8. Supervisor-injected env (`supervisor_injected_env`)

the environment the dev supervisor injects when it spawns a runner — not present in this process's own configuration at all, only in the env it was handed

- Resolved by: `qontinui-supervisor process::manager (spawn env)`
- Owned by: external-binary
- Status: reported

## 9. Coord-served policy / prompt documents (`coord_prompt_documents`)

configuration served by coord rather than held locally — fetched over the network, so its content is a function of when it was fetched

- Resolved by: `prompt_library::cache_health`
- Owned by: bin
- Status: reported

## 10. Fleet policy dial (TIME-VARYING) (`fleet_policy_dial`)

three process-global caches refreshed by one supervised poll loop — this layer's value can change with NO restart of anything, which is why every reading in this report carries a capture time

- Resolved by: `mcp::fleet_policy_poller::dial_snapshot`
- Owned by: bin
- Status: reported

## 11. Per-account CLAUDE_CONFIG_DIR selection (`claude_config_dir`)

which Claude account config directory a session resolves to — the per-account selection that decides whose credentials and settings a spawned session uses

- Resolved by: `ai_provider::config::get_effective_config_dir`
- Owned by: bin
- Status: reported

## 12. `--settings` carrier the runner materializes (`claude_settings_carrier`)

the settings file the runner writes to disk and hands to Claude Code on the command line — configuration that reaches the session by argv, not by env or by file discovery

- Resolved by: `session::claude_hook::settings_path`
- Owned by: bin
- Status: reported

## 13. `--mcp-config` carrier (`mcp_config_carrier`)

the MCP config argv the identity shim passes through — built in a SEPARATE binary, so what the session received is not observable from the runner's own state

- Resolved by: `bin/qontinui_shim::identity_mcp_config_args`
- Owned by: external-binary
- Status: reported

## 14. `.mcp.json` (`mcp_json`)

the on-disk MCP server manifest the runner writes (coord-mcp port, proxy nonce, bearer) — shared state that an ephemeral runner can rewrite underneath a live session

- Resolved by: `coord_mcp::mcp_json_report`
- Owned by: bin
- Status: reported

## 15. OS keyring / SecureStorage — WITHHELD (`secure_storage_keyring`)

the credential store itself (OS keychain or the encrypted on-disk slot under `QONTINUI_SECURE_STORAGE_DIR`). It is in this inventory so the report DECLARES that it was considered; it is `Withheld` so it can never carry a value into the renderer

- Resolved by: `secure_storage::SecureStorage`
- Owned by: lib
- Status: reported

---

A layer marked `UNKNOWN` in the report could **not be read** at the point the report was taken. It is never a default value. A layer marked `WITHHELD` holds credentials and is never printed.
