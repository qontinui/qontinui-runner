//! Built-in contexts shipped with the runner.
//!
//! These are read-only example contexts that users can copy to their own library.

#![allow(dead_code)]

use super::types::{Context, ContextAutoInclude};

/// Get built-in contexts shipped with the runner.
///
/// These are read-only example contexts that users can copy to their own library.
pub fn get_builtin_contexts() -> Vec<Context> {
    let now = chrono::Utc::now().to_rfc3339();

    vec![
        Context {
            id: "builtin-debugging".to_string(),
            name: "Debugging Guide".to_string(),
            content: r#"## Debugging Guide

When debugging issues:

1. **Check the Iteration Bundle first** - Look for `## Iteration N Data Bundle` in your prompt
   - Pre-Execution Results show step success/failure
   - Application Logs are captured and included
   - Errors and warnings are highlighted
2. **Identify the root cause** - Don't fix symptoms, fix the source
3. **Work autonomously** - Restart services as needed, don't ask the user
4. **Iterate until fixed** - Make changes, test, repeat

### Log Data is Bundled

All relevant logs are delivered in the Iteration Bundle - no file searching required:
- Application logs from user-defined sources
- GUI automation events (if workflow has GUI steps)
- Playwright test results (if workflow has test steps)
- Screenshots from automation steps

Only access raw log files in `.dev-logs/` as a fallback if bundled data is insufficient.
"#
            .to_string(),
            category: Some("debugging".to_string()),
            tags: vec!["debugging".to_string(), "logs".to_string()],
            auto_include: Some(ContextAutoInclude {
                task_mentions: Some(vec![
                    "debug".to_string(),
                    "error".to_string(),
                    "fix".to_string(),
                    "issue".to_string(),
                ]),
                error_patterns: Some(vec!["error".to_string(), "exception".to_string()]),
                ..Default::default()
            }),
            created_at: now.clone(),
            modified_at: now.clone(),
        },
        Context {
            id: "builtin-no-backward-compat".to_string(),
            name: "No Backward Compatibility".to_string(),
            content: r#"## Project Philosophy: No Backward Compatibility

This project is in active development. Backward compatibility is NOT a priority.

### When You Find Legacy Code
- **Fix the source** - Don't add compatibility shims
- **Delete deprecated code** - Don't mark as @deprecated and leave it
- **Update schemas at the source** - Don't add normalization layers
- **Re-export old configs** - If an old config doesn't match, have the user re-export

### Anti-Patterns to Avoid
- Adding `|| legacyValue` fallbacks
- Creating migration layers for old formats
- Maintaining both old and new field names
- Adding "handle both cases" code
"#
            .to_string(),
            category: Some("philosophy".to_string()),
            tags: vec!["philosophy".to_string(), "standards".to_string()],
            auto_include: Some(ContextAutoInclude {
                task_mentions: Some(vec![
                    "legacy".to_string(),
                    "backward".to_string(),
                    "compatibility".to_string(),
                    "deprecated".to_string(),
                ]),
                ..Default::default()
            }),
            created_at: now.clone(),
            modified_at: now.clone(),
        },
        Context {
            id: "builtin-runner-architecture".to_string(),
            name: "Runner Architecture & Logs".to_string(),
            content: include_str!("builtins/runner_architecture.md").to_string(),
            category: Some("architecture".to_string()),
            tags: vec![
                "runner".to_string(),
                "logs".to_string(),
                "debugging".to_string(),
                "architecture".to_string(),
            ],
            auto_include: Some(ContextAutoInclude {
                task_mentions: Some(vec![
                    "runner".to_string(),
                    "log".to_string(),
                    "automation".to_string(),
                    "workflow".to_string(),
                    "screenshot".to_string(),
                    "execution".to_string(),
                ]),
                error_patterns: Some(vec![
                    "failed".to_string(),
                    "error".to_string(),
                    "not found".to_string(),
                ]),
                ..Default::default()
            }),
            created_at: now.clone(),
            modified_at: now.clone(),
        },
        Context {
            id: "builtin-multi-step-guide".to_string(),
            name: "Multi-Step Task Guide".to_string(),
            content: include_str!("builtins/multi_step_guide.md").to_string(),
            category: Some("workflow".to_string()),
            tags: vec![
                "multi-step".to_string(),
                "workflow".to_string(),
                "runner".to_string(),
            ],
            auto_include: None, // Always injected for runner-triggered sessions
            created_at: now.clone(),
            modified_at: now.clone(),
        },
        Context {
            id: "builtin-input-validation".to_string(),
            name: "Input Validation Guide".to_string(),
            content: include_str!("builtins/input_validation.md").to_string(),
            category: Some("debugging".to_string()),
            tags: vec![
                "debugging".to_string(),
                "input".to_string(),
                "coordinates".to_string(),
                "multi-monitor".to_string(),
            ],
            auto_include: None, // Explicitly added when "Capture Input for Validation" is enabled
            created_at: now.clone(),
            modified_at: now.clone(),
        },
        Context {
            id: "builtin-runner-test-api".to_string(),
            name: "Runner Test API Reference".to_string(),
            content: include_str!("builtins/runner_test_api.md").to_string(),
            category: Some("testing".to_string()),
            tags: vec![
                "testing".to_string(),
                "api".to_string(),
                "ui-bridge".to_string(),
                "python".to_string(),
            ],
            auto_include: Some(ContextAutoInclude {
                task_mentions: Some(vec![
                    "test".to_string(),
                    "ui-bridge".to_string(),
                    "playwright".to_string(),
                    "python script".to_string(),
                    "verify".to_string(),
                    "assertion".to_string(),
                ]),
                ..Default::default()
            }),
            created_at: now.clone(),
            modified_at: now.clone(),
        },
        Context {
            id: "builtin-ui-bridge-core".to_string(),
            name: "UI Bridge Control - Core API".to_string(),
            content: include_str!("builtins/ui_bridge_core.md").to_string(),
            category: Some("ui-bridge".to_string()),
            tags: vec![
                "ui-bridge".to_string(),
                "automation".to_string(),
                "api".to_string(),
                "control".to_string(),
            ],
            auto_include: Some(ContextAutoInclude {
                task_mentions: Some(vec![
                    "ui-bridge".to_string(),
                    "ui bridge".to_string(),
                    "control app".to_string(),
                    "automate".to_string(),
                    "click element".to_string(),
                    "mobile app".to_string(),
                    "web app".to_string(),
                ]),
                ..Default::default()
            }),
            created_at: now.clone(),
            modified_at: now.clone(),
        },
        Context {
            id: "builtin-ui-bridge-mobile".to_string(),
            name: "UI Bridge Control - qontinui-mobile".to_string(),
            content: include_str!("builtins/ui_bridge_mobile.md").to_string(),
            category: Some("ui-bridge".to_string()),
            tags: vec![
                "ui-bridge".to_string(),
                "mobile".to_string(),
                "qontinui-mobile".to_string(),
                "react-native".to_string(),
            ],
            auto_include: Some(ContextAutoInclude {
                task_mentions: Some(vec![
                    "qontinui-mobile".to_string(),
                    "mobile app".to_string(),
                    "react native".to_string(),
                    "android".to_string(),
                    "ios".to_string(),
                ]),
                file_patterns: Some(vec!["**/qontinui-mobile/**".to_string()]),
                ..Default::default()
            }),
            created_at: now.clone(),
            modified_at: now.clone(),
        },
        Context {
            id: "builtin-ui-bridge-web".to_string(),
            name: "UI Bridge Control - qontinui-web".to_string(),
            content: include_str!("builtins/ui_bridge_web.md").to_string(),
            category: Some("ui-bridge".to_string()),
            tags: vec![
                "ui-bridge".to_string(),
                "web".to_string(),
                "qontinui-web".to_string(),
                "next.js".to_string(),
            ],
            auto_include: Some(ContextAutoInclude {
                task_mentions: Some(vec![
                    "qontinui-web".to_string(),
                    "web app".to_string(),
                    "next.js".to_string(),
                    "frontend".to_string(),
                ]),
                file_patterns: Some(vec!["**/qontinui-web/**".to_string()]),
                ..Default::default()
            }),
            created_at: now.clone(),
            modified_at: now.clone(),
        },
        Context {
            id: "builtin-ui-bridge-custom".to_string(),
            name: "UI Bridge Control - Custom App Template".to_string(),
            content: include_str!("builtins/ui_bridge_custom.md").to_string(),
            category: Some("ui-bridge".to_string()),
            tags: vec![
                "ui-bridge".to_string(),
                "template".to_string(),
                "custom".to_string(),
            ],
            auto_include: None, // Manual inclusion only
            created_at: now.clone(),
            modified_at: now.clone(),
        },
        Context {
            id: "builtin-shell-command-schema".to_string(),
            name: "Shell Command Step Schema".to_string(),
            content: include_str!("builtins/shell_command_schema.md").to_string(),
            category: Some("workflow-generation".to_string()),
            tags: vec![
                "shell".to_string(),
                "command".to_string(),
                "schema".to_string(),
                "workflow".to_string(),
                "step-type".to_string(),
            ],
            auto_include: Some(ContextAutoInclude {
                task_mentions: Some(vec![
                    "shell".to_string(),
                    "command".to_string(),
                    "install".to_string(),
                    "build".to_string(),
                    "deploy".to_string(),
                    "setup".to_string(),
                    "cleanup".to_string(),
                ]),
                ..Default::default()
            }),
            created_at: now.clone(),
            modified_at: now.clone(),
        },
        Context {
            id: "builtin-service-restart".to_string(),
            name: "Service Restart Commands".to_string(),
            content: include_str!("builtins/service_restart.md").to_string(),
            category: Some("development".to_string()),
            tags: vec![
                "restart".to_string(),
                "services".to_string(),
                "development".to_string(),
            ],
            auto_include: Some(ContextAutoInclude {
                task_mentions: Some(vec![
                    "restart".to_string(),
                    "service".to_string(),
                    "backend".to_string(),
                    "frontend".to_string(),
                ]),
                ..Default::default()
            }),
            created_at: now.clone(),
            modified_at: now,
        },
    ]
}
