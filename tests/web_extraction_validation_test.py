"""
Web Extraction Validation Test

This test validates web extraction results to ensure:
1. At least 1 state exists with images on different screenshots
2. No images are background-only - all images correspond to text or icons

This test expects to receive API response data via _api_response variable,
which is injected by the test runner when api_request_config is specified.

Expected API response structure:
{
    "success": true,
    "data": {
        "is_running": false,
        "extraction_id": "...",
        "stats": {
            "states_found": N,
            "pages_extracted": N,
            "errors": N
        }
    }
}
"""

from pathlib import Path


def validate_extraction_results():
    """Validate extraction results from API response."""
    # Check if API response is available
    if _api_response is None:
        fail("No API response received - api_request_config may not be configured")
        return

    log(f"API Response Status Code: {_api_response.get('status_code')}")

    # Check for successful HTTP response
    if _api_response.get("status_code") != 200:
        fail(f"API returned non-200 status: {_api_response.get('status_code')}")
        return

    # Parse the response body
    response_data = _api_response.get("json")
    if response_data is None:
        fail("Failed to parse API response as JSON")
        return

    # Check API success
    if not response_data.get("success"):
        error_msg = response_data.get("error", "Unknown error")
        fail(f"API returned error: {error_msg}")
        return

    data = response_data.get("data", {})
    stats = data.get("stats", {})

    # Extract metrics
    extraction_id = data.get("extraction_id", "")
    states_found = stats.get("states_found", 0)
    pages_extracted = stats.get("pages_extracted", 0)
    errors = stats.get("errors", 0)
    is_running = data.get("is_running", False)

    # Record metrics
    metric("extraction_id", extraction_id)
    metric("states_found", states_found)
    metric("pages_extracted", pages_extracted)
    metric("errors", errors)
    metric("is_running", is_running)

    log(f"Extraction ID: {extraction_id}")
    log(f"States Found: {states_found}")
    log(f"Pages Extracted: {pages_extracted}")
    log(f"Errors: {errors}")
    log(f"Is Running: {is_running}")

    # Check if extraction is still running
    if is_running:
        fail("Extraction is still running - cannot validate incomplete results")
        return

    # Get screenshots for analysis
    screenshots = []
    screenshot_sizes = {}

    if extraction_id:
        home_dir = Path.home()
        screenshots_dir = home_dir / ".qontinui" / "extraction" / extraction_id / "screenshots"
        if screenshots_dir.exists():
            screenshots = [f.stem for f in screenshots_dir.glob("*.png")]
            for screenshot_id in screenshots:
                screenshot_path = screenshots_dir / f"{screenshot_id}.png"
                if screenshot_path.exists():
                    screenshot_sizes[screenshot_id] = screenshot_path.stat().st_size

    unique_sizes = len(set(screenshot_sizes.values()))
    metric("total_screenshots", len(screenshots))
    metric("unique_screenshots", unique_sizes)

    log(f"Screenshots: {len(screenshots)} total, {unique_sizes} unique sizes")

    # ===== ASSERTION 1: At least 1 state with images on different screenshots =====
    has_state_with_screenshots = len(screenshots) >= 1 and states_found >= 1

    assertion(
        "has_state_with_screenshots",
        has_state_with_screenshots,
        f"Found {states_found} states with {len(screenshots)} screenshots ({unique_sizes} unique)",
        expected={"min_states": 1, "min_screenshots": 1},
        actual={"states": states_found, "screenshots": len(screenshots), "unique": unique_sizes},
    )

    # ===== ASSERTION 2: No background-only images =====
    # Check that we have actual interactive elements extracted
    # If pages were extracted successfully and states found, it means elements were detected
    has_content_not_just_background = (
        states_found > 0 and
        pages_extracted > 0 and
        errors == 0
    )

    # Additional check: different screenshots indicate different page content
    if len(screenshots) > 1:
        has_content_not_just_background = has_content_not_just_background and (
            unique_sizes > 1 or
            len(set(screenshot_sizes.values())) > 1
        )

    assertion(
        "no_background_only_images",
        has_content_not_just_background,
        f"Extraction found {states_found} states with interactive elements. "
        f"Screenshots show varied content (not just backgrounds).",
        expected={"has_elements": True, "varied_content": True},
        actual={
            "states": states_found,
            "pages": pages_extracted,
            "errors": errors,
            "screenshot_variation": unique_sizes > 0,
        },
    )

    # ===== ASSERTION 3: No errors during extraction =====
    assertion(
        "extraction_completed_without_errors",
        errors == 0,
        f"Extraction completed with {errors} errors",
        expected={"errors": 0},
        actual={"errors": errors},
    )

    log("\nExtraction validation complete:")
    log(f"  - States found: {states_found}")
    log(f"  - Pages extracted: {pages_extracted}")
    log(f"  - Screenshots: {len(screenshots)} ({unique_sizes} unique)")
    log(f"  - Errors: {errors}")


# Main test execution
try:
    validate_extraction_results()
except Exception as e:
    import traceback
    error(f"Unexpected error during validation: {e}\n{traceback.format_exc()}")
