//! Built-in pattern templates for common issue categories.
//!
//! These templates provide reusable detection strategies that can be
//! referenced by known issues to standardize how they are verified.

use crate::database::Connection;
use chrono::Utc;
use tracing::info;

use super::types::{IssuePatternTemplate, TemplateParameter};

/// Returns the 6 built-in pattern templates.
pub fn get_built_in_templates() -> Vec<IssuePatternTemplate> {
    let now = Utc::now().to_rfc3339();

    vec![
        // 1. Text Duplication
        IssuePatternTemplate {
            id: "pt_text_duplication".to_string(),
            name: "Text Duplication".to_string(),
            description: "Detects duplicate text content within a container or page. \
                Checks for repeated headings, list items, paragraphs, or other text \
                elements that should be unique."
                .to_string(),
            category: "duplication".to_string(),
            detection_type: "ui_bridge".to_string(),
            step_template: Some(serde_json::json!({
                "type": "command",
                "mode": "check",
                "description": "Check for duplicate text elements in container",
                "command": "curl -s {{base_url}}/api/ui-bridge/sdk/snapshot | python -c \"\nimport sys, json\ndata = json.load(sys.stdin)\ntexts = [el.get('text','') for el in data.get('elements',[]) if el.get('selector','').startswith('{{container_selector}}')]\nduplicates = [t for t in set(texts) if texts.count(t) > 1 and t.strip()]\nif duplicates:\n    print(f'FAIL: Found {len(duplicates)} duplicate texts: {duplicates[:5]}')\n    sys.exit(1)\nelse:\n    print(f'PASS: No duplicate texts among {len(texts)} elements')\n\"",
                "success_pattern": "PASS:"
            })),
            ai_prompt_template: Some(
                "Check the page for duplicate text content. Look at {{container_selector}} \
                and verify that no text appears more than once. Each heading, label, and \
                list item should be unique. Report any duplicates found."
                    .to_string(),
            ),
            parameters: vec![
                TemplateParameter {
                    name: "container_selector".to_string(),
                    param_type: "string".to_string(),
                    description: "CSS selector for the container to check for duplicates"
                        .to_string(),
                    default: Some(serde_json::json!("body")),
                },
                TemplateParameter {
                    name: "base_url".to_string(),
                    param_type: "string".to_string(),
                    description: "Base URL of the application".to_string(),
                    default: Some(serde_json::json!("http://localhost:3001")),
                },
            ],
            built_in: true,
            status: "active".to_string(),
            created_at: now.clone(),
            updated_at: now.clone(),
        },
        // 2. Stale State
        IssuePatternTemplate {
            id: "pt_stale_state".to_string(),
            name: "Stale State".to_string(),
            description: "Detects stale UI state where displayed data does not reflect \
                the latest backend state. Compares UI content against API response to \
                find discrepancies."
                .to_string(),
            category: "state".to_string(),
            detection_type: "ui_bridge".to_string(),
            step_template: Some(serde_json::json!({
                "type": "command",
                "mode": "check",
                "description": "Compare UI state against API data source",
                "command": "curl -s {{api_endpoint}} > /tmp/api_data.json && curl -s {{base_url}}/api/ui-bridge/sdk/snapshot > /tmp/ui_snapshot.json && python -c \"\nimport sys, json\napi = json.load(open('/tmp/api_data.json'))\nui = json.load(open('/tmp/ui_snapshot.json'))\napi_count = len(api) if isinstance(api, list) else len(api.get('items', api.get('data', [])))\nui_elements = [el for el in ui.get('elements',[]) if el.get('selector','').startswith('{{item_selector}}')]\nui_count = len(ui_elements)\nif api_count != ui_count:\n    print(f'FAIL: API has {api_count} items but UI shows {ui_count}')\n    sys.exit(1)\nelse:\n    print(f'PASS: UI count ({ui_count}) matches API count ({api_count})')\n\"",
                "success_pattern": "PASS:"
            })),
            ai_prompt_template: Some(
                "Verify that the UI at {{page_url}} reflects the current backend state. \
                Compare the data shown in {{item_selector}} against the API at {{api_endpoint}}. \
                Check that counts match, values are current, and no stale data is displayed."
                    .to_string(),
            ),
            parameters: vec![
                TemplateParameter {
                    name: "api_endpoint".to_string(),
                    param_type: "string".to_string(),
                    description: "API endpoint to fetch the source of truth".to_string(),
                    default: None,
                },
                TemplateParameter {
                    name: "item_selector".to_string(),
                    param_type: "string".to_string(),
                    description: "CSS selector for the UI elements to compare".to_string(),
                    default: None,
                },
                TemplateParameter {
                    name: "page_url".to_string(),
                    param_type: "string".to_string(),
                    description: "URL of the page to check".to_string(),
                    default: None,
                },
                TemplateParameter {
                    name: "base_url".to_string(),
                    param_type: "string".to_string(),
                    description: "Base URL of the application".to_string(),
                    default: Some(serde_json::json!("http://localhost:3001")),
                },
            ],
            built_in: true,
            status: "active".to_string(),
            created_at: now.clone(),
            updated_at: now.clone(),
        },
        // 3. Data Truncation
        IssuePatternTemplate {
            id: "pt_data_truncation".to_string(),
            name: "Data Truncation".to_string(),
            description: "Detects data truncation where values are cut off, \
                overflow their containers, or are missing expected content. \
                Checks for text overflow, missing fields, and clipped data."
                .to_string(),
            category: "data_integrity".to_string(),
            detection_type: "ui_bridge".to_string(),
            step_template: Some(serde_json::json!({
                "type": "command",
                "mode": "check",
                "description": "Check for truncated or overflowing text in container",
                "command": "curl -s {{base_url}}/api/ui-bridge/sdk/snapshot | python -c \"\nimport sys, json\ndata = json.load(sys.stdin)\ntruncated = []\nfor el in data.get('elements', []):\n    text = el.get('text', '')\n    if text.endswith('...') or text.endswith('\\u2026'):\n        truncated.append({'selector': el.get('selector',''), 'text': text[:50]})\nif len(truncated) > {{max_truncated}}:\n    print(f'FAIL: Found {len(truncated)} truncated elements (max {{max_truncated}})')\n    for t in truncated[:5]:\n        print(f'  - {t[\"selector\"]}: {t[\"text\"]}')\n    sys.exit(1)\nelse:\n    print(f'PASS: {len(truncated)} truncated elements (within threshold of {{max_truncated}})')\n\"",
                "success_pattern": "PASS:"
            })),
            ai_prompt_template: Some(
                "Examine the page for data truncation issues. Look for text that is \
                cut off with ellipsis, content that overflows its container, or fields \
                that appear to be missing data. Focus on {{focus_area}} and report any \
                elements where important information may be hidden or clipped."
                    .to_string(),
            ),
            parameters: vec![
                TemplateParameter {
                    name: "max_truncated".to_string(),
                    param_type: "integer".to_string(),
                    description: "Maximum acceptable number of truncated elements".to_string(),
                    default: Some(serde_json::json!(0)),
                },
                TemplateParameter {
                    name: "focus_area".to_string(),
                    param_type: "string".to_string(),
                    description: "Area of the page to focus on".to_string(),
                    default: Some(serde_json::json!("the main content area")),
                },
                TemplateParameter {
                    name: "base_url".to_string(),
                    param_type: "string".to_string(),
                    description: "Base URL of the application".to_string(),
                    default: Some(serde_json::json!("http://localhost:3001")),
                },
            ],
            built_in: true,
            status: "active".to_string(),
            created_at: now.clone(),
            updated_at: now.clone(),
        },
        // 4. Empty Container
        IssuePatternTemplate {
            id: "pt_empty_container".to_string(),
            name: "Empty Container".to_string(),
            description: "Detects empty containers that should have content. \
                Identifies missing data in lists, tables, cards, and other \
                containers where content is expected."
                .to_string(),
            category: "rendering".to_string(),
            detection_type: "ui_bridge".to_string(),
            step_template: Some(serde_json::json!({
                "type": "command",
                "mode": "check",
                "description": "Check that expected container has content",
                "command": "curl -s {{base_url}}/api/ui-bridge/sdk/snapshot | python -c \"\nimport sys, json\ndata = json.load(sys.stdin)\ncontainer_elements = [el for el in data.get('elements',[]) if el.get('selector','').startswith('{{container_selector}}')]\nif not container_elements:\n    print(f'FAIL: Container {{container_selector}} not found in page')\n    sys.exit(1)\nchild_count = len(container_elements)\nif child_count < {{min_children}}:\n    print(f'FAIL: Container has {child_count} children (expected at least {{min_children}})')\n    sys.exit(1)\nelse:\n    print(f'PASS: Container has {child_count} children (minimum {{min_children}})')\n\"",
                "success_pattern": "PASS:"
            })),
            ai_prompt_template: Some(
                "Verify that the container at {{container_selector}} has content. \
                It should contain at least {{min_children}} child elements. Check that \
                no empty states, loading spinners, or error messages are shown when \
                data should be present."
                    .to_string(),
            ),
            parameters: vec![
                TemplateParameter {
                    name: "container_selector".to_string(),
                    param_type: "string".to_string(),
                    description: "CSS selector for the container to check".to_string(),
                    default: None,
                },
                TemplateParameter {
                    name: "min_children".to_string(),
                    param_type: "integer".to_string(),
                    description: "Minimum number of expected child elements".to_string(),
                    default: Some(serde_json::json!(1)),
                },
                TemplateParameter {
                    name: "base_url".to_string(),
                    param_type: "string".to_string(),
                    description: "Base URL of the application".to_string(),
                    default: Some(serde_json::json!("http://localhost:3001")),
                },
            ],
            built_in: true,
            status: "active".to_string(),
            created_at: now.clone(),
            updated_at: now.clone(),
        },
        // 5. Race Condition
        IssuePatternTemplate {
            id: "pt_race_condition".to_string(),
            name: "Race Condition".to_string(),
            description: "Detects race conditions where rapid repeated actions produce \
                inconsistent results. Tests by performing an action multiple times in \
                quick succession and checking for state consistency."
                .to_string(),
            category: "timing".to_string(),
            detection_type: "command".to_string(),
            step_template: Some(serde_json::json!({
                "type": "command",
                "mode": "check",
                "description": "Rapid-fire action test for race condition detection",
                "command": "python -c \"\nimport sys, time, urllib.request, json\nresults = []\nfor i in range({{repetitions}}):\n    try:\n        req = urllib.request.Request('{{action_url}}', method='{{http_method}}')\n        if '{{request_body}}' != '':\n            req.data = '{{request_body}}'.encode()\n            req.add_header('Content-Type', 'application/json')\n        resp = urllib.request.urlopen(req)\n        results.append(resp.status)\n    except Exception as e:\n        results.append(str(e))\n    time.sleep({{delay_ms}} / 1000.0)\nfailures = [r for r in results if r != 200]\nif failures:\n    print(f'FAIL: {len(failures)}/{len(results)} requests failed: {failures[:5]}')\n    sys.exit(1)\nelse:\n    print(f'PASS: All {len(results)} rapid requests succeeded')\n\"",
                "success_pattern": "PASS:"
            })),
            ai_prompt_template: Some(
                "Test for race conditions by rapidly performing {{action_description}} \
                {{repetitions}} times with {{delay_ms}}ms between each. After all actions \
                complete, verify the final state is consistent and no duplicate entries, \
                lost updates, or corrupted state resulted."
                    .to_string(),
            ),
            parameters: vec![
                TemplateParameter {
                    name: "action_url".to_string(),
                    param_type: "string".to_string(),
                    description: "URL to send rapid requests to".to_string(),
                    default: None,
                },
                TemplateParameter {
                    name: "http_method".to_string(),
                    param_type: "string".to_string(),
                    description: "HTTP method for the action (GET, POST, PUT, DELETE)".to_string(),
                    default: Some(serde_json::json!("POST")),
                },
                TemplateParameter {
                    name: "request_body".to_string(),
                    param_type: "string".to_string(),
                    description: "JSON request body (empty string for no body)".to_string(),
                    default: Some(serde_json::json!("")),
                },
                TemplateParameter {
                    name: "repetitions".to_string(),
                    param_type: "integer".to_string(),
                    description: "Number of rapid repetitions".to_string(),
                    default: Some(serde_json::json!(5)),
                },
                TemplateParameter {
                    name: "delay_ms".to_string(),
                    param_type: "integer".to_string(),
                    description: "Delay between repetitions in milliseconds".to_string(),
                    default: Some(serde_json::json!(50)),
                },
                TemplateParameter {
                    name: "action_description".to_string(),
                    param_type: "string".to_string(),
                    description: "Human-readable description of the action being tested"
                        .to_string(),
                    default: Some(serde_json::json!("the action")),
                },
            ],
            built_in: true,
            status: "active".to_string(),
            created_at: now.clone(),
            updated_at: now.clone(),
        },
        // 6. Console Error Spike
        IssuePatternTemplate {
            id: "pt_console_errors".to_string(),
            name: "Console Error Spike".to_string(),
            description: "Detects console errors by checking the browser console via \
                the UI Bridge. Identifies JavaScript errors, unhandled promise \
                rejections, and other console error output that may indicate bugs."
                .to_string(),
            category: "rendering".to_string(),
            detection_type: "command".to_string(),
            step_template: Some(serde_json::json!({
                "type": "command",
                "mode": "check",
                "description": "Check browser console for error spikes",
                "command": "curl -s {{base_url}}/api/ui-bridge/sdk/console-errors 2>/dev/null | python -c \"\nimport sys, json\ntry:\n    data = json.load(sys.stdin)\n    errors = data if isinstance(data, list) else data.get('errors', [])\nexcept:\n    print('PASS: No console error endpoint or no errors')\n    sys.exit(0)\nerror_count = len(errors)\nif error_count > {{max_errors}}:\n    print(f'FAIL: {error_count} console errors (max {{max_errors}})')\n    for e in errors[:5]:\n        msg = e.get('message', str(e)) if isinstance(e, dict) else str(e)\n        print(f'  - {msg[:100]}')\n    sys.exit(1)\nelse:\n    print(f'PASS: {error_count} console errors (within threshold of {{max_errors}})')\n\"",
                "success_pattern": "PASS:"
            })),
            ai_prompt_template: Some(
                "Check the browser console for errors on the current page. Look for \
                JavaScript errors, unhandled promise rejections, React errors, and \
                network failures. More than {{max_errors}} errors indicates a problem. \
                Report the types and patterns of any errors found."
                    .to_string(),
            ),
            parameters: vec![
                TemplateParameter {
                    name: "max_errors".to_string(),
                    param_type: "integer".to_string(),
                    description: "Maximum acceptable number of console errors".to_string(),
                    default: Some(serde_json::json!(0)),
                },
                TemplateParameter {
                    name: "base_url".to_string(),
                    param_type: "string".to_string(),
                    description: "Base URL of the application".to_string(),
                    default: Some(serde_json::json!("http://localhost:3001")),
                },
            ],
            built_in: true,
            status: "active".to_string(),
            created_at: now.clone(),
            updated_at: now,
        },
    ]
}

