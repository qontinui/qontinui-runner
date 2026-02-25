/**
 * Workflow Generation Templates
 *
 * Pre-built prompt templates for the AI Generate panel. These fill the
 * description textarea with a structured prompt that guides the AI to
 * generate a specific type of workflow. Users edit the [PLACEHOLDER]
 * values before hitting Generate.
 */

// =============================================================================
// Types
// =============================================================================

export interface WorkflowGenerationTemplate {
  id: string;
  name: string;
  description: string;
  icon: string;
  content: string;
  advancedDefaults?: {
    discoveryMode?: "auto" | "enabled" | "disabled";
    category?: string;
    tags?: string;
  };
}

// =============================================================================
// Templates
// =============================================================================

export const GENERATION_TEMPLATES: WorkflowGenerationTemplate[] = [
  {
    id: "compare-two-apps",
    name: "Compare Two Live Apps",
    icon: "GitCompare",
    description: "Snapshot two running apps via UI Bridge and compare their structure",
    content: `Compare two running applications side-by-side using UI Bridge snapshots.

## Reference App
- URL: [REFERENCE_APP_URL, e.g. http://localhost:3000]
- Description: [Brief description of the reference/expected app]

## Target App
- URL: [TARGET_APP_URL, e.g. http://localhost:3001]
- Description: [Brief description of the target app to compare]

## Comparison Focus
- [What to compare: layout, content, element presence, text accuracy, navigation structure]

## Instructions
1. Connect to both apps via UI Bridge
2. Snapshot the reference app on each relevant route
3. Snapshot the target app on the same routes
4. Compare structural and visual differences
5. Generate verification steps for each finding (missing elements, text mismatches, layout issues)
6. Categorize findings by severity (critical / major / minor / info)`,
    advancedDefaults: {
      discoveryMode: "enabled",
      category: "comparison",
    },
  },
  {
    id: "compare-against-baseline",
    name: "Compare Against Baseline",
    icon: "GitCompare",
    description: "Compare a running app against a known-good description or spec",
    content: `Compare a running application's current state against a known-good baseline.

## Application Under Test
- URL: [APP_URL, e.g. http://localhost:3000]
- Routes to check: [ROUTES, e.g. /, /dashboard, /settings]

## Expected Baseline
Describe what the application should look like:
- [Expected elements, layout, content on each route]
- [Key UI components that must be present]
- [Text content that must match exactly]
- [Navigation links and their targets]

## Comparison Focus
- [What matters most: element presence, text accuracy, layout, functionality]

## Instructions
1. Connect to the running app via UI Bridge
2. Navigate to each specified route and take a snapshot
3. Compare the actual state against the baseline description above
4. Report any deviations as findings with severity levels
5. Generate verification steps to continuously monitor these expectations`,
    advancedDefaults: {
      discoveryMode: "enabled",
      category: "comparison",
    },
  },
  {
    id: "full-app-review",
    name: "Full App UI Review",
    icon: "Globe",
    description: "Comprehensive review of all pages — find issues, gaps, and improvements",
    content: `Perform a comprehensive UI review of a running application.

## Application
- URL: [APP_URL, e.g. http://localhost:3000]
- Type: [web / desktop]
- Key routes: [LIST_ROUTES, or "discover all accessible routes"]

## Review Checklist
Check each page for:
- Missing or broken UI elements
- Layout and alignment issues
- Empty states with no guidance
- Broken links or navigation dead-ends
- Form inputs without labels or validation feedback
- Inconsistent styling or spacing
- Accessibility gaps (missing aria labels, low contrast, no keyboard nav)
- Loading states and error handling
- Responsive layout issues

## Focus Areas (optional)
- [Specific areas of concern, or leave blank for full review]

## Instructions
1. Connect via UI Bridge and discover all accessible routes
2. Snapshot each route and analyze the element tree
3. For each issue found, note the route, element, and severity
4. Generate verification steps that check for each issue
5. Organize findings by route, then by severity`,
    advancedDefaults: {
      discoveryMode: "enabled",
      category: "review",
    },
  },
  {
    id: "regression-test-suite",
    name: "Regression Test Suite",
    icon: "TestTube2",
    description:
      "Run all test commands for a project, collect failures, and create verification steps",
    content: `Generate a workflow that runs all test suites for a project and creates verification steps for each failure.

## Project
- Path: [PROJECT_PATH, e.g. C:\\Users\\me\\myproject]
- Language/Framework: [LANGUAGE, e.g. TypeScript/React, Python/FastAPI, Rust]

## Test Commands
List the test commands to run (or leave blank to auto-detect):
- [e.g., npm test, pytest, cargo test]

## Instructions
1. Discover available test commands in the project (package.json scripts, pyproject.toml, Cargo.toml)
2. Run each test command and capture output
3. Parse test results — identify passing and failing tests
4. For each failure, create a verification step that:
   - Runs the specific failing test
   - Checks the exit code and output for the expected pass result
5. Group verification steps by test suite / module
6. Add a final summary step that reports overall pass/fail counts`,
    advancedDefaults: {
      category: "testing",
    },
  },
  {
    id: "api-health-check",
    name: "API Health Check",
    icon: "Activity",
    description: "Hit a list of API endpoints, verify response codes and shapes, report failures",
    content: `Generate a workflow that checks the health of API endpoints and verifies their responses.

## API Base URL
- URL: [BASE_URL, e.g. http://localhost:8000]
- Auth: [AUTH_METHOD, e.g. none, Bearer token, API key header]

## Endpoints to Check
List endpoints (or leave blank to auto-discover from OpenAPI/swagger):
- [GET /health — expect 200]
- [GET /api/v1/users — expect 200, array response]
- [POST /api/v1/auth/login — expect 200 with token field]

## Verification Rules
- [Expected status codes per endpoint]
- [Required response fields or shapes]
- [Maximum acceptable response time in ms]

## Instructions
1. If no endpoints listed, try to discover them from OpenAPI spec at /docs or /openapi.json
2. For each endpoint, send the appropriate HTTP request
3. Verify the response status code matches expectations
4. Validate the response body shape (required fields, types)
5. Measure response time and flag slow endpoints
6. Create a verification step for each endpoint check
7. Generate a summary with overall API health status`,
    advancedDefaults: {
      category: "monitoring",
    },
  },
  {
    id: "cross-browser-comparison",
    name: "Cross-Browser Comparison",
    icon: "Monitor",
    description:
      "Open an app in multiple browsers via Playwright, screenshot each, compare for differences",
    content: `Generate a workflow that compares an application's rendering across multiple browsers.

## Application
- URL: [APP_URL, e.g. http://localhost:3000]
- Routes to compare: [ROUTES, e.g. /, /dashboard, /settings, /login]

## Browsers
- [chromium, firefox, webkit] (Playwright browser engines)

## Comparison Focus
- Layout differences (element positioning, sizing)
- Font rendering variations
- Color and styling inconsistencies
- Missing or broken elements in specific browsers
- Responsive breakpoints: [WIDTHS, e.g. 1920, 1280, 768, 375]

## Instructions
1. Launch each browser engine via Playwright
2. Navigate to each route at each viewport width
3. Take a full-page screenshot in each browser
4. Compare screenshots across browsers for the same route/width
5. Identify visual differences and categorize by severity
6. Create verification steps that check each finding
7. Organize results by route, then by browser pair`,
    advancedDefaults: {
      discoveryMode: "enabled",
      category: "comparison",
    },
  },
  {
    id: "smoke-test-after-deploy",
    name: "Smoke Test After Deploy",
    icon: "Rocket",
    description: "Post-deployment verification: check critical user flows still work",
    content: `Generate a workflow that verifies critical user flows work correctly after a deployment.

## Application
- URL: [APP_URL, e.g. https://staging.myapp.com]
- Environment: [ENV, e.g. staging, production]

## Critical Flows to Verify
1. **Authentication**
   - Login with valid credentials → lands on dashboard
   - Invalid credentials → shows error message
   - Logout → redirects to login page

2. **Navigation**
   - All main nav links load without errors
   - Breadcrumbs and back navigation work
   - [Add specific routes critical to your app]

3. **Key Forms**
   - [FORM_1, e.g. User profile update — fill fields, submit, verify saved]
   - [FORM_2, e.g. Search — enter query, verify results appear]

4. **Data Display**
   - [PAGE_1, e.g. Dashboard — verify charts/widgets load with data]
   - [PAGE_2, e.g. List views — verify items render, pagination works]

## Instructions
1. Connect to the deployed application via UI Bridge or Playwright
2. Execute each critical flow in sequence
3. For each step, verify the expected outcome (page loads, elements present, data displayed)
4. If a flow fails, capture a screenshot and error details
5. Create verification steps for each flow checkpoint
6. Generate a deployment health report with pass/fail per flow`,
    advancedDefaults: {
      category: "testing",
    },
  },
];
