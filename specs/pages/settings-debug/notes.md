# SettingsDebugPage

Re-authored under the canonical `settings-debug` page id (the prior spec was
retired in the 2026-06-04 page-spec gap cleanup for using a wrong page id). This
is the shared Settings arm with the `advanced` sub-tab active (the
`settings-debug` tab maps to Settings → "advanced"), so `metadata.component` is
`Settings`.

Authored against a live snapshot and validated via `POST /spec-check` on a temp
runner built from main `8c2fc7e8`: `full_match`, overallMatchRate 1.0, 5 states,
15 assertions, 1 read-only transition (open the Debug sub-tab), zero
`no_candidates` misses.

All criteria are ID-first against stable UI Bridge ids (the settings sub-tab
rail, the Advanced/Debug section headings, Save Debug Settings, the debug
option labels, the Device Information block + Refresh device info, and the
Experimental section).
