/**
 * Diagnostic Model Override Tutorial (Informational)
 *
 * Explains the diagnostician's AI triage system in the orchestration loop:
 * how the diagnostic phase works, what model override does, how data flows
 * from the UI form through the Rust backend to the AI provider, and what
 * the results look like in the iteration history table.
 */

import type { Tutorial } from "../../../types/tutorial";

export const diagnosticModelOverrideTutorial: Tutorial = {
  id: "diagnostic-model-override",
  title: "Diagnostic AI Triage & Model Override",
  description:
    "Understand the orchestration loop's diagnostic phase — how it captures UI state, runs assertions, triages failures with AI, and how model override lets you control which AI model performs the triage.",
  duration: "8 minutes",
  difficulty: "intermediate",
  mode: "contextual",
  focusPage: "orchestration-loop",
  category: "AI Features",
  tags: ["featured", "diagnostics", "orchestration-loop", "model-override", "ai-triage"],
  prerequisites: ["getting-started"],
  learningObjectives: [
    "Understand the orchestration loop's diagnostic phase and when it runs",
    "Know how UI Bridge assertions and page health checks work together",
    "Understand the AI triage pipeline: what it receives, what it returns",
    "Configure a model override for faster or more capable triage",
    "Read diagnostic results in the iteration history table",
    "Understand how failed diagnostics feed context into the next iteration",
  ],
  steps: [
    // =========================================================================
    // Step 1: The diagnostic phase in the orchestration loop
    // =========================================================================
    {
      id: "intro",
      title: "The Diagnostic Phase",
      content: `The **Orchestration Loop** runs an iterative cycle: build a workflow, execute it, reflect on results, implement fixes, and rebuild. The **diagnostic phase** sits between Execute and Reflect.

**What it does:**
1. Captures the page's health state via UI Bridge
2. Runs user-defined assertions against the live page
3. If anything fails — calls an AI model to **triage** the failure
4. Classifies the root cause into a category
5. Suggests how to rewrite the prompt for the next iteration

Without diagnostics, the loop relies solely on reflection. With diagnostics, it gets structured evidence about *what went wrong* on the actual page.`,
      estimatedDuration: 1,
      tips: [
        "The diagnostic phase is optional — enable it via the checkbox in pipeline mode",
        "It only runs when the workflow execution completes successfully (no crash)",
      ],
    },

    // =========================================================================
    // Step 2: Enabling diagnostics in the UI
    // =========================================================================
    {
      id: "enable-diagnostics",
      title: "Enabling the Diagnostic Phase",
      content: `To use diagnostics, you need to be in **Pipeline mode** on the Orchestration Loop panel.

**Steps to enable:**
1. Select the **Pipeline** radio button (it's the default)
2. Check **Enable Diagnostic Phase** — this reveals a teal-bordered config panel
3. An **Exit** strategy dropdown also appears with "Diagnostic Evaluation" as an option

When "Diagnostic Evaluation" is the exit strategy, the loop stops when all assertions pass and the page is healthy — not just when the reflector sees zero fixes.`,
      targetElement: {
        selector: '[data-tutorial-id="ol-enable-diagnose"]',
        highlightType: "spotlight",
        position: "bottom",
        scrollIntoView: true,
      },
      estimatedDuration: 1,
      tips: [
        "You can also use diagnostics with Reflection exit — the diagnostic data enriches the reflector's context even if it's not the exit condition",
      ],
    },

    // =========================================================================
    // Step 3: The diagnostic config panel
    // =========================================================================
    {
      id: "config-panel",
      title: "Diagnostic Configuration",
      content: `The teal-bordered panel has four controls:

- **Assertions (JSON array)** — UI Bridge assertion objects that define what "correct" looks like. Example: \`[{"type": "visible", "role": "tab", "name": "Transitions"}]\`
- **Capture DOM snapshot** — Whether to capture a truncated DOM snapshot for the AI triage context
- **Max chars** — How many characters of the snapshot to include (default 8000)
- **Model override** — Which AI model to use for the triage call

The assertions are the "spec" for your expected page state. The AI triage only runs when something fails.`,
      targetElement: {
        selector: '[data-tutorial-id="ol-diag-config-panel"]',
        highlightType: "border",
        position: "top",
        scrollIntoView: true,
      },
      estimatedDuration: 1,
      details: `**Assertion format:**
Each assertion is a JSON object matching the UI Bridge assertion schema:
\`\`\`json
[
  { "type": "visible", "role": "tab", "name": "Transitions" },
  { "type": "count", "role": "row", "min": 1 },
  { "type": "hasText", "selector": ".status", "text": "Ready" }
]
\`\`\`

The runner sends these to the target runner's UI Bridge endpoint, which evaluates them against the live DOM.`,
    },

    // =========================================================================
    // Step 4: What the diagnostician does internally
    // =========================================================================
    {
      id: "diagnostician-pipeline",
      title: "Inside the Diagnostician",
      content: `When the diagnostic phase runs, the Rust backend (\`diagnostician.rs\`) executes this pipeline:

1. **Page health** — calls \`get_page_health()\` on the target runner
2. **Assertions** — calls \`run_assertions()\` with your JSON array
3. **Pass check** — if health is OK *and* all assertions pass → **PASS**, skip triage
4. **Snapshot** (if enabled) — captures \`get_ui_snapshot()\` truncated to max chars
5. **AI triage** — builds a prompt with all the evidence and sends it to the AI

The triage prompt includes: page health JSON, assertion results, DOM snapshot, and the full history of previous diagnostic attempts in this loop run.`,
      estimatedDuration: 1,
      tips: [
        "The diagnostician is called from the orchestration loop's main Rust task",
        "If the workflow crashed (status != completed), the diagnostic phase is skipped entirely",
      ],
      details: `**Data flow:**
\`\`\`
Target Runner UI → UI Bridge → get_page_health()
                             → run_assertions()
                             → get_ui_snapshot()
                                    ↓
                           diagnostician.rs
                                    ↓
                    build_triage_prompt() → AI model
                                    ↓
                    parse_triage_response() → DiagnosticResult
\`\`\`

The \`DiagnosticResult\` struct contains: \`passed\`, \`page_health\`, \`assertion_results\`, \`root_cause\`, \`diagnosis\`, and \`prompt_rewrite_suggestion\`.`,
    },

    // =========================================================================
    // Step 5: Root cause categories
    // =========================================================================
    {
      id: "root-cause-categories",
      title: "Root Cause Classification",
      content: `The AI triage classifies each failure into exactly **one** root cause category:

| Category | Meaning |
|----------|---------|
| **bad_ui_rendering** | The UI rendered incorrectly (wrong components, broken layout) |
| **bad_ui_bridge_evaluation** | UI Bridge misread the page (wrong selector, timing) |
| **bad_verification_steps** | The assertions themselves are wrong (testing the wrong thing) |
| **bad_generation_prompt** | The workflow prompt was unclear or incomplete |
| **bad_state_machine_logic** | Generated state machine has logical errors |
| **infrastructure_issue** | Network error, timeout, crashed app |
| **unknown** | Cannot determine |

This classification drives what the loop does next — a bad prompt triggers a prompt rewrite, while an infrastructure issue might just retry.`,
      estimatedDuration: 1,
    },

    // =========================================================================
    // Step 6: Model override — what and why
    // =========================================================================
    {
      id: "model-override-concept",
      title: "Model Override — What & Why",
      content: `The **Model override** field lets you choose which AI model performs the diagnostic triage call.

**Why would you want this?**

- **Speed** — Use a faster/cheaper model (e.g., \`claude-haiku-4-5-20251001\`) for rapid iteration loops where you're running 10+ iterations
- **Capability** — Use a more capable model (e.g., \`claude-opus-4-6\`) for complex UI diagnosis where subtle rendering bugs need deep analysis
- **Cost control** — Triage runs on every failing iteration, so model choice impacts total cost
- **Experimentation** — Try different models to see which gives more actionable suggestions

When the field is empty (shows "(default)"), the runner uses its standard model routing.`,
      targetElement: {
        selector: '[data-tutorial-id="ol-diag-model-override"]',
        highlightType: "spotlight",
        position: "top",
        scrollIntoView: true,
      },
      estimatedDuration: 1,
      tips: [
        "The override only affects the diagnostic triage call — it does not change the model used for workflow generation, reflection, or fix implementation",
        "You can save a config with a specific model override to the config library for reuse",
      ],
    },

    // =========================================================================
    // Step 7: How model override flows through the system
    // =========================================================================
    {
      id: "model-override-dataflow",
      title: "Model Override Data Flow",
      content: `Here's how the model override value travels from the UI to the actual AI call:

1. **UI** — You type a model ID into the "Model override" input
2. **Form state** — Stored as \`diagnoseModelOverride: string\` in the React form
3. **Config JSON** — Sent as \`pipeline.diagnose.model_override: string | null\` via Tauri IPC
4. **Rust types** — Deserialized into \`DiagnosePhaseConfig.model_override: Option<String>\`
5. **Diagnostician** — Cloned into the \`spawn_blocking\` closure
6. **AI routing** — Passed to \`run_prompt_with_model_override()\` which routes to the specific model
7. **Fallback** — When \`None\`, the routing layer uses default model selection (complexity-based routing)

The key function is \`run_prompt_with_model_override\` in \`ai_provider/routing.rs\` — it builds a \`TaskContext\` and uses the AI router to select the provider and model.`,
      estimatedDuration: 1,
      details: `**The Rust call site in diagnostician.rs:**
\`\`\`rust
let model_override = config.model_override.clone();
let triage_result = tokio::task::spawn_blocking(move || {
    let context = TaskContext::from_prompt(&triage_prompt);
    run_prompt_with_model_override(
        &triage_prompt,
        &context,
        None,           // no doctor handle
        model_override.as_deref(),
        None, None, None, None, None,  // no other overrides
    )
}).await;
\`\`\`

The \`spawn_blocking\` is needed because AI calls are synchronous (they block on HTTP) and the orchestration loop runs on an async Tokio runtime.`,
    },

    // =========================================================================
    // Step 8: Reading results in the iteration history
    // =========================================================================
    {
      id: "reading-results",
      title: "Reading Diagnostic Results",
      content: `After each iteration, the **Iteration History** table shows results. The **Diag** column tells you what the diagnostician found:

- **PASS** (green badge) — Page health OK, all assertions passed. No triage needed.
- **FAIL · bad generation prompt** (red badge) — Shows the root cause category after the dot
- **--** (muted) — Diagnostics weren't configured or the workflow crashed

**Expanding a row** (click the chevron) reveals:
- **Diagnosis** — The AI's 1-3 sentence explanation of what went wrong
- **Suggested prompt rewrite** — Specific changes for the next iteration (in a teal highlight box)
- **Assertion chips** — Each assertion's pass/fail with ✓ or ✗, hover for full JSON details`,
      targetElement: {
        selector: '[data-tutorial-id="ol-iteration-history"]',
        highlightType: "border",
        position: "top",
        scrollIntoView: true,
      },
      estimatedDuration: 1,
    },

    // =========================================================================
    // Step 9: How diagnostics feed the next iteration
    // =========================================================================
    {
      id: "feedback-loop",
      title: "The Diagnostic Feedback Loop",
      content: `Failed diagnostics don't just report — they **drive the next iteration**:

1. The \`prompt_rewrite_suggestion\` from the triage is collected
2. \`build_diagnostic_context()\` assembles a history of all previous attempts
3. This history is injected into the next workflow generation prompt
4. The generator sees *why* each previous attempt failed and what was suggested
5. The last suggestion gets an **IMPORTANT** prefix to ensure the generator prioritizes it

This creates a closed feedback loop:
\`\`\`
Generate → Execute → Diagnose → Inject context → Re-generate
\`\`\`

With a good model override (e.g., a capable model for complex failures), the quality of the rewrite suggestion directly impacts how quickly the loop converges.`,
      estimatedDuration: 1,
      tips: [
        "If the loop keeps getting the same root cause, the model may need more context — consider adding detail to the build description",
        "The diagnostic history is cumulative — later iterations see all previous failures, not just the last one",
      ],
    },

    // =========================================================================
    // Step 10: Summary & tips
    // =========================================================================
    {
      id: "summary",
      title: "Summary",
      content: `**Key takeaways:**

- The diagnostic phase gives the orchestration loop **structured evidence** about page failures
- **Assertions** define "correct" — the AI triage explains *why* it's not correct
- **Model override** controls the cost/capability tradeoff for the triage call
- Results appear in the iteration history with expandable detail
- Failed diagnostics feed context into the next iteration's prompt

**Recommended configurations:**
- Rapid iteration (10+ loops): Use a fast model like \`claude-haiku-4-5-20251001\`
- Complex UI bugs: Use \`claude-sonnet-4-6\` or \`claude-opus-4-6\` for deeper analysis
- Default: Leave empty — the router picks based on prompt complexity`,
      estimatedDuration: 1,
      resources: [
        {
          title: "diagnostician.rs — Rust backend",
          url: "qontinui-runner/src-tauri/src/orchestration_loop/diagnostician.rs",
        },
        {
          title: "OrchestrationLoopPanel.tsx — UI component",
          url: "qontinui-runner/src/components/orchestration-loop/OrchestrationLoopPanel.tsx",
        },
      ],
    },
  ],
};
