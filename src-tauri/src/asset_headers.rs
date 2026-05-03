//! Tauri webview cache hardening (Phase P2.2 of `tmp_plans/sw-cache-invalidation.md`).
//!
//! Tauri's bundled asset protocol (the `tauri://` / `http://tauri.localhost/`
//! scheme that serves the embedded `dist/` frontend in `--features
//! custom-protocol` builds) does not expose a `Cache-Control` knob via
//! `tauri.conf.json`. WebView2 will therefore cache the `index.html`
//! response across runner restarts, which means a webview that survives a
//! binary swap (e.g. `POST /runners/spawn-test {rebuild: true}` over an
//! already-running test runner) can serve stale shell HTML — and the stale
//! HTML references stale `<script src="/assets/index-<oldhash>.js">` paths
//! that no longer exist in the new bundle.
//!
//! Tauri 2.10 *does* expose
//! [`WebviewWindowBuilder::on_web_resource_request`], a per-request
//! response-mutation hook that fires AFTER the bundled tauri-protocol
//! handler has produced its `http::Response`. We use it to stamp
//! `Cache-Control: no-store` on the index document only — hashed assets
//! under `/assets/*` keep their default (cacheable) headers because a new
//! build emits new filenames, so cross-build cache poisoning isn't
//! possible there.
//!
//! See `tmp_plans/sw-cache-invalidation.md` § "P2.2 Tauri webview cache
//! hardening" for the full rationale and the binary-swap-mid-session
//! scenario this protects against. The build-id watcher (`P1.3`) catches
//! the same case at the JS layer; this is belt-and-suspenders for the case
//! where WebView2 won't even re-fetch the document because it thinks its
//! cached copy is still fresh.

use std::borrow::Cow;

use tauri::http::{
    header::{HeaderValue, CACHE_CONTROL, PRAGMA},
    Request, Response,
};

/// Returns true for the runner shell's index document — the only response
/// we need to mark `no-store`. Vite emits content-hashed filenames under
/// `assets/`, so those are safe to leave cacheable. Anything else served
/// by the tauri-protocol (favicon, locale json, etc.) is also non-hashed
/// but currently absent from this runner's `dist/` — we keep the predicate
/// tight to avoid over-invalidating if more assets are added later.
fn is_index_document(request: &Request<Vec<u8>>) -> bool {
    if request.uri().scheme_str() != Some("tauri") && request.uri().scheme_str() != Some("http") {
        // Only target the runner's own embedded scheme. On Windows Tauri
        // rewrites `tauri://` to `http://tauri.localhost/...`; on Linux the
        // raw `tauri` scheme reaches the handler. Both paths land here.
        return false;
    }

    let path = request.uri().path();

    // "/" → SPA root navigation. "/index.html" → explicit fetch.
    // "" can occur for opaque origin probes; treat as root.
    matches!(path, "" | "/" | "/index.html")
}

/// Mutate a webview response to stop WebView2 / WKWebView / WebKitGTK from
/// caching the index document. Hashed `assets/*` responses pass through
/// unchanged.
///
/// Wire into the `WebviewWindowBuilder` chain via
/// `.on_web_resource_request(stamp_no_store_on_index)`.
pub fn stamp_no_store_on_index(
    request: Request<Vec<u8>>,
    response: &mut Response<Cow<'static, [u8]>>,
) {
    if !is_index_document(&request) {
        return;
    }

    // `no-store` on its own would suffice for modern engines; we add the
    // legacy `must-revalidate` + `Pragma: no-cache` for defense against
    // any intermediary that downgrades semantics. Matches the supervisor
    // dashboard's index headers (`routes/dashboard.rs`).
    let value = HeaderValue::from_static("no-store, no-cache, must-revalidate");
    response.headers_mut().insert(CACHE_CONTROL, value);
    response
        .headers_mut()
        .insert(PRAGMA, HeaderValue::from_static("no-cache"));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_request(uri: &str) -> Request<Vec<u8>> {
        Request::builder()
            .uri(uri)
            .body(Vec::new())
            .expect("valid request")
    }

    fn make_response() -> Response<Cow<'static, [u8]>> {
        Response::builder()
            .header(CACHE_CONTROL, "max-age=31536000")
            .body(Cow::Borrowed(&b"<!doctype html>"[..]))
            .expect("valid response")
    }

    #[test]
    fn stamps_root_navigation() {
        let mut resp = make_response();
        stamp_no_store_on_index(make_request("tauri://localhost/"), &mut resp);
        assert_eq!(
            resp.headers().get(CACHE_CONTROL).unwrap(),
            "no-store, no-cache, must-revalidate"
        );
        assert_eq!(resp.headers().get(PRAGMA).unwrap(), "no-cache");
    }

    #[test]
    fn stamps_explicit_index_html() {
        let mut resp = make_response();
        stamp_no_store_on_index(make_request("http://tauri.localhost/index.html"), &mut resp);
        assert_eq!(
            resp.headers().get(CACHE_CONTROL).unwrap(),
            "no-store, no-cache, must-revalidate"
        );
    }

    #[test]
    fn leaves_hashed_assets_untouched() {
        let mut resp = make_response();
        stamp_no_store_on_index(
            make_request("http://tauri.localhost/assets/index-abc123.js"),
            &mut resp,
        );
        assert_eq!(
            resp.headers().get(CACHE_CONTROL).unwrap(),
            "max-age=31536000"
        );
        assert!(resp.headers().get(PRAGMA).is_none());
    }

    #[test]
    fn ignores_non_tauri_schemes() {
        let mut resp = make_response();
        stamp_no_store_on_index(
            make_request("https://api.anthropic.com/v1/messages"),
            &mut resp,
        );
        assert_eq!(
            resp.headers().get(CACHE_CONTROL).unwrap(),
            "max-age=31536000"
        );
    }
}
