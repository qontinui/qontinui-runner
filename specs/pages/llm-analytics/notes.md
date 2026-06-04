# LlmObservabilityDashboard

Re-authored under the canonical `llm-analytics` page id (the prior spec was
retired in the 2026-06-04 page-spec gap cleanup for using a wrong page id). This
page is rendered by `LlmObservabilityDashboard` from `../llm-observability`, so
`metadata.component` is `llm-observability` while the page id is `llm-analytics`.

Authored against a live snapshot and validated via `POST /spec-check` on a temp
runner built from main `8c2fc7e8`: `full_match`, overallMatchRate 1.0, 6 states,
16 assertions, zero `no_candidates` misses.

All criteria are ID-first against stable UI Bridge ids (page root, time-range +
refresh toolbar, token-usage empty state, Scripted Output panel, emitter-provider
cascade controls, and the one-shot fallbacks-by-reason table).
