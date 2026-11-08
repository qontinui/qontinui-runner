//! Tab-label registration: the runner webview tells its own backend which
//! terminal pages (tabs) it has, what each is called and in what order (plan
//! `2026-11-08-save-per-tab-runner-session-lists-on-sessions-page-kept-14-days`,
//! D2).
//!
//! Labels and order are webview state: `src/components/terminal/useTerminalPages.ts`
//! keeps them in the webview's own storage, and the backend used to know only
//! page ids. The webview now sends its FULL page list here whenever that list
//! changes, and `terminal::layout::page_labels` keeps the last list for
//! `GET /terminal-pages` and `GET /terminal-layout`.
//!
//! A Tauri command, deliberately not an HTTP route: with no HTTP surface,
//! neither the relay allowlist nor the origin guard has to classify it, and no
//! browser page can rewrite the labels through the loopback API.

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::commands::CommandResponse;
use crate::terminal::layout::{self, PageLabelRegistry, PageLabels, RegisteredPage};

/// Most pages one registration may carry. A real roster is tens of pages; the
/// bound only stops a malformed call from parking an unbounded list.
pub const MAX_REGISTERED_PAGES: usize = 1024;

/// Longest label accepted, in characters.
pub const MAX_LABEL_CHARS: usize = 256;

/// One page as the webview sends it: `{pageId, label, order}`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageLabelInput {
    /// The page id, compared verbatim with `TerminalInfo::page_id`.
    pub page_id: String,
    /// The tab label; blank or absent is served as no label.
    #[serde(default)]
    pub label: Option<String>,
    /// The tab's left-to-right position (0 = leftmost). A negative or
    /// fractional value does not deserialize, so the call is refused before it
    /// reaches the registry.
    pub order: u32,
}

/// Validate one registration and put it in registered order: by `order`,
/// then by position in the list (so equal orders keep the webview's order).
///
/// Refuses the WHOLE registration, leaving the previous one in place, when a
/// page id is blank or listed twice, a label is over [`MAX_LABEL_CHARS`], or
/// the list is over [`MAX_REGISTERED_PAGES`]. A partly-applied list would
/// label some tabs from one roster and some from another.
pub fn normalize_registration(pages: Vec<PageLabelInput>) -> Result<Vec<RegisteredPage>, String> {
    if pages.len() > MAX_REGISTERED_PAGES {
        return Err(format!(
            "terminal_pages_register: {} pages is over the {MAX_REGISTERED_PAGES}-page limit",
            pages.len()
        ));
    }
    let mut seen = HashSet::with_capacity(pages.len());
    let mut ordered = Vec::with_capacity(pages.len());
    for (position, page) in pages.into_iter().enumerate() {
        if page.page_id.trim().is_empty() {
            return Err(format!(
                "terminal_pages_register: page {position} has a blank pageId"
            ));
        }
        if !seen.insert(page.page_id.clone()) {
            return Err(format!(
                "terminal_pages_register: pageId {} is listed twice",
                page.page_id
            ));
        }
        let label = page
            .label
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty());
        if let Some(l) = &label {
            let chars = l.chars().count();
            if chars > MAX_LABEL_CHARS {
                return Err(format!(
                    "terminal_pages_register: the label of pageId {} is {chars} characters, \
                     over the {MAX_LABEL_CHARS}-character limit",
                    page.page_id
                ));
            }
        }
        ordered.push((
            position,
            RegisteredPage {
                page_id: page.page_id,
                label,
                order: page.order,
            },
        ));
    }
    ordered.sort_by_key(|(position, page)| (page.order, *position));
    Ok(ordered.into_iter().map(|(_, page)| page).collect())
}

/// Replace this runner's tab-label registration with the webview's full page
/// list, `{"pages": [{"pageId", "label", "order"}]}`.
///
/// Answers `{registered: <pages kept>, registeredAt: <rfc3339>}`; a refused
/// list (see [`normalize_registration`]) is an error and changes nothing.
#[tauri::command]
pub fn terminal_pages_register(pages: Vec<PageLabelInput>) -> Result<CommandResponse, String> {
    register_into(layout::page_labels(), pages, Utc::now())
}

/// [`terminal_pages_register`] against an explicit registry and clock, so the
/// command's whole effect is testable without the process-wide registry.
fn register_into(
    registry: &PageLabelRegistry,
    pages: Vec<PageLabelInput>,
    at: DateTime<Utc>,
) -> Result<CommandResponse, String> {
    let pages = normalize_registration(pages)?;
    let count = pages.len();
    registry.replace(PageLabels::new(pages, at));
    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(serde_json::json!({
            "registered": count,
            "registeredAt": layout::rfc3339(at),
        })),
    })
}

// Tauri commands this module owns — the ONLY registration site (see
// `crate::ipc_registry`). A `#[tauri::command]` fn missing here is
// unreachable from the frontend.
crate::ipc_group!(terminal_pages_register);

