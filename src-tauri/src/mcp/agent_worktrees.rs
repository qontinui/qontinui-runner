//! Coord URL resolution for the worktree-per-agent spawn path.
//!
//! The worktree-allocation flow itself lives in
//! [`crate::agent_worktree`] and is driven in-process by
//! [`crate::agent_worktree::isolated_edit`] (the terminal-spawn /
//! slash-command path). This module now retains only the shared
//! coord-base resolver that several call sites depend on
//! (`file_registry`, `terminal::coord_warn`, `commands::productivity`,
//! `commands::claims`, `fleet`, …).
//!
//! Historical note: a `POST /agents/allocate-local` HTTP endpoint used
//! to live here as a runner-side wrapper over coord's `/agents/allocate`.
//! It never acquired a caller — the in-process `isolated_edit` facade
//! superseded it — so it was removed along with the unused no-claim
//! `allocate_and_materialize` wrapper.

use crate::agent_worktree::coord_ws_to_http;

pub(crate) fn coord_http_base() -> Result<String, String> {
    // Source-of-truth chain: env `COORD_HTTP_URL` → profile `coord_url`
    // (ws→http) → default `http://localhost:9870`.
    if let Ok(v) = std::env::var("COORD_HTTP_URL") {
        if !v.is_empty() {
            return Ok(v);
        }
    }
    let profile = qontinui_runner_lib::profiles::load();
    if let Some(ws) = profile.coord_url.as_deref() {
        return Ok(coord_ws_to_http(ws));
    }
    Ok("http://localhost:9870".to_string())
}
