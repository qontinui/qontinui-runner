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

pub(crate) fn coord_http_base() -> Result<String, String> {
    // Source-of-truth chain: env `COORD_HTTP_URL` → profile `coord_url`
    // (ws→http) → default `http://localhost:9870`. Delegates to the shared
    // resolver; the dev-localhost guess is now logged once per process. This
    // resolver never errored before (it always fell back to localhost-Ok), so
    // the `Result` contract is preserved by always returning Ok.
    Ok(qontinui_runner_lib::profiles::coord_base_or_dev_localhost()
        .unwrap_or_else(|| "http://localhost:9870".to_string()))
}
