//! Where vision-capture bytes live, and how a caller reaches them.
//!
//! Plan `2026-10-22-runner-ui-bridge-makes-an-agent-guess-its-own-vocabulary`
//! Phase 3 (D2). Two decisions live here so `vision_routes.rs` only calls them:
//!
//! 1. **The cache root.** `<data_local_dir>/com.qontinui.runner/vision_cache`,
//!    resolved through the instance seam
//!    ([`qontinui_runner_lib::instance_env::runner_data_local_base`] and
//!    [`crate::instance::scope_path`]). It used to be
//!    `current_runner_path().join("tmp_vision_cache")` — inside the runner's
//!    own SOURCE CHECKOUT, and on an installed runner (which has no checkout)
//!    the process CWD. `scope_path` is required, not optional: cache files are
//!    named by the hash of a request key that folds in a per-process mutation
//!    counter starting at 0, and the GET route serves by file name, so two
//!    runners sharing one unscoped directory would overwrite and cross-serve
//!    each other's captures. An unresolvable base FAILS; there is no CWD
//!    fallback.
//! 2. **How a caller gets the bytes.** Every `CaptureResponse` carries
//!    [`cache_url`] — the relative HTTP path `GET /ui-bridge/vision/cache/<sha>`
//!    that works through every door (loopback, relay, remote). A request with
//!    `inline: true` also gets the bytes as `base64` in the same response, up
//!    to [`INLINE_MAX_BYTES`]; over the cap it gets a typed
//!    [`INLINE_TOO_LARGE`] error naming the `url`, never a truncated string.
//!
//! **`sha256` is the hash of the cache KEY, not of the image bytes.** The key
//! is `v=1|mut=<mutation_id>|req=<request>` (see
//! `vision_routes::compose_capture_cache_key`); two identical images captured
//! under different keys have different `sha256` values. Do not treat it as a
//! content hash.

use std::path::{Path, PathBuf};

use axum::{http::StatusCode, Json};
use base64::Engine as _;

use crate::mcp::types::ApiResponse;

/// Leaf directory under the runner's DataLocal base that holds the cache.
pub const VISION_CACHE_DIR_NAME: &str = "vision_cache";

/// Largest capture (encoded bytes) a response will carry inline as `base64`.
/// 5 MiB — the Claude vision input ceiling the default `claude_vision_v1`
/// contract keeps every capture under (`scripts/contract-smoke.ps1` asserts
/// `bytes < 5 MiB`). Larger captures (`png_strict`, `raw`) are fetched by
/// [`cache_url`].
pub const INLINE_MAX_BYTES: usize = 5 * 1024 * 1024;

/// `code` of the 413 a request gets when it asks for `inline: true` on a
/// capture larger than [`INLINE_MAX_BYTES`]. The capture itself succeeded and
/// is cached; the error's `hint` carries `{url, bytes, maxBytes}`.
pub const INLINE_TOO_LARGE: &str = "INLINE_TOO_LARGE";

/// The cache root: `<data_local_base>/vision_cache`, scoped. `mcp_api`'s
/// `ApiState` init calls it with
/// `qontinui_runner_lib::instance_env::runner_data_local_base()` (already
/// re-rooted under an instance root when there is one) and
/// `crate::instance::scope_path` (which appends the `instance-<name>` subdir
/// for a secondary runner outside a root); tests inject both.
///
/// `None` is an error, loudly: falling back to the process CWD is exactly the
/// defect this replaced.
pub fn vision_cache_root_from(
    data_local_base: Option<PathBuf>,
    scope: impl FnOnce(&Path) -> PathBuf,
) -> Result<PathBuf, String> {
    let base = data_local_base.ok_or_else(|| {
        "vision cache root is unresolvable: runner_data_local_base() returned None \
         (no QONTINUI_INSTANCE_ROOT, and the platform reports no data-local directory \
         via dirs::data_local_dir()). Refusing to fall back to the process CWD or the \
         source checkout."
            .to_string()
    })?;
    // Scope FIRST, then join: `<base>/instance-<name>/vision_cache` sits inside
    // the per-instance directory the supervisor's app-data cleanup removes
    // when a temp runner is deleted, so a spawn-test runner's captures do not
    // outlive it.
    Ok(scope(&base).join(VISION_CACHE_DIR_NAME))
}

/// Relative HTTP path that serves a cache entry's bytes:
/// `/ui-bridge/vision/cache/<sha>`. `sha` is the cache-KEY hash (see the
/// module doc), exactly as `CaptureResponse.sha256` carries it.
pub fn cache_url(sha: &str) -> String {
    format!("/ui-bridge/vision/cache/{sha}")
}

/// A capture asked for `inline: true` but is larger than the inline cap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineTooLarge {
    /// Where the (already cached) bytes can be fetched instead.
    pub url: String,
    /// Encoded size of the capture.
    pub bytes: usize,
    /// The cap it exceeded.
    pub max_bytes: usize,
}

