//! Script generation from recordings
//!
//! Generates executable automation scripts from recorded user interactions.

use super::types::*;

/// Script generator for different export formats
pub struct ScriptGenerator;

impl ScriptGenerator {
    /// Generate a script from recording actions
    pub fn generate(
        recording: &Recording,
        actions: &[RecordedAction],
        format: ExportFormat,
        options: &ExportOptions,
    ) -> Result<String, String> {
        match format {
            ExportFormat::Python => Self::generate_python(recording, actions, options),
            ExportFormat::Playwright => Self::generate_playwright(recording, actions, options),
            ExportFormat::Pytest => Self::generate_pytest(recording, actions, options),
            ExportFormat::Cypress => Self::generate_cypress(recording, actions, options),
        }
    }

    /// Get default file name for export format
    pub fn default_file_name(recording: &Recording, format: ExportFormat) -> String {
        let sanitized_name = recording
            .name
            .to_lowercase()
            .replace(' ', "_")
            .replace(|c: char| !c.is_alphanumeric() && c != '_', "");

        match format {
            ExportFormat::Python => format!("{}_automation.py", sanitized_name),
            ExportFormat::Playwright => format!("{}.spec.ts", sanitized_name),
            ExportFormat::Pytest => format!("test_{}.py", sanitized_name),
            ExportFormat::Cypress => format!("{}.cy.ts", sanitized_name),
        }
    }

    /// Generate Python script using UI Bridge client
    fn generate_python(
        recording: &Recording,
        actions: &[RecordedAction],
        options: &ExportOptions,
    ) -> Result<String, String> {
        let mut script = String::new();

        // Header and imports
        script.push_str(
            r#"#!/usr/bin/env python3
"""
Auto-generated automation script from recording.
Recording: "#,
        );
        script.push_str(&recording.name);
        if let Some(desc) = &recording.description {
            script.push_str(&format!("\nDescription: {}", desc));
        }
        script.push_str(&format!("\nBase URL: {}", recording.base_url));
        script.push_str(&format!("\nGenerated: {}", chrono::Utc::now().to_rfc3339()));
        script.push_str(
            r#"
"""

import time
from typing import Optional

# UI Bridge client (assuming qontinui-mcp or similar client)
try:
    from qontinui_mcp import UIBridgeClient
except ImportError:
    # Fallback: use HTTP client directly
    import requests

    class UIBridgeClient:
        def __init__(self, base_url: str = "http://localhost:9876"):
            self.base_url = base_url
            self.session = requests.Session()

        def click(self, selector: str, **kwargs) -> dict:
            return self._execute_action("click", selector, **kwargs)

        def type_text(self, selector: str, text: str, **kwargs) -> dict:
            return self._execute_action("type", selector, value=text, **kwargs)

        def navigate(self, url: str) -> dict:
            resp = self.session.post(f"{self.base_url}/extension/command", json={
                "action": "navigate",
                "params": {"url": url}
            })
            return resp.json()

        def select(self, selector: str, value: str, **kwargs) -> dict:
            return self._execute_action("select", selector, value=value, **kwargs)

        def wait_for_element(self, selector: str, timeout: int = 10000) -> dict:
            return self._execute_action("waitForElement", selector, timeout=timeout)

        def _execute_action(self, action: str, selector: str, **kwargs) -> dict:
            resp = self.session.post(f"{self.base_url}/extension/command", json={
                "action": "executeAction",
                "params": {
                    "action": action,
                    "selector": selector,
                    **kwargs
                }
            })
            return resp.json()


def run_automation(base_url: Optional[str] = None) -> None:
    """Run the recorded automation sequence."""
    client = UIBridgeClient()

    # Navigate to starting URL
    start_url = base_url or ""#,
        );
        script.push_str(&format!("\"{}\"", recording.base_url));
        script.push_str(
            r#"
    print(f"Navigating to {start_url}")
    client.navigate(start_url)
"#,
        );

        // Add wait based on strategy
        Self::add_python_wait(&mut script, options);

        // Generate action code
        for (i, action) in actions.iter().enumerate() {
            script.push_str(&format!(
                "\n    # Action {}: {} on {}\n",
                i + 1,
                action.action_type,
                action.target.tag_name
            ));

            let selector = Self::get_best_selector(&action.target, options);

            match action.action_type {
                ActionType::Click => {
                    script.push_str(&format!("    print(\"Clicking: {}\")\n", selector));
                    script.push_str(&format!("    client.click(\"{}\")\n", selector));
                }
                ActionType::Type => {
                    if let Some(ActionData::Type(data)) = &action.action_data {
                        script.push_str(&format!("    print(\"Typing into: {}\")\n", selector));
                        if data.clear_first {
                            script.push_str(&format!("    client.click(\"{}\")\n", selector));
                            script.push_str("    client.type_text(\"\", clear_first=True)\n");
                        }
                        script.push_str(&format!(
                            "    client.type_text(\"{}\", \"{}\")\n",
                            selector,
                            data.value.replace('\\', "\\\\").replace('"', "\\\"")
                        ));
                    }
                }
                ActionType::Navigate => {
                    if let Some(ActionData::Navigate(data)) = &action.action_data {
                        script
                            .push_str(&format!("    print(\"Navigating to: {}\")\n", data.to_url));
                        script.push_str(&format!("    client.navigate(\"{}\")\n", data.to_url));
                    }
                }
                ActionType::Select => {
                    if let Some(ActionData::Select(data)) = &action.action_data {
                        script.push_str(&format!(
                            "    print(\"Selecting: {} in {}\")\n",
                            data.value, selector
                        ));
                        script.push_str(&format!(
                            "    client.select(\"{}\", \"{}\")\n",
                            selector, data.value
                        ));
                    }
                }
                ActionType::Scroll => {
                    if let Some(ActionData::Scroll(data)) = &action.action_data {
                        script.push_str(&format!(
                            "    print(\"Scrolling: deltaY={}\")\n",
                            data.delta_y
                        ));
                        script.push_str("    # Scroll action - implement based on your client\n");
                    }
                }
                ActionType::Keypress => {
                    if let Some(ActionData::Keypress(data)) = &action.action_data {
                        script.push_str(&format!("    print(\"Pressing key: {}\")\n", data.key));
                        script.push_str(&format!(
                            "    # Keypress: {} - implement based on your client\n",
                            data.key
                        ));
                    }
                }
                ActionType::Hover => {
                    script.push_str(&format!("    print(\"Hovering: {}\")\n", selector));
                    script.push_str("    # Hover action - implement based on your client\n");
                }
            }

            // Add wait between actions
            if i < actions.len() - 1 {
                Self::add_python_wait(&mut script, options);
            }
        }

        script.push_str(
            r#"
    print("Automation completed successfully!")


if __name__ == "__main__":
    run_automation()
"#,
        );

        Ok(script)
    }

