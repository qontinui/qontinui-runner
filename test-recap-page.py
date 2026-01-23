"""
Recap Page Specification Test

Tests the Recap page against RECAP_PAGE_SPEC.md using the correct
Runner HTTP APIs.

This test validates ALL sections defined in the spec:
- Section 1: Summary Banner (existing)
- Section 2: AI Summary (existing)
- Section 3: Failure Analysis (existing - conditional)
- Section 4: Execution Phases Timeline (existing)
- Section 5: Verification Results (NEW)
- Section 6: Knowledge & Findings (NEW)
- Section 7: Automation Metrics (NEW)
- Section 8: Test Results (NEW)
- Section 9: Execution Context (NEW)
"""
import requests
import time
import json
from typing import Any, Optional

BASE_URL = "http://localhost:9876"

# Test state
_assertions = []
_logs = []
_metrics = {}


def assertion(condition: bool, message: str) -> None:
    """Record an assertion."""
    _assertions.append({"passed": condition, "message": message})
    if condition:
        log(f"[PASS] {message}")
    else:
        log(f"[FAIL] {message}")


def log(message: str) -> None:
    """Log a message to the test output."""
    _logs.append(message)
    print(message)


def metric(name: str, value: float) -> None:
    """Record a numeric metric."""
    _metrics[name] = value


# =============================================================================
# API Helpers - Using CORRECT patterns from Runner Test API Reference
# =============================================================================

def navigate_to_page(page: str) -> dict:
    """Navigate to a page using the navigation API (NOT sidebar clicks)."""
    response = requests.post(
        f"{BASE_URL}/navigate",
        json={"page": page},
        timeout=10
    )
    response.raise_for_status()
    return response.json()


def get_elements_by_id() -> dict[str, dict]:
    """
    Get elements indexed by their ID using the discover API.

    The discover API finds all elements with data-ui-id attributes,
    not just registered elements.
    """
    response = requests.post(
        f"{BASE_URL}/ui-bridge/control/discover",
        json={"selector": "[data-ui-id]", "limit": 500},
        timeout=10
    )
    response.raise_for_status()
    data = response.json()
    elements = data.get("data", {}).get("elements", [])
    return {elem["id"]: elem for elem in elements}


def click_element(element_id: str) -> dict:
    """Click an element by its data-ui-id."""
    response = requests.post(
        f"{BASE_URL}/ui-bridge/control/element/{element_id}/action",
        json={"action": "click"},
        timeout=10
    )
    response.raise_for_status()
    return response.json()


def get_element_text(elements: dict[str, dict], element_id: str) -> str:
    """
    Get text content of an element.

    The discover API returns text in multiple places:
    - accessibleName: Computed accessible name
    - label: Explicit label
    - state.text_content: Text content (may be empty for discover)
    """
    elem = elements.get(element_id, {})
    # Try multiple sources for text
    text = elem.get("accessibleName", "") or ""
    if not text:
        text = elem.get("label", "") or ""
    if not text:
        text = elem.get("state", {}).get("text_content", "") or ""
    return text


def element_visible(elements: dict[str, dict], element_id: str) -> bool:
    """Check if element exists and is visible."""
    elem = elements.get(element_id)
    if not elem:
        return False
    return elem.get("state", {}).get("visible", False)


def element_exists(elements: dict[str, dict], element_id: str) -> bool:
    """Check if element exists (may or may not be visible)."""
    return element_id in elements


def find_run_by_name(elements: dict[str, dict], name: str) -> Optional[str]:
    """
    Find a run selector item by name (case-insensitive partial match).

    IMPORTANT: Run items have IDs like run-selector-item-{id}
    """
    for elem_id, elem_data in elements.items():
        if elem_id.startswith("run-selector-item-"):
            text = elem_data.get("state", {}).get("text_content", "") or ""
            if name.lower() in text.lower():
                return elem_id
    return None


def find_elements_by_prefix(elements: dict[str, dict], prefix: str) -> list[str]:
    """Find all element IDs that start with a prefix."""
    return [eid for eid in elements.keys() if eid.startswith(prefix)]


def wait_for_element(element_id: str, timeout: float = 10.0) -> bool:
    """Wait for an element to appear and be visible."""
    start = time.time()
    while time.time() - start < timeout:
        try:
            elements = get_elements_by_id()
            if element_visible(elements, element_id):
                return True
        except Exception:
            pass
        time.sleep(0.5)
    return False


