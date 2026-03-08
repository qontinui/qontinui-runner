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
    includeDesignGuidance?: boolean;
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
    id: "multi-phase-plan",
    name: "Multi-Phase Plan",
    icon: "ListOrdered",
    description: "Sequential implementation with distinct research, build, and test phases",
    content: `Create a multi-stage workflow that breaks this task into sequential phases, each with its own verification.

## Task Description
[DESCRIBE_THE_TASK — what needs to be accomplished end-to-end]

## Phases (adjust as needed)
1. **Research / Analysis**
   - [What to investigate or discover before starting implementation]
   - Verification: [How to confirm research is complete — e.g., output file exists, key findings documented]

2. **Implementation**
   - [What to build, modify, or configure]
   - Verification: [How to confirm implementation works — e.g., tests pass, build succeeds, endpoint responds]

3. **Testing / Validation**
   - [What to test beyond basic implementation checks]
   - Verification: [How to confirm quality — e.g., integration tests, UI checks, performance benchmarks]

## Instructions
1. Create a multi-stage workflow using the \`stages\` array
2. Each phase becomes a stage with its own setup, verification, agentic, and completion steps
3. Assess difficulty per stage and set provider/model accordingly:
   - Simple stages (checks, file ops): use faster model
   - Complex stages (implementation, debugging): use capable model
4. Every stage MUST have deterministic verification steps
5. Later stages can reference output from earlier stages
6. Set \`stop_on_failure: false\` unless sequential correctness is critical`,
    advancedDefaults: {
      category: "planning",
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
  {
    id: "design-quality-review",
    name: "Design Quality Review",
    icon: "Palette",
    description: "Evaluate UX and visual polish using UI Bridge quality metrics + AI analysis",
    content: `Evaluate a page's design quality in two phases: UX first, then visual polish.

## Application
- URL: [APP_URL, e.g. http://localhost:3001]
- Page: [PAGE_PATH, e.g. /admin, /dashboard]

## Phase 1: UX Evaluation (Do This First)
Run the quality evaluator and focus on UX metrics:
1. Connect to the app via UI Bridge
2. Navigate to the target page
3. Run sdk_design_evaluate with context "general"
4. Review UX-category metrics: contentOverflow, aboveFoldRatio, informationDensity, containerEfficiency, viewportUtilization
5. For each UX issue found, create a verification step that checks the specific metric

Ask yourself:
- Can the user see the most important content without scrolling?
- Is screen space used efficiently, or are containers oversized for their data?
- Is content reachable? What percentage is below the fold?
- Does the layout use the full viewport, or waste large regions?

## Phase 2: Visual Polish (After UX Is Addressed)
Only focus on polish after UX metrics score well (>70):
1. Re-run sdk_design_evaluate
2. Review polish metrics: spacing, color, typography, consistency
3. Also review qualitatively — things metrics can't catch:
   - Does the visual hierarchy match importance hierarchy?
   - Are related items grouped logically?
   - Is the page scannable? Can you find key info in 3 seconds?
   - Are interactive elements clearly distinguishable from static content?
   - Is there a clear primary action on the page?

## Critical Constraint: Preserve All Information
When implementing style or layout refactors, ALL information and functionality from the original page MUST be retained. A UX improvement means better presentation of the same data — never removal of content. If the original page shows 8 tabs, 6 metrics, or a table with 7 columns, the redesigned version must include all of them. Reformat, reorganize, and restyle freely, but do not drop features, sections, or data fields.

## Phase 3: Recommendations
For each finding, generate an agentic prompt step that:
- Describes the specific issue with element IDs
- Explains why it hurts usability or polish
- Suggests a concrete fix within the existing design system
- Ensures all original information and features are preserved in the fix`,
    advancedDefaults: {
      discoveryMode: "enabled",
      category: "review",
      includeDesignGuidance: true,
    },
  },
  {
    id: "frontend-design-implementation",
    name: "Frontend Design Implementation",
    icon: "Paintbrush",
    description:
      "Implement or redesign a frontend page with intentional design direction and quality verification",
    content: `Implement or redesign a frontend page with a specific design direction, ensuring high visual quality and UX.

## Project
- Path: [PROJECT_PATH, e.g. C:\\Users\\me\\my-app]
- Framework: [FRAMEWORK, e.g. Next.js 15, Vite + React]
- App URL when running: [APP_URL, e.g. http://localhost:3000]

## Design Brief
- Page/Route: [PAGE_PATH, e.g. /dashboard, /landing, /settings]
- Purpose: [What this page does and who it's for]
- Key content: [List the main content sections, data displays, or interactive elements]

## Aesthetic Direction
Pick one (or describe your own):
- [ ] Brutalist — raw, bold, monospace, harsh contrasts
- [ ] Minimalist — generous whitespace, single accent, typography-driven
- [ ] Retro-futuristic — neon accents, dark backgrounds, tech-inspired
- [ ] Editorial — magazine-like layouts, dramatic type scale, asymmetric
- [ ] Organic — soft curves, natural colors, flowing layouts
- [ ] Industrial — exposed structure, monochrome with accent, dense information
- [ ] Custom: [DESCRIBE_YOUR_DIRECTION]

## Typography & Color Preferences (optional)
- Font pairing: [e.g. "Space Grotesk for headings, Inter for body" or "surprise me"]
- Color mood: [e.g. "warm earth tones", "cool professional blues", "high-contrast dark mode"]
- Inspirations: [Any reference sites, styles, or moods to draw from]

## Instructions
1. Analyze the current page state (if it exists) via UI Bridge
2. Implement the design following the aesthetic direction above
3. Apply the design guidance principles (typography hierarchy, color cohesion, spatial composition, motion)
4. Ensure all original content and functionality is preserved if redesigning
5. Run sdk_design_evaluate to verify design quality scores
6. Iterate on any metrics scoring below 70
7. Verify UI Bridge auto-registration discovers new interactive elements`,
    advancedDefaults: {
      discoveryMode: "enabled",
      category: "design",
      tags: "frontend, design",
      includeDesignGuidance: true,
    },
  },
  {
    id: "ui-bridge-onboard",
    name: "Add UI Bridge SDK",
    icon: "Plug",
    description:
      "Instrument a React project with UI Bridge SDK — provider, AutoRegisterProvider, and spec files",
    content: `Instrument an existing React project with the UI Bridge SDK so it can be discovered, inspected, and verified by Qontinui workflows.

## Project
- Path: [PROJECT_PATH, e.g. C:\\Users\\me\\my-react-app]
- Framework: [FRAMEWORK, e.g. Next.js 15, Vite + React, Create React App]
- App URL when running: [APP_URL, e.g. http://localhost:3000]

## What to Instrument
- [List key pages/routes, e.g. /dashboard, /settings, /users, or "all routes"]
- [List key interactive elements, e.g. forms, modals, navigation, or "discover automatically"]

## Instructions
1. Install the UI Bridge SDK package: \`npm install @anthropic/ui-bridge\` (or check if already installed)
2. Wrap the root layout with \`<UIBridgeProvider>\` — find the top-level layout file and add the provider
3. Wrap the app with \`<AutoRegisterProvider>\` inside the UIBridgeProvider — this automatically discovers and registers all interactive elements with stable semantic IDs
4. For each page/route listed above:
   a. Register component-level actions with \`useUIComponent\` for forms and complex components (optional, for advanced actions)
   b. Register important UI states (loading, error, empty) with \`useUIState\` (optional)
   c. Create a \`.spec.uibridge.json\` file alongside the page with grouped assertions:
      - An \`element-presence\` group verifying core layout elements exist
      - A \`state-consistency\` group verifying interactive element states (enabled/disabled, initial values)
      - A \`semantic\` group describing the page's purpose and key capabilities
      - Use diverse assertion types: exists, enabled, disabled, hasText, containsText, hasValue — not just exists
      - Use conditions for state-dependent assertions
      - Use graduated severity: critical for core, warning for important, info for nice-to-have
5. Verify the instrumented app starts without errors and the UI Bridge endpoints respond
6. Connect via UI Bridge and confirm elements are discoverable

## Spec File Format
Each spec file should follow this structure:
\`\`\`json
{
  "version": "1.0.0",
  "description": "Specs for [page name]",
  "groups": [
    { "id": "...", "name": "...", "category": "element-presence", "assertions": [...], "source": "manual" },
    { "id": "...", "name": "...", "category": "state-consistency", "assertions": [...], "source": "manual" },
    { "id": "...", "name": "...", "category": "semantic", "assertions": [...], "source": "manual" }
  ],
  "metadata": { "component": "...", "pageUrl": "/...", "elementSource": "sdk" }
}
\`\`\``,
    advancedDefaults: {
      discoveryMode: "enabled",
      category: "instrumentation",
      tags: "ui-bridge, sdk, onboarding",
    },
  },
];
