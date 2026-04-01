/**
 * Multimodal Vision Pipeline Tutorial
 *
 * Informational tutorial explaining the TuriX-CUA inspired visual AI
 * system: multimodal prompts, annotated screenshots, Brain/Actor
 * separation, playbook skills, and progressive memory compression.
 */

import type { Tutorial } from "../../../types/tutorial";

export const multimodalVisionTutorial: Tutorial = {
  id: "multimodal-vision",
  title: "Multimodal Vision Pipeline",
  description:
    "Understand how qontinui sends annotated screenshots to vision-capable LLMs for intelligent UI analysis. Covers the Brain/Actor two-phase pattern, screenshot annotation, normalized coordinates, domain playbooks, and progressive memory compression.",
  duration: "12 minutes",
  difficulty: "advanced",
  mode: "contextual",
  focusPage: "settings",
  category: "AI Features",
  tags: ["featured", "ai", "vision", "multimodal", "brain-actor", "screenshots", "advanced"],
  prerequisites: ["ai-analysis"],
  learningObjectives: [
    "Understand how screenshots are annotated with numbered element markers for LLMs",
    "Know the difference between multimodal (API) and text-only (CLI) providers",
    "Understand the Brain/Actor two-phase LLM pattern and when to enable it",
    "Know how domain playbooks inject contextual knowledge into AI prompts",
    "Understand progressive memory compression for long-running agentic loops",
  ],
  steps: [
    {
      id: "intro",
      title: "Visual AI for Automation",
      content: `Qontinui can **see** what's on screen and reason about it visually. Instead of relying on text-only descriptions, the system:

1. **Captures screenshots** of the target application
2. **Annotates** them with numbered colored markers on interactive elements
3. **Sends the annotated image** to a vision-capable LLM (Claude API, Gemini API)
4. **Receives structured analysis** — what's on screen, what to do next

This is called the **Multimodal Vision Pipeline**, inspired by TuriX-CUA's Set-of-Mark annotation pattern.`,
      tips: [
        "Multimodal means 'multiple modes of input' — text + images in one prompt",
        "Only API providers support vision. CLI providers fall back to text-only.",
      ],
      estimatedDuration: 1,
    },
    {
      id: "two-contexts",
      title: "Two Automation Contexts",
      content: `The vision pipeline operates differently depending on what you're automating:

**UI Bridge Apps (White-Box)**
Apps with the UI Bridge SDK integrated — your own web apps. The system has access to 50+ element properties: semantic types, ARIA roles, computed styles, form validation, change tracking, and exact screen coordinates.

**Black-Box Desktop Apps**
Third-party native apps where you can only see the screen. The system uses screenshot-based detection: template matching, OCR, accessibility trees (Windows UIA), and vision LLM fallback.

The pipeline **always prefers UI Bridge data** when available — it's richer, faster, and more reliable than screenshot inference.`,
      details: `### Data Source Comparison

| Property | UI Bridge | Black-Box |
|----------|-----------|-----------|
| Element type | Exact (button, input, etc.) | Inferred from visuals |
| Position | normalizedRect (0.0-1.0) | Pixel coords → normalized |
| Labels | ARIA + textContent | OCR + accessibility tree |
| State | visible/enabled/focused/value | Not directly available |
| Latency | ~5ms (DOM query) | 50-2000ms (cascade detection) |

The element collector (\`vision/element_collector.rs\`) tries the UI Bridge endpoint first, then falls back to DetectedElement from step outputs.`,
      tips: [
        "UI Bridge apps get annotation data instantly from the DOM — no screenshot analysis needed",
        "For black-box apps, 12 cascade detection backends are tried cheapest-first",
      ],
      estimatedDuration: 2,
    },
    {
      id: "provider-config",
      title: "Multimodal Provider Configuration",
      content: `Vision features require an **API provider** — CLI providers cannot send images.

**Vision-Capable Providers:**
- **Claude API** — Sends images as \`{"type": "image", "source": {"type": "base64", ...}}\` content blocks
- **Gemini API** — Sends images as \`{"inline_data": {"mime_type": "image/png", ...}}\` parts

**Text-Only Providers (fallback):**
- **Claude CLI** — Strips images, sends text only, logs a warning
- **Gemini CLI** — Same text-only fallback

The routing layer **auto-switches from CLI to API** when it detects multimodal content, so you don't need to manually change providers.`,
      targetElement: {
        selector: "ai-settings-panel",
        highlightType: "spotlight",
        position: "left",
      },
      tips: [
        "You can configure API keys on the Settings > AI page",
        "The system prompt is sent via Claude's top-level 'system' field and Gemini's 'systemInstruction' field",
        "Token costs are higher for vision calls (~1600 tokens per 1024px image tile)",
      ],
      estimatedDuration: 1,
    },
    {
      id: "screenshot-annotation",
      title: "Screenshot Annotation Engine",
      content: `Before sending a screenshot to the LLM, the annotation engine draws **numbered colored rectangles** on interactive elements:

**Color Coding by Type:**
- 🟢 **Green** — Buttons, submit controls
- 🔵 **Blue** — Input fields, textareas
- 🟠 **Orange** — Links
- 🟣 **Purple** — Dropdowns, selects
- 🔵 **Teal** — Checkboxes, radios
- ⬜ **Gray** — Other interactive elements

Each element gets a sequential number (1, 2, 3...) drawn in the top-left corner of its box. The LLM references elements by number: *"click element [3]"*.`,
      details: `### How Annotation Works

1. **Collect elements** — From UI Bridge snapshot or CascadeDetector results
2. **Filter** — Keep only interactive types (button, input, select, etc.), skip hidden, cap at 50
3. **Normalize coordinates** — All positions converted to 0.0-1.0 range (resolution-independent)
4. **Draw** — Colored rectangles + number labels via pixel-font rendering (no font dependency)
5. **Resize** — Image scaled to max 1024px width (saves ~75% of vision tokens)
6. **Encode** — Base64 PNG output alongside a text element index

The text index format:
\`\`\`
## Screen Elements
[1] "Login" button at (0.120, 0.340) size (0.080, 0.040)
[2] "Email" input at (0.100, 0.220) size (0.250, 0.030)
\`\`\`

Both the annotated image and text index are sent to the LLM.`,
      tips: [
        "Normalized coordinates (0.0-1.0) make element references portable across screen resolutions",
        "The 50-element cap keeps the annotation readable and token-efficient",
        "Hidden elements (visible: false) are automatically excluded",
      ],
      estimatedDuration: 2,
    },
    {
      id: "brain-actor",
      title: "Brain/Actor Two-Phase Pattern",
      content: `The most powerful vision feature splits reasoning from execution:

**Brain** (expensive vision model — Opus, GPT-4V)
- Receives the annotated screenshot + element index + UI state
- Analyzes what's on screen and assesses progress toward the goal
- Produces a structured action plan: *"1. Click element [3], 2. Type 'hello' in [5]"*

**Actor** (cheap text model — Haiku, Sonnet)
- Receives only the Brain's text plan (no images — no vision token cost)
- Translates the plan into concrete UI actions
- If confused, signals **RequestContext** to trigger a Brain re-invocation

This saves ~50% of LLM costs because the Actor never sees images.`,
      details: `### Brain/Actor Data Flow

\`\`\`
Iteration N:
  1. should_invoke_brain() → true (first iter, cache expired, or RequestContext)
  2. Capture screenshot → annotate → MultimodalPrompt
  3. Brain LLM call (vision model, ~1600 tokens for image)
     → Returns: screen_analysis, suggested_plan[], context_notes
  4. Cache BrainOutput (TTL: brain_cache_ttl_iterations, default 1)
  5. Build Actor prompt = Brain's plan + element index (text only)
  6. Actor LLM call (text model, no vision tokens)
     → Returns: WorkerOutput with actions + signals
  7. Process signals:
     - RequestContext → force Brain re-invocation next time
     - RecordStore → persist key-value data for future Brain context
  8. Next iteration...
\`\`\`

**Configuration** (\`agentic_verification_config\`):
- \`brain_actor_enabled: true\` — opt-in toggle
- \`max_brain_reinvocations: 2\` — cap per iteration
- \`brain_cache_ttl_iterations: 1\` — how long to reuse Brain output

The Brain/Actor orchestrator is defined in \`orchestrator/brain_actor.rs\`.`,
      tips: [
        "Brain/Actor mode is opt-in: set brain_actor_enabled: true in the agentic verification config",
        "The Actor can save intermediate findings via RecordStore signals for the Brain to use later",
        "Brain re-invocation is capped at 2 per iteration to prevent infinite loops",
      ],
      estimatedDuration: 2,
    },
    {
      id: "playbooks",
      title: "Domain Knowledge Playbooks",
      content: `**Playbooks** are human-editable markdown files that inject domain-specific knowledge into AI prompts. They answer: *"How do I navigate this specific application?"*

Store playbooks in \`~/.qontinui/playbooks/\` as \`.md\` files:

\`\`\`markdown
---
name: Salesforce Login Flow
description: How to log into Salesforce
triggers:
  - type: url_pattern
    value: "*.salesforce.com/*"
---

## Steps
1. Username field has ID "username"
2. Click "Log In" button after entering credentials
3. MFA code input appears if required
\`\`\`

When the automation target matches a trigger, the playbook content is automatically injected into the workflow generation prompt and the Brain's system prompt.`,
      details: `### Playbook Trigger Types

| Type | Match Logic | Example |
|------|------------|---------|
| \`app_name\` | Case-insensitive contains | \`"Salesforce"\` matches "Salesforce Lightning" |
| \`url_pattern\` | Segment-based glob | \`"*.salesforce.com/*"\` matches any Salesforce URL |
| \`tag\` | Exact match | \`"crm"\` matches workflows tagged "crm" |

Playbooks with no triggers are always included (universal domain knowledge).

**Token Budget:** Playbook content is truncated to 2000 chars in workflow generation and 1500 chars in Brain prompts to prevent context bloat.

**Storage:** Playbooks extend the existing SkillTemplate system with a \`Playbook\` variant. They appear on the Skills page alongside step-template skills.`,
      tips: [
        "Playbooks are loaded from ~/.qontinui/playbooks/ at prompt assembly time",
        "Multiple playbooks can match simultaneously — all matched content is injected",
        "The YAML frontmatter parser is in skills/playbook_parser.rs",
      ],
      estimatedDuration: 1,
    },
    {
      id: "memory-compression",
      title: "Progressive Memory Compression",
      content: `Agentic verification loops can run many iterations. Without compression, the context window fills up with stale history from early attempts.

**The Problem:** Each iteration adds ~200 chars of approach/result text. After 20 iterations, that's 4000+ chars of history — most of it about failed approaches that were already abandoned.

**The Solution:** Progressive compression keeps the history budget under **4000 characters**:

1. **Recent iterations** (last 3) stay verbatim — full detail
2. **Older iterations** get compressed — either merged by character truncation or summarized by an LLM

**Two Compression Modes:**
- **CharBased** (default) — Merges oldest entries by concatenating and truncating to 100 chars
- **LlmSummarized** — Sends oldest entries to a Haiku model: *"Summarize what was tried and what failed"*, replaces with a single summary entry. Falls back to CharBased if the LLM call fails.`,
      tips: [
        "LLM summarization uses temperature=0.3 for factual accuracy",
        "All string truncation is UTF-8 safe (chars().take(N), not byte slicing)",
        "The compressed history appears in the worker prompt as '## Previous Attempts'",
      ],
      estimatedDuration: 1,
    },
    {
      id: "click-highlights",
      title: "Visual Click Highlighting",
      content: `During automation, brief visual highlights appear at each action location — a pulsing colored circle that fades after 600ms. This gives you real-time feedback of what the system is doing.

**For UI Bridge apps:** A CSS overlay div is injected by \`showClickHighlight()\` in the action executor. Uses \`position: fixed; pointer-events: none; z-index: 999999\` — purely visual, never interferes with interaction.

**For black-box apps:** A transparent Tauri overlay window renders the same animation at screen coordinates.

**Color by Action Type:**
| Action | Color |
|--------|-------|
| Click | Green (#00c800) |
| Type/SendKeys | Blue (#0064ff) |
| Scroll | Orange (#ff8c00) |
| Select | Purple (#b400b4) |
| Focus | Teal (#00b4b4) |`,
      tips: [
        "Highlights auto-remove from the DOM after 600ms",
        "The UI Bridge action-executor calls showElementHighlight() after each performAction()",
        "If the import fails, the highlight is silently skipped (non-fatal)",
      ],
      estimatedDuration: 1,
    },
    {
      id: "architecture-overview",
      title: "Full Pipeline Architecture",
      content: `Here's how all the pieces fit together when an agentic verification workflow runs with Brain/Actor mode:

**Data Flow:**
\`\`\`
UI Bridge snapshot ──→ element_collector ──→ annotator
         OR                                     ↓
CascadeDetector ────→ DetectedElement ──→ annotated screenshot
                                              ↓
                                    MultimodalPrompt
                                    (text + image)
                                         ↓
                                  Claude/Gemini API
                                         ↓
                                    BrainOutput
                                (analysis + plan)
                                         ↓
                              Actor prompt (text only)
                                         ↓
                                   WorkerOutput
                                 (actions + signals)
                                         ↓
                              Signal processing:
                              - RequestContext → re-invoke Brain
                              - RecordStore → persist data
\`\`\`

**Key Files:**
- \`ai_provider/multimodal.rs\` — ContentBlock types + provider serialization
- \`vision/annotator.rs\` — Screenshot annotation engine
- \`vision/element_collector.rs\` — Element collection from UI Bridge / cascade
- \`orchestrator/brain_actor.rs\` — Two-phase LLM orchestration
- \`skills/playbook_parser.rs\` — Markdown playbook loading
- \`orchestrator/compression.rs\` — Progressive memory compression`,
      details: `### Backend File Map

| Layer | File | Purpose |
|-------|------|---------|
| API Types | \`ai_provider/multimodal.rs\` | ContentBlock, ImageSource, MultimodalPrompt |
| Claude API | \`ai_provider/claude_api.rs\` | run_claude_api_multimodal() |
| Gemini API | \`ai_provider/gemini_api.rs\` | run_gemini_api_multimodal() |
| Routing | \`ai_provider/routing.rs\` | run_prompt_with_routing_multimodal() |
| Annotation | \`vision/annotator.rs\` | annotate_screenshot() |
| Element Collection | \`vision/element_collector.rs\` | collect_from_ui_bridge(), collect_from_detected_elements() |
| Shared Types | \`vision/types.rs\` | NormalizedRect |
| Brain/Actor | \`orchestrator/brain_actor.rs\` | BrainActorOrchestrator |
| Signals | \`orchestrator/structured_output.rs\` | RequestContext, RecordStore |
| Compression | \`orchestrator/compression.rs\` | summarize_iterations_with_llm() |
| Loop Integration | \`agentic_verification_loop.rs\` | Brain/Actor wiring in main loop |
| Health Monitor | \`health_monitor.rs\` | VerifierUiContext + element extraction |
| Playbooks | \`skills/playbook_parser.rs\` | parse_playbook(), playbooks_for_context() |
| Click Overlay (UI Bridge) | \`ui-bridge/debug/click-highlight.ts\` | showClickHighlight() |
| Click Overlay (Black-box) | \`click_overlay.rs\` | Tauri transparent overlay window |`,
      estimatedDuration: 2,
    },
    {
      id: "summary",
      title: "Vision Pipeline Unlocked!",
      content: `You now understand qontinui's multimodal vision system!

**What You Learned:**
- Screenshots are annotated with numbered colored markers for LLM consumption
- API providers (Claude API, Gemini API) support vision; CLI providers fall back to text
- The Brain/Actor pattern splits expensive vision reasoning from cheap action execution
- Domain playbooks inject application-specific knowledge via markdown files
- Progressive compression keeps iteration history under 4000 chars
- Click highlights provide real-time visual feedback during automation

**To Enable:**
Set \`brain_actor_enabled: true\` in your workflow's agentic verification config and use an API provider.

**Related Tutorials:**
- *AI-Powered Automation* — Basics of AI configuration
- *Running Your First Workflow* — How the agentic loop works`,
      resources: [
        {
          title: "TuriX-CUA Integration Plan",
          url: "https://github.com/TurixAI/TuriX-CUA",
        },
      ],
      estimatedDuration: 1,
    },
  ],
};