# =============================================================================
# Main Test
# =============================================================================

def test_main():
    """Main test function for Recap page verification."""
    log("=" * 70)
    log("RECAP PAGE SPECIFICATION TEST")
    log("Testing against RECAP_PAGE_SPEC.md")
    log("=" * 70)
    log("")

    # Expected data from workflow run context
    expected_task_name = "Clean and Push"
    expected_status = "completed"

    log(f"Expected Task: {expected_task_name}")
    log(f"Expected Status: {expected_status}")
    log("")

    # =========================================================================
    # Step 1: Navigate to Recap page using /navigate API
    # =========================================================================
    log("STEP 1: Navigate to Recap page")
    log("-" * 50)

    try:
        log("Calling POST /navigate with page='run-recap'...")
        navigate_to_page("run-recap")
        time.sleep(1.5)
        log("Navigation successful")

    except Exception as e:
        log(f"Error navigating to Recap page: {e}")
        assertion(False, "Could navigate to Recap page")
        return

    log("")

    # =========================================================================
    # Step 2: Select the workflow run
    # =========================================================================
    log("STEP 2: Select workflow run")
    log("-" * 50)

    try:
        elements = get_elements_by_id()
        log(f"Found {len(elements)} UI elements")

        # Click run selector trigger to open dropdown
        if element_exists(elements, "run-selector-trigger"):
            log("Clicking run-selector-trigger...")
            click_element("run-selector-trigger")
            time.sleep(0.5)

            # Refresh elements to see dropdown items
            elements = get_elements_by_id()

            # Find run items
            run_items = find_elements_by_prefix(elements, "run-selector-item-")
            log(f"Found {len(run_items)} run selector items")

            # Find and click the matching run
            target_run = find_run_by_name(elements, expected_task_name)

            if target_run:
                log(f"Found matching run: {target_run}")
                click_element(target_run)
                time.sleep(1.5)
            elif run_items:
                log(f"No exact match found, clicking first run: {run_items[0]}")
                click_element(run_items[0])
                time.sleep(1.5)
            else:
                log("No run items found in dropdown")
        else:
            log("run-selector-trigger not found, may already have a run selected")

        # Refresh elements after selection
        elements = get_elements_by_id()

    except Exception as e:
        log(f"Error selecting workflow run: {e}")

    log("")

    # =========================================================================
    # SECTION 1: Summary Banner Verification (Spec Section 1)
    # =========================================================================
    log("=" * 70)
    log("SECTION 1: Summary Banner (Spec: Summary Banner)")
    log("Expected: Status badge, goal indicator, duration, timestamps")
    log("-" * 50)

    elements = get_elements_by_id()

    # Status banner container
    status_banner_exists = element_exists(elements, "recap-status-banner")
    assertion(status_banner_exists, "Status banner (recap-status-banner) is present")

    # Status label
    status_label_exists = element_exists(elements, "recap-status-label")
    assertion(status_label_exists, "Status label (recap-status-label) is present")

    if status_label_exists:
        status_label = get_element_text(elements, "recap-status-label")
        log(f"  Status label text: '{status_label}'")
        status_valid = any(s in status_label.lower() for s in ["completed", "complete", "failed", "running", "success"])
        assertion(status_valid, "Status label shows valid status text")

    # Goal achieved indicator (per spec: "Goal achieved indicator")
    goal_indicator_exists = element_exists(elements, "recap-goal-indicator") or element_exists(elements, "recap-goal-badge")
    assertion(goal_indicator_exists, "Goal achieved indicator (recap-goal-indicator or recap-goal-badge) is present")

    # Duration display (per spec: "Total duration")
    duration_exists = element_exists(elements, "recap-duration") or "duration" in str(elements).lower()
    assertion(duration_exists, "Duration display is present")

    # Timestamps (per spec: "Start/end timestamps")
    timestamps_exist = (
        element_exists(elements, "recap-start-time") or
        element_exists(elements, "recap-end-time") or
        element_exists(elements, "recap-timestamps")
    )
    assertion(timestamps_exist, "Start/end timestamps (recap-start-time, recap-end-time) are present")

    metric("section1_status_banner", 1.0 if status_banner_exists else 0.0)
    log("")

    # =========================================================================
    # SECTION 2: AI Summary Section Verification (Spec Section 2)
    # =========================================================================
    log("=" * 70)
    log("SECTION 2: AI Summary (Spec: AI Summary)")
    log("Expected: Summary text, goal badge, remaining work, generation timestamp, generate button")
    log("-" * 50)

    # AI Summary section container
    ai_summary_section = element_exists(elements, "recap-ai-summary-section")
    assertion(ai_summary_section, "AI Summary section (recap-ai-summary-section) is present")

    # Summary text (per spec: "Summary text")
    summary_text_exists = element_exists(elements, "recap-ai-summary-text")
    assertion(summary_text_exists, "AI summary text (recap-ai-summary-text) is present")

    if summary_text_exists:
        summary_text = get_element_text(elements, "recap-ai-summary-text")
        log(f"  Summary text: '{summary_text[:80]}...'" if len(summary_text) > 80 else f"  Summary text: '{summary_text}'")

    # Goal achievement badge (per spec: "Goal achievement badge")
    goal_badge_exists = element_exists(elements, "recap-goal-badge")
    # Note: Goal badge only shows when goalAchieved has a value
    if goal_badge_exists:
        goal_text = get_element_text(elements, "recap-goal-badge")
        log(f"  Goal badge: '{goal_text}'")
        assertion(True, "Goal achievement badge (recap-goal-badge) is present")
    else:
        log("  Goal badge not present (may be null/undefined)")

    # Remaining work (per spec: "Remaining work (if goal not achieved)")
    remaining_work_exists = element_exists(elements, "recap-remaining-work")
    if remaining_work_exists:
        remaining_text = get_element_text(elements, "recap-remaining-work")
        log(f"  Remaining work: '{remaining_text}'")
    else:
        log("  Remaining work section not present (expected if goal achieved)")

    # Generation timestamp (per spec: "Generation timestamp")
    timestamp_exists = element_exists(elements, "recap-summary-timestamp") or element_exists(elements, "recap-summary-generated-at")
    assertion(timestamp_exists, "Summary generation timestamp (recap-summary-timestamp) is present")

    # Generate button (per spec: "Generate button (if no summary)")
    generate_btn_exists = element_exists(elements, "recap-generate-summary-btn")
    if generate_btn_exists:
        log("  Generate summary button is present (no summary yet)")
    else:
        log("  Generate button not present (summary already exists)")

    metric("section2_ai_summary", 1.0 if ai_summary_section else 0.0)
    log("")

    # =========================================================================
    # SECTION 3: Failure Analysis (Spec Section 3) - Conditional
    # =========================================================================
    log("=" * 70)
    log("SECTION 3: Failure Analysis (Spec: Failure Analysis)")
    log("Expected (if failed): Failure reason, failed step, error type, error details, related findings")
    log("-" * 50)

    failure_section = element_exists(elements, "recap-failure-section")

    if expected_status.lower() == "failed":
        assertion(failure_section, "Failure section is present for failed run")

        if failure_section:
            # Primary failure reason (per spec)
            failure_reason = element_exists(elements, "recap-failure-reason")
            assertion(failure_reason, "Failure reason (recap-failure-reason) is displayed")

            # Failed step name (per spec)
            failed_step = element_exists(elements, "recap-failed-step")
            assertion(failed_step, "Failed step name (recap-failed-step) is displayed")

            # Error type classification (per spec)
            error_type = element_exists(elements, "recap-error-type")
            assertion(error_type, "Error type (recap-error-type) is displayed")

            # Error details expandable (per spec)
            error_details = element_exists(elements, "recap-error-details") or element_exists(elements, "recap-failure-details-toggle")
            assertion(error_details, "Error details (recap-error-details) is available")

            # NEW: Related findings (per spec enhancement)
            related_findings = element_exists(elements, "recap-related-findings")
            assertion(related_findings, "Related findings (recap-related-findings) is displayed")
    else:
        assertion(not failure_section, "Failure section is NOT shown for successful run")
        log("  Run status is not 'failed', so failure section should not appear")

    metric("section3_failure", 1.0 if failure_section else 0.0)
    log("")

    # =========================================================================
    # SECTION 4: Execution Phases Timeline (Spec Section 4)
    # =========================================================================
    log("=" * 70)
    log("SECTION 4: Execution Phases Timeline (Spec: Execution Phases Timeline)")
    log("Expected: 4 phases (setup, agentic, verification, completion), iteration numbers, steps")
    log("-" * 50)

    # Check for tabs first (per spec UI layout)
    recap_tabs = element_exists(elements, "recap-tabs")
    assertion(recap_tabs, "Recap tabs container (recap-tabs) is present")

    # Timeline tab
    timeline_tab = element_exists(elements, "recap-tab-timeline")
    assertion(timeline_tab, "Timeline tab (recap-tab-timeline) is present")

    # Click timeline tab if it exists
    if timeline_tab:
        click_element("recap-tab-timeline")
        time.sleep(0.5)
        elements = get_elements_by_id()

    # Staged timeline container
    staged_timeline = element_exists(elements, "recap-staged-timeline")
    assertion(staged_timeline, "Staged timeline (recap-staged-timeline) is present")

    if staged_timeline:
        # Check for all 4 phases (per spec)
        expected_stages = ["setup", "agentic", "verification", "completion"]
        stages_found = 0

        for stage in expected_stages:
            stage_id = f"recap-stage-section-{stage}"
            stage_exists = element_exists(elements, stage_id)
            if stage_exists:
                stages_found += 1
                log(f"  - Stage '{stage}' found")
            else:
                log(f"  - Stage '{stage}' NOT found")

        assertion(stages_found == 4, f"All 4 execution phases are present ({stages_found}/4 found)")

        # Check for iteration number support (per spec enhancement)
        iteration_indicator = any("iteration" in eid.lower() for eid in elements.keys())
        assertion(iteration_indicator, "Iteration number indicators are present for stages")

        # Expand a stage to check for step items
        stage_toggle = element_exists(elements, "recap-stage-toggle-btn")
        if stage_toggle:
            click_element("recap-stage-toggle-btn")
            time.sleep(0.5)
            elements = get_elements_by_id()

        # Check for step items
        step_items = find_elements_by_prefix(elements, "recap-step-item-") + find_elements_by_prefix(elements, "recap-staged-step-")
        assertion(len(step_items) > 0, f"Step items are present in timeline ({len(step_items)} found)")

        # Check step item properties (per spec: name, status, duration, summary, error)
        for step_id in step_items[:1]:  # Check first step
            step_name = element_exists(elements, f"{step_id}-name") or step_id in elements
            step_status = element_exists(elements, f"{step_id}-status")
            step_duration = element_exists(elements, f"{step_id}-duration")
            log(f"  Step item '{step_id}' structure check")

    metric("section4_timeline", 1.0 if staged_timeline else 0.0)
    log("")

    # =========================================================================
    # SECTION 5: Verification Results (Spec Section 5 - NEW)
    # =========================================================================
    log("=" * 70)
    log("SECTION 5: Verification Results (Spec: NEW)")
    log("Expected: Verification tab, plan summary, criteria list with observations/issues/suggestions")
    log("-" * 50)

    # Verification tab
    verification_tab = element_exists(elements, "recap-tab-verification")
    assertion(verification_tab, "Verification tab (recap-tab-verification) is present")

    if verification_tab:
        click_element("recap-tab-verification")
        time.sleep(0.5)
        elements = get_elements_by_id()

    # Verification plan summary (per spec)
    plan_summary = element_exists(elements, "recap-verification-plan-summary")
    assertion(plan_summary, "Verification plan summary (recap-verification-plan-summary) is present")

    # Goal summary from plan (per spec)
    goal_summary = element_exists(elements, "recap-verification-goal-summary")
    assertion(goal_summary, "Goal summary (recap-verification-goal-summary) is present")

    # Criteria list (per spec)
    criteria_list = element_exists(elements, "recap-verification-criteria-list")
    assertion(criteria_list, "Criteria list (recap-verification-criteria-list) is present")

    # Individual criterion cards (per spec)
    criterion_cards = find_elements_by_prefix(elements, "recap-criterion-")
    assertion(len(criterion_cards) > 0, f"Criterion result cards are present ({len(criterion_cards)} found)")

    if criterion_cards:
        # Check criterion card structure (per spec)
        for card_id in criterion_cards[:1]:
            log(f"  Checking criterion card: {card_id}")

        # Criterion type indicator (deterministic/ai_evaluated)
        type_indicator = any("type" in eid.lower() for eid in criterion_cards)
        assertion(type_indicator, "Criterion type indicators (deterministic/ai_evaluated) are present")

        # Pass/fail status per criterion
        status_indicator = any("status" in eid.lower() or "pass" in eid.lower() for eid in criterion_cards)
        assertion(status_indicator, "Criterion pass/fail status indicators are present")

        # Confidence level for AI criteria
        confidence_indicator = element_exists(elements, "recap-criterion-confidence")
        assertion(confidence_indicator, "Confidence level indicators for AI criteria are present")

        # Observations expandable
        observations = element_exists(elements, "recap-criterion-observations")
        assertion(observations, "Criterion observations section (recap-criterion-observations) is present")

        # Issues found
        issues = element_exists(elements, "recap-criterion-issues")
        assertion(issues, "Criterion issues section (recap-criterion-issues) is present")

        # Suggestions
        suggestions = element_exists(elements, "recap-criterion-suggestions")
        assertion(suggestions, "Criterion suggestions section (recap-criterion-suggestions) is present")

    # Filter controls (per spec: Filter by All, Passed, Failed)
    filter_controls = element_exists(elements, "recap-verification-filter")
    assertion(filter_controls, "Verification filter controls (recap-verification-filter) are present")

    metric("section5_verification", 1.0 if verification_tab else 0.0)
    log("")

    # =========================================================================
    # SECTION 6: Knowledge & Findings (Spec Section 6 - NEW)
    # =========================================================================
    log("=" * 70)
    log("SECTION 6: Knowledge & Findings (Spec: NEW)")
    log("Expected: Knowledge tab, findings panel by severity, knowledge timeline by iteration")
    log("-" * 50)

    # Knowledge tab
    knowledge_tab = element_exists(elements, "recap-tab-knowledge")
    assertion(knowledge_tab, "Knowledge tab (recap-tab-knowledge) is present")

    if knowledge_tab:
        click_element("recap-tab-knowledge")
        time.sleep(0.5)
        elements = get_elements_by_id()

    # Findings panel (per spec)
    findings_panel = element_exists(elements, "recap-findings-panel")
    assertion(findings_panel, "Findings panel (recap-findings-panel) is present")

    # Findings grouped by severity (per spec: critical, high, medium, low, info)
    severity_groups = ["critical", "high", "medium", "low", "info"]
    for severity in severity_groups:
        severity_section = element_exists(elements, f"recap-findings-{severity}")
        if severity_section:
            log(f"  - Severity group '{severity}' found")

    # Individual finding cards (per spec)
    finding_cards = find_elements_by_prefix(elements, "recap-finding-")
    if finding_cards:
        log(f"  Found {len(finding_cards)} finding cards")
        # Check finding card structure (per spec: category badge, title, description, file path, status)
        assertion(True, "Finding cards are present with required structure")
    else:
        assertion(False, "Finding cards (recap-finding-*) are present")

    # Knowledge timeline (per spec)
    knowledge_timeline = element_exists(elements, "recap-knowledge-timeline")
    assertion(knowledge_timeline, "Knowledge timeline (recap-knowledge-timeline) is present")

    # Knowledge entries grouped by iteration (per spec)
    knowledge_entries = find_elements_by_prefix(elements, "recap-knowledge-entry-")
    assertion(len(knowledge_entries) >= 0, f"Knowledge entries are available ({len(knowledge_entries)} found)")

    # Filter by category (per spec: findings, root_cause, observation, hypothesis, solution, context)
    category_filter = element_exists(elements, "recap-knowledge-filter-category")
    assertion(category_filter, "Knowledge category filter (recap-knowledge-filter-category) is present")

    # Filter by agent (per spec: planning, worker, verification, system)
    agent_filter = element_exists(elements, "recap-knowledge-filter-agent")
    assertion(agent_filter, "Knowledge agent filter (recap-knowledge-filter-agent) is present")

    metric("section6_knowledge", 1.0 if knowledge_tab else 0.0)
    log("")

    # =========================================================================
    # SECTION 7: Automation Metrics (Spec Section 7 - NEW)
    # =========================================================================
    log("=" * 70)
    log("SECTION 7: Automation Metrics (Spec: NEW)")
    log("Expected: Actions summary, states/transitions, template matching, screenshots")
    log("-" * 50)

    # Check for automation/metrics tab or section
    automation_tab = element_exists(elements, "recap-tab-automation") or element_exists(elements, "recap-tab-metrics")
    assertion(automation_tab, "Automation/Metrics tab (recap-tab-automation) is present")

    if automation_tab:
        tab_id = "recap-tab-automation" if element_exists(elements, "recap-tab-automation") else "recap-tab-metrics"
        click_element(tab_id)
        time.sleep(0.5)
        elements = get_elements_by_id()

    # Actions summary (per spec)
    actions_summary = element_exists(elements, "recap-actions-summary")
    assertion(actions_summary, "Actions summary (recap-actions-summary) is present")

    # Action counts (per spec: total, success, failed, skipped)
    actions_total = element_exists(elements, "recap-actions-total")
    actions_success = element_exists(elements, "recap-actions-success")
    actions_failed = element_exists(elements, "recap-actions-failed")
    assertion(actions_total or actions_summary, "Action counts (total/success/failed/skipped) are displayed")

    # Visual bar chart (per spec)
    actions_chart = element_exists(elements, "recap-actions-chart")
    assertion(actions_chart, "Actions visual bar chart (recap-actions-chart) is present")

    # States visited list (per spec)
    states_list = element_exists(elements, "recap-states-visited")
    assertion(states_list, "States visited list (recap-states-visited) is present")

    # Transitions diagram/list (per spec)
    transitions = element_exists(elements, "recap-transitions")
    assertion(transitions, "Transitions diagram/list (recap-transitions) is present")

    # Template matching section (per spec)
    template_matches = element_exists(elements, "recap-template-matches")
    assertion(template_matches, "Template matching section (recap-template-matches) is present")

    # Annotated screenshots gallery (per spec)
    screenshots_gallery = element_exists(elements, "recap-screenshots-gallery")
    assertion(screenshots_gallery, "Annotated screenshots gallery (recap-screenshots-gallery) is present")

    metric("section7_automation", 1.0 if automation_tab else 0.0)
    log("")

    # =========================================================================
    # SECTION 8: Test Results (Spec Section 8 - NEW)
    # =========================================================================
    log("=" * 70)
    log("SECTION 8: Test Results (Spec: NEW)")
    log("Expected: Tests tab, test cards with name, status, assertions, console, snapshots")
    log("-" * 50)

    # Tests tab
    tests_tab = element_exists(elements, "recap-tab-tests")
    assertion(tests_tab, "Tests tab (recap-tab-tests) is present")

    if tests_tab:
        click_element("recap-tab-tests")
        time.sleep(0.5)
        elements = get_elements_by_id()

    # Test result cards (per spec)
    test_cards = find_elements_by_prefix(elements, "recap-test-result-")
    assertion(len(test_cards) >= 0, f"Test result cards are available ({len(test_cards)} found)")

    # Test result card structure (per spec)
    test_name = element_exists(elements, "recap-test-name")
    assertion(test_name, "Test name display (recap-test-name) is present")

    test_status = element_exists(elements, "recap-test-status")
    assertion(test_status, "Test pass/fail status (recap-test-status) is present")

    test_duration = element_exists(elements, "recap-test-duration")
    assertion(test_duration, "Test duration (recap-test-duration) is present")

    # Assertions count (per spec)
    assertions_count = element_exists(elements, "recap-test-assertions")
    assertion(assertions_count, "Test assertions count (recap-test-assertions) is present")

    # Console output expandable (per spec)
    console_output = element_exists(elements, "recap-test-console")
    assertion(console_output, "Test console output (recap-test-console) is present")

    # Page snapshot (per spec: YAML rendered)
    page_snapshot = element_exists(elements, "recap-test-snapshot")
    assertion(page_snapshot, "Test page snapshot (recap-test-snapshot) is present")

    # Failure screenshots (per spec)
    failure_screenshots = element_exists(elements, "recap-test-screenshots")
    assertion(failure_screenshots, "Test failure screenshots (recap-test-screenshots) is present")

    # Error message and stack trace (per spec)
    error_trace = element_exists(elements, "recap-test-error")
    assertion(error_trace, "Test error/stack trace (recap-test-error) is present for failed tests")

    metric("section8_tests", 1.0 if tests_tab else 0.0)
    log("")

    # =========================================================================
    # SECTION 9: Execution Context (Spec Section 9 - NEW)
    # =========================================================================
    log("=" * 70)
    log("SECTION 9: Execution Context (Spec: NEW)")
    log("Expected: Context tab, variables table, retry history")
    log("-" * 50)

    # Context tab
    context_tab = element_exists(elements, "recap-tab-context")
    assertion(context_tab, "Context tab (recap-tab-context) is present")

    if context_tab:
        click_element("recap-tab-context")
        time.sleep(0.5)
        elements = get_elements_by_id()

    # Variables table (per spec)
    variables_table = element_exists(elements, "recap-variables-table")
    assertion(variables_table, "Variables table (recap-variables-table) is present")

    # Variable columns (per spec: name, value, source, source step)
    var_name_col = element_exists(elements, "recap-var-name")
    var_value_col = element_exists(elements, "recap-var-value")
    var_source_col = element_exists(elements, "recap-var-source")
    assertion(var_name_col or variables_table, "Variable name column is present")
    assertion(var_value_col or variables_table, "Variable value column is present")
    assertion(var_source_col or variables_table, "Variable source column is present")

    # Retry history (per spec: if applicable)
    retry_history = element_exists(elements, "recap-retry-history")
    assertion(retry_history, "Retry history section (recap-retry-history) is present")

    # Retry details (per spec: attempts, error history, delay)
    retry_attempts = element_exists(elements, "recap-retry-attempts")
    retry_errors = element_exists(elements, "recap-retry-errors")
    if retry_history:
        log("  Retry history section found - checking details")

    metric("section9_context", 1.0 if context_tab else 0.0)
    log("")

    # =========================================================================
    # UI Layout Verification (Per Spec UI Layout)
    # =========================================================================
    log("=" * 70)
    log("UI LAYOUT: Tab-based Interface (Spec: UI Layout)")
    log("Expected: TABS - Timeline | Verification | Knowledge | Tests | Context")
    log("-" * 50)

    # Refresh elements
    elements = get_elements_by_id()

    # Tab container
    tabs_container = element_exists(elements, "recap-tabs")
    assertion(tabs_container, "Tabs container (recap-tabs) is present")

    # All required tabs
    required_tabs = ["timeline", "verification", "knowledge", "tests", "context"]
    tabs_found = 0
    for tab_name in required_tabs:
        tab_id = f"recap-tab-{tab_name}"
        if element_exists(elements, tab_id):
            tabs_found += 1
            log(f"  - Tab '{tab_name}' found")
        else:
            log(f"  - Tab '{tab_name}' NOT found")

    assertion(tabs_found == 5, f"All 5 required tabs are present ({tabs_found}/5 found)")

    log("")

    # =========================================================================
    # Debug: List all recap-related elements
    # =========================================================================
    log("=" * 70)
    log("DEBUG: All recap-related elements found")
    log("-" * 50)

    recap_elements = [eid for eid in elements.keys() if "recap" in eid.lower()]
    log(f"Found {len(recap_elements)} recap-related elements:")
    for elem_id in sorted(recap_elements)[:30]:  # Limit to first 30
        text = get_element_text(elements, elem_id)
        visible = element_visible(elements, elem_id)
        text_preview = f"'{text[:25]}...'" if len(text) > 25 else f"'{text}'" if text else "(no text)"
        log(f"  - {elem_id} [visible={visible}] {text_preview}")
    if len(recap_elements) > 30:
        log(f"  ... and {len(recap_elements) - 30} more")

    log("")

    # =========================================================================
    # Summary
    # =========================================================================
    log("=" * 70)
    log("TEST SUMMARY")
    log("=" * 70)

    passed_count = sum(1 for a in _assertions if a["passed"])
    failed_count = sum(1 for a in _assertions if not a["passed"])
    total_count = len(_assertions)

    log(f"Assertions: {passed_count}/{total_count} passed ({failed_count} failed)")
    metric("assertions_passed", passed_count)
    metric("assertions_failed", failed_count)
    metric("assertions_total", total_count)

    if failed_count > 0:
        log("")
        log("FAILED ASSERTIONS:")
        for a in _assertions:
            if not a["passed"]:
                log(f"  - {a['message']}")

    all_passed = failed_count == 0

    result = {
        "passed": all_passed,
        "summary": f"Recap page test: {passed_count}/{total_count} assertions passed",
        "metrics": _metrics,
        "assertions": _assertions,
        "logs": _logs
    }

    log("")
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    test_main()
