//! Runner-as-CI-node executor (plan `2026-07-15-runner-as-ci-node-migration`,
//! Phase 2).
//!
//! Coord dispatches candidate builds to connected runners over the SAME
//! Redis-fanout `/ws` socket family the agent runtime consumes:
//!
//! - inbound `events.ci.build_requested.<device_id>` — payload
//!   `{dispatch_id, repo, head_sha, fetch_url, candidate_ref, manifest_path,
//!   check_name, coord_http_url}`
//! - inbound `events.ci.build_cancelled.<device_id>` — payload `{dispatch_id}`
//! - inbound `events.ci.settings_requested.<device_id>` — the owner's
//!   CI-node configuration from qontinui-web, published by coord's
//!   `POST /devenv/ci-node-dispatch` and applied by [`settings_directive`]
//!   (which re-validates every field; coord is not this machine's security
//!   boundary)
//! - outbound REST, **device-JWT authenticated** (coord mounts the ingest
//!   routes behind `require_jwt` and matches the JWT's `device_id`/
//!   `tenant_id` claims against the dispatch row's assignee):
//!   `POST {coord}/coord/ci/dispatches/{id}/progress` (batched log lines +
//!   monotonic `progress_seq`) and `POST .../result` (`conclusion` +
//!   per-step `summary` + ~32 KB `log_tail` + the optional `test_results`
//!   JUnit artifact).
//!
//! That device-JWT binding is why the `test_results` artifact rides the
//! RESULT route rather than coord's `/coord/test-results/ingest`: the latter
//! is gated on `COORD_INGEST_TOKEN`, a fleet secret, and this lane runs on
//! customers' machines. The result route proves WHICH device is reporting for
//! WHICH dispatch, and coord already knows that dispatch's repo, sha and
//! tenant — so the artifact carries no attribution at all and a runner cannot
//! spoof another tenant's merge-gate inputs. See `qontinui_ci_exec::junit`.
//!
//! The executor itself — checkout, the `.qontinui/ci.toml` manifest, tool,
//! sibling and service provisioning, the steps, the JUnit capture — is the
//! shared `qontinui-ci-exec` crate (qontinui-schemas `ci-exec`), the same one
//! the CI host agent and the standalone `qontinui-ci` CLI run. This module is
//! the runner as a HOST of it: the coord subscription, admission, reporting,
//! the owner's settings directive, and the runner's implementations of the
//! crate's host traits ([`host`]).
//!
//! The executed commands come EXCLUSIVELY from the repo's own
//! `.qontinui/ci.toml` at the dispatched SHA (coord supplies no commands —
//! plan §4.1), gated by the local per-repo allowlist in
//! [`crate::settings::CiNodeSettings`]. Everything is off by default; with
//! `ci_node.enabled = false` this module admits nothing (it still listens,
//! so the owner can turn it on from the web — see [`settings_directive`]).
//!
//! Deliberately NOT on the local `:9876` surface: CI is driven only from the
//! coord WS (plan §7.6 — no new capability on the unauthenticated loopback
//! surface).

pub(crate) mod admission;
pub(crate) mod host;
pub(crate) mod reporting;
pub(crate) mod settings_directive;
pub(crate) mod subscription;

use tracing::info;

/// Spawn the CI-node runtime. Mirrors `agent_runtime::spawn_runtime`:
/// no device identity or coord base ⇒ inert.
///
/// The subscription is held regardless of `ci_node.enabled` — the setting
/// gates ADMISSION, not delivery, because `settings_requested` is how the
/// opt-in gets flipped from qontinui-web and a disabled device holding no
/// socket could never receive it. See `subscription`'s module doc.
pub fn spawn_ci_node_runtime() {
    let Some(device_id) = crate::agent_runtime::load_local_device_id() else {
        info!(
            "ci_node: ~/.qontinui/machine.json missing or device_id unparseable — \
             CI-node runtime disabled. Skipping."
        );
        return;
    };
    if qontinui_runner_lib::profiles::connected_coord_base().is_none() {
        info!("ci_node: runner is ISOLATED (no coord configured, not a hosted tier) — CI-node runtime disabled. Skipping.");
        return;
    }
    info!("ci_node: starting for device_id={device_id}");
    tokio::spawn(async move {
        subscription::subscribe_loop(device_id).await;
    });
}

/// Cancel every in-flight CI build (app shutdown seam in `main.rs`). The
/// Windows Job Object (kill-on-close) is the hard backstop for the child
/// process trees; this token cancel lets executors attempt a best-effort
/// `cancelled` result POST before the process dies. Coord's dispatch-lease
/// sweeper covers whatever doesn't make it out.
pub fn shutdown_all() {
    admission::cancel_all_for_shutdown();
}

#[cfg(test)]
mod tests {
    use qontinui_ci_exec::manifest::{parse_and_validate, SiblingPin, CANONICAL_TOOLCHAINS};
    use qontinui_ci_exec::sibling::{lookup_pin, SIBLING_PIN_FILE};

