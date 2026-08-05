//! The project **Preview window** — plan `2026-07-24-runner-projects-dashboard`
//! §7.1 step 3 ("show it").
//!
//! A second Tauri webview window pointed at the project's `front_page_url`.
//! §7.1 resolved this against the two alternatives on 2026-07-28: an `<iframe>`
//! in the Projects tab is blocked by the app CSP (`tauri.conf.json` declares no
//! `frame-src`, so `default-src 'self'` governs frames), and an embedded child
//! webview needs the `tauri/unstable` feature this build does not enable. A
//! separate window works today, has in-tree precedent (`click_overlay.rs` builds
//! one with `WebviewUrl::External`), and is not governed by the main window's
//! CSP.
//!
//! The payoff over "just open a browser": the preview is a real webview, so it
//! stays a **UI Bridge target**. "Show me my site" and "click that button for
//! me" remain the same surface. `tauri-plugin-opener` stays available as the
//! escape hatch, invoked by the frontend directly — it is not wrapped here.
//!
//! One window per project, keyed by [`preview_window_label`]. Re-opening an
//! already-open preview focuses and re-navigates it rather than stacking a
//! second window on the same project.

use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

/// Window label for a project's preview. Deterministic so a second `Open` finds
/// the window the first one built.
///
/// Tauri window labels are restricted to alphanumerics plus `-`, `/`, `:` and
/// `_`; a `SavedProject.id` is a UUID today, but the id is only *usually* a
/// UUID (`discover_projects` and the import path both mint their own), so
/// anything outside that set is replaced rather than trusted. Without this a
/// stray character makes `build()` fail with an opaque label error.
pub fn preview_window_label(project_id: &str) -> String {
    let sanitized: String = project_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    format!("project-preview-{sanitized}")
}

/// Accept only `http`/`https` for a preview window.
///
/// `front_page_url` is user-editable free text that lands in a webview with no
/// app CSP over it, so the scheme is the whole security boundary here:
/// `file://` would turn the Open button into an arbitrary-local-file viewer and
/// `javascript:` into script execution in that window. Neither is a capability
/// the Projects dashboard is meant to hand out, and neither is needed — a
/// project front page is served over HTTP.
///
/// Remote `https` origins are deliberately allowed: a project whose front page
/// is deployed is a legitimate thing to preview, and the operator could reach it
/// from a browser anyway.
pub fn validate_preview_url(url: &str) -> Result<url::Url, String> {
    let parsed = url::Url::parse(url.trim()).map_err(|e| {
        format!("'{url}' is not a valid URL: {e}. Expected something like http://localhost:3000")
    })?;
    match parsed.scheme() {
        "http" | "https" => Ok(parsed),
        other => Err(format!(
            "Preview only opens http:// or https:// addresses — '{other}:' is not allowed. \
             Set the project's front page to something like http://localhost:3000."
        )),
    }
}

/// Open (or focus + re-navigate) the preview window for a project.
///
/// Returns `true` when a new window was created, `false` when an existing one
/// was reused — the caller uses that only for telemetry/prose; both are success.
#[tauri::command]
pub async fn open_project_preview(
    app: AppHandle,
    project_id: String,
    url: String,
    title: Option<String>,
) -> Result<bool, String> {
    let parsed = validate_preview_url(&url)?;
    let label = preview_window_label(&project_id);
    let window_title = title.unwrap_or_else(|| "Preview".to_string());

    // Already open — navigate it to the (possibly corrected) URL and raise it.
    // `set_focus` on a minimised window also restores it, which is what an
    // operator pressing Open a second time means by it.
    if let Some(existing) = app.get_webview_window(&label) {
        existing
            .navigate(parsed.clone())
            .map_err(|e| format!("Failed to navigate the preview window: {e}"))?;
        let _ = existing.unminimize();
        existing
            .set_focus()
            .map_err(|e| format!("Failed to focus the preview window: {e}"))?;
        return Ok(false);
    }

    WebviewWindowBuilder::new(&app, &label, WebviewUrl::External(parsed))
        .title(window_title)
        .inner_size(1280.0, 900.0)
        .resizable(true)
        .build()
        .map(|_| true)
        .map_err(|e| format!("Failed to open the preview window: {e}"))
}

/// Close a project's preview window. `Ok(false)` when there was none open —
/// closing an absent window is not an error, so the caller can offer "Close
/// preview" unconditionally.
#[tauri::command]
pub async fn close_project_preview(app: AppHandle, project_id: String) -> Result<bool, String> {
    let label = preview_window_label(&project_id);
    match app.get_webview_window(&label) {
        Some(w) => {
            w.close()
                .map_err(|e| format!("Failed to close the preview window: {e}"))?;
            Ok(true)
        }
        None => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_is_stable_and_scoped_per_project() {
        assert_eq!(
            preview_window_label("6f1c2f0e-9a4b-4c3d-8e1f-0a2b3c4d5e6f"),
            "project-preview-6f1c2f0e-9a4b-4c3d-8e1f-0a2b3c4d5e6f"
        );
        assert_ne!(preview_window_label("a"), preview_window_label("b"));
    }

    #[test]
    fn label_sanitizes_anything_tauri_would_reject() {
        // Path separators and spaces are the realistic cases — an id minted
        // from a directory name rather than a UUID.
        assert_eq!(
            preview_window_label("C:\\my projects\\pizzeria"),
            "project-preview-C--my-projects-pizzeria"
        );
        assert!(preview_window_label("héllo wörld")
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-'));
    }

    #[test]
    fn http_and_https_are_accepted() {
        assert!(validate_preview_url("http://localhost:3000").is_ok());
        assert!(validate_preview_url("http://127.0.0.1:8080/menu").is_ok());
        assert!(validate_preview_url("https://pizzeria.example.com").is_ok());
        // Surrounding whitespace is a paste artefact, not a different URL.
        assert!(validate_preview_url("  http://localhost:3000  ").is_ok());
    }

    #[test]
    fn local_file_and_script_schemes_are_refused() {
        for bad in [
            "file:///C:/Windows/System32/drivers/etc/hosts",
            "javascript:alert(1)",
            "data:text/html,<script>alert(1)</script>",
            "tauri://localhost",
        ] {
            let err = validate_preview_url(bad).unwrap_err();
            assert!(
                err.contains("http:// or https://"),
                "{bad} must be refused by scheme, got: {err}"
            );
        }
    }

    #[test]
    fn unparseable_input_names_the_shape_it_wanted() {
        // The common operator mistake: a bare host:port with no scheme.
        let err = validate_preview_url("localhost:3000/menu").unwrap_err();
        assert!(
            err.contains("http://localhost:3000") || err.contains("not allowed"),
            "error must show the expected shape, got: {err}"
        );
        assert!(validate_preview_url("").is_err());
    }
}