    /// Generate Playwright TypeScript test
    fn generate_playwright(
        recording: &Recording,
        actions: &[RecordedAction],
        options: &ExportOptions,
    ) -> Result<String, String> {
        let test_name = options
            .test_name
            .clone()
            .unwrap_or_else(|| recording.name.clone());

        let mut script = String::new();

        // Header
        script.push_str(
            r#"/**
 * Auto-generated Playwright test from recording.
 * Recording: "#,
        );
        script.push_str(&recording.name);
        script.push_str(&format!("\n * Base URL: {}", recording.base_url));
        script.push_str(&format!(
            "\n * Generated: {}",
            chrono::Utc::now().to_rfc3339()
        ));
        script.push_str(
            r#"
 */

import { test, expect } from '@playwright/test';

test.describe('"#,
        );
        script.push_str(&test_name);
        script.push_str(
            r#"', () => {
  test('"#,
        );
        script.push_str(&test_name);
        script.push_str(
            r#"', async ({ page }) => {
    // Navigate to starting URL
    await page.goto('"#,
        );
        script.push_str(&recording.base_url);
        script.push_str("');\n");

        // Add wait
        Self::add_playwright_wait(&mut script, options);

        // Generate action code
        for (i, action) in actions.iter().enumerate() {
            script.push_str(&format!(
                "\n    // Action {}: {} on {}\n",
                i + 1,
                action.action_type,
                action.target.tag_name
            ));

            let selector = Self::get_playwright_selector(&action.target, options);

            match action.action_type {
                ActionType::Click => {
                    script.push_str(&format!(
                        "    await page.locator('{}').click();\n",
                        selector
                    ));
                }
                ActionType::Type => {
                    if let Some(ActionData::Type(data)) = &action.action_data {
                        if data.clear_first {
                            script.push_str(&format!(
                                "    await page.locator('{}').clear();\n",
                                selector
                            ));
                        }
                        script.push_str(&format!(
                            "    await page.locator('{}').fill('{}');\n",
                            selector,
                            data.value.replace('\'', "\\'")
                        ));
                    }
                }
                ActionType::Navigate => {
                    if let Some(ActionData::Navigate(data)) = &action.action_data {
                        script.push_str(&format!("    await page.goto('{}');\n", data.to_url));
                    }
                }
                ActionType::Select => {
                    if let Some(ActionData::Select(data)) = &action.action_data {
                        script.push_str(&format!(
                            "    await page.locator('{}').selectOption('{}');\n",
                            selector, data.value
                        ));
                    }
                }
                ActionType::Scroll => {
                    if let Some(ActionData::Scroll(data)) = &action.action_data {
                        script.push_str(&format!(
                            "    await page.mouse.wheel({}, {});\n",
                            data.delta_x, data.delta_y
                        ));
                    }
                }
                ActionType::Keypress => {
                    if let Some(ActionData::Keypress(data)) = &action.action_data {
                        script
                            .push_str(&format!("    await page.keyboard.press('{}');\n", data.key));
                    }
                }
                ActionType::Hover => {
                    script.push_str(&format!(
                        "    await page.locator('{}').hover();\n",
                        selector
                    ));
                }
            }

            // Add visibility assertion if requested
            if options.include_visibility_assertions && action.action_type != ActionType::Navigate {
                script.push_str(&format!(
                    "    await expect(page.locator('{}')).toBeVisible();\n",
                    selector
                ));
            }

            // Add wait between actions
            if i < actions.len() - 1 {
                Self::add_playwright_wait(&mut script, options);
            }
        }

        script.push_str("  });\n});\n");

        Ok(script)
    }

    /// Generate pytest-playwright test
    fn generate_pytest(
        recording: &Recording,
        actions: &[RecordedAction],
        options: &ExportOptions,
    ) -> Result<String, String> {
        let test_name = options
            .test_name
            .clone()
            .map(|n| format!("test_{}", n.to_lowercase().replace(' ', "_")))
            .unwrap_or_else(|| format!("test_{}", recording.name.to_lowercase().replace(' ', "_")));

        let mut script = String::new();

        // Header
        script.push_str(
            r#""""
Auto-generated pytest-playwright test from recording.
Recording: "#,
        );
        script.push_str(&recording.name);
        script.push_str(&format!("\nBase URL: {}", recording.base_url));
        script.push_str(&format!("\nGenerated: {}", chrono::Utc::now().to_rfc3339()));
        script.push_str(
            r#"
"""

import pytest
from playwright.sync_api import Page, expect


"#,
        );
        script.push_str(&format!("def {}(page: Page) -> None:\n", test_name));
        if let Some(desc) = &options.test_description {
            script.push_str(&format!("    \"\"\"{}.\"\"\"\n", desc));
        } else {
            script.push_str(&format!("    \"\"\"Test: {}.\"\"\"\n", recording.name));
        }

        // Navigate to starting URL
        script.push_str(&format!("    page.goto(\"{}\")\n", recording.base_url));

        // Add wait
        Self::add_pytest_wait(&mut script, options);

        // Generate action code
        for (i, action) in actions.iter().enumerate() {
            script.push_str(&format!(
                "\n    # Action {}: {} on {}\n",
                i + 1,
                action.action_type,
                action.target.tag_name
            ));

            let selector = Self::get_playwright_selector(&action.target, options);

            match action.action_type {
                ActionType::Click => {
                    script.push_str(&format!("    page.locator(\"{}\").click()\n", selector));
                }
                ActionType::Type => {
                    if let Some(ActionData::Type(data)) = &action.action_data {
                        if data.clear_first {
                            script
                                .push_str(&format!("    page.locator(\"{}\").clear()\n", selector));
                        }
                        script.push_str(&format!(
                            "    page.locator(\"{}\").fill(\"{}\")\n",
                            selector,
                            data.value.replace('"', "\\\"")
                        ));
                    }
                }
                ActionType::Navigate => {
                    if let Some(ActionData::Navigate(data)) = &action.action_data {
                        script.push_str(&format!("    page.goto(\"{}\")\n", data.to_url));
                    }
                }
                ActionType::Select => {
                    if let Some(ActionData::Select(data)) = &action.action_data {
                        script.push_str(&format!(
                            "    page.locator(\"{}\").select_option(\"{}\")\n",
                            selector, data.value
                        ));
                    }
                }
                ActionType::Scroll => {
                    if let Some(ActionData::Scroll(data)) = &action.action_data {
                        script.push_str(&format!(
                            "    page.mouse.wheel({}, {})\n",
                            data.delta_x, data.delta_y
                        ));
                    }
                }
                ActionType::Keypress => {
                    if let Some(ActionData::Keypress(data)) = &action.action_data {
                        script.push_str(&format!("    page.keyboard.press(\"{}\")\n", data.key));
                    }
                }
                ActionType::Hover => {
                    script.push_str(&format!("    page.locator(\"{}\").hover()\n", selector));
                }
            }

            // Add visibility assertion if requested
            if options.include_visibility_assertions && action.action_type != ActionType::Navigate {
                script.push_str(&format!(
                    "    expect(page.locator(\"{}\")).to_be_visible()\n",
                    selector
                ));
            }

            // Add wait between actions
            if i < actions.len() - 1 {
                Self::add_pytest_wait(&mut script, options);
            }
        }

        Ok(script)
    }

    /// Generate Cypress test (placeholder)
    fn generate_cypress(
        recording: &Recording,
        actions: &[RecordedAction],
        options: &ExportOptions,
    ) -> Result<String, String> {
        let test_name = options
            .test_name
            .clone()
            .unwrap_or_else(|| recording.name.clone());

        let mut script = String::new();

        // Header
        script.push_str(
            r#"/**
 * Auto-generated Cypress test from recording.
 * Recording: "#,
        );
        script.push_str(&recording.name);
        script.push_str(&format!("\n * Base URL: {}", recording.base_url));
        script.push_str(&format!(
            "\n * Generated: {}",
            chrono::Utc::now().to_rfc3339()
        ));
        script.push_str(
            r#"
 */

describe('"#,
        );
        script.push_str(&test_name);
        script.push_str(
            r#"', () => {
  it('"#,
        );
        script.push_str(&test_name);
        script.push_str(
            r#"', () => {
    // Navigate to starting URL
    cy.visit('"#,
        );
        script.push_str(&recording.base_url);
        script.push_str("');\n");

        // Generate action code
        for (i, action) in actions.iter().enumerate() {
            script.push_str(&format!(
                "\n    // Action {}: {} on {}\n",
                i + 1,
                action.action_type,
                action.target.tag_name
            ));

            let selector = Self::get_best_selector(&action.target, options);

            match action.action_type {
                ActionType::Click => {
                    script.push_str(&format!("    cy.get('{}').click();\n", selector));
                }
                ActionType::Type => {
                    if let Some(ActionData::Type(data)) = &action.action_data {
                        if data.clear_first {
                            script.push_str(&format!("    cy.get('{}').clear();\n", selector));
                        }
                        script.push_str(&format!(
                            "    cy.get('{}').type('{}');\n",
                            selector,
                            data.value.replace('\'', "\\'")
                        ));
                    }
                }
                ActionType::Navigate => {
                    if let Some(ActionData::Navigate(data)) = &action.action_data {
                        script.push_str(&format!("    cy.visit('{}');\n", data.to_url));
                    }
                }
                ActionType::Select => {
                    if let Some(ActionData::Select(data)) = &action.action_data {
                        script.push_str(&format!(
                            "    cy.get('{}').select('{}');\n",
                            selector, data.value
                        ));
                    }
                }
                _ => {
                    script.push_str(&format!(
                        "    // {} action - implement as needed\n",
                        action.action_type
                    ));
                }
            }
        }

        script.push_str("  });\n});\n");

        Ok(script)
    }

    /// Get the best selector based on priority
    fn get_best_selector(target: &ActionTarget, options: &ExportOptions) -> String {
        for priority in &options.selector_priority {
            match priority.as_str() {
                "ui_id" => {
                    if let Some(ui_id) = &target.ui_id {
                        return format!("[data-testid=\"{}\"]", ui_id);
                    }
                }
                "css" => {
                    if !target.selector.is_empty() {
                        return target.selector.clone();
                    }
                }
                "xpath" => {
                    if let Some(xpath) = &target.xpath {
                        return xpath.clone();
                    }
                }
                "text" => {
                    if let Some(text) = &target.text_content {
                        if !text.is_empty() && text.len() < 50 {
                            return format!(":has-text(\"{}\")", text);
                        }
                    }
                }
                _ => {}
            }
        }

        // Fallback to CSS selector or tag name
        if !target.selector.is_empty() {
            target.selector.clone()
        } else {
            target.tag_name.clone()
        }
    }

    /// Get Playwright-optimized selector
    fn get_playwright_selector(target: &ActionTarget, options: &ExportOptions) -> String {
        // Playwright has special syntax for data attributes and text
        for priority in &options.selector_priority {
            match priority.as_str() {
                "ui_id" => {
                    if let Some(ui_id) = &target.ui_id {
                        return format!("[data-testid='{}']", ui_id);
                    }
                }
                "css" => {
                    if !target.selector.is_empty() {
                        return target.selector.replace('"', "'");
                    }
                }
                "xpath" => {
                    if let Some(xpath) = &target.xpath {
                        return format!("xpath={}", xpath);
                    }
                }
                "text" => {
                    if let Some(text) = &target.text_content {
                        if !text.is_empty() && text.len() < 50 {
                            return format!("text={}", text);
                        }
                    }
                }
                _ => {}
            }
        }

        // Fallback
        if !target.selector.is_empty() {
            target.selector.replace('"', "'")
        } else {
            target.tag_name.clone()
        }
    }

    /// Add Python wait statement
    fn add_python_wait(script: &mut String, options: &ExportOptions) {
        match options.wait_strategy.as_str() {
            "fixed" => {
                script.push_str(&format!(
                    "    time.sleep({:.1})\n",
                    options.fixed_wait_ms as f64 / 1000.0
                ));
            }
            "networkidle" | "domcontentloaded" | "load" => {
                // For Python client, we'd typically use a fixed wait as fallback
                script.push_str("    time.sleep(0.5)  # Wait for page to settle\n");
            }
            _ => {
                script.push_str("    time.sleep(0.5)\n");
            }
        }
    }

    /// Add Playwright wait statement
    fn add_playwright_wait(script: &mut String, options: &ExportOptions) {
        match options.wait_strategy.as_str() {
            "networkidle" => {
                script.push_str("    await page.waitForLoadState('networkidle');\n");
            }
            "domcontentloaded" => {
                script.push_str("    await page.waitForLoadState('domcontentloaded');\n");
            }
            "load" => {
                script.push_str("    await page.waitForLoadState('load');\n");
            }
            "fixed" => {
                script.push_str(&format!(
                    "    await page.waitForTimeout({});\n",
                    options.fixed_wait_ms
                ));
            }
            _ => {}
        }
    }

    /// Add pytest wait statement
    fn add_pytest_wait(script: &mut String, options: &ExportOptions) {
        match options.wait_strategy.as_str() {
            "networkidle" => {
                script.push_str("    page.wait_for_load_state(\"networkidle\")\n");
            }
            "domcontentloaded" => {
                script.push_str("    page.wait_for_load_state(\"domcontentloaded\")\n");
            }
            "load" => {
                script.push_str("    page.wait_for_load_state(\"load\")\n");
            }
            "fixed" => {
                script.push_str(&format!(
                    "    page.wait_for_timeout({})\n",
                    options.fixed_wait_ms
                ));
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_file_name() {
        let recording = Recording {
            id: "test".to_string(),
            name: "My Test Recording".to_string(),
            description: None,
            base_url: "https://example.com".to_string(),
            action_count: 0,
            status: RecordingStatus::Completed,
            started_at: "2024-01-01T00:00:00Z".to_string(),
            completed_at: None,
            duration_ms: None,
            browser_info: None,
            tab_id: None,
            tags: vec![],
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
        };

        assert_eq!(
            ScriptGenerator::default_file_name(&recording, ExportFormat::Python),
            "my_test_recording_automation.py"
        );
        assert_eq!(
            ScriptGenerator::default_file_name(&recording, ExportFormat::Playwright),
            "my_test_recording.spec.ts"
        );
        assert_eq!(
            ScriptGenerator::default_file_name(&recording, ExportFormat::Pytest),
            "test_my_test_recording.py"
        );
    }
}