    /// The executor's closed set of `[canonical]` toolchains is exactly the set
    /// `env_agent` has a version-manager cascade for — the runner is the host
    /// that converges them, so a key on one side only would validate and then
    /// be unsatisfiable (or be convergeable and never declarable).
    #[test]
    fn canonical_toolchains_are_exactly_what_env_agent_converges() {
        let appliable: Vec<&str> = qontinui_runner_lib::env_agent::apply_versions::APPLIABLE_TOOLS
            .iter()
            .map(|t| t.key())
            .collect();
        assert_eq!(CANONICAL_TOOLCHAINS, appliable.as_slice());
    }

    /// Lane parity on this repo's REAL files, so the audit the manifests ask
    /// for in prose ("when ci.yml's clone list changes, change this list in
    /// the same PR") fails `cargo test` instead of waiting for an incident:
    ///
    /// * `.qontinui/ci.toml` parses and validates as shipped;
    /// * every `[[siblings]]` entry that says `pin-file` is listed in
    ///   `.github/sibling-pins.conf` with a usable SHA, and no entry of any
    ///   kind is listed there UNUSABLY (a half-finished bump on `main` would
    ///   red the Actions lane too, so it is caught here first);
    /// * every repo the pin file lists is a declared sibling here — a pin for
    ///   a repo this lane never checks out is a pin nothing reads, i.e. the
    ///   two lanes' sibling lists have drifted;
    /// * every repo the pin file lists that this lane does NOT read the pin
    ///   for (a listed sibling on `declared-adaptation` / `default-branch`)
    ///   is named in [`KNOWN_DIVERGENT`] — the ci.toml block that records
    ///   the divergence is prose, and this is what makes an UNRECORDED one
    ///   red: a sibling the Actions lane pins and this lane floats, with no
    ///   line here admitting it. When the two entries flip to `pin-file`
    ///   the list empties, and it must, because an entry left in it that
    ///   is no longer divergent is refused too.
    ///
    /// Read against the checked-in bytes (`include_str!`), never a copy:
    /// a copy is the second source of truth this whole mechanism exists to
    /// avoid.
    #[test]
    fn this_repo_manifests_agree_on_which_siblings_are_pinned() {
        /// Siblings the pin file lists that `.qontinui/ci.toml` deliberately
        /// does NOT read the pin for yet — its DECLARED DIVERGENCE block says
        /// why (coord's allocate-lane reader must accept `pin-file` first).
        /// Deleting the divergence deletes the entry here, in the same PR.
        const KNOWN_DIVERGENT: &[&str] = &[
            "qontinui/qontinui-schemas",
            "qontinui/qontinui-web",
            "qontinui/ui-bridge",
        ];

        let ci_toml = include_str!("../../../.qontinui/ci.toml");
        let pin_file = include_str!("../../../.github/sibling-pins.conf");
        let manifest = parse_and_validate(ci_toml)
            .expect("this repo's own .qontinui/ci.toml must parse and validate");
        assert!(
            !manifest.siblings.is_empty(),
            "this repo declares siblings; an empty list means the wrong file was read"
        );
        for s in &manifest.siblings {
            let pinned = lookup_pin(pin_file, &s.repo).unwrap_or_else(|e| {
                panic!("{SIBLING_PIN_FILE} entry for {} is unusable: {e}", s.repo)
            });
            let reads_pin = s.pin == SiblingPin::PinFile;
            let admitted = KNOWN_DIVERGENT.contains(&s.repo.as_str());
            match (reads_pin, pinned.is_some(), admitted) {
                (true, false, _) => panic!(
                    "{} says pin = \"pin-file\" in .qontinui/ci.toml but {SIBLING_PIN_FILE} does \
                     not list it — every dispatch would hard-fail on this sibling",
                    s.repo
                ),
                (true, true, true) => panic!(
                    "{} reads its pin now; drop it from KNOWN_DIVERGENT — the divergence it \
                     admits no longer exists",
                    s.repo
                ),
                (false, true, false) => panic!(
                    "{SIBLING_PIN_FILE} pins {} but .qontinui/ci.toml resolves it by {:?}, and \
                     nothing admits the divergence: the Actions lane compiles against the pinned \
                     commit while this lane floats. Either set pin = \"pin-file\" or record why \
                     not in ci.toml's DECLARED DIVERGENCE block AND in KNOWN_DIVERGENT here",
                    s.repo, s.pin
                ),
                (false, false, true) => panic!(
                    "{} is in KNOWN_DIVERGENT but {SIBLING_PIN_FILE} does not list it — there is \
                     no pin to diverge from; drop the entry",
                    s.repo
                ),
                (true, true, false) | (false, true, true) | (false, false, false) => {}
            }
        }
        let declared: Vec<&str> = manifest.siblings.iter().map(|s| s.repo.as_str()).collect();
        let listed: Vec<&str> = pin_file
            .lines()
            .map(|l| l.split_once('#').map_or(l, |(code, _)| code))
            .filter_map(|l| l.split_whitespace().next())
            .collect();
        assert!(
            !listed.is_empty(),
            "{SIBLING_PIN_FILE} lists no repos; an empty list means the wrong file was read"
        );
        for repo in listed {
            assert!(
                declared.contains(&repo),
                "{SIBLING_PIN_FILE} pins {repo}, which .qontinui/ci.toml does not declare as a \
                 sibling — the two lanes' sibling lists have drifted"
            );
        }
    }
}