impl InlineTooLarge {
    /// The 413 envelope: `code: INLINE_TOO_LARGE`, with `url`, `bytes` and
    /// `maxBytes` in `hint` so a caller can follow `url` without parsing prose.
    pub fn into_api_error(self) -> (StatusCode, Json<ApiResponse<()>>) {
        let mut resp = ApiResponse::<()>::error_with_code(
            format!(
                "capture is {} bytes, over the {}-byte inline cap; \
                 fetch it with GET {} instead of inline: true",
                self.bytes, self.max_bytes, self.url
            ),
            INLINE_TOO_LARGE,
        );
        resp.hint = Some(serde_json::json!({
            "url": self.url,
            "bytes": self.bytes,
            "maxBytes": self.max_bytes,
        }));
        (StatusCode::PAYLOAD_TOO_LARGE, Json(resp))
    }
}

/// The two response fields that tell a caller where the bytes are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureAccess {
    /// [`cache_url`] of the entry.
    pub url: String,
    /// Standard base64 of the bytes, only when the request asked for it.
    pub base64: Option<String>,
}

/// Build `url` (always) and `base64` (when `inline`) for a cache entry whose
/// encoded bytes are `bytes`. Over [`INLINE_MAX_BYTES`] with `inline` set this
/// is [`InlineTooLarge`].
pub fn capture_access(
    sha: &str,
    bytes: &[u8],
    inline: bool,
) -> Result<CaptureAccess, InlineTooLarge> {
    capture_access_capped(sha, bytes, inline, INLINE_MAX_BYTES)
}

