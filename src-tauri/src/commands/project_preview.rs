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
/// Returns `true` when a new window was created, `false` when an existing —
/// and **probed** — one was reused; the caller uses that only for
/// telemetry/prose, and both are success. A reused window that turns out to
/// have no webview is an `Err`, not an `Ok(false)`: see the reuse branch.
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
    //
    // ⚠ "Already open" is only Tauri's word for it, and it is not enough to
    // act on. Tauri inserts a window into its own registry the moment
    // `build()` returns `Ok` (`tauri` 2.11.1 `src/manager/webview.rs`,
    // `attach_webview`) and only removes it on a `Destroyed` event, which can
    // never fire for a window wry never created — so `get_webview_window`
    // finds a HOLLOW registration exactly like a healthy one. Both calls below
    // would then lie: `navigate` and `set_focus` are fire-and-forget
    // `send_user_message` messages whose handler early-returns for a window id
    // absent from wry's map, so both return `Ok` and this command would report
    // `Ok(false)` ("reused an existing one") for doing precisely nothing —
    // every time, for the rest of the process's life. Probe first.
    if let Some(existing) = app.get_webview_window(&label) {
        // On a blocking thread for the same reason the build path below is:
        // the probe is an unbounded event-loop round-trip.
        let probe_window = existing.clone();
        let probe_label = label.clone();
        tauri::async_runtime::spawn_blocking(move || {
            // Counted for the wedge diagnostics' blocking-pool saturation figure.
            // `tauri::async_runtime::spawn_blocking` delegates to tokio's pool, so an
            // uncounted body here would make "N/512 slots in use" undercount.
            let _slot = qontinui_runner_lib::wedge_diagnostics::BlockingSlot::enter();
            crate::webview_recovery::verify_window_has_a_webview(&probe_window, &probe_label)
        })
        .await
        .map_err(|e| format!("preview probe task for {label} panicked: {e}"))?
        .map_err(|e| {
            format!(
                "{e} This project's preview cannot be reopened until the runner restarts: \
                 Tauri holds the label `{label}` for the life of the process, so building a \
                 replacement fails with `WebviewLabelAlreadyExists`."
            )
        })?;

        existing
            .navigate(parsed.clone())
            .map_err(|e| format!("Failed to navigate the preview window: {e}"))?;
        let _ = existing.unminimize();
        existing
            .set_focus()
            .map_err(|e| format!("Failed to focus the preview window: {e}"))?;
        return Ok(false);
    }

    // Built on a BLOCKING thread, not this tokio worker: the post-build webview
    // probe below is an unbounded event-loop round-trip
    // (`webview_recovery::verify_window_has_a_webview`), which on a cold
    // WebView2 profile takes seconds and on a wedged event loop never returns.
    // No timeout, deliberately — a slow-but-healthy build must not be reported
    // as a failure.
    let app_for_build = app.clone();
    let label_for_build = label.clone();
    tauri::async_runtime::spawn_blocking(move || {
        // Counted for the wedge diagnostics' blocking-pool saturation figure.
        // `tauri::async_runtime::spawn_blocking` delegates to tokio's pool, so an
        // uncounted body here would make "N/512 slots in use" undercount.
        let _slot = qontinui_runner_lib::wedge_diagnostics::BlockingSlot::enter();
        let builder = WebviewWindowBuilder::new(
            &app_for_build,
            &label_for_build,
            WebviewUrl::External(parsed),
        )
        .title(window_title)
        .inner_size(1280.0, 900.0)
        .resizable(true);

        // Same WebView2 environment as this runner's main window (isolated
        // user-data folder + browser args). Without it Tauri forces the PRIMARY's
        // `%LOCALAPPDATA%\com.qontinui.runner` profile root on this preview, which
        // on a secondary runner fails with `HRESULT(0x8007139F)`.
        let builder = crate::webview_recovery::apply_main_window_env_options(builder);

        let window = builder
            .build()
            .map_err(|e| format!("Failed to open the preview window: {e}"))?;

        // `build()` returning `Ok` proves nothing about the webview. What a
        // probe failure means HERE, decided rather than copied: the preview is
        // a single operator-requested window with nothing else depending on it,
        // so the honest response is to fail the invoke that asked for it — the
        // operator pressed Open and gets told it did not open, instead of
        // staring at an empty frame.
        //
        // It is **not** retryable. An earlier version of this comment claimed
        // "a later Open builds a new window under the same label, so nothing is
        // latched"; that is false, and it was the premise the reuse branch's
        // silent `Ok(false)` rested on. Tauri registered this window on
        // `build()`'s `Ok` and can only drop it on a `Destroyed` event wry will
        // never emit for a window it never created, so
        // `project-preview-<id>` is burned for the life of the process:
        // rebuilding it returns `Error::WebviewLabelAlreadyExists`
        // (`tauri` 2.11.1 `src/manager/webview.rs:436-438`). A later Open takes
        // the reuse branch instead and is failed there by the same probe. This
        // is the same semantics `WindowAssignments::reserved_labels` documents
        // for `term-N`.
        //
        // A per-attempt label suffix WOULD make it retryable and was
        // considered. Declined: `preview_window_label` is a pure function of
        // the project id, and it is how the reuse branch above and
        // `close_project_preview` find the window at all. Making the label
        // unpredictable buys a retry at the cost of the one-window-per-project
        // key both of those depend on — a worse defect than a truthful error
        // that names the restart as the cure. Nothing goes into `ui_error` —
        // see the "no backend writer" section of `crate::ui_error`.
        crate::webview_recovery::verify_window_has_a_webview(&window, &label_for_build)?;

        // A preview webview that later DIES is a different failure from one
        // that was never created; the point-in-time probe cannot see it.
        crate::webview_recovery::attach_non_main_process_failed_handler(&window);
        Ok(true)
    })
    .await
    .map_err(|e| format!("preview build task for {label} panicked: {e}"))?
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
