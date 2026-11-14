//! [`super::release_workdir_proxy_nonces`] revokes EVERY credential bound to a
//! workdir, immediately: one test per nonce class (device, ephemeral, agent)
//! plus a graced (superseded) device key. Edge `4253717f` of plan
//! `2026-11-13-spawn-refused-tree-followups`: a never-spawned tree's removal
//! used the grace-preserving eviction, which kept a device key live for the
//! grace window with no re-provision ever possible behind it.
//!
//! The device, ephemeral and agent tests lock in behaviour that already
//! existed: the session-close release always revoked every LIVE binding for a
//! workdir. Only the graced test here (and
//! `agent_worktree::remove_unspawned`'s
//! `a_removed_trees_proxy_nonces_are_revoked_with_no_grace`) fails on the old
//! code — the first because graced keys were left to expire, the second
//! because removal used the grace-preserving eviction.

use super::*;

/// A workdir no other test binds, so the process-global registry is ours.
fn fresh_workdir(tag: &str) -> String {
    format!("D:/release-wd-{tag}-{}", Uuid::now_v7())
}

/// Neither live nor graced, tombstoned as `Revoked`, and refused by the
/// proxy's own lookups.
fn assert_revoked_totally(nonce: &str) {
    let snap = NonceRegistry::global().test_snapshot();
    assert!(
        !snap.live_contains(nonce),
        "the nonce must leave the live map"
    );
    assert!(
        !snap.graced_contains(nonce),
        "the nonce must not survive in the grace map"
    );
    assert!(
        matches!(
            NonceRegistry::global()
                .test_tombstones()
                .get(nonce)
                .map(|t| t.kind),
            Some(TombstoneKind::Revoked)
        ),
        "the nonce must be tombstoned Revoked"
    );
    assert!(
        !proxy_nonce_is_valid(nonce),
        "the proxy must refuse the revoked nonce"
    );
    assert_eq!(
        proxy_principal_for_nonce(nonce),
        None,
        "the proxy must resolve no principal for the revoked nonce"
    );
}

fn cleanup(nonce: &str) {
    NonceRegistry::global().test_tombstones().remove(nonce);
}

#[test]
fn a_device_nonce_is_revoked_with_no_grace() {
    let _amb = crate::test_env::isolated_ambient();
    let wd = fresh_workdir("device");
    let nonce = register_proxy_nonce(&wd, None, None);
    assert!(
        proxy_nonce_is_valid(&nonce),
        "fixture: the nonce starts live"
    );

    release_workdir_proxy_nonces(&wd, "test: never-spawned tree removed");

    assert_revoked_totally(&nonce);
    cleanup(&nonce);
}

#[test]
fn an_ephemeral_device_nonce_is_revoked() {
    let _amb = crate::test_env::isolated_ambient();
    let wd = fresh_workdir("ephemeral");
    let nonce = register_session_proxy_nonce(&wd, None);
    assert!(
        NonceRegistry::global()
            .test_snapshot()
            .live_contains(&nonce),
        "fixture: the ephemeral nonce starts in the live map"
    );

    release_workdir_proxy_nonces(&wd, "test: never-spawned tree removed");

    assert_revoked_totally(&nonce);
    cleanup(&nonce);
}

#[test]
fn an_agent_nonce_is_revoked() {
    let _amb = crate::test_env::isolated_ambient();
    let wd = fresh_workdir("agent");
    let agent_id = Uuid::now_v7();
    let nonce = register_agent_proxy_nonce(&wd, agent_id);
    assert_eq!(
        proxy_principal_for_nonce(&nonce),
        Some(ProxyPrincipal::Agent { agent_id }),
        "fixture: the agent nonce starts live"
    );

    release_workdir_proxy_nonces(&wd, "test: never-spawned tree removed");

    assert_revoked_totally(&nonce);
    cleanup(&nonce);
}

/// The vet's gap: a superseded device key riding grace for the workdir is
/// revoked in the same call, not left to expire on its own window.
#[test]
fn a_graced_device_nonce_for_the_workdir_is_revoked() {
    let _amb = crate::test_env::isolated_ambient();
    let wd = fresh_workdir("graced");
    let old = register_proxy_nonce(&wd, None, None);
    // The grace-preserving eviction: `old` leaves the live map and rides grace.
    evict_proxy_nonces_for_workdir(&wd);
    {
        let snap = NonceRegistry::global().test_snapshot();
        assert!(!snap.live_contains(&old), "fixture: evicted from live");
        assert!(snap.graced_contains(&old), "fixture: riding grace");
    }
    assert!(
        proxy_nonce_is_valid(&old),
        "fixture: a graced key is still accepted by the proxy"
    );
    // A graced key for ANOTHER workdir must survive the release.
    let other_wd = fresh_workdir("graced-other");
    let other = register_proxy_nonce(&other_wd, None, None);
    evict_proxy_nonces_for_workdir(&other_wd);

    release_workdir_proxy_nonces(&wd, "test: never-spawned tree removed");

    assert_revoked_totally(&old);
    assert!(
        NonceRegistry::global()
            .test_snapshot()
            .graced_contains(&other),
        "a graced key bound to a different workdir must be left alone"
    );
    cleanup(&old);
    NonceRegistry::global().test_graced().remove(&other);
    NonceRegistry::global().test_tombstones().remove(&other);
}