fn capture_access_capped(
    sha: &str,
    bytes: &[u8],
    inline: bool,
    max_bytes: usize,
) -> Result<CaptureAccess, InlineTooLarge> {
    let url = cache_url(sha);
    if !inline {
        return Ok(CaptureAccess { url, base64: None });
    }
    if bytes.len() > max_bytes {
        return Err(InlineTooLarge {
            url,
            bytes: bytes.len(),
            max_bytes,
        });
    }
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    Ok(CaptureAccess {
        url,
        base64: Some(encoded),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::ui_bridge::vision_routes::{find_cache_file, CaptureResponse};
    use qontinui_runner_lib::instance_env::{platform_dir_from, PlatformDir, RUNNER_BUNDLE_ID};

    /// The DataLocal base exactly as `runner_data_local_base()` builds it, for
    /// an injected instance root / machine dir.
    fn data_local_base(root: Option<&Path>, machine: &Path) -> Option<PathBuf> {
        let machine = machine.to_path_buf();
        platform_dir_from(PlatformDir::DataLocal, root, move || Some(machine))
            .map(|d| d.join(RUNNER_BUNDLE_ID))
    }

    #[test]
    fn vision_cache_root_resolves_under_the_instance_root() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("subject");
        let machine = tmp.path().join("machine-localappdata");

        let base = data_local_base(Some(&root), &machine);
        let cache = vision_cache_root_from(base, |b| {
            crate::instance::scope_path_for(b, Some(&root), || {
                panic!("a base already inside the instance root must not be re-scoped")
            })
        })
        .expect("resolvable");

        assert_eq!(
            cache,
            root.join("data")
                .join("local")
                .join(RUNNER_BUNDLE_ID)
                .join("vision_cache")
        );
        assert!(cache.starts_with(&root), "cache {cache:?} escaped the root");
        assert!(!cache.starts_with(&machine));
        let cwd = std::env::current_dir().expect("cwd");
        assert!(!cache.starts_with(&cwd), "cache {cache:?} is under the CWD");
        assert!(!cache.to_string_lossy().contains("tmp_vision_cache"));
    }

    #[test]
    fn two_instance_names_get_two_cache_roots() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let machine = tmp.path().join("machine-localappdata");
        let root_for = |name: &str| {
            let name = name.to_string();
            vision_cache_root_from(data_local_base(None, &machine), move |b| {
                crate::instance::scope_path_for(b, None, || {
                    crate::instance::resolve_data_subdir(Some(&name), None, None, 9876)
                })
            })
            .expect("resolvable")
        };
        let alpha = root_for("alpha");
        let beta = root_for("beta");
        assert_ne!(alpha, beta, "two instances must not share a vision cache");
        let shared = machine.join(RUNNER_BUNDLE_ID);
        assert_eq!(alpha, shared.join("instance-alpha").join("vision_cache"));
        assert_eq!(beta, shared.join("instance-beta").join("vision_cache"));

        // Two ROOTED subjects likewise get two roots.
        let r1 = tmp.path().join("r1");
        let r2 = tmp.path().join("r2");
        let rooted = |r: &Path| {
            vision_cache_root_from(data_local_base(Some(r), &machine), |b| {
                crate::instance::scope_path_for(b, Some(r), || None)
            })
            .expect("resolvable")
        };
        assert_ne!(rooted(&r1), rooted(&r2));
    }

    #[test]
    fn unresolvable_base_fails_loudly_never_cwd() {
        let err = vision_cache_root_from(None, |b| b.to_path_buf())
            .expect_err("a None base must be an error, not a CWD fallback");
        assert!(err.contains("runner_data_local_base"), "{err}");
        assert!(err.contains("CWD"), "{err}");
    }

    #[test]
    fn cache_url_is_the_get_by_sha_route() {
        let sha = "ab".repeat(32);
        assert_eq!(cache_url(&sha), format!("/ui-bridge/vision/cache/{sha}"));
        let routes = crate::mcp::ui_bridge::vision_routes::route_entries();
        assert!(
            routes
                .iter()
                .any(|(m, p)| *m == "GET" && *p == "/ui-bridge/vision/cache/{sha256}"),
            "cache_url must name a served route; route_entries: {routes:?}"
        );
    }

    /// `inline: true` returns base64 of exactly the bytes the GET-by-sha route
    /// serves for the same `url`.
    #[test]
    fn inline_base64_round_trips_to_the_get_by_sha_bytes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cache =
            qontinui_vision_core::VisionCache::new(tmp.path(), 8 * 1024 * 1024).expect("cache");
        let key = qontinui_vision_core::sha256_of(b"v=1|mut=0|req=test");
        let bytes: Vec<u8> = (0..20_000u32).map(|i| (i * 7 % 251) as u8).collect();
        let hit = cache.put(&key, &bytes, "jpeg").expect("put");

        let access = capture_access(&hit.sha256_hex, &bytes, true).expect("under cap");
        let sha_from_url = access
            .url
            .strip_prefix("/ui-bridge/vision/cache/")
            .expect("url names the cache route");
        // What `vision_cache_get_handler` serves for that sha.
        let (path, ext) = find_cache_file(cache.root(), sha_from_url)
            .expect("readdir")
            .expect("entry for the url's sha");
        assert_eq!(ext, "jpeg");
        let served = std::fs::read(path).expect("read");

        let decoded = base64::engine::general_purpose::STANDARD
            .decode(access.base64.expect("inline requested"))
            .expect("valid base64");
        assert_eq!(decoded, served);
        assert_eq!(decoded, bytes);
    }

    #[test]
    fn inline_is_omitted_unless_requested() {
        let access = capture_access("cd", b"abc", false).expect("no cap without inline");
        assert_eq!(access.base64, None);
        // Not-inline never trips the cap, however large.
        let big = vec![0u8; 64];
        assert!(capture_access_capped("cd", &big, false, 8).is_ok());
    }

    #[test]
    fn over_cap_inline_is_inline_too_large_naming_the_url() {
        let sha = "ef".repeat(32);
        let at_cap = vec![1u8; 16];
        assert!(capture_access_capped(&sha, &at_cap, true, 16).is_ok());

        let over = vec![1u8; 17];
        let err = capture_access_capped(&sha, &over, true, 16).expect_err("over cap");
        assert_eq!(
            err,
            InlineTooLarge {
                url: cache_url(&sha),
                bytes: 17,
                max_bytes: 16,
            }
        );

        let (status, Json(body)) = err.into_api_error();
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
        let v = serde_json::to_value(&body).expect("serialize");
        assert_eq!(v["code"], "INLINE_TOO_LARGE");
        assert_eq!(v["success"], false);
        assert_eq!(v["hint"]["url"], cache_url(&sha));
        assert_eq!(v["hint"]["bytes"], 17);
        assert_eq!(v["hint"]["maxBytes"], 16);
        assert!(v.get("data").is_none(), "no truncated payload: {v}");
    }

    #[test]
    fn real_cap_is_five_mib() {
        assert_eq!(INLINE_MAX_BYTES, 5 * 1024 * 1024);
        let over = vec![0u8; INLINE_MAX_BYTES + 1];
        assert!(capture_access("00", &over, true).is_err());
    }

    /// No field of a serialized capture response names the legacy checkout
    /// directory, and the response carries `url` — not `path`.
    #[test]
    fn capture_response_has_url_and_no_tmp_vision_cache() {
        let sha = "12".repeat(32);
        let access = capture_access(&sha, b"\xff\xd8\xff", true).expect("small");
        let resp = CaptureResponse {
            url: access.url,
            base64: access.base64,
            sha256: sha.clone(),
            width: 1,
            height: 1,
            bytes: 3,
            format: "jpeg".to_string(),
            contract: "claude_vision_v1".to_string(),
            capture_backend: None,
        };
        let v = serde_json::to_value(&resp).expect("serialize");
        let obj = v.as_object().expect("object");
        assert!(!obj.contains_key("path"), "path must be gone: {v}");
        assert!(!obj.contains_key("relativeCachePath"), "{v}");
        assert_eq!(obj["url"], format!("/ui-bridge/vision/cache/{sha}"));
        assert_eq!(obj["base64"], "/9j/");
        let text = v.to_string();
        assert!(!text.contains("tmp_vision_cache"), "{text}");

        // base64 is omitted, not null, when not requested.
        let plain = CaptureResponse {
            base64: None,
            ..resp
        };
        let v = serde_json::to_value(&plain).expect("serialize");
        assert!(
            !v.as_object().expect("object").contains_key("base64"),
            "{v}"
        );
    }
}
