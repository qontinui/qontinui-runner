#![cfg(test)]

use uuid::Uuid;

/// A device JWT claiming `tenant`, unexpired for the life of the test
/// process (the slot selector refuses an expired one). One string per
/// tenant per process, so "the request presented the token we stored" is
/// a real comparison.
pub(crate) fn device_jwt(tenant: Uuid) -> String {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    static EXP: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    let exp = *EXP.get_or_init(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock is after the unix epoch")
            .as_secs()
            + 3_600
    });
    let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"none","typ":"JWT"}"#);
    let payload = URL_SAFE_NO_PAD.encode(
        serde_json::json!({"sub_type": "device", "tenant_id": tenant, "exp": exp}).to_string(),
    );
    format!("{header}.{payload}.sig")
}

/// A runner paired for `a` (the default binding: the legacy slot, and by
/// the mirror its own slot) that also holds `b`'s own slot — inside an
/// isolated ambient, so every credential read lands in a throwaway store and
/// never the OS keychain. Hold the returned guard for the test's life; it
/// also holds the process-wide env lock.
pub(crate) fn two_tenant_runner(
    a: Uuid,
    b: Uuid,
) -> qontinui_runner_lib::ambient::test_support::IsolatedAmbient {
    let ambient = isolated_store();
    let am = crate::auth::AuthManager::new();
    am.store_tokens(&device_jwt(a), "").expect("store A");
    am.store_tenant_device_jwt(&b, &device_jwt(b))
        .expect("store B");
    ambient
}

/// An EMPTY credential store inside an isolated ambient, with the OS keychain
/// disabled — what a "no usable credential" test must run in, so it can never
/// read (or be satisfied by) the real store. Hold the guard for the test's life;
/// it also holds the process-wide env lock.
pub(crate) fn isolated_store() -> qontinui_runner_lib::ambient::test_support::IsolatedAmbient {
    let ambient = crate::test_env::isolated_ambient();
    std::env::set_var("QONTINUI_DISABLE_KEYCHAIN", "1");
    ambient
}