#[cfg(test)]
mod tests {
    use super::*;

    fn input(page_id: &str, label: Option<&str>, order: u32) -> PageLabelInput {
        PageLabelInput {
            page_id: page_id.to_string(),
            label: label.map(str::to_string),
            order,
        }
    }

    fn at() -> DateTime<Utc> {
        DateTime::<Utc>::from_timestamp_millis(1_794_128_400_000).unwrap()
    }

    /// The command's whole effect: the webview's JSON shape deserializes, the
    /// list lands in registered order with trimmed labels, and the answer
    /// says how many pages it kept and when.
    #[test]
    fn a_registration_replaces_the_labels_in_registered_order() {
        let registry = PageLabelRegistry::new();
        let pages: Vec<PageLabelInput> = serde_json::from_value(serde_json::json!([
            {"pageId": "p-b", "label": " Beta ", "order": 1},
            {"pageId": "default", "label": "Terminal", "order": 0},
            {"pageId": "p-c", "label": "   ", "order": 2},
            {"pageId": "p-d", "order": 3},
        ]))
        .expect("the webview's shape deserializes");

        let reply = register_into(&registry, pages, at()).expect("accepted");
        assert!(reply.success);
        assert_eq!(
            reply.data,
            Some(serde_json::json!({
                "registered": 4,
                "registeredAt": "2026-11-08T09:00:00.000Z",
            }))
        );

        let labels = registry.snapshot();
        assert!(labels.registered());
        assert_eq!(labels.registered_at(), Some(at()));
        let served: Vec<(&str, Option<&str>, u32)> = labels
            .pages()
            .iter()
            .map(|p| (p.page_id.as_str(), p.label.as_deref(), p.order))
            .collect();
        assert_eq!(
            served,
            vec![
                ("default", Some("Terminal"), 0),
                ("p-b", Some("Beta"), 1),
                ("p-c", None, 2),
                ("p-d", None, 3),
            ]
        );

        // A later registration replaces the list whole: a page it drops is
        // no longer registered.
        register_into(&registry, vec![input("p-b", Some("Renamed"), 0)], at()).unwrap();
        let served: Vec<(String, Option<String>)> = registry
            .snapshot()
            .pages()
            .iter()
            .map(|p| (p.page_id.clone(), p.label.clone()))
            .collect();
        assert_eq!(
            served,
            vec![("p-b".to_string(), Some("Renamed".to_string()))]
        );
    }

    /// Equal orders keep the webview's list order.
    #[test]
    fn equal_orders_keep_list_order() {
        let ordered = normalize_registration(vec![
            input("x", None, 5),
            input("y", None, 1),
            input("z", None, 5),
        ])
        .unwrap();
        let ids: Vec<&str> = ordered.iter().map(|p| p.page_id.as_str()).collect();
        assert_eq!(ids, vec!["y", "x", "z"]);
    }

    /// A refused list leaves the previous registration exactly as it was.
    #[test]
    fn a_refused_registration_changes_nothing() {
        let registry = PageLabelRegistry::new();
        register_into(&registry, vec![input("p1", Some("One"), 0)], at()).unwrap();
        let before = registry.snapshot();

        let refusals = [
            vec![input("p1", Some("One"), 0), input("p1", Some("Again"), 1)],
            vec![input("  ", Some("Blank id"), 0)],
            vec![input("p2", Some(&"x".repeat(MAX_LABEL_CHARS + 1)), 0)],
            (0..=MAX_REGISTERED_PAGES)
                .map(|i| input(&format!("p{i}"), None, i as u32))
                .collect(),
        ];
        for pages in refusals {
            let n = pages.len();
            let err = register_into(&registry, pages, at()).unwrap_err();
            assert!(
                err.starts_with("terminal_pages_register:"),
                "{n} pages: {err}"
            );
            assert_eq!(registry.snapshot(), before, "{err}");
        }
        // A label exactly at the limit is accepted.
        register_into(
            &registry,
            vec![input("p2", Some(&"é".repeat(MAX_LABEL_CHARS)), 0)],
            at(),
        )
        .expect("at the limit, counted in characters");
    }

    /// A position is a non-negative whole number: a negative or fractional
    /// `order` is refused at deserialization, so it never reaches the registry.
    #[test]
    fn a_negative_or_fractional_order_does_not_deserialize() {
        for order in [serde_json::json!(-1), serde_json::json!(1.5)] {
            let parsed = serde_json::from_value::<Vec<PageLabelInput>>(serde_json::json!([
                {"pageId": "p", "label": "Tab", "order": order},
            ]));
            assert!(parsed.is_err(), "order {order} must be refused");
        }
    }

    /// The command name the webview invokes is registered with the IPC router.
    #[test]
    fn the_command_is_in_this_modules_ipc_group() {
        assert_eq!(IPC_NAMES, &["terminal_pages_register"]);
    }
}