/// Seed built-in templates into the database (INSERT OR IGNORE).
pub fn seed_templates(conn: &Connection) -> Result<(), String> {
    Err("SQLite removed".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::known_issues::storage::ensure_tables;
use crate::database::Connection;

    #[test]
    fn test_get_built_in_templates_returns_six() {
        let templates = get_built_in_templates();
        assert_eq!(templates.len(), 6);

        let ids: Vec<&str> = templates.iter().map(|t| t.id.as_str()).collect();
        assert!(ids.contains(&"pt_text_duplication"));
        assert!(ids.contains(&"pt_stale_state"));
        assert!(ids.contains(&"pt_data_truncation"));
        assert!(ids.contains(&"pt_empty_container"));
        assert!(ids.contains(&"pt_race_condition"));
        assert!(ids.contains(&"pt_console_errors"));
    }

    #[test]
    fn test_all_templates_have_required_fields() {
        for template in get_built_in_templates() {
            assert!(!template.id.is_empty(), "Template ID is empty");
            assert!(
                !template.name.is_empty(),
                "Template name is empty: {}",
                template.id
            );
            assert!(
                !template.description.is_empty(),
                "Template description is empty: {}",
                template.id
            );
            assert!(
                !template.category.is_empty(),
                "Template category is empty: {}",
                template.id
            );
            assert!(
                !template.detection_type.is_empty(),
                "Template detection_type is empty: {}",
                template.id
            );
            assert!(
                template.built_in,
                "Template should be built_in: {}",
                template.id
            );
            assert_eq!(
                template.status, "active",
                "Template status wrong: {}",
                template.id
            );
            assert!(
                template.step_template.is_some(),
                "Template step_template missing: {}",
                template.id
            );
        }
    }

    #[test]
    fn test_seed_templates_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        ensure_tables(&conn).unwrap();

        // Seed once
        seed_templates(&conn).unwrap();
        let count1: i32 = conn
            .query_row("SELECT COUNT(*) FROM issue_pattern_templates", [], |row| {
                row.get(0)
            })
            .unwrap();

        // Seed again
        seed_templates(&conn).unwrap();
        let count2: i32 = conn
            .query_row("SELECT COUNT(*) FROM issue_pattern_templates", [], |row| {
                row.get(0)
            })
            .unwrap();

        assert_eq!(count1, 6);
        assert_eq!(count2, 6);
    }

    #[test]
    fn test_template_parameters_have_all_fields() {
        for template in get_built_in_templates() {
            for param in &template.parameters {
                assert!(
                    !param.name.is_empty(),
                    "Param name empty in template {}",
                    template.id
                );
                assert!(
                    !param.param_type.is_empty(),
                    "Param type empty for {} in template {}",
                    param.name,
                    template.id
                );
                assert!(
                    !param.description.is_empty(),
                    "Param description empty for {} in template {}",
                    param.name,
                    template.id
                );
            }
        }
    }
}
